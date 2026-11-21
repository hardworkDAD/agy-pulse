#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

use agy_pulse::dashboard::{
    DASHBOARD_APP_ID, DASHBOARD_SHORTCUT_MARKER, DASHBOARD_STATE_FILENAME,
    build_app_window_launch_spec,
};
use agy_pulse::platform::DesktopOs;

fn agypl_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agypl"))
}

struct IsolatedEnv {
    _temp: TempDir,
    home_dir: PathBuf,
    xdg_data_home: PathBuf,
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
        let xdg_data_home = home_dir.join(".local/share");
        let cli_data_dir = root.join("cli_data");
        let ide_data_dir = root.join("ide_data");
        let global_cfg_dir = root.join("global_cfg");
        let xdg_cfg_dir = root.join("xdg_cfg");
        let workspace_dir = root.join("workspace");
        fs::create_dir_all(&home_dir).unwrap();
        fs::create_dir_all(&xdg_data_home).unwrap();
        fs::create_dir_all(&cli_data_dir).unwrap();
        fs::create_dir_all(&ide_data_dir).unwrap();
        fs::create_dir_all(&global_cfg_dir).unwrap();
        fs::create_dir_all(&xdg_cfg_dir).unwrap();
        fs::create_dir_all(&workspace_dir).unwrap();
        Self {
            _temp: temp,
            home_dir,
            xdg_data_home,
            cli_data_dir,
            ide_data_dir,
            global_cfg_dir,
            xdg_cfg_dir,
            workspace_dir,
        }
    }

    fn state_file_path(&self) -> PathBuf {
        self.cli_data_dir.join(DASHBOARD_STATE_FILENAME)
    }

    fn base_cmd(&self) -> Command {
        let mut cmd = Command::new(agypl_bin());
        cmd.current_dir(&self.workspace_dir)
            .env("HOME", &self.home_dir)
            .env("USERPROFILE", &self.home_dir)
            .env("XDG_DATA_HOME", &self.xdg_data_home)
            .env("XDG_CONFIG_HOME", &self.xdg_cfg_dir)
            .env("AGY_PULSE_DATA_DIR", &self.cli_data_dir)
            .env("AGY_PULSE_IDE_DATA_DIR", &self.ide_data_dir)
            .env("AGY_PULSE_GLOBAL_CONFIG_DIR", &self.global_cfg_dir)
            .env("AGY_PULSE_NO_OPEN", "1")
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

fn free_loopback_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind ephemeral port");
    listener.local_addr().unwrap().port()
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
) -> Result<HttpResponse, std::io::Error> {
    let mut stream = TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(500),
    )?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;

    let mut req = format!("{method} {target} HTTP/1.1\r\n");
    req.push_str(&format!("Host: 127.0.0.1:{port}\r\n"));
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("Connection: close\r\n\r\n");
    stream.write_all(req.as_bytes())?;
    stream.flush()?;

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "missing HTTP separator")
    })?;
    let status_line = head.lines().next().unwrap_or_default();
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    Ok(HttpResponse {
        status,
        body: body.to_string(),
    })
}

fn open_sse_stream(port: u16, client_id: &str) -> TcpStream {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect SSE");
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let req = format!(
        "GET /api/keepalive?client={client_id} HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
         Origin: http://127.0.0.1:{port}\r\n\
         Accept: text/event-stream\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).unwrap();
    stream.flush().unwrap();

    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut status_line = String::new();
    reader.read_line(&mut status_line).unwrap();
    assert!(
        status_line.contains("200 OK"),
        "expected 200 OK for SSE keepalive, got: {status_line}"
    );

    let mut content_type = String::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some((k, v)) = trimmed.split_once(':')
            && k.eq_ignore_ascii_case("content-type")
        {
            content_type = v.trim().to_string();
        }
    }
    assert!(
        content_type.contains("text/event-stream"),
        "SSE must return text/event-stream, got: {content_type}"
    );

    let mut first_event = String::new();
    reader.read_line(&mut first_event).unwrap();
    assert!(
        first_event.starts_with(':'),
        "expected initial SSE comment frame, got: {first_event}"
    );

    stream
}

