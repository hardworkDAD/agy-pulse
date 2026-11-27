use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::domain::{
    ContextWindowSnapshot, CostSnapshot, HostIdentity, ModelIdentity, ProviderQuotaSummary,
    PulseSnapshot, QuotaWindow, TokenUsage, WorkspaceInfo, is_openai_provider_key,
    parse_iso8601_to_epoch_ms, resolve_model_display_name,
};
use crate::platform;

pub(super) struct ParsedStdin {
    pub snapshot: PulseSnapshot,
    pub workspace_dir: PathBuf,
    pub transcript_path: Option<PathBuf>,
    pub api_duration_ms: Option<u64>,
    pub stdin_total_output_tokens: Option<u64>,
    pub stdin_current_usage_val: Option<Value>,
    pub stdin_context_window_val: Option<Value>,
    pub diagnostics: Vec<String>,
}

fn empty_parsed_stdin(
    fallback_cwd: &Path,
    observed_at_ms: i64,
    diagnostics: Vec<String>,
) -> ParsedStdin {
    ParsedStdin {
        snapshot: PulseSnapshot {
            host: HostIdentity {
                username: platform::current_username(),
                email: None,
                ..Default::default()
            },
            observed_at_ms,
            ..Default::default()
        },
        workspace_dir: fallback_cwd.to_path_buf(),
        transcript_path: None,
        api_duration_ms: None,
        stdin_total_output_tokens: None,
        stdin_current_usage_val: None,
        stdin_context_window_val: None,
        diagnostics,
    }
}

pub(super) fn parse_host_stdin(
    stdin_text: &str,
    fallback_cwd: &Path,
    observed_at_ms: i64,
) -> ParsedStdin {
    let trimmed = stdin_text.trim();
    if trimmed.is_empty() {
        return empty_parsed_stdin(fallback_cwd, observed_at_ms, Vec::new());
    }

    let root: Value = match serde_json::from_str::<Value>(trimmed) {
        Ok(val) if val.is_object() => val,
        Ok(_) => {
            return empty_parsed_stdin(
                fallback_cwd,
                observed_at_ms,
                vec!["Host stdin JSON root must be an object".to_string()],
            );
        }
        Err(err) => {
            return empty_parsed_stdin(
                fallback_cwd,
                observed_at_ms,
                vec![format!("Failed to parse host stdin JSON: {err}")],
            );
        }
    };

    let workspace_dir = extract_workspace_dir(&root, fallback_cwd);
    let conversation_id = extract_non_empty_str(root.get("conversation_id"));
    let session_id = extract_non_empty_str(root.get("session_id"));
    let transcript_path = extract_non_empty_str(root.get("transcript_path")).map(PathBuf::from);
    let has_conversation_identity =
        conversation_id.is_some() || session_id.is_some() || transcript_path.is_some();

    let model = extract_model(&root);
    let plan_tier = extract_non_empty_str(root.get("plan_tier"))
        .or_else(|| extract_non_empty_str(root.get("tier")));
    let cycle_mode = extract_non_empty_str(root.get("cycle_mode"));
    let email = extract_non_empty_str(root.get("email"));
    let username = extract_non_empty_str(root.get("username"))
        .or_else(|| email.clone())
        .or_else(platform::current_username);

    let terminal_width = root
        .get("terminal_width")
        .and_then(Value::as_u64)
        .filter(|&w| w > 0 && w <= u16::MAX as u64)
        .map(|w| w as u16);

    let task_count = root.get("task_count").and_then(Value::as_u64).unwrap_or(0);
    let api_duration_ms = as_u64_non_neg(root.get("cost").and_then(|c| c.get("total_api_duration_ms")))
        .or_else(|| as_u64_non_neg(root.get("total_api_duration_ms")));

    let native_total_usd = extract_native_cost(&root);
    let cw_val = find_context_window(&root, 0).cloned();
    let stdin_current_usage_val = cw_val.as_ref().and_then(|cw| cw.get("current_usage")).cloned();
    let stdin_total_output_tokens = cw_val
        .as_ref()
        .and_then(|cw| as_u64_non_neg(cw.get("total_output_tokens")));
    let context = cw_val.as_ref().map(parse_context_window);
    let quota = root
        .get("quota")
        .and_then(|q| parse_stdin_quota(q, observed_at_ms));

    ParsedStdin {
        snapshot: PulseSnapshot {
            has_conversation_identity,
            model,
            host: HostIdentity {
                conversation_id,
                session_id,
                plan_tier,
                cycle_mode,
                username,
                email,
            },
            workspace: WorkspaceInfo::default(),
            context,
            cost: CostSnapshot {
                native_total_usd,
                ..Default::default()
            },
            terminal_width,
            task_count,
            observed_at_ms,
            quota,
            ..Default::default()
        },
        workspace_dir,
        transcript_path,
        api_duration_ms,
        stdin_total_output_tokens,
        stdin_current_usage_val,
        stdin_context_window_val: cw_val,
        diagnostics: Vec::new(),
    }
}

