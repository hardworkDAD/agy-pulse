use std::collections::BTreeMap;
use std::io::{Read, Write};

use flate2::{Compression, GzBuilder, read::GzDecoder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::domain::{ProviderQuotaSummary, UNASSIGNED_ACCOUNT_ID, normalize_account_id};

use super::SyncError;
use super::config::DEFAULT_SYNC_PREFIX;

pub const SYNC_WIRE_FORMAT_VERSION: u32 = 1;
pub const MAX_DECOMPRESSED_CHUNK_BYTES: usize = 64 * 1024 * 1024;
pub const MANIFEST_CHUNK_STATE_KEY: &str = "__manifest__";

/// Normalize sync object prefix by trimming whitespace and leading/trailing `/`.
/// Falls back to `"agy-pulse-sync"` if empty.
pub fn normalize_prefix(raw: &str) -> String {
    let trimmed = raw.trim().trim_matches('/');
    if trimmed.is_empty() {
        DEFAULT_SYNC_PREFIX.to_string()
    } else {
        trimmed.to_string()
    }
}

fn contains_disallowed_chars(s: &str) -> bool {
    s.chars().any(|c| c.is_control() || c == '\0')
}

/// Validate that `seg` is a single safe relative path segment (non-empty, not `.` / `..`, no `/`, `\`, or control chars).
pub fn validate_single_segment<'a>(seg: &'a str, label: &str) -> Result<&'a str, SyncError> {
    if seg.is_empty() {
        return Err(SyncError::InvalidKey(format!("{label} must not be empty")));
    }
    if seg == "." || seg == ".." {
        return Err(SyncError::InvalidKey(format!(
            "{label} must not be '.' or '..'"
        )));
    }
    if seg.contains('/') || seg.contains('\\') || contains_disallowed_chars(seg) {
        return Err(SyncError::InvalidKey(format!(
            "{label} contains disallowed path or control characters"
        )));
    }
    Ok(seg)
}

/// Validate a relative object key (non-empty, no leading/trailing `/`, no `\`, no empty segments, no `.` or `..`, no control chars).
pub fn validate_relative_key(key: &str) -> Result<&str, SyncError> {
    if key.is_empty() {
        return Err(SyncError::InvalidKey("object key must not be empty".into()));
    }
    if key.starts_with('/') || key.ends_with('/') {
        return Err(SyncError::InvalidKey(format!(
            "object key must not start or end with '/': {key}"
        )));
    }
    if key.contains('\\') || contains_disallowed_chars(key) {
        return Err(SyncError::InvalidKey(format!(
            "object key contains disallowed characters: {key}"
        )));
    }
    for seg in key.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return Err(SyncError::InvalidKey(format!(
                "object key contains invalid segment: {key}"
            )));
        }
    }
    Ok(key)
}

/// Validate a list prefix (may be `""` to list all keys, otherwise must pass `validate_relative_key` after stripping trailing `/`).
pub fn validate_relative_prefix(prefix: &str) -> Result<String, SyncError> {
    if prefix.is_empty() {
        return Ok(String::new());
    }
    if prefix.starts_with('/') || prefix.contains('\\') || contains_disallowed_chars(prefix) {
        return Err(SyncError::InvalidKey(format!(
            "prefix contains disallowed characters: {prefix}"
        )));
    }
    let trimmed = prefix.trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(SyncError::InvalidKey(format!(
            "prefix must not be root slash: {prefix}"
        )));
    }
    validate_relative_key(trimmed)?;
    Ok(trimmed.to_string())
}

fn is_safe_account_key_byte(b: u8) -> bool {
    matches!(
        b,
        b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'@' | b'+' | b'-'
    )
}

fn validate_account_string_safety(s: &str, label: &str) -> Result<(), SyncError> {
    if s.is_empty()
        || s == "."
        || s == ".."
        || s.contains('/')
        || s.contains('\\')
        || contains_disallowed_chars(s)
    {
        return Err(SyncError::InvalidKey(format!("unsafe {label} value: {s}")));
    }
    Ok(())
}

