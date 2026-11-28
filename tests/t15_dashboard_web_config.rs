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
        guard.remove("AGY_PULSE_SYNC_TOKEN");
        guard.remove("AGY_PULSE_SYNC_PASSWORD");
        guard.remove("AGY_PULSE_S3_ACCESS_KEY");
        guard.remove("AGY_PULSE_S3_SECRET_KEY");
    }

    fn global_config_path(&self) -> PathBuf {
        self.global_cfg_dir.join("agy-pulse.config.json")
    }

    fn ledger_path(&self) -> PathBuf {
        self.cli_data_dir.join("agy-pulse-ledger.sqlite")
    }

    fn start_server(&self, idle_timeout_ms: u64) -> DashboardServer {
        self.start_server_with_roots(idle_timeout_ms, Some(Vec::new()))
    }

    fn start_server_with_roots(
        &self,
        idle_timeout_ms: u64,
        source_roots: Option<Vec<agy_pulse::ledger::SourceRoot>>,
    ) -> DashboardServer {
        DashboardServer::start(DashboardServerConfig {
            port: 0,
            lang: LanguageSetting::En,
            no_sync: true,
            defer_initial_sync: true,
            idle_timeout_ms,
            ledger_path: Some(self.ledger_path()),
            source_roots,
            ..Default::default()
        })
        .expect("start isolated dashboard server")
    }
}

struct HttpResponse {
    status: u16,
    head: String,
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
        head: head.to_string(),
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
    assert_eq!(j1["config"]["display"]["showModelCache"], true);
    for retired_key in [
        "showModel",
        "showTokenBar",
        "showContextBar",
        "showTurnCacheHitRate",
        "showCost",
    ] {
        assert!(
            j1["config"]["display"].get(retired_key).is_none(),
            "retired display key {retired_key} must not appear in DTO"
        );
    }
    for active_key in [
        "showContextMonitor",
        "showModelCache",
        "showCurrentDir",
        "showCycleMode",
        "showSpeed",
        "showBreadcrumbs",
        "showGitBranch",
        "showUsername",
        "showDailyCost",
        "showWeeklyCost",
        "showRoutedCost",
        "showDashboardUrl",
    ] {
        assert!(
            j1["config"]["display"][active_key].is_boolean(),
            "expected boolean display key {active_key} in DTO"
        );
    }
    assert_eq!(j1["config"]["display"]["breadcrumbCount"], 3);
    assert_eq!(j1["config"]["display"]["quotaStyle"], "table");
    assert!(j1["config"]["display"].get("columnWidth").is_none());
    assert_eq!(j1["config"]["icons"], json!({ "branch": null }));
    assert_eq!(j1["themePresets"].as_array().unwrap().len(), 4);
    assert_eq!(
        j1["allowedColors"],
        json!(["green", "gray", "blue", "magenta", "yellow", "cyan", "red"])
    );
    assert_eq!(j1["runtime"]["currentPort"], port);
    assert_eq!(j1["runtime"]["currentIdleTimeoutSec"], 600);

    // 2. PUT creating global file with custom port & idle timeout -> restartRequired = true
    //    Also include retired keys in PUT payload to verify silent compatibility.
    let mut cfg1 = j1["config"].clone();
    cfg1["language"] = json!("zh");
    cfg1["theme"] = json!("Ocean Blue");
    cfg1["display"]["showGitBranch"] = json!(true);
    cfg1["display"]["showModel"] = json!(true);
    cfg1["display"]["showTokenBar"] = json!(true);
    cfg1["display"]["showContextBar"] = json!(true);
    cfg1["display"]["showTurnCacheHitRate"] = json!(true);
    cfg1["display"]["showCost"] = json!(true);
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
    assert_eq!(p1["config"]["display"]["showGitBranch"], true);
    assert_eq!(p1["config"]["display"]["username"], "alice");
    assert_eq!(p1["config"]["icons"], json!({ "branch": "B" }));
    assert!(p1["config"]["display"].get("showModel").is_none());

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
    cfg2["display"]["showDailyCost"] = json!(true);
    // Keep dashboardPort & dashboardIdleTimeoutSec unchanged -> restartRequired = false
    cfg2["display"]["unicode"] = Value::Null;
    cfg2["display"]["username"] = json!("   ");
    cfg2["display"]["cacheHitThresholds"] = Value::Null;
    cfg2["display"]["modelSpeedExpireTurns"] = Value::Null;
    cfg2["display"]["modelSpeedExpireMinutes"] = Value::Null;
    cfg2["icons"] = json!({ "branch": "  " });

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
    assert_eq!(p2["config"]["icons"], json!({ "branch": null }));

