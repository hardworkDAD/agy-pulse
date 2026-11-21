use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::config::{LanguageSetting, PulseConfig};
use crate::platform::{self, DesktopOs};

use super::{
    DASHBOARD_APP_ID, DASHBOARD_ICON_SVG, DEFAULT_DASHBOARD_PORT, DEFAULT_IDLE_TIMEOUT_SEC,
    LOOPBACK_HOST,
};

pub const DASHBOARD_STATE_FILENAME: &str = "agy-pulse-dashboard.json";
pub const DASHBOARD_SPAWN_LOCK_FILENAME: &str = "agy-pulse-dashboard.lock";
pub const DASHBOARD_SHORTCUT_MARKER: &str = "agy-pulse-dashboard-shortcut";
pub const DASHBOARD_ICON_MARKER: &str = "agy-pulse-dashboard-icon";
pub const STATUSLINE_IDLE_TIMEOUT_SEC: u64 = 1800;
pub const SPAWN_LOCK_TTL_MS: u64 = 10_000;
pub const DASHBOARD_PING_THROTTLE_MS: u64 = 5 * 60 * 1000;

const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_millis(400);
const PING_TIMEOUT_MS: u64 = 150;
const DAEMON_START_TIMEOUT: Duration = Duration::from_millis(8_000);
const DAEMON_STOP_TIMEOUT: Duration = Duration::from_millis(3_000);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Persisted discovery metadata written by the background dashboard daemon.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DashboardStateFile {
    pub app: String,
    pub pid: u32,
    pub port: u16,
    pub host: String,
    pub url: String,
    #[serde(default)]
    pub idle_timeout_ms: u64,
    #[serde(default)]
    pub started_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HealthResponsePayload {
    ok: bool,
    app: String,
    pid: u32,
    port: u16,
    #[serde(default)]
    idle_timeout_ms: u64,
    #[serde(default)]
    started_at: String,
}

/// Live health status of a reachable dashboard instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningDashboardInfo {
    pub url: String,
    pub host: String,
    pub port: u16,
    pub pid: u32,
    pub idle_timeout_ms: u64,
    pub started_at: String,
}

/// Result returned by `ensure_background_dashboard`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnsureDashboardResult {
    pub info: RunningDashboardInfo,
    pub reused: bool,
}

/// Result returned by `stop_background_dashboard`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopDashboardResult {
    pub stopped: bool,
    pub pid: Option<u32>,
    pub port: Option<u16>,
    pub url: Option<String>,
}

/// Resolved browser command & CLI arguments for launching the dashboard window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserLaunchSpec {
    pub browser_name: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub app_mode: bool,
}

/// Result returned after opening the dashboard in an app window or default browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserOpenResult {
    pub ok: bool,
    pub mode: &'static str,
    pub browser: String,
}

/// Installation status of the OS desktop/menu shortcut for the dashboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutStatus {
    pub installed: bool,
    pub path: Option<PathBuf>,
}

/// Report of installed desktop shortcut artifacts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutInstallReport {
    pub os: DesktopOs,
    pub primary_path: PathBuf,
    pub files: Vec<PathBuf>,
}

/// Report of removed and skipped files during fingerprint-safe shortcut uninstallation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutUninstallReport {
    pub removed: Vec<PathBuf>,
    pub skipped: Vec<PathBuf>,
}

/// Return the discovery state file path (`agy-pulse-dashboard.json`).
pub fn dashboard_state_path() -> PathBuf {
    platform::resolve_antigravity_path(DASHBOARD_STATE_FILENAME)
}

/// Read and parse `agy-pulse-dashboard.json` if present and valid.
pub fn read_dashboard_state() -> Option<DashboardStateFile> {
    let path = dashboard_state_path();
    let raw = fs::read_to_string(path).ok()?;
    let parsed: DashboardStateFile = serde_json::from_str(&raw).ok()?;
    if parsed.port == 0 || parsed.pid == 0 {
        return None;
    }
    Some(parsed)
}

/// Atomically persist `agy-pulse-dashboard.json` with `0600` permissions.
pub fn write_dashboard_state(state: &DashboardStateFile) -> io::Result<PathBuf> {
    let path = dashboard_state_path();
    let bytes = serde_json::to_vec_pretty(state)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    platform::write_atomic_0600(&path, &bytes)?;
    Ok(path)
}

/// Remove `agy-pulse-dashboard.json` only if its `pid` matches `owner_pid`.
pub fn clear_dashboard_state_if_owner(owner_pid: u32) -> bool {
    if owner_pid == 0 {
        return false;
    }
    let path = dashboard_state_path();
    let Some(current) = read_dashboard_state() else {
        return false;
    };
    if current.pid != owner_pid {
        return false;
    }
    fs::remove_file(path).is_ok()
}

