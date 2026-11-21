use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::domain::{TokenUsage, calculate_token_cost};
use crate::platform;

use super::proto_wire::{
    GenerationMetadataRow, MAX_VALID_STREAM_SPEED, TrajectoryMetadata,
    extract_compaction_limit_from_executor_metadata, extract_compaction_limit_from_gen_metadata,
    extract_gen_metadata_row_details, extract_step_timestamp_and_bot, extract_trajectory_meta,
    is_valid_compaction_limit,
};
use super::sqlite_ro::{
    is_no_such_table_error, is_sqlite_open_or_access_error, with_readonly_db,
};
use super::transcript::ModelTokenAcc;

const COMPACTION_CACHE_FILENAME: &str = "agy-pulse-compaction-cache.json";
const MAX_DB_CACHE_ENTRIES: usize = 24;
const MAX_RECENT_DBS: usize = 5;
const MAX_PERF_RECENT_DBS: usize = 30;
const MAX_PERF_DB_ENTRIES: usize = 256;
const PERF_DB_SCHEMA_VERSION: u32 = 4;

/// Pure-data conversation SQLite record exported by `HostReader` for downstream `ledger` use.
/// Never leaks `rusqlite::Connection` or raw Protobuf bytes.
#[derive(Debug, Clone, Default)]
pub struct ConversationDbRecord {
    pub conversation_id: String,
    pub db_path: PathBuf,
    pub parent_cascade_id: Option<String>,
    pub root_cascade_id: Option<String>,
    pub cascade_id: Option<String>,
    pub compaction_limit: Option<u64>,
    pub trajectory: Option<TrajectoryMetadata>,
    pub step_timestamps: BTreeMap<i64, i64>,
    pub bot_timestamps: BTreeMap<String, i64>,
    pub gen_rows: Vec<GenerationMetadataRow>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct DbLimitCacheEntry {
    sig: String,
    limit: u64,
    updated_at: i64,
    last_access_at: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct DbModelPerfAcc {
    pub stream_samples: u64,
    pub sum_out: u64,
    pub sum_stream_sec: f64,
    pub ttft_samples: u64,
    pub sum_ttft_sec: f64,
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
    pub steps: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct DbPerfCacheEntry {
    schema_ver: u32,
    sig: String,
    conversation_id: String,
    parent_cascade_id: Option<String>,
    root_cascade_id: Option<String>,
    user_turns: u64,
    max_step_index: Option<u64>,
    total_token_steps: u64,
    models: BTreeMap<String, DbModelPerfAcc>,
    total_cost_usd: f64,
    updated_at: i64,
    last_access_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct CompactionCacheFile {
    #[serde(rename = "_db_limits")]
    db_limits: BTreeMap<String, DbLimitCacheEntry>,
    #[serde(rename = "_db_perf")]
    db_perf: BTreeMap<String, DbPerfCacheEntry>,
    #[serde(rename = "_last_known_limit")]
    last_known_limit: Option<u64>,
    #[serde(rename = "_last_known_mtime")]
    last_known_mtime: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct ModelPerfSummary {
    pub model: String,
    pub avg_speed: Option<f64>,
    pub avg_ttft_sec: Option<f64>,
    pub stream_samples: u64,
    pub last_updated_ms: i64,
    pub turns_ago: u64,
}

#[derive(Debug, Clone, Default)]
pub(super) struct CascadeDbEntryCost {
    pub total_cost_usd: f64,
    pub steps: u64,
    pub max_step_index: Option<u64>,
    pub models: BTreeMap<String, ModelTokenAcc>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct CascadeDbCostTree {
    pub main_cost_usd: f64,
    pub main_steps: u64,
    pub main_max_step_index: Option<u64>,
    pub main_by_model: BTreeMap<String, ModelTokenAcc>,
    pub children_cost_usd: f64,
    pub children_by_model: BTreeMap<String, ModelTokenAcc>,
    pub matched_by_id: BTreeMap<String, CascadeDbEntryCost>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct ResolvedModelPerf {
    pub models: BTreeMap<String, ModelPerfSummary>,
    pub cascade_cost_tree: Option<CascadeDbCostTree>,
}

pub(super) fn extract_transcript_context(transcript_path: Option<&Path>) -> (Option<PathBuf>, Option<String>) {
    let Some(tp) = transcript_path else {
        return (None, None);
    };
    let raw = tp.to_string_lossy();
    let normalized = raw.replace('\\', "/");
    let marker = "/brain/";
    let Some(pos) = normalized.rfind(marker) else {
        return (None, None);
    };
    let base_dir = if pos > 0 {
        Some(PathBuf::from(&normalized[..pos]))
    } else {
        None
    };
    let rest = &normalized[pos + marker.len()..];
    let conv_id = rest
        .split('/')
        .next()
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);
    (base_dir, conv_id)
}

fn collect_candidate_roots(transcript_base_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = platform::get_antigravity_roots();
    if let Some(base) = transcript_base_dir
        && !roots.iter().any(|r| r == base)
    {
        roots.insert(0, base.to_path_buf());
    }
    roots
}

fn resolve_compaction_cache_path() -> PathBuf {
    platform::resolve_antigravity_path(COMPACTION_CACHE_FILENAME)
}

fn resolve_active_db_path(conversation_id: Option<&str>, candidate_roots: &[PathBuf]) -> Option<PathBuf> {
    let conv_id = conversation_id.filter(|s| !s.trim().is_empty())?;
    for root in candidate_roots {
        let candidate = root.join("conversations").join(format!("{conv_id}.db"));
        if fs::metadata(&candidate).is_ok_and(|m| m.is_file()) {
            return Some(candidate);
        }
    }
    None
}

fn sidecar_path(db_path: &Path, suffix: &str) -> PathBuf {
    let mut os = OsString::from(db_path.as_os_str());
    os.push(suffix);
    PathBuf::from(os)
}

fn mtime_ms_from_meta(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn get_db_max_mtime_ms(db_path: &Path) -> i64 {
    let mut max_mtime = 0i64;
    if let Ok(st) = fs::metadata(db_path)
        && st.is_file()
    {
        max_mtime = max_mtime.max(mtime_ms_from_meta(&st));
    }
    let wal = sidecar_path(db_path, "-wal");
    if let Ok(wal_st) = fs::metadata(&wal)
        && wal_st.is_file()
        && wal_st.len() > 0
    {
        max_mtime = max_mtime.max(mtime_ms_from_meta(&wal_st));
    }
    max_mtime
}

fn get_db_signature(db_path: &Path) -> Option<String> {
    let db_meta = fs::metadata(db_path).ok()?;
    if !db_meta.is_file() {
        return None;
    }
    let wal = sidecar_path(db_path, "-wal");
    let (wal_size, wal_mtime) = match fs::metadata(&wal) {
        Ok(wm) if wm.is_file() => (wm.len(), mtime_ms_from_meta(&wm)),
        _ => (0, 0),
    };
    Some(format!(
        "{}:{}:{wal_size}:{wal_mtime}",
        db_meta.len(),
        mtime_ms_from_meta(&db_meta)
    ))
}

#[derive(Debug, Clone)]
struct DbPathEntry {
    path: PathBuf,
    mtime_ms: i64,
}

fn list_recent_conversation_dbs(
    roots: &[PathBuf],
    exclude_path: Option<&Path>,
    max_dbs: usize,
) -> Vec<DbPathEntry> {
    let mut dbs = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();

    for root in roots {
        let conv_dir = root.join("conversations");
        let Ok(rd) = fs::read_dir(&conv_dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("db") {
                continue;
            }
            if exclude_path.is_some_and(|ex| ex == path) || !seen.insert(path.clone()) {
                continue;
            }
            if fs::metadata(&path).is_ok_and(|m| m.is_file()) {
                dbs.push(DbPathEntry {
                    mtime_ms: get_db_max_mtime_ms(&path),
                    path,
                });
            }
        }
    }

    dbs.sort_by_key(|b| std::cmp::Reverse(b.mtime_ms));
    dbs.truncate(max_dbs);
    dbs
}

fn read_compaction_cache(cache_path: &Path) -> CompactionCacheFile {
    let Ok(raw) = fs::read_to_string(cache_path) else {
        return CompactionCacheFile::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

fn prune_limits_cache(map: &mut BTreeMap<String, DbLimitCacheEntry>, max_entries: usize) {
    if map.len() <= max_entries {
        return;
    }
    let mut keys_by_lru: Vec<(String, i64)> = map
        .iter()
        .map(|(k, v)| (k.clone(), v.last_access_at.unwrap_or(v.updated_at)))
        .collect();
    keys_by_lru.sort_by_key(|(_, ts)| *ts);
    let remove_count = map.len() - max_entries;
    for (k, _) in keys_by_lru.into_iter().take(remove_count) {
        map.remove(&k);
    }
}

fn prune_perf_cache(
    map: &mut BTreeMap<String, DbPerfCacheEntry>,
    max_entries: usize,
    protected_keys: &HashSet<String>,
) {
    if map.len() <= max_entries {
        return;
    }
    if protected_keys.is_empty() {
        let mut keys_by_lru: Vec<(String, i64)> = map
            .iter()
            .map(|(k, v)| (k.clone(), v.last_access_at.max(v.updated_at)))
            .collect();
        keys_by_lru.sort_by_key(|(_, ts)| *ts);
        let remove_count = map.len() - max_entries;
        for (k, _) in keys_by_lru.into_iter().take(remove_count) {
            map.remove(&k);
        }
        return;
    }

    let mut protected_entries: Vec<(String, i64)> = Vec::new();
    let mut evictable_entries: Vec<(String, i64)> = Vec::new();
    for (k, v) in map.iter() {
        let ts = v.last_access_at.max(v.updated_at);
        if protected_keys.contains(k) {
            protected_entries.push((k.clone(), ts));
        } else {
            evictable_entries.push((k.clone(), ts));
        }
    }
    protected_entries.sort_by_key(|(_, ts)| *ts);
    evictable_entries.sort_by_key(|(_, ts)| *ts);

    let keep_protected_start = protected_entries.len().saturating_sub(max_entries);
    let kept_protected = &protected_entries[keep_protected_start..];
    let remaining_slots = max_entries.saturating_sub(kept_protected.len());
    let keep_evictable_start = evictable_entries.len().saturating_sub(remaining_slots);
    let kept_evictable = &evictable_entries[keep_evictable_start..];

    let mut keep_keys: HashSet<String> = HashSet::with_capacity(max_entries);
    for (k, _) in kept_protected {
        keep_keys.insert(k.clone());
    }
    for (k, _) in kept_evictable {
        keep_keys.insert(k.clone());
    }
    map.retain(|k, _| keep_keys.contains(k));
}

fn write_compaction_cache(cache_path: &Path, cache: &CompactionCacheFile) {
    let Ok(bytes) = serde_json::to_vec(cache) else {
        return;
    };
    let _ = platform::write_atomic_0600(cache_path, &bytes);
}

fn query_sqlite_compaction_limit(db_path: &Path) -> Option<u64> {
    with_readonly_db(db_path, |conn| {
        match conn.prepare("SELECT data FROM gen_metadata ORDER BY idx DESC LIMIT 16") {
            Ok(mut stmt) => {
                let mut rows = stmt.query([])?;
                while let Some(row) = rows.next()? {
                    let blob: Vec<u8> = row.get(0)?;
                    if let Some(lim) = extract_compaction_limit_from_gen_metadata(&blob) {
                        return Ok(Some(lim));
                    }
                }
            }
            Err(err) if is_sqlite_open_or_access_error(&err) => return Err(err),
            Err(err) if is_no_such_table_error(&err) => {}
            Err(_) => {}
        }

        match conn.prepare("SELECT data FROM executor_metadata ORDER BY idx DESC LIMIT 16") {
            Ok(mut stmt) => {
                let mut rows = stmt.query([])?;
                while let Some(row) = rows.next()? {
                    let blob: Vec<u8> = row.get(0)?;
                    if let Some(lim) = extract_compaction_limit_from_executor_metadata(&blob) {
                        return Ok(Some(lim));
                    }
                }
            }
            Err(err) if is_sqlite_open_or_access_error(&err) => return Err(err),
            Err(err) if is_no_such_table_error(&err) => {}
            Err(_) => {}
        }

        Ok(None)
    })
    .flatten()
}

/// Resolve server-configured `CheckpointConfig.max_context_tokens` from the active
/// conversation `.db`, compaction cache, or top 5 recent `.db` files.
pub(super) fn resolve_server_compaction_limit(
    conversation_id: Option<&str>,
    transcript_path: Option<&Path>,
    now_ms: i64,
) -> Option<u64> {
    let (transcript_base_dir, derived_conv_id) = extract_transcript_context(transcript_path);
    let effective_conv_id = conversation_id.or(derived_conv_id.as_deref());
    let candidate_roots = collect_candidate_roots(transcript_base_dir.as_deref());
    let cache_path = resolve_compaction_cache_path();
    let mut cache = read_compaction_cache(&cache_path);
    let active_db_path = resolve_active_db_path(effective_conv_id, &candidate_roots);

    if let Some(ref active_path) = active_db_path {
        let key = active_path.to_string_lossy().into_owned();
        let sig = get_db_signature(active_path);
        if let Some(ref sig_str) = sig
            && let Some(cached) = cache.db_limits.get(&key)
            && cached.sig == *sig_str
            && is_valid_compaction_limit(cached.limit)
        {
            return Some(cached.limit);
        }

        if let Some(lim) = query_sqlite_compaction_limit(active_path)
            && is_valid_compaction_limit(lim)
        {
            let mtime_ms = get_db_max_mtime_ms(active_path);
            cache.db_limits.insert(
                key,
                DbLimitCacheEntry {
                    sig: sig.unwrap_or_default(),
                    limit: lim,
                    updated_at: now_ms,
                    last_access_at: Some(now_ms),
                },
            );
            prune_limits_cache(&mut cache.db_limits, MAX_DB_CACHE_ENTRIES);
            cache.last_known_limit = Some(lim);
            cache.last_known_mtime = Some(mtime_ms);
            write_compaction_cache(&cache_path, &cache);
            return Some(lim);
        }
    }

    let recent_dbs = list_recent_conversation_dbs(
        &candidate_roots,
        active_db_path.as_deref(),
        MAX_RECENT_DBS,
    );
    let newest_recent_mtime = recent_dbs.first().map(|d| d.mtime_ms).unwrap_or(0);
    let last_known_mtime = cache.last_known_mtime.unwrap_or(0);

    if let Some(last_lim) = cache.last_known_limit
        && is_valid_compaction_limit(last_lim)
        && (recent_dbs.is_empty() || newest_recent_mtime <= last_known_mtime)
    {
        return Some(last_lim);
    }

    for recent in &recent_dbs {
        let key = recent.path.to_string_lossy().into_owned();
        let sig = get_db_signature(&recent.path);
        if let Some(ref sig_str) = sig
            && let Some(cached) = cache.db_limits.get(&key)
            && cached.sig == *sig_str
            && is_valid_compaction_limit(cached.limit)
        {
            let lim = cached.limit;
            cache.last_known_limit = Some(lim);
            cache.last_known_mtime = Some(recent.mtime_ms);
            write_compaction_cache(&cache_path, &cache);
            return Some(lim);
        }

        if let Some(lim) = query_sqlite_compaction_limit(&recent.path)
            && is_valid_compaction_limit(lim)
        {
            cache.db_limits.insert(
                key,
                DbLimitCacheEntry {
                    sig: sig.unwrap_or_default(),
                    limit: lim,
                    updated_at: now_ms,
                    last_access_at: Some(now_ms),
                },
            );
            prune_limits_cache(&mut cache.db_limits, MAX_DB_CACHE_ENTRIES);
            cache.last_known_limit = Some(lim);
            cache.last_known_mtime = Some(recent.mtime_ms);
            write_compaction_cache(&cache_path, &cache);
            return Some(lim);
        }
    }

    cache
        .last_known_limit
        .filter(|&lim| is_valid_compaction_limit(lim))
}

struct RawDbTelemetry {
    models: BTreeMap<String, DbModelPerfAcc>,
    parent_cascade_id: Option<String>,
    root_cascade_id: Option<String>,
    total_cost_usd: f64,
    max_step_index: Option<u64>,
    total_token_steps: u64,
}

fn accumulate_row_into_telemetry(state: &mut RawDbTelemetry, idx: i64, blob: &[u8]) {
    let decoded = extract_gen_metadata_row_details(idx, blob);
    if state.parent_cascade_id.is_none() {
        state.parent_cascade_id = decoded.parent_cascade_id;
    }
    if state.root_cascade_id.is_none() {
        state.root_cascade_id = decoded.root_cascade_id;
    }
    for &step_idx in &decoded.step_indices {
        state.max_step_index = Some(state.max_step_index.map_or(step_idx, |m| m.max(step_idx)));
    }

    for step in decoded.steps {
        if let Some(ls) = step.last_step_index {
            state.max_step_index = Some(state.max_step_index.map_or(ls, |m| m.max(ls)));
        }
        let has_any_token = step.input_tokens > 0
            || step.output_tokens > 0
            || step.cache_read_tokens > 0
            || step.cache_write_tokens > 0;
        if !step.valid_stream && !step.valid_ttft && !has_any_token {
            continue;
        }

        let acc = state.models.entry(step.model_key).or_default();
        if step.valid_stream
            && let Some(sec) = step.stream_sec
        {
            acc.stream_samples = acc.stream_samples.saturating_add(1);
            acc.sum_out = acc.sum_out.saturating_add(step.output_tokens);
            acc.sum_stream_sec += sec;
        }
        if step.valid_ttft
            && let Some(ttft) = step.ttft_sec
        {
            acc.ttft_samples = acc.ttft_samples.saturating_add(1);
            acc.sum_ttft_sec += ttft;
        }
        if has_any_token {
            // gen_metadata f4.f1 + f4.f2 (input_tokens) and f4.f5 (cache_read_tokens) are mutually exclusive.
            let usage = TokenUsage {
                input_tokens: step.input_tokens,
                output_tokens: step.output_tokens,
                cache_read_input_tokens: step.cache_read_tokens,
                cache_creation_input_tokens: step.cache_write_tokens,
                cache_exclusive: true,
            };
            let step_cost =
                calculate_token_cost(&step.step_model, &usage, false, false).unwrap_or(0.0);
            acc.input_tokens = acc.input_tokens.saturating_add(step.input_tokens);
            acc.cache_read_tokens = acc.cache_read_tokens.saturating_add(step.cache_read_tokens);
            acc.cache_write_tokens = acc.cache_write_tokens.saturating_add(step.cache_write_tokens);
            acc.output_tokens = acc.output_tokens.saturating_add(step.output_tokens);
            acc.cost_usd += step_cost;
            acc.steps = acc.steps.saturating_add(1);
            state.total_cost_usd += step_cost;
            state.total_token_steps = state.total_token_steps.saturating_add(1);
        }
    }
}

fn query_raw_db_telemetry(db_path: &Path) -> Option<RawDbTelemetry> {
    with_readonly_db(db_path, |conn| {
        let mut state = RawDbTelemetry {
            models: BTreeMap::new(),
            parent_cascade_id: None,
            root_cascade_id: None,
            total_cost_usd: 0.0,
            max_step_index: None,
            total_token_steps: 0,
        };
        match conn.prepare("SELECT idx, data FROM gen_metadata ORDER BY idx ASC") {
            Ok(mut stmt) => {
                let mut rows = stmt.query([])?;
                while let Some(row) = rows.next()? {
                    let idx: i64 = row.get(0)?;
                    let blob: Vec<u8> = row.get(1)?;
                    if !blob.is_empty() {
                        accumulate_row_into_telemetry(&mut state, idx, &blob);
                    }
                }
                Ok(Some(state))
            }
            Err(err) if is_sqlite_open_or_access_error(&err) => Err(err),
            Err(err) if is_no_such_table_error(&err) => Ok(Some(state)),
            Err(_) => Ok(None),
        }
    })
    .flatten()
}

fn has_valid_model_perf_samples(models: &BTreeMap<String, DbModelPerfAcc>) -> bool {
    models
        .values()
        .any(|s| s.stream_samples > 0 || s.ttft_samples > 0)
}

fn find_transcript_for_db(db_path: &Path, candidate_roots: &[PathBuf]) -> Option<PathBuf> {
    let conv_id = db_path.file_stem()?.to_str()?;
    if conv_id.is_empty() {
        return None;
    }
    if let Some(conv_dir) = db_path.parent()
        && let Some(root) = conv_dir.parent()
    {
        let direct = root
            .join("brain")
            .join(conv_id)
            .join(".system_generated")
            .join("logs")
            .join("transcript.jsonl");
        if fs::metadata(&direct).is_ok_and(|m| m.is_file()) {
            return Some(direct);
        }
    }
    for root in candidate_roots {
        let candidate = root
            .join("brain")
            .join(conv_id)
            .join(".system_generated")
            .join("logs")
            .join("transcript.jsonl");
        if fs::metadata(&candidate).is_ok_and(|m| m.is_file()) {
            return Some(candidate);
        }
    }
    None
}

fn count_user_inputs_in_transcript_file(transcript_path: &Path) -> Option<u64> {
    let mut file = File::open(transcript_path).ok()?;
    let mut buf = [0u8; 64 * 1024];
    let mut carry: Vec<u8> = Vec::new();
    let mut count = 0u64;

    let count_line = |line_bytes: &[u8], count: &mut u64| {
        let Ok(s) = std::str::from_utf8(line_bytes) else {
            return;
        };
        if !s.contains("\"USER_INPUT\"") {
            return;
        }
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return;
        }
        if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(trimmed)
            && map.get("type").and_then(serde_json::Value::as_str) == Some("USER_INPUT")
        {
            *count += 1;
        }
    };

    while let Ok(n) = file.read(&mut buf) {
        if n == 0 {
            break;
        }
        let slice = &buf[..n];
        let mut start = 0usize;
        while start < slice.len() {
            if let Some(rel) = slice[start..].iter().position(|&b| b == b'\n') {
                let end = start + rel;
                if carry.is_empty() {
                    count_line(&slice[start..end], &mut count);
                } else {
                    carry.extend_from_slice(&slice[start..end]);
                    count_line(&carry, &mut count);
                    carry.clear();
                }
                start = end + 1;
            } else {
                let tail = &slice[start..];
                if carry.len() + tail.len() <= 4 * 1024 * 1024 {
                    carry.extend_from_slice(tail);
                }
                break;
            }
        }
    }
    if !carry.is_empty() {
        count_line(&carry, &mut count);
    }
    Some(count)
}

fn estimate_db_user_turns(
    db_path: &Path,
    telemetry: &RawDbTelemetry,
    candidate_roots: &[PathBuf],
) -> u64 {
    let conv_id = db_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let is_child_db = telemetry.parent_cascade_id.is_some()
        || telemetry
            .root_cascade_id
            .as_deref()
            .is_some_and(|r| !conv_id.is_empty() && r != conv_id);
    let total_steps: u64 = telemetry.models.values().map(|s| s.steps).sum();
    let has_perf = has_valid_model_perf_samples(&telemetry.models);
    let has_activity = total_steps > 0 || has_perf;

    if let Some(tp) = find_transcript_for_db(db_path, candidate_roots)
        && let Some(counted) = count_user_inputs_in_transcript_file(&tp)
    {
        if counted > 0 {
            return counted;
        }
        return if has_activity {
            u64::from(!is_child_db)
        } else {
            0
        };
    }

    if !is_child_db && has_activity {
        (total_steps.div_ceil(4)).max(1)
    } else {
        0
    }
}

pub(super) fn merge_db_models_into(
    target: &mut BTreeMap<String, ModelTokenAcc>,
    source: &BTreeMap<String, DbModelPerfAcc>,
) {
    for (model_key, stats) in source {
        if model_key.is_empty() {
            continue;
        }
        let cost_usd = if stats.cost_usd.is_finite() && stats.cost_usd >= 0.0 {
            stats.cost_usd
        } else {
            0.0
        };
        if stats.steps == 0
            && cost_usd == 0.0
            && stats.input_tokens == 0
            && stats.cache_read_tokens == 0
            && stats.cache_write_tokens == 0
            && stats.output_tokens == 0
        {
            continue;
        }
        let acc = target.entry(model_key.clone()).or_default();
        acc.cost_usd += cost_usd;
        acc.input_tokens = acc.input_tokens.saturating_add(stats.input_tokens);
        acc.cache_read_tokens = acc.cache_read_tokens.saturating_add(stats.cache_read_tokens);
        acc.cache_write_tokens = acc.cache_write_tokens.saturating_add(stats.cache_write_tokens);
        acc.output_tokens = acc.output_tokens.saturating_add(stats.output_tokens);
        acc.steps = acc.steps.saturating_add(stats.steps);
    }
}

/// Aggregate recent `<= 30` non-empty `.db` streaming speeds & TTFTs, and resolve
/// the active Cascade's lineage cost tree (main + all descendant subagent DBs).
#[allow(clippy::too_many_arguments)]
pub(super) fn resolve_model_perf_stats(
    conversation_id: Option<&str>,
    transcript_path: Option<&Path>,
    child_conversation_ids: &[String],
    current_session_turns: Option<u64>,
    current_session_model_turns_ago: &BTreeMap<String, u64>,
    current_session_model_last_active_ms: &BTreeMap<String, i64>,
    now_ms: i64,
) -> ResolvedModelPerf {
    let (transcript_base_dir, derived_conv_id) = extract_transcript_context(transcript_path);
    let effective_conv_id = conversation_id.or(derived_conv_id.as_deref());
    let child_ids_set: HashSet<String> = child_conversation_ids
        .iter()
        .filter(|id| !id.is_empty() && effective_conv_id != Some(id.as_str()))
        .cloned()
        .collect();
    let has_cascade_scope = effective_conv_id.is_some() || !child_ids_set.is_empty();

    let candidate_roots = collect_candidate_roots(transcript_base_dir.as_deref());
    let cache_path = resolve_compaction_cache_path();
    let mut cache = read_compaction_cache(&cache_path);
    let active_db_path = resolve_active_db_path(effective_conv_id, &candidate_roots);
    let all_dbs = list_recent_conversation_dbs(&candidate_roots, None, usize::MAX);

    if active_db_path.is_none() && all_dbs.is_empty() && !has_cascade_scope {
        return ResolvedModelPerf::default();
    }

    let max_cached_access_at = cache
        .db_perf
        .values()
        .map(|e| e.last_access_at.max(e.updated_at))
        .max()
        .unwrap_or(0);

    struct DbPerfLoader<'a> {
        cache: &'a CompactionCacheFile,
        candidate_roots: &'a [PathBuf],
        now_ms: i64,
        max_cached_access_at: i64,
        next_access_at: i64,
        patch_db_perf: BTreeMap<String, DbPerfCacheEntry>,
        loaded_by_path: BTreeMap<PathBuf, DbPerfCacheEntry>,
        attempted_paths: HashSet<PathBuf>,
    }

    impl<'a> DbPerfLoader<'a> {
        fn load(&mut self, db_path: &Path, touch_lru: bool) -> Option<DbPerfCacheEntry> {
            if let Some(existing) = self.loaded_by_path.get(db_path).cloned() {
                let key = db_path.to_string_lossy().into_owned();
                if touch_lru
                    && existing.last_access_at.max(existing.updated_at) < self.max_cached_access_at
                    && !self.patch_db_perf.contains_key(&key)
                {
                    let mut touched = existing;
                    touched.last_access_at = self.next_access_at;
                    self.next_access_at += 1;
                    self.loaded_by_path
                        .insert(db_path.to_path_buf(), touched.clone());
                    self.patch_db_perf.insert(key, touched.clone());
                    return Some(touched);
                }
                return Some(existing);
            }
            if !self.attempted_paths.insert(db_path.to_path_buf()) {
                return None;
            }

            let key = db_path.to_string_lossy().into_owned();
            let sig = get_db_signature(db_path);
            if let Some(ref sig_str) = sig
                && let Some(cached) = self.cache.db_perf.get(&key)
                && cached.schema_ver == PERF_DB_SCHEMA_VERSION
                && cached.sig == *sig_str
            {
                if touch_lru
                    && cached.last_access_at.max(cached.updated_at) < self.max_cached_access_at
                {
                    let mut touched = cached.clone();
                    touched.last_access_at = self.next_access_at;
                    self.next_access_at += 1;
                    self.loaded_by_path
                        .insert(db_path.to_path_buf(), touched.clone());
                    self.patch_db_perf.insert(key, touched.clone());
                    return Some(touched);
                }
                self.loaded_by_path
                    .insert(db_path.to_path_buf(), cached.clone());
                return Some(cached.clone());
            }

            let telemetry = query_raw_db_telemetry(db_path)?;
            let user_turns = estimate_db_user_turns(db_path, &telemetry, self.candidate_roots);
            let conv_id = db_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let record = DbPerfCacheEntry {
                schema_ver: PERF_DB_SCHEMA_VERSION,
                sig: sig.unwrap_or_default(),
                conversation_id: conv_id,
                parent_cascade_id: telemetry.parent_cascade_id,
                root_cascade_id: telemetry.root_cascade_id,
                user_turns,
                max_step_index: telemetry.max_step_index,
                total_token_steps: telemetry.total_token_steps,
                models: telemetry.models,
                total_cost_usd: telemetry.total_cost_usd,
                updated_at: self.now_ms,
                last_access_at: self.next_access_at,
            };
            self.next_access_at += 1;
            self.loaded_by_path
                .insert(db_path.to_path_buf(), record.clone());
            self.patch_db_perf.insert(key, record.clone());
            Some(record)
        }
    }

    let mut loader = DbPerfLoader {
        cache: &cache,
        candidate_roots: &candidate_roots,
        now_ms,
        max_cached_access_at,
        next_access_at: now_ms.max(max_cached_access_at + 1),
        patch_db_perf: BTreeMap::new(),
        loaded_by_path: BTreeMap::new(),
        attempted_paths: HashSet::new(),
    };

    let mut perf_window_dbs: Vec<DbPathEntry> = Vec::new();
    let mut perf_seen_paths: HashSet<PathBuf> = HashSet::new();

    if let Some(ref active_path) = active_db_path
        && let Some(rec) = loader.load(active_path, true)
        && has_valid_model_perf_samples(&rec.models)
    {
        perf_window_dbs.push(DbPathEntry {
            path: active_path.clone(),
            mtime_ms: get_db_max_mtime_ms(active_path),
        });
        perf_seen_paths.insert(active_path.clone());
    }

    for item in &all_dbs {
        if perf_window_dbs.len() >= MAX_PERF_RECENT_DBS {
            break;
        }
        if perf_seen_paths.contains(&item.path) {
            continue;
        }
        if let Some(rec) = loader.load(&item.path, false)
            && has_valid_model_perf_samples(&rec.models)
        {
            perf_window_dbs.push(item.clone());
            perf_seen_paths.insert(item.path.clone());
        }
    }

    let mut lineage_conv_ids: HashSet<String> = HashSet::new();
    if let Some(cid) = effective_conv_id {
        lineage_conv_ids.insert(cid.to_string());
    }
    for cid in &child_ids_set {
        lineage_conv_ids.insert(cid.clone());
    }

    if has_cascade_scope {
        if let Some(ref active_path) = active_db_path {
            loader.load(active_path, true);
        }
        for child_id in &child_ids_set {
            if let Some(child_db) = resolve_active_db_path(Some(child_id), &candidate_roots) {
                loader.load(&child_db, true);
            }
        }
        for item in all_dbs.iter().take(MAX_PERF_DB_ENTRIES) {
            loader.load(&item.path, false);
        }

        let mut prev_size = usize::MAX;
        while lineage_conv_ids.len() != prev_size {
            prev_size = lineage_conv_ids.len();
            let mut added = true;
            while added {
                added = false;
                for entry in cache.db_perf.values().chain(loader.loaded_by_path.values()) {
                    let cid = entry.conversation_id.as_str();
                    if cid.is_empty() || lineage_conv_ids.contains(cid) {
                        continue;
                    }
                    let linked = entry
                        .root_cascade_id
                        .as_deref()
                        .is_some_and(|r| lineage_conv_ids.contains(r))
                        || entry
                            .parent_cascade_id
                            .as_deref()
                            .is_some_and(|p| lineage_conv_ids.contains(p));
                    if linked {
                        lineage_conv_ids.insert(cid.to_string());
                        added = true;
                    }
                }
            }

            let cached_matching_paths: Vec<PathBuf> = cache
                .db_perf
                .iter()
                .filter_map(|(path_str, entry)| {
                    if !entry.conversation_id.is_empty()
                        && lineage_conv_ids.contains(&entry.conversation_id)
                    {
                        let p = PathBuf::from(path_str);
                        if !loader.loaded_by_path.contains_key(&p)
                            && fs::metadata(&p).is_ok_and(|m| m.is_file())
                        {
                            return Some(p);
                        }
                    }
                    None
                })
                .collect();
            for p in cached_matching_paths {
                loader.load(&p, true);
            }

            let lineage_list: Vec<String> = lineage_conv_ids.iter().cloned().collect();
            for cid in lineage_list {
                if let Some(resolved_path) = resolve_active_db_path(Some(&cid), &candidate_roots)
                    && !loader.loaded_by_path.contains_key(&resolved_path)
                {
                    loader.load(&resolved_path, true);
                }
            }
        }
    }

    let loaded_by_path = loader.loaded_by_path;
    let patch_db_perf = loader.patch_db_perf;

    if loaded_by_path.is_empty() && perf_window_dbs.is_empty() {
        return ResolvedModelPerf::default();
    }

    let mut protected_perf_keys: HashSet<String> = HashSet::new();
    if has_cascade_scope && !lineage_conv_ids.is_empty() {
        for (db_path_str, entry) in &cache.db_perf {
            let cid = if !entry.conversation_id.is_empty() {
                entry.conversation_id.clone()
            } else {
                Path::new(db_path_str)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string()
            };
            if !cid.is_empty() && lineage_conv_ids.contains(&cid) {
                protected_perf_keys.insert(db_path_str.clone());
            }
        }
        for (db_path, entry) in &loaded_by_path {
            let cid = if !entry.conversation_id.is_empty() {
                entry.conversation_id.clone()
            } else {
                db_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string()
            };
            if !cid.is_empty() && lineage_conv_ids.contains(&cid) {
                protected_perf_keys.insert(db_path.to_string_lossy().into_owned());
            }
        }
    }

    if !patch_db_perf.is_empty() {
        cache.db_perf.extend(patch_db_perf);
        prune_perf_cache(&mut cache.db_perf, MAX_PERF_DB_ENTRIES, &protected_perf_keys);
        write_compaction_cache(&cache_path, &cache);
    }

    #[derive(Default)]
    struct PerfTotalsAcc {
        stream_samples: u64,
        sum_out: u64,
        sum_stream_sec: f64,
        ttft_samples: u64,
        sum_ttft_sec: f64,
        last_updated_ms: i64,
    }

    let mut totals: BTreeMap<String, PerfTotalsAcc> = BTreeMap::new();
    for item in &perf_window_dbs {
        let Some(record) = loaded_by_path.get(&item.path) else {
            continue;
        };
        let db_mtime_ms = item.mtime_ms.max(0);
        for (model_key, stats) in &record.models {
            if model_key.is_empty() || (stats.stream_samples == 0 && stats.ttft_samples == 0) {
                continue;
            }
            let acc = totals.entry(model_key.clone()).or_default();
            acc.stream_samples = acc.stream_samples.saturating_add(stats.stream_samples);
            acc.sum_out = acc.sum_out.saturating_add(stats.sum_out);
            acc.sum_stream_sec += stats.sum_stream_sec;
            acc.ttft_samples = acc.ttft_samples.saturating_add(stats.ttft_samples);
            acc.sum_ttft_sec += stats.sum_ttft_sec;
            if db_mtime_ms > acc.last_updated_ms {
                acc.last_updated_ms = db_mtime_ms;
            }
        }
    }

    let mtime_by_path: HashMap<&Path, i64> = all_dbs
        .iter()
        .map(|d| (d.path.as_path(), d.mtime_ms))
        .collect();
    struct LoadedConvItem<'a> {
        record: &'a DbPerfCacheEntry,
        mtime_ms: i64,
    }
    let mut by_conv_id: BTreeMap<String, LoadedConvItem<'_>> = BTreeMap::new();
    for (db_path, record) in &loaded_by_path {
        let cid = if !record.conversation_id.is_empty() {
            record.conversation_id.clone()
        } else {
            db_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string()
        };
        if cid.is_empty() {
            continue;
        }
        let db_mtime_ms = mtime_by_path
            .get(db_path.as_path())
            .copied()
            .unwrap_or_else(|| get_db_max_mtime_ms(db_path));
        let replace = by_conv_id
            .get(&cid)
            .is_none_or(|prev| db_mtime_ms > prev.mtime_ms);
        if replace {
            by_conv_id.insert(
                cid,
                LoadedConvItem {
                    record,
                    mtime_ms: db_mtime_ms,
                },
            );
        }
    }

    let resolve_root_key = |cid: &str, rec: &DbPerfCacheEntry| -> (String, String, bool) {
        if let Some(main_cid) = effective_conv_id
            && lineage_conv_ids.contains(cid)
        {
            return (main_cid.to_string(), main_cid.to_string(), false);
        }
        if let Some(ref root_id) = rec.root_cascade_id {
            if by_conv_id.contains_key(root_id) {
                return (root_id.clone(), root_id.clone(), false);
            }
            return (format!("orphan:{cid}"), cid.to_string(), true);
        }
        let mut cur_id = cid.to_string();
        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(cur_id.clone());
        loop {
            let parent_opt = by_conv_id.get(&cur_id).and_then(|item| {
                item.record
                    .parent_cascade_id
                    .as_deref()
                    .or(item.record.root_cascade_id.as_deref())
            });
            let Some(parent_id) = parent_opt else {
                return (cur_id.clone(), cur_id, false);
            };
            if visited.contains(parent_id) {
                return (cur_id.clone(), cur_id, false);
            }
            if !by_conv_id.contains_key(parent_id) {
                return (format!("orphan:{cid}"), cid.to_string(), true);
            }
            visited.insert(parent_id.to_string());
            cur_id = parent_id.to_string();
        }
    };

    struct RootNode {
        root_id: String,
        is_orphan: bool,
        mtime_ms: i64,
        user_turns: u64,
        db_models: HashSet<String>,
    }
    let mut root_nodes_map: BTreeMap<String, RootNode> = BTreeMap::new();
    for (cid, item) in &by_conv_id {
        let (root_key, root_id, is_orphan) = resolve_root_key(cid, item.record);
        let node = root_nodes_map.entry(root_key).or_insert_with(|| RootNode {
            root_id: root_id.clone(),
            is_orphan,
            mtime_ms: 0,
            user_turns: 0,
            db_models: HashSet::new(),
        });
        if item.mtime_ms > node.mtime_ms {
            node.mtime_ms = item.mtime_ms;
        }
        if !is_orphan && cid == &node.root_id {
            node.user_turns = item.record.user_turns;
        }
        for (m_key, stats) in &item.record.models {
            if !m_key.is_empty() && (stats.stream_samples > 0 || stats.ttft_samples > 0) {
                node.db_models.insert(m_key.clone());
            }
        }
    }

    let current_root_models_and_turns = effective_conv_id
        .and_then(|cid| root_nodes_map.get(cid))
        .map(|n| (n.db_models.clone(), n.is_orphan, n.user_turns));

    let mut historical_roots: Vec<&RootNode> = root_nodes_map
        .iter()
        .filter(|(k, _)| effective_conv_id != Some(k.as_str()))
        .map(|(_, v)| v)
        .collect();
    historical_roots.sort_by_key(|b| std::cmp::Reverse(b.mtime_ms));

    let mut min_turns_ago: BTreeMap<String, u64> = BTreeMap::new();
    let mut cumulative_turns = 0u64;
    let has_current_turn_ctx =
        current_session_turns.is_some() || !current_session_model_turns_ago.is_empty();

    if has_current_turn_ctx {
        for (m_key, &ago) in current_session_model_turns_ago {
            min_turns_ago
                .entry(m_key.clone())
                .and_modify(|e| *e = (*e).min(ago))
                .or_insert(ago);
        }
        if let Some((ref db_models, _, _)) = current_root_models_and_turns {
            for m_key in db_models {
                min_turns_ago.entry(m_key.clone()).or_insert(0);
            }
        }
        if let Some(turns) = current_session_turns {
            cumulative_turns = cumulative_turns.saturating_add(turns);
        }
    } else if let Some((ref db_models, is_orphan, user_turns)) = current_root_models_and_turns {
        for m_key in db_models {
            min_turns_ago.entry(m_key.clone()).or_insert(0);
        }
        if !is_orphan {
            cumulative_turns = cumulative_turns.saturating_add(user_turns.max(1));
        }
    }

    for node in historical_roots {
        for m_key in &node.db_models {
            min_turns_ago.entry(m_key.clone()).or_insert(cumulative_turns);
        }
        if !node.is_orphan {
            cumulative_turns = cumulative_turns.saturating_add(node.user_turns.max(1));
        }
    }

    let mut models = BTreeMap::new();
    for (model_key, agg) in totals {
        let raw_speed = if agg.sum_stream_sec > 0.0 && agg.sum_out > 0 {
            Some(agg.sum_out as f64 / agg.sum_stream_sec)
        } else {
            None
        };
        let avg_speed = raw_speed.filter(|&s| s.is_finite() && s > 0.0 && s <= MAX_VALID_STREAM_SPEED);
        let avg_ttft_sec = if agg.ttft_samples > 0 {
            Some(agg.sum_ttft_sec / agg.ttft_samples as f64)
        } else {
            None
        };
        let session_active = current_session_model_last_active_ms
            .get(&model_key)
            .copied()
            .unwrap_or(0);
        let last_updated_ms = agg.last_updated_ms.max(session_active);
        models.insert(
            model_key.clone(),
            ModelPerfSummary {
                model: model_key.clone(),
                avg_speed,
                avg_ttft_sec,
                stream_samples: agg.stream_samples,
                last_updated_ms,
                turns_ago: min_turns_ago.get(&model_key).copied().unwrap_or(0),
            },
        );
    }

    let mut cascade_cost_tree = None;
    if has_cascade_scope {
        let mut main_cost_usd = 0.0f64;
        let mut main_steps = 0u64;
        let mut main_max_step_index: Option<u64> = None;
        let mut main_by_model: BTreeMap<String, ModelTokenAcc> = BTreeMap::new();
        let mut children_cost_usd = 0.0f64;
        let mut children_by_model: BTreeMap<String, ModelTokenAcc> = BTreeMap::new();
        let mut matched_by_id: BTreeMap<String, CascadeDbEntryCost> = BTreeMap::new();
        let mut seen_conv_ids: HashSet<String> = HashSet::new();

        for (db_path, record) in &loaded_by_path {
            let db_conv_id = if !record.conversation_id.is_empty() {
                record.conversation_id.clone()
            } else {
                db_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string()
            };
            if db_conv_id.is_empty() || seen_conv_ids.contains(&db_conv_id) {
                continue;
            }
            let is_main = effective_conv_id == Some(db_conv_id.as_str());
            let is_child = !is_main
                && (lineage_conv_ids.contains(&db_conv_id)
                    || effective_conv_id.is_some_and(|main_id| {
                        record.root_cascade_id.as_deref() == Some(main_id)
                            || record.parent_cascade_id.as_deref() == Some(main_id)
                    }));
            if !is_main && !is_child {
                continue;
            }

            seen_conv_ids.insert(db_conv_id.clone());
            let db_cost = if record.total_cost_usd.is_finite() && record.total_cost_usd >= 0.0 {
                record.total_cost_usd
            } else {
                0.0
            };
            let mut entry_models = BTreeMap::new();
            merge_db_models_into(&mut entry_models, &record.models);
            matched_by_id.insert(
                db_conv_id,
                CascadeDbEntryCost {
                    total_cost_usd: db_cost,
                    steps: record.total_token_steps,
                    max_step_index: record.max_step_index,
                    models: entry_models,
                },
            );

            if is_main {
                main_cost_usd += db_cost;
                main_steps = main_steps.saturating_add(record.total_token_steps);
                if let Some(ms) = record.max_step_index {
                    main_max_step_index = Some(main_max_step_index.map_or(ms, |cur| cur.max(ms)));
                }
                merge_db_models_into(&mut main_by_model, &record.models);
            } else {
                children_cost_usd += db_cost;
                merge_db_models_into(&mut children_by_model, &record.models);
            }
        }

        if !matched_by_id.is_empty() {
            cascade_cost_tree = Some(CascadeDbCostTree {
                main_cost_usd,
                main_steps,
                main_max_step_index,
                main_by_model,
                children_cost_usd,
                children_by_model,
                matched_by_id,
            });
        }
    }

    ResolvedModelPerf {
        models,
        cascade_cost_tree,
    }
}

/// Read a full pure-data `ConversationDbRecord` from a host `.db` safely without locking
/// or exposing `rusqlite::Connection` / raw Protobuf bytes (designed for Ticket 05 `ledger`).
pub(super) fn read_conversation_db(db_path: &Path) -> Option<ConversationDbRecord> {
    let conversation_id = db_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

    with_readonly_db(db_path, |conn| {
        let mut rec = ConversationDbRecord {
            conversation_id: conversation_id.clone(),
            db_path: db_path.to_path_buf(),
            ..Default::default()
        };

        let traj_sql = "SELECT * FROM trajectory_metadata_blob WHERE id = 'main' LIMIT 1";
        let traj_fallback_sql = "SELECT * FROM trajectory_metadata_blob LIMIT 1";
        let mut read_traj = |sql: &str| -> Result<bool, rusqlite::Error> {
            match conn.prepare(sql) {
                Ok(mut stmt) => {
                    let mut rows = stmt.query([])?;
                    if let Some(row) = rows.next()? {
                        for col_name in ["data", "blob", "metadata", "value"] {
                            if let Ok(blob) = row.get::<_, Vec<u8>>(col_name)
                                && !blob.is_empty()
                            {
                                let meta = extract_trajectory_meta(&blob);
                                if rec.cascade_id.is_none() {
                                    rec.cascade_id = meta.cascade_id.clone();
                                }
                                rec.trajectory = Some(meta);
                                return Ok(true);
                            }
                        }
                        return Ok(true);
                    }
                    Ok(false)
                }
                Err(err) if is_sqlite_open_or_access_error(&err) => Err(err),
                Err(_) => Ok(false),
            }
        };
        if !read_traj(traj_sql)? {
            let _ = read_traj(traj_fallback_sql)?;
        }

        match conn.prepare("SELECT idx, metadata FROM steps") {
            Ok(mut stmt) => {
                let mut rows = stmt.query([])?;
                while let Some(row) = rows.next()? {
                    let idx: i64 = row.get(0)?;
                    let blob: Vec<u8> = row.get(1)?;
                    if blob.is_empty() {
                        continue;
                    }
                    let (ts_opt, bot_opt) = extract_step_timestamp_and_bot(&blob);
                    if let Some(ts_ms) = ts_opt {
                        rec.step_timestamps.insert(idx, ts_ms);
                        if let Some(bot_uuid) = bot_opt {
                            rec.bot_timestamps.entry(bot_uuid).or_insert(ts_ms);
                        }
                    }
                }
            }
            Err(err) if is_sqlite_open_or_access_error(&err) => return Err(err),
            Err(_) => {}
        }

        match conn.prepare("SELECT idx, data FROM gen_metadata ORDER BY idx ASC") {
            Ok(mut stmt) => {
                let mut rows = stmt.query([])?;
                while let Some(row) = rows.next()? {
                    let idx: i64 = row.get(0)?;
                    let blob: Vec<u8> = row.get(1)?;
                    if blob.is_empty() {
                        continue;
                    }
                    if rec.compaction_limit.is_none() {
                        rec.compaction_limit = extract_compaction_limit_from_gen_metadata(&blob);
                    }
                    let decoded = extract_gen_metadata_row_details(idx, &blob);
                    if rec.parent_cascade_id.is_none() {
                        rec.parent_cascade_id = decoded.parent_cascade_id.clone();
                    }
                    if rec.root_cascade_id.is_none() {
                        rec.root_cascade_id = decoded.root_cascade_id.clone();
                    }
                    if rec.cascade_id.is_none() {
                        rec.cascade_id = decoded.cascade_id.clone();
                    }
                    rec.gen_rows.push(decoded);
                }
            }
            Err(err) if is_sqlite_open_or_access_error(&err) => return Err(err),
            Err(err) if is_no_such_table_error(&err) => {}
            Err(_) => {}
        }

        if rec.compaction_limit.is_none()
            && let Ok(mut stmt) =
                conn.prepare("SELECT data FROM executor_metadata ORDER BY idx DESC LIMIT 16")
            && let Ok(mut rows) = stmt.query([])
        {
            while let Ok(Some(row)) = rows.next() {
                if let Ok(blob) = row.get::<_, Vec<u8>>(0)
                    && let Some(lim) = extract_compaction_limit_from_executor_metadata(&blob)
                {
                    rec.compaction_limit = Some(lim);
                    break;
                }
            }
        }

        Ok(rec)
    })
}
