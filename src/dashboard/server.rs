use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::config::LanguageSetting;
use crate::ledger::{
    CsvExportKind, LedgerError, LedgerGranularity, LedgerQueryFilter, LedgerSourceFilter,
    LedgerStore, LedgerSyncOptions, LedgerSyncReport, LedgerTimeRange, SourceRoot,
    SyncStatusOverview, redact_sync_error_message,
};
use crate::platform;
use crate::render::resolve_language_from_locale;
use crate::sync::{SyncOutcomeKind, build_remote, load_sync_config, sync_both};

use super::http_wire::{
    HttpParseError, HttpRequest, finish_early_error_response, has_valid_csrf_token,
    is_allowed_host, is_strict_same_origin, is_strict_same_origin_with_required_origin,
    parse_query_params, query_param, read_http_request, write_http_response,
};
use super::keepalive::{
    KeepaliveState, resolve_close_grace_ms, resolve_ping_lease_ms, resolve_sse_ping_interval_ms,
};
use super::{
    DASHBOARD_APP_ID, DASHBOARD_ICON_SVG, DEFAULT_DASHBOARD_PORT, LOOPBACK_HOST,
    render_dashboard_html_with_token,
};

const DEFAULT_AUTO_SYNC_INTERVAL_MS: u64 = 10_000;
const REMOTE_SYNC_THROTTLE_MS: u64 = 30_000;
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(15);
const SSE_EOF_POLL_TIMEOUT: Duration = Duration::from_millis(50);
const SOCKET_IO_TIMEOUT: Duration = Duration::from_secs(5);
const OVERLOAD_WRITE_TIMEOUT: Duration = Duration::from_millis(250);
pub const MAX_CONCURRENT_CONNECTIONS: usize = 64;
pub const MAX_SSE_CONNECTIONS: usize = 32;
const DASHBOARD_CSP: &str = "default-src 'self'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self'; img-src 'self' data: blob:;";

/// Errors returned when configuring or starting the local dashboard server.
#[derive(Debug, thiserror::Error)]
pub enum DashboardError {
    #[error("Dashboard server must bind exclusively to {LOOPBACK_HOST}, got '{0}'")]
    NonLoopbackHost(String),
    #[error("Dashboard server I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("Dashboard ledger error: {0}")]
    Ledger(#[from] LedgerError),
}

/// Configuration for `DashboardServer::start`.
#[derive(Debug, Clone)]
pub struct DashboardServerConfig {
    pub host: String,
    pub port: u16,
    pub lang: LanguageSetting,
    pub no_sync: bool,
    pub defer_initial_sync: bool,
    pub idle_timeout_ms: u64,
    pub close_grace_ms: Option<u64>,
    pub auto_sync_interval_ms: u64,
    pub ledger_path: Option<PathBuf>,
    pub source_roots: Option<Vec<SourceRoot>>,
    pub enable_collector: bool,
    pub max_concurrent_connections: usize,
    pub max_sse_connections: usize,
}

impl Default for DashboardServerConfig {
    fn default() -> Self {
        Self {
            host: LOOPBACK_HOST.to_string(),
            port: DEFAULT_DASHBOARD_PORT,
            lang: LanguageSetting::Auto,
            no_sync: false,
            defer_initial_sync: false,
            idle_timeout_ms: 0,
            close_grace_ms: None,
            auto_sync_interval_ms: DEFAULT_AUTO_SYNC_INTERVAL_MS,
            ledger_path: None,
            source_roots: None,
            enable_collector: false,
            max_concurrent_connections: MAX_CONCURRENT_CONNECTIONS,
            max_sse_connections: MAX_SSE_CONNECTIONS,
        }
    }
}

/// Runtime metadata returned after the dashboard HTTP server binds to loopback.
#[derive(Debug, Clone)]
pub struct DashboardServerInfo {
    pub url: String,
    pub port: u16,
    pub host: String,
    pub started_at: String,
    pub sync_report: Option<LedgerSyncReport>,
}

pub(super) struct LedgerSyncWriter {
    pub(super) store: LedgerStore,
    last_sync_instant: Option<Instant>,
}

pub(super) fn refresh_sync_identity_hints(sync_options: &mut LedgerSyncOptions) {
    let (default_account_id, device_name) = platform::read_sync_identity_hints();
    sync_options.default_account_id = default_account_id;
    sync_options.device_name = device_name;
}

fn generate_csrf_token() -> io::Result<String> {
    platform::random_hex(32)
}

pub(super) struct ServerSharedState {
    pub(super) port: u16,
    default_lang: LanguageSetting,
    no_sync: bool,
    pub(super) idle_timeout_ms: u64,
    auto_sync_interval_ms: u64,
    sse_ping_interval: Duration,
    started_at: String,
    ledger_path: PathBuf,
    pub(super) local_device_id: String,
    pub(super) csrf_token: String,
    pub(super) pulse_config_lock: Mutex<()>,
    pub(super) sync_config_lock: Mutex<()>,
    pub(super) sync_options: Mutex<LedgerSyncOptions>,
    pub(super) keepalive: Arc<KeepaliveState>,
    shutdown: AtomicBool,
    collector_shutdown: Arc<AtomicBool>,
    max_concurrent_connections: usize,
    max_sse_connections: usize,
    active_connections: Arc<AtomicUsize>,
    active_sse_connections: Arc<AtomicUsize>,
    sync_remote_in_progress: Arc<AtomicBool>,
    last_remote_sync_instant: Mutex<Option<Instant>>,
    sync_write: Mutex<LedgerSyncWriter>,
}

struct AtomicSlotGuard {
    counter: Arc<AtomicUsize>,
}

impl AtomicSlotGuard {
    fn try_acquire(counter: &Arc<AtomicUsize>, limit: usize) -> Option<Self> {
        let mut current = counter.load(Ordering::Acquire);
        loop {
            if current >= limit {
                return None;
            }
            match counter.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(Self {
                        counter: Arc::clone(counter),
                    });
                }
                Err(actual) => current = actual,
            }
        }
    }
}