/// Percent-encode an account identifier into a filesystem/URL-safe `<account_key>`,
/// preserving `[a-z0-9._@+-]` and rejecting empty, `.`, `..`, `/`, `\`, or control chars.
pub fn encode_account_key(account_id: &str) -> Result<String, SyncError> {
    validate_account_string_safety(account_id, "account_id")?;
    let mut out = String::with_capacity(account_id.len());
    for &b in account_id.as_bytes() {
        if is_safe_account_key_byte(b) {
            out.push(b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    validate_account_string_safety(&out, "encoded account_key")?;
    Ok(out)
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Decode a `<account_key>` back into the original `account_id`, rejecting malformed `%XX`,
/// invalid UTF-8, empty strings, `.`, `..`, `/`, `\`, or control chars.
pub fn decode_account_key(account_key: &str) -> Result<String, SyncError> {
    validate_account_string_safety(account_key, "account_key")?;
    let bytes = account_key.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err(SyncError::InvalidKey(format!(
                    "truncated percent-encoding in account_key: {account_key}"
                )));
            }
            let hi = hex_nibble(bytes[i + 1]).ok_or_else(|| {
                SyncError::InvalidKey(format!("invalid hex in account_key: {account_key}"))
            })?;
            let lo = hex_nibble(bytes[i + 2]).ok_or_else(|| {
                SyncError::InvalidKey(format!("invalid hex in account_key: {account_key}"))
            })?;
            decoded.push((hi << 4) | lo);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }

    let s = String::from_utf8(decoded)
        .map_err(|_| SyncError::InvalidKey(format!("non-UTF-8 account_key: {account_key}")))?;
    validate_account_string_safety(&s, "decoded account_id")?;
    Ok(s)
}

fn validate_normalized_prefix(prefix: &str) -> Result<String, SyncError> {
    let norm = normalize_prefix(prefix);
    validate_relative_key(&norm)?;
    Ok(norm)
}

/// Build `<prefix>/accounts`.
pub fn accounts_prefix(prefix: &str) -> Result<String, SyncError> {
    let norm = validate_normalized_prefix(prefix)?;
    let key = format!("{norm}/accounts");
    validate_relative_key(&key)?;
    Ok(key)
}

/// Build `<prefix>/accounts/<account_key>/devices/<device_id>/manifest.json`.
pub fn manifest_key(prefix: &str, account_id: &str, device_id: &str) -> Result<String, SyncError> {
    let norm = validate_normalized_prefix(prefix)?;
    let normalized_acct =
        normalize_account_id(account_id).unwrap_or_else(|| UNASSIGNED_ACCOUNT_ID.to_string());
    let acct_key = encode_account_key(&normalized_acct)?;
    let dev_seg = validate_single_segment(device_id.trim(), "device_id")?;
    let key = format!("{norm}/accounts/{acct_key}/devices/{dev_seg}/manifest.json");
    validate_relative_key(&key)?;
    Ok(key)
}

/// Build `<prefix>/accounts/<account_key>/devices/<device_id>/chunks/<YYYY-MM>.jsonl.gz`.
pub fn chunk_key(
    prefix: &str,
    account_id: &str,
    device_id: &str,
    month_key: &str,
) -> Result<String, SyncError> {
    let norm = validate_normalized_prefix(prefix)?;
    let normalized_acct =
        normalize_account_id(account_id).unwrap_or_else(|| UNASSIGNED_ACCOUNT_ID.to_string());
    let acct_key = encode_account_key(&normalized_acct)?;
    let dev_seg = validate_single_segment(device_id.trim(), "device_id")?;
    let month_seg = validate_single_segment(month_key.trim(), "month_key")?;
    let key = format!("{norm}/accounts/{acct_key}/devices/{dev_seg}/chunks/{month_seg}.jsonl.gz");
    validate_relative_key(&key)?;
    Ok(key)
}

/// Parsed components of a remote `manifest.json` object key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestKeyParts {
    pub account_id: String,
    pub account_key: String,
    pub device_id: String,
}

/// Parse `<prefix>/accounts/<account_key>/devices/<device_id>/manifest.json`.
pub fn parse_manifest_key(prefix: &str, key: &str) -> Option<ManifestKeyParts> {
    let norm = validate_normalized_prefix(prefix).ok()?;
    let valid_key = validate_relative_key(key).ok()?;
    let expected_root = format!("{norm}/accounts/");
    let rest = valid_key.strip_prefix(&expected_root)?;
    let mut parts = rest.split('/');
    let acct_key = parts.next()?;
    let devices_lit = parts.next()?;
    let device_id = parts.next()?;
    let file_lit = parts.next()?;
    if parts.next().is_some() || devices_lit != "devices" || file_lit != "manifest.json" {
        return None;
    }
    validate_single_segment(device_id, "device_id").ok()?;
    let decoded_acct = decode_account_key(acct_key).ok()?;
    let account_id = normalize_account_id(&decoded_acct)?;
    Some(ManifestKeyParts {
        account_id,
        account_key: acct_key.to_string(),
        device_id: device_id.to_string(),
    })
}

/// Check whether `key` belongs to `<prefix>/accounts/<any>/devices/<target_device_id>/...`.
pub fn key_belongs_to_device(prefix: &str, key: &str, target_device_id: &str) -> bool {
    let Ok(norm) = validate_normalized_prefix(prefix) else {
        return false;
    };
    let Ok(valid_key) = validate_relative_key(key) else {
        return false;
    };
    let expected_root = format!("{norm}/accounts/");
    let Some(rest) = valid_key.strip_prefix(&expected_root) else {
        return false;
    };
    let mut parts = rest.split('/');
    let Some(acct_key) = parts.next() else {
        return false;
    };
    let Some(devices_lit) = parts.next() else {
        return false;
    };
    let Some(dev_id) = parts.next() else {
        return false;
    };
    let Some(_tail) = parts.next() else {
        return false;
    };
    devices_lit == "devices" && decode_account_key(acct_key).is_ok() && dev_id == target_device_id
}

/// Quota snapshot payload embedded inside `DeviceManifest`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestQuotaSnapshot {
    pub observed_at_ms: i64,
    pub quota: ProviderQuotaSummary,
}

