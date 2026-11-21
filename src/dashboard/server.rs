use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::{SecondsFormat, Utc};
use serde::Serialize;

use crate::config::LanguageSetting;
use crate::ledger::{
    CsvExportKind, LedgerError, LedgerGranularity, LedgerQueryFilter, LedgerSourceFilter,
    LedgerStore, LedgerSyncOptions, LedgerSyncReport, LedgerTimeRange, SourceRoot,
};
use crate::platform;
use crate::render::resolve_language_from_locale;

use super::http_wire::{
    HttpParseError, HttpRequest, is_allowed_host, is_allowed_origin, parse_query_params,
    query_param, read_http_request, write_http_response,
};
use super::keepalive::{
    KeepaliveState, resolve_close_grace_ms, resolve_ping_lease_ms, resolve_sse_ping_interval_ms,
};
use super::{DASHBOARD_APP_ID, DASHBOARD_ICON_SVG, DEFAULT_DASHBOARD_PORT, LOOPBACK_HOST, render_dashboard_html};

const DEFAULT_AUTO_SYNC_INTERVAL_MS: u64 = 10_000;
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(15);
const SSE_EOF_POLL_TIMEOUT: Duration = Duration::from_millis(50);
const SOCKET_IO_TIMEOUT: Duration = Duration::from_secs(5);
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

struct LedgerRuntime {
    store: LedgerStore,
    last_sync_instant: Option<Instant>,
    sync_options: LedgerSyncOptions,
}

struct ServerSharedState {
    port: u16,
    default_lang: LanguageSetting,
    no_sync: bool,
    idle_timeout_ms: u64,
    auto_sync_interval_ms: u64,
    sse_ping_interval: Duration,
    started_at: String,
    keepalive: Arc<KeepaliveState>,
    shutdown: AtomicBool,
    ledger: Mutex<LedgerRuntime>,
}

impl ServerSharedState {
    fn maybe_auto_sync(&self, guard: &mut LedgerRuntime) {
        if self.no_sync || self.auto_sync_interval_ms == 0 {
            return;
        }
        if let Some(prev) = guard.last_sync_instant
            && (prev.elapsed().as_millis() as u64) < self.auto_sync_interval_ms
        {
            return;
        }
        match guard.store.sync_all(&guard.sync_options) {
            Ok(_) => {
                guard.last_sync_instant = Some(Instant::now());
            }
            Err(err) => {
                platform::write_error_log_safely(&format!(
                    "Dashboard auto-sync error: {err}"
                ));
            }
        }
    }
}

/// Single-binary lightweight `127.0.0.1` HTTP/1.1 server for the Telemetry Dashboard.
pub struct DashboardServer {
    info: DashboardServerInfo,
    shared: Arc<ServerSharedState>,
    accept_thread: Option<JoinHandle<()>>,
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
        let sync_options = LedgerSyncOptions {
            source_roots: config.source_roots,
            ..Default::default()
        };

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

