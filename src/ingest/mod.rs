mod db_telemetry;
mod proto_wire;
mod speed_tracker;
mod sqlite_ro;
mod stdin;
mod transcript;
mod workspace;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::domain::{
    ChildModelSpeed, ModelIdentity, ModelTokenTotals, PulseSnapshot, TurnCacheMetrics,
    calculate_turn_cache_metrics, compact_model_name, resolve_model_display_name,
};
use crate::platform;
use crate::profile::{IngestStageTimings, Stage, set_stage};

pub use db_telemetry::ConversationDbRecord;
pub use proto_wire::{GenerationMetadataRow, GenerationTelemetryStep, TrajectoryMetadata};
pub use sqlite_ro::cleanup_stale_temp_db_dirs;

/// Pure in-memory stdin extraction result for thin frontend snapshot merging.
#[derive(Debug, Clone)]
pub struct StdinLiveFields {
    pub snapshot: PulseSnapshot,
    pub workspace_dir: PathBuf,
    pub transcript_path: Option<PathBuf>,
}

/// Result of reading host standard input and local workspace metadata.
#[derive(Debug, Clone)]
pub struct IngestResult {
    pub snapshot: PulseSnapshot,
    pub workspace_dir: PathBuf,
    pub conversation_id: Option<String>,
    pub rolling_cost_key: Option<String>,
    pub child_conversation_ids: Vec<String>,
    pub prefetched_db_records: HashMap<(PathBuf, String), Arc<ConversationDbRecord>>,
    pub diagnostics: Vec<String>,
    pub stage_timings: IngestStageTimings,
}

fn current_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Narrow facade for reading host inputs and workspace state.
pub struct HostReader;

impl HostReader {
    /// Read a pure-data `ConversationDbRecord` from a host conversation `.db` safely
    /// without lock contention or exposing `rusqlite::Connection` / raw Protobuf bytes.
    pub fn read_conversation_db(db_path: &Path) -> Option<ConversationDbRecord> {
        db_telemetry::read_conversation_db(db_path)
    }

    /// Parse host stdin payload and enrich with transcript state, output speed,
    /// lock-free SQLite telemetry/compaction/cascade costs, and workspace metadata.
    pub fn read_live_pulse(stdin_text: &str, fallback_cwd: &Path) -> IngestResult {
        Self::read_live_pulse_with_deadline(stdin_text, fallback_cwd, None)
    }

    /// Pure in-memory stdin snapshot for hard-timeout degradation (zero disk I/O).
    pub fn parse_stdin_fallback(stdin_text: &str, fallback_cwd: &Path) -> (PulseSnapshot, PathBuf) {
        let live = Self::parse_stdin_live_fields(stdin_text, fallback_cwd);
        (live.snapshot, live.workspace_dir)
    }

    /// Pure in-memory stdin extraction including `turn_cache_metrics` and `transcript_path` (zero disk I/O).
    pub fn parse_stdin_live_fields(stdin_text: &str, fallback_cwd: &Path) -> StdinLiveFields {
        let observed_at_ms = current_epoch_ms();
        let mut parsed = stdin::parse_host_stdin(stdin_text, fallback_cwd, observed_at_ms);
        parsed.snapshot.workspace.current_dir =
            workspace::resolve_current_dir_name(&parsed.workspace_dir);
        let raw_model_name = parsed
            .snapshot
            .model
            .as_ref()
            .map(|m| {
                if !m.display_name.is_empty() {
                    m.display_name.as_str()
                } else {
                    m.id.as_str()
                }
            })
            .unwrap_or("");
        parsed.snapshot.turn_cache_metrics = resolve_turn_cache_metrics(
            [
                parsed.stdin_current_usage_val.as_ref(),
                parsed.stdin_context_window_val.as_ref(),
                None,
                None,
            ],
            raw_model_name,
        );
        StdinLiveFields {
            snapshot: parsed.snapshot,
            workspace_dir: parsed.workspace_dir,
            transcript_path: parsed.transcript_path,
        }
    }