fn wait_until(timeout: Duration, mut pred: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if pred() {
            return true;
        }
        thread::sleep(Duration::from_millis(30));
    }
    pred()
}

#[test]
fn t08_background_daemon_single_instance_reuse_status_stop_and_stale_recovery() {
    let env = IsolatedEnv::new();
    let port = free_loopback_port();

    // 1. Seed a stale dead-PID discovery file; `status` should detect dead process & self-heal.
    let stale_payload = serde_json::json!({
        "app": DASHBOARD_APP_ID,
        "pid": 999_999_999u32,
        "port": port,
        "host": "127.0.0.1",
        "url": format!("http://127.0.0.1:{port}"),
        "idleTimeoutMs": 600_000,
        "startedAt": "2025-01-01T00:00:00.000Z"
    });
    fs::write(
        env.state_file_path(),
        serde_json::to_string_pretty(&stale_payload).unwrap(),
    )
    .unwrap();

    let status_stale = env
        .base_cmd()
        .args(["dashboard", "status", "--port", &port.to_string()])
        .output()
        .unwrap();
    assert!(
        status_stale.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&status_stale.stderr)
    );
    let stale_out = String::from_utf8_lossy(&status_stale.stdout);
    assert!(
        stale_out.contains("not running"),
        "stale dead PID must report not running, got: {stale_out}"
    );
    assert!(
        !env.state_file_path().exists(),
        "stale discovery file should be cleaned up automatically"
    );

    // 2. Seed another stale file and run `dashboard open --no-open` to auto-heal & start daemon.
    fs::write(
        env.state_file_path(),
        serde_json::to_string_pretty(&stale_payload).unwrap(),
    )
    .unwrap();

    let open1 = env
        .base_cmd()
        .args([
            "dashboard",
            "open",
            "--port",
            &port.to_string(),
            "--no-open",
            "--no-sync",
        ])
        .output()
        .unwrap();
    assert!(
        open1.status.success(),
        "first `dashboard open` failed: {}",
        String::from_utf8_lossy(&open1.stderr)
    );
    let open1_out = String::from_utf8_lossy(&open1.stdout);
    assert!(
        open1_out.contains("started in background"),
        "expected started in background message, got: {open1_out}"
    );

    let state_raw = fs::read_to_string(env.state_file_path()).expect("discovery state file written");
    let state1: Value = serde_json::from_str(&state_raw).unwrap();
    assert_eq!(state1["app"], DASHBOARD_APP_ID);
    assert_eq!(state1["port"], port);
    let daemon_pid = state1["pid"].as_u64().expect("daemon pid") as u32;
    assert!(daemon_pid > 0 && daemon_pid != 999_999_999);

    let health = send_http(port, "GET", "/api/health", &[]).expect("health check");
    assert_eq!(health.status, 200);
    let health_json: Value = serde_json::from_str(&health.body).unwrap();
    assert_eq!(health_json["pid"], daemon_pid);
    assert_eq!(health_json["port"], port);

    // 3. Second `dashboard open` must reuse the same healthy instance (same PID & port).
    let open2 = env
        .base_cmd()
        .args([
            "dashboard",
            "open",
            "--port",
            &port.to_string(),
            "--no-open",
            "--no-sync",
        ])
        .output()
        .unwrap();
    assert!(open2.status.success());
    let open2_out = String::from_utf8_lossy(&open2.stdout);
    assert!(
        open2_out.contains("already running"),
        "second `open` must reuse existing daemon, got: {open2_out}"
    );
    let state2: Value =
        serde_json::from_str(&fs::read_to_string(env.state_file_path()).unwrap()).unwrap();
    assert_eq!(state2["pid"], daemon_pid, "PID must not change on reuse");

    // 4. `dashboard status` reports running with URL & PID.
    let status_live = env
        .base_cmd()
        .args(["dashboard", "status", "--port", &port.to_string()])
        .output()
        .unwrap();
    assert!(status_live.status.success());
    let status_out = String::from_utf8_lossy(&status_live.stdout);
    assert!(
        status_out.contains("running")
            && status_out.contains(&format!("http://127.0.0.1:{port}"))
            && status_out.contains(&daemon_pid.to_string()),
        "unexpected live status output: {status_out}"
    );

    // 5. `dashboard stop` gracefully stops the daemon and removes discovery state file.
    let stop_res = env
        .base_cmd()
        .args(["dashboard", "stop", "--port", &port.to_string()])
        .output()
        .unwrap();
    assert!(
        stop_res.status.success(),
        "stop failed: {}",
        String::from_utf8_lossy(&stop_res.stderr)
    );
    let stop_out = String::from_utf8_lossy(&stop_res.stdout);
    assert!(
        stop_out.contains("Stopped") && stop_out.contains(&daemon_pid.to_string()),
        "expected Stopped message, got: {stop_out}"
    );
    assert!(
        !env.state_file_path().exists(),
        "discovery file must be cleaned on stop"
    );
    assert!(
        send_http(port, "GET", "/api/health", &[]).is_err(),
        "server must no longer accept connections after stop"
    );
}