/// Statusline-facing dashboard discovery and auto-start metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusLineDashboardInfo {
    pub running: bool,
    pub spawned: bool,
    pub url: String,
    pub host: String,
    pub port: u16,
    pub pid: Option<u32>,
    pub idle_timeout_ms: u64,
    pub started_at: String,
    pub state_mtime_ms: i64,
}

/// Outcome returned by `touch_running_dashboard`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TouchDashboardResult {
    Skipped(&'static str),
    Pinged { ok: bool },
    Respawned { ok: bool },
}

/// Validate that `raw` is a clean `http://` loopback URL (`127.0.0.1` or `localhost`)
/// without credentials, whitespace, control characters, or invalid ports.
pub fn is_valid_loopback_dashboard_url(raw: &str) -> bool {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed
            .chars()
            .any(|c| c.is_whitespace() || (c as u32) < 0x20 || c == '\x7F')
    {
        return false;
    }
    let Ok(parsed) = Url::parse(trimmed) else {
        return false;
    };
    if parsed.scheme() != "http" {
        return false;
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return false;
    }
    let Some(host) = parsed.host_str() else {
        return false;
    };
    if !host.eq_ignore_ascii_case("127.0.0.1") && !host.eq_ignore_ascii_case("localhost") {
        return false;
    }
    parsed.port_or_known_default().is_some_and(|p| p >= 1)
}

/// Resolve the preferred dashboard port (`AGY_PULSE_DASHBOARD_PORT` > `config.display.dashboard_port` > `3789`).
pub fn resolve_dashboard_port(config: &PulseConfig) -> u16 {
    if let Ok(raw) = env::var("AGY_PULSE_DASHBOARD_PORT") {
        let trimmed = raw.trim();
        if !trimmed.is_empty()
            && let Ok(port) = trimmed.parse::<u16>()
            && port >= 1
        {
            return port;
        }
    }
    if let Some(port) = config.display.dashboard_port
        && port >= 1
    {
        return port;
    }
    DEFAULT_DASHBOARD_PORT
}

/// Resolve idle auto-exit seconds for statusline-launched dashboards
/// (`AGY_PULSE_DASHBOARD_IDLE_SEC` > `config.display.dashboard_idle_timeout_sec` > `1800`).
pub fn resolve_statusline_idle_timeout_sec(config: &PulseConfig) -> u64 {
    if let Ok(raw) = env::var("AGY_PULSE_DASHBOARD_IDLE_SEC") {
        let trimmed = raw.trim();
        if !trimmed.is_empty()
            && let Ok(sec) = trimmed.parse::<u64>()
        {
            return sec;
        }
    }
    if let Some(sec) = config.display.dashboard_idle_timeout_sec {
        return sec;
    }
    STATUSLINE_IDLE_TIMEOUT_SEC
}

/// Return the spawn lock file path (`agy-pulse-dashboard.lock`).
pub fn dashboard_spawn_lock_path() -> PathBuf {
    platform::resolve_antigravity_path(DASHBOARD_SPAWN_LOCK_FILENAME)
}

