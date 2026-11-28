#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};
use serde_json::Value;
use tempfile::TempDir;

use agy_pulse::config::LanguageSetting;
use agy_pulse::dashboard::{
    DASHBOARD_APP_ID, DASHBOARD_ICON_SVG, DEFAULT_DASHBOARD_PORT, DashboardAction,
    DashboardServer, DashboardServerConfig, parse_dashboard_args,
};

fn agypl_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agypl"))
}

fn agy_pulse_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agy-pulse"))
}

struct IsolatedEnv {
    _temp: TempDir,
    home_dir: PathBuf,
    cli_data_dir: PathBuf,
    ide_data_dir: PathBuf,
    global_cfg_dir: PathBuf,
    xdg_cfg_dir: PathBuf,
    workspace_dir: PathBuf,
}

impl IsolatedEnv {
    fn new() -> Self {
        let temp = TempDir::new().expect("create temp dir");
        let root = temp.path();
        let home_dir = root.join("home");
        let cli_data_dir = root.join("cli_data");
        let ide_data_dir = root.join("ide_data");
        let global_cfg_dir = root.join("global_cfg");
        let xdg_cfg_dir = root.join("xdg_cfg");
        let workspace_dir = root.join("workspace");
        fs::create_dir_all(&home_dir).unwrap();
        fs::create_dir_all(&cli_data_dir).unwrap();
        fs::create_dir_all(&ide_data_dir).unwrap();
        fs::create_dir_all(&global_cfg_dir).unwrap();
        fs::create_dir_all(&xdg_cfg_dir).unwrap();
        fs::create_dir_all(&workspace_dir).unwrap();
        Self {
            _temp: temp,
            home_dir,
            cli_data_dir,
            ide_data_dir,
            global_cfg_dir,
            xdg_cfg_dir,
            workspace_dir,
        }
    }

    fn ledger_path(&self) -> PathBuf {
        self.cli_data_dir.join("agy-pulse-ledger.sqlite")
    }