impl Drop for AtomicSlotGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
    }
}

impl ServerSharedState {
    fn maybe_auto_sync(&self) {
        if self.no_sync || self.auto_sync_interval_ms == 0 {
            return;
        }
        let Ok(mut guard) = self.sync_write.try_lock() else {
            // Skip auto-sync without blocking when another local sync is already in flight.
            return;
        };
        if let Some(prev) = guard.last_sync_instant
            && (prev.elapsed().as_millis() as u64) < self.auto_sync_interval_ms
        {
            return;
        }
        let sync_opts = {
            let Ok(mut opts_guard) = self.sync_options.lock() else {
                return;
            };
            refresh_sync_identity_hints(&mut opts_guard);
            opts_guard.clone()
        };
        match guard.store.sync_all(&sync_opts) {
            Ok(_) => {
                guard.last_sync_instant = Some(Instant::now());
            }
            Err(err) => {
                platform::write_error_log_safely(&format!("Dashboard auto-sync error: {err}"));
            }
        }
    }
}

/// Single-binary lightweight `127.0.0.1` HTTP/1.1 server for the Telemetry Dashboard.
pub struct DashboardServer {
    info: DashboardServerInfo,
    shared: Arc<ServerSharedState>,
    accept_thread: Option<JoinHandle<()>>,
    collector: Option<crate::collector::CollectorServerHandle>,
}

impl DashboardServer {
    /// Validate loopback-only binding, run optional initial ledger sync, and start serving HTTP.
    pub fn start(config: DashboardServerConfig) -> Result<Self, DashboardError> {
        if config.host != LOOPBACK_HOST {
            return Err(DashboardError::NonLoopbackHost(config.host));
        }

        let ledger_path = config
            .ledger_path
            .unwrap_or_else(LedgerStore::default_ledger_path);
        let mut store = LedgerStore::open_at(&ledger_path)?;
        let local_device_id = store.local_device_id()?;
        let mut sync_options = LedgerSyncOptions {
            source_roots: config.source_roots,
            ..Default::default()
        };
        refresh_sync_identity_hints(&mut sync_options);

        let mut last_sync_instant = None;
        let mut sync_report = None;
        if !config.no_sync && !config.defer_initial_sync {
            match store.sync_all(&sync_options) {
                Ok(report) => {
                    last_sync_instant = Some(Instant::now());
                    sync_report = Some(report);
                }
                Err(err) => {
                    platform::write_error_log_safely(&format!(
                        "Dashboard initial sync error: {err}"
                    ));
                }
            }
        }

        let listener = match TcpListener::bind((LOOPBACK_HOST, config.port)) {
            Ok(l) => l,
            Err(err) if err.kind() == io::ErrorKind::AddrInUse && config.port != 0 => {
                TcpListener::bind((LOOPBACK_HOST, 0))?
            }
            Err(err) => return Err(DashboardError::Io(err)),
        };
        listener.set_nonblocking(true)?;
        let actual_port = listener.local_addr()?.port();

        let locale = platform::read_locale_env();
        let default_lang = resolve_language_from_locale(config.lang, locale.as_deref());
        let started_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let started_instant = Instant::now();
        let close_grace_ms = resolve_close_grace_ms(config.close_grace_ms);
        let ping_lease_ms = resolve_ping_lease_ms();
        let sse_ping_interval = Duration::from_millis(resolve_sse_ping_interval_ms());
        let keepalive = Arc::new(KeepaliveState::new(
            config.idle_timeout_ms,
            close_grace_ms,
            ping_lease_ms,
            started_instant,
        ));

        let csrf_token = generate_csrf_token()?;
        let collector_shutdown = Arc::new(AtomicBool::new(false));

        let shared = Arc::new(ServerSharedState {
            port: actual_port,
            default_lang,
            no_sync: config.no_sync,
            idle_timeout_ms: config.idle_timeout_ms,
            auto_sync_interval_ms: config.auto_sync_interval_ms,
            sse_ping_interval,
            started_at: started_at.clone(),
            ledger_path,
            local_device_id,
            csrf_token,
            pulse_config_lock: Mutex::new(()),
            sync_config_lock: Mutex::new(()),
            sync_options: Mutex::new(sync_options),
            keepalive,
            shutdown: AtomicBool::new(false),
            collector_shutdown: Arc::clone(&collector_shutdown),
            max_concurrent_connections: config.max_concurrent_connections.max(1),
            max_sse_connections: config.max_sse_connections.max(1),
            active_connections: Arc::new(AtomicUsize::new(0)),
            active_sse_connections: Arc::new(AtomicUsize::new(0)),
            sync_remote_in_progress: Arc::new(AtomicBool::new(false)),
            last_remote_sync_instant: Mutex::new(None),
            sync_write: Mutex::new(LedgerSyncWriter {
                store,
                last_sync_instant,
            }),
        });

        let collector = if config.enable_collector {
            let collector_keepalive = Arc::clone(&shared.keepalive);
            let on_activity: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                collector_keepalive.touch_activity();
            });
            match crate::collector::CollectorServerHandle::start_embedded(
                collector_shutdown,
                on_activity,
            ) {
                Ok(handle) => Some(handle),
                Err(err) => {
                    platform::write_error_log_safely(&format!(
                        "Embedded collector start error: {err}"
                    ));
                    None
                }
            }
        } else {
            None
        };

        let thread_shared = Arc::clone(&shared);
        let accept_thread = thread::Builder::new()
            .name("agy-pulse-dashboard".to_string())
            .spawn(move || run_accept_loop(listener, thread_shared))?;

        let info = DashboardServerInfo {
            url: format!("http://{LOOPBACK_HOST}:{actual_port}"),
            port: actual_port,
            host: LOOPBACK_HOST.to_string(),
            started_at,
            sync_report,
        };

        Ok(Self {
            info,
            shared,
            accept_thread: Some(accept_thread),
            collector,
        })
    }

    /// Metadata about the running server (bound port, loopback URL, initial sync report).
    pub fn info(&self) -> &DashboardServerInfo {
        &self.info
    }

    /// Process-level CSRF token injected into served dashboard HTML pages.
    pub fn csrf_token(&self) -> &str {
        self.shared.csrf_token.as_str()
    }

    /// Block until the server stops (e.g. via idle timeout or `shutdown()`).
    pub fn wait_until_stopped(&mut self) {
        if let Some(handle) = self.accept_thread.take() {
            let _ = handle.join();
        }
        self.shared
            .collector_shutdown
            .store(true, Ordering::Release);
        if let Some(mut collector) = self.collector.take() {
            collector.shutdown();
        }
    }

    /// Signal graceful shutdown and wait for the listener thread to exit.
    pub fn shutdown(&mut self) {
        self.shared.shutdown.store(true, Ordering::Release);
        self.shared
            .collector_shutdown
            .store(true, Ordering::Release);
        if let Some(handle) = self.accept_thread.take() {
            let _ = handle.join();
        }
        if let Some(mut collector) = self.collector.take() {
            collector.shutdown();
        }
    }
}

