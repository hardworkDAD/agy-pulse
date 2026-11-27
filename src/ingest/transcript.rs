use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Instant, UNIX_EPOCH};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::domain::{
    ChildModelSpeed, ImageQuotaExhausted, ModelTokenTotals, TokenUsage, TurnPeakByMode,
    calculate_token_cost, compact_model_name, model_includes_cache_in_input,
    parse_iso8601_to_epoch_ms,
};
use crate::platform;

use super::speed_tracker::{
    PlannerResponseSpan, is_valid_speed, prune_json_cache_dir, sha256_hex,
};
use super::stdin::{find_context_window_in_map, is_context_window_map};

const TRANSCRIPT_CACHE_VERSION: u32 = 11;
const TRANSCRIPT_CACHE_HEAD_BYTES: usize = 256;
const MAX_TRANSCRIPT_CACHE_FILES: usize = 32;
const FULL_CHUNK_BYTES: usize = 64 * 1024;
const MAX_FULL_LINE_BYTES: usize = 4 * 1024 * 1024;
const MAX_FULL_TRANSCRIPT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CHILD_AGENTS: usize = 256;
const MAX_CHILD_TAIL_READS: usize = 32;
const STREAMING_DELTA_THRESHOLD_BYTES: u64 = 8 * 1024 * 1024;
const MAX_TRANSCRIPT_DELTA_BYTES: u64 = 64 * 1024 * 1024;
const MAX_RECENT_TOKEN_STEPS: usize = 512;
const MAX_CHILD_TAIL_BYTES: u64 = 65_536;
const MAX_CHILD_OPEN_TURN_MS: i64 = 120_000;
const MIN_CHILD_API_DELTA_MS: i64 = 500;
const MAX_PERSISTED_CHILD_SPEEDS: usize = 32;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct CachedTurnUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub model: Option<String>,
    pub cache_exclusive: bool,
}

impl CachedTurnUsage {
    pub fn to_domain(&self) -> TokenUsage {
        TokenUsage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_read_input_tokens: self.cache_read_input_tokens,
            cache_creation_input_tokens: self.cache_creation_input_tokens,
            cache_exclusive: self.cache_exclusive,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct ModelTokenAcc {
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
    pub steps: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct TranscriptTokenStepCost {
    pub step_index: Option<u64>,
    pub model_key: String,
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct SubagentHint {
    model: String,
    role: String,
    type_name: String,
    step_index: u64,
    time_ms: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ChildAgentRecord {
    id: String,
    kind: String,
    name: String,
    model_hint: String,
    active: bool,
    killed: bool,
    added_step: u64,
    last_active_ms: i64,
    added_turn: u64,
    last_active_turn: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct PersistedChildSpeed {
    model: String,
    speed: f64,
    updated_at: i64,
    event_ms: i64,
    mtime_ms: i64,
    first_order: u64,
    last_active_turn: Option<u64>,
    last_active_ms: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct TranscriptAccState {
    steps: u64,
    usage: Option<Value>,
    max_historical_cache: u64,
    image_exhausted_reset_ms: Option<i64>,
    total_api_duration_ms: u64,
    pending_planner_start_ms: Option<i64>,
    last_planner_response: Option<PlannerResponseSpan>,
    compaction_count: u64,
    total_input_tokens: u64,
    total_output_tokens: u64,
    total_cache_read_tokens: u64,
    total_cache_creation_tokens: u64,
    transcript_cost_usd: f64,
    token_steps_count: u64,
    recent_token_steps: Vec<TranscriptTokenStepCost>,
    model_totals: BTreeMap<String, ModelTokenAcc>,
    last_known_model: String,
    peak_raw_turn_tokens: u64,
    last_turn_usage: Option<CachedTurnUsage>,
    inclusive_peak: u64,
    exclusive_peak: u64,
    child_agents: BTreeMap<String, ChildAgentRecord>,
    persisted_child_speeds: BTreeMap<String, PersistedChildSpeed>,
    user_turns: u64,
    last_user_input_ms: i64,
    has_planner_response: bool,
    model_last_seen_turn: BTreeMap<String, u64>,
    model_last_active_ms: BTreeMap<String, i64>,
    pending_subagent_hints: Vec<SubagentHint>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct TranscriptCacheFile {
    version: u32,
    ino: u64,
    offset: u64,
    head_len: usize,
    head_hash: String,
    state: TranscriptAccState,
}

pub(super) struct ChildActivitySummary {
    pub model_hint: String,
    pub last_active_turn: u64,
    pub last_active_ms: i64,
}

pub(super) struct ScannedTranscript {
    pub steps: u64,
    pub user_turns: u64,
    pub last_user_input_ms: Option<i64>,
    pub has_planner_response: bool,
    pub usage: Option<Value>,
    pub last_turn_usage: Option<CachedTurnUsage>,
    pub last_known_model: String,
    pub max_historical_cache: u64,
    pub compaction_count: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub peak_raw_turn_tokens: u64,
    pub turn_history_max_by_mode: TurnPeakByMode,
    pub last_planner_response: Option<PlannerResponseSpan>,
    pub main_model_last_active_ms: Option<i64>,
    pub child_model_speeds: Vec<ChildModelSpeed>,
    pub model_token_totals: Vec<ModelTokenTotals>,
    pub image_exhausted: Option<ImageQuotaExhausted>,
    pub transcript_cost_usd: f64,
    pub transcript_cost_by_model: BTreeMap<String, ModelTokenAcc>,
    pub token_steps_count: u64,
    pub recent_token_steps: Vec<TranscriptTokenStepCost>,
    pub all_child_ids: Vec<String>,
    pub model_last_seen_turn: BTreeMap<String, u64>,
    pub model_last_active_ms: BTreeMap<String, i64>,
    pub child_activities: Vec<ChildActivitySummary>,
    pub persisted_child_turns: Vec<(String, Option<u64>, i64)>,
    pub child_tail_activities: Vec<(String, i64)>,
}

fn normalized_transcript_path_str(transcript_path: &Path) -> String {
    let resolved = fs::canonicalize(transcript_path)
        .or_else(|_| std::path::absolute(transcript_path))
        .unwrap_or_else(|_| transcript_path.to_path_buf());
    resolved.to_string_lossy().into_owned()
}

fn transcript_cache_path(transcript_path: &Path) -> PathBuf {
    let key = sha256_hex(normalized_transcript_path_str(transcript_path).as_bytes());
    platform::resolve_antigravity_path(&format!("transcript-cache/{key}.json"))
}

fn compute_head_hash(transcript_path: &Path, byte_len: usize) -> Option<String> {
    let len = byte_len.min(TRANSCRIPT_CACHE_HEAD_BYTES);
    if len == 0 {
        return Some(String::new());
    }
    let mut file = File::open(transcript_path).ok()?;
    let mut buf = vec![0u8; len];
    let mut total = 0usize;
    while total < len {
        match file.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(_) => return None,
        }
    }
    Some(sha256_hex(&buf[..total]))
}

fn read_transcript_cache(
    cache_path: &Path,
    expected_ino: u64,
    file_size: u64,
    transcript_path: &Path,
) -> Option<(u64, TranscriptAccState)> {
    let raw = fs::read_to_string(cache_path).ok()?;
    let cached: TranscriptCacheFile = serde_json::from_str(&raw).ok()?;
    if cached.version != TRANSCRIPT_CACHE_VERSION {
        return None;
    }
    if cached.ino != expected_ino {
        return None;
    }
    if cached.offset > file_size {
        return None;
    }
    if cached.head_len > TRANSCRIPT_CACHE_HEAD_BYTES || (cached.head_len as u64) > file_size {
        return None;
    }
    let current_head_hash = compute_head_hash(transcript_path, cached.head_len)?;
    if current_head_hash != cached.head_hash {
        return None;
    }
    Some((cached.offset, cached.state))
}

fn write_transcript_cache(
    cache_path: &Path,
    ino: u64,
    offset: u64,
    state: &TranscriptAccState,
    transcript_path: &Path,
    file_size: u64,
) {
    let head_len = (file_size as usize).min(TRANSCRIPT_CACHE_HEAD_BYTES);
    let Some(head_hash) = compute_head_hash(transcript_path, head_len) else {
        return;
    };
    let payload = TranscriptCacheFile {
        version: TRANSCRIPT_CACHE_VERSION,
        ino,
        offset,
        head_len,
        head_hash,
        state: state.clone(),
    };
    let Ok(bytes) = serde_json::to_vec(&payload) else {
        return;
    };
    if platform::write_cache_atomic_0600(cache_path, &bytes).is_ok()
        && let Some(dir) = cache_path.parent()
    {
        prune_json_cache_dir(dir, MAX_TRANSCRIPT_CACHE_FILES);
    }
}

fn as_u64_val(val: Option<&Value>) -> Option<u64> {
    let v = val?;
    if let Some(u) = v.as_u64() {
        return Some(u);
    }
    if let Some(f) = v.as_f64()
        && f.is_finite()
        && f >= 0.0
    {
        return Some(f.trunc() as u64);
    }
    None
}

fn parse_entry_time_ms(entry: &Map<String, Value>) -> Option<i64> {
    entry
        .get("created_at")
        .and_then(Value::as_str)
        .and_then(parse_iso8601_to_epoch_ms)
}

fn load_full_entries_map(transcript_path: &Path, needed_steps: &HashSet<u64>) -> HashMap<u64, Value> {
    let mut map = HashMap::new();
    if needed_steps.is_empty() {
        return map;
    }
    let Some(parent) = transcript_path.parent() else {
        return map;
    };
    let full_path = parent.join("transcript_full.jsonl");
    let Ok(meta) = fs::metadata(&full_path) else {
        return map;
    };
    if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_FULL_TRANSCRIPT_BYTES {
        return map;
    }
    let Ok(mut file) = File::open(&full_path) else {
        return map;
    };

    let needles: Vec<String> = needed_steps.iter().map(ToString::to_string).collect();
    let mut buf = vec![0u8; FULL_CHUNK_BYTES];
    let mut carry: Vec<u8> = Vec::new();
    let mut oversized = false;

    let process_line = |line_bytes: &[u8], out: &mut HashMap<u64, Value>| {
        if line_bytes.is_empty() || line_bytes.len() > MAX_FULL_LINE_BYTES {
            return;
        }
        let Ok(line_str) = std::str::from_utf8(line_bytes) else {
            return;
        };
        if !needles.iter().any(|n| line_str.contains(n)) {
            return;
        }
        let trimmed = line_str.trim();
        if trimmed.is_empty() {
            return;
        }
        if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(trimmed)
            && let Some(step) = as_u64_val(obj.get("step_index"))
            && needed_steps.contains(&step)
        {
            out.insert(step, Value::Object(obj));
        }
    };

    while let Ok(n) = file.read(&mut buf) {
        if n == 0 {
            break;
        }
        let slice = &buf[..n];
        let mut start = 0usize;
        while start < slice.len() {
            if let Some(rel_nl) = slice[start..].iter().position(|&b| b == b'\n') {
                let end = start + rel_nl;
                if oversized {
                    oversized = false;
                    carry.clear();
                } else if carry.is_empty() {
                    process_line(&slice[start..end], &mut map);
                } else {
                    carry.extend_from_slice(&slice[start..end]);
                    process_line(&carry, &mut map);
                    carry.clear();
                }
                if map.len() >= needed_steps.len() {
                    return map;
                }
                start = end + 1;
            } else {
                let tail = &slice[start..];
                if !oversized {
                    if carry.len() + tail.len() > MAX_FULL_LINE_BYTES {
                        oversized = true;
                        carry.clear();
                    } else {
                        carry.extend_from_slice(tail);
                    }
                }
                break;
            }
        }
    }
    if !oversized && !carry.is_empty() {
        process_line(&carry, &mut map);
    }
    map
}

pub(super) fn resolve_brain_context(transcript_path: &Path) -> (Option<PathBuf>, Option<String>) {
    let resolved = fs::canonicalize(transcript_path)
        .or_else(|_| std::path::absolute(transcript_path))
        .unwrap_or_else(|_| transcript_path.to_path_buf());

    if let Some(logs_dir) = resolved.parent()
        && let Some(sys_dir) = logs_dir.parent()
        && let Some(conv_dir) = sys_dir.parent()
        && let Some(maybe_brain) = conv_dir.parent()
        && maybe_brain.file_name().and_then(|s| s.to_str()) == Some("brain")
    {
        let conv_id = conv_dir
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);
        return (Some(maybe_brain.to_path_buf()), conv_id);
    }

    let norm = resolved.to_string_lossy().replace('\\', "/");
    if let Some(idx) = norm.rfind("/brain/") {
        let after = &norm[idx + "/brain/".len()..];
        let conv_id = after
            .split('/')
            .next()
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);
        let brain_dir = PathBuf::from(&norm[..idx + "/brain".len()]);
        return (Some(brain_dir), conv_id);
    }
    (None, None)
}

fn extract_json_objects(text: &str, max_objects: usize) -> Vec<Map<String, Value>> {
    if !text.contains('{') {
        return Vec::new();
    }
    let mut objects = Vec::new();
    let bytes = text.as_bytes();
    let limit = bytes.len().min(262_144);
    let mut i = 0usize;
    let mut retries = 0usize;

    while i < limit && objects.len() < max_objects {
        let Some(rel) = bytes[i..limit].iter().position(|&b| b == b'{') else {
            break;
        };
        let start = i + rel;
        let mut depth = 0i32;
        let mut in_string = false;
        let mut escape = false;
        let mut end = None;

        for (offset, &b) in bytes[start..limit].iter().enumerate() {
            if in_string {
                if escape {
                    escape = false;
                } else if b == b'\\' {
                    escape = true;
                } else if b == b'"' {
                    in_string = false;
                }
            } else if b == b'"' {
                in_string = true;
            } else if b == b'{' {
                depth += 1;
            } else if b == b'}' {
                depth -= 1;
                if depth == 0 {
                    end = Some(start + offset);
                    break;
                }
            }
        }

        let Some(end_idx) = end else {
            i = start + 1;
            retries += 1;
            if retries > 32 {
                break;
            }
            continue;
        };

        if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text[start..=end_idx]) {
            objects.push(map);
            i = end_idx + 1;
        } else {
            i = start + 1;
            retries += 1;
            if retries > 32 {
                break;
            }
        }
    }

    objects
}

fn has_mention(text: &str, name: &str) -> bool {
    if text.is_empty() || name.is_empty() {
        return false;
    }
    let needle = format!("@{name}");
    let mut start = 0usize;
    while let Some(pos) = text[start..].find(&needle) {
        let abs = start + pos;
        let after = abs + needle.len();
        let next_ch = text[after..].chars().next();
        let boundary = next_ch
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'));
        if boundary {
            return true;
        }
        start = after;
    }
    false
}

fn effective_turn(acc: &TranscriptAccState) -> u64 {
    if acc.user_turns > 0 {
        acc.user_turns
    } else if acc.has_planner_response {
        1
    } else {
        0
    }
}

fn register_swarm_member(
    acc: &mut TranscriptAccState,
    id: &str,
    name: Option<&str>,
    model_hint: Option<&str>,
    step_idx: u64,
    valid_time_ms: Option<i64>,
    main_conv_id: Option<&str>,
) {
    if id.is_empty() || main_conv_id == Some(id) {
        return;
    }
    let cur_turn = effective_turn(acc);
    let existing = acc.child_agents.get(id);
    let resolved_name = name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| existing.map(|e| e.name.clone()))
        .unwrap_or_else(|| id.chars().take(8).collect());
    let resolved_hint = model_hint
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| existing.map(|e| e.model_hint.clone()))
        .unwrap_or_default();
    let prev_ms = existing.map(|e| e.last_active_ms).unwrap_or(0);
    let added_turn = existing.map(|e| e.added_turn).unwrap_or(cur_turn);

    acc.child_agents.insert(
        id.to_string(),
        ChildAgentRecord {
            id: id.to_string(),
            kind: "swarm".to_string(),
            name: resolved_name,
            model_hint: resolved_hint,
            active: true,
            killed: false,
            added_step: step_idx,
            last_active_ms: valid_time_ms.unwrap_or(0).max(prev_ms),
            added_turn,
            last_active_turn: cur_turn,
        },
    );
}

fn register_subagent(
    acc: &mut TranscriptAccState,
    id: &str,
    model_hint: Option<&str>,
    role_hint: Option<&str>,
    step_idx: u64,
    valid_time_ms: Option<i64>,
    main_conv_id: Option<&str>,
) {
    if id.is_empty() || main_conv_id == Some(id) {
        return;
    }
    if acc
        .child_agents
        .get(id)
        .is_some_and(|e| e.kind == "swarm")
    {
        return;
    }
    let cur_turn = effective_turn(acc);
    let is_new = !acc.child_agents.contains_key(id);
    let hint = if is_new && !acc.pending_subagent_hints.is_empty() {
        Some(acc.pending_subagent_hints.remove(0))
    } else {
        None
    };
    let existing = acc.child_agents.get(id);
    let resolved_hint = model_hint
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| hint.as_ref().map(|h| h.model.clone()).filter(|s| !s.is_empty()))
        .or_else(|| existing.map(|e| e.model_hint.clone()))
        .unwrap_or_default();
    let resolved_name = role_hint
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            hint.as_ref()
                .map(|h| {
                    if !h.role.is_empty() {
                        h.role.clone()
                    } else {
                        h.type_name.clone()
                    }
                })
                .filter(|s| !s.is_empty())
        })
        .or_else(|| existing.map(|e| e.name.clone()))
        .unwrap_or_else(|| id.chars().take(8).collect());
    let prev_ms = existing.map(|e| e.last_active_ms).unwrap_or(0);
    let hint_ms = hint.as_ref().map(|h| h.time_ms).unwrap_or(0);
    let added_turn = existing.map(|e| e.added_turn).unwrap_or(cur_turn);

    acc.child_agents.insert(
        id.to_string(),
        ChildAgentRecord {
            id: id.to_string(),
            kind: "subagent".to_string(),
            name: resolved_name,
            model_hint: resolved_hint,
            active: true,
            killed: false,
            added_step: step_idx,
            last_active_ms: valid_time_ms.unwrap_or(0).max(hint_ms).max(prev_ms),
            added_turn,
            last_active_turn: cur_turn,
        },
    );
}