    /// Parse host stdin payload and enrich within an optional soft deadline so statusline
    /// rendering degrades gracefully instead of exceeding the host command timeout.
    pub fn read_live_pulse_with_deadline(
        stdin_text: &str,
        fallback_cwd: &Path,
        deadline: Option<Instant>,
    ) -> IngestResult {
        let mut timings = IngestStageTimings::default();
        let observed_at_ms = current_epoch_ms();

        set_stage(Stage::IngestParse);
        let t_parse = Instant::now();
        let mut parsed = stdin::parse_host_stdin(stdin_text, fallback_cwd, observed_at_ms);

        let mut transcript_path = parsed.transcript_path.clone();
        if transcript_path.as_ref().is_none_or(|p| !p.exists())
            && let Some(conv_id) = parsed.snapshot.host.conversation_id.as_deref()
        {
            let candidate = platform::resolve_antigravity_path(&format!(
                "brain/{conv_id}/.system_generated/logs/transcript.jsonl"
            ));
            if candidate.exists() || transcript_path.is_none() {
                transcript_path = Some(candidate);
            }
        }

        if transcript_path.as_ref().is_some_and(|p| p.is_file()) {
            parsed.snapshot.has_conversation_identity = true;
        }

        let (_, derived_conv_id) = db_telemetry::extract_transcript_context(transcript_path.as_deref());
        let effective_conv_id = parsed
            .snapshot
            .host
            .conversation_id
            .clone()
            .or(derived_conv_id);

        let stdin_model_ident = parsed.snapshot.model.as_ref().map(|m| {
            if !m.display_name.is_empty() {
                m.display_name.clone()
            } else {
                m.id.clone()
            }
        });
        timings.parse_ms = t_parse.elapsed().as_millis() as u64;

        set_stage(Stage::IngestTranscript);
        let t_transcript = Instant::now();
        let scanned = transcript_path.as_deref().and_then(|tp| {
            transcript::scan_transcript_incremental(
                tp,
                stdin_model_ident.as_deref(),
                observed_at_ms,
                deadline,
            )
        });
        timings.transcript_ms = t_transcript.elapsed().as_millis() as u64;

        let mut scanned_usage_val: Option<Value> = None;
        let mut last_planner_response = None;

        if let Some(ref sc) = scanned {
            scanned_usage_val = sc.usage.clone();
            last_planner_response = sc.last_planner_response.clone();

            parsed.snapshot.transcript_steps = sc.steps;
            parsed.snapshot.compaction_count = sc.compaction_count;
            parsed.snapshot.peak_raw_turn_tokens = sc.peak_raw_turn_tokens;
            parsed.snapshot.turn_history_max_by_mode = sc.turn_history_max_by_mode.clone();
            parsed.snapshot.max_historical_cache = sc.max_historical_cache;
            parsed.snapshot.transcript_total_input_tokens = sc.total_input_tokens;
            parsed.snapshot.transcript_total_output_tokens = sc.total_output_tokens;
            parsed.snapshot.child_model_speeds = sc.child_model_speeds.clone();
            parsed.snapshot.model_token_totals = sc.model_token_totals.clone();
            parsed.snapshot.main_model_last_active_ms = sc.main_model_last_active_ms;
            parsed.snapshot.image_exhausted = sc.image_exhausted.clone();
            parsed.snapshot.last_turn_usage = sc.last_turn_usage.as_ref().map(|lt| lt.to_domain());
            parsed.snapshot.last_turn_model = sc
                .last_turn_usage
                .as_ref()
                .and_then(|lt| lt.model.clone())
                .or_else(|| {
                    if sc.last_known_model.is_empty() {
                        None
                    } else {
                        Some(sc.last_known_model.clone())
                    }
                });

            if parsed.snapshot.model.is_none()
                && let Some(raw_model) = parsed.snapshot.last_turn_model.as_deref()
            {
                parsed.snapshot.model = Some(ModelIdentity {
                    id: raw_model.to_string(),
                    display_name: resolve_model_display_name(raw_model, None),
                    effort: None,
                });
            }

            if let Some(ref sc_usage) = scanned_usage_val {
                let sc_ctx = stdin::parse_context_window(sc_usage);
                if let Some(ref mut existing_ctx) = parsed.snapshot.context {
                    if existing_ctx.context_window_size == 0 && sc_ctx.context_window_size > 0 {
                        existing_ctx.context_window_size = sc_ctx.context_window_size;
                    }
                    if existing_ctx.current_usage.is_none() {
                        existing_ctx.current_usage = sc_ctx.current_usage;
                    }
                } else {
                    parsed.snapshot.context = Some(sc_ctx);
                }
            }
        }

        let raw_model_name = parsed
            .snapshot
            .model
            .as_ref()
            .map(|m| {
                if !m.display_name.is_empty() {
                    m.display_name.as_str()
                } else {
                    m.id.as_str()
                }
            })
            .or(parsed.snapshot.last_turn_model.as_deref())
            .unwrap_or("");

        let transcript_current_usage_val = scanned_usage_val
            .as_ref()
            .and_then(|u| u.get("current_usage"));
        parsed.snapshot.turn_cache_metrics = resolve_turn_cache_metrics(
            [
                parsed.stdin_current_usage_val.as_ref(),
                parsed.stdin_context_window_val.as_ref(),
                transcript_current_usage_val,
                scanned_usage_val.as_ref(),
            ],
            raw_model_name,
        );

        set_stage(Stage::IngestSpeed);
        let t_speed = Instant::now();
        let speed_transcript_key = transcript_path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .or_else(|| parsed.snapshot.host.session_id.clone());
        let speed_model_key = parsed
            .snapshot
            .model
            .as_ref()
            .map(|m| {
                if !m.id.is_empty() {
                    m.id.as_str()
                } else {
                    m.display_name.as_str()
                }
            })
            .or(parsed.snapshot.last_turn_model.as_deref())
            .unwrap_or("");
        parsed.snapshot.output_speed_tok_s = speed_tracker::compute_output_speed(
            speed_transcript_key.as_deref(),
            speed_model_key,
            parsed.api_duration_ms,
            parsed.stdin_total_output_tokens,
            last_planner_response.as_ref(),
            observed_at_ms,
        );
        timings.speed_ms = t_speed.elapsed().as_millis() as u64;

        let active_model_key_owned = parsed
            .snapshot
            .model
            .as_ref()
            .map(|m| {
                let id = m.id.trim();
                if !id.is_empty() {
                    id
                } else {
                    m.display_name.trim()
                }
            })
            .filter(|s| !s.is_empty())
            .or_else(|| {
                parsed
                    .snapshot
                    .last_turn_model
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            })
            .map(str::to_ascii_lowercase);
        let mut compaction_cache = db_telemetry::load_compaction_cache_handle();
        let mut db_memo = db_telemetry::DbReadMemo::new();

        set_stage(Stage::IngestCompaction);
        let t_compaction = Instant::now();
        parsed.snapshot.server_compaction_limit = db_telemetry::resolve_server_compaction_limit(
            effective_conv_id.as_deref(),
            transcript_path.as_deref(),
            active_model_key_owned.as_deref(),
            observed_at_ms,
            deadline,
            &mut compaction_cache,
            &mut db_memo,
        );
        timings.compaction_ms = t_compaction.elapsed().as_millis() as u64;

        set_stage(Stage::IngestDbPerf);
        let t_db_perf = Instant::now();
        let (session_turns, session_turns_ago, session_last_active_ms) =
            build_session_activity_context(scanned.as_ref());
        let all_child_ids = scanned
            .as_ref()
            .map(|s| s.all_child_ids.as_slice())
            .unwrap_or(&[]);
        let last_user_input_ms = scanned.as_ref().and_then(|s| s.last_user_input_ms);
        let resolved_perf = db_telemetry::resolve_model_perf_stats(
            effective_conv_id.as_deref(),
            transcript_path.as_deref(),
            all_child_ids,
            session_turns,
            last_user_input_ms,
            &session_turns_ago,
            &session_last_active_ms,
            observed_at_ms,
            deadline,
            &mut compaction_cache,
            &mut db_memo,
        );
        compaction_cache.flush_if_dirty();

        enrich_speeds_from_model_perf(
            &mut parsed.snapshot,
            &resolved_perf.models,
            &session_last_active_ms,
        );
        timings.db_perf_ms = t_db_perf.elapsed().as_millis() as u64;

        set_stage(Stage::IngestCascade);
        let t_cascade = Instant::now();
        apply_cascade_cost_breakdown(
            &mut parsed.snapshot,
            transcript_path.as_deref(),
            scanned.as_ref(),
            resolved_perf.cascade_cost_tree.as_ref(),
            observed_at_ms,
            deadline,
        );
        timings.cascade_ms = t_cascade.elapsed().as_millis() as u64;

        set_stage(Stage::IngestWorkspace);
        let t_ws = Instant::now();
        parsed.snapshot.workspace.current_dir =
            workspace::resolve_current_dir_name(&parsed.workspace_dir);
        parsed.snapshot.workspace.git_branch =
            workspace::resolve_git_branch(&parsed.workspace_dir, deadline);
        parsed.snapshot.workspace_meta =
            workspace::scan_workspace_metadata(&parsed.workspace_dir, deadline);
        timings.workspace_ms = t_ws.elapsed().as_millis() as u64;

        let child_conversation_ids: Vec<String> = scanned
            .as_ref()
            .map(|s| s.all_child_ids.clone())
            .unwrap_or_default();
        let rolling_cost_key = resolve_rolling_cost_key(
            &parsed.snapshot,
            transcript_path.as_deref(),
            &parsed.workspace_dir,
        );

        let prefetched_db_records: HashMap<(PathBuf, String), Arc<ConversationDbRecord>> = db_memo
            .into_iter()
            .filter_map(|(k, v)| v.map(|rec| (k, rec)))
            .collect();

        IngestResult {
            snapshot: parsed.snapshot,
            workspace_dir: parsed.workspace_dir,
            conversation_id: effective_conv_id,
            rolling_cost_key,
            child_conversation_ids,
            prefetched_db_records,
            diagnostics: parsed.diagnostics,
            stage_timings: timings,
        }
    }
}

