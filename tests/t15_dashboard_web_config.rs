#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use agy_pulse::config::LanguageSetting;
use agy_pulse::dashboard::{DashboardServer, DashboardServerConfig};

static ENV_MUTEX: Mutex<()> = Mutex::new(());

struct EnvGuard {
    saved: Vec<(String, Option<OsString>)>,
}

impl EnvGuard {
    fn new() -> Self {
        Self { saved: Vec::new() }
    }

    fn record(&mut self, key: &str) {
        if !self.saved.iter().any(|(k, _)| k == key) {
            self.saved.push((key.to_string(), env::var_os(key)));
        }
    }

    fn set(&mut self, key: &str, val: &str) {
        self.record(key);
        // SAFETY: guarded by `ENV_MUTEX` in all in-process tests in this integration binary.
        unsafe { env::set_var(key, val) };
    }

    fn remove(&mut self, key: &str) {
        self.record(key);
        // SAFETY: guarded by `ENV_MUTEX` in all in-process tests in this integration binary.
        unsafe { env::remove_var(key) };
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, orig) in self.saved.iter().rev() {
            match orig {
                Some(v) => unsafe { env::set_var(key, v) },
                None => unsafe { env::remove_var(key) },
            }
        }
    }
}

struct IsolatedEnv {
    _temp: TempDir,
    home_dir: PathBuf,
    cli_data_dir: PathBuf,
    ide_data_dir: PathBuf,
    global_cfg_dir: PathBuf,
    xdg_cfg_dir: PathBuf,
    workspace_dir: PathBuf,
    sync_cfg_path: PathBuf,
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
        let sync_cfg_path = root.join("sync-config.json");
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
            sync_cfg_path,
        }
    }

    fn apply_env(&self, guard: &mut EnvGuard) {
        guard.set("HOME", self.home_dir.to_str().unwrap());
        guard.set("USERPROFILE", self.home_dir.to_str().unwrap());
        guard.set(
            "XDG_DATA_HOME",
            self.home_dir.join(".local/share").to_str().unwrap(),
        );
        guard.set("XDG_CONFIG_HOME", self.xdg_cfg_dir.to_str().unwrap());
        guard.set("AGY_PULSE_DATA_DIR", self.cli_data_dir.to_str().unwrap());
        guard.set("AGY_PULSE_IDE_DATA_DIR", self.ide_data_dir.to_str().unwrap());
        guard.set(
            "AGY_PULSE_GLOBAL_CONFIG_DIR",
            self.global_cfg_dir.to_str().unwrap(),
        );
        guard.set("AGY_PULSE_SYNC_CONFIG", self.sync_cfg_path.to_str().unwrap());
        guard.set("LC_ALL", "en_US.UTF-8");
        guard.set("LANG", "en_US.UTF-8");
        guard.remove("LC_CTYPE");
        guard.remove("AGY_PULSE_LANGUAGE");
        guard.remove("AGY_PULSE_DASHBOARD_PORT");
        guard.remove("AGY_PULSE_DASHBOARD_IDLE_SEC");
        guard.remove("AGY_PULSE_FORCE_ASCII");
        guard.remove("AGY_PULSE_FORCE_UNICODE");
    }

    fn global_config_path(&self) -> PathBuf {
        self.global_cfg_dir.join("agy-pulse.config.json")
    }

    fn ledger_path(&self) -> PathBuf {
        self.cli_data_dir.join("agy-pulse-ledger.sqlite")
    }

    fn start_server(&self, idle_timeout_ms: u64) -> DashboardServer {
        DashboardServer::start(DashboardServerConfig {
            port: 0,
            lang: LanguageSetting::En,
            no_sync: true,
            defer_initial_sync: true,
            idle_timeout_ms,
            ledger_path: Some(self.ledger_path()),
            source_roots: Some(Vec::new()),
            ..Default::default()
        })
        .expect("start isolated dashboard server")
    }
}

struct HttpResponse {
    status: u16,
    body: String,
}

fn send_http(
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
    let status_line = head.lines().next().expect("status line");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .expect("status code")
        .parse()
        .expect("parse status code");
    HttpResponse {
        status,
        body: resp_body.to_string(),
    }
}