    let disk_after: Value =
        serde_json::from_slice(&fs::read(env.global_config_path()).unwrap()).unwrap();
    assert_eq!(disk_after["customTopLevel"], json!({ "keep": true }));
    assert_eq!(disk_after["display"]["columnWidth"], 110);
    assert_eq!(disk_after["display"]["customNested"], "preserve-me");
    assert_eq!(disk_after["display"]["showDailyCost"], true);
    assert!(disk_after["display"].get("showModel").is_none());
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
    ext_val["display"]["showGitBranch"] = json!(true);
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
        ("icons.branch", json!({ "icons": { "branch": "a\tb" } })),
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

// ============================================================================
// Sync Config HTTP API Tests (/api/config/sync, /api/config/sync/test)
// ============================================================================

struct MockSyncHttpServer {
    port: u16,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl MockSyncHttpServer {
    fn start<F>(handler: F) -> Self
    where
        F: Fn(&str, &str, &[(String, String)], &[u8]) -> (u16, &'static str, Vec<u8>)
            + Send
            + Sync
            + 'static,
    {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_flag = std::sync::Arc::clone(&stop);
        let handler = std::sync::Arc::new(handler);

        let handle = std::thread::spawn(move || {
            while !stop_flag.load(std::sync::atomic::Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

                        let mut buf = Vec::new();
                        let mut tmp = [0u8; 4096];
                        let header_end = loop {
                            let n = stream.read(&mut tmp).unwrap_or(0);
                            if n == 0 {
                                break None;
                            }
                            buf.extend_from_slice(&tmp[..n]);
                            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                                break Some(pos);
                            }
                        };
                        let Some(pos) = header_end else {
                            continue;
                        };
                        let head = String::from_utf8_lossy(&buf[..pos]).into_owned();
                        let mut lines = head.lines();
                        let req_line = lines.next().unwrap_or("");
                        let mut parts = req_line.split_whitespace();
                        let method = parts.next().unwrap_or("").to_string();
                        let target = parts.next().unwrap_or("").to_string();
                        let mut headers = Vec::new();
                        let mut content_len = 0usize;
                        for line in lines {
                            if let Some((k, v)) = line.split_once(':') {
                                let name = k.trim().to_ascii_lowercase();
                                let val = v.trim().to_string();
                                if name == "content-length" {
                                    content_len = val.parse().unwrap_or(0);
                                }
                                headers.push((name, val));
                            }
                        }
                        let mut body = buf[pos + 4..].to_vec();
                        while body.len() < content_len {
                            let n = stream.read(&mut tmp).unwrap_or(0);
                            if n == 0 {
                                break;
                            }
                            body.extend_from_slice(&tmp[..n]);
                        }
                        body.truncate(content_len);

                        let (status, reason, resp_body) =
                            handler(&method, &target, &headers, &body);
                        let resp = format!(
                            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            resp_body.len()
                        );
                        let _ = stream.write_all(resp.as_bytes());
                        let _ = stream.write_all(&resp_body);
                        let _ = stream.flush();
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            port,
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for MockSyncHttpServer {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

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

fn write_test_conv_db(db_path: &std::path::Path, cid: &str, ts_ms: i64) {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let conn = rusqlite::Connection::open(db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS steps (idx INTEGER PRIMARY KEY, metadata BLOB);
         CREATE TABLE IF NOT EXISTS gen_metadata (idx INTEGER PRIMARY KEY, data BLOB);
         CREATE TABLE IF NOT EXISTS trajectory_metadata_blob (id TEXT PRIMARY KEY, data BLOB);",
    )
    .unwrap();

    let mut ws = pb_str(1, "file:///work/sync-cfg-test");
    ws.extend(pb_str(4, "main"));
    let mut traj = pb_bytes(1, &ws);
    traj.extend(pb_timestamp_ms(2, ts_ms));
    traj.extend(pb_str(6, cid));

    let step_meta = pb_timestamp_ms(1, ts_ms);

    let mut usage = pb_varint(2, 1_000);
    usage.extend(pb_varint(3, 500));
    usage.extend(pb_varint(5, 200));
    let mut gen_entry = pb_str(21, "Claude 4.6 Sonnet");
    gen_entry.extend(pb_bytes(4, &usage));
    let mut gen_blob = pb_varint(2, 1);
    gen_blob.extend(pb_bytes(1, &gen_entry));

    conn.execute(
        "INSERT INTO trajectory_metadata_blob (id, data) VALUES ('main', ?1)",
        rusqlite::params![traj],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO steps (idx, metadata) VALUES (1, ?1)",
        rusqlite::params![step_meta],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO gen_metadata (idx, data) VALUES (1, ?1)",
        rusqlite::params![gen_blob],
    )
    .unwrap();
}

#[test]
fn t15_sync_config_security_guards_and_methods() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let env = IsolatedEnv::new();
    let mut guard = EnvGuard::new();
    env.apply_env(&mut guard);

    let mut server = env.start_server(300_000);
    let port = server.info().port;
    let token = server.csrf_token().to_string();

    for (method, path, body) in [
        ("GET", "/api/config/sync", ""),
        ("PUT", "/api/config/sync", "{}"),
        ("DELETE", "/api/config/sync", "{}"),
        ("POST", "/api/config/sync/test", "{}"),
    ] {
        let missing = send_http(
            port,
            method,
            path,
            &[("Content-Type", "application/json")],
            body,
        );
        assert_eq!(missing.status, 403);
        assert_eq!(
            serde_json::from_str::<Value>(&missing.body).unwrap()["error"],
            "invalid or missing CSRF token"
        );

        let cross = send_http(
            port,
            method,
            path,
            &[
                ("X-Agy-Pulse-Token", &token),
                ("Sec-Fetch-Site", "cross-site"),
                ("Content-Type", "application/json"),
            ],
            body,
        );
        assert_eq!(cross.status, 403);
        assert_eq!(
            serde_json::from_str::<Value>(&cross.body).unwrap()["error"],
            "cross-origin request rejected"
        );

        let bad_origin = send_http(
            port,
            method,
            path,
            &[
                ("X-Agy-Pulse-Token", &token),
                ("Origin", "https://evil.example"),
                ("Content-Type", "application/json"),
            ],
            body,
        );
        assert_eq!(bad_origin.status, 403);
    }

    for (method, path) in [
        ("PUT", "/api/config/sync"),
        ("DELETE", "/api/config/sync"),
        ("POST", "/api/config/sync/test"),
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

    for (method, path) in [
        ("POST", "/api/config/sync"),
        ("GET", "/api/config/sync/test"),
        ("PUT", "/api/config/sync/test"),
        ("DELETE", "/api/config/sync/test"),
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

    server.shutdown();
}

#[test]
fn t15_sync_config_all_remotes_secret_lifecycle_and_zero_plaintext_echo() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let env = IsolatedEnv::new();
    let mut guard = EnvGuard::new();
    env.apply_env(&mut guard);

    let mut server = env.start_server(300_000);
    let port = server.info().port;
    let token = server.csrf_token().to_string();

    // 1. Initial GET when absent
    let g0 = send_http(
        port,
        "GET",
        "/api/config/sync",
        &[("X-Agy-Pulse-Token", &token)],
        "",
    );
    assert_eq!(g0.status, 200);
    let j0: Value = serde_json::from_str(&g0.body).unwrap();
    assert_eq!(j0["exists"], false);
    assert_eq!(j0["version"], "absent");
    assert!(j0["parseError"].is_null());
    let local_device_id = j0["localDeviceId"].as_str().unwrap().to_string();
    assert!(!local_device_id.is_empty());
    assert_eq!(j0["config"]["enabled"], false);
    assert_eq!(j0["config"]["deviceId"], local_device_id);
    assert_eq!(j0["config"]["remote"]["type"], "local-dir");
    assert_eq!(j0["config"]["remote"]["path"], "");

    // 2. Save local-dir (creates file with 0600 and fills deviceId)
    let local_remote_dir = env._temp.path().join("sync-remote-local");
    let put_local = serde_json::to_string(&json!({
        "expectedVersion": "absent",
        "config": {
            "enabled": true,
            "deviceName": "workstation-1",
            "defaultAccount": "Alice@Example.COM",
            "pushIntervalSec": 120,
            "prefix": "/team/pulse/",
            "remote": {
                "type": "local-dir",
                "path": local_remote_dir.to_str().unwrap(),
            }
        }
    }))
    .unwrap();
    let p1 = send_http(
        port,
        "PUT",
        "/api/config/sync",
        &[
            ("X-Agy-Pulse-Token", &token),
            ("Content-Type", "application/json"),
        ],
        &put_local,
    );
    assert_eq!(p1.status, 200, "{}", p1.body);
    let jp1: Value = serde_json::from_str(&p1.body).unwrap();
    assert_eq!(jp1["exists"], true);
    assert_eq!(jp1["remoteChanged"], true);
    assert_eq!(jp1["config"]["deviceId"], local_device_id);
    assert_eq!(jp1["config"]["deviceName"], "workstation-1");
    assert_eq!(jp1["config"]["defaultAccount"], "alice@example.com");
    assert_eq!(jp1["config"]["pushIntervalSec"], 120);
    assert_eq!(jp1["config"]["prefix"], "team/pulse");
    assert_eq!(jp1["config"]["remote"]["type"], "local-dir");
    assert!(env.sync_cfg_path.exists());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&env.sync_cfg_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    let mut cur_ver = jp1["version"].as_str().unwrap().to_string();

    // 3. Save WebDAV with password + token (set) + userinfo in URL -> zero plaintext echo
    let dav_pass = "dav-secret-pass-991";
    let dav_tok = "dav-secret-tok-882";
    let url_secret = "url-embedded-pass-773";
    let put_dav = serde_json::to_string(&json!({
        "expectedVersion": cur_ver,
        "config": {
            "enabled": true,
            "deviceName": "workstation-1",
            "defaultAccount": "alice@example.com",
            "pushIntervalSec": 300,
            "prefix": "agy-pulse-sync",
            "remote": {
                "type": "webdav",
                "url": format!("https://alice:{url_secret}@dav.example.com/pulse/"),
                "username": "alice",
                "allowInsecureHttp": false,
                "secrets": {
                    "password": { "mode": "set", "value": dav_pass },
                    "token": { "mode": "set", "value": dav_tok }
                }
            }
        }
    }))
    .unwrap();
    let p_dav = send_http(
        port,
        "PUT",
        "/api/config/sync",
        &[
            ("X-Agy-Pulse-Token", &token),
            ("Content-Type", "application/json"),
        ],
        &put_dav,
    );
    assert_eq!(p_dav.status, 200, "{}", p_dav.body);
    assert!(!p_dav.body.contains(dav_pass));
    assert!(!p_dav.body.contains(dav_tok));
    assert!(!p_dav.body.contains(url_secret));
    let jdav: Value = serde_json::from_str(&p_dav.body).unwrap();
    assert_eq!(jdav["remoteChanged"], true);
    assert_eq!(jdav["config"]["remote"]["type"], "webdav");
    assert_eq!(jdav["config"]["remote"]["url"], "https://dav.example.com/pulse");
    assert_eq!(jdav["config"]["remote"]["secrets"]["password"]["configured"], true);
    assert_eq!(jdav["config"]["remote"]["secrets"]["token"]["configured"], true);
    cur_ver = jdav["version"].as_str().unwrap().to_string();

    // Verify GET also never echoes plaintext
    let gdav = send_http(
        port,
        "GET",
        "/api/config/sync",
        &[("X-Agy-Pulse-Token", &token)],
        "",
    );
    assert_eq!(gdav.status, 200);
    assert!(!gdav.body.contains(dav_pass));
    assert!(!gdav.body.contains(dav_tok));
    assert!(!gdav.body.contains(url_secret));

    // Verify disk JSON keeps snake_case keys inside `remote`
    let disk_dav: Value =
        serde_json::from_slice(&fs::read(&env.sync_cfg_path).unwrap()).unwrap();
    assert!(disk_dav["remote"].get("allow_insecure_http").is_some());
    assert_eq!(disk_dav["remote"]["password"], dav_pass);
    assert_eq!(disk_dav["remote"]["token"], dav_tok);

    // 4. Secret lifecycle: keep password & token while updating prefix -> remoteChanged false when same url/prefix
    let put_dav_keep = serde_json::to_string(&json!({
        "expectedVersion": cur_ver,
        "config": {
            "enabled": true,
            "deviceName": "workstation-renamed",
            "defaultAccount": "alice@example.com",
            "pushIntervalSec": 300,
            "prefix": "agy-pulse-sync",
            "remote": {
                "type": "webdav",
                "url": "https://dav.example.com/pulse",
                "username": "alice",
                "allowInsecureHttp": false,
                "secrets": {
                    "password": { "mode": "keep" },
                    "token": { "mode": "keep" }
                }
            }
        }
    }))
    .unwrap();
    let p_keep = send_http(
        port,
        "PUT",
        "/api/config/sync",
        &[
            ("X-Agy-Pulse-Token", &token),
            ("Content-Type", "application/json"),
        ],
        &put_dav_keep,
    );
    assert_eq!(p_keep.status, 200, "{}", p_keep.body);
    let jkeep: Value = serde_json::from_str(&p_keep.body).unwrap();
    assert_eq!(jkeep["remoteChanged"], false);
    assert_eq!(jkeep["config"]["remote"]["secrets"]["password"]["configured"], true);
    assert_eq!(jkeep["config"]["remote"]["secrets"]["token"]["configured"], true);
    let disk_keep: Value =
        serde_json::from_slice(&fs::read(&env.sync_cfg_path).unwrap()).unwrap();
    assert_eq!(disk_keep["remote"]["password"], dav_pass);
    assert_eq!(disk_keep["remote"]["token"], dav_tok);
    cur_ver = jkeep["version"].as_str().unwrap().to_string();

    // Clear password, keep token
    let put_dav_clear = serde_json::to_string(&json!({
        "expectedVersion": cur_ver,
        "config": {
            "enabled": true,
            "deviceName": "workstation-renamed",
            "defaultAccount": "alice@example.com",
            "pushIntervalSec": 300,
            "prefix": "agy-pulse-sync",
            "remote": {
                "type": "webdav",
                "url": "https://dav.example.com/pulse",
                "username": "alice",
                "allowInsecureHttp": false,
                "secrets": {
                    "password": { "mode": "clear" },
                    "token": { "mode": "keep" }
                }
            }
        }
    }))
    .unwrap();
    let p_clear = send_http(
        port,
        "PUT",
        "/api/config/sync",
        &[
            ("X-Agy-Pulse-Token", &token),
            ("Content-Type", "application/json"),
        ],
        &put_dav_clear,
    );
    assert_eq!(p_clear.status, 200, "{}", p_clear.body);
    let jclear: Value = serde_json::from_str(&p_clear.body).unwrap();
    assert_eq!(jclear["config"]["remote"]["secrets"]["password"]["configured"], false);
    assert_eq!(jclear["config"]["remote"]["secrets"]["token"]["configured"], true);
    cur_ver = jclear["version"].as_str().unwrap().to_string();

    // 5. Cross-type switch webdav -> cf-worker with mode=keep must NOT inherit webdav token!
    let switch_enabled = serde_json::to_string(&json!({
        "expectedVersion": cur_ver,
        "config": {
            "enabled": true,
            "pushIntervalSec": 300,
            "prefix": "agy-pulse-sync",
            "remote": {
                "type": "cf-worker",
                "url": "https://worker.example.com",
                "allowInsecureHttp": false,
                "secrets": {
                    "token": { "mode": "keep" }
                }
            }
        }
    }))
    .unwrap();
    let p_switch_fail = send_http(
        port,
        "PUT",
        "/api/config/sync",
        &[
            ("X-Agy-Pulse-Token", &token),
            ("Content-Type", "application/json"),
        ],
        &switch_enabled,
    );
    assert_eq!(p_switch_fail.status, 400);
    assert_eq!(
        serde_json::from_str::<Value>(&p_switch_fail.body).unwrap()["field"],
        "remote.secrets.token"
    );

    // Disabled save with mode=keep succeeds and has configured=false (no cross-type leakage on disk)
    let switch_disabled = serde_json::to_string(&json!({
        "expectedVersion": cur_ver,
        "config": {
            "enabled": false,
            "pushIntervalSec": 300,
            "prefix": "agy-pulse-sync",
            "remote": {
                "type": "cf-worker",
                "url": "https://worker.example.com",
                "allowInsecureHttp": false,
                "secrets": {
                    "token": { "mode": "keep" }
                }
            }
        }
    }))
    .unwrap();
    let p_switch_ok = send_http(
        port,
        "PUT",
        "/api/config/sync",
        &[
            ("X-Agy-Pulse-Token", &token),
            ("Content-Type", "application/json"),
        ],
        &switch_disabled,
    );
    assert_eq!(p_switch_ok.status, 200, "{}", p_switch_ok.body);
    let jsw: Value = serde_json::from_str(&p_switch_ok.body).unwrap();
    assert_eq!(jsw["config"]["remote"]["type"], "cf-worker");
    assert_eq!(jsw["config"]["remote"]["secrets"]["token"]["configured"], false);
    assert!(!fs::read_to_string(&env.sync_cfg_path).unwrap().contains(dav_tok));
    cur_ver = jsw["version"].as_str().unwrap().to_string();

    // 6. Save S3 with snake_case persistence & zero plaintext echo
    let s3_ak = "AKIA_TEST_ACCESS_441";
    let s3_sk = "SECRET_KEY_TEST_442";
    let put_s3 = serde_json::to_string(&json!({
        "expectedVersion": cur_ver,
        "config": {
            "enabled": true,
            "pushIntervalSec": 600,
            "prefix": "agy-pulse-sync",
            "remote": {
                "type": "s3",
                "endpoint": "https://s3.us-east-1.amazonaws.com",
                "region": "us-east-1",
                "bucket": "my-pulse-bucket",
                "pathStyle": false,
                "allowInsecureHttp": false,
                "secrets": {
                    "accessKey": { "mode": "set", "value": s3_ak },
                    "secretKey": { "mode": "set", "value": s3_sk }
                }
            }
        }
    }))
    .unwrap();
    let p_s3 = send_http(
        port,
        "PUT",
        "/api/config/sync",
        &[
            ("X-Agy-Pulse-Token", &token),
            ("Content-Type", "application/json"),
        ],
        &put_s3,
    );
    assert_eq!(p_s3.status, 200, "{}", p_s3.body);
    assert!(!p_s3.body.contains(s3_ak));
    assert!(!p_s3.body.contains(s3_sk));
    let js3: Value = serde_json::from_str(&p_s3.body).unwrap();
    assert_eq!(js3["config"]["remote"]["type"], "s3");
    assert_eq!(js3["config"]["remote"]["pathStyle"], false);
    assert_eq!(js3["config"]["remote"]["secrets"]["accessKey"]["configured"], true);
    assert_eq!(js3["config"]["remote"]["secrets"]["secretKey"]["configured"], true);
    let disk_s3: Value =
        serde_json::from_slice(&fs::read(&env.sync_cfg_path).unwrap()).unwrap();
    assert_eq!(disk_s3["remote"]["path_style"], false);
    assert_eq!(disk_s3["remote"]["allow_insecure_http"], false);
    assert_eq!(disk_s3["remote"]["access_key"], s3_ak);
    assert_eq!(disk_s3["remote"]["secret_key"], s3_sk);
    cur_ver = js3["version"].as_str().unwrap().to_string();

    // 7. Save cf-worker with explicit token
    let cf_tok = "cf-secret-token-555";
    let put_cf = serde_json::to_string(&json!({
        "expectedVersion": cur_ver,
        "config": {
            "enabled": true,
            "pushIntervalSec": 300,
            "prefix": "agy-pulse-sync",
            "remote": {
                "type": "cf-worker",
                "url": "https://sync.example.workers.dev",
                "allowInsecureHttp": false,
                "secrets": {
                    "token": { "mode": "set", "value": cf_tok }
                }
            }
        }
    }))
    .unwrap();
    let p_cf = send_http(
        port,
        "PUT",
        "/api/config/sync",
        &[
            ("X-Agy-Pulse-Token", &token),
            ("Content-Type", "application/json"),
        ],
        &put_cf,
    );
    assert_eq!(p_cf.status, 200, "{}", p_cf.body);
    assert!(!p_cf.body.contains(cf_tok));
    let jcf: Value = serde_json::from_str(&p_cf.body).unwrap();
    assert_eq!(jcf["config"]["remote"]["type"], "cf-worker");
    assert_eq!(jcf["config"]["remote"]["secrets"]["token"]["configured"], true);

    // 8. Field validation checks -> 400 with exact field
    let cur_ver = jcf["version"].as_str().unwrap().to_string();
    for (bad_cfg, expected_field) in [
        (
            json!({
                "enabled": false,
                "defaultAccount": "unassigned",
                "pushIntervalSec": 300,
                "prefix": "agy-pulse-sync",
                "remote": { "type": "local-dir", "path": "" }
            }),
            "defaultAccount",
        ),
        (
            json!({
                "enabled": false,
                "defaultAccount": "not-an-email",
                "pushIntervalSec": 300,
                "prefix": "agy-pulse-sync",
                "remote": { "type": "local-dir", "path": "" }
            }),
            "defaultAccount",
        ),
        (
            json!({
                "enabled": false,
                "pushIntervalSec": 0,
                "prefix": "agy-pulse-sync",
                "remote": { "type": "local-dir", "path": "" }
            }),
            "pushIntervalSec",
        ),
        (
            json!({
                "enabled": false,
                "pushIntervalSec": 300,
                "prefix": "foo/../bar",
                "remote": { "type": "local-dir", "path": "" }
            }),
            "prefix",
        ),
        (
            json!({
                "enabled": false,
                "pushIntervalSec": 300,
                "prefix": "agy-pulse-sync",
                "remote": {
                    "type": "cf-worker",
                    "url": "https://sync.example.workers.dev",
                    "secrets": { "token": { "mode": "set", "value": "   " } }
                }
            }),
            "remote.secrets.token",
        ),
    ] {
        let body = serde_json::to_string(&json!({
            "expectedVersion": cur_ver,
            "config": bad_cfg
        }))
        .unwrap();
        let bad_res = send_http(
            port,
            "PUT",
            "/api/config/sync",
            &[
                ("X-Agy-Pulse-Token", &token),
                ("Content-Type", "application/json"),
            ],
            &body,
        );
        assert_eq!(bad_res.status, 400, "{}", bad_res.body);
        assert_eq!(
            serde_json::from_str::<Value>(&bad_res.body).unwrap()["field"],
            expected_field
        );
    }

    server.shutdown();
}

#[test]
fn t15_sync_config_env_overrides_never_persisted_to_disk() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let env = IsolatedEnv::new();
    let mut guard = EnvGuard::new();
    env.apply_env(&mut guard);

    let env_token = "env-only-cf-token-xyz-999";
    guard.set("AGY_PULSE_SYNC_TOKEN", env_token);

    let mut server = env.start_server(300_000);
    let port = server.info().port;
    let token = server.csrf_token().to_string();

    // Seed cf-worker without token via PUT (enabled=true allowed because env provides token)
    let put_body = serde_json::to_string(&json!({
        "expectedVersion": "absent",
        "config": {
            "enabled": true,
            "pushIntervalSec": 300,
            "prefix": "agy-pulse-sync",
            "remote": {
                "type": "cf-worker",
                "url": "https://worker.example.com",
                "allowInsecureHttp": false,
                "secrets": {
                    "token": { "mode": "keep" }
                }
            }
        }
    }))
    .unwrap();
    let p_res = send_http(
        port,
        "PUT",
        "/api/config/sync",
        &[
            ("X-Agy-Pulse-Token", &token),
            ("Content-Type", "application/json"),
        ],
        &put_body,
    );
    assert_eq!(p_res.status, 200, "{}", p_res.body);
    assert!(!p_res.body.contains(env_token));
    let jp: Value = serde_json::from_str(&p_res.body).unwrap();
    assert_eq!(jp["envOverrides"]["token"], "AGY_PULSE_SYNC_TOKEN");
    assert_eq!(jp["config"]["remote"]["secrets"]["token"]["configured"], false);
    assert_eq!(
        jp["config"]["remote"]["secrets"]["token"]["overriddenByEnv"],
        "AGY_PULSE_SYNC_TOKEN"
    );

    // Disk file must NEVER contain env token
    let disk_raw = fs::read_to_string(&env.sync_cfg_path).unwrap();
    assert!(!disk_raw.contains(env_token));

    // GET also shows overriddenByEnv and never echoes secret
    let g_res = send_http(
        port,
        "GET",
        "/api/config/sync",
        &[("X-Agy-Pulse-Token", &token)],
        "",
    );
    assert_eq!(g_res.status, 200);
    assert!(!g_res.body.contains(env_token));
    let jg: Value = serde_json::from_str(&g_res.body).unwrap();
    assert_eq!(jg["envOverrides"]["token"], "AGY_PULSE_SYNC_TOKEN");
    assert_eq!(jg["config"]["remote"]["secrets"]["token"]["configured"], false);
    assert_eq!(
        jg["config"]["remote"]["secrets"]["token"]["overriddenByEnv"],
        "AGY_PULSE_SYNC_TOKEN"
    );

    server.shutdown();
}

#[test]
fn t15_sync_config_test_endpoint_probes_and_redacts_without_disk_writes() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let env = IsolatedEnv::new();
    let mut guard = EnvGuard::new();
    env.apply_env(&mut guard);

    let mut server = env.start_server(300_000);
    let port = server.info().port;
    let token = server.csrf_token().to_string();

    // 1. local-dir probe succeeds without creating sync_cfg_path
    let probe_dir = env._temp.path().join("probe-local-dir");
    let local_probe_body = serde_json::to_string(&json!({
        "config": {
            "enabled": false,
            "prefix": "agy-pulse-sync",
            "remote": {
                "type": "local-dir",
                "path": probe_dir.to_str().unwrap()
            }
        }
    }))
    .unwrap();
    let res_local = send_http(
        port,
        "POST",
        "/api/config/sync/test",
        &[
            ("X-Agy-Pulse-Token", &token),
            ("Content-Type", "application/json"),
        ],
        &local_probe_body,
    );
    assert_eq!(res_local.status, 200);
    let jl: Value = serde_json::from_str(&res_local.body).unwrap();
    assert_eq!(jl["ok"], true);
    assert!(jl["target"].as_str().unwrap().contains("local-dir:"));
    assert!(jl["message"].as_str().unwrap().contains("local-dir"));
    assert!(jl["latencyMs"].is_u64());
    assert!(!env.sync_cfg_path.exists());

    // 2. Mock CF Worker server: success then 401 failure echoing secret
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, String, Option<String>)>::new()));
    let fail_mode = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let secret_tok = "mock-cf-secret-token-4321";
    let url_pass = "mock-url-pass-8765";

    let seen_srv = std::sync::Arc::clone(&seen);
    let fail_srv = std::sync::Arc::clone(&fail_mode);
    let mock_cf = MockSyncHttpServer::start(move |method, target, headers, _body| {
        let auth = headers
            .iter()
            .find(|(k, _)| k == "authorization")
            .map(|(_, v)| v.clone());
        seen_srv
            .lock()
            .unwrap()
            .push((method.to_string(), target.to_string(), auth));
        if fail_srv.load(std::sync::atomic::Ordering::Relaxed) {
            (
                401,
                "Unauthorized",
                format!("bad token: {secret_tok} / {url_pass}").into_bytes(),
            )
        } else {
            (200, "OK", br#"{"ok":true,"storage":"r2"}"#.to_vec())
        }
    });

    let cf_probe_body = serde_json::to_string(&json!({
        "config": {
            "enabled": false,
            "prefix": "agy-pulse-sync",
            "remote": {
                "type": "cf-worker",
                "url": format!("http://127.0.0.1:{}", mock_cf.port),
                "allowInsecureHttp": false,
                "secrets": {
                    "token": { "mode": "set", "value": secret_tok }
                }
            }
        }
    }))
    .unwrap();
    let res_cf_ok = send_http(
        port,
        "POST",
        "/api/config/sync/test",
        &[
            ("X-Agy-Pulse-Token", &token),
            ("Content-Type", "application/json"),
        ],
        &cf_probe_body,
    );
    assert_eq!(res_cf_ok.status, 200);
    assert!(!res_cf_ok.body.contains(secret_tok));
    let jcf_ok: Value = serde_json::from_str(&res_cf_ok.body).unwrap();
    assert_eq!(jcf_ok["ok"], true);
    assert!(jcf_ok["message"].as_str().unwrap().contains("r2"));
    {
        let log = seen.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].0, "GET");
        assert_eq!(log[0].1, "/v1/health");
        assert_eq!(log[0].2.as_deref(), Some("Bearer mock-cf-secret-token-4321"));
    }

    fail_mode.store(true, std::sync::atomic::Ordering::Relaxed);
    let cf_fail_body = serde_json::to_string(&json!({
        "config": {
            "enabled": false,
            "prefix": "agy-pulse-sync",
            "remote": {
                "type": "cf-worker",
                "url": format!("http://user:{url_pass}@127.0.0.1:{}", mock_cf.port),
                "allowInsecureHttp": false,
                "secrets": {
                    "token": { "mode": "set", "value": secret_tok }
                }
            }
        }
    }))
    .unwrap();
    let res_cf_fail = send_http(
        port,
        "POST",
        "/api/config/sync/test",
        &[
            ("X-Agy-Pulse-Token", &token),
            ("Content-Type", "application/json"),
        ],
        &cf_fail_body,
    );
    assert_eq!(res_cf_fail.status, 200);
    assert!(!res_cf_fail.body.contains(secret_tok));
    assert!(!res_cf_fail.body.contains(url_pass));
    let jcf_fail: Value = serde_json::from_str(&res_cf_fail.body).unwrap();
    assert_eq!(jcf_fail["ok"], false);
    assert!(!jcf_fail["error"].as_str().unwrap().is_empty());

    // 3. Mock WebDAV probe success
    let dav_seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, Option<String>)>::new()));
    let dav_seen_srv = std::sync::Arc::clone(&dav_seen);
    let mock_dav = MockSyncHttpServer::start(move |method, _target, headers, _body| {
        let auth = headers
            .iter()
            .find(|(k, _)| k == "authorization")
            .map(|(_, v)| v.clone());
        dav_seen_srv
            .lock()
            .unwrap()
            .push((method.to_string(), auth));
        (207, "Multi-Status", b"<D:multistatus/>".to_vec())
    });

    let dav_pass = "dav-probe-secret";
    let dav_probe_body = serde_json::to_string(&json!({
        "config": {
            "enabled": false,
            "prefix": "agy-pulse-sync",
            "remote": {
                "type": "webdav",
                "url": format!("http://127.0.0.1:{}/dav", mock_dav.port),
                "username": "alice",
                "allowInsecureHttp": false,
                "secrets": {
                    "password": { "mode": "set", "value": dav_pass },
                    "token": { "mode": "clear" }
                }
            }
        }
    }))
    .unwrap();
    let res_dav = send_http(
        port,
        "POST",
        "/api/config/sync/test",
        &[
            ("X-Agy-Pulse-Token", &token),
            ("Content-Type", "application/json"),
        ],
        &dav_probe_body,
    );
    assert_eq!(res_dav.status, 200);
    assert!(!res_dav.body.contains(dav_pass));
    let jdav: Value = serde_json::from_str(&res_dav.body).unwrap();
    assert_eq!(jdav["ok"], true);
    {
        let log = dav_seen.lock().unwrap();
        assert_eq!(log[0].0, "PROPFIND");
        assert!(log[0].1.as_deref().unwrap().starts_with("Basic "));
    }

    // 4. Invalid prefix rejected with 400
    let bad_prefix_res = send_http(
        port,
        "POST",
        "/api/config/sync/test",
        &[
            ("X-Agy-Pulse-Token", &token),
            ("Content-Type", "application/json"),
        ],
        r#"{"config":{"prefix":"../escape","remote":{"type":"local-dir","path":"/tmp"}}}"#,
    );
    assert_eq!(bad_prefix_res.status, 400);
    assert_eq!(
        serde_json::from_str::<Value>(&bad_prefix_res.body).unwrap()["field"],
        "prefix"
    );

    // Still zero disk writes!
    assert!(!env.sync_cfg_path.exists());

    server.shutdown();
}