/// Acquire an exclusive spawn lock file (`0600`), reclaiming expired locks older than `ttl_ms`.
pub fn acquire_dashboard_spawn_lock(ttl_ms: u64) -> bool {
    let lock_path = dashboard_spawn_lock_path();
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let payload = format!("{{\"pid\":{},\"ts\":{now_ms}}}", std::process::id());

    for _attempt in 0..2 {
        match platform::create_exclusive_file_0600(&lock_path, payload.as_bytes()) {
            Ok(true) => return true,
            Ok(false) => {
                let Ok(meta) = fs::metadata(&lock_path) else {
                    return false;
                };
                let Ok(modified) = meta.modified() else {
                    return false;
                };
                let age_ms = SystemTime::now()
                    .duration_since(modified)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                if age_ms < ttl_ms {
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

/// Remove the dashboard spawn lock file if present.
pub fn release_dashboard_spawn_lock() -> bool {
    fs::remove_file(dashboard_spawn_lock_path()).is_ok()
}

/// Inspect `agy-pulse-dashboard.json` and local process liveness without performing any network I/O.
/// Cleans up stale state files when the recorded PID is no longer alive.
pub fn get_running_dashboard_info() -> Option<StatusLineDashboardInfo> {
    let state = read_dashboard_state()?;
    if state.app != DASHBOARD_APP_ID {
        return None;
    }
    if !platform::is_process_alive(state.pid) {
        clear_dashboard_state_if_owner(state.pid);
        return None;
    }

    let trimmed_host = state.host.trim();
    let host = if trimmed_host.eq_ignore_ascii_case("localhost") {
        "localhost".to_string()
    } else {
        LOOPBACK_HOST.to_string()
    };

    let raw_url = if !state.url.trim().is_empty() {
        state.url.trim().to_string()
    } else {
        format!("http://{host}:{}", state.port)
    };
    if !is_valid_loopback_dashboard_url(&raw_url) {
        return None;
    }

    let state_mtime_ms = fs::metadata(dashboard_state_path())
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    Some(StatusLineDashboardInfo {
        running: true,
        spawned: false,
        url: raw_url,
        host,
        port: state.port,
        pid: Some(state.pid),
        idle_timeout_ms: state.idle_timeout_ms,
        started_at: state.started_at,
        state_mtime_ms,
    })
}

/// Spawn a detached background dashboard daemon without waiting for HTTP readiness.
pub fn spawn_background_dashboard_detached(
    port: u16,
    idle_timeout_sec: u64,
    lang: LanguageSetting,
    no_sync: bool,
) -> io::Result<u32> {
    let exe_path = env::current_exe()?;
    let port_str = port.to_string();
    let idle_str = idle_timeout_sec.to_string();
    let mut args: Vec<&str> = vec![
        "dashboard",
        "--serve-background",
        "--no-open",
        "--port",
        &port_str,
        "--idle-timeout",
        &idle_str,
    ];
    match lang {
        LanguageSetting::Zh => args.extend(["--lang", "zh"]),
        LanguageSetting::En => args.extend(["--lang", "en"]),
        LanguageSetting::Auto => {}
    }
    if no_sync {
        args.push("--no-sync");
    }
    platform::spawn_detached(&exe_path, &args)
}

/// Resolve the dashboard URL for statusline rendering, optionally spawning the daemon non-blockingly.
pub fn resolve_dashboard_url_for_statusline(
    config: &PulseConfig,
    lang: LanguageSetting,
    auto_start: bool,
) -> Option<StatusLineDashboardInfo> {
    if let Some(running) = get_running_dashboard_info() {
        return Some(running);
    }
    if !auto_start {
        return None;
    }

    let port = resolve_dashboard_port(config);
    let idle_sec = resolve_statusline_idle_timeout_sec(config);
    let spawned = if acquire_dashboard_spawn_lock(SPAWN_LOCK_TTL_MS) {
        if spawn_background_dashboard_detached(port, idle_sec, lang, false).is_ok() {
            true
        } else {
            release_dashboard_spawn_lock();
            return None;
        }
    } else {
        false
    };

    Some(StatusLineDashboardInfo {
        running: false,
        spawned,
        url: format!("http://{LOOPBACK_HOST}:{port}"),
        host: LOOPBACK_HOST.to_string(),
        port,
        pid: None,
        idle_timeout_ms: idle_sec.saturating_mul(1000),
        started_at: String::new(),
        state_mtime_ms: 0,
    })
}

/// Send a short-timeout `POST /api/ping` keep-alive to a loopback dashboard server.
pub fn ping_dashboard(host: &str, port: u16, timeout_ms: u64) -> bool {
    if port == 0 {
        return false;
    }
    let ip: IpAddr = if host.eq_ignore_ascii_case("localhost") {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else if let Ok(parsed) = host.parse::<IpAddr>()
        && parsed.is_loopback()
    {
        parsed
    } else {
        return false;
    };

    let timeout = Duration::from_millis(timeout_ms.max(1));
    let addr = SocketAddr::new(ip, port);
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, timeout) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));

    let req = format!(
        "POST /api/ping HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(req.as_bytes()).is_err() || stream.flush().is_err() {
        return false;
    }

    let mut raw = Vec::with_capacity(256);
    let mut chunk = [0u8; 256];
    while raw.len() < 512 {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                raw.extend_from_slice(&chunk[..n]);
                if raw.windows(2).any(|w| w == b"\r\n") {
                    break;
                }
            }
        }
    }
    let text = String::from_utf8_lossy(&raw);
    text.lines().next().is_some_and(|line| line.contains(" 200 "))
}

/// Throttled keep-alive ping and self-healing respawn for a running dashboard instance.
pub fn touch_running_dashboard(
    info: &StatusLineDashboardInfo,
    config: &PulseConfig,
    lang: LanguageSetting,
    auto_start: bool,
    now_ms: i64,
) -> TouchDashboardResult {
    if !info.running {
        return TouchDashboardResult::Skipped("not_running");
    }
    if info.idle_timeout_ms == 0 {
        return TouchDashboardResult::Skipped("no_idle_timeout");
    }

    let throttle_ms = (info.idle_timeout_ms / 3).clamp(1_000, DASHBOARD_PING_THROTTLE_MS);
    if info.state_mtime_ms > 0
        && now_ms.saturating_sub(info.state_mtime_ms) < throttle_ms as i64
    {
        return TouchDashboardResult::Skipped("throttled");
    }

    if ping_dashboard(&info.host, info.port, PING_TIMEOUT_MS) {
        let _ = platform::touch_file_mtime(&dashboard_state_path());
        return TouchDashboardResult::Pinged { ok: true };
    }

    if let Some(pid) = info.pid {
        clear_dashboard_state_if_owner(pid);
    }
    if auto_start && acquire_dashboard_spawn_lock(SPAWN_LOCK_TTL_MS) {
        let port = resolve_dashboard_port(config);
        let idle_sec = resolve_statusline_idle_timeout_sec(config);
        if spawn_background_dashboard_detached(port, idle_sec, lang, false).is_ok() {
            return TouchDashboardResult::Respawned { ok: true };
        }
        release_dashboard_spawn_lock();
    }
    TouchDashboardResult::Pinged { ok: false }
}

/// Probe `GET /api/health` on `host:port` to verify a live `agy-pulse-dashboard` instance.
pub fn probe_dashboard_health(host: &str, port: u16) -> Option<RunningDashboardInfo> {
    if port == 0 {
        return None;
    }
    let ip: std::net::IpAddr = host.parse().ok()?;
    let addr = SocketAddr::new(ip, port);
    let mut stream = TcpStream::connect_timeout(&addr, HEALTH_PROBE_TIMEOUT).ok()?;
    let _ = stream.set_read_timeout(Some(HEALTH_PROBE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(HEALTH_PROBE_TIMEOUT));

    let req = format!(
        "GET /api/health HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).ok()?;
    stream.flush().ok()?;

    let mut raw = Vec::with_capacity(1024);
    let _ = stream.read_to_end(&mut raw);
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text.split_once("\r\n\r\n")?;
    let status = head
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse::<u16>()
        .ok()?;
    if status != 200 {
        return None;
    }

    let payload: HealthResponsePayload = serde_json::from_str(body).ok()?;
    if !payload.ok || payload.app != DASHBOARD_APP_ID {
        return None;
    }
    let resolved_port = if payload.port > 0 { payload.port } else { port };
    Some(RunningDashboardInfo {
        url: format!("http://{host}:{resolved_port}"),
        host: host.to_string(),
        port: resolved_port,
        pid: payload.pid,
        idle_timeout_ms: payload.idle_timeout_ms,
        started_at: payload.started_at,
    })
}

/// Find a healthy running dashboard on `host:port` or via `agy-pulse-dashboard.json`.
/// Automatically cleans up stale discovery files whose PIDs are no longer alive.
pub fn find_running_dashboard(host: &str, port: u16) -> Option<RunningDashboardInfo> {
    if let Some(info) = probe_dashboard_health(host, port) {
        return Some(info);
    }

    if let Some(state) = read_dashboard_state() {
        let state_host = if state.host.trim().is_empty() {
            host
        } else {
            state.host.as_str()
        };
        if state.port != port
            && let Some(info) = probe_dashboard_health(state_host, state.port)
        {
            return Some(info);
        }
        if !platform::is_process_alive(state.pid) {
            clear_dashboard_state_if_owner(state.pid);
        }
    }

    None
}

/// Reuse an existing healthy background dashboard or spawn a detached daemon and wait until ready.
pub fn ensure_background_dashboard(
    host: &str,
    port: u16,
    idle_timeout_sec: u64,
    lang: LanguageSetting,
    no_sync: bool,
) -> Result<EnsureDashboardResult, String> {
    if let Some(existing) = find_running_dashboard(host, port) {
        return Ok(EnsureDashboardResult {
            info: existing,
            reused: true,
        });
    }

    let exe_path = env::current_exe()
        .map_err(|err| format!("Failed to locate current executable: {err}"))?;

    let effective_idle = if idle_timeout_sec > 0 {
        idle_timeout_sec
    } else {
        DEFAULT_IDLE_TIMEOUT_SEC
    };
    let port_str = port.to_string();
    let idle_str = effective_idle.to_string();
    let mut args: Vec<&str> = vec![
        "dashboard",
        "--serve-background",
        "--no-open",
        "--host",
        host,
        "--port",
        &port_str,
        "--idle-timeout",
        &idle_str,
    ];
    match lang {
        LanguageSetting::Zh => args.extend(["--lang", "zh"]),
        LanguageSetting::En => args.extend(["--lang", "en"]),
        LanguageSetting::Auto => {}
    }
    if no_sync {
        args.push("--no-sync");
    }

    let _child_pid = platform::spawn_detached(&exe_path, &args)
        .map_err(|err| format!("Failed to spawn background dashboard process: {err}"))?;

    let deadline = Instant::now() + DAEMON_START_TIMEOUT;
    loop {
        if let Some(running) = find_running_dashboard(host, port) {
            return Ok(EnsureDashboardResult {
                info: running,
                reused: false,
            });
        }
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        thread::sleep(POLL_INTERVAL.min(deadline - now));
    }

    Err(format!(
        "Timed out after {}ms waiting for background agy-pulse dashboard on {host}:{port}",
        DAEMON_START_TIMEOUT.as_millis()
    ))
}

/// Gracefully stop a running background dashboard daemon and clean up its discovery file.
pub fn stop_background_dashboard(host: &str, port: u16) -> Result<StopDashboardResult, String> {
    let Some(running) = find_running_dashboard(host, port) else {
        if let Some(stale) = read_dashboard_state()
            && !platform::is_process_alive(stale.pid)
        {
            clear_dashboard_state_if_owner(stale.pid);
        }
        return Ok(StopDashboardResult {
            stopped: false,
            pid: None,
            port: None,
            url: None,
        });
    };

    if running.pid == 0 {
        return Ok(StopDashboardResult {
            stopped: false,
            pid: None,
            port: None,
            url: None,
        });
    }

    platform::terminate_process(running.pid)
        .map_err(|err| format!("Failed to stop dashboard PID {}: {err}", running.pid))?;

    let deadline = Instant::now() + DAEMON_STOP_TIMEOUT;
    while Instant::now() < deadline {
        if probe_dashboard_health(&running.host, running.port).is_none()
            && !platform::is_process_alive(running.pid)
        {
            break;
        }
        thread::sleep(POLL_INTERVAL);
    }

    clear_dashboard_state_if_owner(running.pid);
    Ok(StopDashboardResult {
        stopped: true,
        pid: Some(running.pid),
        port: Some(running.port),
        url: Some(running.url),
    })
}

/// Construct cross-platform browser CLI arguments for frameless `--app=<url>` window mode.
pub fn build_app_window_launch_spec(
    os: DesktopOs,
    browser_name: &str,
    browser_path: &Path,
    url: &str,
) -> BrowserLaunchSpec {
    let app_flag = format!("--app={url}");
    match os {
        DesktopOs::Macos => BrowserLaunchSpec {
            browser_name: browser_name.to_string(),
            program: PathBuf::from("open"),
            args: vec![
                "-na".to_string(),
                browser_path.to_string_lossy().into_owned(),
                "--args".to_string(),
                app_flag,
                "--new-window".to_string(),
            ],
            app_mode: true,
        },
        DesktopOs::Windows => BrowserLaunchSpec {
            browser_name: browser_name.to_string(),
            program: browser_path.to_path_buf(),
            args: vec![app_flag, "--new-window".to_string()],
            app_mode: true,
        },
        DesktopOs::Linux => BrowserLaunchSpec {
            browser_name: browser_name.to_string(),
            program: browser_path.to_path_buf(),
            args: vec![
                app_flag,
                format!("--class={DASHBOARD_APP_ID}"),
                "--new-window".to_string(),
            ],
            app_mode: true,
        },
    }
}

/// Open the dashboard URL in an app-mode Chromium window if available, else the system browser.
pub fn open_dashboard_window(url: &str, app_mode: bool) -> BrowserOpenResult {
    if env::var("AGY_PULSE_NO_OPEN").ok().as_deref() == Some("1") {
        return BrowserOpenResult {
            ok: false,
            mode: "suppressed",
            browser: "none".to_string(),
        };
    }

    let os = platform::current_desktop_os();
    if app_mode
        && let Some((browser_name, browser_path)) = platform::detect_app_window_browser_path(os)
    {
        let spec = build_app_window_launch_spec(os, &browser_name, &browser_path, url);
        let arg_refs: Vec<&str> = spec.args.iter().map(String::as_str).collect();
        if platform::spawn_detached(&spec.program, &arg_refs).is_ok() {
            return BrowserOpenResult {
                ok: true,
                mode: "app",
                browser: spec.browser_name,
            };
        }
    }

    let ok = platform::open_url_default_browser(os, url);
    BrowserOpenResult {
        ok,
        mode: "browser",
        browser: "default".to_string(),
    }
}

struct ShortcutPaths {
    primary_path: PathBuf,
    secondary_path: Option<PathBuf>,
    icon_path: PathBuf,
    extra_script_path: Option<PathBuf>,
    plist_path: Option<PathBuf>,
}

fn resolve_shortcut_paths(os: DesktopOs) -> Option<ShortcutPaths> {
    let home = platform::home_dir()?;
    match os {
        DesktopOs::Macos => {
            let bundle = home.join("Applications").join("agy-pulse Dashboard.app");
            let plist = bundle.join("Contents").join("Info.plist");
            let script = bundle
                .join("Contents")
                .join("MacOS")
                .join("agy-pulse-dashboard");
            let icon = bundle
                .join("Contents")
                .join("Resources")
                .join("agy-pulse-dashboard.svg");
            Some(ShortcutPaths {
                primary_path: bundle,
                secondary_path: None,
                icon_path: icon,
                extra_script_path: Some(script),
                plist_path: Some(plist),
            })
        }
        DesktopOs::Windows => {
            let local_app = platform::windows_local_app_data()?;
            let app_data = platform::windows_app_data()?;
            let vbs = local_app.join("agy-pulse").join("agy-pulse-dashboard.vbs");
            let icon = local_app.join("agy-pulse").join("agy-pulse-dashboard.svg");
            let start_lnk = app_data
                .join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs")
                .join("agy-pulse Dashboard.lnk");
            let desktop_lnk = home.join("Desktop").join("agy-pulse Dashboard.lnk");
            Some(ShortcutPaths {
                primary_path: start_lnk,
                secondary_path: Some(desktop_lnk),
                icon_path: icon,
                extra_script_path: Some(vbs),
                plist_path: None,
            })
        }
        DesktopOs::Linux => {
            let xdg_data = platform::xdg_data_home()?;
            let entry = xdg_data
                .join("applications")
                .join("agy-pulse-dashboard.desktop");
            let icon = xdg_data
                .join("icons")
                .join("hicolor")
                .join("scalable")
                .join("apps")
                .join("agy-pulse-dashboard.svg");
            let desktop_copy = home.join("Desktop").join("agy-pulse-dashboard.desktop");
            Some(ShortcutPaths {
                primary_path: entry,
                secondary_path: Some(desktop_copy),
                icon_path: icon,
                extra_script_path: None,
                plist_path: None,
            })
        }
    }
}

fn is_our_shortcut_file(path: &Path) -> bool {
    fs::read_to_string(path)
        .map(|content| content.contains(DASHBOARD_SHORTCUT_MARKER))
        .unwrap_or(false)
}

fn is_our_icon_file(path: &Path) -> bool {
    fs::read_to_string(path)
        .map(|content| content.contains(DASHBOARD_ICON_MARKER))
        .unwrap_or(false)
}

fn escape_vbs_string(raw: &str) -> String {
    raw.replace('"', "\"\"")
}

fn escape_ps_single_quoted(raw: &str) -> String {
    raw.replace('\'', "''")
}

/// Install a desktop/application-menu shortcut containing the exclusive agy-pulse fingerprint.
pub fn install_desktop_shortcuts(
    include_desktop: bool,
    port: Option<u16>,
) -> Result<ShortcutInstallReport, String> {
    let os = platform::current_desktop_os();
    let paths = resolve_shortcut_paths(os)
        .ok_or_else(|| "Unable to resolve user home/data directory for shortcuts".to_string())?;
    let exe_path = env::current_exe()
        .map_err(|err| format!("Failed to locate current executable: {err}"))?;
    let exe_str = exe_path.to_string_lossy();
    let port_flag = match port {
        Some(p) if p > 0 => format!(" --port {p}"),
        _ => String::new(),
    };

    let mut files = Vec::new();
    match os {
        DesktopOs::Macos => {
            let plist_path = paths
                .plist_path
                .as_ref()
                .ok_or_else(|| "Missing macOS Info.plist path".to_string())?;
            let script_path = paths
                .extra_script_path
                .as_ref()
                .ok_or_else(|| "Missing macOS launcher script path".to_string())?;
            for p in [plist_path, script_path, &paths.icon_path] {
                if let Some(parent) = p.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|e| format!("Failed to create {}: {e}", parent.display()))?;
                }
            }
            let plist = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                 <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
                 <!-- {DASHBOARD_SHORTCUT_MARKER} -->\n\
                 <plist version=\"1.0\">\n\
                 <dict>\n  \
                   <key>CFBundleDevelopmentRegion</key>\n  \
                   <string>en</string>\n  \
                   <key>CFBundleExecutable</key>\n  \
                   <string>agy-pulse-dashboard</string>\n  \
                   <key>CFBundleIdentifier</key>\n  \
                   <string>dev.agy-pulse.dashboard</string>\n  \
                   <key>CFBundleName</key>\n  \
                   <string>agy-pulse Dashboard</string>\n  \
                   <key>CFBundleDisplayName</key>\n  \
                   <string>agy-pulse Dashboard</string>\n  \
                   <key>CFBundlePackageType</key>\n  \
                   <string>APPL</string>\n  \
                   <key>CFBundleVersion</key>\n  \
                   <string>1.0</string>\n  \
                   <key>LSUIElement</key>\n  \
                   <true/>\n\
                 </dict>\n\
                 </plist>\n"
            );
            let script = format!(
                "#!/bin/sh\n# {DASHBOARD_SHORTCUT_MARKER}\nexec \"{exe_str}\" dashboard open{port_flag}\n"
            );
            fs::write(plist_path, plist)
                .map_err(|e| format!("Failed to write {}: {e}", plist_path.display()))?;
            fs::write(script_path, script)
                .map_err(|e| format!("Failed to write {}: {e}", script_path.display()))?;
            let _ = platform::set_executable_0755(script_path);
            fs::write(&paths.icon_path, DASHBOARD_ICON_SVG)
                .map_err(|e| format!("Failed to write {}: {e}", paths.icon_path.display()))?;
            files.push(plist_path.clone());
            files.push(script_path.clone());
            files.push(paths.icon_path.clone());
        }
        DesktopOs::Windows => {
            let vbs_path = paths
                .extra_script_path
                .as_ref()
                .ok_or_else(|| "Missing Windows VBS path".to_string())?;
            if let Some(parent) = vbs_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create {}: {e}", parent.display()))?;
            }
            let vbs_cmd = format!("\"\"{}\"\" dashboard open{port_flag}", escape_vbs_string(&exe_str));
            let vbs_content = format!(
                "' {DASHBOARD_SHORTCUT_MARKER}\r\nSet WshShell = CreateObject(\"WScript.Shell\")\r\nWshShell.Run \"{vbs_cmd}\", 0, False\r\n"
            );
            fs::write(vbs_path, vbs_content)
                .map_err(|e| format!("Failed to write {}: {e}", vbs_path.display()))?;
            fs::write(&paths.icon_path, DASHBOARD_ICON_SVG)
                .map_err(|e| format!("Failed to write {}: {e}", paths.icon_path.display()))?;
            files.push(vbs_path.clone());
            files.push(paths.icon_path.clone());

            let mut targets = vec![paths.primary_path.clone()];
            if include_desktop && let Some(ref desk) = paths.secondary_path {
                targets.push(desk.clone());
            }
            let mut ps_commands = vec!["$WshShell = New-Object -ComObject WScript.Shell;".to_string()];
            let vbs_str = vbs_path.to_string_lossy();
            for lnk in &targets {
                if let Some(parent) = lnk.parent() {
                    ps_commands.push(format!(
                        "New-Item -ItemType Directory -Force -Path '{}' | Out-Null;",
                        escape_ps_single_quoted(&parent.to_string_lossy())
                    ));
                }
                let lnk_str = lnk.to_string_lossy();
                ps_commands.push(format!(
                    "$S = $WshShell.CreateShortcut('{}'); \
                     $S.TargetPath = 'wscript.exe'; \
                     $S.Arguments = '\"{}\"'; \
                     $S.Description = 'agy-pulse Telemetry Dashboard ({DASHBOARD_SHORTCUT_MARKER})'; \
                     $S.Save();",
                    escape_ps_single_quoted(&lnk_str),
                    escape_ps_single_quoted(&vbs_str),
                ));
                files.push(lnk.clone());
            }
            let joined_ps = ps_commands.join(" ");
            platform::run_silent_helper(
                "powershell.exe",
                &[
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-Command",
                    &joined_ps,
                ],
            );
        }
        DesktopOs::Linux => {
            if let Some(parent) = paths.icon_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create {}: {e}", parent.display()))?;
            }
            if let Some(parent) = paths.primary_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create {}: {e}", parent.display()))?;
            }

            fs::write(&paths.icon_path, DASHBOARD_ICON_SVG)
                .map_err(|e| format!("Failed to write {}: {e}", paths.icon_path.display()))?;
            files.push(paths.icon_path.clone());

            let icon_str = paths.icon_path.to_string_lossy();
            let entry_content = format!(
                "[Desktop Entry]\n\
                 # {DASHBOARD_SHORTCUT_MARKER}\n\
                 Type=Application\n\
                 Version=1.0\n\
                 Name=agy-pulse Dashboard\n\
                 Comment=Local Telemetry Dashboard for agy-pulse\n\
                 Exec=\"{exe_str}\" dashboard open{port_flag}\n\
                 Icon={icon_str}\n\
                 Terminal=false\n\
                 Categories=Development;Utility;\n\
                 StartupNotify=true\n\
                 StartupWMClass={DASHBOARD_APP_ID}\n"
            );
            fs::write(&paths.primary_path, &entry_content)
                .map_err(|e| format!("Failed to write {}: {e}", paths.primary_path.display()))?;
            let _ = platform::set_executable_0755(&paths.primary_path);
            files.push(paths.primary_path.clone());

            if include_desktop && let Some(ref desk) = paths.secondary_path {
                if let Some(parent) = desk.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                if fs::write(desk, &entry_content).is_ok() {
                    let _ = platform::set_executable_0755(desk);
                    files.push(desk.clone());
                }
            }

            if let Some(apps_dir) = paths.primary_path.parent() {
                let dir_str = apps_dir.to_string_lossy();
                platform::run_silent_helper("update-desktop-database", &[&dir_str]);
            }
        }
    }

    Ok(ShortcutInstallReport {
        os,
        primary_path: paths.primary_path,
        files,
    })
}