fn extract_html_csrf_token(port: u16) -> String {
    let res = send_http(port, "GET", "/", &[], "");
    assert_eq!(res.status, 200);
    let marker = "var CSRF_TOKEN = '";
    let start = res.body.find(marker).expect("CSRF_TOKEN in HTML") + marker.len();
    let rest = &res.body[start..];
    let end = rest.find('\'').expect("closing quote");
    rest[..end].to_string()
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[test]
fn t15_security_guards_csrf_same_origin_content_type_and_method() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let env = IsolatedEnv::new();
    let mut guard = EnvGuard::new();
    env.apply_env(&mut guard);

    let mut server = env.start_server(300_000);
    let port = server.info().port;
    let token = extract_html_csrf_token(port);
    assert_eq!(token, server.csrf_token());

    // 1. Missing or wrong CSRF token -> 403
    for (method, path, body) in [
        ("GET", "/api/config/pulse", ""),
        ("PUT", "/api/config/pulse", "{}"),
        ("POST", "/api/config/pulse/preview", "{}"),
    ] {
        let missing = send_http(
            port,
            method,
            path,
            &[("Content-Type", "application/json")],
            body,
        );
        assert_eq!(missing.status, 403);
        let j: Value = serde_json::from_str(&missing.body).unwrap();
        assert_eq!(j["error"], "invalid or missing CSRF token");

        let wrong = send_http(
            port,
            method,
            path,
            &[
                ("X-Agy-Pulse-Token", "0000"),
                ("Content-Type", "application/json"),
            ],
            body,
        );
        assert_eq!(wrong.status, 403);
        let j_wrong: Value = serde_json::from_str(&wrong.body).unwrap();
        assert_eq!(j_wrong["error"], "invalid or missing CSRF token");
    }

    // 2. Cross-site Sec-Fetch-Site -> 403
    let cross_site = send_http(
        port,
        "GET",
        "/api/config/pulse",
        &[
            ("X-Agy-Pulse-Token", &token),
            ("Sec-Fetch-Site", "cross-site"),
        ],
        "",
    );
    assert_eq!(cross_site.status, 403);
    assert_eq!(
        serde_json::from_str::<Value>(&cross_site.body).unwrap()["error"],
        "cross-origin request rejected"
    );

    // 3. Disallowed Origin -> 403
    let bad_origin = send_http(
        port,
        "POST",
        "/api/config/pulse/preview",
        &[
            ("X-Agy-Pulse-Token", &token),
            ("Origin", "https://evil.example"),
            ("Content-Type", "application/json"),
        ],
        r#"{"config":{}}"#,
    );
    assert_eq!(bad_origin.status, 403);
    assert_eq!(
        serde_json::from_str::<Value>(&bad_origin.body).unwrap()["error"],
        "cross-origin request rejected"
    );

    // 4. Non-JSON Content-Type on PUT / POST -> 400
    for (method, path) in [
        ("PUT", "/api/config/pulse"),
        ("POST", "/api/config/pulse/preview"),
    ] {
        let bad_ct = send_http(
            port,
            method,
            path,
            &[
                ("X-Agy-Pulse-Token", &token),
                ("Content-Type", "text/plain"),
            ],
            "{}",
        );
        assert_eq!(bad_ct.status, 400);
        assert_eq!(
            serde_json::from_str::<Value>(&bad_ct.body).unwrap()["error"],
            "Content-Type must be application/json"
        );
    }

    // 5. Unsupported HTTP methods -> 405
    for (method, path) in [
        ("POST", "/api/config/pulse"),
        ("DELETE", "/api/config/pulse"),
        ("GET", "/api/config/pulse/preview"),
        ("PUT", "/api/config/pulse/preview"),
    ] {
        let res = send_http(
            port,
            method,
            path,
            &[
                ("X-Agy-Pulse-Token", &token),
                ("Content-Type", "application/json"),
            ],
            "{}",
        );
        assert_eq!(res.status, 405);
        assert_eq!(
            serde_json::from_str::<Value>(&res.body).unwrap()["error"],
            "Method not allowed"
        );
    }

    // 6. Valid same-origin localhost headers succeed
    let ok = send_http(
        port,
        "GET",
        "/api/config/pulse",
        &[
            ("X-Agy-Pulse-Token", &token),
            ("Sec-Fetch-Site", "Same-Origin"),
            ("Origin", &format!("http://localhost:{port}")),
        ],
        "",
    );
    assert_eq!(ok.status, 200);
    let ok_json: Value = serde_json::from_str(&ok.body).unwrap();
    assert_eq!(ok_json["runtime"]["currentPort"], port);
    assert_eq!(ok_json["runtime"]["currentIdleTimeoutSec"], 300);

    server.shutdown();
}

