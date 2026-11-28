use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::quota::ProviderQuotaSummary;

/// Sentinel account identifier used when a conversation or device has not been assigned an email.
pub const UNASSIGNED_ACCOUNT_ID: &str = "unassigned";

fn default_sync_source() -> String {
    "cli".to_string()
}

/// Normalize a raw account identifier (email) by trimming whitespace and lowercasing ASCII.
/// Returns `None` if the trimmed input is empty.
pub fn normalize_account_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_ascii_lowercase())
    }
}

/// Immutable device identity paired with a user-facing device name and locality flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceIdentity {
    pub device_id: String,
    pub device_name: String,
    #[serde(default)]
    pub is_local: bool,
}

/// Persisted device record stored in `sync_devices`, scoped per account and device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncDeviceRecord {
    pub account_id: String,
    pub device_id: String,
    pub device_name: String,
    pub is_local: bool,
    pub last_seen_ms: i64,
    pub plan_tier: Option<String>,
}

/// Cross-device quota observation snapshot bound to an account and reporting device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaSnapshot {
    pub account_id: String,
    pub device_id: String,
    pub device_name: String,
    #[serde(default)]
    pub plan_tier: Option<String>,
    pub observed_at_ms: i64,
    #[serde(default)]
    pub quota: ProviderQuotaSummary,
}

/// Pure-value conversation metadata projection exchanged between `ledger` and `sync`.
/// Intentionally omits host-local paths, file signatures, `workspace`, and `branch`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncConvRecord {
    pub conversation_id: String,
    #[serde(default)]
    pub created_at_ms: Option<i64>,
    pub cascade_id: String,
    #[serde(default)]
    pub parent_conversation_id: Option<String>,
    #[serde(default)]
    pub root_conversation_id: Option<String>,
    #[serde(default = "default_sync_source")]
    pub source: String,
}

/// Pure-value telemetry step projection exchanged between `ledger` and `sync`.
/// Intentionally omits `workspace` and `branch`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncStepRecord {
    pub step_uid: String,
    pub conversation_id: String,
    pub gen_idx: i64,
    pub entry_idx: i64,
    #[serde(default)]
    pub step_index: Option<i64>,
    pub ts_ms: i64,
    pub model: String,
    #[serde(default)]
    pub raw_model: String,
    #[serde(default)]
    pub input_tokens: i64,
    #[serde(default)]
    pub cache_read_tokens: i64,
    #[serde(default)]
    pub cache_write_tokens: i64,
    #[serde(default)]
    pub output_tokens: i64,
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub stream_sec: Option<f64>,
    #[serde(default)]
    pub ttft_sec: Option<f64>,
    #[serde(default)]
    pub valid_stream: i64,
    #[serde(default)]
    pub valid_ttft: i64,
    #[serde(default)]
    pub parent_conversation_id: Option<String>,
    #[serde(default)]
    pub root_conversation_id: Option<String>,
    #[serde(default = "default_sync_source")]
    pub source: String,
}

/// Monthly bucket of local conversations and steps exported for a single account.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LocalMonthlyExport {
    pub convs: Vec<SyncConvRecord>,
    pub steps: Vec<SyncStepRecord>,
}

/// Snapshot of multi-device sync metadata keys stored in `ledger_meta`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerSyncMeta {
    pub last_sync_push_ms: Option<i64>,
    pub last_sync_pull_ms: Option<i64>,
    pub last_sync_error: Option<String>,
    pub last_sync_remote_fingerprint: Option<String>,
}

/// Map of UTC `YYYY-MM` month keys to exported local conversations and steps.
pub type LocalMonthlyExportMap = BTreeMap<String, LocalMonthlyExport>;

/// Pure read-only summary of `agy-pulse-sync.json` for ledger status display and error redaction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncConfigSummary {
    pub enabled: bool,
    pub remote_type: Option<String>,
    pub secrets: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum RawRemoteSummary {
    LocalDir {},
    #[serde(rename = "webdav", alias = "web-dav")]
    WebDav {
        #[serde(default)]
        password: Option<String>,
        #[serde(default)]
        token: Option<String>,
    },
    S3 {
        #[serde(default)]
        access_key: Option<String>,
        #[serde(default)]
        secret_key: Option<String>,
    },
    CfWorker {
        #[serde(default)]
        token: Option<String>,
    },
}

impl Default for RawRemoteSummary {
    fn default() -> Self {
        Self::LocalDir {}
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawSyncConfigSummaryFile {
    enabled: bool,
    remote: RawRemoteSummary,
}

fn push_trimmed_secret(secrets: &mut Vec<String>, val: Option<&str>, bearer: bool) {
    if let Some(trimmed) = val.map(str::trim).filter(|s| !s.is_empty()) {
        secrets.push(trimmed.to_string());
        if bearer {
            secrets.push(format!("Bearer {trimmed}"));
        }
    }
}

/// Parse a read-only `SyncConfigSummary` from raw `agy-pulse-sync.json` text without I/O.
pub fn parse_sync_config_summary(raw_json: &str) -> Result<SyncConfigSummary, serde_json::Error> {
    let parsed: RawSyncConfigSummaryFile = serde_json::from_str(raw_json)?;
    let mut secrets = Vec::new();
    let remote_type = match &parsed.remote {
        RawRemoteSummary::LocalDir {} => "local-dir",
        RawRemoteSummary::WebDav { password, token } => {
            push_trimmed_secret(&mut secrets, password.as_deref(), false);
            push_trimmed_secret(&mut secrets, token.as_deref(), true);
            "webdav"
        }
        RawRemoteSummary::S3 {
            access_key,
            secret_key,
        } => {
            push_trimmed_secret(&mut secrets, access_key.as_deref(), false);
            push_trimmed_secret(&mut secrets, secret_key.as_deref(), false);
            "s3"
        }
        RawRemoteSummary::CfWorker { token } => {
            push_trimmed_secret(&mut secrets, token.as_deref(), true);
            "cf-worker"
        }
    };
    Ok(SyncConfigSummary {
        enabled: parsed.enabled,
        remote_type: Some(remote_type.to_string()),
        secrets,
    })
}

/// Strip `user:pass@` userinfo from any URLs embedded inside diagnostic/error text.
pub fn strip_url_userinfo_in_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(idx) = rest.find("://") {
        let after_scheme = &rest[idx + 3..];
        let auth_end = after_scheme
            .find(|c: char| c == '/' || c == '?' || c == '#' || c.is_whitespace())
            .unwrap_or(after_scheme.len());
        let authority = &after_scheme[..auth_end];
        if let Some(at_idx) = authority.rfind('@') {
            out.push_str(&rest[..idx + 3]);
            out.push_str(&authority[at_idx + 1..]);
            rest = &after_scheme[auth_end..];
        } else {
            out.push_str(&rest[..idx + 3]);
            rest = after_scheme;
        }
    }
    out.push_str(rest);
    out
}

/// Pure credential scrubbing: strips URL userinfo, masks `secrets` with `[REDACTED]`,
/// drops control characters, and caps output at 512 chars.
pub fn redact_sync_secrets(msg: &str, secrets: &[String]) -> String {
    let mut out = strip_url_userinfo_in_text(msg);
    for secret in secrets {
        if !secret.is_empty() {
            out = out.replace(secret, "[REDACTED]");
        }
    }
    out.chars().filter(|c| !c.is_control()).take(512).collect()
}
