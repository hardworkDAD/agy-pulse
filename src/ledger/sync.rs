use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::domain::{
    ProviderQuotaSummary, QuotaSnapshot, TokenUsage, UNASSIGNED_ACCOUNT_ID, calculate_token_cost,
    normalize_account_id,
};
use crate::ingest::HostReader;
use crate::platform;

use super::LedgerError;
use super::query::{
    ConvLineageMeta, LedgerSource, local_date_key, local_today_bounds,
    resolve_root_conversation_id,
};
use super::schema::{
    DEFAULT_BUSY_RETRIES, read_local_device_id, run_with_busy_retry, run_with_busy_retry_n,
};

pub(super) const SYNC_THROTTLE_MS: i64 = 15_000;
const ROLLING_CURSOR_MAX_AGE_MS: i64 = 24 * 60 * 60 * 1000;
const SEVEN_DAY_WINDOW_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const WEEK_START_JITTER_MS: i64 = 60_000;

#[derive(Debug, Clone)]
pub struct SourceRoot {
    pub root: PathBuf,
    pub source: LedgerSource,
}

#[derive(Debug, Clone, Default)]
pub struct LedgerSyncOptions {
    pub source_roots: Option<Vec<SourceRoot>>,
    pub force: bool,
    pub throttle_ms: Option<i64>,
    pub now_ms: Option<i64>,
    pub active_account_id: Option<String>,
    pub default_account_id: Option<String>,
    pub device_name: Option<String>,
    pub plan_tier: Option<String>,
}

/// Result summary returned by `LedgerStore::assign_unassigned_account`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountAssignReport {
    pub account_id: String,
    pub conversations_updated: usize,
    pub steps_updated: usize,
    pub dry_run: bool,
}

/// Validate and normalize an account email address (`<non-empty>@<non-empty>`, no whitespace/control chars).
pub fn validate_and_normalize_account_email(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let (local, domain) = trimmed.split_once('@')?;
    if local.is_empty() || domain.is_empty() || domain.contains('@') {
        return None;
    }
    Some(trimmed.to_ascii_lowercase())
}

fn normalize_non_unassigned_account(raw: Option<&str>) -> Option<String> {
    raw.and_then(normalize_account_id)
        .filter(|acct| acct != UNASSIGNED_ACCOUNT_ID)
}

fn resolve_effective_device_name(hint: Option<&str>) -> String {
    hint.and_then(platform::sanitize_device_name)
        .or_else(platform::current_hostname)
        .unwrap_or_else(|| "local".to_string())
}

struct BatchSyncPolicy {
    allow_active_account: bool,
    active_account_id: Option<String>,
    default_account_id: Option<String>,
    device_name: String,
    plan_tier: Option<String>,
    now_ms: i64,
}