#[test]
fn t08_keepalive_survives_background_idle_survives_reload_and_exits_after_close_grace() {
    let env = IsolatedEnv::new();
    let port = free_loopback_port();

    // Start background daemon with short startup idle (1s) and short close grace (350ms).
    let open_out = env
        .base_cmd()
        .env("AGY_PULSE_DASHBOARD_IDLE_SEC", "1")
        .env("AGY_PULSE_DASHBOARD_CLOSE_GRACE_MS", "350")
        .args([
            "dashboard",
            "open",
            "--port",
            &port.to_string(),
            "--no-open",
            "--no-sync",
        ])
        .output()
        .unwrap();
    assert!(
        open_out.status.success(),
        "open failed: {}",
        String::from_utf8_lossy(&open_out.stderr)
    );

    // Verify rendered HTML removes the old `visibilityState !== 'hidden'` limit and wires SSE + sendBeacon.
    let html_res = send_http(port, "GET", "/", &[]).expect("GET /");
    assert_eq!(html_res.status, 200);
    assert!(
        !html_res.body.contains("visibilityState !== 'hidden'"),
        "HTML must not pause heartbeats when tab is hidden/minimized"
    );
    assert!(html_res.body.contains("/api/keepalive"));
    assert!(html_res.body.contains("/api/disconnect"));
    assert!(html_res.body.contains("pagehide"));
    assert!(html_res.body.contains("sendBeacon"));

    // 1. Connect SSE keepalive (simulating minimized / background tab) and hold > 1s startup idle.
    let sse_tab1 = open_sse_stream(port, "tab-1");
    thread::sleep(Duration::from_millis(1450));
    let still_alive = send_http(port, "GET", "/api/health", &[])
        .expect("server must stay alive past idle timeout while SSE is connected");
    assert_eq!(still_alive.status, 200);

    // 1b. Multi-tab: open tab-extra via SSE + tab-ping via POST /api/ping?client=tab-ping,
    // close tab-1 -> server remains alive; close tab-ping & tab-extra later.
    let sse_extra = open_sse_stream(port, "tab-extra");
    let ping_res = send_http(
        port,
        "POST",
        "/api/ping?client=tab-ping",
        &[("Origin", &format!("http://127.0.0.1:{port}"))],
    )
    .expect("ping lease");
    assert_eq!(ping_res.status, 200);
    let ping_json: Value = serde_json::from_str(&ping_res.body).unwrap();
    assert_eq!(ping_json["activeClients"], 3);

    // 2. Origin check on /api/disconnect must block cross-origin beacons (403).
    let bad_origin = send_http(
        port,
        "POST",
        "/api/disconnect?client=tab-1",
        &[("Origin", "https://evil.example")],
    )
    .expect("cross-origin disconnect response");
    assert_eq!(bad_origin.status, 403);

    // Closing tab-1 and tab-extra while tab-ping still holds an active lease must NOT trigger exit.
    let disc1 = send_http(
        port,
        "POST",
        "/api/disconnect?client=tab-1",
        &[("Origin", &format!("http://127.0.0.1:{port}"))],
    )
    .expect("same-origin disconnect");
    assert_eq!(disc1.status, 200);
    drop(sse_tab1);

    let disc_extra = send_http(
        port,
        "POST",
        "/api/disconnect?client=tab-extra",
        &[("Origin", &format!("http://127.0.0.1:{port}"))],
    )
    .expect("disconnect tab-extra");
    assert_eq!(disc_extra.status, 200);
    drop(sse_extra);

    thread::sleep(Duration::from_millis(550));
    let alive_via_ping_lease = send_http(port, "GET", "/api/health", &[])
        .expect("server must stay alive past close grace while ping lease is active");
    assert_eq!(alive_via_ping_lease.status, 200);

    // 3. Simulate F5 page refresh: disconnect tab-ping, then reconnect tab-2 within 350ms grace.
    let disc_ping = send_http(
        port,
        "POST",
        "/api/disconnect?client=tab-ping",
        &[("Origin", &format!("http://127.0.0.1:{port}"))],
    )
    .expect("disconnect tab-ping");
    assert_eq!(disc_ping.status, 200);

    thread::sleep(Duration::from_millis(80));
    let sse_tab2 = open_sse_stream(port, "tab-2");
    thread::sleep(Duration::from_millis(550));
    let alive_after_reload = send_http(port, "GET", "/api/health", &[])
        .expect("server must survive F5 reload within close grace");
    assert_eq!(alive_after_reload.status, 200);

    // 4. Simulate true close of the last page: disconnect tab-2 and drop SSE.
    let disc2 = send_http(
        port,
        "POST",
        "/api/disconnect?client=tab-2",
        &[("Origin", &format!("http://127.0.0.1:{port}"))],
    )
    .expect("disconnect tab-2");
    assert_eq!(disc2.status, 200);
    drop(sse_tab2);

    let exited = wait_until(Duration::from_millis(2500), || {
        !env.state_file_path().exists() && send_http(port, "GET", "/api/health", &[]).is_err()
    });
    assert!(
        exited,
        "background server must auto-exit and clean discovery state after close grace expires"
    );

    // 5. Startup orphan guard: starting background server without any client connecting must auto-exit.
    let port2 = free_loopback_port();
    let child = env
        .base_cmd()
        .env("AGY_PULSE_DASHBOARD_IDLE_SEC", "1")
        .env("AGY_PULSE_DASHBOARD_CLOSE_GRACE_MS", "200")
        .args([
            "dashboard",
            "--serve-background",
            "--no-open",
            "--no-sync",
            "--port",
            &port2.to_string(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut guard = ChildGuard(child);

    let ready = wait_until(Duration::from_secs(3), || {
        send_http(port2, "GET", "/api/health", &[])
            .map(|r| r.status == 200)
            .unwrap_or(false)
    });
    assert!(ready, "background serve child did not become ready");

    let orphan_reclaimed = wait_until(Duration::from_secs(3), || {
        matches!(guard.0.try_wait(), Ok(Some(_)))
            && !env.state_file_path().exists()
    });
    assert!(
        orphan_reclaimed,
        "background serve with zero client connections must auto-exit after startup idle"
    );
}

#[test]
fn t08_app_window_command_construction_and_fingerprint_safe_shortcuts() {
    // 1. Pure cross-platform browser --app window argument construction
    let linux_spec = build_app_window_launch_spec(
        DesktopOs::Linux,
        "google-chrome",
        Path::new("/usr/bin/google-chrome"),
        "http://127.0.0.1:3789",
    );
    assert_eq!(linux_spec.program, PathBuf::from("/usr/bin/google-chrome"));
    assert_eq!(
        linux_spec.args,
        vec![
            "--app=http://127.0.0.1:3789",
            "--class=agy-pulse-dashboard",
            "--new-window",
        ]
    );

    let mac_spec = build_app_window_launch_spec(
        DesktopOs::Macos,
        "Google Chrome",
        Path::new("/Applications/Google Chrome.app"),
        "http://127.0.0.1:3789",
    );
    assert_eq!(mac_spec.program, PathBuf::from("open"));
    assert_eq!(
        mac_spec.args,
        vec![
            "-na",
            "/Applications/Google Chrome.app",
            "--args",
            "--app=http://127.0.0.1:3789",
            "--new-window",
        ]
    );

    let win_spec = build_app_window_launch_spec(
        DesktopOs::Windows,
        "msedge",
        Path::new(r"C:\Program Files\Microsoft\Edge\Application\msedge.exe"),
        "http://127.0.0.1:3789",
    );
    assert_eq!(
        win_spec.program,
        PathBuf::from(r"C:\Program Files\Microsoft\Edge\Application\msedge.exe")
    );
    assert_eq!(
        win_spec.args,
        vec!["--app=http://127.0.0.1:3789", "--new-window"]
    );

    // 2. Shortcut installation & fingerprint-safe uninstallation in isolated TempDir
    let env = IsolatedEnv::new();
    let apps_dir = env.xdg_data_home.join("applications");
    let desktop_dir = env.home_dir.join("Desktop");
    fs::create_dir_all(&apps_dir).unwrap();
    fs::create_dir_all(&desktop_dir).unwrap();

    // User custom files in same directories must never be touched
    let user_app_file = apps_dir.join("user-custom.desktop");
    let user_desktop_file = desktop_dir.join("my-notes.desktop");
    fs::write(&user_app_file, "[Desktop Entry]\nName=My App\n").unwrap();
    fs::write(&user_desktop_file, "[Desktop Entry]\nName=Notes\n").unwrap();

    // Even a same-named file without our fingerprint marker must NOT be deleted by uninstall
    let foreign_same_name = apps_dir.join("agy-pulse-dashboard.desktop");
    fs::write(
        &foreign_same_name,
        "[Desktop Entry]\nName=User Custom Entry\n",
    )
    .unwrap();
    let foreign_uninstall = env
        .base_cmd()
        .args(["dashboard", "uninstall-shortcut", "--desktop"])
        .output()
        .unwrap();
    assert!(foreign_uninstall.status.success());
    assert!(
        foreign_same_name.exists(),
        "uninstall-shortcut must NOT delete files lacking the exclusive fingerprint"
    );
    fs::remove_file(&foreign_same_name).unwrap();

    // Install with --desktop
    let install_out = env
        .base_cmd()
        .args(["dashboard", "install-shortcut", "--desktop"])
        .output()
        .unwrap();
    assert!(
        install_out.status.success(),
        "install-shortcut failed: {}",
        String::from_utf8_lossy(&install_out.stderr)
    );

    let installed_entry = apps_dir.join("agy-pulse-dashboard.desktop");
    let installed_desktop = desktop_dir.join("agy-pulse-dashboard.desktop");
    let installed_icon = env
        .xdg_data_home
        .join("icons/hicolor/scalable/apps/agy-pulse-dashboard.svg");
    assert!(installed_entry.exists(), "missing app menu desktop entry");
    assert!(installed_desktop.exists(), "missing Desktop shortcut copy");
    assert!(installed_icon.exists(), "missing shortcut icon SVG");

    let entry_text = fs::read_to_string(&installed_entry).unwrap();
    assert!(entry_text.contains(DASHBOARD_SHORTCUT_MARKER));
    assert!(entry_text.contains("dashboard open"));

    let status_installed = env
        .base_cmd()
        .args(["dashboard", "status"])
        .output()
        .unwrap();
    assert!(status_installed.status.success());
    let status_text = String::from_utf8_lossy(&status_installed.stdout);
    assert!(
        status_text.contains("installed"),
        "expected shortcut installed in status, got: {status_text}"
    );

    // Uninstall removes only fingerprinted files, leaving user custom files intact
    let uninstall_out = env
        .base_cmd()
        .args(["dashboard", "uninstall-shortcut", "--desktop"])
        .output()
        .unwrap();
    assert!(uninstall_out.status.success());

    assert!(!installed_entry.exists());
    assert!(!installed_desktop.exists());
    assert!(!installed_icon.exists());
    assert!(user_app_file.exists(), "user app file was wrongly deleted");
    assert!(
        user_desktop_file.exists(),
        "user desktop file was wrongly deleted"
    );
}