#[test]
fn t15_pulse_config_get_absent_put_preserve_unknown_clear_optionals_and_restart_required() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let env = IsolatedEnv::new();
    let mut guard = EnvGuard::new();
    env.apply_env(&mut guard);

    // Seed an unrelated workspace config in cwd; GET /api/config/pulse must ignore it.
    let ws_cfg = env.workspace_dir.join("agy-pulse.config.json");
    fs::write(&ws_cfg, r#"{"enabled":false,"language":"zh"}"#).unwrap();

    let mut server = env.start_server(600_000);
    let port = server.info().port;
    let token = server.csrf_token().to_string();

    // 1. Initial GET when global file does not exist
    let get1 = send_http(
        port,
        "GET",
        "/api/config/pulse",
        &[("X-Agy-Pulse-Token", &token)],
        "",
    );
    assert_eq!(get1.status, 200);
    let j1: Value = serde_json::from_str(&get1.body).unwrap();
    assert_eq!(j1["version"], "absent");
    assert_eq!(j1["exists"], false);
    assert!(j1["parseError"].is_null());
    assert_eq!(
        j1["filePath"].as_str().unwrap(),
        env.global_config_path().to_string_lossy()
    );
    assert_eq!(j1["config"], j1["defaults"]);
    assert_eq!(j1["config"]["enabled"], true);
    assert_eq!(j1["config"]["language"], "auto");
    assert_eq!(j1["config"]["display"]["showContextMonitor"], true);
    assert_eq!(j1["config"]["display"]["showModel"], false);
    assert_eq!(j1["config"]["display"]["breadcrumbCount"], 3);
    assert_eq!(j1["config"]["display"]["quotaStyle"], "table");
    assert!(j1["config"]["display"].get("columnWidth").is_none());
    assert_eq!(
        j1["config"]["icons"],
        json!({ "branch": null, "token": null, "ctx": null })
    );
    assert_eq!(j1["themePresets"].as_array().unwrap().len(), 4);
    assert_eq!(
        j1["allowedColors"],
        json!(["green", "gray", "blue", "magenta", "yellow", "cyan", "red"])
    );
    assert_eq!(j1["runtime"]["currentPort"], port);
    assert_eq!(j1["runtime"]["currentIdleTimeoutSec"], 600);

    // 2. PUT creating global file with custom port & idle timeout -> restartRequired = true
    let mut cfg1 = j1["config"].clone();
    cfg1["language"] = json!("zh");
    cfg1["theme"] = json!("Ocean Blue");
    cfg1["display"]["showModel"] = json!(true);
    cfg1["display"]["dashboardPort"] = json!(4567);
    cfg1["display"]["dashboardIdleTimeoutSec"] = json!(900);
    cfg1["display"]["unicode"] = json!(true);
    cfg1["display"]["username"] = json!("  alice  ");
    cfg1["display"]["cacheHitThresholds"] = json!({ "excellent": 85.0, "partial": 45.0 });
    cfg1["display"]["modelSpeedExpireTurns"] = json!(5);
    cfg1["display"]["modelSpeedExpireMinutes"] = json!(2.5);
    cfg1["icons"] = json!({ "branch": " B ", "token": "T", "ctx": "C" });

    let put1_body = serde_json::to_string(&json!({
        "expectedVersion": "absent",
        "config": cfg1,
    }))
    .unwrap();
    let put1 = send_http(
        port,
        "PUT",
        "/api/config/pulse",
        &[
            ("X-Agy-Pulse-Token", &token),
            ("Content-Type", "application/json"),
        ],
        &put1_body,
    );
    assert_eq!(put1.status, 200);
    let p1: Value = serde_json::from_str(&put1.body).unwrap();
    assert_eq!(p1["exists"], true);
    assert!(p1["parseError"].is_null());
    assert_eq!(p1["restartRequired"], true);
    assert_eq!(p1["resolvedLanguage"], "zh");
    assert_eq!(p1["config"]["display"]["username"], "alice");
    assert_eq!(p1["config"]["icons"]["branch"], "B");

    let disk_bytes_1 = fs::read(env.global_config_path()).unwrap();
    assert_eq!(p1["version"], sha256_hex(&disk_bytes_1));

    // 3. Add unknown fields + display.columnWidth + icons.{plan,model} to disk JSON,
    //    then clear managed optionals via PUT and verify unknown keys are preserved while cleared keys are removed.
    let mut disk_val: Value = serde_json::from_slice(&disk_bytes_1).unwrap();
    disk_val["customTopLevel"] = json!({ "keep": true });
    disk_val["display"]["columnWidth"] = json!(110);
    disk_val["display"]["customNested"] = json!("preserve-me");
    disk_val["icons"]["plan"] = json!("P");
    disk_val["icons"]["model"] = json!("M");
    let modified_bytes = serde_json::to_vec_pretty(&disk_val).unwrap();
    fs::write(env.global_config_path(), &modified_bytes).unwrap();
    let expected_v2 = sha256_hex(&modified_bytes);

    let get2 = send_http(
        port,
        "GET",
        "/api/config/pulse",
        &[("X-Agy-Pulse-Token", &token)],
        "",
    );
    assert_eq!(get2.status, 200);
    let j2: Value = serde_json::from_str(&get2.body).unwrap();
    assert_eq!(j2["version"], expected_v2);
    assert!(j2["config"]["display"].get("columnWidth").is_none());
    assert!(j2["config"]["icons"].get("plan").is_none());
    assert!(j2["config"].get("customTopLevel").is_none());

    let mut cfg2 = j2["config"].clone();
    cfg2["language"] = json!("en");
    cfg2["display"]["showCost"] = json!(true);
    // Keep dashboardPort & dashboardIdleTimeoutSec unchanged -> restartRequired = false
    cfg2["display"]["unicode"] = Value::Null;
    cfg2["display"]["username"] = json!("   ");
    cfg2["display"]["cacheHitThresholds"] = Value::Null;
    cfg2["display"]["modelSpeedExpireTurns"] = Value::Null;
    cfg2["display"]["modelSpeedExpireMinutes"] = Value::Null;
    cfg2["icons"] = json!({ "branch": "  ", "token": null, "ctx": null });

    let put2_body = serde_json::to_string(&json!({
        "expectedVersion": expected_v2,
        "config": cfg2,
    }))
    .unwrap();
    let put2 = send_http(
        port,
        "PUT",
        "/api/config/pulse",
        &[
            ("X-Agy-Pulse-Token", &token),
            ("Content-Type", "application/json"),
        ],
        &put2_body,
    );
    assert_eq!(put2.status, 200);
    let p2: Value = serde_json::from_str(&put2.body).unwrap();
    assert_eq!(p2["restartRequired"], false);
    assert_eq!(p2["resolvedLanguage"], "en");
    assert!(p2["config"]["display"]["unicode"].is_null());
    assert!(p2["config"]["display"]["username"].is_null());
    assert!(p2["config"]["display"]["cacheHitThresholds"].is_null());
    assert_eq!(
        p2["config"]["icons"],
        json!({ "branch": null, "token": null, "ctx": null })
    );

    let disk_after: Value =
        serde_json::from_slice(&fs::read(env.global_config_path()).unwrap()).unwrap();
    assert_eq!(disk_after["customTopLevel"], json!({ "keep": true }));
    assert_eq!(disk_after["display"]["columnWidth"], 110);
    assert_eq!(disk_after["display"]["customNested"], "preserve-me");
    assert_eq!(disk_after["display"]["showCost"], true);
    assert!(disk_after["display"].get("unicode").is_none());
    assert!(disk_after["display"].get("username").is_none());
    assert!(disk_after["display"].get("cacheHitThresholds").is_none());
    assert!(disk_after["display"].get("modelSpeedExpireTurns").is_none());
    assert!(disk_after["display"].get("modelSpeedExpireMinutes").is_none());
    assert_eq!(disk_after["icons"]["plan"], "P");
    assert_eq!(disk_after["icons"]["model"], "M");
    assert!(disk_after["icons"].get("branch").is_none());
    assert!(disk_after["icons"].get("token").is_none());
    assert!(disk_after["icons"].get("ctx").is_none());

    server.shutdown();
}