fn resolve_rolling_cost_key(
    snapshot: &PulseSnapshot,
    transcript_path: Option<&Path>,
    workspace_dir: &Path,
) -> Option<String> {
    if let Some(sid) = snapshot
        .host
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(sid.to_string());
    }
    if let Some(cid) = snapshot
        .host
        .conversation_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(cid.to_string());
    }
    if let Some(tp) = transcript_path {
        let raw = tp.to_string_lossy();
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            let norm = trimmed.replace('\\', "/");
            if let Some(idx) = norm.rfind("/brain/") {
                let after = &norm[idx + "/brain/".len()..];
                let first_seg = after.split('/').next().unwrap_or("").trim();
                if !first_seg.is_empty() && first_seg != ".system_generated" {
                    return Some(first_seg.to_string());
                }
            } else {
                return Some(trimmed.to_string());
            }
        }
    }
    if snapshot.has_conversation_identity && !workspace_dir.as_os_str().is_empty() {
        return Some(format!("cwd:{}", workspace_dir.to_string_lossy()));
    }
    None
}

fn build_session_activity_context(
    scanned: Option<&transcript::ScannedTranscript>,
) -> (Option<u64>, BTreeMap<String, u64>, BTreeMap<String, i64>) {
    let Some(sc) = scanned else {
        return (None, BTreeMap::new(), BTreeMap::new());
    };
    let effective_turns = if sc.user_turns > 0 {
        sc.user_turns
    } else if sc.has_planner_response || sc.steps > 0 {
        1
    } else {
        0
    };

    let mut turns_ago: BTreeMap<String, u64> = BTreeMap::new();
    let mut record_ago = |m_key: &str, last_turn: u64| {
        if m_key.is_empty() {
            return;
        }
        let ago = effective_turns.saturating_sub(last_turn);
        turns_ago
            .entry(m_key.to_string())
            .and_modify(|e| *e = (*e).min(ago))
            .or_insert(ago);
    };
    for (m_key, &last_turn) in &sc.model_last_seen_turn {
        record_ago(m_key, last_turn);
    }
    for child in &sc.child_activities {
        let m_key = compact_model_name(&child.model_hint);
        record_ago(&m_key, child.last_active_turn);
    }
    for (m_key, turn_opt, _) in &sc.persisted_child_turns {
        if let Some(t) = turn_opt {
            record_ago(m_key, *t);
        }
    }

    let mut active_ms: BTreeMap<String, i64> = BTreeMap::new();
    let mut record_ms = |m_key: &str, ts: i64| {
        if m_key.is_empty() || ts <= 0 {
            return;
        }
        active_ms
            .entry(m_key.to_string())
            .and_modify(|e| *e = (*e).max(ts))
            .or_insert(ts);
    };
    for child in &sc.child_activities {
        let m_key = compact_model_name(&child.model_hint);
        record_ms(&m_key, child.last_active_ms);
    }
    for (m_key, &ts) in &sc.model_last_active_ms {
        record_ms(m_key, ts);
    }
    for (m_key, ts) in &sc.child_tail_activities {
        record_ms(m_key, *ts);
    }
    for (m_key, _, ts) in &sc.persisted_child_turns {
        record_ms(m_key, *ts);
    }

    (Some(effective_turns), turns_ago, active_ms)
}