#[test]
fn t15_sync_config_conflict_corrupt_delete_and_identity_sync_integration() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let env = IsolatedEnv::new();
    let mut guard = EnvGuard::new();
    env.apply_env(&mut guard);

    let roots = vec![agy_pulse::ledger::SourceRoot {
        root: env.cli_data_dir.clone(),
        source: agy_pulse::ledger::LedgerSource::Cli,
    }];
    let mut server = env.start_server_with_roots(300_000, Some(roots));
    let port = server.info().port;
    let token = server.csrf_token().to_string();

    // 1. Corrupt JSON tolerance + recovery via PUT
    let corrupt_bytes = b"{not-valid-sync-json";
    fs::write(&env.sync_cfg_path, corrupt_bytes).unwrap();
    let corrupt_ver = sha256_hex(corrupt_bytes);

    let g_corrupt = send_http(
        port,
        "GET",
        "/api/config/sync",
        &[("X-Agy-Pulse-Token", &token)],
        "",
    );
    assert_eq!(g_corrupt.status, 200);
    let jc: Value = serde_json::from_str(&g_corrupt.body).unwrap();
    assert_eq!(jc["exists"], true);
    assert_eq!(jc["version"], corrupt_ver);
    assert!(jc["parseError"].as_str().unwrap().contains("invalid JSON"));

    // 2. Stale expectedVersion -> 409 Conflict on PUT
    let stale_put = send_http(
        port,
        "PUT",
        "/api/config/sync",
        &[
            ("X-Agy-Pulse-Token", &token),
            ("Content-Type", "application/json"),
        ],
        r#"{"expectedVersion":"stale-ver","config":{"enabled":false,"remote":{"type":"local-dir","path":""}}}"#,
    );
    assert_eq!(stale_put.status, 409);
    let jconflict: Value = serde_json::from_str(&stale_put.body).unwrap();
    assert_eq!(jconflict["error"], "version_conflict");
    assert_eq!(jconflict["currentVersion"], corrupt_ver);

    // 3. Recover via PUT and set new defaultAccount + deviceName
    let remote_dir = env._temp.path().join("sync-remote");
    let put_body = serde_json::to_string(&json!({
        "expectedVersion": corrupt_ver,
        "config": {
            "enabled": false,
            "deviceName": "web-configured-rig",
            "defaultAccount": "webcfg@example.com",
            "pushIntervalSec": 300,
            "prefix": "agy-pulse-sync",
            "remote": {
                "type": "local-dir",
                "path": remote_dir.to_str().unwrap()
            }
        }
    }))
    .unwrap();
    let p_ok = send_http(
        port,
        "PUT",
        "/api/config/sync",
        &[
            ("X-Agy-Pulse-Token", &token),
            ("Content-Type", "application/json"),
        ],
        &put_body,
    );
    assert_eq!(p_ok.status, 200, "{}", p_ok.body);
    let saved_ver = serde_json::from_str::<Value>(&p_ok.body).unwrap()["version"]
        .as_str()
        .unwrap()
        .to_string();

    // 4. Write new conversation DB, trigger POST /api/sync, verify /api/overview uses new identity
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    write_test_conv_db(
        &env.cli_data_dir
            .join("conversations")
            .join("conv-web-sync-cfg-1.db"),
        "conv-web-sync-cfg-1",
        now_ms - 5_000,
    );

    let sync_res = send_http(
        port,
        "POST",
        "/api/sync",
        &[
            ("Origin", &format!("http://127.0.0.1:{port}")),
            ("X-Agy-Pulse-Token", &token),
        ],
        "",
    );
    assert_eq!(sync_res.status, 200, "{}", sync_res.body);

    let ov_res = send_http(port, "GET", "/api/overview?range=7d", &[], "");
    assert_eq!(ov_res.status, 200);
    let ov: Value = serde_json::from_str(&ov_res.body).unwrap();
    let accounts = ov["meta"]["availableAccounts"].as_array().unwrap();
    assert!(
        accounts
            .iter()
            .any(|a| a["accountId"] == "webcfg@example.com"),
        "expected webcfg@example.com in {accounts:?}"
    );
    let devices = ov["meta"]["availableDevices"].as_array().unwrap();
    assert!(
        devices
            .iter()
            .any(|d| d["deviceName"] == "web-configured-rig"),
        "expected web-configured-rig in {devices:?}"
    );

    // 5. Stale DELETE -> 409 Conflict
    let del_conflict = send_http(
        port,
        "DELETE",
        "/api/config/sync",
        &[
            ("X-Agy-Pulse-Token", &token),
            ("Content-Type", "application/json"),
        ],
        r#"{"expectedVersion":"wrong-version"}"#,
    );
    assert_eq!(del_conflict.status, 409);
    assert_eq!(
        serde_json::from_str::<Value>(&del_conflict.body).unwrap()["currentVersion"],
        saved_ver
    );
    assert!(env.sync_cfg_path.exists());

    // 6. Valid DELETE removes sync config only, preserving SQLite ledger data
    let del_ok = send_http(
        port,
        "DELETE",
        "/api/config/sync",
        &[
            ("X-Agy-Pulse-Token", &token),
            ("Content-Type", "application/json"),
        ],
        &serde_json::to_string(&json!({ "expectedVersion": saved_ver })).unwrap(),
    );
    assert_eq!(del_ok.status, 200);
    let jdel: Value = serde_json::from_str(&del_ok.body).unwrap();
    assert_eq!(jdel["deleted"], true);
    assert_eq!(jdel["version"], "absent");
    assert!(!env.sync_cfg_path.exists());
    assert!(env.ledger_path().exists());

    // Ledger data remains queryable after deleting sync config
    let ov_after = send_http(port, "GET", "/api/overview?range=7d", &[], "");
    assert_eq!(ov_after.status, 200);
    let ov_after_json: Value = serde_json::from_str(&ov_after.body).unwrap();
    assert_eq!(ov_after_json["summary"]["totalSteps"], 1);

    server.shutdown();
}