#[test]
fn t15_pulse_config_409_conflict_and_validation_errors() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let env = IsolatedEnv::new();
    let mut guard = EnvGuard::new();
    env.apply_env(&mut guard);

    let mut server = env.start_server(600_000);
    let port = server.info().port;
    let token = server.csrf_token().to_string();

    // Seed initial file via PUT
    let get1: Value = serde_json::from_str(
        &send_http(
            port,
            "GET",
            "/api/config/pulse",
            &[("X-Agy-Pulse-Token", &token)],
            "",
        )
        .body,
    )
    .unwrap();
    let put1_body = serde_json::to_string(&json!({
        "expectedVersion": "absent",
        "config": get1["config"],
    }))
    .unwrap();
    let put1: Value = serde_json::from_str(
        &send_http(
            port,
            "PUT",
            "/api/config/pulse",
            &[
                ("X-Agy-Pulse-Token", &token),
                ("Content-Type", "application/json"),
            ],
            &put1_body,
        )
        .body,
    )
    .unwrap();
    let v1 = put1["version"].as_str().unwrap().to_string();

    // Simulate external disk modification -> v2
    let mut ext_val: Value =
        serde_json::from_slice(&fs::read(env.global_config_path()).unwrap()).unwrap();
    ext_val["display"]["showModel"] = json!(true);
    let ext_bytes = serde_json::to_vec_pretty(&ext_val).unwrap();
    fs::write(env.global_config_path(), &ext_bytes).unwrap();
    let v2 = sha256_hex(&ext_bytes);
    assert_ne!(v1, v2);

    // Submitting with stale v1 -> 409 Conflict with currentVersion = v2
    let conflict_body = serde_json::to_string(&json!({
        "expectedVersion": v1,
        "config": get1["config"],
    }))
    .unwrap();
    let conflict_res = send_http(
        port,
        "PUT",
        "/api/config/pulse",
        &[
            ("X-Agy-Pulse-Token", &token),
            ("Content-Type", "application/json"),
        ],
        &conflict_body,
    );
    assert_eq!(conflict_res.status, 409);
    let conflict_json: Value = serde_json::from_str(&conflict_res.body).unwrap();
    assert_eq!(conflict_json["error"], "version_conflict");
    assert_eq!(conflict_json["currentVersion"], v2);

    // Validation failures -> 400 with exact field
    let invalid_cases: Vec<(&str, Value)> = vec![
        (
            "thresholds.warning",
            json!({ "thresholds": { "warning": 0.95, "critical": 0.5 } }),
        ),
        (
            "thresholds.critical",
            json!({ "thresholds": { "warning": 0.5, "critical": 1.2 } }),
        ),
        ("theme", json!({ "theme": "Neon Pink" })),
        (
            "theme.primary",
            json!({
                "theme": {
                    "primary": "purple",
                    "secondary": "gray",
                    "warning": "yellow",
                    "critical": "red"
                }
            }),
        ),
        (
            "display.dashboardPort",
            json!({ "display": { "dashboardPort": 0 } }),
        ),
        (
            "display.breadcrumbCount",
            json!({ "display": { "breadcrumbCount": 21 } }),
        ),
        (
            "display.terminalWidth",
            json!({ "display": { "terminalWidth": 10 } }),
        ),
        (
            "display.cacheHitThresholds.partial",
            json!({ "display": { "cacheHitThresholds": { "excellent": 40.0, "partial": 60.0 } } }),
        ),
        (
            "display.modelSpeedExpireTurns",
            json!({ "display": { "modelSpeedExpireTurns": 0 } }),
        ),
        (
            "display.modelSpeedExpireMinutes",
            json!({ "display": { "modelSpeedExpireMinutes": 0.0 } }),
        ),
        (
            "display.username",
            json!({ "display": { "username": "bad\nuser" } }),
        ),
        ("icons.branch", json!({ "icons": { "branch": "123456789" } })),
        ("icons.token", json!({ "icons": { "token": "a\tb" } })),
    ];

    for (expected_field, patch) in invalid_cases {
        let body = serde_json::to_string(&json!({
            "expectedVersion": v2,
            "config": patch,
        }))
        .unwrap();
        let res = send_http(
            port,
            "PUT",
            "/api/config/pulse",
            &[
                ("X-Agy-Pulse-Token", &token),
                ("Content-Type", "application/json"),
            ],
            &body,
        );
        assert_eq!(res.status, 400, "expected 400 for {expected_field}");
        let j: Value = serde_json::from_str(&res.body).unwrap();
        assert_eq!(j["field"], expected_field);
    }

    // Confirm disk file was untouched by rejected requests
    assert_eq!(fs::read(env.global_config_path()).unwrap(), ext_bytes);

    server.shutdown();
}

