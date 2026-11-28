use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use url::Url;

use crate::domain::{UNASSIGNED_ACCOUNT_ID, normalize_account_id};
use crate::platform;

use super::SyncError;
use super::protocol::normalize_prefix;

pub const DEFAULT_PUSH_INTERVAL_SEC: u64 = 300;
pub const DEFAULT_SYNC_PREFIX: &str = "agy-pulse-sync";

fn default_push_interval_sec() -> u64 {
    DEFAULT_PUSH_INTERVAL_SEC
}

fn default_prefix() -> String {
    DEFAULT_SYNC_PREFIX.to_string()
}

fn default_s3_region() -> String {
    "auto".to_string()
}

fn default_true() -> bool {
    true
}

fn redact_opt(opt: &Option<String>) -> &'static str {
    match opt.as_deref() {
        Some(s) if !s.is_empty() => "[REDACTED]",
        _ => "<none>",
    }
}

/// Strip credentials (`user:pass@`) and query/fragment from a URL for safe display/fingerprinting.
pub(crate) fn sanitize_url_for_display(raw: &str) -> String {
    let trimmed = raw.trim();
    let Ok(mut parsed) = Url::parse(trimmed) else {
        return strip_url_userinfo_fallback(trimmed);
    };
    if !parsed.username().is_empty() || parsed.password().is_some() {
        let _ = parsed.set_username("");
        let _ = parsed.set_password(None);
    }
    parsed.set_query(None);
    parsed.set_fragment(None);
    let rendered = parsed.to_string();
    rendered.trim_end_matches('/').to_string()
}

fn strip_url_userinfo_fallback(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if let Some(scheme_idx) = trimmed.find("://") {
        let after_scheme = &trimmed[scheme_idx + 3..];
        let authority_end = after_scheme.find('/').unwrap_or(after_scheme.len());
        let authority = &after_scheme[..authority_end];
        if let Some(at_idx) = authority.rfind('@') {
            let prefix = &trimmed[..scheme_idx + 3];
            let host_and_rest = &after_scheme[at_idx + 1..];
            return format!("{prefix}{host_and_rest}");
        }
    }
    trimmed.to_string()
}

/// Configuration for the single active sync remote backend.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum RemoteConfig {
    LocalDir {
        path: String,
    },
    #[serde(rename = "webdav", alias = "web-dav")]
    WebDav {
        url: String,
        #[serde(default)]
        username: Option<String>,
        #[serde(default)]
        password: Option<String>,
        #[serde(default)]
        token: Option<String>,
        #[serde(default)]
        allow_insecure_http: bool,
    },
    S3 {
        endpoint: String,
        #[serde(default = "default_s3_region")]
        region: String,
        bucket: String,
        #[serde(default)]
        access_key: Option<String>,
        #[serde(default)]
        secret_key: Option<String>,
        #[serde(default = "default_true")]
        path_style: bool,
        #[serde(default)]
        allow_insecure_http: bool,
    },
    CfWorker {
        url: String,
        #[serde(default)]
        token: Option<String>,
        #[serde(default)]
        allow_insecure_http: bool,
    },
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self::LocalDir {
            path: String::new(),
        }
    }
}

impl fmt::Debug for RemoteConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LocalDir { path } => f.debug_struct("LocalDir").field("path", path).finish(),
            Self::WebDav {
                url,
                username,
                password,
                token,
                allow_insecure_http,
            } => f
                .debug_struct("WebDav")
                .field("url", &sanitize_url_for_display(url))
                .field("username", username)
                .field("password", &redact_opt(password))
                .field("token", &redact_opt(token))
                .field("allow_insecure_http", allow_insecure_http)
                .finish(),
            Self::S3 {
                endpoint,
                region,
                bucket,
                access_key,
                secret_key,
                path_style,
                allow_insecure_http,
            } => f
                .debug_struct("S3")
                .field("endpoint", &sanitize_url_for_display(endpoint))
                .field("region", region)
                .field("bucket", bucket)
                .field("access_key", &redact_opt(access_key))
                .field("secret_key", &redact_opt(secret_key))
                .field("path_style", path_style)
                .field("allow_insecure_http", allow_insecure_http)
                .finish(),
            Self::CfWorker {
                url,
                token,
                allow_insecure_http,
            } => f
                .debug_struct("CfWorker")
                .field("url", &sanitize_url_for_display(url))
                .field("token", &redact_opt(token))
                .field("allow_insecure_http", allow_insecure_http)
                .finish(),
        }
    }
}