fn enrich_speeds_from_model_perf(
    snapshot: &mut PulseSnapshot,
    model_perf: &BTreeMap<String, db_telemetry::ModelPerfSummary>,
    session_last_active_ms: &BTreeMap<String, i64>,
) {
    if model_perf.is_empty() {
        return;
    }
    let main_ident = snapshot
        .model
        .as_ref()
        .map(|m| {
            if !m.display_name.is_empty() {
                m.display_name.as_str()
            } else {
                m.id.as_str()
            }
        })
        .or(snapshot.last_turn_model.as_deref())
        .unwrap_or("");
    let main_model_key = compact_model_name(main_ident);

    if !main_model_key.is_empty()
        && let Some(main_perf) = model_perf.get(&main_model_key)
    {
        snapshot.main_model_turns_ago = Some(main_perf.turns_ago);
        if let Some(avg_speed) = main_perf.avg_speed
            && avg_speed.is_finite()
            && avg_speed > 0.0
        {
            snapshot.output_speed_tok_s = Some(avg_speed);
        }
        if let Some(avg_ttft) = main_perf.avg_ttft_sec
            && avg_ttft.is_finite()
            && avg_ttft >= 0.0
        {
            snapshot.ttft_sec = Some(avg_ttft);
        }
        if main_perf.last_updated_ms > 0 {
            let prev = snapshot.main_model_last_active_ms.unwrap_or(0);
            snapshot.main_model_last_active_ms = Some(prev.max(main_perf.last_updated_ms));
        }
    }

    let mut seen_models: HashSet<String> = HashSet::new();
    for item in &mut snapshot.child_model_speeds {
        let norm = compact_model_name(&item.model);
        seen_models.insert(item.model.clone());
        if !norm.is_empty() {
            seen_models.insert(norm.clone());
        }
        let perf = model_perf
            .get(&item.model)
            .or_else(|| (!norm.is_empty()).then(|| model_perf.get(&norm)).flatten());
        if let Some(p) = perf {
            if let Some(avg_speed) = p.avg_speed
                && avg_speed.is_finite()
                && avg_speed > 0.0
            {
                item.speed_tok_s = avg_speed;
            }
            if let Some(avg_ttft) = p.avg_ttft_sec
                && avg_ttft.is_finite()
                && avg_ttft >= 0.0
            {
                item.ttft_sec = Some(avg_ttft);
            }
            if p.last_updated_ms > 0 {
                item.last_active_ms = item.last_active_ms.max(p.last_updated_ms);
            }
            item.turns_ago = item.turns_ago.min(p.turns_ago);
        }
        if let Some(&sess_ts) = session_last_active_ms.get(&item.model) {
            item.last_active_ms = item.last_active_ms.max(sess_ts);
        }
    }

    let mut extras: Vec<&db_telemetry::ModelPerfSummary> = model_perf
        .values()
        .filter(|p| {
            let norm = compact_model_name(&p.model);
            let is_main = !main_model_key.is_empty()
                && (p.model == main_model_key || norm == main_model_key);
            !is_main
                && !seen_models.contains(&p.model)
                && p.avg_speed.is_some_and(|s| s.is_finite() && s > 0.0)
        })
        .collect();
    extras.sort_by(|a, b| {
        b.last_updated_ms
            .cmp(&a.last_updated_ms)
            .then_with(|| b.stream_samples.cmp(&a.stream_samples))
            .then_with(|| a.model.cmp(&b.model))
    });
    for p in extras {
        if let Some(speed_tok_s) = p.avg_speed {
            let sess_ts = session_last_active_ms
                .get(&p.model)
                .copied()
                .unwrap_or(0);
            snapshot.child_model_speeds.push(ChildModelSpeed {
                model: p.model.clone(),
                speed_tok_s,
                ttft_sec: p.avg_ttft_sec,
                turns_ago: p.turns_ago,
                last_active_ms: p.last_updated_ms.max(sess_ts),
            });
        }
    }
    snapshot
        .child_model_speeds
        .sort_by_key(|c| std::cmp::Reverse(c.last_active_ms));
}