fn parse_reset_time_ms(val: Option<&Value>) -> Option<i64> {
    let v = val?;
    match v {
        Value::String(s) => parse_iso8601_to_epoch_ms(s).filter(|&ms| ms > 0),
        Value::Number(n) => {
            let num = n.as_f64().filter(|x| x.is_finite() && *x > 0.0)?;
            // Epoch seconds (< 100 billion) vs epoch milliseconds (>= 100 billion).
            if num >= 100_000_000_000.0 {
                Some(num.round() as i64)
            } else {
                Some((num * 1000.0).round() as i64)
            }
        }
        _ => None,
    }
}

fn parse_quota_window(entry: &Value, observed_at_ms: i64) -> Option<QuotaWindow> {
    let map = entry.as_object()?;
    if map.get("disabled").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let raw_fraction = map
        .get("remaining_fraction")
        .or_else(|| map.get("remainingFraction"))
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite())?;
    let remaining_fraction = raw_fraction.clamp(0.0, 1.0);
    let reset_time_ms = parse_reset_time_ms(
        map.get("reset_time").or_else(|| map.get("resetTime")),
    )
    .or_else(|| {
        map.get("reset_in_seconds")
            .or_else(|| map.get("resetInSeconds"))
            .and_then(Value::as_f64)
            .filter(|secs| secs.is_finite() && *secs > 0.0)
            .map(|secs| observed_at_ms.saturating_add((secs * 1000.0).round() as i64))
    });
    Some(QuotaWindow {
        remaining_fraction,
        reset_time_ms,
    })
}

fn parse_stdin_quota(quota_val: &Value, observed_at_ms: i64) -> Option<ProviderQuotaSummary> {
    let map = quota_val.as_object()?;
    let mut summary = ProviderQuotaSummary::default();
    let mut any_assigned = false;

    if let Some(entry) = map.get("gemini-5h")
        && let Some(win) = parse_quota_window(entry, observed_at_ms)
    {
        summary.google.five_hour = Some(win);
        any_assigned = true;
    }
    if let Some(entry) = map.get("gemini-weekly")
        && let Some(win) = parse_quota_window(entry, observed_at_ms)
    {
        summary.google.weekly = Some(win);
        any_assigned = true;
    }

    for key in ["anthropic-5h", "claude-5h", "3p-5h"] {
        if let Some(entry) = map.get(key)
            && let Some(win) = parse_quota_window(entry, observed_at_ms)
        {
            summary.claude.five_hour = Some(win);
            any_assigned = true;
        }
    }
    for key in ["anthropic-weekly", "claude-weekly", "3p-weekly"] {
        if let Some(entry) = map.get(key)
            && let Some(win) = parse_quota_window(entry, observed_at_ms)
        {
            summary.claude.weekly = Some(win);
            any_assigned = true;
        }
    }

    for (key, entry) in map {
        let lower = key.to_ascii_lowercase();
        if is_openai_provider_key(&lower) {
            continue;
        }
        if lower.starts_with("argon")
            && let Some(win) = parse_quota_window(entry, observed_at_ms)
        {
            if lower.contains("5h")
                || lower.contains("five_hour")
                || lower.contains("five-hour")
                || lower.contains("fivehour")
            {
                summary.argon.five_hour = Some(win);
            } else if lower.contains("week") {
                summary.argon.weekly = Some(win);
            } else {
                let is_five_hour = win.reset_time_ms.is_some_and(|r| {
                    let delta = r - observed_at_ms;
                    delta > 0 && delta <= 6 * 3_600_000
                });
                if is_five_hour {
                    summary.argon.five_hour = Some(win);
                } else {
                    summary.argon.weekly = Some(win);
                }
            }
            any_assigned = true;
        }
    }

    if any_assigned { Some(summary) } else { None }
}

fn extract_workspace_dir(root: &Value, fallback_cwd: &Path) -> PathBuf {
    if let Some(dir) = root
        .get("workspace")
        .and_then(|w| w.get("current_dir"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return PathBuf::from(dir);
    }
    if let Some(dir) = root
        .get("cwd")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return PathBuf::from(dir);
    }
    fallback_cwd.to_path_buf()
}

fn extract_non_empty_str(val: Option<&Value>) -> Option<String> {
    val.and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

fn extract_model(root: &Value) -> Option<ModelIdentity> {
    let top_effort = extract_non_empty_str(root.get("effort"))
        .or_else(|| extract_non_empty_str(root.get("reasoning_effort")));

    match root.get("model") {
        Some(Value::Object(map)) => {
            let id = extract_non_empty_str(map.get("id")).unwrap_or_default();
            let raw_display = extract_non_empty_str(map.get("display_name"));
            let effort = extract_non_empty_str(map.get("effort")).or(top_effort);
            if id.is_empty() && raw_display.is_none() {
                return None;
            }
            let display_name = if let Some(explicit) = raw_display.as_deref() {
                resolve_model_display_name(&id, Some(explicit))
            } else {
                resolve_model_display_name(&id, None)
            };
            Some(ModelIdentity {
                id,
                display_name,
                effort,
            })
        }
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(ModelIdentity {
                    id: trimmed.to_string(),
                    display_name: resolve_model_display_name(trimmed, None),
                    effort: top_effort,
                })
            }
        }
        _ => None,
    }
}

