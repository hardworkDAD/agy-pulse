mod speed_tracker;
mod stdin;
mod transcript;
mod workspace;

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::domain::{
    ModelIdentity, PulseSnapshot, TurnCacheMetrics, calculate_turn_cache_metrics,
    resolve_model_display_name,
};
use crate::platform;

/// Result of reading host standard input and local workspace metadata.
#[derive(Debug, Clone)]
pub struct IngestResult {
    pub snapshot: PulseSnapshot,
    pub workspace_dir: PathBuf,
    pub diagnostics: Vec<String>,
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
    /// Parse host stdin payload and enrich with transcript state, output speed,
    /// and local workspace metadata.
    pub fn read_live_pulse(stdin_text: &str, fallback_cwd: &Path) -> IngestResult {
        let observed_at_ms = current_epoch_ms();
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

        let stdin_model_ident = parsed.snapshot.model.as_ref().map(|m| {
            if !m.display_name.is_empty() {
                m.display_name.clone()
            } else {
                m.id.clone()
            }
        });

        let scanned = transcript_path.as_deref().and_then(|tp| {
            transcript::scan_transcript_incremental(tp, stdin_model_ident.as_deref(), observed_at_ms)
        });

        let mut scanned_usage_val: Option<Value> = None;
        let mut last_planner_response = None;

        if let Some(sc) = scanned {
            scanned_usage_val = sc.usage;
            last_planner_response = sc.last_planner_response;

            parsed.snapshot.compaction_count = sc.compaction_count;
            parsed.snapshot.peak_raw_turn_tokens = sc.peak_raw_turn_tokens;
            parsed.snapshot.turn_history_max_by_mode = sc.turn_history_max_by_mode;
            parsed.snapshot.max_historical_cache = sc.max_historical_cache;
            parsed.snapshot.transcript_total_input_tokens = sc.total_input_tokens;
            parsed.snapshot.transcript_total_output_tokens = sc.total_output_tokens;
            parsed.snapshot.child_model_speeds = sc.child_model_speeds;
            parsed.snapshot.model_token_totals = sc.model_token_totals;
            parsed.snapshot.main_model_last_active_ms = sc.main_model_last_active_ms;
            parsed.snapshot.image_exhausted = sc.image_exhausted;
            parsed.snapshot.last_turn_usage = sc.last_turn_usage.as_ref().map(|lt| lt.to_domain());
            parsed.snapshot.last_turn_model = sc
                .last_turn_usage
                .as_ref()
                .and_then(|lt| lt.model.clone())
                .or(if sc.last_known_model.is_empty() {
                    None
                } else {
                    Some(sc.last_known_model)
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

        parsed.snapshot.workspace.current_dir =
            workspace::resolve_current_dir_name(&parsed.workspace_dir);
        parsed.snapshot.workspace.git_branch =
            workspace::resolve_git_branch(&parsed.workspace_dir);
        parsed.snapshot.workspace_meta = workspace::scan_workspace_metadata(&parsed.workspace_dir);

        IngestResult {
            snapshot: parsed.snapshot,
            workspace_dir: parsed.workspace_dir,
            diagnostics: parsed.diagnostics,
        }
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