fn prune_child_agents(child_agents: &mut BTreeMap<String, ChildAgentRecord>) {
    if child_agents.len() <= MAX_CHILD_AGENTS {
        return;
    }
    let mut entries: Vec<ChildAgentRecord> = child_agents.values().cloned().collect();
    entries.sort_by(|a, b| {
        let a_alive = u8::from(a.active && !a.killed);
        let b_alive = u8::from(b.active && !b.killed);
        b_alive
            .cmp(&a_alive)
            .then_with(|| b.added_step.cmp(&a.added_step))
            .then_with(|| b.last_active_ms.cmp(&a.last_active_ms))
    });
    let keep: HashSet<String> = entries
        .into_iter()
        .take(MAX_CHILD_AGENTS)
        .map(|e| e.id)
        .collect();
    child_agents.retain(|id, _| keep.contains(id));
}

fn unquote_json_string_once(raw: &str) -> Cow<'_, str> {
    let trimmed = raw.trim();
    if trimmed.len() >= 2
        && trimmed.starts_with('"')
        && trimmed.ends_with('"')
        && let Ok(unquoted) = serde_json::from_str::<String>(trimmed)
    {
        return Cow::Owned(unquoted);
    }
    Cow::Borrowed(trimmed)
}

fn extract_tool_arg_str(val: Option<&Value>) -> Option<String> {
    let raw = val.and_then(Value::as_str)?.trim();
    if raw.is_empty() {
        return None;
    }
    let unquoted = unquote_json_string_once(raw);
    let trimmed = unquoted.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn extract_tool_arg_str_list(val: Option<&Value>) -> Vec<String> {
    let mut out = Vec::new();
    match val {
        Some(Value::Array(arr)) => {
            for item in arr {
                if let Some(s) = extract_tool_arg_str(Some(item)) {
                    out.push(s);
                }
            }
        }
        Some(Value::String(raw)) => {
            let unquoted = unquote_json_string_once(raw);
            let candidate = unquoted.trim();
            if candidate.starts_with('[')
                && let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(candidate)
            {
                for item in &arr {
                    if let Some(s) = extract_tool_arg_str(Some(item)) {
                        out.push(s);
                    }
                }
                return out;
            }
            if let Some(s) = extract_tool_arg_str(val) {
                out.push(s);
            }
        }
        _ => {}
    }
    out
}

fn process_planner_tool_calls(
    tool_calls: Option<&Value>,
    acc: &mut TranscriptAccState,
    step_idx: u64,
    valid_time_ms: Option<i64>,
) {
    let Some(Value::Array(calls)) = tool_calls else {
        return;
    };
    let cur_turn = effective_turn(acc);

    for tc in calls {
        let Some(tc_obj) = tc.as_object() else {
            continue;
        };
        let tool_name = tc_obj
            .get("name")
            .or_else(|| tc_obj.get("function").and_then(|f| f.get("name")))
            .and_then(Value::as_str)
            .unwrap_or("");

        let raw_args = tc_obj
            .get("arguments")
            .or_else(|| tc_obj.get("args"))
            .or_else(|| tc_obj.get("function").and_then(|f| f.get("arguments")));
        let parsed_args: Value = match raw_args {
            Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
            Some(v) => v.clone(),
            None => Value::Null,
        };
        let args = parsed_args.as_object();

        match tool_name {
            "invoke_subagent" => {
                let raw_model = args
                    .and_then(|a| extract_tool_arg_str(a.get("Model").or_else(|| a.get("model"))))
                    .unwrap_or_default();
                let model = if raw_model.eq_ignore_ascii_case("inherit") {
                    String::new()
                } else {
                    raw_model
                };
                let role = args
                    .and_then(|a| extract_tool_arg_str(a.get("Role").or_else(|| a.get("role"))))
                    .unwrap_or_default();
                let type_name = args
                    .and_then(|a| {
                        extract_tool_arg_str(a.get("TypeName").or_else(|| a.get("typeName")))
                    })
                    .unwrap_or_default();
                acc.pending_subagent_hints.push(SubagentHint {
                    model,
                    role,
                    type_name,
                    step_index: step_idx,
                    time_ms: valid_time_ms.unwrap_or(0),
                });
                if acc.pending_subagent_hints.len() > 32 {
                    acc.pending_subagent_hints.remove(0);
                }
            }
            "send_message" => {
                if let Some(recipient) = args.and_then(|a| {
                    extract_tool_arg_str(
                        a.get("Recipient")
                            .or_else(|| a.get("recipient"))
                            .or_else(|| a.get("conversationId"))
                            .or_else(|| a.get("conversation_id")),
                    )
                }) && let Some(child) = acc.child_agents.get_mut(&recipient)
                {
                    child.active = true;
                    child.killed = false;
                    child.added_step = step_idx;
                    child.last_active_turn = cur_turn;
                    if let Some(ts) = valid_time_ms {
                        child.last_active_ms = child.last_active_ms.max(ts);
                    }
                }
            }
            "manage_subagents" => {
                let action = args
                    .and_then(|a| extract_tool_arg_str(a.get("Action").or_else(|| a.get("action"))))
                    .unwrap_or_default();
                if action == "kill_all" {
                    for child in acc.child_agents.values_mut() {
                        if child.kind == "subagent" {
                            child.active = false;
                            child.killed = true;
                        }
                    }
                } else if action == "kill" {
                    let raw_ids = args.and_then(|a| {
                        a.get("ConversationIds")
                            .or_else(|| a.get("conversationIds"))
                            .or_else(|| a.get("ConversationId"))
                            .or_else(|| a.get("conversationId"))
                    });
                    for target_id in extract_tool_arg_str_list(raw_ids) {
                        if let Some(child) = acc.child_agents.get_mut(&target_id)
                            && child.kind == "subagent"
                        {
                            child.active = false;
                            child.killed = true;
                        }
                    }
                }
            }
            "post_message" => {
                if let Some(msg_text) = args
                    .and_then(|a| extract_tool_arg_str(a.get("text").or_else(|| a.get("Text"))))
                {
                    let mention_all = has_mention(&msg_text, "all");
                    for child in acc.child_agents.values_mut() {
                        if child.kind == "swarm"
                            && !child.killed
                            && (mention_all || has_mention(&msg_text, &child.name))
                        {
                            child.active = true;
                            child.last_active_turn = cur_turn;
                            if let Some(ts) = valid_time_ms {
                                child.last_active_ms = child.last_active_ms.max(ts);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn process_generic_child_events(
    entry: &Map<String, Value>,
    acc: &mut TranscriptAccState,
    main_conv_id: Option<&str>,
    step_idx: u64,
    valid_time_ms: Option<i64>,
) {
    let mut objects: Vec<Map<String, Value>> = Vec::new();
    let mut content_str = "";

    match entry.get("content") {
        Some(Value::Object(map)) => objects.push(map.clone()),
        Some(Value::String(s)) => {
            content_str = s.as_str();
            let has_signal = content_str.contains("conversation_id")
                || content_str.contains("conversationId")
                || content_str.contains("model_switched")
                || content_str.contains("killed");
            if !has_signal {
                return;
            }
            objects = extract_json_objects(content_str, 16);
        }
        _ => return,
    }

    let mut seen_sub_ids: HashSet<String> = HashSet::new();

    for obj in &objects {
        let status = obj.get("status").and_then(Value::as_str).unwrap_or("");
        if status == "added" {
            let top_model = obj
                .get("model_display_name")
                .or_else(|| obj.get("model"))
                .and_then(Value::as_str);
            if let Some(cid) = obj.get("conversation_id").and_then(Value::as_str) {
                let trimmed = cid.trim();
                if !trimmed.is_empty() {
                    register_swarm_member(
                        acc,
                        trimmed,
                        obj.get("name").and_then(Value::as_str),
                        top_model,
                        step_idx,
                        valid_time_ms,
                        main_conv_id,
                    );
                }
            }
            if let Some(Value::Array(members)) = obj.get("members") {
                for member in members {
                    let Some(m_obj) = member.as_object() else {
                        continue;
                    };
                    if let Some(cid) = m_obj.get("conversation_id").and_then(Value::as_str) {
                        let trimmed = cid.trim();
                        if !trimmed.is_empty() {
                            let m_model = m_obj
                                .get("model_display_name")
                                .or_else(|| m_obj.get("model"))
                                .and_then(Value::as_str)
                                .or(top_model);
                            register_swarm_member(
                                acc,
                                trimmed,
                                m_obj.get("name").and_then(Value::as_str),
                                m_model,
                                step_idx,
                                valid_time_ms,
                                main_conv_id,
                            );
                        }
                    }
                }
            }
            continue;
        }

        let next_model = obj
            .get("model_display_name")
            .or_else(|| obj.get("model"))
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        if (status == "model_switched" || (status.contains("switch") && !next_model.is_empty()))
            && !next_model.is_empty()
        {
            let target_name = obj.get("name").and_then(Value::as_str);
            let target_id = obj.get("conversation_id").and_then(Value::as_str);
            for child in acc.child_agents.values_mut() {
                if child.kind == "swarm"
                    && (target_name == Some(child.name.as_str())
                        || target_id == Some(child.id.as_str()))
                {
                    child.model_hint = next_model.to_string();
                }
            }
            continue;
        }

        if status == "killed" {
            let mut targets: Vec<String> = Vec::new();
            if let Some(n) = obj.get("name").and_then(Value::as_str)
                && !n.trim().is_empty()
            {
                targets.push(n.trim().to_string());
            }
            if let Some(Value::Array(names)) = obj.get("names") {
                for v in names {
                    if let Some(n) = v.as_str()
                        && !n.trim().is_empty()
                    {
                        targets.push(n.trim().to_string());
                    }
                }
            }
            let target_id = obj
                .get("conversation_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("");
            for child in acc.child_agents.values_mut() {
                if child.kind != "swarm" {
                    continue;
                }
                if (!target_id.is_empty() && child.id == target_id)
                    || targets
                        .iter()
                        .any(|t| child.name == *t || child.name.starts_with(&format!("{t}-")))
                {
                    child.active = false;
                    child.killed = true;
                }
            }
            continue;
        }

        if let Some(sub_id) = obj
            .get("conversationId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            seen_sub_ids.insert(sub_id.to_string());
            register_subagent(
                acc,
                sub_id,
                obj.get("model_display_name")
                    .or_else(|| obj.get("model"))
                    .and_then(Value::as_str),
                obj.get("name")
                    .or_else(|| obj.get("role"))
                    .and_then(Value::as_str),
                step_idx,
                valid_time_ms,
                main_conv_id,
            );
        }
    }

    if content_str.contains("conversationId") {
        static UUID_RE: OnceLock<Option<Regex>> = OnceLock::new();
        if let Some(re) = UUID_RE.get_or_init(|| {
            Regex::new(
                r#"(?i)conversationId["']?\s*[:=]\s*["']?([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})"#,
            )
            .ok()
        }) {
            for cap in re.captures_iter(content_str) {
                if let Some(m) = cap.get(1) {
                    let sub_id = m.as_str();
                    if seen_sub_ids.insert(sub_id.to_string()) {
                        register_subagent(acc, sub_id, None, None, step_idx, valid_time_ms, main_conv_id);
                    }
                }
            }
        }
    }

    prune_child_agents(&mut acc.child_agents);
}

fn deep_find<'a>(val: &'a Value, key: &str, max_depth: u8) -> Option<&'a Value> {
    if max_depth == 0 {
        return None;
    }
    match val {
        Value::Object(map) => {
            if let Some(v) = map.get(key) {
                return Some(v);
            }
            for v in map.values() {
                if let Some(found) = deep_find(v, key, max_depth - 1) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(arr) => {
            for v in arr {
                if let Some(found) = deep_find(v, key, max_depth - 1) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

fn parse_duration_to_ms(duration_str: &str) -> i64 {
    static SEG_RE: OnceLock<Option<Regex>> = OnceLock::new();
    let mut ms = 0.0f64;
    if let Some(re) = SEG_RE.get_or_init(|| Regex::new(r"(?i)(\d+(?:\.\d+)?)\s*(ms|h|m|s)").ok()) {
        let mut matched = false;
        for cap in re.captures_iter(duration_str) {
            matched = true;
            let val: f64 = cap.get(1).and_then(|m| m.as_str().parse().ok()).unwrap_or(0.0);
            if !val.is_finite() {
                continue;
            }
            match cap.get(2).map(|m| m.as_str().to_ascii_lowercase()).as_deref() {
                Some("ms") => ms += val,
                Some("h") => ms += val * 3_600_000.0,
                Some("m") => ms += val * 60_000.0,
                Some("s") => ms += val * 1_000.0,
                _ => {}
            }
        }
        if matched {
            return ms.round() as i64;
        }
    }
    if let Ok(val) = duration_str.trim().parse::<f64>()
        && val.is_finite()
    {
        return (val * 1000.0).round() as i64;
    }
    0
}

fn process_error_rate_limit(
    entry: &Map<String, Value>,
    acc: &mut TranscriptAccState,
    now_ms: i64,
) {
    let is_structured_error = entry.get("status").and_then(Value::as_str) == Some("ERROR")
        || entry.get("error").is_some_and(Value::is_object)
        || entry.get("type").and_then(Value::as_str) == Some("ERROR");
    if !is_structured_error {
        return;
    }

    let entry_val = Value::Object(entry.clone());
    let target_content = entry.get("content").and_then(Value::as_str).unwrap_or("");
    let err_obj = entry.get("error").and_then(Value::as_object);
    let is_rate_limit = deep_find(&entry_val, "RESOURCE_EXHAUSTED", 6).is_some()
        || deep_find(&entry_val, "quotaResetDelay", 6).is_some()
        || deep_find(&entry_val, "quotaResetTimeStamp", 6).is_some()
        || err_obj.is_some_and(|e| {
            e.get("code").and_then(Value::as_i64) == Some(429)
                || e.get("status").and_then(Value::as_str) == Some("RESOURCE_EXHAUSTED")
        })
        || target_content.contains("HTTP 429")
        || target_content.contains("RESOURCE_EXHAUSTED");

    if !is_rate_limit {
        return;
    }

    let mut delay_ms = 0i64;
    let mut timestamp_ms = 0i64;

    let delay_str = deep_find(&entry_val, "quotaResetDelay", 6)
        .or_else(|| deep_find(&entry_val, "retryDelay", 6))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            static DELAY_RE: OnceLock<Option<Regex>> = OnceLock::new();
            DELAY_RE
                .get_or_init(|| {
                    Regex::new(r#"(?i)quotaResetDelay["']?\s*[:=]\s*["']?([0-9.hms]+)"#).ok()
                })
                .as_ref()
                .and_then(|re| re.captures(target_content))
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_string())
        });
    if let Some(d) = delay_str.filter(|s| !s.is_empty()) {
        delay_ms = parse_duration_to_ms(&d);
    }

    let ts_str = deep_find(&entry_val, "quotaResetTimeStamp", 6)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            static TS_RE: OnceLock<Option<Regex>> = OnceLock::new();
            TS_RE
                .get_or_init(|| {
                    Regex::new(r#"(?i)quotaResetTimeStamp["']?\s*[:=]\s*["']?([0-9T:\-.+Z]+)"#).ok()
                })
                .as_ref()
                .and_then(|re| re.captures(target_content))
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_string())
        });
    if let Some(ts) = ts_str.filter(|s| !s.is_empty())
        && let Some(parsed) = parse_iso8601_to_epoch_ms(&ts)
    {
        timestamp_ms = parsed;
    }

    let reset_time_ms = if timestamp_ms > 0 {
        timestamp_ms
    } else if delay_ms > 0 {
        let entry_time = parse_entry_time_ms(entry).unwrap_or(now_ms);
        entry_time + delay_ms
    } else {
        0
    };

    if reset_time_ms > now_ms {
        let should_update = acc
            .image_exhausted_reset_ms
            .is_none_or(|prev| reset_time_ms > prev);
        if should_update {
            acc.image_exhausted_reset_ms = Some(reset_time_ms);
        }
    }
}

fn process_transcript_entry(
    mut entry: Map<String, Value>,
    acc: &mut TranscriptAccState,
    main_conv_id: Option<&str>,
    full_entries: Option<&HashMap<u64, Value>>,
    now_ms: i64,
) {
    if let Some(full_map) = full_entries
        && entry
            .get("truncated_fields")
            .and_then(Value::as_array)
            .is_some_and(|arr| !arr.is_empty())
        && let Some(step_idx) = as_u64_val(entry.get("step_index"))
        && let Some(Value::Object(full_obj)) = full_map.get(&step_idx)
    {
        for (k, v) in full_obj {
            entry.insert(k.clone(), v.clone());
        }
    }

    if let Some(step) = as_u64_val(entry.get("step_index"))
        && step > acc.steps
    {
        acc.steps = step;
    }

    let entry_type = entry.get("type").and_then(Value::as_str).unwrap_or("");
    let entry_source = entry.get("source").and_then(Value::as_str).unwrap_or("");

    if entry_type == "CHECKPOINT" {
        acc.compaction_count += 1;
        acc.last_turn_usage = None;
        acc.max_historical_cache = 0;
        if let Some(Value::Object(usage_map)) = acc.usage.as_mut() {
            usage_map.remove("current_usage");
            usage_map.remove("used_percentage");
            usage_map.insert("input_tokens".to_string(), Value::from(0));
            usage_map.insert("cache_read_input_tokens".to_string(), Value::from(0));
            usage_map.insert("cache_creation_input_tokens".to_string(), Value::from(0));
            if usage_map.contains_key("cache_read_tokens") {
                usage_map.insert("cache_read_tokens".to_string(), Value::from(0));
            }
            if usage_map.contains_key("cache_creation_tokens") {
                usage_map.insert("cache_creation_tokens".to_string(), Value::from(0));
            }
        }
    }

    let valid_time_ms = parse_entry_time_ms(&entry);
    let step_idx = as_u64_val(entry.get("step_index")).unwrap_or(acc.steps);

    let is_interrupting_step = matches!(entry_type, "USER_INPUT" | "SYSTEM_MESSAGE" | "CHECKPOINT")
        || entry_source == "SYSTEM";

    if is_interrupting_step
        && let Some(last) = acc.last_planner_response.as_mut()
        && last.end_ms.is_none()
    {
        last.interrupted = true;
    }

    if let Some(pending_start) = acc.pending_planner_start_ms {
        if let Some(ts) = valid_time_ms
            && !is_interrupting_step
            && ts >= pending_start
        {
            acc.total_api_duration_ms += (ts - pending_start) as u64;
            if let Some(last) = acc.last_planner_response.as_mut()
                && last.end_ms.is_none()
            {
                last.end_ms = Some(ts);
            }
            if !acc.last_known_model.is_empty() && ts > 0 {
                let end_model_key = compact_model_name(&acc.last_known_model);
                if !end_model_key.is_empty() {
                    let prev = acc.model_last_active_ms.get(&end_model_key).copied().unwrap_or(0);
                    acc.model_last_active_ms.insert(end_model_key, prev.max(ts));
                }
            }
        }
        acc.pending_planner_start_ms = None;
    }

    if entry_type == "USER_INPUT" {
        acc.user_turns += 1;
        if let Some(ts) = valid_time_ms
            && ts > 0
        {
            acc.last_user_input_ms = acc.last_user_input_ms.max(ts);
        }
    }

    if entry_type == "SYSTEM_MESSAGE" {
        let content_str = entry.get("content").and_then(Value::as_str).unwrap_or("");
        if content_str.contains("All your subagents and background tasks have been stopped") {
            for child in acc.child_agents.values_mut() {
                child.active = false;
            }
        }
        let cur_turn = effective_turn(acc);
        let touch_sender = |agents: &mut BTreeMap<String, ChildAgentRecord>, sender: &str| {
            if let Some(child) = agents.get_mut(sender) {
                child.last_active_turn = cur_turn;
                if let Some(ts) = valid_time_ms {
                    child.last_active_ms = child.last_active_ms.max(ts);
                }
                return;
            }
            for child in agents.values_mut() {
                if !child.killed && child.name == sender {
                    child.last_active_turn = cur_turn;
                    if let Some(ts) = valid_time_ms {
                        child.last_active_ms = child.last_active_ms.max(ts);
                    }
                }
            }
        };
        if let Some(sender) = entry.get("sender").and_then(Value::as_str) {
            touch_sender(&mut acc.child_agents, sender);
        }
        static SENDER_RE: OnceLock<Option<Regex>> = OnceLock::new();
        if !content_str.is_empty()
            && let Some(re) = SENDER_RE.get_or_init(|| Regex::new(r"\bsender=([^\s\]]+)").ok())
            && let Some(caps) = re.captures(content_str)
            && let Some(m) = caps.get(1)
        {
            touch_sender(&mut acc.child_agents, m.as_str());
        }
    }

    if entry_type == "PLANNER_RESPONSE" {
        acc.has_planner_response = true;
        if let Some(ts) = valid_time_ms {
            acc.pending_planner_start_ms = Some(ts);
            let raw_out = as_u64_val(entry.get("output_tokens")).or_else(|| {
                as_u64_val(
                    entry
                        .get("context_window")
                        .and_then(|cw| cw.get("current_usage"))
                        .and_then(|cu| cu.get("output_tokens")),
                )
            });
            acc.last_planner_response = Some(PlannerResponseSpan {
                start_ms: ts,
                end_ms: None,
                output_tokens: raw_out,
                interrupted: false,
            });
        }
        process_planner_tool_calls(entry.get("tool_calls"), acc, step_idx, valid_time_ms);
    }

    if entry_type == "GENERIC" {
        process_generic_child_events(&entry, acc, main_conv_id, step_idx, valid_time_ms);
    }

    let cw_opt: Option<Value> = if let Some(cw) = entry
        .get("context_window")
        .filter(|v| v.as_object().is_some_and(is_context_window_map))
    {
        Some(cw.clone())
    } else if is_context_window_map(&entry) {
        Some(Value::Object(entry.clone()))
    } else {
        find_context_window_in_map(&entry, 0).cloned()
    };
    if let Some(Value::Object(mut cw_map)) = cw_opt.clone() {
        let prev_win_size = acc
            .usage
            .as_ref()
            .and_then(|u| as_u64_val(u.get("context_window_size")));
        if let Some(prev_size) = prev_win_size
            && as_u64_val(cw_map.get("context_window_size")).is_none()
        {
            cw_map.insert("context_window_size".to_string(), Value::from(prev_size));
        }
        let cur_obj = cw_map
            .get("current_usage")
            .and_then(Value::as_object)
            .unwrap_or(&cw_map);
        let cache_read_val = as_u64_val(cur_obj.get("cache_read_input_tokens"))
            .or_else(|| as_u64_val(cur_obj.get("cache_read_tokens")))
            .or_else(|| as_u64_val(cw_map.get("cache_read_input_tokens")))
            .or_else(|| as_u64_val(cw_map.get("cache_read_tokens")))
            .unwrap_or(0);
        if cache_read_val > acc.max_historical_cache {
            acc.max_historical_cache = cache_read_val;
        }
        acc.usage = Some(Value::Object(cw_map));
    }

    let has_top_level_tokens = as_u64_val(entry.get("input_tokens")).is_some()
        || as_u64_val(entry.get("output_tokens")).is_some()
        || as_u64_val(entry.get("cache_read_tokens")).is_some()
        || as_u64_val(entry.get("cache_read_input_tokens")).is_some()
        || as_u64_val(entry.get("cache_creation_tokens")).is_some()
        || as_u64_val(entry.get("cache_creation_input_tokens")).is_some();

    let raw_model: Option<String> = if entry_type == "PLANNER_RESPONSE" {
        entry
            .get("model")
            .or_else(|| entry.get("model_name"))
            .or_else(|| entry.get("model_display_name"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
    } else {
        None
    };

    if let Some(ref rm) = raw_model {
        acc.last_known_model = rm.clone();
        let m_key = compact_model_name(rm);
        if !m_key.is_empty() {
            acc.model_last_seen_turn.insert(m_key, effective_turn(acc));
        }
    }
    if entry_type == "PLANNER_RESPONSE"
        && let Some(ts) = valid_time_ms
        && ts > 0
    {
        let step_model_str = raw_model.as_deref().unwrap_or(&acc.last_known_model);
        let step_model_key = compact_model_name(step_model_str);
        if !step_model_key.is_empty() {
            let prev = acc.model_last_active_ms.get(&step_model_key).copied().unwrap_or(0);
            acc.model_last_active_ms.insert(step_model_key, prev.max(ts));
        }
    }

    let cw_cur = cw_opt
        .as_ref()
        .and_then(|cw| cw.get("current_usage"))
        .and_then(Value::as_object);
    if entry_type == "PLANNER_RESPONSE" && (has_top_level_tokens || cw_cur.is_some()) {
        let inp = as_u64_val(entry.get("input_tokens"))
            .or_else(|| cw_cur.and_then(|c| as_u64_val(c.get("input_tokens"))))
            .unwrap_or(0);
        let out = as_u64_val(entry.get("output_tokens"))
            .or_else(|| cw_cur.and_then(|c| as_u64_val(c.get("output_tokens"))))
            .unwrap_or(0);
        let c_read = as_u64_val(entry.get("cache_read_tokens"))
            .or_else(|| as_u64_val(entry.get("cache_read_input_tokens")))
            .or_else(|| cw_cur.and_then(|c| as_u64_val(c.get("cache_read_input_tokens"))))
            .or_else(|| cw_cur.and_then(|c| as_u64_val(c.get("cache_read_tokens"))))
            .unwrap_or(0);
        let c_create = as_u64_val(entry.get("cache_creation_tokens"))
            .or_else(|| as_u64_val(entry.get("cache_creation_input_tokens")))
            .or_else(|| cw_cur.and_then(|c| as_u64_val(c.get("cache_creation_input_tokens"))))
            .or_else(|| cw_cur.and_then(|c| as_u64_val(c.get("cache_creation_tokens"))))
            .unwrap_or(0);

        acc.total_input_tokens = acc.total_input_tokens.saturating_add(inp);
        acc.total_output_tokens = acc.total_output_tokens.saturating_add(out);
        acc.total_cache_read_tokens = acc.total_cache_read_tokens.saturating_add(c_read);
        acc.total_cache_creation_tokens = acc.total_cache_creation_tokens.saturating_add(c_create);
        if c_read > acc.max_historical_cache {
            acc.max_historical_cache = c_read;
        }

        let inclusive_turn = inp.max(c_create.saturating_add(c_read)).saturating_add(out);
        let exclusive_turn = inp
            .saturating_add(c_create)
            .saturating_add(c_read)
            .saturating_add(out);
        if inclusive_turn > acc.inclusive_peak {
            acc.inclusive_peak = inclusive_turn;
        }
        if exclusive_turn > acc.exclusive_peak {
            acc.exclusive_peak = exclusive_turn;
        }
        if exclusive_turn > acc.peak_raw_turn_tokens {
            acc.peak_raw_turn_tokens = exclusive_turn;
        }

        let step_model = raw_model
            .as_deref()
            .unwrap_or(&acc.last_known_model);
        let model_key = {
            let k = compact_model_name(step_model);
            if k.is_empty() {
                "Unknown".to_string()
            } else {
                k
            }
        };
        let cache_total = c_read.saturating_add(c_create);
        let cache_inclusive = cw_opt.is_some()
            && model_includes_cache_in_input(step_model)
            && cache_total > 0
            && inp >= cache_total;
        let effective_uncached_in = if cache_inclusive {
            inp.saturating_sub(cache_total)
        } else {
            inp
        };

        let usage_for_cost = TokenUsage {
            input_tokens: inp,
            output_tokens: out,
            cache_read_input_tokens: c_read,
            cache_creation_input_tokens: c_create,
            cache_exclusive: cw_opt.is_none(),
        };
        let step_cost_usd =
            calculate_token_cost(step_model, &usage_for_cost, cache_inclusive, false).unwrap_or(0.0);

        acc.transcript_cost_usd += step_cost_usd;
        acc.token_steps_count = acc.token_steps_count.saturating_add(1);
        if acc.recent_token_steps.len() >= MAX_RECENT_TOKEN_STEPS {
            acc.recent_token_steps.remove(0);
        }
        acc.recent_token_steps.push(TranscriptTokenStepCost {
            step_index: as_u64_val(entry.get("step_index")),
            model_key: model_key.clone(),
            input_tokens: effective_uncached_in,
            cache_read_tokens: c_read,
            cache_write_tokens: c_create,
            output_tokens: out,
            cost_usd: step_cost_usd,
        });

        let model_acc = acc.model_totals.entry(model_key).or_default();
        model_acc.input_tokens = model_acc.input_tokens.saturating_add(effective_uncached_in);
        model_acc.cache_read_tokens = model_acc.cache_read_tokens.saturating_add(c_read);
        model_acc.cache_write_tokens = model_acc.cache_write_tokens.saturating_add(c_create);
        model_acc.output_tokens = model_acc.output_tokens.saturating_add(out);
        model_acc.cost_usd += step_cost_usd;
        model_acc.steps = model_acc.steps.saturating_add(1);

        acc.last_turn_usage = Some(CachedTurnUsage {
            input_tokens: inp,
            output_tokens: out,
            cache_read_input_tokens: c_read,
            cache_creation_input_tokens: c_create,
            model: raw_model,
            cache_exclusive: cw_opt.is_none(),
        });

        if cw_opt.is_none() {
            let prev_win_size = acc
                .usage
                .as_ref()
                .and_then(|u| as_u64_val(u.get("context_window_size")));
            let mut synthetic = Map::new();
            synthetic.insert("total_input_tokens".to_string(), Value::from(acc.total_input_tokens));
            synthetic.insert(
                "total_output_tokens".to_string(),
                Value::from(acc.total_output_tokens),
            );
            if let Some(ws) = prev_win_size {
                synthetic.insert("context_window_size".to_string(), Value::from(ws));
            }
            let mut cur_map = Map::new();
            cur_map.insert("input_tokens".to_string(), Value::from(inp));
            cur_map.insert("output_tokens".to_string(), Value::from(out));
            cur_map.insert("cache_read_input_tokens".to_string(), Value::from(c_read));
            cur_map.insert(
                "cache_creation_input_tokens".to_string(),
                Value::from(c_create),
            );
            synthetic.insert("current_usage".to_string(), Value::Object(cur_map));
            acc.usage = Some(Value::Object(synthetic));
        }
    }

    process_error_rate_limit(&entry, acc, now_ms);
}

struct ChildTailTelemetry {
    speed_result: Option<ChildSpeedSample>,
    latest_event_ms: i64,
    latest_model_label: String,
}

struct ChildSpeedSample {
    model_label: String,
    speed: f64,
    event_ms: i64,
    mtime_ms: i64,
}

struct PendingChildPlanner {
    start_ms: i64,
    output_tokens: u64,
    model: String,
    has_tool_calls: bool,
}

fn extract_child_output_tokens(
    entry: &Map<String, Value>,
    prev_total_out: Option<u64>,
    is_from_start: bool,
) -> (u64, Option<u64>) {
    let raw_per_turn = as_u64_val(entry.get("output_tokens"))
        .or_else(|| {
            as_u64_val(
                entry
                    .get("context_window")
                    .and_then(|cw| cw.get("current_usage"))
                    .and_then(|cu| cu.get("output_tokens")),
            )
        })
        .or_else(|| {
            as_u64_val(
                entry
                    .get("usage")
                    .and_then(|u| u.get("current_usage"))
                    .and_then(|cu| cu.get("output_tokens")),
            )
        })
        .or_else(|| as_u64_val(entry.get("usage").and_then(|u| u.get("output_tokens"))));

    if let Some(tok) = raw_per_turn {
        return (tok, prev_total_out);
    }

    let raw_total = as_u64_val(
        entry
            .get("context_window")
            .and_then(|cw| cw.get("total_output_tokens")),
    )
    .or_else(|| as_u64_val(entry.get("usage").and_then(|u| u.get("total_output_tokens"))));

    if let Some(total_out) = raw_total {
        if let Some(prev) = prev_total_out {
            if total_out >= prev {
                return (total_out - prev, Some(total_out));
            }
            return (0, Some(total_out));
        }
        if is_from_start {
            return (total_out, Some(total_out));
        }
        return (0, Some(total_out));
    }
    (0, prev_total_out)
}

fn read_child_tail_telemetry(
    child_transcript_path: &Path,
    child: &ChildAgentRecord,
    now_ms: i64,
) -> Option<ChildTailTelemetry> {
    let meta = fs::metadata(child_transcript_path).ok()?;
    if !meta.is_file() || meta.len() == 0 {
        return None;
    }
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let file_size = meta.len();
    let to_read = file_size.min(MAX_CHILD_TAIL_BYTES);
    let offset = file_size - to_read;
    let mut file = File::open(child_transcript_path).ok()?;
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = vec![0u8; to_read as usize];
    let mut total_read = 0usize;
    while total_read < buf.len() {
        match file.read(&mut buf[total_read..]) {
            Ok(0) => break,
            Ok(n) => total_read += n,
            Err(_) => return None,
        }
    }
    let chunk = String::from_utf8_lossy(&buf[..total_read]);
    let mut lines: Vec<&str> = chunk.split('\n').collect();
    if offset > 0 && !lines.is_empty() {
        lines.remove(0);
    }

    let mut latest_model = String::new();
    let mut latest_planner_ms: i64 = 0;
    let mut latest_planner_model = String::new();
    let mut pending_planner: Option<PendingChildPlanner> = None;
    let mut last_closed_speed: Option<(f64, i64, String)> = None;
    let mut prev_total_out: Option<u64> = None;

    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(Value::Object(entry)) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        let valid_time_ms = parse_entry_time_ms(&entry);
        let entry_type = entry.get("type").and_then(Value::as_str).unwrap_or("");
        let entry_source = entry.get("source").and_then(Value::as_str).unwrap_or("");

        if let Some(pending) = pending_planner.take() {
            let is_interrupting = matches!(entry_type, "USER_INPUT" | "SYSTEM_MESSAGE")
                || entry_source == "SYSTEM";
            if !is_interrupting
                && let Some(ts) = valid_time_ms
                && ts >= pending.start_ms
            {
                let duration_ms = ts - pending.start_ms;
                if duration_ms >= MIN_CHILD_API_DELTA_MS && pending.output_tokens > 0 {
                    let candidate =
                        pending.output_tokens as f64 / (duration_ms as f64 / 1000.0);
                    if is_valid_speed(candidate) {
                        let model = if !pending.model.is_empty() {
                            pending.model
                        } else {
                            latest_model.clone()
                        };
                        last_closed_speed = Some((candidate, ts, model));
                    }
                }
            }
        }

        if entry_type == "PLANNER_RESPONSE" {
            let raw_model = entry
                .get("model")
                .or_else(|| entry.get("model_name"))
                .or_else(|| entry.get("model_display_name"))
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("");
            if !raw_model.is_empty() {
                latest_model = raw_model.to_string();
            }
            if let Some(ts) = valid_time_ms
                && ts > 0
                && ts >= latest_planner_ms
            {
                latest_planner_ms = ts;
                latest_planner_model = if !raw_model.is_empty() {
                    raw_model.to_string()
                } else if !latest_model.is_empty() {
                    latest_model.clone()
                } else {
                    child.model_hint.clone()
                };
            }
            let (tokens, next_total_out) =
                extract_child_output_tokens(&entry, prev_total_out, offset == 0);
            prev_total_out = next_total_out;
            let has_tool_calls = entry
                .get("tool_calls")
                .and_then(Value::as_array)
                .is_some_and(|a| !a.is_empty());
            if let Some(ts) = valid_time_ms {
                let m = if !raw_model.is_empty() {
                    raw_model.to_string()
                } else {
                    latest_model.clone()
                };
                pending_planner = Some(PendingChildPlanner {
                    start_ms: ts,
                    output_tokens: tokens,
                    model: m,
                    has_tool_calls,
                });
            }
        }
    }

    let mut selected_speed = last_closed_speed;
    if let Some(pending) = pending_planner
        && !pending.has_tool_calls
        && pending.output_tokens > 0
        && now_ms >= pending.start_ms
    {
        let trailing_ms = now_ms - pending.start_ms;
        if (MIN_CHILD_API_DELTA_MS..=MAX_CHILD_OPEN_TURN_MS).contains(&trailing_ms) {
            let candidate = pending.output_tokens as f64 / (trailing_ms as f64 / 1000.0);
            if is_valid_speed(candidate) {
                let model = if !pending.model.is_empty() {
                    pending.model
                } else {
                    latest_model.clone()
                };
                selected_speed = Some((candidate, now_ms, model));
            }
        }
    }

    let speed_result = selected_speed.and_then(|(speed, event_ms, model)| {
        if !is_valid_speed(speed) {
            return None;
        }
        let effective_model = if !model.is_empty() {
            model.as_str()
        } else if !latest_model.is_empty() {
            latest_model.as_str()
        } else {
            child.model_hint.as_str()
        };
        let label = compact_model_name(effective_model);
        let model_label = if label.is_empty() {
            "Sub".to_string()
        } else {
            label
        };
        Some(ChildSpeedSample {
            model_label,
            speed,
            event_ms,
            mtime_ms,
        })
    });

    let speed_event_ms = speed_result.as_ref().map(|s| s.event_ms).unwrap_or(0);
    let latest_event_ms = latest_planner_ms.max(speed_event_ms);
    let raw_latest_model = if latest_planner_ms >= speed_event_ms && !latest_planner_model.is_empty()
    {
        latest_planner_model.as_str()
    } else if !latest_model.is_empty() {
        latest_model.as_str()
    } else {
        child.model_hint.as_str()
    };
    let latest_model_label = {
        let c = compact_model_name(raw_latest_model);
        if !c.is_empty() {
            c
        } else {
            speed_result
                .as_ref()
                .map(|s| s.model_label.clone())
                .unwrap_or_default()
        }
    };

    Some(ChildTailTelemetry {
        speed_result,
        latest_event_ms,
        latest_model_label,
    })
}

#[derive(Debug, Clone)]
struct MergedChildCandidate {
    model: String,
    speed: f64,
    event_ms: i64,
    mtime_ms: i64,
    first_order: u64,
    last_active_turn: u64,
    last_active_ms: i64,
}

#[allow(clippy::too_many_arguments)]
fn merge_child_candidate(
    map: &mut HashMap<String, MergedChildCandidate>,
    model_label: &str,
    speed: f64,
    event_ms: i64,
    mtime_ms: i64,
    added_step: u64,
    last_active_turn: u64,
    last_active_ms: i64,
) {
    match map.get_mut(model_label) {
        None => {
            map.insert(
                model_label.to_string(),
                MergedChildCandidate {
                    model: model_label.to_string(),
                    speed,
                    event_ms,
                    mtime_ms,
                    first_order: added_step,
                    last_active_turn,
                    last_active_ms,
                },
            );
        }
        Some(existing) => {
            let is_newer =
                event_ms > existing.event_ms || (event_ms == existing.event_ms && mtime_ms > existing.mtime_ms);
            if is_newer {
                existing.speed = speed;
                existing.event_ms = event_ms;
                existing.mtime_ms = mtime_ms;
            }
            if added_step < existing.first_order {
                existing.first_order = added_step;
            }
            if last_active_turn > existing.last_active_turn {
                existing.last_active_turn = last_active_turn;
            }
            if last_active_ms > existing.last_active_ms {
                existing.last_active_ms = last_active_ms;
            }
        }
    }
}

struct ResolvedChildSpeeds {
    speeds: Vec<ChildModelSpeed>,
    activities: Vec<ChildActivitySummary>,
    tail_activities: Vec<(String, i64)>,
    persisted_changed: bool,
}

fn resolve_child_model_speeds(
    transcript_path: &Path,
    acc: &mut TranscriptAccState,
    now_ms: i64,
    deadline: Option<Instant>,
) -> ResolvedChildSpeeds {
    if acc.child_agents.is_empty() && acc.persisted_child_speeds.is_empty() {
        return ResolvedChildSpeeds {
            speeds: Vec::new(),
            activities: Vec::new(),
            tail_activities: Vec::new(),
            persisted_changed: false,
        };
    }
    let (brain_dir, _) = resolve_brain_context(transcript_path);
    let current_turns = acc
        .user_turns
        .max(u64::from(acc.has_planner_response));

    let mut registered: Vec<ChildAgentRecord> = acc.child_agents.values().cloned().collect();
    registered.sort_by(|a, b| {
        let a_active = u8::from(a.active && !a.killed);
        let b_active = u8::from(b.active && !b.killed);
        b_active
            .cmp(&a_active)
            .then_with(|| b.added_step.cmp(&a.added_step))
            .then_with(|| b.last_active_ms.cmp(&a.last_active_ms))
    });
    registered.truncate(MAX_CHILD_AGENTS);

    let mut live_by_model: HashMap<String, MergedChildCandidate> = HashMap::new();
    let mut unresolved: Vec<(ChildAgentRecord, i64, u64)> = Vec::new();
    let mut model_last_active_ms = acc.model_last_active_ms.clone();
    let mut child_activities: Vec<ChildActivitySummary> = Vec::with_capacity(registered.len());

    for (child_idx, child) in registered.iter().enumerate() {
        let tail = if child_idx < MAX_CHILD_TAIL_READS
            && !deadline.is_some_and(|d| Instant::now() >= d)
        {
            brain_dir.as_ref().and_then(|bd| {
                let cp = bd
                    .join(&child.id)
                    .join(".system_generated")
                    .join("logs")
                    .join("transcript.jsonl");
                read_child_tail_telemetry(&cp, child, now_ms)
            })
        } else {
            None
        };

        let tail_latest_ms = tail.as_ref().map(|t| t.latest_event_ms).unwrap_or(0);
        let child_last_active_ms = child.last_active_ms.max(tail_latest_ms);
        let base_turn = child.last_active_turn.max(child.added_turn);
        let child_last_active_turn = if acc.last_user_input_ms > 0
            && tail_latest_ms >= acc.last_user_input_ms
        {
            current_turns
        } else {
            base_turn
        };

        let resolved_model_hint = tail
            .as_ref()
            .filter(|t| !t.latest_model_label.is_empty())
            .map(|t| t.latest_model_label.clone())
            .or_else(|| {
                tail.as_ref()
                    .and_then(|t| t.speed_result.as_ref())
                    .map(|s| s.model_label.clone())
            })
            .unwrap_or_else(|| child.model_hint.clone());
        child_activities.push(ChildActivitySummary {
            model_hint: resolved_model_hint,
            last_active_turn: child_last_active_turn,
            last_active_ms: child_last_active_ms,
        });

        if let Some(ref t) = tail
            && t.latest_event_ms > 0
            && !t.latest_model_label.is_empty()
        {
            let prev = model_last_active_ms
                .get(&t.latest_model_label)
                .copied()
                .unwrap_or(0);
            model_last_active_ms.insert(t.latest_model_label.clone(), prev.max(child_last_active_ms));
        } else if !child.model_hint.is_empty() && child.last_active_ms > 0 {
            let hint_key = compact_model_name(&child.model_hint);
            if !hint_key.is_empty() {
                let prev = model_last_active_ms.get(&hint_key).copied().unwrap_or(0);
                model_last_active_ms.insert(hint_key, prev.max(child.last_active_ms));
            }
        }

        if let Some(sample) = tail.and_then(|t| t.speed_result) {
            let cand_last_active_ms = child_last_active_ms
                .max(sample.event_ms)
                .max(model_last_active_ms.get(&sample.model_label).copied().unwrap_or(0));
            merge_child_candidate(
                &mut live_by_model,
                &sample.model_label,
                sample.speed,
                sample.event_ms,
                sample.mtime_ms,
                child.added_step,
                child_last_active_turn,
                cand_last_active_ms,
            );
        } else {
            unresolved.push((child.clone(), child_last_active_ms, child_last_active_turn));
        }
    }

    let mut by_model: HashMap<String, MergedChildCandidate> = live_by_model.clone();

    for (child, child_last_active_ms, child_last_active_turn) in unresolved {
        let hint = child.model_hint.trim();
        if hint.is_empty() {
            continue;
        }
        let model_label = compact_model_name(hint);
        if model_label.is_empty() || live_by_model.contains_key(&model_label) {
            continue;
        }
        if let Some(persisted) = acc.persisted_child_speeds.get(&model_label)
            && is_valid_speed(persisted.speed)
        {
            let eff_last_ms = persisted
                .last_active_ms
                .unwrap_or(0)
                .max(persisted.event_ms)
                .max(persisted.updated_at)
                .max(child_last_active_ms)
                .max(child.last_active_ms)
                .max(model_last_active_ms.get(&model_label).copied().unwrap_or(0));
            let ts = if eff_last_ms > 0 { eff_last_ms } else { now_ms };
            let mtime_ms = if persisted.mtime_ms > 0 {
                persisted.mtime_ms
            } else {
                ts
            };
            merge_child_candidate(
                &mut by_model,
                &model_label,
                persisted.speed,
                ts,
                mtime_ms,
                child.added_step,
                child_last_active_turn,
                eff_last_ms,
            );
        }
    }

    for entry in acc.persisted_child_speeds.values() {
        let entry_turn = entry.last_active_turn.unwrap_or(current_turns);
        let entry_last_ms = entry
            .last_active_ms
            .unwrap_or(0)
            .max(entry.event_ms)
            .max(entry.updated_at)
            .max(model_last_active_ms.get(&entry.model).copied().unwrap_or(0));
        if let Some(existing) = by_model.get_mut(&entry.model) {
            if entry_turn > existing.last_active_turn {
                existing.last_active_turn = entry_turn;
            }
            if entry_last_ms > existing.last_active_ms {
                existing.last_active_ms = entry_last_ms;
            }
            continue;
        }
        if !is_valid_speed(entry.speed) {
            continue;
        }
        by_model.insert(
            entry.model.clone(),
            MergedChildCandidate {
                model: entry.model.clone(),
                speed: entry.speed,
                event_ms: if entry.event_ms > 0 { entry.event_ms } else { entry.updated_at },
                mtime_ms: if entry.mtime_ms > 0 { entry.mtime_ms } else { entry.updated_at },
                first_order: entry.first_order,
                last_active_turn: entry_turn,
                last_active_ms: entry_last_ms,
            },
        );
    }

    let mut persisted_changed = false;
    for (model_label, live_entry) in &live_by_model {
        let updated_at = if live_entry.event_ms > 0 {
            live_entry.event_ms
        } else if live_entry.mtime_ms > 0 {
            live_entry.mtime_ms
        } else {
            now_ms
        };
        let eff_last_ms = live_entry
            .last_active_ms
            .max(live_entry.event_ms)
            .max(model_last_active_ms.get(model_label).copied().unwrap_or(0));
        let next_entry = PersistedChildSpeed {
            model: model_label.clone(),
            speed: live_entry.speed,
            updated_at,
            event_ms: live_entry.event_ms,
            mtime_ms: live_entry.mtime_ms,
            first_order: live_entry.first_order,
            last_active_turn: Some(live_entry.last_active_turn),
            last_active_ms: Some(eff_last_ms),
        };
        let needs_update = match acc.persisted_child_speeds.get(model_label) {
            None => true,
            Some(prev) => {
                (prev.speed - next_entry.speed).abs() > 1e-9
                    || prev.event_ms != next_entry.event_ms
                    || prev.mtime_ms != next_entry.mtime_ms
                    || prev.first_order != next_entry.first_order
                    || prev.last_active_turn != next_entry.last_active_turn
                    || prev.last_active_ms != next_entry.last_active_ms
            }
        };
        if needs_update {
            acc.persisted_child_speeds
                .insert(model_label.clone(), next_entry);
            persisted_changed = true;
        }
    }

    if acc.persisted_child_speeds.len() > MAX_PERSISTED_CHILD_SPEEDS {
        let mut by_recency: Vec<(String, i64)> = acc
            .persisted_child_speeds
            .iter()
            .map(|(k, v)| {
                let ts = v
                    .last_active_ms
                    .unwrap_or(0)
                    .max(v.event_ms)
                    .max(v.updated_at);
                (k.clone(), ts)
            })
            .collect();
        by_recency.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let keep: HashSet<String> = by_recency
            .into_iter()
            .take(MAX_PERSISTED_CHILD_SPEEDS)
            .map(|(k, _)| k)
            .collect();
        acc.persisted_child_speeds.retain(|k, _| keep.contains(k));
        persisted_changed = true;
    }

    let mut active_candidates: Vec<(MergedChildCandidate, u64, i64)> = Vec::new();
    for item in by_model.into_values() {
        if !is_valid_speed(item.speed) {
            continue;
        }
        let turns_ago = current_turns.saturating_sub(item.last_active_turn);
        let last_active_ms = item
            .last_active_ms
            .max(model_last_active_ms.get(&item.model).copied().unwrap_or(0));
        active_candidates.push((item, turns_ago, last_active_ms));
    }

    let tail_activities: Vec<(String, i64)> = model_last_active_ms.into_iter().collect();

    if active_candidates.is_empty() {
        return ResolvedChildSpeeds {
            speeds: Vec::new(),
            activities: child_activities,
            tail_activities,
            persisted_changed,
        };
    }

    active_candidates.sort_by(|(a, _, _), (b, _, _)| {
        b.event_ms
            .cmp(&a.event_ms)
            .then_with(|| b.mtime_ms.cmp(&a.mtime_ms))
            .then_with(|| a.first_order.cmp(&b.first_order))
            .then_with(|| a.model.cmp(&b.model))
    });

    let result = active_candidates
        .into_iter()
        .map(|(item, turns_ago, last_active_ms)| ChildModelSpeed {
            model: item.model,
            speed_tok_s: item.speed,
            ttft_sec: None,
            turns_ago,
            last_active_ms,
        })
        .collect();
    ResolvedChildSpeeds {
        speeds: result,
        activities: child_activities,
        tail_activities,
        persisted_changed,
    }
}

#[inline]
fn find_byte(needle: u8, haystack: &[u8]) -> Option<usize> {
    #[cfg(unix)]
    {
        if haystack.is_empty() {
            return None;
        }
        // SAFETY: `haystack.as_ptr()` is valid for `haystack.len()` bytes.
        let ptr = unsafe {
            libc::memchr(
                haystack.as_ptr().cast::<libc::c_void>(),
                needle as libc::c_int,
                haystack.len(),
            )
        };
        if ptr.is_null() {
            None
        } else {
            Some((ptr as usize) - (haystack.as_ptr() as usize))
        }
    }
    #[cfg(not(unix))]
    {
        haystack.iter().position(|&b| b == needle)
    }
}

#[inline]
fn contains_truncated_fields_key_bytes(haystack: &[u8]) -> bool {
    const NEEDLE: &[u8] = b"\"truncated_fields\"";
    let mut p = 0usize;
    while let Some(rel) = find_byte(b'f', &haystack[p..]) {
        let idx = p + rel;
        if idx >= 11 && haystack[idx - 11..].starts_with(NEEDLE) {
            return true;
        }
        p = idx + 1;
    }
    false
}

const SIGNAL_MIN_LEN: usize = 5;
const TRANSCRIPT_SIGNALS_LEN5: &[&[u8]] = &[
    b"ERROR",
    b"error",
    b"SYSTEM",
    b"content",
    b"CHECKPOINT",
    b"USER_INPUT",
    b"current_usage",
    b"context_window",
    b"used_percentage",
    b"PLANNER_RESPONSE",
    b"truncated_fields",
    b"total_input_tokens",
    b"total_output_tokens",
];

const fn build_signal_shift_table() -> [u8; 256] {
    let mut table = [SIGNAL_MIN_LEN as u8; 256];
    let mut s_idx = 0usize;
    while s_idx < TRANSCRIPT_SIGNALS_LEN5.len() {
        let sig = TRANSCRIPT_SIGNALS_LEN5[s_idx];
        let mut pos = 0usize;
        while pos < SIGNAL_MIN_LEN {
            let dist = (SIGNAL_MIN_LEN - 1 - pos) as u8;
            let b = sig[pos] as usize;
            if dist < table[b] {
                table[b] = dist;
            }
            pos += 1;
        }
        s_idx += 1;
    }
    table
}

static SIGNAL_SHIFT_TABLE: [u8; 256] = build_signal_shift_table();

#[inline]
fn matches_signal_len5_at(tail: &[u8], b4: u8) -> bool {
    match b4 {
        b'R' => tail.starts_with(b"ERROR"),
        b'r' => tail.starts_with(b"error"),
        b'E' => tail.starts_with(b"SYSTEM"),
        b'e' => {
            tail.starts_with(b"content")
                || tail.starts_with(b"context_window")
                || tail.starts_with(b"current_usage")
        }
        b'K' => tail.starts_with(b"CHECKPOINT"),
        b'_' => tail.starts_with(b"USER_INPUT") || tail.starts_with(b"used_percentage"),
        b'N' => tail.starts_with(b"PLANNER_RESPONSE"),
        b'c' => tail.starts_with(b"truncated_fields"),
        b'l' => {
            tail.starts_with(b"total_input_tokens") || tail.starts_with(b"total_output_tokens")
        }
        _ => false,
    }
}

fn find_first_transcript_signal_bytes(haystack: &[u8]) -> Option<usize> {
    let mut first_u = None;
    let mut p = 0usize;
    while let Some(rel) = find_byte(b'\\', &haystack[p..]) {
        let idx = p + rel;
        if haystack.get(idx + 1) == Some(&b'u') {
            first_u = Some(idx);
            break;
        }
        p = idx + 1;
    }

    let search_end = first_u.unwrap_or(haystack.len());
    if search_end >= SIGNAL_MIN_LEN {
        let end_limit = search_end - SIGNAL_MIN_LEN;
        let mut i = 0usize;
        while i <= end_limit {
            let b4 = haystack[i + 4];
            let sh = SIGNAL_SHIFT_TABLE[b4 as usize] as usize;
            if sh == 0 {
                if matches_signal_len5_at(&haystack[i..], b4) {
                    return Some(i);
                }
                i += 1;
            } else {
                i += sh;
            }
        }
    }

    first_u
}

#[inline]
fn is_json_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n')
}

#[inline]
fn parse_ascii_u64(digits: &[u8]) -> Option<u64> {
    if digits.is_empty() {
        return None;
    }
    let mut val = 0u64;
    for &d in digits {
        val = val.checked_mul(10)?.checked_add((d - b'0') as u64)?;
    }
    Some(val)
}

fn parse_transcript_json_object(line_bytes: &[u8]) -> Option<Map<String, Value>> {
    let trimmed = line_bytes.trim_ascii();
    if trimmed.is_empty() {
        return None;
    }
    match serde_json::from_slice::<Value>(trimmed) {
        Ok(Value::Object(obj)) => Some(obj),
        Ok(_) => None,
        Err(_) => {
            let lossy = String::from_utf8_lossy(line_bytes);
            let trimmed_str = lossy.trim();
            if trimmed_str.is_empty() {
                return None;
            }
            match serde_json::from_str::<Value>(trimmed_str) {
                Ok(Value::Object(obj)) => Some(obj),
                _ => None,
            }
        }
    }
}

fn collect_truncated_step_bytes(line_bytes: &[u8], needed_full_steps: &mut HashSet<u64>) {
    if !contains_truncated_fields_key_bytes(line_bytes) {
        return;
    }
    if let Some(obj) = parse_transcript_json_object(line_bytes)
        && obj
            .get("truncated_fields")
            .and_then(Value::as_array)
            .is_some_and(|arr| !arr.is_empty())
        && let Some(step) = as_u64_val(obj.get("step_index"))
    {
        needed_full_steps.insert(step);
    }
}

fn can_fast_skip_transcript_line_bytes(trimmed: &[u8]) -> bool {
    trimmed.first() == Some(&b'{')
        && trimmed.last() == Some(&b'}')
        && find_first_transcript_signal_bytes(trimmed).is_none()
}

#[cfg(test)]
fn can_fast_skip_transcript_line(trimmed: &str) -> bool {
    can_fast_skip_transcript_line_bytes(trimmed.as_bytes())
}

fn fast_extract_step_index_bytes(bytes: &[u8]) -> Option<u64> {
    const NEEDLE: &[u8] = b"\"step_index\"";

    // O(1) fast path: most lines start directly with `{"step_index":<uint>,...`.
    if bytes.first() == Some(&b'{') {
        let mut j = 1usize;
        while j < bytes.len() && is_json_ws(bytes[j]) {
            j += 1;
        }
        if bytes[j..].starts_with(NEEDLE) {
            j += NEEDLE.len();
            while j < bytes.len() && is_json_ws(bytes[j]) {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b':' {
                j += 1;
                while j < bytes.len() && is_json_ws(bytes[j]) {
                    j += 1;
                }
                let val_start = j;
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    j += 1;
                }
                if j > val_start
                    && j < bytes.len()
                    && (matches!(bytes[j], b',' | b'}') || is_json_ws(bytes[j]))
                    && let Some(u) = parse_ascii_u64(&bytes[val_start..j])
                {
                    return Some(u);
                }
            }
        }
    }

    let mut has_step_index_key = false;
    let mut p = 0usize;
    while let Some(rel) = find_byte(b'_', &bytes[p..]) {
        let idx = p + rel;
        if idx >= 5 && bytes[idx - 5..].starts_with(NEEDLE) {
            has_step_index_key = true;
            break;
        }
        p = idx + 1;
    }
    if !has_step_index_key {
        return None;
    }

    let mut depth = 0usize;
    let mut in_string = false;
    let mut escape = false;
    let mut i = 0usize;

    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }

        match b {
            b'"' => {
                if depth == 1 && bytes[i..].starts_with(NEEDLE) {
                    let mut j = i + NEEDLE.len();
                    while j < bytes.len() && is_json_ws(bytes[j]) {
                        j += 1;
                    }
                    if j < bytes.len() && bytes[j] == b':' {
                        j += 1;
                        while j < bytes.len() && is_json_ws(bytes[j]) {
                            j += 1;
                        }
                        let val_start = j;
                        while j < bytes.len()
                            && !matches!(bytes[j], b',' | b'}' | b' ' | b'\t' | b'\r' | b'\n')
                        {
                            j += 1;
                        }
                        let val_bytes = &bytes[val_start..j];
                        if val_bytes.is_empty() {
                            return None;
                        }
                        if val_bytes.iter().all(u8::is_ascii_digit)
                            && let Some(u) = parse_ascii_u64(val_bytes)
                        {
                            return Some(u);
                        }
                        if let Ok(val_str) = std::str::from_utf8(val_bytes)
                            && let Ok(f) = val_str.parse::<f64>()
                            && f.is_finite()
                            && f >= 0.0
                        {
                            return Some(f.trunc() as u64);
                        }
                        return None;
                    }
                }
                in_string = true;
            }
            b'{' | b'[' => {
                depth += 1;
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
        i += 1;
    }
    None
}

#[cfg(test)]
fn fast_extract_step_index(line: &str) -> Option<u64> {
    fast_extract_step_index_bytes(line.as_bytes())
}

fn process_clean_zone_bytes(
    clean_bytes: &[u8],
    state: &mut TranscriptAccState,
    pass2_lines: &mut usize,
    deadline: Option<Instant>,
) -> (bool, u64) {
    let mut pos = 0usize;
    while pos < clean_bytes.len() {
        let Some(rel_nl) = find_byte(b'\n', &clean_bytes[pos..]) else {
            break;
        };
        *pass2_lines += 1;
        if (*pass2_lines).is_multiple_of(1024)
            && deadline.is_some_and(|d| Instant::now() >= d)
        {
            return (false, pos as u64);
        }
        let end = pos + rel_nl;
        let trimmed = clean_bytes[pos..end].trim_ascii();
        if trimmed.first() == Some(&b'{')
            && trimmed.last() == Some(&b'}')
            && let Some(step) = fast_extract_step_index_bytes(trimmed)
            && step > state.steps
        {
            state.steps = step;
        }
        pos = end + 1;
    }
    (true, pos as u64)
}

struct TwoPassContext<'a> {
    transcript_path: &'a Path,
    start_offset: u64,
    capped_delta: u64,
    now_ms: i64,
    deadline: Option<Instant>,
}

fn scan_transcript_two_pass(
    file: &mut File,
    ctx: TwoPassContext<'_>,
    state: &mut TranscriptAccState,
    offset: &mut u64,
    cache_dirty: &mut bool,
) {
    let mut buf = vec![0u8; FULL_CHUNK_BYTES];
    let mut carry: Vec<u8> = Vec::new();
    let mut oversized = false;
    let mut bytes_read_total = 0u64;
    let mut consumed_bytes = 0u64;
    let mut pending_len = 0u64;
    let mut needed_full_steps: HashSet<u64> = HashSet::new();
    let mut pass1_timed_out = false;

    while bytes_read_total < ctx.capped_delta {
        if ctx.deadline.is_some_and(|d| Instant::now() >= d) {
            pass1_timed_out = true;
            break;
        }
        let to_read = (ctx.capped_delta - bytes_read_total).min(FULL_CHUNK_BYTES as u64) as usize;
        let Ok(n) = file.read(&mut buf[..to_read]) else {
            break;
        };
        if n == 0 {
            break;
        }
        bytes_read_total += n as u64;
        let slice = &buf[..n];
        let mut start = 0usize;

        if !carry.is_empty() || oversized {
            if let Some(rel_nl) = find_byte(b'\n', slice) {
                let seg_len = (rel_nl + 1) as u64;
                consumed_bytes += pending_len + seg_len;
                pending_len = 0;
                if oversized {
                    oversized = false;
                    carry.clear();
                } else {
                    carry.extend_from_slice(&slice[..rel_nl]);
                    collect_truncated_step_bytes(&carry, &mut needed_full_steps);
                    carry.clear();
                }
                start = rel_nl + 1;
            } else {
                pending_len += slice.len() as u64;
                if !oversized {
                    if carry.len() + slice.len() > MAX_FULL_LINE_BYTES {
                        oversized = true;
                        carry.clear();
                    } else {
                        carry.extend_from_slice(slice);
                    }
                }
                continue;
            }
        }

        let rest = &slice[start..];
        if rest.is_empty() {
            continue;
        }
        if !contains_truncated_fields_key_bytes(rest) {
            if let Some(last_nl) = rest.iter().rposition(|&b| b == b'\n') {
                consumed_bytes += pending_len + (last_nl + 1) as u64;
                pending_len = 0;
                let tail = &rest[last_nl + 1..];
                if !tail.is_empty() {
                    pending_len += tail.len() as u64;
                    if tail.len() > MAX_FULL_LINE_BYTES {
                        oversized = true;
                    } else {
                        carry.extend_from_slice(tail);
                    }
                }
            } else {
                pending_len += rest.len() as u64;
                if rest.len() > MAX_FULL_LINE_BYTES {
                    oversized = true;
                } else {
                    carry.extend_from_slice(rest);
                }
            }
        } else {
            let mut pos = 0usize;
            while pos < rest.len() {
                if let Some(rel_nl) = find_byte(b'\n', &rest[pos..]) {
                    let end = pos + rel_nl;
                    consumed_bytes += pending_len + (end - pos + 1) as u64;
                    pending_len = 0;
                    collect_truncated_step_bytes(&rest[pos..end], &mut needed_full_steps);
                    pos = end + 1;
                } else {
                    let tail = &rest[pos..];
                    pending_len += tail.len() as u64;
                    if tail.len() > MAX_FULL_LINE_BYTES {
                        oversized = true;
                    } else {
                        carry.extend_from_slice(tail);
                    }
                    break;
                }
            }
        }
    }

    let pass1_end_offset = ctx.start_offset + consumed_bytes;
    if pass1_timed_out
        || pass1_end_offset <= ctx.start_offset
        || ctx.deadline.is_some_and(|d| Instant::now() >= d)
    {
        return;
    }

    let full_map = if needed_full_steps.is_empty() {
        None
    } else {
        Some(load_full_entries_map(ctx.transcript_path, &needed_full_steps))
    };

    if ctx.deadline.is_some_and(|d| Instant::now() >= d)
        || file.seek(SeekFrom::Start(ctx.start_offset)).is_err()
    {
        return;
    }

    let (_, main_conv_id) = resolve_brain_context(ctx.transcript_path);
    let pass2_limit = pass1_end_offset - ctx.start_offset;
    let mut pass2_read = 0u64;
    let mut pass2_consumed = 0u64;
    let mut pass2_pending = 0u64;
    let mut pass2_lines = 0usize;
    carry.clear();
    oversized = false;

    'pass2: while pass2_read < pass2_limit {
        if ctx.deadline.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        let to_read = (pass2_limit - pass2_read).min(FULL_CHUNK_BYTES as u64) as usize;
        let Ok(n) = file.read(&mut buf[..to_read]) else {
            break;
        };
        if n == 0 {
            break;
        }
        pass2_read += n as u64;
        let slice = &buf[..n];
        let mut start = 0usize;

        if !carry.is_empty() || oversized {
            if let Some(rel_nl) = find_byte(b'\n', slice) {
                pass2_lines += 1;
                if pass2_lines.is_multiple_of(256)
                    && ctx.deadline.is_some_and(|d| Instant::now() >= d)
                {
                    break 'pass2;
                }
                pass2_consumed += pass2_pending + (rel_nl + 1) as u64;
                pass2_pending = 0;
                if oversized {
                    oversized = false;
                    carry.clear();
                } else {
                    carry.extend_from_slice(&slice[..rel_nl]);
                    let trimmed = carry.trim_ascii();
                    if !trimmed.is_empty() {
                        if state.pending_planner_start_ms.is_none()
                            && can_fast_skip_transcript_line_bytes(trimmed)
                        {
                            if let Some(step) = fast_extract_step_index_bytes(trimmed)
                                && step > state.steps
                            {
                                state.steps = step;
                            }
                        } else if let Some(obj) = parse_transcript_json_object(&carry) {
                            process_transcript_entry(
                                obj,
                                state,
                                main_conv_id.as_deref(),
                                full_map.as_ref(),
                                ctx.now_ms,
                            );
                        }
                    }
                    carry.clear();
                }
                *offset = ctx.start_offset + pass2_consumed;
                *cache_dirty = true;
                start = rel_nl + 1;
            } else {
                pass2_pending += slice.len() as u64;
                if !oversized {
                    if carry.len() + slice.len() > MAX_FULL_LINE_BYTES {
                        oversized = true;
                        carry.clear();
                    } else {
                        carry.extend_from_slice(slice);
                    }
                }
                continue;
            }
        }

        while start < slice.len() {
            if state.pending_planner_start_ms.is_some() {
                if let Some(rel_nl) = find_byte(b'\n', &slice[start..]) {
                    pass2_lines += 1;
                    if pass2_lines.is_multiple_of(256)
                        && ctx.deadline.is_some_and(|d| Instant::now() >= d)
                    {
                        break 'pass2;
                    }
                    let end = start + rel_nl;
                    pass2_consumed += (end - start + 1) as u64;
                    if let Some(obj) = parse_transcript_json_object(&slice[start..end]) {
                        process_transcript_entry(
                            obj,
                            state,
                            main_conv_id.as_deref(),
                            full_map.as_ref(),
                            ctx.now_ms,
                        );
                    }
                    *offset = ctx.start_offset + pass2_consumed;
                    *cache_dirty = true;
                    start = end + 1;
                } else {
                    let tail = &slice[start..];
                    pass2_pending += tail.len() as u64;
                    if tail.len() > MAX_FULL_LINE_BYTES {
                        oversized = true;
                    } else {
                        carry.extend_from_slice(tail);
                    }
                    break;
                }
                continue;
            }

            let rest = &slice[start..];
            let next_sig_pos = find_first_transcript_signal_bytes(rest);
            match next_sig_pos {
                None => {
                    if let Some(last_nl) = rest.iter().rposition(|&b| b == b'\n') {
                        let clean_end = start + last_nl + 1;
                        let (completed, clean_consumed) = process_clean_zone_bytes(
                            &slice[start..clean_end],
                            state,
                            &mut pass2_lines,
                            ctx.deadline,
                        );
                        if clean_consumed > 0 {
                            pass2_consumed += clean_consumed;
                            *offset = ctx.start_offset + pass2_consumed;
                            *cache_dirty = true;
                        }
                        if !completed {
                            break 'pass2;
                        }
                        start = clean_end;
                    }
                    let tail = &slice[start..];
                    if !tail.is_empty() {
                        pass2_pending += tail.len() as u64;
                        if tail.len() > MAX_FULL_LINE_BYTES {
                            oversized = true;
                        } else {
                            carry.extend_from_slice(tail);
                        }
                    }
                    break;
                }
                Some(sig_rel) => {
                    if let Some(last_clean_nl) = rest[..sig_rel].iter().rposition(|&b| b == b'\n') {
                        let clean_end = start + last_clean_nl + 1;
                        let (completed, clean_consumed) = process_clean_zone_bytes(
                            &slice[start..clean_end],
                            state,
                            &mut pass2_lines,
                            ctx.deadline,
                        );
                        if clean_consumed > 0 {
                            pass2_consumed += clean_consumed;
                            *offset = ctx.start_offset + pass2_consumed;
                            *cache_dirty = true;
                        }
                        if !completed {
                            break 'pass2;
                        }
                        start = clean_end;
                    }
                    if let Some(rel_nl) = find_byte(b'\n', &slice[start..]) {
                        pass2_lines += 1;
                        if pass2_lines.is_multiple_of(256)
                            && ctx.deadline.is_some_and(|d| Instant::now() >= d)
                        {
                            break 'pass2;
                        }
                        let end = start + rel_nl;
                        pass2_consumed += (end - start + 1) as u64;
                        if let Some(obj) = parse_transcript_json_object(&slice[start..end]) {
                            process_transcript_entry(
                                obj,
                                state,
                                main_conv_id.as_deref(),
                                full_map.as_ref(),
                                ctx.now_ms,
                            );
                        }
                        *offset = ctx.start_offset + pass2_consumed;
                        *cache_dirty = true;
                        start = end + 1;
                    } else {
                        let tail = &slice[start..];
                        pass2_pending += tail.len() as u64;
                        if tail.len() > MAX_FULL_LINE_BYTES {
                            oversized = true;
                        } else {
                            carry.extend_from_slice(tail);
                        }
                        break;
                    }
                }
            }
        }
    }
}

/// Incrementally scan a host `transcript.jsonl` file using inode + 256-byte head SHA-256 caching,
/// lazy `transcript_full.jsonl` backfill for truncated entries, compaction tracking, and child speed resolution.
pub(super) fn scan_transcript_incremental(
    transcript_path: &Path,
    main_model_ident: Option<&str>,
    now_ms: i64,
    deadline: Option<Instant>,
) -> Option<ScannedTranscript> {
    let meta = fs::metadata(transcript_path).ok()?;
    if !meta.is_file() {
        return None;
    }

    let f_ino = platform::file_unique_id(&meta);
    let f_size = meta.len();
    let cache_path = transcript_cache_path(transcript_path);
    let cached = read_transcript_cache(&cache_path, f_ino, f_size, transcript_path);

    let (mut offset, mut state) = match cached {
        Some((off, st)) => (off, st),
        None => (0, TranscriptAccState::default()),
    };

    let mut cache_dirty = false;
    if offset < f_size
        && !deadline.is_some_and(|d| Instant::now() >= d)
        && let Ok(mut file) = File::open(transcript_path)
        && file.seek(SeekFrom::Start(offset)).is_ok()
    {
        let start_offset = offset;
        let raw_delta = f_size - start_offset;
        let capped_delta = raw_delta.min(MAX_TRANSCRIPT_DELTA_BYTES);

        if capped_delta <= STREAMING_DELTA_THRESHOLD_BYTES {
            enum ChunkEntry {
                Parsed(Map<String, Value>),
                FastSkipCandidate {
                    start: usize,
                    end: usize,
                    step: Option<u64>,
                },
            }

            let to_read = capped_delta as usize;
            let mut buf = vec![0u8; to_read];
            let mut total_read = 0usize;
            while total_read < to_read {
                if deadline.is_some_and(|d| Instant::now() >= d) {
                    break;
                }
                match file.read(&mut buf[total_read..]) {
                    Ok(0) => break,
                    Ok(n) => total_read += n,
                    Err(_) => break,
                }
            }
            let view = &buf[..total_read];
            if let Some(last_nl) = view.iter().rposition(|&b| b == b'\n') {
                let mut chunk_entries: Vec<(ChunkEntry, u64)> = Vec::new();
                let mut needed_full_steps: HashSet<u64> = HashSet::new();
                let mut line_start = 0usize;
                let mut saw_skipped_line = false;

                while line_start <= last_nl {
                    let Some(rel_nl) = find_byte(b'\n', &view[line_start..=last_nl]) else {
                        break;
                    };
                    let line_end = line_start + rel_nl;
                    let next_off = start_offset + (line_end + 1) as u64;
                    let cur_start = line_start;
                    line_start = line_end + 1;
                    let trimmed = view[cur_start..line_end].trim_ascii();
                    if trimmed.is_empty() {
                        saw_skipped_line = true;
                        continue;
                    }
                    if can_fast_skip_transcript_line_bytes(trimmed) {
                        chunk_entries.push((
                            ChunkEntry::FastSkipCandidate {
                                start: cur_start,
                                end: line_end,
                                step: fast_extract_step_index_bytes(trimmed),
                            },
                            next_off,
                        ));
                    } else if let Some(obj) = parse_transcript_json_object(&view[cur_start..line_end])
                    {
                        if obj
                            .get("truncated_fields")
                            .and_then(Value::as_array)
                            .is_some_and(|arr| !arr.is_empty())
                            && let Some(step) = as_u64_val(obj.get("step_index"))
                        {
                            needed_full_steps.insert(step);
                        }
                        chunk_entries.push((ChunkEntry::Parsed(obj), next_off));
                    } else {
                        saw_skipped_line = true;
                    }
                }

                let full_map = if needed_full_steps.is_empty()
                    || deadline.is_some_and(|d| Instant::now() >= d)
                {
                    None
                } else {
                    Some(load_full_entries_map(transcript_path, &needed_full_steps))
                };

                if !deadline.is_some_and(|d| Instant::now() >= d) {
                    let (_, main_conv_id) = resolve_brain_context(transcript_path);
                    let mut completed_all = true;
                    for (idx, (item, next_off)) in chunk_entries.into_iter().enumerate() {
                        if idx > 0
                            && idx.is_multiple_of(256)
                            && deadline.is_some_and(|d| Instant::now() >= d)
                        {
                            completed_all = false;
                            break;
                        }
                        match item {
                            ChunkEntry::Parsed(entry) => {
                                process_transcript_entry(
                                    entry,
                                    &mut state,
                                    main_conv_id.as_deref(),
                                    full_map.as_ref(),
                                    now_ms,
                                );
                                offset = next_off;
                                cache_dirty = true;
                            }
                            ChunkEntry::FastSkipCandidate { start, end, step } => {
                                if state.pending_planner_start_ms.is_none() {
                                    if let Some(s) = step
                                        && s > state.steps
                                    {
                                        state.steps = s;
                                    }
                                    offset = next_off;
                                    cache_dirty = true;
                                } else if let Some(obj) =
                                    parse_transcript_json_object(&view[start..end])
                                {
                                    process_transcript_entry(
                                        obj,
                                        &mut state,
                                        main_conv_id.as_deref(),
                                        full_map.as_ref(),
                                        now_ms,
                                    );
                                    offset = next_off;
                                    cache_dirty = true;
                                } else {
                                    saw_skipped_line = true;
                                }
                            }
                        }
                    }
                    if completed_all {
                        let chunk_end = start_offset + (last_nl + 1) as u64;
                        if chunk_end > offset || saw_skipped_line {
                            offset = offset.max(chunk_end);
                            cache_dirty = true;
                        }
                    }
                }
            }
        } else {
            // Two-pass 64KB O(1)-memory streaming scan for huge deltas (>8MB, capped at 64MB per call).
            scan_transcript_two_pass(
                &mut file,
                TwoPassContext {
                    transcript_path,
                    start_offset,
                    capped_delta,
                    now_ms,
                    deadline,
                },
                &mut state,
                &mut offset,
                &mut cache_dirty,
            );
        }
    } else if f_size == 0 && cache_path.exists() {
        cache_dirty = true;
    }

    let ResolvedChildSpeeds {
        speeds: child_model_speeds,
        activities: resolved_child_activities,
        tail_activities: child_tail_activities,
        persisted_changed: child_persisted_changed,
    } = resolve_child_model_speeds(transcript_path, &mut state, now_ms, deadline);

    if cache_dirty || child_persisted_changed {
        write_transcript_cache(&cache_path, f_ino, offset, &state, transcript_path, f_size);
    }

    let main_ident = main_model_ident
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            state
                .last_turn_usage
                .as_ref()
                .and_then(|lt| lt.model.as_deref())
        })
        .unwrap_or(&state.last_known_model);
    let main_model_key = compact_model_name(main_ident);

    let main_last_active_ms = {
        let model_ts = if !main_model_key.is_empty() {
            state.model_last_active_ms.get(&main_model_key).copied().unwrap_or(0)
        } else {
            0
        };
        let planner_end = state
            .last_planner_response
            .as_ref()
            .and_then(|p| p.end_ms)
            .unwrap_or(0);
        let planner_start = state
            .last_planner_response
            .as_ref()
            .map(|p| p.start_ms)
            .unwrap_or(0);
        let best = model_ts.max(planner_end).max(planner_start);
        if best > 0 { Some(best) } else { None }
    };

    let model_token_totals: Vec<ModelTokenTotals> = state
        .model_totals
        .iter()
        .map(|(k, v)| ModelTokenTotals {
            model_key: k.clone(),
            input_tokens: v.input_tokens,
            cache_read_tokens: v.cache_read_tokens,
            cache_write_tokens: v.cache_write_tokens,
            output_tokens: v.output_tokens,
            steps: v.steps,
        })
        .collect();

    let image_exhausted = state
        .image_exhausted_reset_ms
        .filter(|&reset_ms| reset_ms > now_ms)
        .map(|reset_time_ms| ImageQuotaExhausted { reset_time_ms });

    let all_child_ids: Vec<String> = state.child_agents.keys().cloned().collect();
    let child_activities: Vec<ChildActivitySummary> = if !resolved_child_activities.is_empty() {
        resolved_child_activities
    } else {
        state
            .child_agents
            .values()
            .map(|c| ChildActivitySummary {
                model_hint: c.model_hint.clone(),
                last_active_turn: c.last_active_turn.max(c.added_turn),
                last_active_ms: c.last_active_ms,
            })
            .collect()
    };
    let persisted_child_turns: Vec<(String, Option<u64>, i64)> = state
        .persisted_child_speeds
        .iter()
        .map(|(k, v)| {
            let ts = v
                .last_active_ms
                .unwrap_or(0)
                .max(v.event_ms)
                .max(v.updated_at);
            (k.clone(), v.last_active_turn, ts)
        })
        .collect();

    Some(ScannedTranscript {
        steps: state.steps,
        user_turns: state.user_turns,
        last_user_input_ms: (state.last_user_input_ms > 0).then_some(state.last_user_input_ms),
        has_planner_response: state.has_planner_response,
        usage: state.usage,
        last_turn_usage: state.last_turn_usage,
        last_known_model: state.last_known_model,
        max_historical_cache: state.max_historical_cache,
        compaction_count: state.compaction_count,
        total_input_tokens: state.total_input_tokens,
        total_output_tokens: state.total_output_tokens,
        peak_raw_turn_tokens: state.peak_raw_turn_tokens,
        turn_history_max_by_mode: TurnPeakByMode {
            inclusive_peak: state.inclusive_peak,
            exclusive_peak: state.exclusive_peak,
        },
        last_planner_response: state.last_planner_response,
        main_model_last_active_ms: main_last_active_ms,
        child_model_speeds,
        model_token_totals,
        image_exhausted,
        transcript_cost_usd: state.transcript_cost_usd,
        transcript_cost_by_model: state.model_totals,
        token_steps_count: state.token_steps_count,
        recent_token_steps: state.recent_token_steps,
        all_child_ids,
        model_last_seen_turn: state.model_last_seen_turn,
        model_last_active_ms: state.model_last_active_ms,
        child_activities,
        persisted_child_turns,
        child_tail_activities,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::ingest::stdin::find_context_window;

    #[test]
    fn fast_skip_state_matches_full_serde_across_edge_cases() {
        let now_ms = 1_760_000_000_000i64;
        let main_conv_id = Some("main-conv-0001");

        let lines = vec![
            // 1. USER_INPUT
            r#"{"step_index":1,"type":"USER_INPUT","created_at":"2026-10-01T00:00:00.000Z"}"#,
            // 2. Ordinary GENERIC filler line (fast-skippable)
            r#"{"step_index":2,"type":"GENERIC","pad":"xxxxxxxxxxxxxxxx"}"#,
            // 3. GENERIC with float & whitespace step_index (fast-skippable)
            r#"  { "type" : "GENERIC" , "step_index" : 4.9 , "pad" : "whitespace-float" }  "#,
            // 4. Nested/escaped step_index must NOT override steps (fast-skippable)
            r#"{"type":"GENERIC","meta":{"step_index":999},"pad":"\"step_index\":888"}"#,
            // 5. PLANNER_RESPONSE sets pending_planner_start_ms
            r#"{"step_index":5,"type":"PLANNER_RESPONSE","created_at":"2026-10-01T00:00:01.000Z","model":"gemini-4-carbon-high","output_tokens":50,"input_tokens":1000}"#,
            // 6. GENERIC filler immediately after PLANNER_RESPONSE: MUST NOT skip so it settles pending_planner_start_ms
            r#"{"step_index":6,"type":"GENERIC","created_at":"2026-10-01T00:00:03.500Z","pad":"post-planner"}"#,
            // 7. Subsequent GENERIC filler after settlement: fast-skippable again
            r#"{"step_index":7,"type":"GENERIC","pad":"after-settle"}"#,
            // 8. GENERIC with "content" subagent event: MUST NOT skip
            r#"{"step_index":8,"type":"GENERIC","created_at":"2026-10-01T00:00:04.000Z","content":{"status":"added","conversation_id":"sub-agent-0001","name":"worker-1","model":"gemini-4-carbon-high"}}"#,
            // 9. context_window entry: MUST NOT skip
            r#"{"step_index":9,"type":"GENERIC","context_window":{"context_window_size":500000,"current_usage":{"input_tokens":200,"output_tokens":30,"cache_read_input_tokens":100}}}"#,
            // 10. CHECKPOINT entry: MUST NOT skip
            r#"{"step_index":10,"type":"CHECKPOINT","created_at":"2026-10-01T00:00:05.000Z"}"#,
            // 11. Uppercase ERROR entry: MUST NOT skip
            r#"{"step_index":11,"status":"ERROR","created_at":"2026-10-01T00:00:06.000Z","content":"HTTP 429 RESOURCE_EXHAUSTED quotaResetDelay=30s"}"#,
            // 12. Lowercase error object entry: MUST NOT skip
            r#"{"step_index":12,"type":"OTHER","created_at":"2026-10-01T00:00:07.000Z","error":{"code":429,"status":"RESOURCE_EXHAUSTED","quotaResetDelay":"120s"}}"#,
            // 13. Unicode escape bypass (\u0045RROR): MUST NOT skip
            r#"{"step_index":13,"status":"\u0045RROR","created_at":"2026-10-01T00:00:08.000Z","content":"HTTP 429 RESOURCE_EXHAUSTED quotaResetDelay=240s"}"#,
            // 14. Trailing GENERIC with float .0 step_index (fast-skippable)
            r#"{"step_index":14.0,"type":"GENERIC","pad":"final-pad"}"#,
        ];

        // Verify direct predicate behavior on key samples
        assert!(can_fast_skip_transcript_line(lines[1].trim()));
        assert!(can_fast_skip_transcript_line(lines[2].trim()));
        assert!(can_fast_skip_transcript_line(lines[3].trim()));
        assert!(!can_fast_skip_transcript_line(lines[4].trim()));
        assert!(can_fast_skip_transcript_line(lines[5].trim()));
        assert!(!can_fast_skip_transcript_line(lines[7].trim()));
        assert!(!can_fast_skip_transcript_line(lines[8].trim()));
        assert!(!can_fast_skip_transcript_line(lines[9].trim()));
        assert!(!can_fast_skip_transcript_line(lines[10].trim()));
        assert!(!can_fast_skip_transcript_line(lines[11].trim()));
        assert!(!can_fast_skip_transcript_line(lines[12].trim()));

        assert_eq!(fast_extract_step_index(lines[1].trim()), Some(2));
        assert_eq!(fast_extract_step_index(lines[2].trim()), Some(4));
        assert_eq!(fast_extract_step_index(lines[3].trim()), None);
        assert_eq!(fast_extract_step_index(lines[13].trim()), Some(14));

        // Full reference execution
        let mut full_state = TranscriptAccState::default();
        for line in &lines {
            let trimmed = line.trim();
            if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(trimmed) {
                process_transcript_entry(obj, &mut full_state, main_conv_id, None, now_ms);
            }
        }

        // Fast-skip execution
        let mut fast_state = TranscriptAccState::default();
        let mut skipped_count = 0usize;
        for line in &lines {
            let trimmed = line.trim();
            if fast_state.pending_planner_start_ms.is_none()
                && can_fast_skip_transcript_line(trimmed)
            {
                skipped_count += 1;
                if let Some(step) = fast_extract_step_index(trimmed)
                    && step > fast_state.steps
                {
                    fast_state.steps = step;
                }
            } else if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(trimmed) {
                process_transcript_entry(obj, &mut fast_state, main_conv_id, None, now_ms);
            }
        }

        // Lines 2, 3, 4, 7, 14 are fast-skipped; line 6 is NOT skipped due to pending_planner_start_ms.
        assert_eq!(skipped_count, 5);
        assert_eq!(fast_state.total_api_duration_ms, 2_500);
        assert_eq!(fast_state.steps, 14);
        assert_eq!(fast_state.compaction_count, 1);
        assert_eq!(
            serde_json::to_value(&fast_state).unwrap(),
            serde_json::to_value(&full_state).unwrap()
        );
    }

    #[test]
    fn find_context_window_in_map_matches_value_lookup() {
        let nested_cw: Value = serde_json::json!({
            "step_index": 1,
            "context_window": {
                "context_window_size": 500000,
                "current_usage": { "input_tokens": 10, "output_tokens": 5 }
            }
        });
        let deep_cw: Value = serde_json::json!({
            "step_index": 2,
            "payload": {
                "inner": {
                    "total_input_tokens": 123,
                    "total_output_tokens": 45
                }
            }
        });

        for sample in [&nested_cw, &deep_cw] {
            let map = sample.as_object().unwrap();
            assert_eq!(
                find_context_window_in_map(map, 0),
                find_context_window(sample, 0)
            );
        }
    }

    #[test]
    fn two_pass_streaming_matches_full_serde_across_64kb_boundaries() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let main_id = "44440000-0000-0000-0000-000000000064";
        let logs_dir = dir
            .path()
            .join("brain")
            .join(main_id)
            .join(".system_generated")
            .join("logs");
        fs::create_dir_all(&logs_dir).unwrap();
        let transcript_path = logs_dir.join("transcript.jsonl");
        let full_path = logs_dir.join("transcript_full.jsonl");

        let now_ms = 1_760_000_000_000i64;
        let full_error_line = serde_json::json!({
            "step_index": 500,
            "status": "ERROR",
            "created_at": "2026-10-01T00:00:02.000Z",
            "content": "HTTP 429 RESOURCE_EXHAUSTED",
            "error": {
                "code": 429,
                "status": "RESOURCE_EXHAUSTED",
                "details": [{ "quotaResetTimeStamp": "2026-10-01T02:00:00.000Z" }]
            }
        })
        .to_string();
        fs::write(&full_path, format!("{full_error_line}\n")).unwrap();

        let mut raw = String::new();
        raw.push_str(
            r#"{"step_index":1,"type":"USER_INPUT","created_at":"2026-10-01T00:00:00.000Z"}"#,
        );
        raw.push('\n');

        let mut step = 2u64;
        let push_clean_until = |buf: &mut String, step: &mut u64, target_offset: usize| {
            let pad = "x".repeat(160);
            while buf.len() + 260 < target_offset {
                buf.push_str(&format!(
                    "{{\"step_index\":{},\"type\":\"GENERIC\",\"pad\":\"{pad}\"}}\n",
                    *step
                ));
                *step += 1;
            }
            if buf.len() < target_offset {
                let rem = target_offset - buf.len();
                let prefix = format!("{{\"step_index\":{},\"type\":\"GENERIC\",\"pad\":\"", *step);
                let fill_len = rem.saturating_sub(prefix.len() + 3);
                let fill = "y".repeat(fill_len);
                buf.push_str(&format!("{prefix}{fill}\"}}\n"));
                *step += 1;
            }
        };

        // 1. Place truncated_fields entry straddling the 1st 64KB chunk boundary.
        let trunc_line = r#"{"step_index":500,"status":"ERROR","created_at":"2026-10-01T00:00:02.000Z","content":"...truncated...","truncated_fields":["content"]}"#;
        let trunc_key_offset = trunc_line.find("truncated_fields").unwrap();
        push_clean_until(&mut raw, &mut step, FULL_CHUNK_BYTES - trunc_key_offset - 6);
        assert!(raw.len() < FULL_CHUNK_BYTES);
        assert!(raw.len() + trunc_line.len() > FULL_CHUNK_BYTES);
        raw.push_str(trunc_line);
        raw.push('\n');
        step = 501;

        // 2. Place PLANNER_RESPONSE straddling the 2nd 64KB chunk boundary.
        let planner_line = r#"{"step_index":1000,"type":"PLANNER_RESPONSE","created_at":"2026-10-01T00:00:03.000Z","model":"gemini-4-carbon-high","output_tokens":77,"input_tokens":500}"#;
        let planner_key_offset = planner_line.find("PLANNER_RESPONSE").unwrap();
        push_clean_until(&mut raw, &mut step, 2 * FULL_CHUNK_BYTES - planner_key_offset - 8);
        assert!(raw.len() < 2 * FULL_CHUNK_BYTES);
        assert!(raw.len() + planner_line.len() > 2 * FULL_CHUNK_BYTES);
        raw.push_str(planner_line);
        raw.push('\n');
        // Next line settles pending_planner_start_ms
        raw.push_str(
            r#"{"step_index":1001,"type":"GENERIC","created_at":"2026-10-01T00:00:05.500Z","pad":"settle-1"}"#,
        );
        raw.push('\n');
        step = 1002;

        // 3. Place another PLANNER_RESPONSE right before 3rd 64KB boundary so its settlement line straddles 3 * 64KB.
        let settle_line = r#"{"step_index":1501,"type":"GENERIC","created_at":"2026-10-01T00:00:09.000Z","pad":"settle-across-boundary"}"#;
        let planner_line_2 = r#"{"step_index":1500,"type":"PLANNER_RESPONSE","created_at":"2026-10-01T00:00:07.000Z","model":"gemini-4-carbon-high","output_tokens":23,"input_tokens":200}"#;
        push_clean_until(
            &mut raw,
            &mut step,
            3 * FULL_CHUNK_BYTES - planner_line_2.len() - 1 - settle_line.len() / 2,
        );
        raw.push_str(planner_line_2);
        raw.push('\n');
        assert!(raw.len() < 3 * FULL_CHUNK_BYTES);
        assert!(raw.len() + settle_line.len() > 3 * FULL_CHUNK_BYTES);
        raw.push_str(settle_line);
        raw.push('\n');
        step = 1502;

        // 4. Clean GENERIC line straddling the 4th 64KB chunk boundary.
        push_clean_until(&mut raw, &mut step, 4 * FULL_CHUNK_BYTES - 12);
        let clean_boundary_line = format!(
            "{{\"step_index\":{step},\"type\":\"GENERIC\",\"pad\":\"clean-across-boundary\"}}"
        );
        assert!(raw.len() < 4 * FULL_CHUNK_BYTES);
        assert!(raw.len() + clean_boundary_line.len() > 4 * FULL_CHUNK_BYTES);
        raw.push_str(&clean_boundary_line);
        raw.push('\n');

        {
            let mut f = std::io::BufWriter::new(File::create(&transcript_path).unwrap());
            f.write_all(raw.as_bytes()).unwrap();
            f.flush().unwrap();
        }

        let file_len = fs::metadata(&transcript_path).unwrap().len();
        let mut needed_steps = HashSet::new();
        needed_steps.insert(500);
        let full_map = load_full_entries_map(&transcript_path, &needed_steps);

        let mut full_state = TranscriptAccState::default();
        for line in raw.lines() {
            let trimmed = line.trim();
            if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(trimmed) {
                process_transcript_entry(obj, &mut full_state, Some(main_id), Some(&full_map), now_ms);
            }
        }

        let mut file = File::open(&transcript_path).unwrap();
        let mut two_pass_state = TranscriptAccState::default();
        let mut offset = 0u64;
        let mut cache_dirty = false;
        scan_transcript_two_pass(
            &mut file,
            TwoPassContext {
                transcript_path: &transcript_path,
                start_offset: 0,
                capped_delta: file_len,
                now_ms,
                deadline: None,
            },
            &mut two_pass_state,
            &mut offset,
            &mut cache_dirty,
        );

        assert!(cache_dirty);
        assert_eq!(offset, file_len);
        assert_eq!(two_pass_state.steps, step);
        assert_eq!(two_pass_state.total_output_tokens, 100);
        assert_eq!(two_pass_state.total_api_duration_ms, 4_500);
        assert!(two_pass_state.image_exhausted_reset_ms.is_some());
        assert_eq!(
            serde_json::to_value(&two_pass_state).unwrap(),
            serde_json::to_value(&full_state).unwrap()
        );
    }

    #[test]
    fn signal_matcher_matches_reference_signals_at_all_offsets() {
        let ref_re = regex::bytes::Regex::new(
            r"\\u|truncated_fields|CHECKPOINT|USER_INPUT|PLANNER_RESPONSE|SYSTEM|content|context_window|total_input_tokens|total_output_tokens|used_percentage|current_usage|ERROR|error",
        )
        .unwrap();

        let samples: &[&[u8]] = &[
            b"",
            b"abc",
            b"{\"step_index\":1,\"type\":\"GENERIC\",\"pad\":\"xxxxxxxx\"}",
            b"\\u0045RROR",
            b"xx\\n\\t\\u1234yy",
            b"ERROR",
            b"xERROR",
            b"error",
            b"SYSTEM_MESSAGE",
            b"context_window_size",
            b"prefix truncated_fields suffix",
            b"prefix \\u middle error",
            b"prefix error middle \\u",
            b"CHECKPOINT",
            b"USER_INPUT",
            b"PLANNER_RESPONSE",
            b"content",
            b"total_input_tokens",
            b"total_output_tokens",
            b"used_percentage",
            b"current_usage",
        ];

        for &sample in samples {
            assert_eq!(
                find_first_transcript_signal_bytes(sample),
                ref_re.find(sample).map(|m| m.start()),
                "mismatch for {:?}",
                String::from_utf8_lossy(sample)
            );
        }
    }
}
