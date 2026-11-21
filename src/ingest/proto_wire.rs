use std::borrow::Cow;

use crate::domain::compact_model_name;

pub(super) const MIN_COMPACTION_LIMIT: u64 = 16_000;
pub(super) const MAX_COMPACTION_LIMIT: u64 = 10_000_000;
pub(super) const MIN_VALID_STREAM_SEC: f64 = 0.05;
pub(super) const MAX_VALID_STREAM_SEC: f64 = 600.0;
pub(super) const MAX_VALID_STREAM_SPEED: f64 = 5000.0;
pub(super) const MAX_VALID_TTFT_SEC: f64 = 600.0;

/// Pure-data per-generation step telemetry decoded from `gen_metadata` protobuf blobs.
#[derive(Debug, Clone, PartialEq)]
pub struct GenerationTelemetryStep {
    pub model_key: String,
    pub step_model: String,
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub output_tokens: u64,
    pub stream_sec: Option<f64>,
    pub ttft_sec: Option<f64>,
    pub valid_stream: bool,
    pub valid_ttft: bool,
    pub last_step_index: Option<u64>,
    pub bot_uuid: Option<String>,
}

/// Decoded `gen_metadata` row containing step indices, cascade lineage, and step telemetry.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GenerationMetadataRow {
    pub idx: i64,
    pub step_indices: Vec<u64>,
    pub parent_cascade_id: Option<String>,
    pub root_cascade_id: Option<String>,
    pub cascade_id: Option<String>,
    pub steps: Vec<GenerationTelemetryStep>,
}

/// Decoded `trajectory_metadata_blob` pure-data summary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrajectoryMetadata {
    pub workspace: Option<String>,
    pub branch: Option<String>,
    pub created_at_ms: Option<i64>,
    pub cascade_id: Option<String>,
}

pub(super) fn is_valid_compaction_limit(val: u64) -> bool {
    (MIN_COMPACTION_LIMIT..=MAX_COMPACTION_LIMIT).contains(&val)
}