fn merge_model_acc_into(
    target: &mut BTreeMap<String, transcript::ModelTokenAcc>,
    source: &BTreeMap<String, transcript::ModelTokenAcc>,
) {
    for (k, v) in source {
        if k.is_empty() {
            continue;
        }
        let acc = target.entry(k.clone()).or_default();
        acc.input_tokens = acc.input_tokens.saturating_add(v.input_tokens);
        acc.cache_read_tokens = acc.cache_read_tokens.saturating_add(v.cache_read_tokens);
        acc.cache_write_tokens = acc.cache_write_tokens.saturating_add(v.cache_write_tokens);
        acc.output_tokens = acc.output_tokens.saturating_add(v.output_tokens);
        acc.cost_usd += v.cost_usd;
        acc.steps = acc.steps.saturating_add(v.steps);
    }
}

fn reconcile_db_and_transcript(
    db_cost: f64,
    db_steps: u64,
    db_max_step_index: Option<u64>,
    db_models: &BTreeMap<String, transcript::ModelTokenAcc>,
    sc: &transcript::ScannedTranscript,
) -> (f64, BTreeMap<String, transcript::ModelTokenAcc>) {
    let trailing: &[transcript::TranscriptTokenStepCost] = if let Some(max_db_idx) = db_max_step_index {
        let split_pos = sc
            .recent_token_steps
            .iter()
            .position(|s| s.step_index.is_some_and(|idx| idx > max_db_idx));
        match split_pos {
            Some(pos) => &sc.recent_token_steps[pos..],
            None => &[],
        }
    } else if sc.token_steps_count > db_steps {
        let extra = (sc.token_steps_count - db_steps) as usize;
        let len = sc.recent_token_steps.len();
        if extra <= len {
            &sc.recent_token_steps[len - extra..]
        } else {
            &[]
        }
    } else {
        &[]
    };

    if !trailing.is_empty() {
        let mut combined_cost = db_cost;
        let mut combined_models = db_models.clone();
        for step in trailing {
            if step.step_index.is_some()
                && db_max_step_index.is_some_and(|m| step.step_index <= Some(m))
            {
                continue;
            }
            combined_cost += step.cost_usd;
            let acc = combined_models.entry(step.model_key.clone()).or_default();
            acc.input_tokens = acc.input_tokens.saturating_add(step.input_tokens);
            acc.cache_read_tokens = acc.cache_read_tokens.saturating_add(step.cache_read_tokens);
            acc.cache_write_tokens = acc.cache_write_tokens.saturating_add(step.cache_write_tokens);
            acc.output_tokens = acc.output_tokens.saturating_add(step.output_tokens);
            acc.cost_usd += step.cost_usd;
            acc.steps = acc.steps.saturating_add(1);
        }
        if sc.transcript_cost_usd > combined_cost + 1e-9 {
            return (sc.transcript_cost_usd, sc.transcript_cost_by_model.clone());
        }
        return (combined_cost, combined_models);
    }

    if sc.transcript_cost_usd > db_cost + 1e-9 {
        (sc.transcript_cost_usd, sc.transcript_cost_by_model.clone())
    } else {
        (db_cost, db_models.clone())
    }
}

