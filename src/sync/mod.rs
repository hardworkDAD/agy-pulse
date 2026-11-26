pub mod cf_worker;
pub mod config;
pub mod engine;
pub mod http_util;
pub mod local_dir;
pub mod protocol;
pub mod s3;
pub mod webdav;

use serde::{Deserialize, Serialize};

pub use cf_worker::CfWorkerRemote;
pub use config::{
    RemoteConfig, SyncConfig, load_sync_config, load_sync_config_from, save_sync_config,
    save_sync_config_to,
};
pub use engine::{
    PruneOutcome, SyncOutcome, SyncOutcomeKind, prune_device, pull, push, sync_both,
};
pub use local_dir::LocalDirRemote;
pub use protocol::{
    ChunkMeta, ChunkRecord, DeviceManifest, ManifestKeyParts, ManifestQuotaSnapshot,
    decode_account_key, decode_chunk_gz, encode_account_key, encode_chunk_gz,
    validate_relative_key,
};
pub use s3::S3Remote;
pub use webdav::WebDavRemote;

/// Result returned by `SyncRemote::probe()` describing the reachable remote backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProbeInfo {
    pub backend: String,
    pub detail: String,
}

/// Unified error type for global sync configuration, wire protocol, and remote backends.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("Sync configuration error: {0}")]
    Config(String),
    #[error("Invalid sync object key: {0}")]
    InvalidKey(String),
    #[error("Invalid sync protocol payload: {0}")]
    Protocol(String),
    #[error("Unsupported remote backend: {0}")]
    UnsupportedRemote(String),
    #[error("Sync lock is held by another running sync process")]
    AlreadyRunning,
    #[error("Sync I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Sync JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Ledger error during sync: {0}")]
    Ledger(#[from] crate::ledger::LedgerError),
    #[error("Remote backend error: {0}")]
    Remote(String),
}

/// Uniform object-storage contract implemented by every sync remote adapter.
pub trait SyncRemote: Send + Sync {
    fn probe(&self) -> Result<RemoteProbeInfo, SyncError>;
    fn list(&self, prefix: &str) -> Result<Vec<String>, SyncError>;
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, SyncError>;
    fn put(&self, key: &str, bytes: &[u8], content_type: &str) -> Result<(), SyncError>;
    fn delete(&self, key: &str) -> Result<(), SyncError>;
}

/// Construct the concrete `SyncRemote` backend configured in `cfg`.
pub fn build_remote(cfg: &SyncConfig) -> Result<Box<dyn SyncRemote>, SyncError> {
    match &cfg.remote {
        RemoteConfig::LocalDir { path } => {
            let trimmed = path.trim();
            if trimmed.is_empty() {
                return Err(SyncError::Config(
                    "local-dir remote path must not be empty".to_string(),
                ));
            }
            Ok(Box::new(LocalDirRemote::new(std::path::PathBuf::from(
                trimmed,
            ))))
        }
        RemoteConfig::WebDav {
            url,
            username,
            password,
            token,
            allow_insecure_http,
        } => Ok(Box::new(WebDavRemote::new(
            url,
            username.as_deref(),
            password.as_deref(),
            token.as_deref(),
            *allow_insecure_http,
        )?)),
        RemoteConfig::S3 {
            endpoint,
            region,
            bucket,
            access_key,
            secret_key,
            path_style,
            allow_insecure_http,
        } => Ok(Box::new(S3Remote::new(
            endpoint,
            region,
            bucket,
            access_key.as_deref(),
            secret_key.as_deref(),
            *path_style,
            *allow_insecure_http,
        )?)),
        RemoteConfig::CfWorker {
            url,
            token,
            allow_insecure_http,
        } => Ok(Box::new(CfWorkerRemote::new(
            url,
            token.as_deref(),
            *allow_insecure_http,
        )?)),
    }
}
