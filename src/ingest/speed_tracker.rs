use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::platform;

pub(super) const MIN_API_DELTA_MS: u64 = 500;
pub(super) const MAX_OPEN_TURN_TRAILING_MS: u64 = 120_000;
pub(super) const MAX_SPEED_CACHE_FILES: usize = 32;
pub(super) const SPEED_CACHE_VERSION: u32 = 1;
pub(super) const MAX_VALID_SPEED: f64 = 5000.0;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct PlannerResponseSpan {
    pub start_ms: i64,
    pub end_ms: Option<i64>,
    pub output_tokens: Option<u64>,
    pub interrupted: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct SpeedCacheState {
    version: u32,
    source: Option<String>,
    model_key: String,
    speed: Option<f64>,
    api_duration_ms: Option<u64>,
    total_output: Option<u64>,
    planner_start_ms: Option<i64>,
    planner_end_ms: Option<i64>,
    planner_output_tokens: Option<u64>,
    pending: Option<bool>,
}

pub(super) fn is_valid_speed(value: f64) -> bool {
    value.is_finite() && value > 0.0 && value <= MAX_VALID_SPEED
}

pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

pub(super) fn normalize_cache_target(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let p = Path::new(trimmed);
    if p.is_absolute() || trimmed.contains('/') || trimmed.contains('\\') {
        let resolved = fs::canonicalize(p)
            .or_else(|_| std::path::absolute(p))
            .unwrap_or_else(|_| PathBuf::from(trimmed));
        resolved.to_string_lossy().into_owned()
    } else {
        trimmed.to_string()
    }
}

pub(super) fn prune_json_cache_dir(dir: &Path, max_files: usize) {
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<(PathBuf, SystemTime)> = Vec::new();
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        if let Ok(meta) = entry.metadata()
            && meta.is_file()
        {
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            entries.push((path, mtime));
        }
    }
    if entries.len() <= max_files {
        return;
    }
    entries.sort_by_key(|(_, mtime)| *mtime);
    let to_remove = entries.len() - max_files;
    for (path, _) in entries.into_iter().take(to_remove) {
        let _ = fs::remove_file(path);
    }
}

fn speed_cache_path(transcript_key: &str) -> PathBuf {
    let target = normalize_cache_target(transcript_key);
    let key = sha256_hex(target.as_bytes());
    platform::resolve_antigravity_path(&format!("speed-cache/{key}.json"))
}

fn read_speed_state(path: &Path) -> Option<SpeedCacheState> {
    let raw = fs::read_to_string(path).ok()?;
    let state: SpeedCacheState = serde_json::from_str(&raw).ok()?;
    if state.version != SPEED_CACHE_VERSION {
        return None;
    }
    if let Some(speed) = state.speed
        && !is_valid_speed(speed)
    {
        return None;
    }
    Some(state)
}

fn write_speed_state(path: &Path, state: &SpeedCacheState) {
    let Ok(bytes) = serde_json::to_vec(state) else {
        return;
    };
    if platform::write_cache_atomic_0600(path, &bytes).is_ok()
        && let Some(dir) = path.parent()
    {
        prune_json_cache_dir(dir, MAX_SPEED_CACHE_FILES);
    }
}

fn sample_api_speed(
    cache_path: &Path,
    previous: Option<&SpeedCacheState>,
    model_key: &str,
    api_duration_ms: u64,
    total_output: u64,
) -> Option<f64> {
    let mut speed = None;
    if let Some(prev) = previous
        && prev.source.as_deref() == Some("api")
        && prev.model_key == model_key
        && let (Some(prev_ms), Some(prev_out)) = (prev.api_duration_ms, prev.total_output)
    {
        if api_duration_ms == prev_ms && total_output == prev_out {
            return prev.speed;
        }
        if api_duration_ms >= prev_ms && total_output >= prev_out {
            let delta_ms = api_duration_ms - prev_ms;
            let delta_tokens = total_output - prev_out;
            if delta_ms >= MIN_API_DELTA_MS && delta_tokens > 0 {
                let rate = delta_tokens as f64 / (delta_ms as f64 / 1000.0);
                if is_valid_speed(rate) {
                    speed = Some(rate);
                }
            }
        }
    }

    let next = SpeedCacheState {
        version: SPEED_CACHE_VERSION,
        source: Some("api".to_string()),
        model_key: model_key.to_string(),
        speed,
        api_duration_ms: Some(api_duration_ms),
        total_output: Some(total_output),
        ..Default::default()
    };
    write_speed_state(cache_path, &next);
    speed
}