/// Global-only private synchronization configuration (`0600` on Unix).
/// Never read from or written to workspace directories.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SyncConfig {
    pub enabled: bool,
    pub device_id: Option<String>,
    pub device_name: Option<String>,
    pub default_account: Option<String>,
    #[serde(default = "default_push_interval_sec")]
    pub push_interval_sec: u64,
    #[serde(default = "default_prefix")]
    pub prefix: String,
    pub remote: RemoteConfig,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            device_id: None,
            device_name: None,
            default_account: None,
            push_interval_sec: DEFAULT_PUSH_INTERVAL_SEC,
            prefix: DEFAULT_SYNC_PREFIX.to_string(),
            remote: RemoteConfig::default(),
        }
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    platform::env_var(name)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn non_empty_env_raw(name: &str) -> Option<String> {
    platform::env_var(name).filter(|v| !v.is_empty())
}

fn clean_opt_string(opt: Option<String>) -> Option<String> {
    opt.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn non_empty_opt_string(opt: Option<String>) -> Option<String> {
    opt.filter(|s| !s.is_empty())
}

impl SyncConfig {
    /// Normalize scalar fields, strip leading/trailing `/` from `prefix`, and clamp `push_interval_sec >= 1`.
    pub fn normalize(&mut self) {
        self.device_id = clean_opt_string(self.device_id.take());
        self.device_name = self
            .device_name
            .take()
            .and_then(|s| platform::sanitize_device_name(&s));
        self.default_account = self
            .default_account
            .take()
            .and_then(|s| normalize_account_id(&s))
            .filter(|s| s != UNASSIGNED_ACCOUNT_ID);
        self.push_interval_sec = self.push_interval_sec.max(1);
        self.prefix = normalize_prefix(&self.prefix);

        match &mut self.remote {
            RemoteConfig::LocalDir { path } => {
                *path = path.trim().to_string();
            }
            RemoteConfig::WebDav {
                url,
                username,
                password,
                token,
                ..
            } => {
                *url = url.trim().to_string();
                *username = clean_opt_string(username.take());
                *password = non_empty_opt_string(password.take());
                *token = clean_opt_string(token.take());
            }
            RemoteConfig::S3 {
                endpoint,
                region,
                bucket,
                access_key,
                secret_key,
                ..
            } => {
                *endpoint = endpoint.trim().to_string();
                let trimmed_region = region.trim();
                *region = if trimmed_region.is_empty() {
                    default_s3_region()
                } else {
                    trimmed_region.to_string()
                };
                *bucket = bucket.trim().to_string();
                *access_key = clean_opt_string(access_key.take());
                *secret_key = clean_opt_string(secret_key.take());
            }
            RemoteConfig::CfWorker { url, token, .. } => {
                *url = url.trim().to_string();
                *token = clean_opt_string(token.take());
            }
        }
    }

    /// Override credentials from environment variables when non-empty.
    pub fn apply_env_overrides(&mut self) {
        let env_token = non_empty_env("AGY_PULSE_SYNC_TOKEN");
        let env_password = non_empty_env_raw("AGY_PULSE_SYNC_PASSWORD");
        let env_s3_access = non_empty_env("AGY_PULSE_S3_ACCESS_KEY");
        let env_s3_secret = non_empty_env("AGY_PULSE_S3_SECRET_KEY");

        match &mut self.remote {
            RemoteConfig::LocalDir { .. } => {}
            RemoteConfig::WebDav {
                password, token, ..
            } => {
                if let Some(ref p) = env_password {
                    *password = Some(p.clone());
                }
                if let Some(ref t) = env_token {
                    *token = Some(t.clone());
                }
            }
            RemoteConfig::S3 {
                access_key,
                secret_key,
                ..
            } => {
                if let Some(ref ak) = env_s3_access {
                    *access_key = Some(ak.clone());
                }
                if let Some(ref sk) = env_s3_secret {
                    *secret_key = Some(sk.clone());
                }
            }
            RemoteConfig::CfWorker { token, .. } => {
                if let Some(ref t) = env_token {
                    *token = Some(t.clone());
                }
            }
        }
    }

    /// Return the static kebab-case remote backend identifier.
    pub fn remote_type_str(&self) -> &'static str {
        match self.remote {
            RemoteConfig::LocalDir { .. } => "local-dir",
            RemoteConfig::WebDav { .. } => "webdav",
            RemoteConfig::S3 { .. } => "s3",
            RemoteConfig::CfWorker { .. } => "cf-worker",
        }
    }

    /// Project a read-only `SyncConfigSummary` for ledger status and error redaction.
    pub fn to_summary(&self) -> crate::domain::SyncConfigSummary {
        let mut secrets = Vec::new();
        let push_secret = |buf: &mut Vec<String>, opt: &Option<String>, bearer: bool| {
            if let Some(trimmed) = opt.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                buf.push(trimmed.to_string());
                if bearer {
                    buf.push(format!("Bearer {trimmed}"));
                }
            }
        };
        match &self.remote {
            RemoteConfig::LocalDir { .. } => {}
            RemoteConfig::WebDav {
                password, token, ..
            } => {
                push_secret(&mut secrets, password, false);
                push_secret(&mut secrets, token, true);
            }
            RemoteConfig::S3 {
                access_key,
                secret_key,
                ..
            } => {
                push_secret(&mut secrets, access_key, false);
                push_secret(&mut secrets, secret_key, false);
            }
            RemoteConfig::CfWorker { token, .. } => {
                push_secret(&mut secrets, token, true);
            }
        }
        crate::domain::SyncConfigSummary {
            enabled: self.enabled,
            remote_type: Some(self.remote_type_str().to_string()),
            secrets,
        }
    }

    /// Return a redacted human-readable target summary containing zero credentials.
    pub fn redacted_target(&self) -> String {
        let norm_prefix = normalize_prefix(&self.prefix);
        match &self.remote {
            RemoteConfig::LocalDir { path } => {
                let cleaned = path.trim();
                format!("local-dir:{cleaned} (prefix={norm_prefix})")
            }
            RemoteConfig::WebDav { url, .. } => {
                format!(
                    "webdav:{} (prefix={norm_prefix})",
                    sanitize_url_for_display(url)
                )
            }
            RemoteConfig::S3 {
                endpoint,
                region,
                bucket,
                ..
            } => {
                format!(
                    "s3:{}/{} (region={}, prefix={norm_prefix})",
                    sanitize_url_for_display(endpoint),
                    bucket.trim(),
                    region.trim()
                )
            }
            RemoteConfig::CfWorker { url, .. } => {
                format!(
                    "cf-worker:{} (prefix={norm_prefix})",
                    sanitize_url_for_display(url)
                )
            }
        }
    }

    /// Compute a stable SHA-256 fingerprint of remote identity + prefix (excluding all secrets).
    pub fn remote_fingerprint(&self) -> String {
        let norm_prefix = normalize_prefix(&self.prefix);
        let canonical = match &self.remote {
            RemoteConfig::LocalDir { path } => {
                let trimmed = path.trim();
                let norm_path = if trimmed == "/" {
                    "/".to_string()
                } else {
                    trimmed.trim_end_matches(['/', '\\']).to_string()
                };
                format!("local-dir|{norm_path}|{norm_prefix}")
            }
            RemoteConfig::WebDav { url, .. } => {
                format!("webdav|{}|{norm_prefix}", sanitize_url_for_display(url))
            }
            RemoteConfig::S3 {
                endpoint,
                region,
                bucket,
                path_style,
                ..
            } => {
                let ep = sanitize_url_for_display(endpoint);
                format!(
                    "s3|{ep}|{}|{}|{path_style}|{norm_prefix}",
                    region.trim(),
                    bucket.trim()
                )
            }
            RemoteConfig::CfWorker { url, .. } => {
                format!("cf-worker|{}|{norm_prefix}", sanitize_url_for_display(url))
            }
        };

        let digest = Sha256::digest(canonical.as_bytes());
        hex::encode(digest)
    }
}

