use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::CollectedEnrichment;
use crate::config::PulseConfig;
use crate::domain::{CostSnapshot, PulseSnapshot};
use crate::ingest::{HostReader, StdinLiveFields};
use crate::platform;

pub const COLLECTOR_SNAPSHOT_SCHEMA_VERSION: u32 = 1;
const MAX_CONVERSATION_SNAPSHOTS: usize = 32;
const GLOBAL_SNAPSHOT_FILENAME: &str = "snap-global.json";

/// Precomputed per-conversation enrichment snapshot stored in `tmpfs` runtime dir.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectedStatuslineSnapshot {
    pub schema_version: u32,
    pub updated_at_ms: i64,
    pub conversation_key: String,
    pub workspace_dir: PathBuf,
    pub enabled: bool,
    pub snapshot: PulseSnapshot,
    pub config: PulseConfig,
}

/// Precomputed global fallback snapshot stored in `tmpfs` runtime dir for cold starts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalStatuslineSnapshot {
    pub schema_version: u32,
    pub updated_at_ms: i64,
    pub enabled: bool,
    pub today_usd: Option<f64>,
    pub week_usd: Option<f64>,
    pub update_available_version: Option<String>,
    pub dashboard_url: Option<String>,
    pub config: PulseConfig,
}

/// Compute a canonical conversation key from pure in-memory stdin fields.
pub fn compute_conversation_key(live: &StdinLiveFields) -> String {
    if let Some(cid) = live
        .snapshot
        .host
        .conversation_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return cid.to_string();
    }
    if let Some(sid) = live
        .snapshot
        .host
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return sid.to_string();
    }
    if let Some(tp) = live.transcript_path.as_deref() {
        let norm = tp.to_string_lossy().replace('\\', "/");
        let trimmed = norm.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    let cwd_norm = live.workspace_dir.to_string_lossy().replace('\\', "/");
    format!("cwd:{cwd_norm}")
}

/// Return `<runtime_dir>/snap-<sha256_prefix_16>.json`.
pub fn conversation_snapshot_path(conversation_key: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(conversation_key.as_bytes());
    let hex_digest = hex::encode(hasher.finalize());
    let prefix = &hex_digest[..16];
    platform::resolve_runtime_dir().join(format!("snap-{prefix}.json"))
}

/// Return `<runtime_dir>/snap-global.json`.
pub fn global_snapshot_path() -> PathBuf {
    platform::resolve_runtime_dir().join(GLOBAL_SNAPSHOT_FILENAME)
}

fn sanitize_opt_f64(slot: &mut Option<f64>) {
    if let Some(v) = *slot
        && !v.is_finite()
    {
        *slot = None;
    }
}

fn sanitize_f64_zero(slot: &mut f64) {
    if !slot.is_finite() {
        *slot = 0.0;
    }
}

/// Replace any non-finite `f64` values with `None`/`0.0` so JSON serialization never fails.
pub fn sanitize_snapshot_f64(snapshot: &mut PulseSnapshot) {
    if let Some(ref mut ctx) = snapshot.context {
        sanitize_opt_f64(&mut ctx.used_percentage);
    }
    sanitize_opt_f64(&mut snapshot.cost.native_total_usd);
    sanitize_opt_f64(&mut snapshot.cost.cascade_usd);
    sanitize_opt_f64(&mut snapshot.cost.children_cost_usd);
    sanitize_opt_f64(&mut snapshot.cost.today_usd);
    sanitize_opt_f64(&mut snapshot.cost.week_usd);
    if let Some(ref mut tcm) = snapshot.turn_cache_metrics {
        sanitize_f64_zero(&mut tcm.hit_rate);
    }
    sanitize_opt_f64(&mut snapshot.output_speed_tok_s);
    sanitize_opt_f64(&mut snapshot.ttft_sec);
    for child in &mut snapshot.child_model_speeds {
        sanitize_f64_zero(&mut child.speed_tok_s);
        sanitize_opt_f64(&mut child.ttft_sec);
    }
    if let Some(ref mut quota) = snapshot.quota {
        for provider in [&mut quota.google, &mut quota.claude, &mut quota.argon] {
            if let Some(ref mut w) = provider.five_hour {
                sanitize_f64_zero(&mut w.remaining_fraction);
            }
            if let Some(ref mut w) = provider.weekly {
                sanitize_f64_zero(&mut w.remaining_fraction);
            }
        }
        for extra in &mut quota.extras {
            if let Some(ref mut w) = extra.quota.five_hour {
                sanitize_f64_zero(&mut w.remaining_fraction);
            }
            if let Some(ref mut w) = extra.quota.weekly {
                sanitize_f64_zero(&mut w.remaining_fraction);
            }
        }
    }
}