#[test]
fn t15_dashboard_html_settings_ui_anchors_placeholders_and_security_headers() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let env = IsolatedEnv::new();
    let mut guard = EnvGuard::new();
    env.apply_env(&mut guard);

    let mut server = env.start_server(300_000);
    let port = server.info().port;

    let res = send_http(port, "GET", "/", &[], "");
    assert_eq!(res.status, 200);

    let head_lower = res.head.to_ascii_lowercase();
    assert!(
        head_lower.contains("content-security-policy: default-src 'self'"),
        "expected CSP header in:\n{}",
        res.head
    );
    assert!(
        head_lower.contains("x-frame-options: deny"),
        "expected X-Frame-Options: DENY in:\n{}",
        res.head
    );
    assert!(
        head_lower.contains("cache-control: no-store"),
        "expected Cache-Control: no-store in:\n{}",
        res.head
    );
    assert!(
        head_lower.contains("x-content-type-options: nosniff"),
        "expected X-Content-Type-Options: nosniff in:\n{}",
        res.head
    );

    assert!(
        !res.body.contains("__AGY_PULSE_"),
        "expected no unsubstituted __AGY_PULSE_* placeholders in rendered HTML"
    );

    let expected_token_decl = format!("var CSRF_TOKEN = '{}';", server.csrf_token());
    assert_eq!(server.csrf_token().len(), 64);
    assert!(
        server.csrf_token().chars().all(|c| c.is_ascii_hexdigit()),
        "expected 64-char hex CSRF token"
    );
    assert!(
        res.body.contains(&expected_token_decl),
        "expected injected CSRF token declaration in HTML"
    );

    for anchor in [
        "id=\"dashboard-view\"",
        "id=\"settings-page\"",
        "id=\"btn-settings-back\"",
        "id=\"settings-tabs\"",
        "id=\"btn-settings\"",
        "id=\"tab-btn-pulse\"",
        "id=\"tab-btn-sync\"",
        "id=\"tab-pane-pulse\"",
        "id=\"tab-pane-sync\"",
        "X-Agy-Pulse-Token",
        "/api/config/pulse",
        "/api/config/pulse/preview",
        "/api/config/sync",
        "/api/config/sync/test",
        "Statusline Segments (12)",
        "状态栏显示项 (12 项)",
        "groupBottomMonitor",
        "groupHeaderLine",
        "groupWorkspaceCost",
        "Session Model Cache Line",
        "模型累计缓存行",
    ] {
        assert!(
            res.body.contains(anchor),
            "expected HTML anchor '{anchor}' in dashboard response"
        );
    }

    for retired_ui in [
        "pulse-icon-token",
        "pulse-icon-ctx",
        "id=\"settings-drawer\"",
        "id=\"settings-overlay\"",
    ] {
        assert!(
            !res.body.contains(retired_ui),
            "retired UI anchor '{retired_ui}' must not appear in dashboard HTML"
        );
    }

    for settings_route in ["/settings", "/settings/", "/settings/pulse", "/settings/sync"] {
        let s_res = send_http(port, "GET", settings_route, &[], "");
        assert_eq!(
            s_res.status, 200,
            "expected 200 OK for standalone settings route {settings_route}"
        );
        let s_head_lower = s_res.head.to_ascii_lowercase();
        assert!(
            s_head_lower.contains("content-security-policy: default-src 'self'"),
            "expected CSP header on {settings_route}"
        );
        assert!(
            s_head_lower.contains("x-frame-options: deny"),
            "expected X-Frame-Options: DENY on {settings_route}"
        );
        assert!(
            s_head_lower.contains("cache-control: no-store"),
            "expected Cache-Control: no-store on {settings_route}"
        );
        assert!(
            s_res.body.contains(&expected_token_decl),
            "expected injected CSRF token declaration on {settings_route}"
        );
        assert!(
            s_res.body.contains("id=\"settings-page\""),
            "expected settings-page anchor on {settings_route}"
        );
    }

    server.shutdown();
}
