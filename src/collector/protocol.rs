use std::collections::HashMap;
use std::fs;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::config::PulseConfig;
use crate::domain::PulseSnapshot;
use crate::platform;

pub const COLLECTOR_APP_ID: &str = "agy-pulse-collector";
pub const COLLECTOR_STATE_FILENAME: &str = "collector.json";
pub const COLLECTOR_LOCK_FILENAME: &str = "collector.lock";
pub const COLLECTOR_LOCK_TTL_MS: u64 = 10_000;
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// Persisted discovery and authentication metadata for the active collector daemon.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CollectorStateFile {
    pub app: String,
    pub pid: u32,
    pub version: String,
    pub exe_mtime_ms: i64,
    pub mode: String,
    pub socket_path: Option<String>,
    pub tcp_port: Option<u16>,
    pub token: String,
    pub started_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CollectorLockPayload {
    #[serde(default)]
    pid: u32,
    #[serde(default)]
    ts: i64,
}

/// Request payload for a single statusline enrichment cycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectorRequest {
    pub token: String,
    pub stdin_text: String,
    pub fallback_cwd: PathBuf,
    pub stdin_ms: u64,
    pub conversation_key: String,
    pub has_warm_snapshot: bool,
    pub wait_ms: u64,
    pub soft_budget_ms: u64,
    pub env_overrides: HashMap<String, Option<String>>,
}

/// IPC request envelope sent from thin statusline frontends or control callers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CollectorWireRequest {
    Ping { token: String },
    Shutdown { token: String },
    Collect(CollectorRequest),
}

impl CollectorWireRequest {
    pub fn token(&self) -> &str {
        match self {
            Self::Ping { token } | Self::Shutdown { token } => token.as_str(),
            Self::Collect(req) => req.token.as_str(),
        }
    }
}

/// Payload returned when a statusline enrichment job completes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectorReadyPayload {
    pub enabled: bool,
    pub snapshot: PulseSnapshot,
    pub config: PulseConfig,
    pub summary: String,
}

/// IPC response envelope returned by the collector daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CollectorWireResponse {
    Pong {
        pid: u32,
        version: String,
        mode: String,
        worker_busy: bool,
    },
    Ack,
    Busy,
    Ready(Box<CollectorReadyPayload>),
    Error(String),
}

/// Cross-platform local IPC stream (Unix domain socket or Windows loopback TCP).
pub enum IpcStream {
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
    #[cfg(windows)]
    Tcp(std::net::TcpStream),
}

impl IpcStream {
    pub fn set_read_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(s) => s.set_read_timeout(dur),
            #[cfg(windows)]
            Self::Tcp(s) => s.set_read_timeout(dur),
        }
    }

    pub fn set_write_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(s) => s.set_write_timeout(dur),
            #[cfg(windows)]
            Self::Tcp(s) => s.set_write_timeout(dur),
        }
    }
}

impl Read for IpcStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            Self::Unix(s) => s.read(buf),
            #[cfg(windows)]
            Self::Tcp(s) => s.read(buf),
        }
    }
}

impl Write for IpcStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            Self::Unix(s) => s.write(buf),
            #[cfg(windows)]
            Self::Tcp(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(s) => s.flush(),
            #[cfg(windows)]
            Self::Tcp(s) => s.flush(),
        }
    }
}

/// Connect to the IPC endpoint described by `state` within `connect_timeout`.
pub fn connect_to_state(state: &CollectorStateFile, connect_timeout: Duration) -> io::Result<IpcStream> {
    #[cfg(unix)]
    {
        let _ = connect_timeout;
        let path_str = state
            .socket_path
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing socket_path"))?;
        let stream = std::os::unix::net::UnixStream::connect(path_str)?;
        Ok(IpcStream::Unix(stream))
    }
    #[cfg(windows)]
    {
        let port = state
            .tcp_port
            .filter(|&p| p > 0)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing tcp_port"))?;
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let stream = std::net::TcpStream::connect_timeout(&addr, connect_timeout.max(Duration::from_millis(1)))?;
        let _ = stream.set_nodelay(true);
        Ok(IpcStream::Tcp(stream))
    }
}

/// Write a 4-byte big-endian length-prefixed JSON frame.
pub fn write_frame<W: Write, T: Serialize>(w: &mut W, msg: &T) -> io::Result<()> {
    let payload = serde_json::to_vec(msg)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("IPC frame exceeds {MAX_FRAME_BYTES} bytes"),
        ));
    }
    let len_u32 = u32::try_from(payload.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("IPC frame length {} exceeds u32::MAX", payload.len()),
        )
    })?;
    let len_be = len_u32.to_be_bytes();
    w.write_all(&len_be)?;
    w.write_all(&payload)?;
    w.flush()
}

/// Read a 4-byte big-endian length-prefixed JSON frame.
pub fn read_frame<R: Read, T: DeserializeOwned>(r: &mut R) -> io::Result<T> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 || len > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Invalid IPC frame length {len}"),
        ));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    serde_json::from_slice(&buf).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

/// Return `<runtime_dir>/collector.json`.
pub fn collector_state_path() -> PathBuf {
    platform::resolve_runtime_dir().join(COLLECTOR_STATE_FILENAME)
}

/// Return `<runtime_dir>/collector.lock`.
pub fn collector_lock_path() -> PathBuf {
    platform::resolve_runtime_dir().join(COLLECTOR_LOCK_FILENAME)
}