/// Remove only shortcuts carrying the exclusive `agy-pulse-dashboard-shortcut` fingerprint.
/// Exported at `dashboard::uninstall_desktop_shortcuts` for direct reuse by Ticket 09 `uninstall`.
pub fn uninstall_desktop_shortcuts() -> ShortcutUninstallReport {
    let os = platform::current_desktop_os();
    let Some(paths) = resolve_shortcut_paths(os) else {
        return ShortcutUninstallReport {
            removed: Vec::new(),
            skipped: Vec::new(),
        };
    };

    let mut removed = Vec::new();
    let mut skipped = Vec::new();

    match os {
        DesktopOs::Macos => {
            if paths.primary_path.exists() {
                let has_marker = paths
                    .extra_script_path
                    .as_deref()
                    .is_some_and(is_our_shortcut_file)
                    || paths.plist_path.as_deref().is_some_and(is_our_shortcut_file);
                if has_marker && fs::remove_dir_all(&paths.primary_path).is_ok() {
                    removed.push(paths.primary_path);
                } else {
                    skipped.push(paths.primary_path);
                }
            }
        }
        DesktopOs::Windows => {
            let vbs_is_ours = paths
                .extra_script_path
                .as_deref()
                .is_some_and(is_our_shortcut_file);
            let mut candidates = Vec::new();
            if let Some(ref vbs) = paths.extra_script_path {
                candidates.push(vbs.clone());
            }
            candidates.push(paths.icon_path);
            candidates.push(paths.primary_path);
            if let Some(ref desk) = paths.secondary_path {
                candidates.push(desk.clone());
            }
            for candidate in candidates {
                if !candidate.exists() {
                    continue;
                }
                if vbs_is_ours && fs::remove_file(&candidate).is_ok() {
                    removed.push(candidate);
                } else {
                    skipped.push(candidate);
                }
            }
        }
        DesktopOs::Linux => {
            let mut removed_entry = false;
            let mut candidates = vec![paths.primary_path.clone()];
            if let Some(ref desk) = paths.secondary_path {
                candidates.push(desk.clone());
            }
            for entry in candidates {
                if !entry.exists() {
                    continue;
                }
                if is_our_shortcut_file(&entry) && fs::remove_file(&entry).is_ok() {
                    removed.push(entry);
                    removed_entry = true;
                } else {
                    skipped.push(entry);
                }
            }
            if paths.icon_path.exists() {
                if (removed_entry || is_our_icon_file(&paths.icon_path))
                    && fs::remove_file(&paths.icon_path).is_ok()
                {
                    removed.push(paths.icon_path);
                } else {
                    skipped.push(paths.icon_path);
                }
            }
            if let Some(apps_dir) = paths.primary_path.parent() {
                let dir_str = apps_dir.to_string_lossy();
                platform::run_silent_helper("update-desktop-database", &[&dir_str]);
            }
        }
    }

    ShortcutUninstallReport { removed, skipped }
}