fn sample_planner_speed(
    cache_path: &Path,
    previous: Option<&SpeedCacheState>,
    model_key: &str,
    planner: &PlannerResponseSpan,
    now_ms: i64,
) -> Option<f64> {
    let same_model = previous
        .is_some_and(|p| p.source.as_deref() == Some("transcript") && p.model_key == model_key);
    let same_response = previous.is_some_and(|p| {
        p.source.as_deref() == Some("transcript") && p.planner_start_ms == Some(planner.start_ms)
    });
    if same_response && !same_model {
        return None;
    }

    let same_sample = same_response
        && previous.is_some_and(|p| {
            p.planner_end_ms == planner.end_ms && p.planner_output_tokens == planner.output_tokens
        });
    if same_sample && previous.is_some_and(|p| p.pending == Some(false)) {
        return previous.and_then(|p| p.speed);
    }

    let is_open = planner.end_ms.is_none();
    let end_or_now = planner.end_ms.unwrap_or(now_ms);
    let duration_ms = if end_or_now >= planner.start_ms {
        (end_or_now - planner.start_ms) as u64
    } else {
        0
    };
    let expired = is_open && (planner.interrupted || duration_ms > MAX_OPEN_TURN_TRAILING_MS);

    let mut speed = None;
    let mut pending = false;

    if let Some(out_tokens) = planner.output_tokens
        && out_tokens > 0
    {
        let too_short = duration_ms < MIN_API_DELTA_MS;
        if expired || (is_open && too_short) {
            speed = if same_model {
                previous.and_then(|p| p.speed)
            } else {
                None
            };
        } else {
            let rate = out_tokens as f64 / (duration_ms as f64 / 1000.0);
            if is_valid_speed(rate) {
                speed = Some(rate);
            }
        }
        pending = is_open && !expired && (too_short || speed.is_none());
    }

    if same_sample
        && let Some(prev) = previous
        && prev.speed == speed
        && prev.pending == Some(pending)
    {
        return speed;
    }

    let next = SpeedCacheState {
        version: SPEED_CACHE_VERSION,
        source: Some("transcript".to_string()),
        model_key: model_key.to_string(),
        speed,
        planner_start_ms: Some(planner.start_ms),
        planner_end_ms: planner.end_ms,
        planner_output_tokens: planner.output_tokens,
        pending: Some(pending),
        ..Default::default()
    };
    write_speed_state(cache_path, &next);
    speed
}

/// Compute instantaneous output token speed from paired cumulative API counters
/// or fallback to the latest timed `PLANNER_RESPONSE` span in the transcript.
pub(super) fn compute_output_speed(
    transcript_key: Option<&str>,
    model_key: &str,
    api_duration_ms: Option<u64>,
    stdin_total_output: Option<u64>,
    last_planner_response: Option<&PlannerResponseSpan>,
    observed_at_ms: i64,
) -> Option<f64> {
    let key = transcript_key.map(str::trim).filter(|s| !s.is_empty())?;
    let cache_path = speed_cache_path(key);
    let previous = read_speed_state(&cache_path);
    let norm_model_key = model_key.trim().to_ascii_lowercase();

    if let (Some(api_ms), Some(total_out)) = (api_duration_ms, stdin_total_output) {
        return sample_api_speed(
            &cache_path,
            previous.as_ref(),
            &norm_model_key,
            api_ms,
            total_out,
        );
    }

    if let Some(planner) = last_planner_response {
        return sample_planner_speed(
            &cache_path,
            previous.as_ref(),
            &norm_model_key,
            planner,
            observed_at_ms,
        );
    }

    if previous.as_ref().is_some_and(|p| p.source.is_some()) {
        let reset = SpeedCacheState {
            version: SPEED_CACHE_VERSION,
            model_key: norm_model_key,
            ..Default::default()
        };
        write_speed_state(&cache_path, &reset);
    }
    None
}