/// Sanitize floating-point fields in `PulseConfig` before JSON serialization.
pub fn sanitize_config_f64(cfg: &mut PulseConfig) {
    if !cfg.thresholds.warning.is_finite() {
        cfg.thresholds.warning = 0.7;
    }
    if !cfg.thresholds.critical.is_finite() {
        cfg.thresholds.critical = 0.9;
    }
    if let Some(ref mut cht) = cfg.display.cache_hit_thresholds {
        sanitize_opt_f64(&mut cht.excellent);
        sanitize_opt_f64(&mut cht.partial);
    }
    sanitize_opt_f64(&mut cfg.display.model_speed_expire_minutes);
}

/// Read a conversation snapshot from `tmpfs` runtime directory if schema matches.
pub fn read_conversation_snapshot(conversation_key: &str) -> Option<CollectedStatuslineSnapshot> {
    let raw = fs::read(conversation_snapshot_path(conversation_key)).ok()?;
    let parsed: CollectedStatuslineSnapshot = serde_json::from_slice(&raw).ok()?;
    if parsed.schema_version != COLLECTOR_SNAPSHOT_SCHEMA_VERSION {
        return None;
    }
    Some(parsed)
}

/// Read the global fallback snapshot from `tmpfs` runtime directory if schema matches.
pub fn read_global_snapshot() -> Option<GlobalStatuslineSnapshot> {
    let raw = fs::read(global_snapshot_path()).ok()?;
    let parsed: GlobalStatuslineSnapshot = serde_json::from_slice(&raw).ok()?;
    if parsed.schema_version != COLLECTOR_SNAPSHOT_SCHEMA_VERSION {
        return None;
    }
    Some(parsed)
}

/// Atomically persist both conversation and global snapshots and prune excess conversation snapshots.
pub(crate) fn write_snapshots_and_prune(
    conversation_key: &str,
    enrichment: &CollectedEnrichment,
    updated_at_ms: i64,
) {
    let mut snapshot = enrichment.snapshot.clone();
    let mut config = enrichment.config.clone();
    sanitize_snapshot_f64(&mut snapshot);
    sanitize_config_f64(&mut config);

    let conv_snap = CollectedStatuslineSnapshot {
        schema_version: COLLECTOR_SNAPSHOT_SCHEMA_VERSION,
        updated_at_ms,
        conversation_key: conversation_key.to_string(),
        workspace_dir: enrichment.workspace_dir.clone(),
        enabled: enrichment.enabled,
        snapshot: snapshot.clone(),
        config: config.clone(),
    };
    if let Ok(bytes) = serde_json::to_vec(&conv_snap) {
        let _ = platform::write_cache_atomic_0600(&conversation_snapshot_path(conversation_key), &bytes);
    }

    let global_snap = GlobalStatuslineSnapshot {
        schema_version: COLLECTOR_SNAPSHOT_SCHEMA_VERSION,
        updated_at_ms,
        enabled: enrichment.enabled,
        today_usd: snapshot.cost.today_usd,
        week_usd: snapshot.cost.week_usd,
        update_available_version: snapshot.update_available_version.clone(),
        dashboard_url: snapshot.dashboard_url.clone(),
        config,
    };
    if let Ok(bytes) = serde_json::to_vec(&global_snap) {
        let _ = platform::write_cache_atomic_0600(&global_snapshot_path(), &bytes);
    }

    prune_old_conversation_snapshots();
}