fn apply_cascade_cost_breakdown(
    snapshot: &mut PulseSnapshot,
    transcript_path: Option<&Path>,
    scanned: Option<&transcript::ScannedTranscript>,
    db_tree: Option<&db_telemetry::CascadeDbCostTree>,
    observed_at_ms: i64,
    deadline: Option<Instant>,
) {
    let (brain_dir, main_conv_id) = transcript_path
        .map(transcript::resolve_brain_context)
        .unwrap_or((None, None));
    let mut by_model: BTreeMap<String, transcript::ModelTokenAcc> = BTreeMap::new();

    let (main_cost_usd, children_cost_usd) = if let Some(tree) = db_tree {
        let (main_cost, main_models) = match scanned {
            Some(sc) => reconcile_db_and_transcript(
                tree.main_cost_usd,
                tree.main_steps,
                tree.main_max_step_index,
                &tree.main_by_model,
                sc,
            ),
            None => (tree.main_cost_usd, tree.main_by_model.clone()),
        };
        merge_model_acc_into(&mut by_model, &main_models);

        let mut child_total = tree.children_cost_usd;
        let mut child_overrides: HashMap<String, BTreeMap<String, transcript::ModelTokenAcc>> =
            HashMap::new();
        let mut uncovered_children: Vec<BTreeMap<String, transcript::ModelTokenAcc>> = Vec::new();

        if let (Some(bd), Some(sc)) = (brain_dir.as_ref(), scanned) {
            for child_id in &sc.all_child_ids {
                if deadline.is_some_and(|d| Instant::now() >= d) {
                    break;
                }
                let child_tp = bd
                    .join(child_id)
                    .join(".system_generated")
                    .join("logs")
                    .join("transcript.jsonl");
                if !fs::metadata(&child_tp).is_ok_and(|m| m.is_file()) {
                    continue;
                }
                let Some(child_sc) = transcript::scan_transcript_incremental(
                    &child_tp,
                    None,
                    observed_at_ms,
                    deadline,
                ) else {
                    continue;
                };
                if let Some(db_child) = tree.matched_by_id.get(child_id) {
                    let (reconciled_cost, reconciled_models) = reconcile_db_and_transcript(
                        db_child.total_cost_usd,
                        db_child.steps,
                        db_child.max_step_index,
                        &db_child.models,
                        &child_sc,
                    );
                    if reconciled_cost > db_child.total_cost_usd + 1e-9 {
                        child_total += reconciled_cost - db_child.total_cost_usd;
                        child_overrides.insert(child_id.clone(), reconciled_models);
                    }
                } else if child_sc.steps > 0
                    || child_sc.transcript_cost_usd > 0.0
                    || !child_sc.transcript_cost_by_model.is_empty()
                {
                    child_total += child_sc.transcript_cost_usd;
                    uncovered_children.push(child_sc.transcript_cost_by_model);
                }
            }
        }

        if child_overrides.is_empty() {
            merge_model_acc_into(&mut by_model, &tree.children_by_model);
        } else {
            for (db_id, entry) in &tree.matched_by_id {
                if main_conv_id.as_deref() == Some(db_id.as_str()) {
                    continue;
                }
                if let Some(ov) = child_overrides.get(db_id) {
                    merge_model_acc_into(&mut by_model, ov);
                } else {
                    merge_model_acc_into(&mut by_model, &entry.models);
                }
            }
        }
        for extra in uncovered_children {
            merge_model_acc_into(&mut by_model, &extra);
        }

        (main_cost, child_total)
    } else if let Some(sc) = scanned {
        let main_cost = sc.transcript_cost_usd;
        merge_model_acc_into(&mut by_model, &sc.transcript_cost_by_model);
        let mut child_total = 0.0f64;
        if let Some(bd) = brain_dir.as_ref() {
            for child_id in &sc.all_child_ids {
                if deadline.is_some_and(|d| Instant::now() >= d) {
                    break;
                }
                let child_tp = bd
                    .join(child_id)
                    .join(".system_generated")
                    .join("logs")
                    .join("transcript.jsonl");
                if !fs::metadata(&child_tp).is_ok_and(|m| m.is_file()) {
                    continue;
                }
                if let Some(child_sc) = transcript::scan_transcript_incremental(
                    &child_tp,
                    None,
                    observed_at_ms,
                    deadline,
                ) && (child_sc.steps > 0
                    || child_sc.transcript_cost_usd > 0.0
                    || !child_sc.transcript_cost_by_model.is_empty())
                {
                    child_total += child_sc.transcript_cost_usd;
                    merge_model_acc_into(&mut by_model, &child_sc.transcript_cost_by_model);
                }
            }
        }
        (main_cost, child_total)
    } else {
        (0.0, 0.0)
    };

    let total_cost = main_cost_usd + children_cost_usd;
    if total_cost.is_finite() && total_cost > 0.0 {
        snapshot.cost.cascade_usd = Some(total_cost);
    }
    if children_cost_usd.is_finite() && children_cost_usd > 0.0 {
        snapshot.cost.children_cost_usd = Some(children_cost_usd);
    }
    if !by_model.is_empty() {
        snapshot.model_token_totals = by_model
            .into_iter()
            .map(|(model_key, acc)| ModelTokenTotals {
                model_key,
                input_tokens: acc.input_tokens,
                cache_read_tokens: acc.cache_read_tokens,
                cache_write_tokens: acc.cache_write_tokens,
                output_tokens: acc.output_tokens,
                steps: acc.steps,
            })
            .collect();
    }
}