#[test]
fn t15_pulse_config_env_overrides_and_corrupt_json_recovery() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let env = IsolatedEnv::new();
    let mut guard = EnvGuard::new();
    env.apply_env(&mut guard);

    guard.set("AGY_PULSE_LANGUAGE", "zh");
    guard.set("AGY_PULSE_DASHBOARD_PORT", "4100");
    guard.set("AGY_PULSE_DASHBOARD_IDLE_SEC", "120");
    guard.set("AGY_PULSE_FORCE_ASCII", "1");

    let initial_disk = serde_json::to_vec_pretty(&json!({
        "language": "en",
        "display": {
            "dashboardPort": 3800,
            "unicode": true
        }
    }))
    .unwrap();
    fs::write(env.global_config_path(), &initial_disk).unwrap();

    let mut server = env.start_server(600_000);
    let port = server.info().port;
    let token = server.csrf_token().to_string();

    // 1. GET reports envOverrides separately without mutating config fields
    let get1 = send_http(
        port,
        "GET",
        "/api/config/pulse",
        &[("X-Agy-Pulse-Token", &token)],
        "",
    );
    assert_eq!(get1.status, 200);
    let j1: Value = serde_json::from_str(&get1.body).unwrap();
    assert_eq!(j1["config"]["language"], "en");
    assert_eq!(j1["config"]["display"]["dashboardPort"], 3800);
    assert_eq!(j1["config"]["display"]["unicode"], true);
    assert_eq!(
        j1["envOverrides"]["language"],
        json!({ "envVar": "AGY_PULSE_LANGUAGE", "value": "zh" })
    );
    assert_eq!(
        j1["envOverrides"]["dashboardPort"],
        json!({ "envVar": "AGY_PULSE_DASHBOARD_PORT", "value": "4100" })
    );
    assert_eq!(
        j1["envOverrides"]["dashboardIdleTimeoutSec"],
        json!({ "envVar": "AGY_PULSE_DASHBOARD_IDLE_SEC", "value": "120" })
    );
    assert_eq!(
        j1["envOverrides"]["unicode"],
        json!({ "envVar": "AGY_PULSE_FORCE_ASCII", "value": "false" })
    );

    // 2. PUT never persists env overrides to disk, and resolves language with AGY_PULSE_LANGUAGE
    let mut cfg_to_save = j1["config"].clone();
    cfg_to_save["display"]["dashboardPort"] = Value::Null;
    cfg_to_save["display"]["unicode"] = Value::Null;
    let put1_body = serde_json::to_string(&json!({
        "expectedVersion": j1["version"],
        "config": cfg_to_save,
    }))
    .unwrap();
    let put1 = send_http(
        port,
        "PUT",
        "/api/config/pulse",
        &[
            ("X-Agy-Pulse-Token", &token),
            ("Content-Type", "application/json"),
        ],
        &put1_body,
    );
    assert_eq!(put1.status, 200);
    let p1: Value = serde_json::from_str(&put1.body).unwrap();
    assert_eq!(p1["resolvedLanguage"], "zh");
    assert_eq!(p1["config"]["language"], "en");
    assert_eq!(
        p1["envOverrides"]["language"],
        json!({ "envVar": "AGY_PULSE_LANGUAGE", "value": "zh" })
    );

    let disk_after: Value =
        serde_json::from_slice(&fs::read(env.global_config_path()).unwrap()).unwrap();
    assert_eq!(disk_after["language"], "en");
    assert!(disk_after["display"].get("dashboardPort").is_none());
    assert!(disk_after["display"].get("unicode").is_none());

    // 3. Corrupt JSON on disk -> GET returns parseError + real sha256 version, PUT recovers cleanly
    let corrupt_bytes = b"{not valid json!!";
    fs::write(env.global_config_path(), corrupt_bytes).unwrap();
    let corrupt_ver = sha256_hex(corrupt_bytes);

    let get_corrupt = send_http(
        port,
        "GET",
        "/api/config/pulse",
        &[("X-Agy-Pulse-Token", &token)],
        "",
    );
    assert_eq!(get_corrupt.status, 200);
    let jc: Value = serde_json::from_str(&get_corrupt.body).unwrap();
    assert_eq!(jc["exists"], true);
    assert_eq!(jc["version"], corrupt_ver);
    assert!(
        jc["parseError"]
            .as_str()
            .unwrap()
            .contains("invalid JSON")
    );

    let recover_body = serde_json::to_string(&json!({
        "expectedVersion": corrupt_ver,
        "config": { "language": "zh" },
    }))
    .unwrap();
    let recover_res = send_http(
        port,
        "PUT",
        "/api/config/pulse",
        &[
            ("X-Agy-Pulse-Token", &token),
            ("Content-Type", "application/json"),
        ],
        &recover_body,
    );
    assert_eq!(recover_res.status, 200);
    let jr: Value = serde_json::from_str(&recover_res.body).unwrap();
    assert_eq!(jr["exists"], true);
    assert!(jr["parseError"].is_null());
    assert_eq!(jr["config"]["language"], "zh");

    server.shutdown();
}