fn prune_old_conversation_snapshots() {
    let runtime_dir = platform::resolve_runtime_dir();
    let Ok(entries) = fs::read_dir(&runtime_dir) else {
        return;
    };
    let mut files: Vec<(SystemTime, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !name_str.starts_with("snap-")
            || !name_str.ends_with(".json")
            || name_str == GLOBAL_SNAPSHOT_FILENAME
        {
            continue;
        }
        let path = entry.path();
        if let Ok(meta) = entry.metadata()
            && meta.is_file()
        {
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            files.push((mtime, path));
        }
    }
    if files.len() <= MAX_CONVERSATION_SNAPSHOTS {
        return;
    }
    files.sort_by_key(|(mtime, _)| *mtime);
    let to_remove = files.len() - MAX_CONVERSATION_SNAPSHOTS;
    for (_, path) in files.into_iter().take(to_remove) {
        let _ = fs::remove_file(path);
    }
}

/// Losslessly overlay fresh in-memory `stdin` fields on top of a cached conversation snapshot.
pub fn merge_live_stdin_into_snapshot(
    cached: &CollectedStatuslineSnapshot,
    stdin_text: &str,
    fallback_cwd: &Path,
) -> PulseSnapshot {
    let live = HostReader::parse_stdin_live_fields(stdin_text, fallback_cwd);
    let mut out = cached.snapshot.clone();

    out.observed_at_ms = live.snapshot.observed_at_ms;
    out.has_conversation_identity |= live.snapshot.has_conversation_identity;

    if live.snapshot.host.conversation_id.is_some() {
        out.host.conversation_id = live.snapshot.host.conversation_id;
    }
    if live.snapshot.host.session_id.is_some() {
        out.host.session_id = live.snapshot.host.session_id;
    }
    if live.snapshot.host.plan_tier.is_some() {
        out.host.plan_tier = live.snapshot.host.plan_tier;
    }
    if live.snapshot.host.cycle_mode.is_some() {
        out.host.cycle_mode = live.snapshot.host.cycle_mode;
    }
    if live.snapshot.host.username.is_some() {
        out.host.username = live.snapshot.host.username;
    }
    if live.snapshot.host.email.is_some() {
        out.host.email = live.snapshot.host.email;
    }

    if live.snapshot.model.is_some() {
        out.model = live.snapshot.model;
    }

    out.workspace.current_dir = live.snapshot.workspace.current_dir;
    if live.workspace_dir != cached.workspace_dir {
        out.workspace.git_branch = None;
        out.workspace_meta = Default::default();
    }

    if let Some(mut live_ctx) = live.snapshot.context {
        if let Some(ref old_ctx) = out.context {
            if live_ctx.context_window_size == 0 {
                live_ctx.context_window_size = old_ctx.context_window_size;
            }
            if live_ctx.current_usage.is_none() {
                live_ctx.current_usage = old_ctx.current_usage.clone();
            }
        }
        out.context = Some(live_ctx);
    }

    if live.snapshot.cost.native_total_usd.is_some() {
        out.cost.native_total_usd = live.snapshot.cost.native_total_usd;
    }
    if live.snapshot.terminal_width.is_some() {
        out.terminal_width = live.snapshot.terminal_width;
    }
    if live.snapshot.quota.is_some() {
        out.quota = live.snapshot.quota;
    }
    if live.snapshot.turn_cache_metrics.is_some() {
        out.turn_cache_metrics = live.snapshot.turn_cache_metrics;
    }
    if live.snapshot.task_count > 0 {
        out.task_count = live.snapshot.task_count;
    }

    out
}