fn has_cache_telemetry(val: &Value) -> bool {
    let Some(map) = val.as_object() else {
        return false;
    };
    map.contains_key("cache_read_input_tokens")
        || map.contains_key("cache_creation_input_tokens")
        || map.contains_key("cache_read_tokens")
        || map.contains_key("cache_creation_tokens")
}

fn has_complete_turn_cache_usage(val: &Value) -> bool {
    let Some(map) = val.as_object() else {
        return false;
    };
    let has_in = stdin::as_u64_non_neg(map.get("input_tokens")).is_some();
    let has_read = stdin::as_u64_non_neg(
        map.get("cache_read_input_tokens")
            .or_else(|| map.get("cache_read_tokens")),
    )
    .is_some();
    let create_ok = match map
        .get("cache_creation_input_tokens")
        .or_else(|| map.get("cache_creation_tokens"))
    {
        None | Some(Value::Null) => true,
        Some(v) => stdin::as_u64_non_neg(Some(v)).is_some(),
    };
    has_in && has_read && create_ok
}

fn resolve_turn_cache_metrics(
    candidates: [Option<&Value>; 4],
    model_name: &str,
) -> Option<TurnCacheMetrics> {
    let mut saw_telemetry = false;
    for cand in candidates.into_iter().flatten() {
        if has_cache_telemetry(cand) {
            saw_telemetry = true;
        }
        if has_complete_turn_cache_usage(cand)
            && let Some(usage) = stdin::parse_token_usage(cand)
        {
            return Some(calculate_turn_cache_metrics(&usage, model_name));
        }
    }
    if saw_telemetry {
        Some(TurnCacheMetrics {
            available: false,
            ..Default::default()
        })
    } else {
        None
    }
}