#[test]
fn t15_pulse_preview_endpoint_no_disk_write() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let env = IsolatedEnv::new();
    let mut guard = EnvGuard::new();
    env.apply_env(&mut guard);

    let mut server = env.start_server(600_000);
    let port = server.info().port;
    let token = server.csrf_token().to_string();

    // 1. Disabled statusline preview
    let disabled_res = send_http(
        port,
        "POST",
        "/api/config/pulse/preview",
        &[
            ("X-Agy-Pulse-Token", &token),
            ("Content-Type", "application/json"),
        ],
        r#"{"config":{"enabled":false,"language":"en"}}"#,
    );
    assert_eq!(disabled_res.status, 200);
    let d_json: Value = serde_json::from_str(&disabled_res.body).unwrap();
    assert_eq!(d_json["resolvedLanguage"], "en");
    assert!(d_json["ansi"].as_str().unwrap().contains("disabled"));

    // 2. Chinese + Ocean Blue preset + extra display segments
    let zh_ocean_body = serde_json::to_string(&json!({
        "config": {
            "enabled": true,
            "language": "zh",
            "theme": "Ocean Blue",
            "display": {
                "showModel": true,
                "showCost": true,
                "showGitBranch": true
            }
        },
        "terminalCols": 120
    }))
    .unwrap();
    let zh_res = send_http(
        port,
        "POST",
        "/api/config/pulse/preview",
        &[
            ("X-Agy-Pulse-Token", &token),
            ("Content-Type", "application/json"),
        ],
        &zh_ocean_body,
    );
    assert_eq!(zh_res.status, 200);
    let zh_json: Value = serde_json::from_str(&zh_res.body).unwrap();
    assert_eq!(zh_json["resolvedLanguage"], "zh");
    let zh_ansi = zh_json["ansi"].as_str().unwrap();
    assert!(zh_ansi.contains("\x1b[34m"), "expected blue ANSI SGR in {zh_ansi:?}");
    assert!(
        zh_ansi.contains("上下文") || zh_ansi.contains("目录"),
        "expected localized Chinese preview output in {zh_ansi:?}"
    );

    // 3. Custom 4-color theme
    let custom_body = serde_json::to_string(&json!({
        "config": {
            "enabled": true,
            "language": "en",
            "theme": {
                "primary": "magenta",
                "secondary": "gray",
                "warning": "cyan",
                "critical": "red"
            }
        }
    }))
    .unwrap();
    let custom_res = send_http(
        port,
        "POST",
        "/api/config/pulse/preview",
        &[
            ("X-Agy-Pulse-Token", &token),
            ("Content-Type", "application/json"),
        ],
        &custom_body,
    );
    assert_eq!(custom_res.status, 200);
    let custom_ansi = serde_json::from_str::<Value>(&custom_res.body).unwrap()["ansi"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        custom_ansi.contains("\x1b[35m"),
        "expected magenta ANSI SGR in {custom_ansi:?}"
    );

    // 4. Invalid preview config -> 400 with field
    let bad_preview = send_http(
        port,
        "POST",
        "/api/config/pulse/preview",
        &[
            ("X-Agy-Pulse-Token", &token),
            ("Content-Type", "application/json"),
        ],
        r#"{"config":{"thresholds":{"warning":0.9,"critical":0.1}}}"#,
    );
    assert_eq!(bad_preview.status, 400);
    assert_eq!(
        serde_json::from_str::<Value>(&bad_preview.body).unwrap()["field"],
        "thresholds.warning"
    );

    // 5. Preview must never create or touch the global config file on disk
    assert!(!env.global_config_path().exists());

    server.shutdown();
}