        let shared = Arc::new(ServerSharedState {
            port: actual_port,
            default_lang,
            no_sync: config.no_sync,
            idle_timeout_ms: config.idle_timeout_ms,
            auto_sync_interval_ms: config.auto_sync_interval_ms,
            sse_ping_interval,
            started_at: started_at.clone(),
            keepalive,
            shutdown: AtomicBool::new(false),
            ledger: Mutex::new(LedgerRuntime {
                store,
                last_sync_instant,
                sync_options,
            }),
        });

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
        })
    }

    /// Metadata about the running server (bound port, loopback URL, initial sync report).
    pub fn info(&self) -> &DashboardServerInfo {
        &self.info
    }

    /// Block until the server stops (e.g. via idle timeout or `shutdown()`).
    pub fn wait_until_stopped(&mut self) {
        if let Some(handle) = self.accept_thread.take() {
            let _ = handle.join();
        }
    }

    /// Signal graceful shutdown and wait for the listener thread to exit.
    pub fn shutdown(&mut self) {
        self.shared.shutdown.store(true, Ordering::Release);
        if let Some(handle) = self.accept_thread.take() {
            let _ = handle.join();
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
            break;
        }
        match listener.accept() {
            Ok((mut stream, _addr)) => {
                let conn_shared = Arc::clone(&shared);
                thread::spawn(move || {
                    handle_connection(&mut stream, &conn_shared);
                });
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
) {
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

fn send_json_value<T: Serialize>(
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
    send_json_value(stream, status, &ErrorPayload { ok: false, error: msg }, &[]);
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

    let include_subagents = !matches!(
        query_param(params, "includeSubagents"),
        Some("false" | "0")
    );

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

fn handle_connection(stream: &mut TcpStream, shared: &ServerSharedState) {
    let _ = stream.set_read_timeout(Some(SOCKET_IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(SOCKET_IO_TIMEOUT));

    let req = match read_http_request(stream) {
        Ok(r) => r,
        Err(HttpParseError::EmptyOrEof) => return,
        Err(HttpParseError::InvalidRequest(msg)) => {
            send_error_json(stream, 400, msg);
            return;
        }
        Err(HttpParseError::Io) => return,
    };

    if !is_allowed_host(req.header("host"), shared.port) {
        send_error_json(stream, 403, "Forbidden: invalid Host header");
        return;
    }

    dispatch_request(stream, &req, shared);
}

fn dispatch_request(stream: &mut TcpStream, req: &HttpRequest, shared: &ServerSharedState) {
    let method = req.method.as_str();
    let params = parse_query_params(&req.query);

    match req.pathname.as_str() {
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
            if !is_allowed_origin(req.header("origin"), shared.port) {
                send_error_json(stream, 403, "Forbidden: invalid Origin header");
                return;
            }
            serve_sse_keepalive(stream, shared, query_param(&params, "client"));
        }
        "/api/ping" => {
            if method != "POST" {
                send_error_json(stream, 405, "Method Not Allowed");
                return;
            }
            if !is_allowed_origin(req.header("origin"), shared.port) {
                send_error_json(stream, 403, "Forbidden: invalid Origin header");
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
            if !is_allowed_origin(req.header("origin"), shared.port) {
                send_error_json(stream, 403, "Forbidden: invalid Origin header");
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
        "/" | "/index.html" => {
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
            let html = render_dashboard_html(initial_lang);
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
            let Ok(mut guard) = shared.ledger.lock() else {
                send_error_json(stream, 500, "Ledger lock poisoned");
                return;
            };
            shared.maybe_auto_sync(&mut guard);
            match guard.store.query_overview(&filter) {
                Ok(overview) => send_json_value(stream, 200, &overview, &[]),
                Err(err) => send_error_json(stream, 500, &err.to_string()),
            }
        }
        "/api/sync" => {
            if method != "POST" {
                send_error_json(stream, 405, "Method Not Allowed");
                return;
            }
            if !is_allowed_origin(req.header("origin"), shared.port) {
                send_error_json(stream, 403, "Forbidden: invalid Origin header");
                return;
            }
            shared.keepalive.touch_activity();
            let Ok(mut guard) = shared.ledger.lock() else {
                send_error_json(stream, 500, "Ledger lock poisoned");
                return;
            };
            let sync_opts = guard.sync_options.clone();
            match guard.store.sync_all(&sync_opts) {
                Ok(report) => {
                    guard.last_sync_instant = Some(Instant::now());
                    send_json_value(stream, 200, &report, &[]);
                }
                Err(err) => send_error_json(stream, 500, &err.to_string()),
            }
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
            let Ok(guard) = shared.ledger.lock() else {
                send_error_json(stream, 500, "Ledger lock poisoned");
                return;
            };
            match guard.store.export_csv(&filter, kind) {
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
            let Ok(guard) = shared.ledger.lock() else {
                send_error_json(stream, 500, "Ledger lock poisoned");
                return;
            };
            match guard.store.export_json(&filter) {
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