impl BatchSyncPolicy {
    fn from_options(options: &LedgerSyncOptions, allow_active_account: bool, now_ms: i64) -> Self {
        Self {
            allow_active_account,
            active_account_id: if allow_active_account {
                normalize_non_unassigned_account(options.active_account_id.as_deref())
            } else {
                None
            },
            default_account_id: normalize_non_unassigned_account(
                options.default_account_id.as_deref(),
            ),
            device_name: resolve_effective_device_name(options.device_name.as_deref()),
            plan_tier: options
                .plan_tier
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned),
            now_ms,
        }
    }

    fn resolve_fallback_account(
        &self,
        source: LedgerSource,
        last_observed_email: Option<&str>,
    ) -> String {
        if self.allow_active_account
            && source == LedgerSource::Cli
            && let Some(ref active) = self.active_account_id
        {
            return active.clone();
        }
        if let Some(ref default_acct) = self.default_account_id {
            return default_acct.clone();
        }
        if let Some(last) = last_observed_email {
            return last.to_string();
        }
        UNASSIGNED_ACCOUNT_ID.to_string()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerSyncReport {
    pub ok: bool,
    pub total_dbs: usize,
    pub cli_dbs: usize,
    pub ide_dbs: usize,
    pub newly_synced_dbs: usize,
    pub steps_upserted: usize,
    pub duration_ms: u64,
    pub ledger_path: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SingleSyncReport {
    pub ok: bool,
    pub skipped: bool,
    pub synced_dbs: usize,
    pub steps_upserted: usize,
    pub ledger_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct RollingCostInput<'a> {
    pub cursor_id: &'a str,
    pub cascade_root_id: Option<&'a str>,
    pub live_cost_usd: Option<f64>,
    pub seven_day_reset_at_ms: Option<i64>,
    pub now_ms: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RollingCostTotals {
    pub today_usd: f64,
    pub week_usd: f64,
}

pub(super) fn current_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn mtime_ms_from_meta(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn sidecar_path(db_path: &Path, suffix: &str) -> PathBuf {
    let mut os = OsString::from(db_path.as_os_str());
    os.push(suffix);
    PathBuf::from(os)
}

fn get_db_signature(db_path: &Path) -> Option<(String, i64)> {
    let meta = fs::metadata(db_path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let db_len = meta.len();
    let db_mtime = mtime_ms_from_meta(&meta);
    let wal_path = sidecar_path(db_path, "-wal");
    let (wal_len, wal_mtime) = match fs::metadata(&wal_path) {
        Ok(wm) if wm.is_file() => (wm.len(), mtime_ms_from_meta(&wm)),
        _ => (0, 0),
    };
    let max_mtime = if wal_len > 0 {
        db_mtime.max(wal_mtime)
    } else {
        db_mtime
    };
    Some((format!("{db_len}:{db_mtime}:{wal_len}:{wal_mtime}"), max_mtime))
}

fn resolve_source_roots(options: &LedgerSyncOptions) -> Vec<SourceRoot> {
    let raw: Vec<SourceRoot> = if let Some(ref explicit) = options.source_roots {
        explicit.clone()
    } else {
        let mut entries = Vec::new();
        for root in platform::get_antigravity_roots() {
            entries.push(SourceRoot {
                root,
                source: LedgerSource::Cli,
            });
        }
        for root in platform::get_antigravity_ide_roots() {
            entries.push(SourceRoot {
                root,
                source: LedgerSource::Ide,
            });
        }
        entries
    };

    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut deduped = Vec::new();
    for entry in raw {
        if entry.root.as_os_str().is_empty() {
            continue;
        }
        let key = fs::canonicalize(&entry.root).unwrap_or_else(|_| entry.root.clone());
        if seen.insert(key) {
            deduped.push(entry);
        }
    }
    deduped
}

struct DiscoveredDb {
    conversation_id: String,
    db_path: PathBuf,
    source: LedgerSource,
}

fn list_all_conversation_dbs(source_roots: &[SourceRoot]) -> Vec<DiscoveredDb> {
    let mut dbs = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();
    for entry in source_roots {
        let conv_dir = entry.root.join("conversations");
        let Ok(rd) = fs::read_dir(&conv_dir) else {
            continue;
        };
        for item in rd.flatten() {
            let path = item.path();
            if path.extension().and_then(|s| s.to_str()) != Some("db") {
                continue;
            }
            let Some(cid) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
            else {
                continue;
            };
            if !seen_ids.insert(cid.clone()) {
                continue;
            }
            if fs::metadata(&path).is_ok_and(|m| m.is_file()) {
                dbs.push(DiscoveredDb {
                    conversation_id: cid,
                    db_path: path,
                    source: entry.source,
                });
            }
        }
    }
    dbs
}

fn find_conversation_db(
    conversation_id: &str,
    source_roots: &[SourceRoot],
) -> Option<(PathBuf, LedgerSource)> {
    if conversation_id.trim().is_empty() {
        return None;
    }
    for entry in source_roots {
        let candidate = entry
            .root
            .join("conversations")
            .join(format!("{conversation_id}.db"));
        if fs::metadata(&candidate).is_ok_and(|m| m.is_file()) {
            return Some((candidate, entry.source));
        }
    }
    None
}

struct IngestedDbRow {
    conversation_id: String,
    db_path: String,
    sig: String,
    last_synced_ms: i64,
    workspace: Option<String>,
    branch: Option<String>,
    created_at_ms: Option<i64>,
    cascade_id: String,
    parent_conversation_id: Option<String>,
    root_conversation_id: Option<String>,
    db_mtime_ms: i64,
    source: LedgerSource,
}

struct IngestedStepRow {
    step_uid: String,
    conversation_id: String,
    gen_idx: i64,
    entry_idx: i64,
    step_index: Option<i64>,
    ts_ms: i64,
    model: String,
    raw_model: String,
    input_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
    output_tokens: i64,
    cost_usd: f64,
    stream_sec: Option<f64>,
    ttft_sec: Option<f64>,
    valid_stream: i64,
    valid_ttft: i64,
    parent_conversation_id: Option<String>,
    root_conversation_id: Option<String>,
    workspace: Option<String>,
    branch: Option<String>,
    source: LedgerSource,
}

struct IngestedConversationBatch {
    db_row: IngestedDbRow,
    steps: Vec<IngestedStepRow>,
}

fn resolve_step_timestamp(
    step_indices: &[u64],
    last_step_index: Option<u64>,
    bot_uuid: Option<&str>,
    step_timestamps: &std::collections::BTreeMap<i64, i64>,
    bot_timestamps: &std::collections::BTreeMap<String, i64>,
    fallback_ts_ms: i64,
) -> (i64, Option<i64>) {
    let mut resolved_step_index = step_indices
        .last()
        .and_then(|&v| i64::try_from(v).ok());

    for &idx_u64 in step_indices.iter().rev() {
        if let Ok(idx_i64) = i64::try_from(idx_u64)
            && let Some(&ts) = step_timestamps.get(&idx_i64)
        {
            return (ts, Some(idx_i64));
        }
    }

    if let Some(last_u64) = last_step_index
        && let Ok(last_i64) = i64::try_from(last_u64)
    {
        if resolved_step_index.is_none() {
            resolved_step_index = Some(last_i64);
        }
        if let Some(&ts) = step_timestamps.get(&last_i64) {
            return (ts, Some(last_i64));
        }
    }

    if let Some(bot) = bot_uuid
        && let Some(&ts) = bot_timestamps.get(bot)
    {
        return (ts, resolved_step_index);
    }

    (fallback_ts_ms, resolved_step_index)
}

struct IngestionTarget<'a> {
    conversation_id: &'a str,
    db_path: &'a Path,
    sig: &'a str,
    max_mtime_ms: i64,
    now_ms: i64,
    source: LedgerSource,
    fallback_parent_id: Option<&'a str>,
    fallback_root_id: Option<&'a str>,
}

fn build_ingestion_records(target: &IngestionTarget<'_>) -> Option<IngestedConversationBatch> {
    let conversation_id = target.conversation_id;
    let db_path = target.db_path;
    let sig = target.sig;
    let max_mtime_ms = target.max_mtime_ms;
    let now_ms = target.now_ms;
    let source = target.source;
    let fallback_parent_id = target.fallback_parent_id;
    let fallback_root_id = target.fallback_root_id;
    let rec = HostReader::read_conversation_db(db_path)?;
    let db_mtime_ms = if max_mtime_ms > 0 {
        max_mtime_ms
    } else {
        now_ms
    };
    let traj = rec.trajectory.as_ref();
    let workspace = traj.and_then(|t| t.workspace.clone());
    let branch = traj.and_then(|t| t.branch.clone());
    let created_at_ms = traj.and_then(|t| t.created_at_ms);
    let fallback_ts_ms = created_at_ms.unwrap_or(db_mtime_ms);

    let mut parent_conv_id = rec
        .parent_cascade_id
        .clone()
        .or_else(|| fallback_parent_id.map(ToOwned::to_owned));
    let mut root_conv_id = rec
        .root_cascade_id
        .clone()
        .or_else(|| fallback_root_id.map(ToOwned::to_owned));
    let mut steps = Vec::new();

    for row in &rec.gen_rows {
        if parent_conv_id.is_none() && row.parent_cascade_id.is_some() {
            parent_conv_id = row.parent_cascade_id.clone();
        }
        if root_conv_id.is_none() && row.root_cascade_id.is_some() {
            root_conv_id = row.root_cascade_id.clone();
        }
        let step_parent = row.parent_cascade_id.clone().or_else(|| parent_conv_id.clone());
        let step_root = row.root_cascade_id.clone().or_else(|| root_conv_id.clone());

        for (entry_idx, entry) in row.steps.iter().enumerate() {
            let (ts_ms, step_index) = resolve_step_timestamp(
                &row.step_indices,
                entry.last_step_index,
                entry.bot_uuid.as_deref(),
                &rec.step_timestamps,
                &rec.bot_timestamps,
                fallback_ts_ms,
            );
            let usage = TokenUsage {
                input_tokens: entry.input_tokens,
                output_tokens: entry.output_tokens,
                cache_read_input_tokens: entry.cache_read_tokens,
                cache_creation_input_tokens: entry.cache_write_tokens,
                cache_exclusive: false,
            };
            let cost_usd = calculate_token_cost(&entry.step_model, &usage, false, false)
                .filter(|c| c.is_finite() && *c >= 0.0)
                .unwrap_or(0.0);

            steps.push(IngestedStepRow {
                step_uid: format!("{conversation_id}:{}:{entry_idx}", row.idx),
                conversation_id: conversation_id.to_string(),
                gen_idx: row.idx,
                entry_idx: entry_idx as i64,
                step_index,
                ts_ms,
                model: entry.model_key.clone(),
                raw_model: entry.step_model.clone(),
                input_tokens: i64::try_from(entry.input_tokens).unwrap_or(i64::MAX),
                cache_read_tokens: i64::try_from(entry.cache_read_tokens).unwrap_or(i64::MAX),
                cache_write_tokens: i64::try_from(entry.cache_write_tokens).unwrap_or(i64::MAX),
                output_tokens: i64::try_from(entry.output_tokens).unwrap_or(i64::MAX),
                cost_usd,
                stream_sec: entry.stream_sec,
                ttft_sec: entry.ttft_sec,
                valid_stream: i64::from(entry.valid_stream),
                valid_ttft: i64::from(entry.valid_ttft),
                parent_conversation_id: step_parent.clone(),
                root_conversation_id: step_root.clone(),
                workspace: workspace.clone(),
                branch: branch.clone(),
                source,
            });
        }
    }

    let db_row = IngestedDbRow {
        conversation_id: conversation_id.to_string(),
        db_path: db_path.to_string_lossy().into_owned(),
        sig: sig.to_string(),
        last_synced_ms: now_ms,
        workspace,
        branch,
        created_at_ms,
        cascade_id: rec
            .cascade_id
            .or_else(|| traj.and_then(|t| t.cascade_id.clone()))
            .unwrap_or_else(|| conversation_id.to_string()),
        parent_conversation_id: parent_conv_id,
        root_conversation_id: root_conv_id,
        db_mtime_ms,
        source,
    };

    Some(IngestedConversationBatch { db_row, steps })
}

fn read_last_observed_email_from_conn(conn: &Connection) -> Result<Option<String>, rusqlite::Error> {
    let raw = conn
        .query_row(
            "SELECT value FROM ledger_meta WHERE key = 'last_observed_email'",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()?;
    Ok(normalize_non_unassigned_account(raw.as_deref()))
}

fn upsert_sync_device_on_conn(
    conn: &Connection,
    account_id: &str,
    device_id: &str,
    device_name: &str,
    is_local: bool,
    last_seen_ms: i64,
    plan_tier: Option<&str>,
) -> Result<(), rusqlite::Error> {
    let normalized_account =
        normalize_account_id(account_id).unwrap_or_else(|| UNASSIGNED_ACCOUNT_ID.to_string());
    let trimmed_device_id = device_id.trim();
    let sanitized_name = platform::sanitize_device_name(device_name);
    let effective_device_name = sanitized_name.as_deref().unwrap_or(trimmed_device_id);

    conn.execute(
        "INSERT INTO sync_devices (account_id, device_id, device_name, is_local, last_seen_ms, plan_tier)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(account_id, device_id) DO UPDATE SET
           device_name = excluded.device_name,
           is_local = excluded.is_local,
           last_seen_ms = MAX(sync_devices.last_seen_ms, excluded.last_seen_ms),
           plan_tier = COALESCE(excluded.plan_tier, sync_devices.plan_tier)",
        params![
            &normalized_account,
            trimmed_device_id,
            effective_device_name,
            i64::from(is_local),
            last_seen_ms,
            plan_tier,
        ],
    )?;
    if is_local && normalized_account != UNASSIGNED_ACCOUNT_ID {
        conn.execute(
            "DELETE FROM sync_devices
             WHERE account_id = 'unassigned'
               AND device_id = ?1
               AND NOT EXISTS (
                 SELECT 1 FROM ingested_dbs WHERE device_id = ?1 AND account_id = 'unassigned'
               )
               AND NOT EXISTS (
                 SELECT 1 FROM quota_snapshots WHERE device_id = ?1 AND account_id = 'unassigned'
               )",
            params![trimmed_device_id],
        )?;
    }
    Ok(())
}

fn upsert_batches(
    conn: &mut Connection,
    batches: &[IngestedConversationBatch],
    full_scan_ts_ms: Option<i64>,
    policy: &BatchSyncPolicy,
    max_retries: u32,
) -> Result<usize, LedgerError> {
    run_with_busy_retry_n(max_retries, || {
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let local_device_id = read_local_device_id(&tx)?;
        let last_observed = read_last_observed_email_from_conn(&tx)?;
        let mut steps_upserted = 0usize;
        let mut touched_accounts: BTreeSet<String> = BTreeSet::new();
        {
            let mut check_db_stmt = tx.prepare(
                "SELECT account_id, device_id FROM ingested_dbs WHERE conversation_id = ?1",
            )?;
            let mut check_step_acct_stmt = tx.prepare(
                "SELECT account_id FROM telemetry_steps
                 WHERE conversation_id = ?1
                   AND (device_id = ?2 OR device_id = '')
                   AND account_id != 'unassigned'
                   AND account_id != ''
                 LIMIT 1",
            )?;
            let mut upgrade_steps_stmt = tx.prepare(
                "UPDATE telemetry_steps
                 SET account_id = ?1
                 WHERE conversation_id = ?2
                   AND (device_id = ?3 OR device_id = '')
                   AND (account_id = 'unassigned' OR account_id = '')",
            )?;
            let mut db_stmt = tx.prepare(
                "INSERT INTO ingested_dbs (
                   conversation_id, db_path, sig, last_synced_ms,
                   workspace, branch, created_at_ms, cascade_id,
                   parent_conversation_id, root_conversation_id, db_mtime_ms, source,
                   account_id, device_id
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                 ON CONFLICT(conversation_id) DO UPDATE SET
                   db_path = excluded.db_path,
                   sig = excluded.sig,
                   last_synced_ms = excluded.last_synced_ms,
                   workspace = excluded.workspace,
                   branch = excluded.branch,
                   created_at_ms = excluded.created_at_ms,
                   cascade_id = excluded.cascade_id,
                   parent_conversation_id = excluded.parent_conversation_id,
                   root_conversation_id = excluded.root_conversation_id,
                   db_mtime_ms = excluded.db_mtime_ms,
                   source = excluded.source,
                   account_id = CASE
                     WHEN ingested_dbs.account_id != 'unassigned' AND ingested_dbs.account_id != '' THEN ingested_dbs.account_id
                     ELSE excluded.account_id
                   END,
                   device_id = excluded.device_id
                 WHERE ingested_dbs.device_id = excluded.device_id OR ingested_dbs.device_id = ''",
            )?;
            let mut step_stmt = tx.prepare(
                "INSERT INTO telemetry_steps (
                   step_uid, conversation_id, gen_idx, entry_idx, step_index, ts_ms,
                   model, raw_model, input_tokens, cache_read_tokens, cache_write_tokens, output_tokens,
                   cost_usd, stream_sec, ttft_sec, valid_stream, valid_ttft,
                   parent_conversation_id, root_conversation_id, workspace, branch, source,
                   account_id, device_id
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)
                 ON CONFLICT(step_uid) DO UPDATE SET
                   conversation_id = excluded.conversation_id,
                   gen_idx = excluded.gen_idx,
                   entry_idx = excluded.entry_idx,
                   step_index = excluded.step_index,
                   ts_ms = excluded.ts_ms,
                   model = excluded.model,
                   raw_model = excluded.raw_model,
                   input_tokens = excluded.input_tokens,
                   cache_read_tokens = excluded.cache_read_tokens,
                   cache_write_tokens = excluded.cache_write_tokens,
                   output_tokens = excluded.output_tokens,
                   cost_usd = excluded.cost_usd,
                   stream_sec = excluded.stream_sec,
                   ttft_sec = excluded.ttft_sec,
                   valid_stream = excluded.valid_stream,
                   valid_ttft = excluded.valid_ttft,
                   parent_conversation_id = excluded.parent_conversation_id,
                   root_conversation_id = excluded.root_conversation_id,
                   workspace = excluded.workspace,
                   branch = excluded.branch,
                   source = excluded.source,
                   account_id = CASE
                     WHEN telemetry_steps.account_id != 'unassigned' AND telemetry_steps.account_id != '' THEN telemetry_steps.account_id
                     ELSE excluded.account_id
                   END,
                   device_id = excluded.device_id
                 WHERE telemetry_steps.device_id = excluded.device_id OR telemetry_steps.device_id = ''",
            )?;

            for batch in batches {
                let d = &batch.db_row;
                let existing_row: Option<(String, String)> = check_db_stmt
                    .query_row(params![d.conversation_id], |r| Ok((r.get(0)?, r.get(1)?)))
                    .optional()?;
                if let Some((_, ref existing_dev)) = existing_row
                    && !existing_dev.is_empty()
                    && existing_dev != &local_device_id
                {
                    continue;
                }

                let existing_acct = existing_row
                    .as_ref()
                    .and_then(|(acct, _)| normalize_account_id(acct))
                    .filter(|acct| acct != UNASSIGNED_ACCOUNT_ID);
                let account_id = match existing_acct {
                    Some(ref acct) => acct.clone(),
                    None => check_step_acct_stmt
                        .query_row(params![d.conversation_id, &local_device_id], |r| {
                            r.get::<_, String>(0)
                        })
                        .optional()?
                        .and_then(|s| normalize_account_id(&s))
                        .filter(|acct| acct != UNASSIGNED_ACCOUNT_ID)
                        .unwrap_or_else(|| {
                            policy.resolve_fallback_account(d.source, last_observed.as_deref())
                        }),
                };

                if existing_row.is_some()
                    && existing_acct.is_none()
                    && account_id != UNASSIGNED_ACCOUNT_ID
                {
                    upgrade_steps_stmt.execute(params![
                        &account_id,
                        &d.conversation_id,
                        &local_device_id,
                    ])?;
                }

                db_stmt.execute(params![
                    d.conversation_id,
                    d.db_path,
                    d.sig,
                    d.last_synced_ms,
                    d.workspace,
                    d.branch,
                    d.created_at_ms,
                    d.cascade_id,
                    d.parent_conversation_id,
                    d.root_conversation_id,
                    d.db_mtime_ms,
                    d.source.as_str(),
                    &account_id,
                    &local_device_id,
                ])?;
                for s in &batch.steps {
                    let changed = step_stmt.execute(params![
                        s.step_uid,
                        s.conversation_id,
                        s.gen_idx,
                        s.entry_idx,
                        s.step_index,
                        s.ts_ms,
                        s.model,
                        s.raw_model,
                        s.input_tokens,
                        s.cache_read_tokens,
                        s.cache_write_tokens,
                        s.output_tokens,
                        s.cost_usd,
                        s.stream_sec,
                        s.ttft_sec,
                        s.valid_stream,
                        s.valid_ttft,
                        s.parent_conversation_id,
                        s.root_conversation_id,
                        s.workspace,
                        s.branch,
                        s.source.as_str(),
                        &account_id,
                        &local_device_id,
                    ])?;
                    if changed > 0 {
                        steps_upserted += 1;
                    }
                }
                touched_accounts.insert(account_id);
            }
        }
        for acct in &touched_accounts {
            let tier = match (&policy.active_account_id, &policy.plan_tier) {
                (Some(active), Some(t)) if active == acct => Some(t.as_str()),
                (None, Some(t)) => Some(t.as_str()),
                _ => None,
            };
            upsert_sync_device_on_conn(
                &tx,
                acct,
                &local_device_id,
                &policy.device_name,
                true,
                policy.now_ms,
                tier,
            )?;
        }
        if let Some(ts) = full_scan_ts_ms {
            tx.execute(
                "INSERT OR REPLACE INTO ledger_meta (key, value) VALUES ('last_full_scan_ms', ?1)",
                params![ts.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(steps_upserted)
    })
    .map_err(LedgerError::from)
}

pub(super) fn sync_all(
    conn: &mut Connection,
    ledger_path: &Path,
    options: &LedgerSyncOptions,
) -> Result<LedgerSyncReport, LedgerError> {
    let started = Instant::now();
    let now_ms = options.now_ms.unwrap_or_else(current_epoch_ms);
    let local_device_id = read_local_device_id(conn)?;
    let policy = BatchSyncPolicy::from_options(options, false, now_ms);
    let last_observed = read_last_observed_email_from_conn(conn)?;
    let can_upgrade_unassigned = policy.default_account_id.is_some() || last_observed.is_some();
    let source_roots = resolve_source_roots(options);
    let discovered = list_all_conversation_dbs(&source_roots);

    let mut cli_dbs = 0usize;
    let mut ide_dbs = 0usize;
    for d in &discovered {
        match d.source {
            LedgerSource::Cli => cli_dbs += 1,
            LedgerSource::Ide => ide_dbs += 1,
        }
    }

    let mut existing: HashMap<String, (String, LedgerSource, String)> = HashMap::new();
    let mut remote_owned_ids: HashSet<String> = HashSet::new();
    {
        let mut stmt = conn.prepare(
            "SELECT conversation_id, sig, source, device_id, account_id FROM ingested_dbs",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let cid: String = row.get(0)?;
            let sig: String = row.get(1)?;
            let src_raw: String = row.get(2)?;
            let dev: String = row.get(3)?;
            let acct: String = row.get(4)?;
            if !dev.is_empty() && dev != local_device_id {
                remote_owned_ids.insert(cid);
            } else {
                existing.insert(cid, (sig, LedgerSource::from_raw(&src_raw), acct));
            }
        }
    }

    let mut batches = Vec::new();
    for item in &discovered {
        if remote_owned_ids.contains(&item.conversation_id) {
            continue;
        }
        let Some((sig, max_mtime_ms)) = get_db_signature(&item.db_path) else {
            continue;
        };
        if !options.force
            && let Some((prev_sig, prev_src, prev_acct)) = existing.get(&item.conversation_id)
            && *prev_sig == sig
            && *prev_src == item.source
        {
            let prev_is_unassigned = normalize_non_unassigned_account(Some(prev_acct)).is_none();
            if !(prev_is_unassigned && can_upgrade_unassigned) {
                continue;
            }
        }
        if let Some(batch) = build_ingestion_records(&IngestionTarget {
            conversation_id: &item.conversation_id,
            db_path: &item.db_path,
            sig: &sig,
            max_mtime_ms,
            now_ms,
            source: item.source,
            fallback_parent_id: None,
            fallback_root_id: None,
        }) {
            batches.push(batch);
        }
    }

    let newly_synced_dbs = batches.len();
    let steps_upserted = if batches.is_empty() {
        run_with_busy_retry(|| {
            conn.execute(
                "INSERT OR REPLACE INTO ledger_meta (key, value) VALUES ('last_full_scan_ms', ?1)",
                params![now_ms.to_string()],
            )
        })?;
        0
    } else {
        upsert_batches(conn, &batches, Some(now_ms), &policy, DEFAULT_BUSY_RETRIES)?
    };

    Ok(LedgerSyncReport {
        ok: true,
        total_dbs: discovered.len(),
        cli_dbs,
        ide_dbs,
        newly_synced_dbs,
        steps_upserted,
        duration_ms: started.elapsed().as_millis() as u64,
        ledger_path: ledger_path.to_path_buf(),
    })
}

pub(super) fn is_full_scan_stale(
    conn: &Connection,
    max_age_ms: i64,
    now_ms: i64,
) -> Result<bool, LedgerError> {
    let last_scan_ms: Option<i64> = conn
        .query_row(
            "SELECT value FROM ledger_meta WHERE key = 'last_full_scan_ms'",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .and_then(|s| s.parse::<i64>().ok());

    Ok(!matches!(
        last_scan_ms,
        Some(prev) if now_ms >= prev && (now_ms - prev) < max_age_ms
    ))
}

pub(super) fn sync_all_if_stale(
    conn: &mut Connection,
    ledger_path: &Path,
    max_age_ms: i64,
    options: &LedgerSyncOptions,
) -> Result<Option<LedgerSyncReport>, LedgerError> {
    let now_ms = options.now_ms.unwrap_or_else(current_epoch_ms);
    if !is_full_scan_stale(conn, max_age_ms, now_ms)? {
        return Ok(None);
    }

    let report = sync_all(conn, ledger_path, options)?;
    Ok(Some(report))
}

pub(super) fn sync_conversation(
    conn: &mut Connection,
    ledger_path: &Path,
    conversation_id: &str,
    child_conversation_ids: &[String],
    options: &LedgerSyncOptions,
    max_retries: u32,
) -> Result<SingleSyncReport, LedgerError> {
    if conversation_id.trim().is_empty() {
        return Ok(SingleSyncReport {
            ok: true,
            skipped: true,
            synced_dbs: 0,
            steps_upserted: 0,
            ledger_path: ledger_path.to_path_buf(),
        });
    }

    let now_ms = options.now_ms.unwrap_or_else(current_epoch_ms);
    let throttle_ms = options.throttle_ms.unwrap_or(SYNC_THROTTLE_MS).max(0);
    let local_device_id = read_local_device_id(conn)?;
    let policy = BatchSyncPolicy::from_options(options, true, now_ms);
    let last_observed = read_last_observed_email_from_conn(conn)?;
    let source_roots = resolve_source_roots(options);

    let mut targets: Vec<(&str, bool)> = vec![(conversation_id, false)];
    let mut seen_targets: HashSet<&str> = HashSet::new();
    seen_targets.insert(conversation_id);
    for child in child_conversation_ids {
        let trimmed = child.trim();
        if !trimmed.is_empty() && seen_targets.insert(trimmed) {
            targets.push((trimmed, true));
        }
    }

    let mut batches = Vec::new();
    for (cid, is_child) in targets {
        let Some((db_path, source)) = find_conversation_db(cid, &source_roots) else {
            continue;
        };
        let Some((sig, max_mtime_ms)) = get_db_signature(&db_path) else {
            continue;
        };

        let existing: Option<(String, LedgerSource, i64, String, String)> = conn
            .query_row(
                "SELECT sig, source, last_synced_ms, device_id, account_id FROM ingested_dbs WHERE conversation_id = ?1",
                params![cid],
                |r| {
                    let s: String = r.get(0)?;
                    let src: String = r.get(1)?;
                    let last_synced_ms: i64 = r.get(2)?;
                    let dev: String = r.get(3)?;
                    let acct: String = r.get(4)?;
                    Ok((s, LedgerSource::from_raw(&src), last_synced_ms, dev, acct))
                },
            )
            .optional()?;

        if let Some((prev_sig, prev_src, prev_last_synced_ms, prev_dev, prev_acct)) = existing {
            if !prev_dev.is_empty() && prev_dev != local_device_id {
                continue;
            }
            let prev_is_unassigned = normalize_non_unassigned_account(Some(&prev_acct)).is_none();
            let resolved_acct = policy.resolve_fallback_account(source, last_observed.as_deref());
            let needs_account_upgrade = prev_is_unassigned && resolved_acct != UNASSIGNED_ACCOUNT_ID;
            if !options.force && !needs_account_upgrade {
                if prev_sig == sig && prev_src == source {
                    continue;
                }
                if throttle_ms > 0
                    && now_ms >= prev_last_synced_ms
                    && (now_ms - prev_last_synced_ms) < throttle_ms
                {
                    continue;
                }
            }
        }

        let fb_parent = if is_child { Some(conversation_id) } else { None };
        let fb_root = if is_child { Some(conversation_id) } else { None };
        if let Some(batch) = build_ingestion_records(&IngestionTarget {
            conversation_id: cid,
            db_path: &db_path,
            sig: &sig,
            max_mtime_ms,
            now_ms,
            source,
            fallback_parent_id: fb_parent,
            fallback_root_id: fb_root,
        }) {
            batches.push(batch);
        }
    }

    if batches.is_empty() {
        return Ok(SingleSyncReport {
            ok: true,
            skipped: true,
            synced_dbs: 0,
            steps_upserted: 0,
            ledger_path: ledger_path.to_path_buf(),
        });
    }

    let synced_dbs = batches.len();
    let steps_upserted = upsert_batches(conn, &batches, None, &policy, max_retries)?;
    Ok(SingleSyncReport {
        ok: true,
        skipped: false,
        synced_dbs,
        steps_upserted,
        ledger_path: ledger_path.to_path_buf(),
    })
}

#[derive(Debug, Clone)]
struct CursorRow {
    cursor_id: String,
    cascade_root_id: Option<String>,
    baseline: f64,
    week_baseline: f64,
    week_start_ms: i64,
    total: f64,
    last_observed_cost: f64,
    ts_ms: i64,
    db_total_anchor: f64,
    db_week_total_anchor: f64,
}

#[derive(Default, Clone, Copy)]
struct RootDbCostStats {
    today_usd: f64,
    week_usd: f64,
    total_usd: f64,
}

fn is_valid_amount(val: f64) -> bool {
    val.is_finite() && val >= 0.0
}

fn compute_db_cost_by_root(
    conn: &Connection,
    local_device_id: &str,
    today_start_ms: i64,
    today_end_ms: i64,
    week_from_ms: i64,
    now_ms: i64,
) -> Result<HashMap<String, RootDbCostStats>, rusqlite::Error> {
    let mut conv_meta: HashMap<String, ConvLineageMeta> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT conversation_id, parent_conversation_id, root_conversation_id, source
             FROM ingested_dbs
             WHERE device_id = ?1",
        )?;
        let mut rows = stmt.query(params![local_device_id])?;
        while let Some(row) = rows.next()? {
            let cid: String = row.get(0)?;
            if cid.is_empty() {
                continue;
            }
            let src_raw: String = row.get(3)?;
            conv_meta.insert(
                cid,
                ConvLineageMeta {
                    workspace: None,
                    branch: None,
                    parent_conversation_id: row
                        .get::<_, Option<String>>(1)?
                        .filter(|s| !s.is_empty()),
                    root_conversation_id: row
                        .get::<_, Option<String>>(2)?
                        .filter(|s| !s.is_empty()),
                    source: LedgerSource::from_raw(&src_raw),
                },
            );
        }
    }

    let mut per_conv: Vec<(String, RootDbCostStats)> = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT conversation_id,
                    MAX(NULLIF(parent_conversation_id, '')),
                    MAX(NULLIF(root_conversation_id, '')),
                    SUM(CASE WHEN cost_usd >= 0.0 AND cost_usd < 1e308 THEN cost_usd ELSE 0.0 END),
                    SUM(CASE WHEN ts_ms >= ?1 AND ts_ms <= ?2 AND cost_usd >= 0.0 AND cost_usd < 1e308 THEN cost_usd ELSE 0.0 END),
                    SUM(CASE WHEN ts_ms >= ?3 AND ts_ms <= ?4 AND cost_usd >= 0.0 AND cost_usd < 1e308 THEN cost_usd ELSE 0.0 END)
             FROM telemetry_steps
             WHERE device_id = ?5
               AND (input_tokens > 0 OR cache_read_tokens > 0 OR cache_write_tokens > 0 OR output_tokens > 0 OR cost_usd > 0.0)
             GROUP BY conversation_id",
        )?;
        let mut rows = stmt.query(params![
            today_start_ms,
            today_end_ms,
            week_from_ms,
            now_ms,
            local_device_id
        ])?;
        while let Some(row) = rows.next()? {
            let cid: String = row.get(0)?;
            let parent_cid = row.get::<_, Option<String>>(1)?.filter(|s| !s.is_empty());
            let root_cid = row.get::<_, Option<String>>(2)?.filter(|s| !s.is_empty());
            let total_usd = row.get::<_, Option<f64>>(3)?.filter(|v| is_valid_amount(*v)).unwrap_or(0.0);
            let today_usd = row.get::<_, Option<f64>>(4)?.filter(|v| is_valid_amount(*v)).unwrap_or(0.0);
            let week_usd = row.get::<_, Option<f64>>(5)?.filter(|v| is_valid_amount(*v)).unwrap_or(0.0);

            conv_meta
                .entry(cid.clone())
                .and_modify(|existing| {
                    if existing.parent_conversation_id.is_none() && parent_cid.is_some() {
                        existing.parent_conversation_id = parent_cid.clone();
                    }
                    if existing.root_conversation_id.is_none() && root_cid.is_some() {
                        existing.root_conversation_id = root_cid.clone();
                    }
                })
                .or_insert_with(|| ConvLineageMeta {
                    workspace: None,
                    branch: None,
                    parent_conversation_id: parent_cid,
                    root_conversation_id: root_cid,
                    source: LedgerSource::Cli,
                });

            per_conv.push((
                cid,
                RootDbCostStats {
                    today_usd,
                    week_usd,
                    total_usd,
                },
            ));
        }
    }

    let mut root_cache: HashMap<String, String> = HashMap::new();
    let mut by_root: HashMap<String, RootDbCostStats> = HashMap::new();
    for (cid, stats) in per_conv {
        let root_id = resolve_root_conversation_id(&cid, &conv_meta, &mut root_cache);
        let entry = by_root.entry(root_id).or_default();
        entry.total_usd += stats.total_usd;
        entry.today_usd += stats.today_usd;
        entry.week_usd += stats.week_usd;
    }
    Ok(by_root)
}

pub(super) fn record_rolling_costs(
    conn: &mut Connection,
    input: &RollingCostInput<'_>,
    max_retries: u32,
) -> Result<Option<RollingCostTotals>, LedgerError> {
    let now_ms = input.now_ms;
    let today_key = local_date_key(now_ms);
    let (today_start_ms, today_end_ms) = local_today_bounds(now_ms);
    let db_week_from_ms = match input.seven_day_reset_at_ms {
        Some(reset_ms) => (reset_ms - SEVEN_DAY_WINDOW_MS).min(now_ms),
        None => now_ms - SEVEN_DAY_WINDOW_MS,
    };

    run_with_busy_retry_n(max_retries, || -> Result<Option<RollingCostTotals>, rusqlite::Error> {
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        // Aggregate ledger totals inside the Immediate transaction so DB reads and cursor anchor
        // updates observe a single atomic snapshot under concurrent sync writers.
        let local_device_id = read_local_device_id(&tx)?;
        let db_by_root = compute_db_cost_by_root(
            &tx,
            &local_device_id,
            today_start_ms,
            today_end_ms,
            db_week_from_ms,
            now_ms,
        )?;

        let read_meta = |key: &str| -> Result<Option<String>, rusqlite::Error> {
            tx.query_row(
                "SELECT value FROM ledger_meta WHERE key = ?1",
                params![key],
                |r| r.get::<_, String>(0),
            )
            .optional()
        };

        let mut stored_date = read_meta("rolling_date")?.unwrap_or_else(|| today_key.clone());
        let mut week_start_ms = read_meta("rolling_week_start_ms")?
            .and_then(|s| s.parse::<i64>().ok())
            .filter(|&v| v > 0)
            .unwrap_or(now_ms);
        let mut week_carry = read_meta("rolling_week_carry_usd")?
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|&v| is_valid_amount(v))
            .unwrap_or(0.0);

        let mut cursors: HashMap<String, CursorRow> = HashMap::new();
        {
            let mut stmt = tx.prepare(
                "SELECT cursor_id, cascade_root_id, baseline, week_baseline, week_start_ms,
                        total, last_observed_cost, ts_ms, db_total_anchor, db_week_total_anchor
                 FROM rolling_cost_cursors",
            )?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let cursor_id: String = row.get(0)?;
                let cascade_root_id: Option<String> = row.get(1)?;
                let baseline: f64 = row.get(2)?;
                let week_baseline: f64 = row.get(3)?;
                let c_week_start: i64 = row.get(4)?;
                let total: f64 = row.get(5)?;
                let last_observed_cost: f64 = row.get(6)?;
                let ts_ms: i64 = row.get(7)?;
                let db_total_anchor: f64 = row.get(8)?;
                let db_week_total_anchor: f64 = row.get(9)?;
                if !is_valid_amount(baseline)
                    || !is_valid_amount(total)
                    || ts_ms <= 0
                {
                    continue;
                }
                cursors.insert(
                    cursor_id.clone(),
                    CursorRow {
                        cursor_id,
                        cascade_root_id,
                        baseline,
                        week_baseline: if is_valid_amount(week_baseline) {
                            week_baseline
                        } else {
                            baseline
                        },
                        week_start_ms: if c_week_start > 0 {
                            c_week_start
                        } else {
                            week_start_ms
                        },
                        total,
                        last_observed_cost: if is_valid_amount(last_observed_cost) {
                            last_observed_cost
                        } else {
                            total
                        },
                        ts_ms,
                        db_total_anchor: if is_valid_amount(db_total_anchor) {
                            db_total_anchor
                        } else {
                            0.0
                        },
                        db_week_total_anchor: if is_valid_amount(db_week_total_anchor) {
                            db_week_total_anchor
                        } else {
                            0.0
                        },
                    },
                );
            }
        }

        // 1. Evict cursors unseen for > 24h, carrying any unpersisted weekly increment into week_carry.
        let mut stale_ids = Vec::new();
        for (cid, cur) in &cursors {
            if now_ms - cur.ts_ms > ROLLING_CURSOR_MAX_AGE_MS {
                let db_total_now = cur
                    .cascade_root_id
                    .as_deref()
                    .and_then(|rid| db_by_root.get(rid))
                    .map(|s| s.total_usd)
                    .unwrap_or(0.0);
                let cursor_week = (cur.total - cur.week_baseline).max(0.0);
                let db_week_delta = (db_total_now - cur.db_week_total_anchor).max(0.0);
                let extra_week = (cursor_week - db_week_delta).max(0.0);
                week_carry += extra_week;
                stale_ids.push(cid.clone());
            }
        }
        for cid in stale_ids {
            cursors.remove(&cid);
        }

        // 2. Midnight day rollover -> carry active cursors over so only today's part counts.
        if stored_date != today_key {
            stored_date = today_key.clone();
            for cur in cursors.values_mut() {
                let db_total_now = cur
                    .cascade_root_id
                    .as_deref()
                    .and_then(|rid| db_by_root.get(rid))
                    .map(|s| s.total_usd)
                    .unwrap_or(0.0);
                cur.baseline = cur.total;
                cur.db_total_anchor = db_total_now;
            }
        }

        // 3. Weekly rolling / quota window rollover.
        let quota_window_start = input
            .seven_day_reset_at_ms
            .map(|reset| reset - SEVEN_DAY_WINDOW_MS);
        let should_reset_week = match quota_window_start {
            Some(ws) => ws > week_start_ms + WEEK_START_JITTER_MS,
            None => now_ms - week_start_ms >= SEVEN_DAY_WINDOW_MS,
        };
        if should_reset_week {
            week_start_ms = now_ms;
            week_carry = 0.0;
            for cur in cursors.values_mut() {
                let db_total_now = cur
                    .cascade_root_id
                    .as_deref()
                    .and_then(|rid| db_by_root.get(rid))
                    .map(|s| s.total_usd)
                    .unwrap_or(0.0);
                cur.week_baseline = cur.total;
                cur.week_start_ms = now_ms;
                cur.db_week_total_anchor = db_total_now;
            }
        }

        // 4. Observe current live conversation cost (with >50% compaction drop anti-regression).
        let cursor_id = input.cursor_id.trim();
        let cascade_root = input
            .cascade_root_id
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or((!cursor_id.is_empty()).then_some(cursor_id));

        if !cursor_id.is_empty()
            && let Some(cost) = input.live_cost_usd.filter(|&c| is_valid_amount(c))
        {
            let db_total_now = cascade_root
                .and_then(|rid| db_by_root.get(rid))
                .map(|s| s.total_usd)
                .unwrap_or(0.0);

            if let Some(cur) = cursors.get_mut(cursor_id) {
                if cur.cascade_root_id.is_none() && cascade_root.is_some() {
                    cur.cascade_root_id = cascade_root.map(ToOwned::to_owned);
                }
                if cost > cur.last_observed_cost {
                    cur.total += cost - cur.last_observed_cost;
                    cur.last_observed_cost = cost;
                } else if cost < cur.last_observed_cost * 0.5 {
                    // Significant compaction or counter reset (>50% drop): advance cursor
                    // without subtracting previously accrued spend.
                    cur.last_observed_cost = cost;
                }
                cur.ts_ms = now_ms;
            } else {
                cursors.insert(
                    cursor_id.to_string(),
                    CursorRow {
                        cursor_id: cursor_id.to_string(),
                        cascade_root_id: cascade_root.map(ToOwned::to_owned),
                        baseline: cost,
                        week_baseline: cost,
                        week_start_ms,
                        total: cost,
                        last_observed_cost: cost,
                        ts_ms: now_ms,
                        db_total_anchor: db_total_now,
                        db_week_total_anchor: db_total_now,
                    },
                );
            }
        }

        // Persist updated rolling state cleanly inside the transaction.
        tx.execute(
            "INSERT OR REPLACE INTO ledger_meta (key, value) VALUES ('rolling_date', ?1)",
            params![stored_date],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO ledger_meta (key, value) VALUES ('rolling_week_start_ms', ?1)",
            params![week_start_ms.to_string()],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO ledger_meta (key, value) VALUES ('rolling_week_carry_usd', ?1)",
            params![format!("{week_carry:.12}")],
        )?;
        tx.execute("DELETE FROM rolling_cost_cursors", [])?;
        {
            let mut ins = tx.prepare(
                "INSERT INTO rolling_cost_cursors (
                   cursor_id, cascade_root_id, baseline, week_baseline, week_start_ms,
                   total, last_observed_cost, ts_ms, db_total_anchor, db_week_total_anchor
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?;
            for cur in cursors.values() {
                ins.execute(params![
                    cur.cursor_id,
                    cur.cascade_root_id,
                    cur.baseline,
                    cur.week_baseline,
                    cur.week_start_ms,
                    cur.total,
                    cur.last_observed_cost,
                    cur.ts_ms,
                    cur.db_total_anchor,
                    cur.db_week_total_anchor,
                ])?;
            }
        }
        tx.commit()?;

        if cursors.is_empty() && week_carry == 0.0 && db_by_root.is_empty() {
            return Ok(None);
        }

        let mut today_usd = 0.0f64;
        let mut week_usd = week_carry;
        let mut matched_roots: HashSet<&str> = HashSet::new();

        for cur in cursors.values() {
            let matched_root = cur
                .cascade_root_id
                .as_deref()
                .filter(|rid| db_by_root.contains_key(*rid));
            let db_stats = matched_root.and_then(|rid| {
                matched_roots.insert(rid);
                db_by_root.get(rid).copied()
            }).unwrap_or_default();

            let cursor_delta_today = (cur.total - cur.baseline).max(0.0);
            let db_delta_today = (db_stats.total_usd - cur.db_total_anchor).max(0.0);
            let unpersisted_today = (cursor_delta_today - db_delta_today).max(0.0);
            today_usd += db_stats.today_usd + unpersisted_today;

            let cursor_delta_week = (cur.total - cur.week_baseline).max(0.0);
            let db_delta_week = (db_stats.total_usd - cur.db_week_total_anchor).max(0.0);
            let unpersisted_week = (cursor_delta_week - db_delta_week).max(0.0);
            week_usd += db_stats.week_usd + unpersisted_week;
        }

        for (root_id, stats) in &db_by_root {
            if !matched_roots.contains(root_id.as_str()) {
                today_usd += stats.today_usd;
                week_usd += stats.week_usd;
            }
        }

        Ok(Some(RollingCostTotals {
            today_usd,
            week_usd,
        }))
    })
    .map_err(LedgerError::from)
}

struct ImmediateTxGuard<'a> {
    conn: &'a Connection,
    committed: bool,
}

impl<'a> ImmediateTxGuard<'a> {
    fn begin(conn: &'a Connection) -> Result<Self, rusqlite::Error> {
        conn.execute_batch("BEGIN IMMEDIATE")?;
        Ok(Self {
            conn,
            committed: false,
        })
    }

    fn commit(mut self) -> Result<(), rusqlite::Error> {
        self.conn.execute_batch("COMMIT")?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for ImmediateTxGuard<'_> {
    fn drop(&mut self) {
        if !self.committed {
            let _ = self.conn.execute_batch("ROLLBACK");
        }
    }
}

pub(super) fn clear_remote_data(conn: &Connection, max_retries: u32) -> Result<usize, LedgerError> {
    run_with_busy_retry_n(max_retries, || {
        let tx = ImmediateTxGuard::begin(conn)?;
        let local_device_id = read_local_device_id(conn)?;
        let deleted_steps = conn.execute(
            "DELETE FROM telemetry_steps WHERE device_id != ?1",
            params![&local_device_id],
        )?;
        conn.execute(
            "DELETE FROM ingested_dbs WHERE device_id != ?1",
            params![&local_device_id],
        )?;
        conn.execute(
            "DELETE FROM sync_devices WHERE device_id != ?1",
            params![&local_device_id],
        )?;
        conn.execute(
            "DELETE FROM quota_snapshots WHERE device_id != ?1",
            params![&local_device_id],
        )?;
        conn.execute("DELETE FROM sync_chunk_state WHERE direction = 'pull'", [])?;
        tx.commit()?;
        Ok(deleted_steps)
    })
    .map_err(LedgerError::from)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn upsert_sync_device(
    conn: &Connection,
    account_id: &str,
    device_id: &str,
    device_name: &str,
    is_local: bool,
    last_seen_ms: i64,
    plan_tier: Option<&str>,
    max_retries: u32,
) -> Result<(), LedgerError> {
    run_with_busy_retry_n(max_retries, || {
        upsert_sync_device_on_conn(
            conn,
            account_id,
            device_id,
            device_name,
            is_local,
            last_seen_ms,
            plan_tier,
        )
    })
    .map_err(LedgerError::from)
}

pub(super) fn last_observed_email(conn: &Connection) -> Result<Option<String>, LedgerError> {
    read_last_observed_email_from_conn(conn).map_err(LedgerError::from)
}

pub(super) fn set_last_observed_email(
    conn: &Connection,
    email: &str,
    max_retries: u32,
) -> Result<(), LedgerError> {
    let Some(normalized) = normalize_non_unassigned_account(Some(email)) else {
        return Ok(());
    };
    if let Ok(Some(current)) = read_last_observed_email_from_conn(conn)
        && current == normalized
    {
        return Ok(());
    }
    run_with_busy_retry_n(max_retries, || {
        conn.execute(
            "INSERT OR REPLACE INTO ledger_meta (key, value) VALUES ('last_observed_email', ?1)",
            params![&normalized],
        )?;
        Ok(())
    })
    .map_err(LedgerError::from)
}

pub(super) fn upsert_quota_snapshot(
    conn: &Connection,
    snapshot: &QuotaSnapshot,
    max_retries: u32,
) -> Result<(), LedgerError> {
    let account_id = normalize_account_id(&snapshot.account_id)
        .unwrap_or_else(|| UNASSIGNED_ACCOUNT_ID.to_string());
    let local_device_id = if snapshot.device_id.trim().is_empty() {
        Some(read_local_device_id(conn)?)
    } else {
        None
    };
    let device_id = local_device_id
        .as_deref()
        .unwrap_or_else(|| snapshot.device_id.trim());
    let sanitized_name = platform::sanitize_device_name(&snapshot.device_name);
    let device_name = sanitized_name.as_deref().unwrap_or(device_id);
    let plan_tier = snapshot
        .plan_tier
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let quota_json = serde_json::to_string(&snapshot.quota)?;

    run_with_busy_retry_n(max_retries, || {
        conn.execute(
            "INSERT INTO quota_snapshots (account_id, device_id, device_name, plan_tier, observed_at_ms, quota_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(account_id, device_id) DO UPDATE SET
               device_name = excluded.device_name,
               plan_tier = COALESCE(excluded.plan_tier, quota_snapshots.plan_tier),
               observed_at_ms = excluded.observed_at_ms,
               quota_json = excluded.quota_json
             WHERE excluded.observed_at_ms >= quota_snapshots.observed_at_ms",
            params![
                &account_id,
                device_id,
                device_name,
                plan_tier,
                snapshot.observed_at_ms,
                &quota_json,
            ],
        )?;
        Ok(())
    })
    .map_err(LedgerError::from)
}

pub(super) fn query_latest_account_quotas(
    conn: &Connection,
) -> Result<Vec<QuotaSnapshot>, LedgerError> {
    let mut stmt = conn.prepare(
        "SELECT account_id, device_id, device_name, plan_tier, observed_at_ms, quota_json
         FROM quota_snapshots
         ORDER BY account_id ASC, observed_at_ms DESC, device_id ASC",
    )?;
    let mut rows = stmt.query([])?;
    let mut best_by_account: HashMap<String, QuotaSnapshot> = HashMap::new();

    while let Some(row) = rows.next()? {
        let raw_account: String = row.get(0)?;
        let device_id: String = row.get(1)?;
        let device_name: String = row.get(2)?;
        let plan_tier: Option<String> = row.get(3)?;
        let observed_at_ms: i64 = row.get(4)?;
        let quota_json: String = row.get(5)?;

        let Ok(quota) = serde_json::from_str::<ProviderQuotaSummary>(&quota_json) else {
            continue;
        };
        let account_id =
            normalize_account_id(&raw_account).unwrap_or_else(|| UNASSIGNED_ACCOUNT_ID.to_string());
        let candidate = QuotaSnapshot {
            account_id: account_id.clone(),
            device_id,
            device_name,
            plan_tier,
            observed_at_ms,
            quota,
        };

        let replace = match best_by_account.get(&account_id) {
            None => true,
            Some(best) => {
                candidate.observed_at_ms > best.observed_at_ms
                    || (candidate.observed_at_ms == best.observed_at_ms
                        && candidate.device_id < best.device_id)
            }
        };
        if replace {
            best_by_account.insert(account_id, candidate);
        }
    }

    let mut result: Vec<QuotaSnapshot> = best_by_account.into_values().collect();
    result.sort_by(|a, b| a.account_id.cmp(&b.account_id));
    Ok(result)
}

fn find_matching_unassigned_conversations(
    conn: &Connection,
    local_device_id: &str,
    workspace_filter: Option<&str>,
) -> Result<Vec<String>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT conversation_id, workspace
         FROM ingested_dbs
         WHERE device_id = ?1 AND account_id = 'unassigned'",
    )?;
    let mut rows = stmt.query(params![local_device_id])?;
    let mut matched = Vec::new();
    while let Some(row) = rows.next()? {
        let cid: String = row.get(0)?;
        let ws: Option<String> = row.get(1)?;
        let is_match = match workspace_filter {
            Some(sub) => ws.as_deref().is_some_and(|w| w.contains(sub)),
            None => true,
        };
        if is_match {
            matched.push(cid);
        }
    }
    Ok(matched)
}

pub(super) fn assign_unassigned_account(
    conn: &Connection,
    email: &str,
    workspace_substr: Option<&str>,
    dry_run: bool,
    max_retries: u32,
) -> Result<AccountAssignReport, LedgerError> {
    let normalized_email = validate_and_normalize_account_email(email)
        .ok_or_else(|| LedgerError::InvalidAccountEmail(email.trim().to_string()))?;
    let ws_filter = workspace_substr.filter(|s| !s.trim().is_empty());

    if dry_run {
        let local_device_id = read_local_device_id(conn)?;
        let matched = find_matching_unassigned_conversations(conn, &local_device_id, ws_filter)?;
        let mut steps_updated = 0usize;
        let mut count_stmt = conn.prepare(
            "SELECT COUNT(*) FROM telemetry_steps
             WHERE conversation_id = ?1 AND device_id = ?2 AND account_id = 'unassigned'",
        )?;
        for cid in &matched {
            let count: i64 = count_stmt.query_row(params![cid, &local_device_id], |r| r.get(0))?;
            steps_updated += usize::try_from(count.max(0)).unwrap_or(0);
        }
        return Ok(AccountAssignReport {
            account_id: normalized_email,
            conversations_updated: matched.len(),
            steps_updated,
            dry_run: true,
        });
    }

    let now_ms = current_epoch_ms();
    let (_, device_name_hint) = platform::read_sync_identity_hints();
    let device_name = resolve_effective_device_name(device_name_hint.as_deref());

    run_with_busy_retry_n(max_retries, || {
        let tx = ImmediateTxGuard::begin(conn)?;
        let local_device_id = read_local_device_id(conn)?;
        let matched = find_matching_unassigned_conversations(conn, &local_device_id, ws_filter)?;

        let mut conversations_updated = 0usize;
        let mut steps_updated = 0usize;
        {
            let mut upd_db = conn.prepare(
                "UPDATE ingested_dbs
                 SET account_id = ?1
                 WHERE conversation_id = ?2 AND device_id = ?3 AND account_id = 'unassigned'",
            )?;
            let mut upd_steps = conn.prepare(
                "UPDATE telemetry_steps
                 SET account_id = ?1
                 WHERE conversation_id = ?2 AND device_id = ?3 AND account_id = 'unassigned'",
            )?;
            for cid in &matched {
                conversations_updated +=
                    upd_db.execute(params![&normalized_email, cid, &local_device_id])?;
                steps_updated +=
                    upd_steps.execute(params![&normalized_email, cid, &local_device_id])?;
            }
        }

        if conversations_updated > 0 || steps_updated > 0 {
            upsert_sync_device_on_conn(
                conn,
                &normalized_email,
                &local_device_id,
                &device_name,
                true,
                now_ms,
                None,
            )?;
        }

        tx.commit()?;
        Ok(AccountAssignReport {
            account_id: normalized_email.clone(),
            conversations_updated,
            steps_updated,
            dry_run: false,
        })
    })
    .map_err(LedgerError::from)
}