/// Names of credential environment variables currently overriding sync configuration.
/// Never contains secret values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncEnvOverrides {
    pub password: Option<&'static str>,
    pub token: Option<&'static str>,
    pub access_key: Option<&'static str>,
    pub secret_key: Option<&'static str>,
}

/// Detect which sync credential environment variables are currently active (names only).
pub fn active_sync_env_overrides() -> SyncEnvOverrides {
    SyncEnvOverrides {
        password: non_empty_env_raw("AGY_PULSE_SYNC_PASSWORD").map(|_| "AGY_PULSE_SYNC_PASSWORD"),
        token: non_empty_env("AGY_PULSE_SYNC_TOKEN").map(|_| "AGY_PULSE_SYNC_TOKEN"),
        access_key: non_empty_env("AGY_PULSE_S3_ACCESS_KEY").map(|_| "AGY_PULSE_S3_ACCESS_KEY"),
        secret_key: non_empty_env("AGY_PULSE_S3_SECRET_KEY").map(|_| "AGY_PULSE_S3_SECRET_KEY"),
    }
}

/// Snapshot of the raw global `agy-pulse-sync.json` without applying environment overrides.
/// Tolerates corrupt JSON so callers can still inspect `version` and recover via `PUT`/`DELETE`.
#[derive(Debug, Clone)]
pub struct RawSyncConfigSnapshot {
    pub path: PathBuf,
    pub exists: bool,
    pub version: String,
    pub parse_error: Option<String>,
    pub config: SyncConfig,
}