/// Metadata summary for a single monthly `.jsonl.gz` chunk inside `DeviceManifest`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkMeta {
    pub sha256: String,
    pub step_count: u64,
    pub bytes: u64,
}

/// Per-(account, device) remote manifest stored at `manifest.json` (Wire Format v1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceManifest {
    pub format_version: u32,
    pub account_id: String,
    pub device_id: String,
    pub device_name: String,
    #[serde(default)]
    pub plan_tier: Option<String>,
    pub updated_at_ms: i64,
    #[serde(default)]
    pub quota_snapshot: Option<ManifestQuotaSnapshot>,
    #[serde(default)]
    pub chunks: BTreeMap<String, ChunkMeta>,
}

/// Privacy-minimized JSONL record inside `chunks/<YYYY-MM>.jsonl.gz`.
/// Intentionally omits `db_path`, `sig`, `workspace`, and `branch`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ChunkRecord {
    Conv {
        conversation_id: String,
        created_at_ms: Option<i64>,
        cascade_id: String,
        parent_conversation_id: Option<String>,
        root_conversation_id: Option<String>,
        source: String,
    },
    Step {
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
        source: String,
    },
}

/// Compute lowercase hex SHA-256 of an arbitrary byte slice.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[derive(Serialize)]
struct ManifestSemanticProjection<'a> {
    format_version: u32,
    account_id: &'a str,
    device_id: &'a str,
    device_name: &'a str,
    plan_tier: &'a Option<String>,
    quota_snapshot: &'a Option<ManifestQuotaSnapshot>,
    chunks: &'a BTreeMap<String, ChunkMeta>,
}

/// Compute a deterministic SHA-256 of `DeviceManifest` semantic fields (excluding `updated_at_ms`)
/// so repeated pushes with no local changes skip re-uploading `manifest.json`.
pub fn manifest_semantic_sha256(manifest: &DeviceManifest) -> Result<String, SyncError> {
    let proj = ManifestSemanticProjection {
        format_version: manifest.format_version,
        account_id: &manifest.account_id,
        device_id: &manifest.device_id,
        device_name: &manifest.device_name,
        plan_tier: &manifest.plan_tier,
        quota_snapshot: &manifest.quota_snapshot,
        chunks: &manifest.chunks,
    };
    let json = serde_json::to_vec(&proj)?;
    Ok(sha256_hex(&json))
}

/// Deterministically sort `records` (`Conv(conversation_id ASC)` then `Step(step_uid ASC)`),
/// serialize as JSONL, gzip-compress with `mtime(0)`, and return `(gz_bytes, sha256_hex)`.
pub fn encode_chunk_gz(records: &[ChunkRecord]) -> Result<(Vec<u8>, String), SyncError> {
    let mut sorted: Vec<&ChunkRecord> = records.iter().collect();
    sorted.sort_by(|a, b| match (a, b) {
        (
            ChunkRecord::Conv {
                conversation_id: ca,
                ..
            },
            ChunkRecord::Conv {
                conversation_id: cb,
                ..
            },
        ) => ca.cmp(cb),
        (ChunkRecord::Conv { .. }, ChunkRecord::Step { .. }) => std::cmp::Ordering::Less,
        (ChunkRecord::Step { .. }, ChunkRecord::Conv { .. }) => std::cmp::Ordering::Greater,
        (ChunkRecord::Step { step_uid: sa, .. }, ChunkRecord::Step { step_uid: sb, .. }) => {
            sa.cmp(sb)
        }
    });

    let mut encoder = GzBuilder::new()
        .mtime(0)
        .operating_system(255)
        .write(Vec::new(), Compression::default());

    for rec in sorted {
        serde_json::to_writer(&mut encoder, rec)?;
        encoder.write_all(b"\n")?;
    }

    let gz_bytes = encoder.finish()?;
    let digest = sha256_hex(&gz_bytes);
    Ok((gz_bytes, digest))
}

/// Decompress gzipped JSONL chunk bytes with a strict `64 MiB` decompressed size cap
/// and parse each non-empty line into `ChunkRecord`.
pub fn decode_chunk_gz(gz_bytes: &[u8]) -> Result<Vec<ChunkRecord>, SyncError> {
    let decoder = GzDecoder::new(gz_bytes);
    let mut limited = decoder.take((MAX_DECOMPRESSED_CHUNK_BYTES + 1) as u64);
    let mut raw = Vec::new();
    limited.read_to_end(&mut raw)?;

    if raw.len() > MAX_DECOMPRESSED_CHUNK_BYTES {
        return Err(SyncError::Protocol(format!(
            "decompressed chunk exceeds {} byte safety limit",
            MAX_DECOMPRESSED_CHUNK_BYTES
        )));
    }

    let text = String::from_utf8(raw)
        .map_err(|e| SyncError::Protocol(format!("chunk JSONL is not valid UTF-8: {e}")))?;

    let mut out = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let rec: ChunkRecord = serde_json::from_str(trimmed).map_err(|e| {
            SyncError::Protocol(format!("invalid chunk record on line {}: {e}", idx + 1))
        })?;
        out.push(rec);
    }
    Ok(out)
}