fn extract_native_cost(root: &Value) -> Option<f64> {
    let candidates = [
        root.get("cost").and_then(|c| c.get("total_cost_usd")),
        root.get("cost").and_then(|c| c.get("total_usd")),
        root.get("total_usd"),
    ];
    for candidate in candidates.into_iter().flatten() {
        if let Some(val) = candidate.as_f64()
            && val.is_finite()
            && val > 0.0
        {
            return Some(val);
        }
    }
    None
}

pub(super) fn is_context_window_map(map: &Map<String, Value>) -> bool {
    map.contains_key("total_input_tokens")
        || map.contains_key("total_output_tokens")
        || map.contains_key("context_window_size")
        || map.contains_key("used_percentage")
        || map.get("current_usage").is_some_and(Value::is_object)
}

fn is_context_window_obj(val: &Value) -> bool {
    val.as_object().is_some_and(is_context_window_map)
}

fn find_context_window_children(map: &Map<String, Value>, depth: u8) -> Option<&Value> {
    for child in map.values() {
        if child.is_object()
            && let Some(found) = find_context_window(child, depth + 1)
        {
            return Some(found);
        }
    }
    None
}

pub(super) fn find_context_window_in_map(map: &Map<String, Value>, depth: u8) -> Option<&Value> {
    if depth > 4 {
        return None;
    }
    if let Some(cw) = map.get("context_window")
        && is_context_window_obj(cw)
    {
        return Some(cw);
    }
    find_context_window_children(map, depth)
}

pub(super) fn find_context_window(val: &Value, depth: u8) -> Option<&Value> {
    if depth > 4 {
        return None;
    }
    let map = val.as_object()?;
    if let Some(cw) = map.get("context_window")
        && is_context_window_obj(cw)
    {
        return Some(cw);
    }
    if is_context_window_map(map) {
        return Some(val);
    }
    find_context_window_children(map, depth)
}

pub(super) fn as_u64_non_neg(val: Option<&Value>) -> Option<u64> {
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

pub(super) fn parse_token_usage(val: &Value) -> Option<TokenUsage> {
    let map = val.as_object()?;
    let input_tokens = as_u64_non_neg(map.get("input_tokens")).unwrap_or(0);
    let output_tokens = as_u64_non_neg(map.get("output_tokens")).unwrap_or(0);
    let cache_read_input_tokens = as_u64_non_neg(map.get("cache_read_input_tokens"))
        .or_else(|| as_u64_non_neg(map.get("cache_read_tokens")))
        .unwrap_or(0);
    let cache_creation_input_tokens = as_u64_non_neg(map.get("cache_creation_input_tokens"))
        .or_else(|| as_u64_non_neg(map.get("cache_creation_tokens")))
        .unwrap_or(0);
    let cache_exclusive = map
        .get("cacheExclusive")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    Some(TokenUsage {
        input_tokens,
        output_tokens,
        cache_read_input_tokens,
        cache_creation_input_tokens,
        cache_exclusive,
    })
}

pub(super) fn parse_context_window(cw: &Value) -> ContextWindowSnapshot {
    let context_window_size = as_u64_non_neg(cw.get("context_window_size")).unwrap_or(0);
    let used_percentage = cw
        .get("used_percentage")
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && *v >= 0.0);
    let total_input_opt = as_u64_non_neg(cw.get("total_input_tokens"));
    let has_total_input = total_input_opt.is_some();
    let total_input_tokens = total_input_opt.unwrap_or(0);
    let total_output_tokens = as_u64_non_neg(cw.get("total_output_tokens")).unwrap_or(0);

    let total_cache_read_tokens = as_u64_non_neg(cw.get("total_cache_read_input_tokens"))
        .or_else(|| as_u64_non_neg(cw.get("total_cache_read_tokens")))
        .or_else(|| as_u64_non_neg(cw.get("cache_read_input_tokens")))
        .or_else(|| as_u64_non_neg(cw.get("cache_read_tokens")));
    let total_cache_creation_tokens = as_u64_non_neg(cw.get("total_cache_creation_input_tokens"))
        .or_else(|| as_u64_non_neg(cw.get("total_cache_creation_tokens")))
        .or_else(|| as_u64_non_neg(cw.get("cache_creation_input_tokens")))
        .or_else(|| as_u64_non_neg(cw.get("cache_creation_tokens")));

    let current_usage = cw.get("current_usage").and_then(parse_token_usage);

    ContextWindowSnapshot {
        context_window_size,
        used_percentage,
        total_input_tokens,
        total_output_tokens,
        total_cache_read_tokens,
        total_cache_creation_tokens,
        current_usage,
        has_total_input,
    }
}