impl Drop for DashboardServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run_accept_loop(listener: TcpListener, shared: Arc<ServerSharedState>) {
    loop {
        if shared.shutdown.load(Ordering::Acquire)
            || shared.keepalive.should_shutdown()
            || platform::shutdown_signal_requested()
        {
            shared.shutdown.store(true, Ordering::Release);
            shared.collector_shutdown.store(true, Ordering::Release);
            break;
        }
        match listener.accept() {
            Ok((mut stream, _addr)) => {
                let _ = stream.set_nonblocking(false);
                let Some(conn_guard) = AtomicSlotGuard::try_acquire(
                    &shared.active_connections,
                    shared.max_concurrent_connections,
                ) else {
                    let _ = stream.set_write_timeout(Some(OVERLOAD_WRITE_TIMEOUT));
                    send_error_json(
                        &mut stream,
                        503,
                        "Service Unavailable: too many concurrent connections",
                    );
                    finish_early_error_response(&mut stream);
                    continue;
                };
                let conn_shared = Arc::clone(&shared);
                if let Err(err) = thread::Builder::new()
                    .name("agy-pulse-http".to_string())
                    .spawn(move || {
                        handle_connection(&mut stream, &conn_shared, conn_guard);
                    })
                {
                    platform::write_error_log_safely(&format!(
                        "Dashboard connection thread spawn failed: {err}"
                    ));
                }
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL_INTERVAL);
            }
            Err(_) => {
                thread::sleep(ACCEPT_POLL_INTERVAL);
            }
        }
    }
}

#[derive(Serialize)]
struct ErrorPayload<'a> {
    ok: bool,
    error: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthPayload<'a> {
    ok: bool,
    app: &'a str,
    pid: u32,
    port: u16,
    idle_timeout_ms: u64,
    close_grace_ms: u64,
    active_clients: usize,
    started_at: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PingPayload {
    ok: bool,
    active: bool,
    active_clients: usize,
    ts: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DisconnectPayload {
    ok: bool,
    active_clients: usize,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SyncRemoteBody {
    #[serde(default)]
    force: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncRemotePayload {
    ok: bool,
    enabled: bool,
    changed: bool,
    throttled: bool,
    in_progress: bool,
    pulled_chunks: usize,
    pushed_chunks: usize,
    steps_imported: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    sync_status: SyncStatusOverview,
}

struct RemoteSyncGuard<'a> {
    flag: &'a AtomicBool,
}

impl Drop for RemoteSyncGuard<'_> {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
    }
}

struct SseConnectionGuard {
    keepalive: Arc<KeepaliveState>,
    conn_id: u64,
}

impl Drop for SseConnectionGuard {
    fn drop(&mut self) {
        self.keepalive.unregister_sse(self.conn_id);
    }
}