/// Read a raw sync config snapshot from `path` without applying environment overrides.
pub fn read_raw_sync_config_snapshot_at(path: &Path) -> Result<RawSyncConfigSnapshot, SyncError> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Ok(RawSyncConfigSnapshot {
                path: path.to_path_buf(),
                exists: false,
                version: "absent".to_string(),
                parse_error: None,
                config: SyncConfig::default(),
            });
        }
        Err(err) => return Err(SyncError::Io(err)),
    };

    let version = hex::encode(Sha256::digest(&bytes));
    let (parse_error, config) = match serde_json::from_slice::<SyncConfig>(&bytes) {
        Ok(mut cfg) => {
            cfg.normalize();
            (None, cfg)
        }
        Err(err) => (Some(format!("invalid JSON: {err}")), SyncConfig::default()),
    };

    Ok(RawSyncConfigSnapshot {
        path: path.to_path_buf(),
        exists: true,
        version,
        parse_error,
        config,
    })
}

/// Read the raw global sync config snapshot from `platform::resolve_sync_config_path()`.
pub fn read_raw_sync_config_snapshot() -> Result<RawSyncConfigSnapshot, SyncError> {
    let path = platform::resolve_sync_config_path();
    read_raw_sync_config_snapshot_at(&path)
}

/// Load the raw private sync configuration from `path` without applying environment overrides.
/// Returns `Ok(Some((config, sha256_hex)))` when present, or `Ok(None)` when missing.
pub fn load_sync_config_raw_from(path: &Path) -> Result<Option<(SyncConfig, String)>, SyncError> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(SyncError::Io(err)),
    };
    let version = hex::encode(Sha256::digest(&bytes));
    let mut cfg: SyncConfig = serde_json::from_slice(&bytes)?;
    cfg.normalize();
    Ok(Some((cfg, version)))
}

/// Load the raw global private sync configuration without applying environment overrides.
pub fn load_sync_config_raw() -> Result<Option<(SyncConfig, String)>, SyncError> {
    let path = platform::resolve_sync_config_path();
    load_sync_config_raw_from(&path)
}

/// Delete the sync configuration file at `path`.
/// Returns `Ok(true)` if removed, `Ok(false)` if already absent.
pub fn delete_sync_config_at(path: &Path) -> Result<bool, SyncError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(SyncError::Io(err)),
    }
}

/// Delete the global sync configuration file (`platform::resolve_sync_config_path()`).
pub fn delete_sync_config() -> Result<bool, SyncError> {
    let path = platform::resolve_sync_config_path();
    delete_sync_config_at(&path)
}

/// Load the private sync configuration from `path`, normalizing fields and applying env overrides.
/// Returns `Ok(None)` when the file does not exist.
pub fn load_sync_config_from(path: &Path) -> Result<Option<SyncConfig>, SyncError> {
    let Some((mut cfg, _version)) = load_sync_config_raw_from(path)? else {
        return Ok(None);
    };
    cfg.apply_env_overrides();
    Ok(Some(cfg))
}

/// Load the global private sync configuration from `platform::resolve_sync_config_path()`.
/// Never reads workspace `agy-pulse.config.json`.
pub fn load_sync_config() -> Result<Option<SyncConfig>, SyncError> {
    let path = platform::resolve_sync_config_path();
    load_sync_config_from(&path)
}

/// Atomically persist `cfg` to `path` with `0700` parent directory and `0600` file permissions
/// enforced both before and after `NamedTempFile::persist`.
pub fn save_sync_config_to(path: &Path, cfg: &SyncConfig) -> Result<(), SyncError> {
    let mut normalized = cfg.clone();
    normalized.normalize();

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        platform::ensure_dir_0700(parent)?;
    }

    let parent_dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    let json = serde_json::to_string_pretty(&normalized)?;
    let mut tmp = NamedTempFile::new_in(parent_dir)?;
    platform::set_file_0600(tmp.path())?;
    tmp.write_all(json.as_bytes())?;
    tmp.write_all(b"\n")?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| SyncError::Io(e.error))?;
    platform::set_file_0600(path)?;
    Ok(())
}

/// Atomically persist `cfg` to `platform::resolve_sync_config_path()`.
pub fn save_sync_config(cfg: &SyncConfig) -> Result<PathBuf, SyncError> {
    let path = platform::resolve_sync_config_path();
    save_sync_config_to(&path, cfg)?;
    Ok(path)
}