/// Overlay fresh in-memory `stdin` fields onto the global fallback snapshot for cold starts.
pub fn merge_live_stdin_into_global(
    global: &GlobalStatuslineSnapshot,
    stdin_text: &str,
    fallback_cwd: &Path,
) -> PulseSnapshot {
    let live = HostReader::parse_stdin_live_fields(stdin_text, fallback_cwd);
    let mut out = live.snapshot;
    out.cost = CostSnapshot {
        today_usd: global.today_usd,
        week_usd: global.week_usd,
        ..out.cost
    };
    out.update_available_version = global.update_available_version.clone();
    out.dashboard_url = global.dashboard_url.clone();
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::domain::{ContextWindowSnapshot, ModelIdentity, TokenUsage, WorkspaceInfo, WorkspaceMetadata};

    #[test]
    fn merge_live_stdin_preserves_cached_enrichment_and_clears_cross_project_meta() {
        let cached = CollectedStatuslineSnapshot {
            schema_version: COLLECTOR_SNAPSHOT_SCHEMA_VERSION,
            updated_at_ms: 1_000,
            conversation_key: "conv-1".to_string(),
            workspace_dir: PathBuf::from("/tmp/proj-a"),
            enabled: true,
            snapshot: PulseSnapshot {
                has_conversation_identity: true,
                model: Some(ModelIdentity {
                    id: "old-model".to_string(),
                    display_name: "Old Model".to_string(),
                    effort: None,
                }),
                workspace: WorkspaceInfo {
                    current_dir: "proj-a".to_string(),
                    git_branch: Some("feat/a".to_string()),
                },
                workspace_meta: WorkspaceMetadata {
                    rules_count: 3,
                    ..Default::default()
                },
                context: Some(ContextWindowSnapshot {
                    context_window_size: 200_000,
                    used_percentage: Some(15.0),
                    current_usage: Some(TokenUsage {
                        input_tokens: 30_000,
                        output_tokens: 100,
                        cache_read_input_tokens: 10_000,
                        cache_creation_input_tokens: 0,
                        cache_exclusive: false,
                    }),
                    ..Default::default()
                }),
                cost: CostSnapshot {
                    today_usd: Some(4.25),
                    week_usd: Some(18.5),
                    ..Default::default()
                },
                server_compaction_limit: Some(160_000),
                dashboard_url: Some("http://127.0.0.1:3789".to_string()),
                ..Default::default()
            },
            config: PulseConfig::default(),
        };

        let same_ws_stdin = serde_json::json!({
            "conversation_id": "conv-1",
            "model": { "id": "claude-opus-4-6-thinking", "display_name": "Claude Opus 4.6 (Thinking)" },
            "workspace": { "current_dir": "/tmp/proj-a" },
            "context_window": { "used_percentage": 42.0, "total_input_tokens": 84_000 }
        })
        .to_string();

        let merged_same = merge_live_stdin_into_snapshot(&cached, &same_ws_stdin, Path::new("/tmp"));
        assert_eq!(merged_same.model.as_ref().unwrap().display_name, "Claude Opus 4.6 (Thinking)");
        assert_eq!(merged_same.context.as_ref().unwrap().context_window_size, 200_000);
        assert!(merged_same.context.as_ref().unwrap().current_usage.is_some());
        assert_eq!(merged_same.server_compaction_limit, Some(160_000));
        assert_eq!(merged_same.cost.today_usd, Some(4.25));
        assert_eq!(merged_same.workspace.git_branch.as_deref(), Some("feat/a"));
        assert_eq!(merged_same.workspace_meta.rules_count, 3);

        let other_ws_stdin = serde_json::json!({
            "conversation_id": "conv-1",
            "workspace": { "current_dir": "/tmp/proj-b" }
        })
        .to_string();
        let merged_other = merge_live_stdin_into_snapshot(&cached, &other_ws_stdin, Path::new("/tmp"));
        assert_eq!(merged_other.workspace.current_dir, "proj-b");
        assert_eq!(merged_other.workspace.git_branch, None);
        assert_eq!(merged_other.workspace_meta.rules_count, 0);
    }

    #[test]
    fn sanitize_snapshot_f64_handles_non_finite_values() {
        let mut snap = PulseSnapshot {
            output_speed_tok_s: Some(f64::NAN),
            cost: CostSnapshot {
                today_usd: Some(f64::INFINITY),
                ..Default::default()
            },
            ..Default::default()
        };
        sanitize_snapshot_f64(&mut snap);
        assert_eq!(snap.output_speed_tok_s, None);
        assert_eq!(snap.cost.today_usd, None);
        assert!(serde_json::to_vec(&snap).is_ok());
    }
}