fn serve_sse_keepalive(
    stream: &mut TcpStream,
    shared: &ServerSharedState,
    client_id: Option<&str>,
    conn_guard: Option<AtomicSlotGuard>,
) {
    let Some(_sse_slot_guard) =
        AtomicSlotGuard::try_acquire(&shared.active_sse_connections, shared.max_sse_connections)
    else {
        let _ = stream.set_write_timeout(Some(OVERLOAD_WRITE_TIMEOUT));
        send_error_json(stream, 503, "Service Unavailable: too many SSE connections");
        finish_early_error_response(stream);
        return;
    };
    drop(conn_guard);

    let conn_id = shared.keepalive.register_sse(client_id);
    let _guard = SseConnectionGuard {
        keepalive: Arc::clone(&shared.keepalive),
        conn_id,
    };

    let headers = "HTTP/1.1 200 OK\r\n\
         Content-Type: text/event-stream; charset=utf-8\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         X-Frame-Options: DENY\r\n\
         Referrer-Policy: no-referrer\r\n\
         Connection: close\r\n\r\n\
         : connected\n\n";
    if stream.write_all(headers.as_bytes()).is_err() || stream.flush().is_err() {
        return;
    }

    let _ = stream.set_read_timeout(Some(SSE_EOF_POLL_TIMEOUT));
    let mut last_ping = Instant::now();
    let mut buf = [0u8; 64];

    loop {
        if shared.shutdown.load(Ordering::Acquire) || !shared.keepalive.is_sse_active(conn_id) {
            break;
        }
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(err)
                if err.kind() == io::ErrorKind::WouldBlock
                    || err.kind() == io::ErrorKind::TimedOut => {}
            Err(_) => break,
        }
        if last_ping.elapsed() >= shared.sse_ping_interval {
            if stream.write_all(b": ping\n\n").is_err() || stream.flush().is_err() {
                break;
            }
            last_ping = Instant::now();
        }
    }
}

#[derive(Serialize)]
struct ManifestIcon<'a> {
    src: &'a str,
    sizes: &'a str,
    #[serde(rename = "type")]
    icon_type: &'a str,
    purpose: &'a str,
}

#[derive(Serialize)]
struct WebManifest<'a> {
    name: &'a str,
    short_name: &'a str,
    start_url: &'a str,
    display: &'a str,
    background_color: &'a str,
    theme_color: &'a str,
    icons: [ManifestIcon<'a>; 1],
}

pub(super) fn send_json_value<T: Serialize>(
    stream: &mut TcpStream,
    status: u16,
    payload: &T,
    extra_headers: &[(&str, &str)],
) {
    let body = serde_json::to_vec(payload)
        .unwrap_or_else(|_| br#"{"ok":false,"error":"Serialization error"}"#.to_vec());
    let _ = write_http_response(
        stream,
        status,
        "application/json; charset=utf-8",
        &body,
        true,
        extra_headers,
    );
}

fn send_error_json(stream: &mut TcpStream, status: u16, msg: &str) {
    send_json_value(
        stream,
        status,
        &ErrorPayload {
            ok: false,
            error: msg,
        },
        &[],
    );
}

fn check_mutating_route_security(
    stream: &mut TcpStream,
    req: &HttpRequest,
    shared: &ServerSharedState,
) -> bool {
    if !has_valid_csrf_token(req, &shared.csrf_token) {
        send_error_json(stream, 403, "Forbidden: invalid or missing CSRF token");
        return false;
    }
    if !is_strict_same_origin_with_required_origin(req, shared.port) {
        send_error_json(
            stream,
            403,
            "Forbidden: invalid or missing Origin / cross-origin request rejected",
        );
        return false;
    }
    true
}

fn parse_overview_filter(params: &[(String, String)]) -> (LedgerQueryFilter, String) {
    let range = query_param(params, "range")
        .and_then(LedgerTimeRange::from_raw)
        .unwrap_or(LedgerTimeRange::Last7d);

    let granularity = query_param(params, "granularity")
        .and_then(LedgerGranularity::from_raw)
        .unwrap_or(LedgerGranularity::Auto);

    let source = query_param(params, "source")
        .map(LedgerSourceFilter::from_raw)
        .unwrap_or(LedgerSourceFilter::All);

    let model = query_param(params, "model")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);

    let workspace = query_param(params, "workspace")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);

    let account = query_param(params, "account")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);

    let device = query_param(params, "device")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);

    let include_subagents = !matches!(query_param(params, "includeSubagents"), Some("false" | "0"));

    let from = query_param(params, "from")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);

    let to = query_param(params, "to")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);

    let safe_range: String = range
        .as_str()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();

    (
        LedgerQueryFilter {
            range,
            from,
            to,
            granularity,
            source,
            model,
            workspace,
            account,
            device,
            include_subagents,
            now_ms: None,
        },
        safe_range,
    )
}