    fn base_cmd(&self, bin: &Path) -> Command {
        let mut cmd = Command::new(bin);
        cmd.current_dir(&self.workspace_dir)
            .env("HOME", &self.home_dir)
            .env("USERPROFILE", &self.home_dir)
            .env("XDG_DATA_HOME", self.home_dir.join(".local/share"))
            .env("XDG_CONFIG_HOME", &self.xdg_cfg_dir)
            .env("AGY_PULSE_DATA_DIR", &self.cli_data_dir)
            .env("AGY_PULSE_IDE_DATA_DIR", &self.ide_data_dir)
            .env("AGY_PULSE_GLOBAL_CONFIG_DIR", &self.global_cfg_dir)
            .env("LC_ALL", "en_US.UTF-8")
            .env("LANG", "en_US.UTF-8");
        cmd
    }
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// --- Minimal Protobuf encoders for test host conversation DBs ---

fn encode_varint(mut v: u64) -> Vec<u8> {
    let mut out = Vec::new();
    while v >= 0x80 {
        out.push(((v & 0x7f) as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
    out
}

fn pb_varint(field_num: u32, val: u64) -> Vec<u8> {
    let mut out = encode_varint(u64::from(field_num) << 3);
    out.extend(encode_varint(val));
    out
}

fn pb_bytes(field_num: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = encode_varint((u64::from(field_num) << 3) | 2);
    out.extend(encode_varint(payload.len() as u64));
    out.extend_from_slice(payload);
    out
}

fn pb_str(field_num: u32, s: &str) -> Vec<u8> {
    pb_bytes(field_num, s.as_bytes())
}

fn pb_timestamp_ms(field_num: u32, ts_ms: i64) -> Vec<u8> {
    let sec = (ts_ms / 1000) as u64;
    let nanos = ((ts_ms % 1000) * 1_000_000) as u64;
    let mut inner = pb_varint(1, sec);
    if nanos > 0 {
        inner.extend(pb_varint(2, nanos));
    }
    pb_bytes(field_num, &inner)
}

fn pb_duration_sec(field_num: u32, sec: u64, nanos: u64) -> Vec<u8> {
    let mut inner = Vec::new();
    if sec > 0 {
        inner.extend(pb_varint(1, sec));
    }
    if nanos > 0 {
        inner.extend(pb_varint(2, nanos));
    }
    pb_bytes(field_num, &inner)
}

fn build_step_metadata_blob(ts_ms: i64) -> Vec<u8> {
    pb_timestamp_ms(1, ts_ms)
}

fn build_trajectory_blob(
    workspace_uri: &str,
    branch: &str,
    created_at_ms: i64,
    cascade_id: Option<&str>,
) -> Vec<u8> {
    let mut ws = pb_str(1, workspace_uri);
    ws.extend(pb_str(4, branch));
    let mut out = pb_bytes(1, &ws);
    out.extend(pb_timestamp_ms(2, created_at_ms));
    if let Some(cid) = cascade_id {
        out.extend(pb_str(6, cid));
    }
    out
}

struct GenStepSpec<'a> {
    display_name: &'a str,
    input_tokens: u64,
    cache_read_tokens: u64,
    output_tokens: u64,
    stream_sec: Option<(u64, u64)>,
    ttft_sec: Option<(u64, u64)>,
    parent_cascade_id: Option<&'a str>,
    root_cascade_id: Option<&'a str>,
}

fn build_gen_blob(step_indices: &[u64], entries: &[GenStepSpec<'_>]) -> Vec<u8> {
    let mut out = Vec::new();
    for &idx in step_indices {
        out.extend(pb_varint(2, idx));
    }
    for e in entries {
        let mut f1 = pb_str(21, e.display_name);
        let mut usage = Vec::new();
        if e.input_tokens > 0 {
            usage.extend(pb_varint(2, e.input_tokens));
        }
        if e.output_tokens > 0 {
            usage.extend(pb_varint(3, e.output_tokens));
        }
        if e.cache_read_tokens > 0 {
            usage.extend(pb_varint(5, e.cache_read_tokens));
        }
        f1.extend(pb_bytes(4, &usage));
        if let Some((sec, nanos)) = e.ttft_sec {
            f1.extend(pb_duration_sec(11, sec, nanos));
        }
        if let Some((sec, nanos)) = e.stream_sec {
            f1.extend(pb_duration_sec(12, sec, nanos));
        }
        for (k, v) in [
            ("parent_cascade_id", e.parent_cascade_id),
            ("root_cascade_id", e.root_cascade_id),
        ] {
            if let Some(val) = v {
                let mut kv = pb_str(1, k);
                kv.extend(pb_str(2, val));
                f1.extend(pb_bytes(20, &kv));
            }
        }
        out.extend(pb_bytes(1, &f1));
    }
    out
}

fn write_conv_db(
    db_path: &Path,
    trajectory: Option<&[u8]>,
    steps: &[(i64, Vec<u8>)],
    gen_rows: &[(i64, Vec<u8>)],
) {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let conn = Connection::open(db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS steps (idx INTEGER PRIMARY KEY, metadata BLOB);
         CREATE TABLE IF NOT EXISTS gen_metadata (idx INTEGER PRIMARY KEY, data BLOB);
         CREATE TABLE IF NOT EXISTS trajectory_metadata_blob (id TEXT PRIMARY KEY, data BLOB);",
    )
    .unwrap();
    for (idx, meta) in steps {
        conn.execute(
            "INSERT INTO steps (idx, metadata) VALUES (?1, ?2)",
            params![idx, meta],
        )
        .unwrap();
    }
    for (idx, data) in gen_rows {
        conn.execute(
            "INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)",
            params![idx, data],
        )
        .unwrap();
    }
    if let Some(traj) = trajectory {
        conn.execute(
            "INSERT INTO trajectory_metadata_blob (id, data) VALUES ('main', ?1)",
            params![traj],
        )
        .unwrap();
    }
}

struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl HttpResponse {
    fn header(&self, name: &str) -> Option<&str> {
        let target = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| k.to_ascii_lowercase() == target)
            .map(|(_, v)| v.as_str())
    }
}

fn send_raw_http(
    port: u16,
    method: &str,
    target: &str,
    headers: &[(&str, &str)],
) -> HttpResponse {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect loopback");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    let mut req = format!("{method} {target} HTTP/1.1\r\n");
    let mut has_host = false;
    for (k, v) in headers {
        if k.eq_ignore_ascii_case("host") {
            has_host = true;
        }
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if !has_host {
        req.push_str(&format!("Host: 127.0.0.1:{port}\r\n"));
    }
    req.push_str("Connection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).unwrap();
    stream.flush().unwrap();

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text
        .split_once("\r\n\r\n")
        .expect("HTTP response header/body separator");
    let mut lines = head.lines();
    let status_line = lines.next().expect("status line");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .expect("status code")
        .parse()
        .expect("parse status code");
    let parsed_headers = lines
        .filter_map(|line| {
            let (k, v) = line.split_once(':')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect();

    HttpResponse {
        status,
        headers: parsed_headers,
        body: body.to_string(),
    }
}

fn extract_html_csrf_token(port: u16) -> String {
    let html_res = send_raw_http(port, "GET", "/", &[]);
    assert_eq!(html_res.status, 200);
    let marker = "var CSRF_TOKEN = '";
    let start = html_res
        .body
        .find(marker)
        .expect("dashboard HTML must contain CSRF_TOKEN")
        + marker.len();
    let rest = &html_res.body[start..];
    let end = rest.find('\'').expect("closing quote for CSRF_TOKEN");
    rest[..end].to_string()
}

fn current_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

#[test]
fn t07_loopback_binding_port_fallback_security_headers_and_403_405_404() {
    let env = IsolatedEnv::new();

    // 1. CLI argument parsing parity
    let defaults = parse_dashboard_args(&[], None);
    assert_eq!(defaults.port, DEFAULT_DASHBOARD_PORT);
    assert!(defaults.open);
    assert_eq!(defaults.host, "127.0.0.1");
    assert_eq!(defaults.lang, LanguageSetting::Auto);
    assert!(!defaults.no_sync);
    assert_eq!(defaults.action, DashboardAction::Serve);

    let parsed = parse_dashboard_args(
        &[
            "dash".into(),
            "--port".into(),
            "4123".into(),
            "--no-open".into(),
            "--no-sync".into(),
            "--zh".into(),
        ],
        None,
    );
    assert_eq!(parsed.port, 4123);
    assert!(!parsed.open);
    assert!(parsed.no_sync);
    assert_eq!(parsed.lang, LanguageSetting::Zh);

    let invalid_port = parse_dashboard_args(&["--port=99999".into()], None);
    assert_eq!(invalid_port.port, DEFAULT_DASHBOARD_PORT);

    // 2. Non-loopback host binding must be rejected
    for bad_host in ["0.0.0.0", "localhost", "::1", "192.168.1.10"] {
        let err = DashboardServer::start(DashboardServerConfig {
            host: bad_host.to_string(),
            port: 0,
            no_sync: true,
            ledger_path: Some(env.ledger_path()),
            ..Default::default()
        })
        .err()
        .expect("non-127.0.0.1 host must error");
        assert!(
            err.to_string().contains("127.0.0.1"),
            "error must mention 127.0.0.1: {err}"
        );
    }

    // 3. Port collision automatically falls back to ephemeral port
    let blocker = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let occupied_port = blocker.local_addr().unwrap().port();

    let mut server = DashboardServer::start(DashboardServerConfig {
        host: "127.0.0.1".to_string(),
        port: occupied_port,
        no_sync: true,
        ledger_path: Some(env.ledger_path()),
        ..Default::default()
    })
    .expect("dashboard server starts with fallback port");

    let info = server.info().clone();
    assert_eq!(info.host, "127.0.0.1");
    assert_ne!(info.port, occupied_port);
    assert!(info.port > 0);
    assert_eq!(info.url, format!("http://127.0.0.1:{}", info.port));
    drop(blocker);

    // 4. GET /?lang=zh returns zero-CDN embedded HTML + CSP & security headers
    let html_res = send_raw_http(info.port, "GET", "/?lang=zh", &[]);
    assert_eq!(html_res.status, 200);
    assert_eq!(
        html_res.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(html_res.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(html_res.header("cache-control"), Some("no-store"));
    assert_eq!(html_res.header("x-frame-options"), Some("DENY"));
    assert_eq!(html_res.header("referrer-policy"), Some("no-referrer"));
    let csp = html_res
        .header("content-security-policy")
        .expect("CSP header present");
    assert!(csp.contains("default-src 'self'"));
    assert!(csp.contains("connect-src 'self'"));
    assert!(html_res.body.starts_with("<!DOCTYPE html>"));
    assert!(html_res.body.contains("<html lang=\"zh-CN\""));
    assert!(html_res.body.contains("var INITIAL_LANG = \"zh\";"));
    assert!(html_res.body.contains(concat!("v", env!("CARGO_PKG_VERSION"))));
    assert!(
        !html_res.body.contains("http://") && !html_res.body.contains("https://"),
        "embedded HTML must contain zero external URLs"
    );
    for ui_id in [
        "range-pills",
        "source-pills",
        "filter-granularity",
        "filter-model",
        "filter-workspace",
        "filter-subagents",
        "btn-sync",
        "btn-export-csv",
        "btn-export-json",
        "btn-lang",
        "btn-theme",
        "session-tbody",
    ] {
        assert!(
            html_res.body.contains(&format!("id=\"{ui_id}\"")),
            "missing UI element #{ui_id}"
        );
    }

    // HEAD / returns 200 with empty body and Content-Length > 0
    let head_res = send_raw_http(info.port, "HEAD", "/index.html", &[]);
    assert_eq!(head_res.status, 200);
    assert!(head_res.body.is_empty());
    let head_len: usize = head_res
        .header("content-length")
        .unwrap()
        .parse()
        .unwrap();
    assert!(head_len > 50_000);

    // 5. Host header DNS rebinding protection -> 403
    let evil_host = format!("evil.com:{}", info.port);
    let bad_host_res = send_raw_http(
        info.port,
        "GET",
        "/api/overview",
        &[("Host", &evil_host)],
    );
    assert_eq!(bad_host_res.status, 403);

    // localhost:<port> Host is allowed
    let local_host = format!("localhost:{}", info.port);
    let ok_host_res = send_raw_http(
        info.port,
        "GET",
        "/api/health",
        &[("Host", &local_host)],
    );
    assert_eq!(ok_host_res.status, 200);

    // 6. Origin + CSRF token validation on POST /api/ping and POST /api/sync -> 403 for cross-site or missing CSRF
    let csrf_token = extract_html_csrf_token(info.port);
    for path in ["/api/ping", "/api/sync"] {
        let missing_csrf = send_raw_http(
            info.port,
            "POST",
            path,
            &[("Origin", &format!("http://localhost:{}", info.port))],
        );
        assert_eq!(missing_csrf.status, 403, "{path} must reject missing CSRF token");

        let bad_origin = send_raw_http(
            info.port,
            "POST",
            path,
            &[
                ("Origin", "https://evil.com"),
                ("X-Agy-Pulse-Token", &csrf_token),
            ],
        );
        assert_eq!(bad_origin.status, 403, "{path} must reject evil Origin");

        let wrong_port_origin = format!("http://127.0.0.1:{}", info.port.saturating_add(1));
        let bad_port = send_raw_http(
            info.port,
            "POST",
            path,
            &[
                ("Origin", &wrong_port_origin),
                ("X-Agy-Pulse-Token", &csrf_token),
            ],
        );
        assert_eq!(bad_port.status, 403, "{path} must reject wrong port Origin");

        let good_origin = format!("http://localhost:{}", info.port);
        let ok_origin = send_raw_http(
            info.port,
            "POST",
            path,
            &[
                ("Origin", &good_origin),
                ("X-Agy-Pulse-Token", &csrf_token),
            ],
        );
        assert_eq!(ok_origin.status, 200, "{path} must allow localhost Origin with valid CSRF token");
    }

    // 7. 405 Method Not Allowed & 404 Not Found
    assert_eq!(send_raw_http(info.port, "GET", "/api/sync", &[]).status, 405);
    assert_eq!(send_raw_http(info.port, "POST", "/api/overview", &[]).status, 405);
    assert_eq!(send_raw_http(info.port, "DELETE", "/api/health", &[]).status, 405);
    assert_eq!(send_raw_http(info.port, "GET", "/nope", &[]).status, 404);

    server.shutdown();
}

#[test]
fn t07_multidimensional_filtering_cascade_tree_and_cli_e2e() {
    let env = IsolatedEnv::new();
    let now = current_ms();
    let t_recent = now - 3_600_000; // 1h ago
    let t_sub_a = now - 2_400_000; // 40m ago
    let t_sub_b = now - 1_200_000; // 20m ago
    let t_ide_2d = now - 2 * 24 * 3_600_000; // 2d ago
    let t_old_20d = now - 20 * 24 * 3_600_000; // 20d ago

    // CLI Cascade root: cli-root-1
    write_conv_db(
        &env.cli_data_dir.join("conversations").join("cli-root-1.db"),
        Some(&build_trajectory_blob(
            "file:///work/alpha",
            "main",
            t_recent,
            Some("cli-root-1"),
        )),
        &[(1, build_step_metadata_blob(t_recent))],
        &[(
            1,
            build_gen_blob(
                &[1],
                &[GenStepSpec {
                    display_name: "Gemini 4 Carbon (High)",
                    input_tokens: 100_000,
                    cache_read_tokens: 400_000,
                    output_tokens: 10_000,
                    stream_sec: Some((50, 0)), // 200 tok/s
                    ttft_sec: Some((2, 0)),
                    parent_cascade_id: None,
                    root_cascade_id: None,
                }],
            ),
        )],
    );

    // Child 1 of cli-root-1
    write_conv_db(
        &env.cli_data_dir.join("conversations").join("cli-sub-a.db"),
        None,
        &[(1, build_step_metadata_blob(t_sub_a))],
        &[(
            1,
            build_gen_blob(
                &[1],
                &[GenStepSpec {
                    display_name: "Claude 4.5 Haiku",
                    input_tokens: 20_000,
                    cache_read_tokens: 80_000,
                    output_tokens: 5_000,
                    stream_sec: Some((25, 0)), // 200 tok/s
                    ttft_sec: Some((1, 0)),
                    parent_cascade_id: Some("cli-root-1"),
                    root_cascade_id: Some("cli-root-1"),
                }],
            ),
        )],
    );

    // Grandchild of cli-root-1 (parent = cli-sub-a)
    write_conv_db(
        &env.cli_data_dir.join("conversations").join("cli-sub-b.db"),
        None,
        &[(1, build_step_metadata_blob(t_sub_b))],
        &[(
            1,
            build_gen_blob(
                &[1],
                &[GenStepSpec {
                    display_name: "Gemini 4 Carbon (High)",
                    input_tokens: 10_000,
                    cache_read_tokens: 40_000,
                    output_tokens: 2_000,
                    stream_sec: Some((10, 0)), // 200 tok/s
                    ttft_sec: Some((0, 500_000_000)),
                    parent_cascade_id: Some("cli-sub-a"),
                    root_cascade_id: None,
                }],
            ),
        )],
    );

    // IDE Cascade 2 days ago
    write_conv_db(
        &env.ide_data_dir.join("conversations").join("ide-root-1.db"),
        Some(&build_trajectory_blob(
            "file:///work/beta",
            "feat/ui",
            t_ide_2d,
            Some("ide-root-1"),
        )),
        &[(1, build_step_metadata_blob(t_ide_2d))],
        &[(
            1,
            build_gen_blob(
                &[1],
                &[GenStepSpec {
                    display_name: "Claude 4.6 Sonnet",
                    input_tokens: 50_000,
                    cache_read_tokens: 150_000,
                    output_tokens: 4_000,
                    stream_sec: Some((20, 0)),
                    ttft_sec: Some((1, 500_000_000)),
                    parent_cascade_id: None,
                    root_cascade_id: None,
                }],
            ),
        )],
    );

    // Old CLI Cascade 20 days ago
    write_conv_db(
        &env.cli_data_dir.join("conversations").join("cli-old-1.db"),
        Some(&build_trajectory_blob(
            "file:///work/alpha",
            "legacy",
            t_old_20d,
            Some("cli-old-1"),
        )),
        &[(1, build_step_metadata_blob(t_old_20d))],
        &[(
            1,
            build_gen_blob(
                &[1],
                &[GenStepSpec {
                    display_name: "Gemini 4 Carbon (High)",
                    input_tokens: 30_000,
                    cache_read_tokens: 90_000,
                    output_tokens: 3_000,
                    stream_sec: Some((15, 0)),
                    ttft_sec: Some((1, 0)),
                    parent_cascade_id: None,
                    root_cascade_id: None,
                }],
            ),
        )],
    );

    // Independent expected USD billing totals matching statusline rates:
    // - Gemini 4 Carbon: $2.00/M in, $0.10/M cache_read, $10.00/M out
    // - Claude 4.5 Haiku: $0.80/M in, $0.08/M cache_read, $4.00/M out
    // - Claude 4.6 Sonnet: $3.00/M in, $0.30/M cache_read, $15.00/M out
    let cost_cli_main: f64 = 0.34;
    let cost_cli_sub_a: f64 = 0.0424;
    let cost_cli_sub_b: f64 = 0.044;
    let cost_ide: f64 = 0.255;
    let cost_old: f64 = 0.099;

    let source_roots = vec![
        agy_pulse::ledger::SourceRoot {
            root: env.cli_data_dir.clone(),
            source: agy_pulse::ledger::LedgerSource::Cli,
        },
        agy_pulse::ledger::SourceRoot {
            root: env.ide_data_dir.clone(),
            source: agy_pulse::ledger::LedgerSource::Ide,
        },
    ];

    let mut server = DashboardServer::start(DashboardServerConfig {
        host: "127.0.0.1".to_string(),
        port: 0,
        ledger_path: Some(env.ledger_path()),
        source_roots: Some(source_roots),
        ..Default::default()
    })
    .expect("start dashboard server");
    let port = server.info().port;

    // 7d overview: cli-root-1 (with 2 subagents) + ide-root-1
    let ov_7d: Value = serde_json::from_str(
        &send_raw_http(port, "GET", "/api/overview?range=7d", &[]).body,
    )
    .unwrap();
    let expected_cli_tree = cost_cli_main + cost_cli_sub_a + cost_cli_sub_b;
    let expected_7d_total = expected_cli_tree + cost_ide;
    assert!(
        (ov_7d["summary"]["totalCostUsd"].as_f64().unwrap() - expected_7d_total).abs() < 1e-9
    );
    assert!(
        (ov_7d["summary"]["mainCostUsd"].as_f64().unwrap() - (cost_cli_main + cost_ide)).abs()
            < 1e-9
    );
    assert!(
        (ov_7d["summary"]["subagentCostUsd"].as_f64().unwrap()
            - (cost_cli_sub_a + cost_cli_sub_b))
            .abs()
            < 1e-9
    );
    assert_eq!(ov_7d["summary"]["rootSessionCount"], 2);
    assert_eq!(ov_7d["summary"]["subagentSessionCount"], 2);
    assert_eq!(ov_7d["summary"]["totalSteps"], 4);

    let sessions = ov_7d["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    let cli_root = sessions
        .iter()
        .find(|s| s["rootConversationId"] == "cli-root-1")
        .expect("cli-root-1 present");
    assert_eq!(cli_root["source"], "cli");
    assert_eq!(cli_root["workspaceLabel"], "alpha");
    assert_eq!(cli_root["subagentCount"], 2);
    assert!((cli_root["totalCostUsd"].as_f64().unwrap() - expected_cli_tree).abs() < 1e-9);
    let children = cli_root["children"].as_array().unwrap();
    assert_eq!(children.len(), 2);
    assert_eq!(children[0]["conversationId"], "cli-sub-a");
    assert!((children[0]["costUsd"].as_f64().unwrap() - cost_cli_sub_a).abs() < 1e-9);
    assert_eq!(children[1]["conversationId"], "cli-sub-b");
    assert!((children[1]["costUsd"].as_f64().unwrap() - cost_cli_sub_b).abs() < 1e-9);

    // 30d includes cli-old-1
    let ov_30d: Value = serde_json::from_str(
        &send_raw_http(port, "GET", "/api/overview?range=30d", &[]).body,
    )
    .unwrap();
    assert!(
        (ov_30d["summary"]["totalCostUsd"].as_f64().unwrap() - (expected_7d_total + cost_old))
            .abs()
            < 1e-9
    );
    assert_eq!(ov_30d["summary"]["rootSessionCount"], 3);

    // 24h excludes ide-root-1 (2d ago) and cli-old-1 (20d ago)
    let ov_24h: Value = serde_json::from_str(
        &send_raw_http(port, "GET", "/api/overview?range=24h", &[]).body,
    )
    .unwrap();
    assert!((ov_24h["summary"]["totalCostUsd"].as_f64().unwrap() - expected_cli_tree).abs() < 1e-9);
    assert_eq!(ov_24h["summary"]["rootSessionCount"], 1);

    // Source filters: cli vs ide
    let ov_ide: Value = serde_json::from_str(
        &send_raw_http(port, "GET", "/api/overview?range=7d&source=ide", &[]).body,
    )
    .unwrap();
    assert!((ov_ide["summary"]["totalCostUsd"].as_f64().unwrap() - cost_ide).abs() < 1e-9);
    assert_eq!(ov_ide["summary"]["rootSessionCount"], 1);

    let ov_cli: Value = serde_json::from_str(
        &send_raw_http(port, "GET", "/api/overview?range=7d&source=cli", &[]).body,
    )
    .unwrap();
    assert!((ov_cli["summary"]["totalCostUsd"].as_f64().unwrap() - expected_cli_tree).abs() < 1e-9);

    // Model filter (percent-encoded space)
    let ov_haiku: Value = serde_json::from_str(
        &send_raw_http(
            port,
            "GET",
            "/api/overview?range=7d&model=Haiku%204.5",
            &[],
        )
        .body,
    )
    .unwrap();
    assert!((ov_haiku["summary"]["totalCostUsd"].as_f64().unwrap() - cost_cli_sub_a).abs() < 1e-9);

    // Workspace filter
    let ov_ws: Value = serde_json::from_str(
        &send_raw_http(port, "GET", "/api/overview?range=7d&workspace=beta", &[]).body,
    )
    .unwrap();
    assert!((ov_ws["summary"]["totalCostUsd"].as_f64().unwrap() - cost_ide).abs() < 1e-9);

    // includeSubagents=0
    let ov_no_sub: Value = serde_json::from_str(
        &send_raw_http(
            port,
            "GET",
            "/api/overview?range=7d&includeSubagents=0",
            &[],
        )
        .body,
    )
    .unwrap();
    assert!(
        (ov_no_sub["summary"]["totalCostUsd"].as_f64().unwrap() - (cost_cli_main + cost_ide)).abs()
            < 1e-9
    );
    assert_eq!(ov_no_sub["summary"]["subagentSessionCount"], 0);

    // Custom epoch range + hour granularity
    let ov_custom: Value = serde_json::from_str(
        &send_raw_http(
            port,
            "GET",
            &format!(
                "/api/overview?range=custom&from={}&to={}&granularity=hour",
                now - 5_000_000,
                now
            ),
            &[],
        )
        .body,
    )
    .unwrap();
    assert_eq!(ov_custom["meta"]["range"], "custom");
    assert_eq!(ov_custom["meta"]["granularity"], "hour");
    assert!(
        (ov_custom["summary"]["totalCostUsd"].as_f64().unwrap() - expected_cli_tree).abs() < 1e-9
    );

    server.shutdown();

    // Cross-process CLI verification: `agypl dashboard --no-open --port 0 --lang zh`
    let mut child = env
        .base_cmd(&agypl_bin())
        .args(["dashboard", "--no-open", "--port", "0", "--lang", "zh"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn agypl dashboard");

    let stdout = child.stdout.take().expect("stdout piped");
    let _guard = ChildGuard(child);
    let mut reader = BufReader::new(stdout);
    let mut bound_url = None;
    for _ in 0..10 {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap() == 0 {
            break;
        }
        if let Some(pos) = line.find("http://127.0.0.1:") {
            let rest = &line[pos..];
            let end = rest
                .find(|c: char| !c.is_ascii_alphanumeric() && c != ':' && c != '.' && c != '/')
                .unwrap_or(rest.len());
            bound_url = Some(rest[..end].to_string());
            break;
        }
    }
    let url = bound_url.expect("agypl dashboard printed listening URL");
    let cli_port: u16 = url
        .trim_start_matches("http://127.0.0.1:")
        .parse()
        .expect("parse CLI bound port");

    let cli_html = send_raw_http(cli_port, "GET", "/", &[]);
    assert_eq!(cli_html.status, 200);
    assert!(cli_html.body.contains("var INITIAL_LANG = \"zh\";"));

    let cli_ov: Value = serde_json::from_str(
        &send_raw_http(cli_port, "GET", "/api/overview?range=7d", &[]).body,
    )
    .unwrap();
    assert!(
        (cli_ov["summary"]["totalCostUsd"].as_f64().unwrap() - expected_7d_total).abs() < 1e-9
    );

    // Also verify `agy-pulse --dashboard --help` prints dashboard usage and exits 0
    let help_out = env
        .base_cmd(&agy_pulse_bin())
        .args(["--dashboard", "--help"])
        .output()
        .unwrap();
    assert!(help_out.status.success());
    let help_text = String::from_utf8_lossy(&help_out.stdout);
    assert!(help_text.contains("--port") && help_text.contains("--no-open"));
}

#[test]
fn t07_csv_json_exports_static_routes_and_sync_api() {
    let env = IsolatedEnv::new();
    let now = current_ms();
    let t1 = now - 600_000;

    write_conv_db(
        &env.cli_data_dir.join("conversations").join("exp-cli.db"),
        Some(&build_trajectory_blob(
            "file:///work/export-proj",
            "main",
            t1,
            Some("exp-cli"),
        )),
        &[(1, build_step_metadata_blob(t1))],
        &[(
            1,
            build_gen_blob(
                &[1],
                &[GenStepSpec {
                    display_name: "Gemini 4 Carbon (High)",
                    input_tokens: 50_000,
                    cache_read_tokens: 200_000,
                    output_tokens: 4_000,
                    stream_sec: Some((20, 0)),
                    ttft_sec: Some((1, 0)),
                    parent_cascade_id: None,
                    root_cascade_id: None,
                }],
            ),
        )],
    );

    let source_roots = vec![
        agy_pulse::ledger::SourceRoot {
            root: env.cli_data_dir.clone(),
            source: agy_pulse::ledger::LedgerSource::Cli,
        },
        agy_pulse::ledger::SourceRoot {
            root: env.ide_data_dir.clone(),
            source: agy_pulse::ledger::LedgerSource::Ide,
        },
    ];

    let mut server = DashboardServer::start(DashboardServerConfig {
        host: "127.0.0.1".to_string(),
        port: 0,
        no_sync: true,
        idle_timeout_ms: 30_000,
        ledger_path: Some(env.ledger_path()),
        source_roots: Some(source_roots),
        ..Default::default()
    })
    .unwrap();
    let port = server.info().port;

    // Static routes: /icon.svg & /manifest.webmanifest
    let icon_res = send_raw_http(port, "GET", "/icon.svg", &[]);
    assert_eq!(icon_res.status, 200);
    assert_eq!(
        icon_res.header("content-type"),
        Some("image/svg+xml; charset=utf-8")
    );
    assert_eq!(icon_res.body, DASHBOARD_ICON_SVG);

    let manifest_res = send_raw_http(port, "GET", "/manifest.webmanifest", &[]);
    assert_eq!(manifest_res.status, 200);
    assert_eq!(
        manifest_res.header("content-type"),
        Some("application/manifest+json; charset=utf-8")
    );
    let manifest_json: Value = serde_json::from_str(&manifest_res.body).unwrap();
    assert_eq!(manifest_json["short_name"], "agy-pulse");
    assert_eq!(manifest_json["icons"][0]["src"], "/icon.svg");

    // /api/health
    let health_res = send_raw_http(port, "GET", "/api/health", &[]);
    assert_eq!(health_res.status, 200);
    let health_json: Value = serde_json::from_str(&health_res.body).unwrap();
    assert_eq!(health_json["ok"], true);
    assert_eq!(health_json["app"], DASHBOARD_APP_ID);
    assert_eq!(health_json["port"], port);
    assert_eq!(health_json["idleTimeoutMs"], 30_000);

    // POST /api/sync ingests exp-cli.db
    let csrf_token = server.csrf_token().to_string();
    let origin = format!("http://127.0.0.1:{port}");
    let sync_res = send_raw_http(
        port,
        "POST",
        "/api/sync",
        &[("Origin", &origin), ("X-Agy-Pulse-Token", &csrf_token)],
    );
    assert_eq!(sync_res.status, 200);
    let sync_json: Value = serde_json::from_str(&sync_res.body).unwrap();
    assert_eq!(sync_json["ok"], true);
    assert_eq!(sync_json["totalDbs"], 1);
    assert_eq!(sync_json["cliDbs"], 1);
    assert_eq!(sync_json["newlySyncedDbs"], 1);
    assert_eq!(sync_json["stepsUpserted"], 1);

    // /api/export.csv for timeseries, models, sessions
    let csv_ts = send_raw_http(port, "GET", "/api/export.csv?kind=timeseries&range=7d", &[]);
    assert_eq!(csv_ts.status, 200);
    assert_eq!(csv_ts.header("content-type"), Some("text/csv; charset=utf-8"));
    assert_eq!(
        csv_ts.header("content-disposition"),
        Some("attachment; filename=\"agy-pulse-telemetry-7d.csv\"")
    );
    assert!(csv_ts.body.starts_with("bucketKey,bucketStartMs,label,costUsd,"));
    assert!(csv_ts.body.lines().count() >= 2);

    let csv_models = send_raw_http(port, "GET", "/api/export.csv?kind=models&range=7d", &[]);
    assert_eq!(csv_models.status, 200);
    assert!(csv_models.body.starts_with("model,costUsd,costSharePct,"));
    assert!(csv_models.body.contains("Carbon"));

    let csv_sessions = send_raw_http(
        port,
        "GET",
        "/api/export.csv?kind=sessions&range=all",
        &[],
    );
    assert_eq!(csv_sessions.status, 200);
    assert_eq!(
        csv_sessions.header("content-disposition"),
        Some("attachment; filename=\"agy-pulse-telemetry-all.csv\"")
    );
    assert!(csv_sessions.body.starts_with("rootConversationId,source,workspace,"));
    assert!(csv_sessions.body.contains("exp-cli"));

    // /api/export.json
    let json_exp = send_raw_http(port, "GET", "/api/export.json?range=30d&source=cli", &[]);
    assert_eq!(json_exp.status, 200);
    assert_eq!(
        json_exp.header("content-type"),
        Some("application/json; charset=utf-8")
    );
    assert_eq!(
        json_exp.header("content-disposition"),
        Some("attachment; filename=\"agy-pulse-telemetry-30d.json\"")
    );
    let exported: Value = serde_json::from_str(&json_exp.body).unwrap();
    assert_eq!(exported["meta"]["backend"], "sqlite");
    assert_eq!(exported["meta"]["source"], "cli");
    assert_eq!(exported["sessions"][0]["rootConversationId"], "exp-cli");

    server.shutdown();
}

fn send_raw_http_with_body(
    port: u16,
    method: &str,
    target: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> HttpResponse {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect loopback");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();

    let mut req = format!("{method} {target} HTTP/1.1\r\n");
    let mut has_host = false;
    for (k, v) in headers {
        if k.eq_ignore_ascii_case("host") {
            has_host = true;
        }
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if !has_host {
        req.push_str(&format!("Host: 127.0.0.1:{port}\r\n"));
    }
    if !body.is_empty() {
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    req.push_str("Connection: close\r\n\r\n");
    req.push_str(body);
    stream.write_all(req.as_bytes()).unwrap();
    stream.flush().unwrap();

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, resp_body) = text
        .split_once("\r\n\r\n")
        .expect("HTTP response header/body separator");
    let mut lines = head.lines();
    let status_line = lines.next().expect("status line");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .expect("status code")
        .parse()
        .expect("parse status code");
    let parsed_headers = lines
        .filter_map(|line| {
            let (k, v) = line.split_once(':')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect();

    HttpResponse {
        status,
        headers: parsed_headers,
        body: resp_body.to_string(),
    }
}

fn spawn_isolated_dashboard(
    env: &IsolatedEnv,
    sync_cfg_path: &Path,
    extra_args: &[&str],
) -> (ChildGuard, u16) {
    let mut cmd = env.base_cmd(&agypl_bin());
    cmd.arg("dashboard")
        .args(["--no-open", "--port", "0"])
        .args(extra_args)
        .env("AGY_PULSE_SYNC_CONFIG", sync_cfg_path)
        .env("AGY_PULSE_NO_DASHBOARD_AUTOSTART", "1")
        .env("AGY_PULSE_DISABLE_UPDATE_CHECK", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn isolated dashboard");
    let stdout = child.stdout.take().expect("stdout piped");
    let guard = ChildGuard(child);
    let mut reader = BufReader::new(stdout);
    let mut bound_url = None;
    for _ in 0..15 {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap() == 0 {
            break;
        }
        if let Some(pos) = line.find("http://127.0.0.1:") {
            let rest = &line[pos..];
            let end = rest
                .find(|c: char| !c.is_ascii_alphanumeric() && c != ':' && c != '.' && c != '/')
                .unwrap_or(rest.len());
            bound_url = Some(rest[..end].to_string());
            break;
        }
    }
    let url = bound_url.expect("isolated dashboard printed listening URL");
    let port: u16 = url
        .trim_start_matches("http://127.0.0.1:")
        .parse()
        .expect("parse bound port");
    (guard, port)
}

#[test]
fn t07_multi_account_device_overview_quotas_and_exports() {
    use agy_pulse::domain::{
        ProviderQuota, ProviderQuotaSummary, QuotaSnapshot, QuotaWindow, SyncConvRecord,
        SyncStepRecord,
    };
    use agy_pulse::ledger::{LedgerSource, LedgerStore, LedgerSyncOptions, SourceRoot};

    let env = IsolatedEnv::new();
    let now = current_ms();
    let t_local_unassigned = now - 3_600_000;
    let t_local_alice = now - 1_800_000;
    let t_remote_alice = now - 900_000;
    let t_remote_bob = now - 600_000;

    // 1. Unassigned IDE conversation on local device
    write_conv_db(
        &env.ide_data_dir.join("conversations").join("local-ide-unassigned.db"),
        Some(&build_trajectory_blob(
            "file:///work/unassigned-proj",
            "main",
            t_local_unassigned,
            Some("local-ide-unassigned"),
        )),
        &[(1, build_step_metadata_blob(t_local_unassigned))],
        &[(
            1,
            build_gen_blob(
                &[1],
                &[GenStepSpec {
                    display_name: "Claude 4.6 Sonnet",
                    input_tokens: 50_000,
                    cache_read_tokens: 150_000,
                    output_tokens: 4_000,
                    stream_sec: Some((20, 0)),
                    ttft_sec: Some((1, 0)),
                    parent_cascade_id: None,
                    root_cascade_id: None,
                }],
            ),
        )],
    );

    let ide_only_root = vec![SourceRoot {
        root: env.ide_data_dir.clone(),
        source: LedgerSource::Ide,
    }];
    let mut store = LedgerStore::open_at(&env.ledger_path()).unwrap();
    store
        .sync_all(&LedgerSyncOptions {
            source_roots: Some(ide_only_root),
            device_name: Some("work-mac".to_string()),
            ..Default::default()
        })
        .unwrap();

    // 2. Alice CLI conversation on local device
    write_conv_db(
        &env.cli_data_dir.join("conversations").join("local-cli-alice.db"),
        Some(&build_trajectory_blob(
            "file:///work/alice-proj",
            "feat/sync",
            t_local_alice,
            Some("local-cli-alice"),
        )),
        &[(1, build_step_metadata_blob(t_local_alice))],
        &[(
            1,
            build_gen_blob(
                &[1],
                &[GenStepSpec {
                    display_name: "Gemini 4 Carbon (High)",
                    input_tokens: 100_000,
                    cache_read_tokens: 400_000,
                    output_tokens: 10_000,
                    stream_sec: Some((50, 0)),
                    ttft_sec: Some((2, 0)),
                    parent_cascade_id: None,
                    root_cascade_id: None,
                }],
            ),
        )],
    );
    store.set_last_observed_email("Alice@Gmail.com").unwrap();
    let cli_only_root = vec![SourceRoot {
        root: env.cli_data_dir.clone(),
        source: LedgerSource::Cli,
    }];
    store
        .sync_all(&LedgerSyncOptions {
            source_roots: Some(cli_only_root),
            default_account_id: Some("alice@gmail.com".to_string()),
            device_name: Some("work-mac".to_string()),
            ..Default::default()
        })
        .unwrap();

    let local_device_id = store.local_device_id().unwrap();

    // 3. Remote device records for Alice (lab-linux) and Bob (bob-win)
    store
        .upsert_sync_device(
            "alice@gmail.com",
            "dev_remote_linux",
            "lab-linux",
            false,
            t_remote_alice,
            Some("Google AI Ultra"),
        )
        .unwrap();
    store
        .import_remote_chunk(
            "alice@gmail.com",
            "dev_remote_linux",
            "2026-11",
            "sha_alice_remote",
            &[SyncConvRecord {
                conversation_id: "remote-alice-1".to_string(),
                created_at_ms: Some(t_remote_alice),
                cascade_id: "remote-alice-1".to_string(),
                parent_conversation_id: None,
                root_conversation_id: None,
                source: "cli".to_string(),
            }],
            &[SyncStepRecord {
                step_uid: "remote-alice-1:1:0".to_string(),
                conversation_id: "remote-alice-1".to_string(),
                gen_idx: 1,
                entry_idx: 0,
                step_index: Some(1),
                ts_ms: t_remote_alice,
                model: "Gemini 4 Carbon".to_string(),
                raw_model: "Gemini 4 Carbon (High)".to_string(),
                input_tokens: 50_000,
                cache_read_tokens: 200_000,
                cache_write_tokens: 0,
                output_tokens: 4_000,
                cost_usd: 0.16,
                stream_sec: Some(20.0),
                ttft_sec: Some(1.0),
                valid_stream: 1,
                valid_ttft: 1,
                parent_conversation_id: None,
                root_conversation_id: None,
                source: "cli".to_string(),
            }],
            now,
        )
        .unwrap();

    store
        .upsert_sync_device(
            "bob@example.com",
            "dev_bob_win",
            "bob-win",
            false,
            t_remote_bob,
            Some("Pro"),
        )
        .unwrap();
    store
        .import_remote_chunk(
            "bob@example.com",
            "dev_bob_win",
            "2026-11",
            "sha_bob_remote",
            &[SyncConvRecord {
                conversation_id: "remote-bob-1".to_string(),
                created_at_ms: Some(t_remote_bob),
                cascade_id: "remote-bob-1".to_string(),
                parent_conversation_id: None,
                root_conversation_id: None,
                source: "ide".to_string(),
            }],
            &[SyncStepRecord {
                step_uid: "remote-bob-1:1:0".to_string(),
                conversation_id: "remote-bob-1".to_string(),
                gen_idx: 1,
                entry_idx: 0,
                step_index: Some(1),
                ts_ms: t_remote_bob,
                model: "Claude 4.6 Sonnet".to_string(),
                raw_model: "Claude 4.6 Sonnet".to_string(),
                input_tokens: 20_000,
                cache_read_tokens: 100_000,
                cache_write_tokens: 0,
                output_tokens: 2_000,
                cost_usd: 0.12,
                stream_sec: Some(10.0),
                ttft_sec: Some(0.8),
                valid_stream: 1,
                valid_ttft: 1,
                parent_conversation_id: None,
                root_conversation_id: None,
                source: "ide".to_string(),
            }],
            now,
        )
        .unwrap();

    // 4. Quota snapshots: Alice has older local snapshot & newer remote snapshot
    store
        .upsert_quota_snapshot(&QuotaSnapshot {
            account_id: "alice@gmail.com".to_string(),
            device_id: local_device_id.clone(),
            device_name: "work-mac".to_string(),
            plan_tier: Some("Google AI Ultra".to_string()),
            observed_at_ms: now - 600_000,
            quota: ProviderQuotaSummary {
                google: ProviderQuota {
                    five_hour: Some(QuotaWindow {
                        remaining_fraction: 0.40,
                        reset_time_ms: Some(now + 3_600_000),
                    }),
                    weekly: None,
                },
                ..Default::default()
            },
        })
        .unwrap();
    store
        .upsert_quota_snapshot(&QuotaSnapshot {
            account_id: "alice@gmail.com".to_string(),
            device_id: "dev_remote_linux".to_string(),
            device_name: "lab-linux".to_string(),
            plan_tier: Some("Google AI Ultra".to_string()),
            observed_at_ms: now - 60_000,
            quota: ProviderQuotaSummary {
                google: ProviderQuota {
                    five_hour: Some(QuotaWindow {
                        remaining_fraction: 0.85,
                        reset_time_ms: Some(now + 7_200_000),
                    }),
                    weekly: Some(QuotaWindow {
                        remaining_fraction: 1.0,
                        reset_time_ms: None,
                    }),
                },
                ..Default::default()
            },
        })
        .unwrap();
    store
        .upsert_quota_snapshot(&QuotaSnapshot {
            account_id: "bob@example.com".to_string(),
            device_id: "dev_bob_win".to_string(),
            device_name: "bob-win".to_string(),
            plan_tier: Some("Pro".to_string()),
            observed_at_ms: now - 120_000,
            quota: ProviderQuotaSummary {
                claude: ProviderQuota {
                    five_hour: Some(QuotaWindow {
                        remaining_fraction: 0.70,
                        reset_time_ms: Some(now - 5_000),
                    }),
                    weekly: None,
                },
                ..Default::default()
            },
        })
        .unwrap();
    drop(store);

    let missing_sync_cfg = env.home_dir.join("missing-sync.json");
    let (_guard, port) = spawn_isolated_dashboard(&env, &missing_sync_cfg, &["--no-sync"]);

    // Overview (all)
    let ov_all: Value =
        serde_json::from_str(&send_raw_http(port, "GET", "/api/overview?range=7d", &[]).body)
            .unwrap();
    assert_eq!(ov_all["meta"]["account"], "all");
    assert_eq!(ov_all["meta"]["device"], "all");
    assert_eq!(ov_all["meta"]["localDeviceId"], local_device_id);
    assert_eq!(ov_all["summary"]["rootSessionCount"], 4);

    let avail_accts = ov_all["meta"]["availableAccounts"].as_array().unwrap();
    assert_eq!(avail_accts.len(), 3);
    let alice_opt = avail_accts
        .iter()
        .find(|a| a["accountId"] == "alice@gmail.com")
        .unwrap();
    assert_eq!(alice_opt["deviceCount"], 2);

    let avail_devs = ov_all["meta"]["availableDevices"].as_array().unwrap();
    assert_eq!(avail_devs.len(), 3);
    assert_eq!(avail_devs[0]["deviceId"], local_device_id);
    assert_eq!(avail_devs[0]["isLocal"], true);

    let by_account = ov_all["byAccount"].as_array().unwrap();
    assert_eq!(by_account.len(), 3);
    let alice_bd = by_account
        .iter()
        .find(|a| a["accountId"] == "alice@gmail.com")
        .unwrap();
    assert!((alice_bd["costUsd"].as_f64().unwrap() - (0.34 + 0.16)).abs() < 1e-9);
    assert_eq!(alice_bd["deviceCount"], 2);
    assert_eq!(alice_bd["sessionCount"], 2);

    let by_device = ov_all["byDevice"].as_array().unwrap();
    assert_eq!(by_device.len(), 3);
    let local_bd = by_device
        .iter()
        .find(|d| d["deviceId"] == local_device_id)
        .unwrap();
    assert_eq!(local_bd["isLocal"], true);
    assert!((local_bd["cliCostUsd"].as_f64().unwrap() - 0.34).abs() < 1e-9);
    assert!((local_bd["ideCostUsd"].as_f64().unwrap() - 0.255).abs() < 1e-9);

    // Account quotas strictly pick latest snapshot per account across devices
    let quotas = ov_all["accountQuotas"].as_array().unwrap();
    assert_eq!(quotas.len(), 2);
    let alice_q = quotas
        .iter()
        .find(|q| q["accountId"] == "alice@gmail.com")
        .unwrap();
    assert_eq!(alice_q["reporterDeviceId"], "dev_remote_linux");
    assert_eq!(alice_q["reporterDeviceName"], "lab-linux");
    assert_eq!(alice_q["planTier"], "Google AI Ultra");
    assert!(
        (alice_q["quota"]["google"]["five_hour"]["remaining_fraction"]
            .as_f64()
            .unwrap()
            - 0.85)
            .abs()
            < 1e-9
    );

    // Session attribution fields
    let sessions = ov_all["sessions"].as_array().unwrap();
    let remote_alice_sess = sessions
        .iter()
        .find(|s| s["rootConversationId"] == "remote-alice-1")
        .unwrap();
    assert_eq!(remote_alice_sess["accountId"], "alice@gmail.com");
    assert_eq!(remote_alice_sess["deviceId"], "dev_remote_linux");
    assert_eq!(remote_alice_sess["deviceName"], "lab-linux");

    // Filter by account=alice@gmail.com (availableAccounts still preserves all 3)
    let ov_alice: Value = serde_json::from_str(
        &send_raw_http(
            port,
            "GET",
            "/api/overview?range=7d&account=alice@gmail.com",
            &[],
        )
        .body,
    )
    .unwrap();
    assert_eq!(ov_alice["meta"]["account"], "alice@gmail.com");
    assert_eq!(ov_alice["meta"]["availableAccounts"].as_array().unwrap().len(), 3);
    assert!((ov_alice["summary"]["totalCostUsd"].as_f64().unwrap() - 0.50).abs() < 1e-9);
    assert_eq!(ov_alice["summary"]["rootSessionCount"], 2);
    assert_eq!(ov_alice["accountQuotas"].as_array().unwrap().len(), 1);

    // Filter by account=unassigned
    let ov_unassigned: Value = serde_json::from_str(
        &send_raw_http(
            port,
            "GET",
            "/api/overview?range=7d&account=unassigned",
            &[],
        )
        .body,
    )
    .unwrap();
    assert_eq!(ov_unassigned["summary"]["rootSessionCount"], 1);
    assert!((ov_unassigned["summary"]["totalCostUsd"].as_f64().unwrap() - 0.255).abs() < 1e-9);

    // Filter by device=local
    let ov_local: Value = serde_json::from_str(
        &send_raw_http(port, "GET", "/api/overview?range=7d&device=local", &[]).body,
    )
    .unwrap();
    assert_eq!(ov_local["meta"]["device"], local_device_id);
    assert_eq!(ov_local["summary"]["rootSessionCount"], 2);
    assert!((ov_local["summary"]["totalCostUsd"].as_f64().unwrap() - (0.34 + 0.255)).abs() < 1e-9);

    // Export JSON with account + device filter
    let json_filtered: Value = serde_json::from_str(
        &send_raw_http(
            port,
            "GET",
            "/api/export.json?range=7d&account=alice@gmail.com&device=dev_remote_linux",
            &[],
        )
        .body,
    )
    .unwrap();
    assert_eq!(json_filtered["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(
        json_filtered["sessions"][0]["rootConversationId"],
        "remote-alice-1"
    );

    // Export CSV with account filter and trailing account_id,device_name columns
    let csv_alice = send_raw_http(
        port,
        "GET",
        "/api/export.csv?kind=sessions&range=7d&account=alice@gmail.com",
        &[],
    )
    .body;
    let first_line = csv_alice.lines().next().unwrap();
    assert!(first_line.starts_with("rootConversationId,source,workspace,"));
    assert!(first_line.ends_with(",models,account_id,device_name"));
    assert!(csv_alice.contains("remote-alice-1"));
    assert!(csv_alice.contains("local-cli-alice"));
    assert!(!csv_alice.contains("remote-bob-1"));
    assert!(!csv_alice.contains("local-ide-unassigned"));
}

#[test]
fn t07_sync_remote_pull_throttle_offline_and_dashboard_html_hooks() {
    use agy_pulse::domain::{ProviderQuota, ProviderQuotaSummary, QuotaSnapshot, QuotaWindow};
    use agy_pulse::ledger::{LedgerSource, LedgerStore, LedgerSyncOptions, SourceRoot};
    use agy_pulse::sync::{LocalDirRemote, RemoteConfig, SyncConfig, push, save_sync_config_to};

    let env = IsolatedEnv::new();
    let remote_dir = env._temp.path().join("remote_store");
    fs::create_dir_all(&remote_dir).unwrap();

    // Device A prepares and pushes a chunk + quota snapshot to local-dir remote
    let device_a_cli_dir = env._temp.path().join("device_a_cli");
    fs::create_dir_all(&device_a_cli_dir).unwrap();
    let now = current_ms();
    let t1 = now - 600_000;
    write_conv_db(
        &device_a_cli_dir.join("conversations").join("dev-a-conv.db"),
        Some(&build_trajectory_blob(
            "file:///work/proj-a",
            "main",
            t1,
            Some("dev-a-conv"),
        )),
        &[(1, build_step_metadata_blob(t1))],
        &[(
            1,
            build_gen_blob(
                &[1],
                &[GenStepSpec {
                    display_name: "Gemini 4 Carbon (High)",
                    input_tokens: 40_000,
                    cache_read_tokens: 160_000,
                    output_tokens: 4_000,
                    stream_sec: Some((20, 0)),
                    ttft_sec: Some((1, 0)),
                    parent_cascade_id: None,
                    root_cascade_id: None,
                }],
            ),
        )],
    );

    let ledger_a_path = env._temp.path().join("device_a_ledger.sqlite");
    let mut store_a = LedgerStore::open_at(&ledger_a_path).unwrap();
    store_a
        .sync_all(&LedgerSyncOptions {
            source_roots: Some(vec![SourceRoot {
                root: device_a_cli_dir,
                source: LedgerSource::Cli,
            }]),
            default_account_id: Some("alice@gmail.com".to_string()),
            device_name: Some("device-alpha".to_string()),
            ..Default::default()
        })
        .unwrap();
    let dev_a_id = store_a.local_device_id().unwrap();
    store_a
        .upsert_quota_snapshot(&QuotaSnapshot {
            account_id: "alice@gmail.com".to_string(),
            device_id: dev_a_id,
            device_name: "device-alpha".to_string(),
            plan_tier: Some("Google AI Ultra".to_string()),
            observed_at_ms: now - 30_000,
            quota: ProviderQuotaSummary {
                google: ProviderQuota {
                    five_hour: Some(QuotaWindow {
                        remaining_fraction: 0.91,
                        reset_time_ms: Some(now + 10_000_000),
                    }),
                    weekly: None,
                },
                ..Default::default()
            },
        })
        .unwrap();

    let cfg_a = SyncConfig {
        enabled: true,
        device_id: None,
        device_name: Some("device-alpha".to_string()),
        default_account: Some("alice@gmail.com".to_string()),
        push_interval_sec: 300,
        prefix: "agy-pulse-sync".to_string(),
        remote: RemoteConfig::LocalDir {
            path: remote_dir.to_string_lossy().into_owned(),
        },
    };
    let remote_a = LocalDirRemote::new(remote_dir.clone());
    let push_out = push(&store_a, &remote_a, &cfg_a, false).unwrap();
    assert_eq!(push_out.chunks_uploaded, 1);

    // Device B (the isolated dashboard process) points AGY_PULSE_SYNC_CONFIG to local-dir remote
    let sync_cfg_path = env._temp.path().join("sync-cfg.json");
    let cfg_b = SyncConfig {
        enabled: true,
        device_id: None,
        device_name: Some("device-beta".to_string()),
        default_account: Some("alice@gmail.com".to_string()),
        push_interval_sec: 300,
        prefix: "agy-pulse-sync".to_string(),
        remote: RemoteConfig::LocalDir {
            path: remote_dir.to_string_lossy().into_owned(),
        },
    };
    save_sync_config_to(&sync_cfg_path, &cfg_b).unwrap();

    let (_guard, port) = spawn_isolated_dashboard(&env, &sync_cfg_path, &["--no-sync"]);

    // 1. HTML contains multi-account/device UI hooks and sync endpoint
    let html = send_raw_http(port, "GET", "/?lang=zh", &[]).body;
    let csrf_token = extract_html_csrf_token(port);
    let origin = format!("http://127.0.0.1:{port}");
    for hook in [
        "/api/sync-remote",
        "id=\"account-pills\"",
        "id=\"device-pills\"",
        "id=\"accounts-quotas-panel\"",
        "id=\"accounts-quotas-grid\"",
        "id=\"sync-status-badge\"",
    ] {
        assert!(html.contains(hook), "dashboard.html missing hook: {hook}");
    }
    for forbidden in ["HUD", "账户余额", "管理后台"] {
        assert!(!html.contains(forbidden), "forbidden term in HTML: {forbidden}");
    }

    // 2. GET /api/overview returns immediately before remote sync
    let before_ov: Value =
        serde_json::from_str(&send_raw_http(port, "GET", "/api/overview?range=all", &[]).body)
            .unwrap();
    assert_eq!(before_ov["syncStatus"]["enabled"], true);
    assert_eq!(before_ov["syncStatus"]["remoteType"], "local-dir");
    assert_eq!(before_ov["sessions"].as_array().unwrap().len(), 0);

    // 3. POST /api/sync-remote pulls Device A's chunk and sets changed=true
    let sync1 = send_raw_http(
        port,
        "POST",
        "/api/sync-remote",
        &[("Origin", &origin), ("X-Agy-Pulse-Token", &csrf_token)],
    );
    assert_eq!(sync1.status, 200);
    let sync1_json: Value = serde_json::from_str(&sync1.body).unwrap();
    assert_eq!(sync1_json["ok"], true);
    assert_eq!(sync1_json["enabled"], true);
    assert_eq!(sync1_json["changed"], true);
    assert_eq!(sync1_json["throttled"], false);
    assert!(sync1_json["pulledChunks"].as_u64().unwrap() >= 1);
    assert!(sync1_json["stepsImported"].as_u64().unwrap() >= 1);

    let after_ov: Value =
        serde_json::from_str(&send_raw_http(port, "GET", "/api/overview?range=all", &[]).body)
            .unwrap();
    assert_eq!(after_ov["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(after_ov["sessions"][0]["deviceName"], "device-alpha");
    assert_eq!(after_ov["accountQuotas"].as_array().unwrap().len(), 1);
    assert_eq!(
        after_ov["accountQuotas"][0]["reporterDeviceName"],
        "device-alpha"
    );

    // 4. Repeat POST /api/sync-remote within 30s without force hits throttle
    let sync2_json: Value = serde_json::from_str(
        &send_raw_http(
            port,
            "POST",
            "/api/sync-remote",
            &[("Origin", &origin), ("X-Agy-Pulse-Token", &csrf_token)],
        )
        .body,
    )
    .unwrap();
    assert_eq!(sync2_json["ok"], true);
    assert_eq!(sync2_json["throttled"], true);
    assert_eq!(sync2_json["changed"], false);

    // 5. JSON body {"force":true} bypasses throttle
    let sync3_json: Value = serde_json::from_str(
        &send_raw_http_with_body(
            port,
            "POST",
            "/api/sync-remote",
            &[
                ("Content-Type", "application/json"),
                ("Origin", &origin),
                ("X-Agy-Pulse-Token", &csrf_token),
            ],
            r#"{"force":true}"#,
        )
        .body,
    )
    .unwrap();
    assert_eq!(sync3_json["ok"], true);
    assert_eq!(sync3_json["throttled"], false);

    // 6. Same remote going offline preserves previously pulled data in /api/overview and records lastError
    fs::remove_dir_all(&remote_dir).unwrap();
    fs::write(&remote_dir, b"simulated-offline-mount").unwrap();

    let sync_offline_res = send_raw_http(
        port,
        "POST",
        "/api/sync-remote?force=true",
        &[("Origin", &origin), ("X-Agy-Pulse-Token", &csrf_token)],
    );
    assert_eq!(sync_offline_res.status, 200);
    let sync_offline_json: Value = serde_json::from_str(&sync_offline_res.body).unwrap();
    assert_eq!(sync_offline_json["ok"], false);
    assert_eq!(sync_offline_json["enabled"], true);
    assert_eq!(sync_offline_json["changed"], false);
    assert!(!sync_offline_json["error"].as_str().unwrap().is_empty());

    // /api/overview still succeeds immediately with 200 and preserves previously pulled data
    let offline_ov_res = send_raw_http(port, "GET", "/api/overview?range=all", &[]);
    assert_eq!(offline_ov_res.status, 200);
    let offline_ov: Value = serde_json::from_str(&offline_ov_res.body).unwrap();
    assert_eq!(offline_ov["sessions"].as_array().unwrap().len(), 1);
    assert!(offline_ov["syncStatus"]["lastError"].is_string());

    // 7. Unreachable CF Worker remote returns ok=false with redacted error (never leaks token)
    let unused_listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let dead_port = unused_listener.local_addr().unwrap().port();
    drop(unused_listener);

    let bad_cfg = SyncConfig {
        enabled: true,
        device_id: None,
        device_name: Some("device-beta".to_string()),
        default_account: Some("alice@gmail.com".to_string()),
        push_interval_sec: 300,
        prefix: "agy-pulse-sync".to_string(),
        remote: RemoteConfig::CfWorker {
            url: format!("http://127.0.0.1:{dead_port}"),
            token: Some("super-secret-token-xyz".to_string()),
            allow_insecure_http: true,
        },
    };
    save_sync_config_to(&sync_cfg_path, &bad_cfg).unwrap();

    let sync_err_res = send_raw_http(
        port,
        "POST",
        "/api/sync-remote?force=true",
        &[("Origin", &origin), ("X-Agy-Pulse-Token", &csrf_token)],
    );
    assert_eq!(sync_err_res.status, 200);
    let sync_err_json: Value = serde_json::from_str(&sync_err_res.body).unwrap();
    assert_eq!(sync_err_json["ok"], false);
    assert_eq!(sync_err_json["enabled"], true);
    assert_eq!(sync_err_json["changed"], false);
    let err_msg = sync_err_json["error"].as_str().unwrap();
    assert!(!err_msg.is_empty());
    assert!(!err_msg.contains("super-secret-token-xyz"));

    let bad_ov_res = send_raw_http(port, "GET", "/api/overview?range=all", &[]);
    assert_eq!(bad_ov_res.status, 200);
    let bad_ov: Value = serde_json::from_str(&bad_ov_res.body).unwrap();
    let last_err = bad_ov["syncStatus"]["lastError"].as_str().unwrap();
    assert!(!last_err.contains("super-secret-token-xyz"));
}

#[test]
fn t07_csrf_token_413_large_body_and_sync_identity_hints_linkage() {
    use agy_pulse::sync::{RemoteConfig, SyncConfig, save_sync_config_to};

    let env = IsolatedEnv::new();
    let remote_dir = env._temp.path().join("remote-hints");
    fs::create_dir_all(&remote_dir).unwrap();

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let step_ts = now - 60_000;

    let conv1_db = env.cli_data_dir.join("conversations").join("hint-conv-1.db");
    let step1 = pb_timestamp_ms(1, step_ts);
    let gen1 = build_gen_blob(
        &[0],
        &[GenStepSpec {
            display_name: "Gemini 3 Flash",
            input_tokens: 1200,
            output_tokens: 300,
            cache_read_tokens: 0,
            ttft_sec: None,
            stream_sec: None,
            parent_cascade_id: None,
            root_cascade_id: None,
        }],
    );
    write_conv_db(
        &conv1_db,
        Some(&build_trajectory_blob("ws-hint", "main", step_ts, None)),
        &[(0, step1)],
        &[(0, gen1)],
    );

    let sync_cfg_path = env._temp.path().join("sync-hints.json");
    let cfg_v1 = SyncConfig {
        enabled: false,
        device_id: None,
        device_name: Some("studio-alpha".to_string()),
        default_account: Some("alpha@example.com".to_string()),
        push_interval_sec: 300,
        prefix: "agy-pulse-sync".to_string(),
        remote: RemoteConfig::LocalDir {
            path: remote_dir.to_string_lossy().into_owned(),
        },
    };
    save_sync_config_to(&sync_cfg_path, &cfg_v1).unwrap();

    let (_guard, port) = spawn_isolated_dashboard(&env, &sync_cfg_path, &[]);

    // 1. Served HTML injects 64-char hex CSRF token and leaves no raw placeholder
    let html_res = send_raw_http(port, "GET", "/", &[]);
    assert_eq!(html_res.status, 200);
    assert!(!html_res.body.contains("__AGY_PULSE_CSRF_TOKEN__"));
    let marker = "var CSRF_TOKEN = '";
    let start = html_res
        .body
        .find(marker)
        .expect("dashboard HTML must contain CSRF_TOKEN declaration")
        + marker.len();
    let rest = &html_res.body[start..];
    let end = rest.find('\'').expect("closing quote for CSRF_TOKEN");
    let token = &rest[..end];
    assert_eq!(token.len(), 64, "CSRF token must be 64 hex chars: {token}");
    assert!(
        token
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
        "CSRF token must be lowercase hex: {token}"
    );

    // 2. Oversized request body (> 64 KiB) returns 413 Payload Too Large
    let oversized_body = "x".repeat(65 * 1024);
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect loopback");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let req_head = format!(
        "POST /api/sync-remote HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        oversized_body.len()
    );
    let _ = stream.write_all(req_head.as_bytes());
    let _ = stream.write_all(oversized_body.as_bytes());
    let _ = stream.flush();
    let mut raw_413 = Vec::new();
    let mut buf_413 = [0u8; 4096];
    loop {
        match stream.read(&mut buf_413) {
            Ok(0) => break,
            Ok(n) => raw_413.extend_from_slice(&buf_413[..n]),
            Err(err) if err.kind() == std::io::ErrorKind::ConnectionReset && !raw_413.is_empty() => {
                break;
            }
            Err(err) => panic!("unexpected read error for 413 response: {err}"),
        }
    }
    let text_413 = String::from_utf8_lossy(&raw_413);
    let (head_413, body_413) = text_413
        .split_once("\r\n\r\n")
        .expect("413 HTTP response header/body separator");
    let status_413: u16 = head_413
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(status_413, 413);
    let err_json: Value = serde_json::from_str(body_413).unwrap();
    assert_eq!(err_json["error"], "request body too large");

    // 2b. POST /api/sync-remote parses JSON body > 4 KiB cleanly (no peek limit)
    let origin = format!("http://127.0.0.1:{port}");
    let padded_force_body = format!("{}{{\"force\":true}}", " ".repeat(5 * 1024));
    let padded_res = send_raw_http_with_body(
        port,
        "POST",
        "/api/sync-remote",
        &[
            ("Content-Type", "application/json"),
            ("Origin", &origin),
            ("X-Agy-Pulse-Token", token),
        ],
        &padded_force_body,
    );
    assert_eq!(padded_res.status, 200);
    let padded_json: Value = serde_json::from_str(&padded_res.body).unwrap();
    assert_eq!(padded_json["ok"], true);
    assert_eq!(padded_json["throttled"], false);

    // 3. Initial ledger scan picked up defaultAccount + deviceName from agy-pulse-sync.json
    let ov1: Value = serde_json::from_str(
        &send_raw_http(port, "GET", "/api/overview?range=all", &[]).body,
    )
    .unwrap();
    assert_eq!(ov1["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(ov1["sessions"][0]["accountId"], "alpha@example.com");
    assert_eq!(ov1["sessions"][0]["deviceName"], "studio-alpha");

    // 4. Update agy-pulse-sync.json identity hints, seed a new conversation, and POST /api/sync
    let cfg_v2 = SyncConfig {
        device_name: Some("studio-beta".to_string()),
        default_account: Some("beta@example.com".to_string()),
        ..cfg_v1
    };
    save_sync_config_to(&sync_cfg_path, &cfg_v2).unwrap();

    let conv2_db = env.cli_data_dir.join("conversations").join("hint-conv-2.db");
    let step2 = pb_timestamp_ms(1, step_ts + 5_000);
    let gen2 = build_gen_blob(
        &[0],
        &[GenStepSpec {
            display_name: "Gemini 3 Flash",
            input_tokens: 900,
            output_tokens: 200,
            cache_read_tokens: 0,
            ttft_sec: None,
            stream_sec: None,
            parent_cascade_id: None,
            root_cascade_id: None,
        }],
    );
    write_conv_db(
        &conv2_db,
        Some(&build_trajectory_blob("ws-hint-2", "main", step_ts + 5_000, None)),
        &[(0, step2)],
        &[(0, gen2)],
    );

    let sync_res = send_raw_http(
        port,
        "POST",
        "/api/sync",
        &[("Origin", &origin), ("X-Agy-Pulse-Token", token)],
    );
    assert_eq!(sync_res.status, 200);

    let ov2: Value = serde_json::from_str(
        &send_raw_http(port, "GET", "/api/overview?range=all", &[]).body,
    )
    .unwrap();
    let sessions = ov2["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    let conv2_session = sessions
        .iter()
        .find(|s| s["rootConversationId"] == "hint-conv-2")
        .expect("hint-conv-2 session present");
    assert_eq!(conv2_session["accountId"], "beta@example.com");
    assert_eq!(conv2_session["deviceName"], "studio-beta");
}

#[test]
fn t07_http_wire_rejects_malformed_content_length_and_transfer_encoding_with_400() {
    let env = IsolatedEnv::new();
    let mut server = DashboardServer::start(DashboardServerConfig {
        host: "127.0.0.1".to_string(),
        port: 0,
        no_sync: true,
        ledger_path: Some(env.ledger_path()),
        ..Default::default()
    })
    .unwrap();
    let port = server.info().port;

    for raw_req in [
        format!("POST /api/sync HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: abc\r\nConnection: close\r\n\r\n"),
        format!("POST /api/sync HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: -1\r\nConnection: close\r\n\r\n"),
        format!("POST /api/sync HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 2\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi"),
        format!("POST /api/sync HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n0\r\n\r\n"),
    ] {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        stream.write_all(raw_req.as_bytes()).unwrap();
        stream.flush().unwrap();
        let mut raw_resp = Vec::new();
        let _ = stream.read_to_end(&mut raw_resp);
        let text = String::from_utf8_lossy(&raw_resp);
        let status_line = text.lines().next().unwrap_or_default();
        assert!(
            status_line.starts_with("HTTP/1.1 400"),
            "expected 400 Bad Request for {raw_req:?}, got {text:?}"
        );
    }

    server.shutdown();
}

#[test]
fn t07_bounded_concurrency_and_sse_limits_return_503() {
    let env = IsolatedEnv::new();
    let mut server = DashboardServer::start(DashboardServerConfig {
        host: "127.0.0.1".to_string(),
        port: 0,
        no_sync: true,
        ledger_path: Some(env.ledger_path()),
        max_concurrent_connections: 2,
        max_sse_connections: 2,
        ..Default::default()
    })
    .unwrap();
    let port = server.info().port;

    // 1. Hold 2 incomplete normal connections -> 3rd connection receives 503 Service Unavailable
    let mut held = Vec::new();
    for _ in 0..2 {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(b"GET /api/health HTTP/1.1\r\n").unwrap();
        s.flush().unwrap();
        held.push(s);
    }
    std::thread::sleep(Duration::from_millis(80));

    let overloaded = send_raw_http(port, "GET", "/api/health", &[]);
    assert_eq!(overloaded.status, 503, "expected 503 when saturated: {}", overloaded.body);
    let overloaded_json: Value = serde_json::from_str(&overloaded.body).unwrap();
    assert_eq!(overloaded_json["ok"], false);

    // Drop held connections -> slots release cleanly via RAII guard
    drop(held);
    let mut recovered = false;
    for _ in 0..20 {
        if send_raw_http(port, "GET", "/api/health", &[]).status == 200 {
            recovered = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(recovered, "server must recover after held connections close");

    // 2. Open 2 SSE connections -> normal /api/health still works, 3rd SSE gets 503
    let mut sse_streams = Vec::new();
    for idx in 0..2 {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let req = format!(
            "GET /api/keepalive?client=c-{idx} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: http://127.0.0.1:{port}\r\nAccept: text/event-stream\r\n\r\n"
        );
        s.write_all(req.as_bytes()).unwrap();
        s.flush().unwrap();
        let mut buf = [0u8; 512];
        let n = s.read(&mut buf).unwrap();
        let head = String::from_utf8_lossy(&buf[..n]);
        assert!(head.starts_with("HTTP/1.1 200"), "SSE {idx} failed: {head}");
        sse_streams.push(s);
    }

    assert_eq!(send_raw_http(port, "GET", "/api/health", &[]).status, 200);

    let sse_3rd = send_raw_http(
        port,
        "GET",
        "/api/keepalive?client=c-overflow",
        &[("Origin", &format!("http://127.0.0.1:{port}"))],
    );
    assert_eq!(sse_3rd.status, 503, "expected 503 on 3rd SSE: {}", sse_3rd.body);
    let sse_json: Value = serde_json::from_str(&sse_3rd.body).unwrap();
    assert_eq!(sse_json["ok"], false);

    // Dropping one SSE stream releases SSE quota cleanly
    sse_streams.pop();
    let mut sse_recovered = false;
    for _ in 0..30 {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let req = format!(
            "GET /api/keepalive?client=c-retry HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: http://127.0.0.1:{port}\r\nAccept: text/event-stream\r\n\r\n"
        );
        s.write_all(req.as_bytes()).unwrap();
        s.flush().unwrap();
        let mut buf = [0u8; 256];
        let n = s.read(&mut buf).unwrap_or(0);
        if String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.1 200") {
            sse_recovered = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    assert!(sse_recovered, "SSE slot must be released after SSE client closes");

    drop(sse_streams);
    server.shutdown();
}

#[test]
fn t07_mutating_routes_require_csrf_and_origin_and_keepalive_checks_same_origin() {
    let env = IsolatedEnv::new();
    let mut server = DashboardServer::start(DashboardServerConfig {
        host: "127.0.0.1".to_string(),
        port: 0,
        no_sync: true,
        ledger_path: Some(env.ledger_path()),
        ..Default::default()
    })
    .unwrap();
    let port = server.info().port;
    let token = server.csrf_token().to_string();
    let good_origin = format!("http://127.0.0.1:{port}");

    for path in ["/api/sync", "/api/sync-remote", "/api/ping", "/api/disconnect"] {
        // Missing CSRF token even with valid Origin -> 403
        let no_csrf = send_raw_http(port, "POST", path, &[("Origin", &good_origin)]);
        assert_eq!(no_csrf.status, 403, "{path} must reject missing CSRF token");

        // Wrong CSRF token -> 403
        let bad_csrf = send_raw_http(
            port,
            "POST",
            path,
            &[("Origin", &good_origin), ("X-Agy-Pulse-Token", "0000")],
        );
        assert_eq!(bad_csrf.status, 403, "{path} must reject wrong CSRF token");

        // Missing Origin even with valid CSRF token -> 403
        let no_origin = send_raw_http(port, "POST", path, &[("X-Agy-Pulse-Token", &token)]);
        assert_eq!(no_origin.status, 403, "{path} must reject missing Origin");

        // Sec-Fetch-Site: cross-site even with valid Origin & CSRF -> 403
        let cross_site = send_raw_http(
            port,
            "POST",
            path,
            &[
                ("Origin", &good_origin),
                ("X-Agy-Pulse-Token", &token),
                ("Sec-Fetch-Site", "cross-site"),
            ],
        );
        assert_eq!(cross_site.status, 403, "{path} must reject cross-site Sec-Fetch-Site");

        // Valid Origin + valid CSRF + same-origin -> 200
        let ok = send_raw_http(
            port,
            "POST",
            path,
            &[
                ("Origin", &good_origin),
                ("X-Agy-Pulse-Token", &token),
                ("Sec-Fetch-Site", "same-origin"),
            ],
        );
        assert_eq!(ok.status, 200, "{path} must allow valid Origin + CSRF: {}", ok.body);
    }

    // GET /api/keepalive rejects bad Sec-Fetch-Site & evil Origin, allows same-origin without Origin
    let ka_cross = send_raw_http(
        port,
        "GET",
        "/api/keepalive?client=bad-sec",
        &[("Sec-Fetch-Site", "cross-site")],
    );
    assert_eq!(ka_cross.status, 403);

    let ka_evil = send_raw_http(
        port,
        "GET",
        "/api/keepalive?client=bad-org",
        &[("Origin", "https://evil.example")],
    );
    assert_eq!(ka_evil.status, 403);

    let mut sse = TcpStream::connect(("127.0.0.1", port)).unwrap();
    sse.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    sse.write_all(
        format!(
            "GET /api/keepalive?client=same-site HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nSec-Fetch-Site: same-origin\r\nAccept: text/event-stream\r\n\r\n"
        )
        .as_bytes(),
    )
    .unwrap();
    sse.flush().unwrap();
    let mut sse_buf = [0u8; 256];
    let n = sse.read(&mut sse_buf).unwrap();
    assert!(String::from_utf8_lossy(&sse_buf[..n]).starts_with("HTTP/1.1 200"));
    drop(sse);

    server.shutdown();
}
