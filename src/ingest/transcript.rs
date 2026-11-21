use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::UNIX_EPOCH;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::domain::{
    ChildModelSpeed, ImageQuotaExhausted, ModelTokenTotals, TokenUsage, TurnPeakByMode,
    compact_model_name, model_includes_cache_in_input, parse_iso8601_to_epoch_ms,
};
use crate::platform;

use super::speed_tracker::{
    PlannerResponseSpan, is_valid_speed, prune_json_cache_dir, sha256_hex,
};
use super::stdin::find_context_window;

const TRANSCRIPT_CACHE_VERSION: u32 = 9;
const TRANSCRIPT_CACHE_HEAD_BYTES: usize = 256;
const MAX_TRANSCRIPT_CACHE_FILES: usize = 32;
const FULL_CHUNK_BYTES: usize = 64 * 1024;
const MAX_FULL_LINE_BYTES: usize = 4 * 1024 * 1024;
const MAX_FULL_TRANSCRIPT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CHILD_AGENTS: usize = 256;
const MAX_CHILD_TAIL_BYTES: u64 = 65_536;
const MAX_CHILD_OPEN_TURN_MS: i64 = 120_000;
const MIN_CHILD_API_DELTA_MS: i64 = 500;
const MAX_MODEL_IDLE_TURNS: u64 = 30;
const MAX_MODEL_IDLE_MS: i64 = 5 * 60 * 1000;
const MAX_CHILD_MODELS_DISPLAY: usize = 3;

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
struct ModelTokenAcc {
    input_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    output_tokens: u64,
    steps: u64,
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
    model_totals: BTreeMap<String, ModelTokenAcc>,
    last_known_model: String,
    peak_raw_turn_tokens: u64,
    last_turn_usage: Option<CachedTurnUsage>,
    inclusive_peak: u64,
    exclusive_peak: u64,
    child_agents: BTreeMap<String, ChildAgentRecord>,
    persisted_child_speeds: BTreeMap<String, PersistedChildSpeed>,
    user_turns: u64,
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

pub(super) struct ScannedTranscript {
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
    if platform::write_atomic_0600(cache_path, &bytes).is_ok()
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

fn resolve_brain_context(transcript_path: &Path) -> (Option<PathBuf>, Option<String>) {
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
                let model = args
                    .and_then(|a| a.get("Model").or_else(|| a.get("model")))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let role = args
                    .and_then(|a| a.get("Role").or_else(|| a.get("role")))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let type_name = args
                    .and_then(|a| a.get("TypeName").or_else(|| a.get("typeName")))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
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
                if let Some(recipient) = args
                    .and_then(|a| {
                        a.get("Recipient")
                            .or_else(|| a.get("recipient"))
                            .or_else(|| a.get("conversationId"))
                            .or_else(|| a.get("conversation_id"))
                    })
                    .and_then(Value::as_str)
                    && let Some(child) = acc.child_agents.get_mut(recipient)
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
                    .and_then(|a| a.get("Action").or_else(|| a.get("action")))
                    .and_then(Value::as_str)
                    .unwrap_or("");
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
                    let mut ids: Vec<&str> = Vec::new();
                    match raw_ids {
                        Some(Value::Array(arr)) => {
                            for v in arr {
                                if let Some(s) = v.as_str() {
                                    ids.push(s);
                                }
                            }
                        }
                        Some(Value::String(s)) => ids.push(s.as_str()),
                        _ => {}
                    }
                    for target_id in ids {
                        if let Some(child) = acc.child_agents.get_mut(target_id)
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
                    .and_then(|a| a.get("text").or_else(|| a.get("Text")))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    let mention_all = has_mention(msg_text, "all");
                    for child in acc.child_agents.values_mut() {
                        if child.kind == "swarm"
                            && !child.killed
                            && (mention_all || has_mention(msg_text, &child.name))
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
    }

    if entry_type == "SYSTEM_MESSAGE" {
        let content_str = entry.get("content").and_then(Value::as_str).unwrap_or("");
        if content_str.contains("All your subagents and background tasks have been stopped") {
            for child in acc.child_agents.values_mut() {
                child.active = false;
            }
        }
        let cur_turn = effective_turn(acc);
        if let Some(sender) = entry.get("sender").and_then(Value::as_str)
            && let Some(child) = acc.child_agents.get_mut(sender)
        {
            child.last_active_turn = cur_turn;
            if let Some(ts) = valid_time_ms {
                child.last_active_ms = child.last_active_ms.max(ts);
            }
        }
        static SENDER_RE: OnceLock<Option<Regex>> = OnceLock::new();
        if !content_str.is_empty()
            && let Some(re) = SENDER_RE.get_or_init(|| Regex::new(r"\bsender=([^\s\]]+)").ok())
            && let Some(caps) = re.captures(content_str)
            && let Some(m) = caps.get(1)
            && let Some(child) = acc.child_agents.get_mut(m.as_str())
        {
            child.last_active_turn = cur_turn;
            if let Some(ts) = valid_time_ms {
                child.last_active_ms = child.last_active_ms.max(ts);
            }
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

    let entry_val = Value::Object(entry.clone());
    let cw_opt = find_context_window(&entry_val, 0).cloned();
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

        let model_acc = acc.model_totals.entry(model_key).or_default();
        model_acc.input_tokens = model_acc.input_tokens.saturating_add(effective_uncached_in);
        model_acc.cache_read_tokens = model_acc.cache_read_tokens.saturating_add(c_read);
        model_acc.cache_write_tokens = model_acc.cache_write_tokens.saturating_add(c_create);
        model_acc.output_tokens = model_acc.output_tokens.saturating_add(out);
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
            if let Some(ts) = valid_time_ms
                && !has_tool_calls
            {
                let m = if !raw_model.is_empty() {
                    raw_model.to_string()
                } else {
                    latest_model.clone()
                };
                pending_planner = Some(PendingChildPlanner {
                    start_ms: ts,
                    output_tokens: tokens,
                    model: m,
                });
            }
        }
    }

    let mut selected_speed = last_closed_speed;
    if let Some(pending) = pending_planner
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

fn resolve_child_model_speeds(
    transcript_path: &Path,
    acc: &mut TranscriptAccState,
    now_ms: i64,
) -> (Vec<ChildModelSpeed>, bool) {
    if acc.child_agents.is_empty() && acc.persisted_child_speeds.is_empty() {
        return (Vec::new(), false);
    }
    let (brain_dir, _) = resolve_brain_context(transcript_path);
    let current_turns = acc
        .user_turns
        .max(u64::from(acc.has_planner_response));

    let mut registered: Vec<ChildAgentRecord> = acc.child_agents.values().cloned().collect();
    registered.sort_by(|a, b| {
        b.added_step
            .cmp(&a.added_step)
            .then_with(|| b.last_active_ms.cmp(&a.last_active_ms))
    });
    registered.truncate(MAX_CHILD_AGENTS);

    let mut live_by_model: HashMap<String, MergedChildCandidate> = HashMap::new();
    let mut unresolved: Vec<(ChildAgentRecord, i64)> = Vec::new();
    let mut model_last_active_ms = acc.model_last_active_ms.clone();

    for child in &registered {
        let tail = brain_dir.as_ref().and_then(|bd| {
            let cp = bd
                .join(&child.id)
                .join(".system_generated")
                .join("logs")
                .join("transcript.jsonl");
            read_child_tail_telemetry(&cp, child, now_ms)
        });

        let child_last_active_ms = child
            .last_active_ms
            .max(tail.as_ref().map(|t| t.latest_event_ms).unwrap_or(0));

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
                child.last_active_turn,
                cand_last_active_ms,
            );
        } else {
            unresolved.push((child.clone(), child_last_active_ms));
        }
    }

    let mut by_model: HashMap<String, MergedChildCandidate> = live_by_model.clone();

    for (child, child_last_active_ms) in unresolved {
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
                child.last_active_turn,
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

    let before_len = acc.persisted_child_speeds.len();
    acc.persisted_child_speeds.retain(|model_label, entry| {
        let active_entry = by_model.get(model_label);
        let last_turn = active_entry
            .map(|a| a.last_active_turn)
            .or(entry.last_active_turn)
            .unwrap_or(current_turns);
        let turns_ago = current_turns.saturating_sub(last_turn);
        let entry_last_ms = active_entry
            .map(|a| a.last_active_ms)
            .unwrap_or(0)
            .max(entry.last_active_ms.unwrap_or(0))
            .max(entry.event_ms)
            .max(entry.updated_at)
            .max(model_last_active_ms.get(model_label).copied().unwrap_or(0));
        let time_expired = entry_last_ms > 0 && (now_ms - entry_last_ms) > MAX_MODEL_IDLE_MS;
        turns_ago <= MAX_MODEL_IDLE_TURNS && !time_expired
    });
    if acc.persisted_child_speeds.len() != before_len {
        persisted_changed = true;
    }

    let mut active_candidates: Vec<(MergedChildCandidate, u64, i64)> = Vec::new();
    for item in by_model.into_values() {
        if !is_valid_speed(item.speed) {
            continue;
        }
        let turns_ago = current_turns.saturating_sub(item.last_active_turn);
        if turns_ago > MAX_MODEL_IDLE_TURNS {
            continue;
        }
        let last_active_ms = item
            .last_active_ms
            .max(model_last_active_ms.get(&item.model).copied().unwrap_or(0));
        if last_active_ms > 0 && (now_ms - last_active_ms) > MAX_MODEL_IDLE_MS {
            continue;
        }
        active_candidates.push((item, turns_ago, last_active_ms));
    }

    if active_candidates.is_empty() {
        return (Vec::new(), persisted_changed);
    }

    active_candidates.sort_by(|(a, _, _), (b, _, _)| {
        b.event_ms
            .cmp(&a.event_ms)
            .then_with(|| b.mtime_ms.cmp(&a.mtime_ms))
    });
    active_candidates.truncate(MAX_CHILD_MODELS_DISPLAY);
    active_candidates.sort_by(|(a, _, _), (b, _, _)| {
        a.first_order
            .cmp(&b.first_order)
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
    (result, persisted_changed)
}

/// Incrementally scan a host `transcript.jsonl` file using inode + 256-byte head SHA-256 caching,
/// lazy `transcript_full.jsonl` backfill for truncated entries, compaction tracking, and child speed resolution.
pub(super) fn scan_transcript_incremental(
    transcript_path: &Path,
    main_model_ident: Option<&str>,
    now_ms: i64,
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
        && let Ok(mut file) = File::open(transcript_path)
        && file.seek(SeekFrom::Start(offset)).is_ok()
    {
        let to_read = (f_size - offset) as usize;
        let mut buf = vec![0u8; to_read];
        let mut total_read = 0usize;
        while total_read < to_read {
            match file.read(&mut buf[total_read..]) {
                Ok(0) => break,
                Ok(n) => total_read += n,
                Err(_) => break,
            }
        }
        let view = &buf[..total_read];
        if let Some(last_nl) = view.iter().rposition(|&b| b == b'\n') {
            let chunk = String::from_utf8_lossy(&view[..=last_nl]);
            offset += (last_nl + 1) as u64;

            let mut parsed_entries: Vec<Map<String, Value>> = Vec::new();
            let mut needed_full_steps: HashSet<u64> = HashSet::new();
            for line in chunk.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(trimmed) {
                    if obj
                        .get("truncated_fields")
                        .and_then(Value::as_array)
                        .is_some_and(|arr| !arr.is_empty())
                        && let Some(step) = as_u64_val(obj.get("step_index"))
                    {
                        needed_full_steps.insert(step);
                    }
                    parsed_entries.push(obj);
                }
            }

            let full_map = if needed_full_steps.is_empty() {
                None
            } else {
                Some(load_full_entries_map(transcript_path, &needed_full_steps))
            };
            let (_, main_conv_id) = resolve_brain_context(transcript_path);
            for entry in parsed_entries {
                process_transcript_entry(
                    entry,
                    &mut state,
                    main_conv_id.as_deref(),
                    full_map.as_ref(),
                    now_ms,
                );
            }
            cache_dirty = true;
        }
    } else if f_size == 0 && cache_path.exists() {
        cache_dirty = true;
    }

    let (child_model_speeds, child_persisted_changed) =
        resolve_child_model_speeds(transcript_path, &mut state, now_ms);

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

    Some(ScannedTranscript {
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
    })
}