/// Read and parse `collector.json` if valid.
pub fn read_collector_state() -> Option<CollectorStateFile> {
    let raw = fs::read_to_string(collector_state_path()).ok()?;
    let parsed: CollectorStateFile = serde_json::from_str(&raw).ok()?;
    if parsed.app != COLLECTOR_APP_ID || parsed.pid == 0 || parsed.token.is_empty() {
        return None;
    }
    Some(parsed)
}

/// Atomically persist `collector.json` with `0600` permissions.
pub fn write_collector_state(state: &CollectorStateFile) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(state)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    platform::write_cache_atomic_0600(&collector_state_path(), &bytes)
}

/// Remove `collector.json` only when both `pid` and `token` match the caller.
/// Returns `true` when this owner actually removed its own state file.
pub fn clear_collector_state_if_owner(owner_pid: u32, expected_token: &str) -> bool {
    if owner_pid == 0 || expected_token.is_empty() {
        return false;
    }
    let Some(current) = read_collector_state() else {
        return false;
    };
    if current.pid != owner_pid || current.token != expected_token {
        return false;
    }
    fs::remove_file(collector_state_path()).is_ok()
}

/// Send a `Shutdown` control frame to an existing collector instance.
pub fn send_shutdown_to_state(state: &CollectorStateFile, timeout: Duration) -> bool {
    let Ok(mut stream) = connect_to_state(state, timeout) else {
        return false;
    };
    let _ = stream.set_write_timeout(Some(timeout));
    let _ = stream.set_read_timeout(Some(timeout));
    let req = CollectorWireRequest::Shutdown {
        token: state.token.clone(),
    };
    if write_frame(&mut stream, &req).is_err() {
        return false;
    }
    matches!(
        read_frame::<_, CollectorWireResponse>(&mut stream),
        Ok(CollectorWireResponse::Ack)
    )
}

fn current_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Acquire an exclusive spawn lock (`collector.lock`) to prevent thundering-herd spawns.
pub fn acquire_collector_spawn_lock(ttl_ms: u64) -> bool {
    let lock_path = collector_lock_path();
    let payload = CollectorLockPayload {
        pid: std::process::id(),
        ts: current_epoch_ms(),
    };
    let Ok(bytes) = serde_json::to_vec(&payload) else {
        return false;
    };

    for _attempt in 0..2 {
        match platform::create_exclusive_file_0600(&lock_path, &bytes) {
            Ok(true) => return true,
            Ok(false) => {
                let meta = match fs::metadata(&lock_path) {
                    Ok(m) => m,
                    Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                    Err(_) => return false,
                };
                let Ok(modified) = meta.modified() else {
                    return false;
                };
                let age_ms = SystemTime::now()
                    .duration_since(modified)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let holder_alive = fs::read_to_string(&lock_path)
                    .ok()
                    .and_then(|raw| serde_json::from_str::<CollectorLockPayload>(&raw).ok())
                    .is_some_and(|p| p.pid > 0 && platform::is_process_alive(p.pid));
                if age_ms < ttl_ms && holder_alive {
                    return false;
                }
                if fs::remove_file(&lock_path).is_err() {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
    false
}

/// Remove `collector.lock` only if owned by the current process PID.
pub fn release_collector_spawn_lock_if_owner() {
    let lock_path = collector_lock_path();
    let Ok(raw) = fs::read_to_string(&lock_path) else {
        return;
    };
    let Ok(parsed) = serde_json::from_str::<CollectorLockPayload>(&raw) else {
        return;
    };
    if parsed.pid == std::process::id() {
        let _ = fs::remove_file(&lock_path);
    }
}

/// Unconditionally remove `collector.lock` (used by the spawned `--collector-bg` child once ready).
pub fn release_collector_spawn_lock() -> bool {
    fs::remove_file(collector_lock_path()).is_ok()
}

/// Return `true` when both executable mtimes are positive and equal, or when either is unavailable (`<= 0`).
pub(super) fn exe_mtime_matches(existing_mtime_ms: i64, current_mtime_ms: i64) -> bool {
    if existing_mtime_ms <= 0 || current_mtime_ms <= 0 {
        return true;
    }
    existing_mtime_ms == current_mtime_ms
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{CollectorWireResponse, MAX_FRAME_BYTES, exe_mtime_matches, read_frame, write_frame};

    #[test]
    fn exe_mtime_matches_treats_non_positive_as_compatible_and_positive_as_strict() {
        assert!(exe_mtime_matches(0, 1_700_000_000_000));
        assert!(exe_mtime_matches(1_700_000_000_000, 0));
        assert!(exe_mtime_matches(-1, 0));
        assert!(exe_mtime_matches(1_700_000_000_000, 1_700_000_000_000));
        assert!(!exe_mtime_matches(1_700_000_000_000, 1_700_000_000_001));
    }

    #[test]
    fn write_frame_rejects_oversized_payload_and_roundtrips_valid_frame() {
        let mut buf = Vec::new();
        let oversized = CollectorWireResponse::Error("a".repeat(MAX_FRAME_BYTES + 1));
        let Err(err) = write_frame(&mut buf, &oversized) else {
            panic!("oversized frame must fail");
        };
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(buf.is_empty());

        if let Err(err) = write_frame(&mut buf, &CollectorWireResponse::Ack) {
            panic!("write_frame valid frame failed: {err}");
        }
        let mut cursor = Cursor::new(buf);
        let decoded: CollectorWireResponse = match read_frame(&mut cursor) {
            Ok(d) => d,
            Err(err) => panic!("read_frame valid frame failed: {err}"),
        };
        assert!(matches!(decoded, CollectorWireResponse::Ack));
    }
}