pub(super) fn decode_varint(buf: &[u8], offset: usize) -> Option<(u64, usize)> {
    if offset >= buf.len() {
        return None;
    }
    let mut res = 0u64;
    let mut shift = 0u32;
    let mut o = offset;
    while o < buf.len() {
        let byte = buf[o];
        o += 1;
        res |= u64::from(byte & 0x7f) << shift;
        if (byte & 0x80) == 0 {
            return Some((res, o));
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
    None
}

pub(super) fn pb_get_sub(buf: &[u8], field_num: u32) -> Option<&[u8]> {
    if field_num == 0 {
        return None;
    }
    let mut o = 0usize;
    let n = buf.len();
    while o < n {
        let (tag, next) = decode_varint(buf, o)?;
        o = next;
        let fn_num = (tag >> 3) as u32;
        let wire = (tag & 0x07) as u8;
        match wire {
            0 => {
                let (_, v_next) = decode_varint(buf, o)?;
                o = v_next;
            }
            2 => {
                let (len_u64, l_next) = decode_varint(buf, o)?;
                let len = usize::try_from(len_u64).ok()?;
                o = l_next;
                let end = o.checked_add(len)?;
                if end > n {
                    return None;
                }
                if fn_num == field_num {
                    return Some(&buf[o..end]);
                }
                o = end;
            }
            1 => {
                o = o.checked_add(8)?;
                if o > n {
                    return None;
                }
            }
            5 => {
                o = o.checked_add(4)?;
                if o > n {
                    return None;
                }
            }
            _ => break,
        }
    }
    None
}

pub(super) fn pb_get_all_sub(buf: &[u8], field_num: u32) -> Vec<&[u8]> {
    let mut results = Vec::new();
    if field_num == 0 {
        return results;
    }
    let mut o = 0usize;
    let n = buf.len();
    while o < n {
        let Some((tag, next)) = decode_varint(buf, o) else {
            break;
        };
        o = next;
        let fn_num = (tag >> 3) as u32;
        let wire = (tag & 0x07) as u8;
        match wire {
            0 => {
                let Some((_, v_next)) = decode_varint(buf, o) else {
                    break;
                };
                o = v_next;
            }
            2 => {
                let Some((len_u64, l_next)) = decode_varint(buf, o) else {
                    break;
                };
                let Ok(len) = usize::try_from(len_u64) else {
                    break;
                };
                o = l_next;
                let Some(end) = o.checked_add(len) else {
                    break;
                };
                if end > n {
                    break;
                }
                if fn_num == field_num {
                    results.push(&buf[o..end]);
                }
                o = end;
            }
            1 => {
                let Some(end) = o.checked_add(8) else {
                    break;
                };
                if end > n {
                    break;
                }
                o = end;
            }
            5 => {
                let Some(end) = o.checked_add(4) else {
                    break;
                };
                if end > n {
                    break;
                }
                o = end;
            }
            _ => break,
        }
    }
    results
}

pub(super) fn pb_get_varint(buf: &[u8], field_num: u32) -> Option<u64> {
    if field_num == 0 {
        return None;
    }
    let mut o = 0usize;
    let n = buf.len();
    while o < n {
        let (tag, next) = decode_varint(buf, o)?;
        o = next;
        let fn_num = (tag >> 3) as u32;
        let wire = (tag & 0x07) as u8;
        match wire {
            0 => {
                let (val, v_next) = decode_varint(buf, o)?;
                o = v_next;
                if fn_num == field_num {
                    return Some(val);
                }
            }
            2 => {
                let (len_u64, l_next) = decode_varint(buf, o)?;
                let len = usize::try_from(len_u64).ok()?;
                o = l_next.checked_add(len)?;
                if o > n {
                    return None;
                }
            }
            1 => {
                o = o.checked_add(8)?;
                if o > n {
                    return None;
                }
            }
            5 => {
                o = o.checked_add(4)?;
                if o > n {
                    return None;
                }
            }
            _ => break,
        }
    }
    None
}

pub(super) fn pb_get_string(buf: &[u8], field_num: u32) -> Option<Cow<'_, str>> {
    pb_get_sub(buf, field_num).map(String::from_utf8_lossy)
}

pub(super) fn pb_get_duration_sec(buf: &[u8], field_num: u32) -> Option<f64> {
    let dur = pb_get_sub(buf, field_num)?;
    let sec = pb_get_varint(dur, 1).unwrap_or(0);
    let nanos = pb_get_varint(dur, 2).unwrap_or(0);
    if sec > 1_000_000_000_000 || nanos >= 1_000_000_000 {
        return None;
    }
    let total = sec as f64 + (nanos as f64 / 1e9);
    if total.is_finite() && total >= 0.0 {
        Some(total)
    } else {
        None
    }
}

fn decode_protobuf_timestamp_ms(ts_sub: Option<&[u8]>) -> Option<i64> {
    let sub = ts_sub?;
    let sec = pb_get_varint(sub, 1)?;
    if sec == 0 || sec > 1_000_000_000_000 {
        return None;
    }
    let nanos = pb_get_varint(sub, 2).unwrap_or(0);
    if nanos >= 1_000_000_000 {
        return None;
    }
    let sec_i64 = i64::try_from(sec).ok()?;
    let ms = sec_i64
        .checked_mul(1000)?
        .checked_add((nanos / 1_000_000) as i64)?;
    Some(ms)
}

/// Extract `CheckpointConfig.max_context_tokens` from `gen_metadata` (`f1 -> f9 -> f10 -> f4`).
pub(super) fn extract_compaction_limit_from_gen_metadata(buf: &[u8]) -> Option<u64> {
    for f1 in pb_get_all_sub(buf, 1) {
        for f9 in pb_get_all_sub(f1, 9) {
            for f10 in pb_get_all_sub(f9, 10) {
                if let Some(lim) = pb_get_varint(f10, 4)
                    && is_valid_compaction_limit(lim)
                {
                    return Some(lim);
                }
            }
        }
    }
    None
}

/// Extract `CheckpointConfig.max_context_tokens` from `executor_metadata` (`f10 -> f2 -> f5`).
pub(super) fn extract_compaction_limit_from_executor_metadata(buf: &[u8]) -> Option<u64> {
    for f10 in pb_get_all_sub(buf, 10) {
        for f2 in pb_get_all_sub(f10, 2) {
            if let Some(lim) = pb_get_varint(f2, 5)
                && is_valid_compaction_limit(lim)
            {
                return Some(lim);
            }
        }
    }
    None
}

fn extract_step_indices_from_gen_metadata(buf: &[u8]) -> Vec<u64> {
    let mut indices = Vec::new();
    let mut o = 0usize;
    let n = buf.len();
    while o < n {
        let Some((tag, next)) = decode_varint(buf, o) else {
            break;
        };
        o = next;
        let fn_num = (tag >> 3) as u32;
        let wire = (tag & 0x07) as u8;
        match wire {
            0 => {
                let Some((val, v_next)) = decode_varint(buf, o) else {
                    break;
                };
                o = v_next;
                if fn_num == 2 {
                    indices.push(val);
                }
            }
            2 => {
                let Some((len_u64, l_next)) = decode_varint(buf, o) else {
                    break;
                };
                let Ok(len) = usize::try_from(len_u64) else {
                    break;
                };
                o = l_next;
                let Some(end) = o.checked_add(len) else {
                    break;
                };
                if end > n {
                    break;
                }
                if fn_num == 2 {
                    let sub = &buf[o..end];
                    let mut so = 0usize;
                    while so < sub.len() {
                        let Some((item, s_next)) = decode_varint(sub, so) else {
                            break;
                        };
                        so = s_next;
                        indices.push(item);
                    }
                }
                o = end;
            }
            1 => {
                let Some(end) = o.checked_add(8) else {
                    break;
                };
                if end > n {
                    break;
                }
                o = end;
            }
            5 => {
                let Some(end) = o.checked_add(4) else {
                    break;
                };
                if end > n {
                    break;
                }
                o = end;
            }
            _ => break,
        }
    }
    indices
}

/// Extract per-generation telemetry, unified token usage (`f4.f1 + f4.f2` input),
/// and cascade lineage keys (`cascade_id`, `parent_cascade_id`, `root_cascade_id`)
/// from a single `gen_metadata` row blob.
pub(super) fn extract_gen_metadata_row_details(idx: i64, buf: &[u8]) -> GenerationMetadataRow {
    let step_indices = extract_step_indices_from_gen_metadata(buf);
    let mut entries = Vec::new();
    let mut system_prompt_only_entries = Vec::new();
    let mut parent_cascade_id: Option<String> = None;
    let mut root_cascade_id: Option<String> = None;
    let mut cascade_id: Option<String> = None;

    for f1 in pb_get_all_sub(buf, 1) {
        let trimmed_display = pb_get_string(f1, 21)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let trimmed_raw = pb_get_string(f1, 19)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let mut trimmed_enum = String::new();
        let mut last_step_index: Option<u64> = None;

        for kv in pb_get_all_sub(f1, 20) {
            let Some(k) = pb_get_string(kv, 1) else {
                continue;
            };
            let Some(v) = pb_get_string(kv, 2) else {
                continue;
            };
            let trimmed_val = v.trim();
            if trimmed_val.is_empty() {
                continue;
            }
            match k.as_ref() {
                "model_enum" if trimmed_enum.is_empty() => {
                    trimmed_enum = trimmed_val.to_string();
                }
                "parent_cascade_id" if parent_cascade_id.is_none() => {
                    parent_cascade_id = Some(trimmed_val.to_string());
                }
                "root_cascade_id" if root_cascade_id.is_none() => {
                    root_cascade_id = Some(trimmed_val.to_string());
                }
                "cascade_id" if cascade_id.is_none() => {
                    cascade_id = Some(trimmed_val.to_string());
                }
                "last_step_index" if last_step_index.is_none() => {
                    if let Ok(parsed) = trimmed_val.parse::<u64>() {
                        last_step_index = Some(parsed);
                    }
                }
                _ => {}
            }
        }

        let step_model = if !trimmed_display.is_empty() {
            trimmed_display
        } else if !trimmed_raw.is_empty() {
            trimmed_raw
        } else {
            trimmed_enum
        };
        let model_key = compact_model_name(&step_model);
        if model_key.is_empty() {
            continue;
        }

        let usage = pb_get_sub(f1, 4);
        let system_prompt_tokens = usage.and_then(|u| pb_get_varint(u, 1)).unwrap_or(0);
        let raw_input_tokens = usage.and_then(|u| pb_get_varint(u, 2)).unwrap_or(0);
        let input_tokens = system_prompt_tokens.saturating_add(raw_input_tokens);
        let output_tokens = usage.and_then(|u| pb_get_varint(u, 3)).unwrap_or(0);
        let cache_read_tokens = usage.and_then(|u| pb_get_varint(u, 5)).unwrap_or(0);
        let cache_write_tokens = 0u64;
        let bot_uuid = usage
            .and_then(|u| pb_get_string(u, 7))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let ttft_sec = pb_get_duration_sec(f1, 11);
        let stream_sec = pb_get_duration_sec(f1, 12);

        let valid_stream = output_tokens > 0
            && stream_sec.is_some_and(|s| {
                s.is_finite()
                    && (MIN_VALID_STREAM_SEC..=MAX_VALID_STREAM_SEC).contains(&s)
                    && (output_tokens as f64 / s) <= MAX_VALID_STREAM_SPEED
            });
        let valid_ttft =
            ttft_sec.is_some_and(|t| t.is_finite() && (0.0..=MAX_VALID_TTFT_SEC).contains(&t));
        let has_other_tokens =
            raw_input_tokens > 0 || output_tokens > 0 || cache_read_tokens > 0 || cache_write_tokens > 0;
        if system_prompt_tokens == 0 && !has_other_tokens && !valid_ttft {
            continue;
        }

        let step = GenerationTelemetryStep {
            model_key,
            step_model,
            input_tokens,
            cache_read_tokens,
            cache_write_tokens,
            output_tokens,
            stream_sec,
            ttft_sec,
            valid_stream,
            valid_ttft,
            last_step_index,
            bot_uuid,
        };
        if has_other_tokens {
            entries.push(step);
        } else {
            system_prompt_only_entries.push(step);
        }
    }

    entries.extend(system_prompt_only_entries);
    GenerationMetadataRow {
        idx,
        step_indices,
        parent_cascade_id,
        root_cascade_id,
        cascade_id,
        steps: entries,
    }
}

pub(super) fn extract_step_timestamp_and_bot(buf: &[u8]) -> (Option<i64>, Option<String>) {
    let ts_ms = decode_protobuf_timestamp_ms(pb_get_sub(buf, 1));
    let bot_uuid = pb_get_sub(buf, 9)
        .and_then(|sub9| pb_get_string(sub9, 7))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    (ts_ms, bot_uuid)
}

fn percent_decode_lossy(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex_slice = &input[i + 1..i + 3];
            if let Ok(b) = u8::from_str_radix(hex_slice, 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn normalize_workspace_uri(raw: Option<Cow<'_, str>>) -> Option<String> {
    let raw_str = raw?;
    let mut s = raw_str.trim().to_string();
    if s.is_empty() {
        return None;
    }
    s = percent_decode_lossy(&s);
    if s.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("file://")) {
        let rest = &s[7..];
        if rest.starts_with('/')
            && rest.len() >= 3
            && rest.as_bytes()[1].is_ascii_alphabetic()
            && rest.as_bytes()[2] == b':'
        {
            s = rest[1..].to_string();
        } else {
            s = rest.to_string();
        }
    }
    let is_windows_drive_root = s.len() >= 2
        && s.as_bytes()[0].is_ascii_alphabetic()
        && s.as_bytes()[1] == b':'
        && (s.len() == 2 || (s.len() == 3 && s.as_bytes()[2] == b'/'));
    if s.len() > 1 && !is_windows_drive_root {
        while s.ends_with('/') {
            s.pop();
        }
    }
    if s.is_empty() { None } else { Some(s) }
}

pub(super) fn extract_trajectory_meta(buf: &[u8]) -> TrajectoryMetadata {
    let mut workspace: Option<String> = None;
    let mut branch: Option<String> = None;
    for w in pb_get_all_sub(buf, 1) {
        if workspace.is_none() {
            workspace = normalize_workspace_uri(pb_get_string(w, 1).or_else(|| pb_get_string(w, 2)));
        }
        if branch.is_none()
            && let Some(br) = pb_get_string(w, 4)
        {
            let trimmed = br.trim();
            if !trimmed.is_empty() {
                branch = Some(trimmed.to_string());
            }
        }
    }
    if workspace.is_none() {
        workspace = normalize_workspace_uri(pb_get_string(buf, 7));
    }
    let created_at_ms = decode_protobuf_timestamp_ms(pb_get_sub(buf, 2));
    let cascade_id = pb_get_string(buf, 6)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    TrajectoryMetadata {
        workspace,
        branch,
        created_at_ms,
        cascade_id,
    }
}