/// Check whether a fingerprinted agy-pulse dashboard shortcut is currently installed.
pub fn get_desktop_shortcut_status() -> ShortcutStatus {
    let os = platform::current_desktop_os();
    let Some(paths) = resolve_shortcut_paths(os) else {
        return ShortcutStatus {
            installed: false,
            path: None,
        };
    };

    match os {
        DesktopOs::Macos => {
            let has_marker = paths
                .extra_script_path
                .as_deref()
                .is_some_and(is_our_shortcut_file)
                || paths.plist_path.as_deref().is_some_and(is_our_shortcut_file);
            if has_marker {
                ShortcutStatus {
                    installed: true,
                    path: Some(paths.primary_path),
                }
            } else {
                ShortcutStatus {
                    installed: false,
                    path: None,
                }
            }
        }
        DesktopOs::Windows => {
            if let Some(ref vbs) = paths.extra_script_path
                && vbs.exists()
                && is_our_shortcut_file(vbs)
            {
                let shown = if paths.primary_path.exists() {
                    paths.primary_path
                } else {
                    vbs.clone()
                };
                return ShortcutStatus {
                    installed: true,
                    path: Some(shown),
                };
            }
            ShortcutStatus {
                installed: false,
                path: None,
            }
        }
        DesktopOs::Linux => {
            let mut candidates = vec![paths.primary_path];
            if let Some(desk) = paths.secondary_path {
                candidates.push(desk);
            }
            for candidate in candidates {
                if candidate.exists() && is_our_shortcut_file(&candidate) {
                    return ShortcutStatus {
                        installed: true,
                        path: Some(candidate),
                    };
                }
            }
            ShortcutStatus {
                installed: false,
                path: None,
            }
        }
    }
}