fn current_epoch_ms_u64() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn handle_connection(
    stream: &mut TcpStream,
    shared: &ServerSharedState,
    conn_guard: AtomicSlotGuard,
) {
    let mut conn_guard = Some(conn_guard);
    let _ = stream.set_read_timeout(Some(SOCKET_IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(SOCKET_IO_TIMEOUT));

    let req = match read_http_request(stream) {
        Ok(r) => r,
        Err(HttpParseError::EmptyOrEof) => return,
        Err(HttpParseError::PayloadTooLarge) => {
            send_error_json(stream, 413, "request body too large");
            finish_early_error_response(stream);
            return;
        }
        Err(HttpParseError::InvalidRequest(msg)) => {
            send_error_json(stream, 400, msg);
            finish_early_error_response(stream);
            return;
        }
        Err(HttpParseError::TimedOut) => {
            send_error_json(stream, 408, "request timeout");
            return;
        }
        Err(HttpParseError::Io) => return,
    };

    if !is_allowed_host(req.header("host"), shared.port) {
        send_error_json(stream, 403, "Forbidden: invalid Host header");
        return;
    }

    dispatch_request(stream, &req, shared, &mut conn_guard);
}

fn read_sync_status_standalone(
    shared: &ServerSharedState,
    cfg: Option<&crate::sync::SyncConfig>,
    in_progress: bool,
) -> SyncStatusOverview {
    let summary = cfg.map(crate::sync::SyncConfig::to_summary);
    match LedgerStore::open_readonly_at(&shared.ledger_path) {
        Ok(store) => store.sync_status_overview(summary.as_ref(), in_progress),
        Err(_) => SyncStatusOverview {
            enabled: cfg.is_some_and(|c| c.enabled),
            remote_type: cfg.map(|c| c.remote_type_str().to_string()),
            in_progress,
            ..Default::default()
        },
    }
}

fn handle_sync_remote(
    stream: &mut TcpStream,
    req: &HttpRequest,
    params: &[(String, String)],
    shared: &ServerSharedState,
) {
    let body_force = serde_json::from_slice::<SyncRemoteBody>(&req.body).is_ok_and(|b| b.force);
    let force = body_force
        || matches!(
            query_param(params, "force").map(str::trim),
            Some(v) if v == "1" || v.eq_ignore_ascii_case("true")
        );

    let cfg = match load_sync_config() {
        Ok(Some(c)) if c.enabled => c,
        Ok(opt_cfg) => {
            let sync_status = read_sync_status_standalone(
                shared,
                opt_cfg.as_ref(),
                shared.sync_remote_in_progress.load(Ordering::Acquire),
            );
            send_json_value(
                stream,
                200,
                &SyncRemotePayload {
                    ok: true,
                    enabled: false,
                    changed: false,
                    throttled: false,
                    in_progress: false,
                    pulled_chunks: 0,
                    pushed_chunks: 0,
                    steps_imported: 0,
                    error: None,
                    sync_status,
                },
                &[],
            );
            return;
        }
        Err(err) => {
            let redacted = redact_sync_error_message(&err.to_string(), None);
            let mut sync_status = read_sync_status_standalone(shared, None, false);
            if sync_status.last_error.is_none() {
                sync_status.last_error = Some(redacted.clone());
            }
            send_json_value(
                stream,
                200,
                &SyncRemotePayload {
                    ok: false,
                    enabled: false,
                    changed: false,
                    throttled: false,
                    in_progress: false,
                    pulled_chunks: 0,
                    pushed_chunks: 0,
                    steps_imported: 0,
                    error: Some(redacted),
                    sync_status,
                },
                &[],
            );
            return;
        }
    };

    if !force
        && let Ok(last_guard) = shared.last_remote_sync_instant.lock()
        && let Some(prev) = *last_guard
        && (prev.elapsed().as_millis() as u64) < REMOTE_SYNC_THROTTLE_MS
    {
        drop(last_guard);
        let in_progress = shared.sync_remote_in_progress.load(Ordering::Acquire);
        let sync_status = read_sync_status_standalone(shared, Some(&cfg), in_progress);
        send_json_value(
            stream,
            200,
            &SyncRemotePayload {
                ok: true,
                enabled: true,
                changed: false,
                throttled: true,
                in_progress,
                pulled_chunks: 0,
                pushed_chunks: 0,
                steps_imported: 0,
                error: None,
                sync_status,
            },
            &[],
        );
        return;
    }

    if shared
        .sync_remote_in_progress
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        let sync_status = read_sync_status_standalone(shared, Some(&cfg), true);
        send_json_value(
            stream,
            200,
            &SyncRemotePayload {
                ok: true,
                enabled: true,
                changed: false,
                throttled: false,
                in_progress: true,
                pulled_chunks: 0,
                pushed_chunks: 0,
                steps_imported: 0,
                error: None,
                sync_status,
            },
            &[],
        );
        return;
    }

    let guard = RemoteSyncGuard {
        flag: &shared.sync_remote_in_progress,
    };

    let store = match LedgerStore::open_at(&shared.ledger_path) {
        Ok(s) => s,
        Err(err) => {
            let summary = cfg.to_summary();
            let redacted = redact_sync_error_message(&err.to_string(), Some(&summary));
            drop(guard);
            let sync_status = SyncStatusOverview {
                enabled: true,
                remote_type: Some(cfg.remote_type_str().to_string()),
                last_error: Some(redacted.clone()),
                in_progress: false,
                ..Default::default()
            };
            send_json_value(
                stream,
                200,
                &SyncRemotePayload {
                    ok: false,
                    enabled: true,
                    changed: false,
                    throttled: false,
                    in_progress: false,
                    pulled_chunks: 0,
                    pushed_chunks: 0,
                    steps_imported: 0,
                    error: Some(redacted),
                    sync_status,
                },
                &[],
            );
            return;
        }
    };

    let before_quotas = store.query_latest_account_quotas().unwrap_or_default();
    let sync_result =
        build_remote(&cfg).and_then(|remote| sync_both(&store, remote.as_ref(), &cfg, false));
    let cfg_summary = cfg.to_summary();

    match sync_result {
        Ok(outcome) if outcome.kind == SyncOutcomeKind::AlreadyRunning => {
            drop(guard);
            let sync_status = store.sync_status_overview(Some(&cfg_summary), true);
            send_json_value(
                stream,
                200,
                &SyncRemotePayload {
                    ok: true,
                    enabled: true,
                    changed: false,
                    throttled: false,
                    in_progress: true,
                    pulled_chunks: 0,
                    pushed_chunks: 0,
                    steps_imported: 0,
                    error: None,
                    sync_status,
                },
                &[],
            );
        }
        Ok(outcome) => {
            if let Ok(mut last_guard) = shared.last_remote_sync_instant.lock() {
                *last_guard = Some(Instant::now());
            }
            let after_quotas = store.query_latest_account_quotas().unwrap_or_default();
            let quotas_changed = before_quotas != after_quotas;
            let changed = outcome.chunks_downloaded > 0
                || outcome.steps_imported > 0
                || outcome.remote_cache_cleared
                || quotas_changed;
            drop(guard);
            let sync_status = store.sync_status_overview(Some(&cfg_summary), false);
            send_json_value(
                stream,
                200,
                &SyncRemotePayload {
                    ok: true,
                    enabled: true,
                    changed,
                    throttled: false,
                    in_progress: false,
                    pulled_chunks: outcome.chunks_downloaded,
                    pushed_chunks: outcome.chunks_uploaded,
                    steps_imported: outcome.steps_imported,
                    error: None,
                    sync_status,
                },
                &[],
            );
        }
        Err(err) => {
            let redacted = redact_sync_error_message(&err.to_string(), Some(&cfg_summary));
            let _ = store.set_sync_meta_field("last_sync_error", Some(&redacted));
            drop(guard);
            let mut sync_status = store.sync_status_overview(Some(&cfg_summary), false);
            sync_status.last_error = Some(redacted.clone());
            send_json_value(
                stream,
                200,
                &SyncRemotePayload {
                    ok: false,
                    enabled: true,
                    changed: false,
                    throttled: false,
                    in_progress: false,
                    pulled_chunks: 0,
                    pushed_chunks: 0,
                    steps_imported: 0,
                    error: Some(redacted),
                    sync_status,
                },
                &[],
            );
        }
    }
}

fn dispatch_request(
    stream: &mut TcpStream,
    req: &HttpRequest,
    shared: &ServerSharedState,
    conn_guard: &mut Option<AtomicSlotGuard>,
) {
    let method = req.method.as_str();
    let params = parse_query_params(&req.query);

    match req.pathname.as_str() {
        "/api/config/pulse" => {
            super::config_api::handle_pulse_config_route(stream, req, shared);
        }
        "/api/config/pulse/preview" => {
            super::config_api::handle_pulse_preview_route(stream, req, shared);
        }
        "/api/config/sync" => {
            super::config_api::handle_sync_config_route(stream, req, shared);
        }
        "/api/config/sync/test" => {
            super::config_api::handle_sync_test_route(stream, req, shared);
        }
        "/api/health" => {
            if method != "GET" {
                send_error_json(stream, 405, "Method Not Allowed");
                return;
            }
            send_json_value(
                stream,
                200,
                &HealthPayload {
                    ok: true,
                    app: DASHBOARD_APP_ID,
                    pid: std::process::id(),
                    port: shared.port,
                    idle_timeout_ms: shared.idle_timeout_ms,
                    close_grace_ms: shared.keepalive.close_grace_ms(),
                    active_clients: shared.keepalive.active_client_count(),
                    started_at: &shared.started_at,
                },
                &[],
            );
        }
        "/api/keepalive" => {
            if method != "GET" {
                send_error_json(stream, 405, "Method Not Allowed");
                return;
            }
            if !is_strict_same_origin(req, shared.port) {
                send_error_json(stream, 403, "Forbidden: cross-origin keepalive rejected");
                return;
            }
            serve_sse_keepalive(
                stream,
                shared,
                query_param(&params, "client"),
                conn_guard.take(),
            );
        }
        "/api/ping" => {
            if method != "POST" {
                send_error_json(stream, 405, "Method Not Allowed");
                return;
            }
            if !check_mutating_route_security(stream, req, shared) {
                return;
            }
            let active_clients = shared.keepalive.record_ping(query_param(&params, "client"));
            send_json_value(
                stream,
                200,
                &PingPayload {
                    ok: true,
                    active: true,
                    active_clients,
                    ts: current_epoch_ms_u64(),
                },
                &[],
            );
        }
        "/api/disconnect" => {
            if method != "POST" {
                send_error_json(stream, 405, "Method Not Allowed");
                return;
            }
            if !check_mutating_route_security(stream, req, shared) {
                return;
            }
            let active_clients = shared
                .keepalive
                .record_disconnect(query_param(&params, "client"));
            send_json_value(
                stream,
                200,
                &DisconnectPayload {
                    ok: true,
                    active_clients,
                },
                &[],
            );
        }
        "/icon.svg" => {
            if method != "GET" && method != "HEAD" {
                send_error_json(stream, 405, "Method Not Allowed");
                return;
            }
            let _ = write_http_response(
                stream,
                200,
                "image/svg+xml; charset=utf-8",
                DASHBOARD_ICON_SVG.as_bytes(),
                method != "HEAD",
                &[],
            );
        }
        "/manifest.webmanifest" => {
            if method != "GET" && method != "HEAD" {
                send_error_json(stream, 405, "Method Not Allowed");
                return;
            }
            let manifest = WebManifest {
                name: "agy-pulse Telemetry Dashboard",
                short_name: "agy-pulse",
                start_url: "/",
                display: "standalone",
                background_color: "#0b1120",
                theme_color: "#0b1120",
                icons: [ManifestIcon {
                    src: "/icon.svg",
                    sizes: "any",
                    icon_type: "image/svg+xml",
                    purpose: "any",
                }],
            };
            let body = serde_json::to_vec_pretty(&manifest).unwrap_or_default();
            let _ = write_http_response(
                stream,
                200,
                "application/manifest+json; charset=utf-8",
                &body,
                method != "HEAD",
                &[],
            );
        }
        "/" | "/index.html" | "/settings" | "/settings/" | "/settings/pulse" | "/settings/sync" => {
            if method != "GET" && method != "HEAD" {
                send_error_json(stream, 405, "Method Not Allowed");
                return;
            }
            shared.keepalive.touch_page_load();
            let initial_lang = match query_param(&params, "lang") {
                Some("zh") => LanguageSetting::Zh,
                Some("en") => LanguageSetting::En,
                _ => shared.default_lang,
            };
            let html = render_dashboard_html_with_token(initial_lang, &shared.csrf_token);
            let _ = write_http_response(
                stream,
                200,
                "text/html; charset=utf-8",
                html.as_bytes(),
                method != "HEAD",
                &[("Content-Security-Policy", DASHBOARD_CSP)],
            );
        }
        "/api/overview" => {
            if method != "GET" {
                send_error_json(stream, 405, "Method Not Allowed");
                return;
            }
            shared.keepalive.touch_activity();
            let (filter, _) = parse_overview_filter(&params);
            shared.maybe_auto_sync();
            let store = match LedgerStore::open_readonly_at(&shared.ledger_path) {
                Ok(s) => s,
                Err(err) => {
                    send_error_json(stream, 500, &err.to_string());
                    return;
                }
            };
            match store.query_overview(&filter) {
                Ok(mut overview) => {
                    overview.sync_status.in_progress =
                        shared.sync_remote_in_progress.load(Ordering::Acquire);
                    send_json_value(stream, 200, &overview, &[]);
                }
                Err(err) => send_error_json(stream, 500, &err.to_string()),
            }
        }
        "/api/sync" => {
            if method != "POST" {
                send_error_json(stream, 405, "Method Not Allowed");
                return;
            }
            if !check_mutating_route_security(stream, req, shared) {
                return;
            }
            shared.keepalive.touch_activity();
            let Ok(mut guard) = shared.sync_write.lock() else {
                send_error_json(stream, 500, "Ledger sync lock poisoned");
                return;
            };
            let sync_opts = {
                let Ok(mut opts_guard) = shared.sync_options.lock() else {
                    send_error_json(stream, 500, "Sync options lock poisoned");
                    return;
                };
                refresh_sync_identity_hints(&mut opts_guard);
                opts_guard.clone()
            };
            match guard.store.sync_all(&sync_opts) {
                Ok(report) => {
                    guard.last_sync_instant = Some(Instant::now());
                    send_json_value(stream, 200, &report, &[]);
                }
                Err(err) => send_error_json(stream, 500, &err.to_string()),
            }
        }
        "/api/sync-remote" => {
            if method != "POST" {
                send_error_json(stream, 405, "Method Not Allowed");
                return;
            }
            if !check_mutating_route_security(stream, req, shared) {
                return;
            }
            shared.keepalive.touch_activity();
            handle_sync_remote(stream, req, &params, shared);
        }
        "/api/export.csv" => {
            if method != "GET" {
                send_error_json(stream, 405, "Method Not Allowed");
                return;
            }
            shared.keepalive.touch_activity();
            let (filter, safe_range) = parse_overview_filter(&params);
            let kind = query_param(&params, "kind")
                .and_then(CsvExportKind::from_raw)
                .unwrap_or(CsvExportKind::Timeseries);
            let store = match LedgerStore::open_readonly_at(&shared.ledger_path) {
                Ok(s) => s,
                Err(err) => {
                    send_error_json(stream, 500, &err.to_string());
                    return;
                }
            };
            match store.export_csv(&filter, kind) {
                Ok(csv) => {
                    let disposition =
                        format!("attachment; filename=\"agy-pulse-telemetry-{safe_range}.csv\"");
                    let _ = write_http_response(
                        stream,
                        200,
                        "text/csv; charset=utf-8",
                        csv.as_bytes(),
                        true,
                        &[("Content-Disposition", &disposition)],
                    );
                }
                Err(err) => send_error_json(stream, 500, &err.to_string()),
            }
        }
        "/api/export.json" => {
            if method != "GET" {
                send_error_json(stream, 405, "Method Not Allowed");
                return;
            }
            shared.keepalive.touch_activity();
            let (filter, safe_range) = parse_overview_filter(&params);
            let store = match LedgerStore::open_readonly_at(&shared.ledger_path) {
                Ok(s) => s,
                Err(err) => {
                    send_error_json(stream, 500, &err.to_string());
                    return;
                }
            };
            match store.export_json(&filter) {
                Ok(json) => {
                    let disposition =
                        format!("attachment; filename=\"agy-pulse-telemetry-{safe_range}.json\"");
                    let _ = write_http_response(
                        stream,
                        200,
                        "application/json; charset=utf-8",
                        json.as_bytes(),
                        true,
                        &[("Content-Disposition", &disposition)],
                    );
                }
                Err(err) => send_error_json(stream, 500, &err.to_string()),
            }
        }
        _ => {
            send_error_json(stream, 404, "Not Found");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;

    static SYNC_CFG_ENV_LOCK: Mutex<()> = Mutex::new(());

    struct SyncCfgEnvGuard(Option<std::ffi::OsString>);

    impl Drop for SyncCfgEnvGuard {
        fn drop(&mut self) {
            match self.0.take() {
                // SAFETY: guarded by `SYNC_CFG_ENV_LOCK`.
                Some(v) => unsafe { std::env::set_var("AGY_PULSE_SYNC_CONFIG", v) },
                None => unsafe { std::env::remove_var("AGY_PULSE_SYNC_CONFIG") },
            }
        }
    }

    #[test]
    fn csrf_token_uses_csprng_64_hex_chars() {
        let Ok(t1) = generate_csrf_token() else {
            panic!("generate_csrf_token failed");
        };
        let Ok(t2) = generate_csrf_token() else {
            panic!("generate_csrf_token failed");
        };
        assert_eq!(t1.len(), 64);
        assert_eq!(t2.len(), 64);
        assert_ne!(t1, t2);
        assert!(t1.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
    }

    #[test]
    fn throttled_sync_remote_does_not_flip_in_progress_flag() {
        let _lock = SYNC_CFG_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let Ok(temp) = TempDir::new() else {
            panic!("tempdir");
        };
        let sync_cfg_path = temp.path().join("sync-cfg.json");
        let remote_dir = temp.path().join("remote");
        let _ = std::fs::create_dir_all(&remote_dir);
        let cfg = crate::sync::SyncConfig {
            enabled: true,
            device_id: None,
            device_name: Some("dev-test".to_string()),
            default_account: None,
            push_interval_sec: 300,
            prefix: "agy-pulse-sync".to_string(),
            remote: crate::sync::RemoteConfig::LocalDir {
                path: remote_dir.to_string_lossy().into_owned(),
            },
        };
        let _ = crate::sync::save_sync_config_to(&sync_cfg_path, &cfg);
        let _env_guard = SyncCfgEnvGuard(std::env::var_os("AGY_PULSE_SYNC_CONFIG"));
        // SAFETY: serialized via SYNC_CFG_ENV_LOCK.
        unsafe { std::env::set_var("AGY_PULSE_SYNC_CONFIG", &sync_cfg_path) };

        let Ok(mut server) = DashboardServer::start(DashboardServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
            no_sync: true,
            defer_initial_sync: true,
            ledger_path: Some(temp.path().join("ledger.sqlite")),
            ..Default::default()
        }) else {
            panic!("start server");
        };

        if let Ok(mut last) = server.shared.last_remote_sync_instant.lock() {
            *last = Some(Instant::now());
        }

        let stop_observer = Arc::new(AtomicBool::new(false));
        let observed_true = Arc::new(AtomicBool::new(false));
        let obs_flag = Arc::clone(&server.shared.sync_remote_in_progress);
        let obs_stop = Arc::clone(&stop_observer);
        let obs_hit = Arc::clone(&observed_true);
        let observer = thread::spawn(move || {
            while !obs_stop.load(Ordering::Acquire) {
                if obs_flag.load(Ordering::Acquire) {
                    obs_hit.store(true, Ordering::Release);
                }
            }
        });

        let port = server.info().port;
        let origin = format!("http://127.0.0.1:{port}");
        let token = server.csrf_token().to_string();
        for _ in 0..80 {
            if let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)) {
                let req = format!(
                    "POST /api/sync-remote HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: {origin}\r\nX-Agy-Pulse-Token: {token}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = s.write_all(req.as_bytes());
                let _ = s.flush();
                let mut sink = Vec::new();
                let _ = s.read_to_end(&mut sink);
            }
        }

        stop_observer.store(true, Ordering::Release);
        let _ = observer.join();
        server.shutdown();

        assert!(
            !observed_true.load(Ordering::Acquire),
            "throttled POST /api/sync-remote must never flip sync_remote_in_progress to true"
        );
    }

    #[test]
    fn atomic_slot_guard_releases_quota_on_normal_drop_and_panic() {
        let counter = Arc::new(AtomicUsize::new(0));
        let Some(g1) = AtomicSlotGuard::try_acquire(&counter, 2) else {
            panic!("slot 1");
        };
        let Some(g2) = AtomicSlotGuard::try_acquire(&counter, 2) else {
            panic!("slot 2");
        };
        assert!(AtomicSlotGuard::try_acquire(&counter, 2).is_none());
        assert_eq!(counter.load(Ordering::Acquire), 2);

        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _held = g2;
            panic!("simulated handler panic");
        }));
        assert!(panicked.is_err());
        assert_eq!(counter.load(Ordering::Acquire), 1);

        drop(g1);
        assert_eq!(counter.load(Ordering::Acquire), 0);
        assert!(AtomicSlotGuard::try_acquire(&counter, 2).is_some());
    }
}
