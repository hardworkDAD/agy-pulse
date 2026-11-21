#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tempfile::TempDir;

use agy_pulse::config::wizard::{WizardRunOptions, WizardRunOutcome, run_wizard_loop};
use agy_pulse::config::{DisplayConfig, LanguageSetting, PulseConfig, load_config};
use agy_pulse::dashboard::{
    DASHBOARD_APP_ID, DASHBOARD_PING_THROTTLE_MS, DASHBOARD_SPAWN_LOCK_FILENAME,
    DASHBOARD_STATE_FILENAME, DashboardServer, DashboardServerConfig, DashboardStateFile,
    DEFAULT_DASHBOARD_PORT, STATUSLINE_IDLE_TIMEOUT_SEC, StatusLineDashboardInfo,
    TouchDashboardResult, acquire_dashboard_spawn_lock, dashboard_spawn_lock_path,
    dashboard_state_path, get_running_dashboard_info, is_valid_loopback_dashboard_url,
    release_dashboard_spawn_lock, resolve_dashboard_port, resolve_dashboard_url_for_statusline,
    resolve_statusline_idle_timeout_sec, touch_running_dashboard, write_dashboard_state,
};
use agy_pulse::domain::{HostIdentity, ModelIdentity, PulseSnapshot, WorkspaceInfo};
use agy_pulse::platform::{self, TerminalCaps};
use agy_pulse::render::{render_statusline, strip_ansi};

static ENV_MUTEX: Mutex<()> = Mutex::new(());

fn agy_pulse_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agy-pulse"))
}

fn agypl_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agypl"))
}

struct EnvVarGuard {
    key: String,
    prev: Option<OsString>,
}

impl EnvVarGuard {
    fn set<V: AsRef<std::ffi::OsStr>>(key: &str, value: V) -> Self {
        let prev = env::var_os(key);
        // SAFETY: protected by `ENV_MUTEX` in all in-process tests that mutate env vars.
        unsafe { env::set_var(key, value) };
        Self {
            key: key.to_string(),
            prev,
        }
    }

    fn unset(key: &str) -> Self {
        let prev = env::var_os(key);
        // SAFETY: protected by `ENV_MUTEX` in all in-process tests that mutate env vars.
        unsafe { env::remove_var(key) };
        Self {
            key: key.to_string(),
            prev,
        }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: restores original environment variable state.
        unsafe {
            match &self.prev {
                Some(v) => env::set_var(&self.key, v),
                None => env::remove_var(&self.key),
            }
        }
    }
}

struct IsolatedEnv {
    _temp: TempDir,
    home_dir: PathBuf,
    xdg_data_home: PathBuf,
    xdg_cfg_home: PathBuf,
    cli_data_dir: PathBuf,
    ide_data_dir: PathBuf,
    global_cfg_dir: PathBuf,
    bin_dir: PathBuf,
    local_app_data: PathBuf,
    app_data: PathBuf,
    workspace_dir: PathBuf,
    tmp_dir: PathBuf,
}

impl IsolatedEnv {
    fn new() -> Self {
        let temp = TempDir::new().expect("create temp dir");
        let root = temp.path();
        let home_dir = root.join("home");
        let xdg_data_home = home_dir.join(".local/share");
        let xdg_cfg_home = home_dir.join(".config");
        let cli_data_dir = root.join("cli_data");
        let ide_data_dir = root.join("ide_data");
        let global_cfg_dir = root.join("global_cfg");
        let bin_dir = root.join("bin");
        let local_app_data = root.join("local_app_data");
        let app_data = root.join("app_data");
        let workspace_dir = root.join("workspace");
        let tmp_dir = root.join("tmp");
        for dir in [
            &home_dir,
            &xdg_data_home,
            &xdg_cfg_home,
            &cli_data_dir,
            &ide_data_dir,
            &global_cfg_dir,
            &bin_dir,
            &local_app_data,
            &app_data,
            &workspace_dir,
            &tmp_dir,
        ] {
            fs::create_dir_all(dir).unwrap();
        }
        Self {
            _temp: temp,
            home_dir,
            xdg_data_home,
            xdg_cfg_home,
            cli_data_dir,
            ide_data_dir,
            global_cfg_dir,
            bin_dir,
            local_app_data,
            app_data,
            workspace_dir,
            tmp_dir,
        }
    }

    fn state_file_path(&self) -> PathBuf {
        self.cli_data_dir.join(DASHBOARD_STATE_FILENAME)
    }

    fn lock_file_path(&self) -> PathBuf {
        self.cli_data_dir.join(DASHBOARD_SPAWN_LOCK_FILENAME)
    }

    fn base_cmd(&self, bin: &Path) -> Command {
        let mut cmd = Command::new(bin);
        cmd.current_dir(&self.workspace_dir)
            .env("HOME", &self.home_dir)
            .env("USERPROFILE", &self.home_dir)
            .env("XDG_DATA_HOME", &self.xdg_data_home)
            .env("XDG_CONFIG_HOME", &self.xdg_cfg_home)
            .env("LOCALAPPDATA", &self.local_app_data)
            .env("LocalAppData", &self.local_app_data)
            .env("APPDATA", &self.app_data)
            .env("AppData", &self.app_data)
            .env("AGY_PULSE_DATA_DIR", &self.cli_data_dir)
            .env("AGY_PULSE_IDE_DATA_DIR", &self.ide_data_dir)
            .env("AGY_PULSE_GLOBAL_CONFIG_DIR", &self.global_cfg_dir)
            .env("AGY_PULSE_BIN_DIR", &self.bin_dir)
            .env("AGY_PULSE_NO_OPEN", "1")
            .env("AGY_PULSE_DISABLE_UPDATE_CHECK", "1")
            .env("TMPDIR", &self.tmp_dir)
            .env_remove("AGY_PULSE_FORCE_ASCII")
            .env_remove("AGY_PULSE_FORCE_UNICODE")
            .env_remove("AGY_PULSE_LANGUAGE")
            .env_remove("COLUMNS")
            .env("LC_ALL", "en_US.UTF-8")
            .env("LANG", "en_US.UTF-8");
        cmd
    }

    fn run_statusline(&self, stdin_text: &str, extra_env: &[(&str, &str)]) -> (i32, String, String) {
        let mut cmd = self.base_cmd(&agy_pulse_bin());
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn agy-pulse");
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(stdin_text.as_bytes()).unwrap();
        }
        let output = child.wait_with_output().unwrap();
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct DashboardDaemonCleanup {
    state_path: PathBuf,
    cli_data_dir: PathBuf,
    port: u16,
}

impl Drop for DashboardDaemonCleanup {
    fn drop(&mut self) {
        let _ = Command::new(agypl_bin())
            .env("AGY_PULSE_DATA_DIR", &self.cli_data_dir)
            .args(["dashboard", "stop", "--port", &self.port.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();

        if let Ok(raw) = fs::read_to_string(&self.state_path)
            && let Ok(val) = serde_json::from_str::<Value>(&raw)
            && let Some(pid) = val["pid"].as_u64()
            && pid > 0
            && pid <= u32::MAX as u64
        {
            let _ = platform::terminate_process(pid as u32);
        }
    }
}

fn free_loopback_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind ephemeral port");
    listener.local_addr().unwrap().port()
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

fn http_get_status_and_body(port: u16, target: &str) -> Option<(u16, String)> {
    let mut stream = TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(250),
    )
    .ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let req = format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).ok()?;
    stream.flush().ok()?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).ok()?;
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text.split_once("\r\n\r\n")?;
    let status = head
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse::<u16>()
        .ok()?;
    Some((status, body.to_string()))
}

#[test]
fn t11_loopback_url_validation_and_line1_rendering() {
    assert!(is_valid_loopback_dashboard_url("http://127.0.0.1:3789"));
    assert!(is_valid_loopback_dashboard_url("http://localhost:8080"));
    assert!(is_valid_loopback_dashboard_url("http://127.0.0.1"));
    assert!(is_valid_loopback_dashboard_url("  http://LOCALHOST:3789  "));

    assert!(!is_valid_loopback_dashboard_url(""));
    assert!(!is_valid_loopback_dashboard_url("https://127.0.0.1:3789"));
    assert!(!is_valid_loopback_dashboard_url("http://192.168.1.5:3789"));
    assert!(!is_valid_loopback_dashboard_url("http://evil.example:3789"));
    assert!(!is_valid_loopback_dashboard_url("http://user:pass@127.0.0.1:3789"));
    assert!(!is_valid_loopback_dashboard_url("http://127.0.0.1:0"));
    assert!(!is_valid_loopback_dashboard_url("http://127.0.0.1:70000"));
    assert!(!is_valid_loopback_dashboard_url("http://127.0.0.1:3789\nfoo"));
    assert!(!is_valid_loopback_dashboard_url("http://127.0.0.1:3789\x07"));
    assert!(!is_valid_loopback_dashboard_url("http://127.0.0.1:3789\x1b[31m"));
    assert!(!is_valid_loopback_dashboard_url("http://127.0.0.1:3789 foo"));

    let base_snapshot = PulseSnapshot {
        has_conversation_identity: true,
        model: Some(ModelIdentity {
            id: "claude-sonnet-4-6".to_string(),
            display_name: "Claude Sonnet 4.6".to_string(),
            effort: None,
        }),
        host: HostIdentity {
            plan_tier: Some("Pro".to_string()),
            cycle_mode: Some("auto".to_string()),
            ..Default::default()
        },
        workspace: WorkspaceInfo {
            current_dir: "ws".to_string(),
            git_branch: None,
        },
        output_speed_tok_s: Some(42.0),
        ttft_sec: Some(0.4),
        update_available_version: Some("2.1.0".to_string()),
        dashboard_url: Some("http://127.0.0.1:3789".to_string()),
        ..Default::default()
    };

    let default_cfg = PulseConfig {
        display: DisplayConfig {
            show_model: Some(true),
            show_context_monitor: Some(false),
            show_model_cache: Some(false),
            ..DisplayConfig::builtin_defaults()
        },
        language: LanguageSetting::En,
        ..Default::default()
    };

    // 1. Default showDashboardUrl=true places URL after speed and before update badge
    let wide = strip_ansi(&render_statusline(
        &base_snapshot,
        &default_cfg,
        &TerminalCaps {
            unicode: true,
            columns: Some(160),
        },
        LanguageSetting::En,
    ));
    assert_eq!(
        wide,
        "ws │ Claude Sonnet 4.6 │ Pro │ auto │ out: 42.0 tok/s (ttft 400ms) │ http://127.0.0.1:3789 │ ⟳ v2.1.0"
    );

    // 2. 80 cols drops TTFT & update badge first while preserving full URL
    let cols_80 = strip_ansi(&render_statusline(
        &base_snapshot,
        &default_cfg,
        &TerminalCaps {
            unicode: true,
            columns: Some(80),
        },
        LanguageSetting::En,
    ));
    assert!(
        cols_80.contains("http://127.0.0.1:3789"),
        "expected full URL preserved at 80 cols, got: {cols_80}"
    );
    assert!(
        !cols_80.contains("ttft"),
        "expected TTFT dropped before URL at 80 cols, got: {cols_80}"
    );

    // 3. 45 cols hides URL as an atomic unit without truncating mid-URL
    let cols_45 = strip_ansi(&render_statusline(
        &base_snapshot,
        &default_cfg,
        &TerminalCaps {
            unicode: true,
            columns: Some(45),
        },
        LanguageSetting::En,
    ));
    assert!(
        !cols_45.contains("http://") && !cols_45.contains("127"),
        "expected entire URL omitted at 45 cols, got: {cols_45}"
    );

    // 4. showDashboardUrl=false hides URL even when snapshot carries one
    let hidden_cfg = PulseConfig {
        display: DisplayConfig {
            show_dashboard_url: Some(false),
            ..default_cfg.display.clone()
        },
        ..default_cfg.clone()
    };
    let hidden = strip_ansi(&render_statusline(
        &base_snapshot,
        &hidden_cfg,
        &TerminalCaps {
            unicode: true,
            columns: Some(160),
        },
        LanguageSetting::En,
    ));
    assert!(!hidden.contains("127.0.0.1"));

    // 5. Malicious non-loopback URLs and OSC 52 escape injection are filtered
    for bad_url in [
        "https://evil.example:3789",
        "http://user:pass@127.0.0.1:3789",
        "http://evil.example\x1b]52;c;Zm9v\x07",
    ] {
        let mut bad_snap = base_snapshot.clone();
        bad_snap.dashboard_url = Some(bad_url.to_string());
        let raw_rendered = render_statusline(
            &bad_snap,
            &default_cfg,
            &TerminalCaps {
                unicode: true,
                columns: Some(160),
            },
            LanguageSetting::En,
        );
        assert!(
            !raw_rendered.contains("evil.example")
                && !raw_rendered.contains("user:pass")
                && !raw_rendered.contains("\x1b]52"),
            "malicious URL leaked into output: {raw_rendered:?}"
        );
    }

    let mut osc_wrapped = base_snapshot.clone();
    osc_wrapped.dashboard_url = Some("http://127.0.0.1:3789\x1b]52;c;Zm9v\x07".to_string());
    let raw_osc = render_statusline(
        &osc_wrapped,
        &default_cfg,
        &TerminalCaps {
            unicode: true,
            columns: Some(160),
        },
        LanguageSetting::En,
    );
    assert!(!raw_osc.contains("\x1b]52") && !raw_osc.contains("Zm9v"));
    assert!(strip_ansi(&raw_osc).contains("http://127.0.0.1:3789"));
}

#[test]
fn t11_config_defaults_and_port_idle_precedence() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let env = IsolatedEnv::new();
    let _g_cfg = EnvVarGuard::set("AGY_PULSE_GLOBAL_CONFIG_DIR", &env.global_cfg_dir);
    let _g_port = EnvVarGuard::unset("AGY_PULSE_DASHBOARD_PORT");
    let _g_idle = EnvVarGuard::unset("AGY_PULSE_DASHBOARD_IDLE_SEC");

    let defaults = DisplayConfig::builtin_defaults();
    assert_eq!(defaults.show_dashboard_url, Some(true));
    assert_eq!(defaults.dashboard_port, None);
    assert_eq!(defaults.dashboard_idle_timeout_sec, None);

    let loaded = load_config(&env.workspace_dir);
    assert_eq!(loaded.display.show_dashboard_url, Some(true));
    assert_eq!(resolve_dashboard_port(&loaded), DEFAULT_DASHBOARD_PORT);
    assert_eq!(
        resolve_statusline_idle_timeout_sec(&loaded),
        STATUSLINE_IDLE_TIMEOUT_SEC
    );

    let mut cfg = loaded;
    cfg.display.dashboard_port = Some(4123);
    cfg.display.dashboard_idle_timeout_sec = Some(0);
    assert_eq!(resolve_dashboard_port(&cfg), 4123);
    assert_eq!(resolve_statusline_idle_timeout_sec(&cfg), 0);

    let _g_port2 = EnvVarGuard::set("AGY_PULSE_DASHBOARD_PORT", "4999");
    let _g_idle2 = EnvVarGuard::set("AGY_PULSE_DASHBOARD_IDLE_SEC", "90");
    assert_eq!(resolve_dashboard_port(&cfg), 4999);
    assert_eq!(resolve_statusline_idle_timeout_sec(&cfg), 90);

    // Invalid env values gracefully fall back to config
    let _g_port_bad = EnvVarGuard::set("AGY_PULSE_DASHBOARD_PORT", "0");
    let _g_idle_bad = EnvVarGuard::set("AGY_PULSE_DASHBOARD_IDLE_SEC", "not-a-number");
    assert_eq!(resolve_dashboard_port(&cfg), 4123);
    assert_eq!(resolve_statusline_idle_timeout_sec(&cfg), 0);
}

#[test]
fn t11_spawn_lock_dead_pid_cleanup_and_touch_keepalive() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let env = IsolatedEnv::new();
    let _g_data = EnvVarGuard::set("AGY_PULSE_DATA_DIR", &env.cli_data_dir);
    let _g_port = EnvVarGuard::unset("AGY_PULSE_DASHBOARD_PORT");
    let _g_idle = EnvVarGuard::unset("AGY_PULSE_DASHBOARD_IDLE_SEC");

    // 1. Spawn lock mutual exclusion, release, and TTL expiration reclaim
    assert!(acquire_dashboard_spawn_lock(10_000));
    assert!(dashboard_spawn_lock_path().exists());
    assert!(!acquire_dashboard_spawn_lock(10_000));
    assert!(release_dashboard_spawn_lock());
    assert!(!dashboard_spawn_lock_path().exists());

    assert!(acquire_dashboard_spawn_lock(10_000));
    let lock_file = fs::OpenOptions::new()
        .write(true)
        .open(dashboard_spawn_lock_path())
        .unwrap();
    lock_file
        .set_modified(SystemTime::now() - Duration::from_secs(30))
        .unwrap();
    drop(lock_file);
    assert!(
        acquire_dashboard_spawn_lock(10_000),
        "expired spawn lock must be reclaimed"
    );
    assert!(release_dashboard_spawn_lock());

    // 2. Dead PID cleanup in get_running_dashboard_info()
    let dead_state = DashboardStateFile {
        app: DASHBOARD_APP_ID.to_string(),
        pid: 999_999_999,
        port: 3789,
        host: "127.0.0.1".to_string(),
        url: "http://127.0.0.1:3789".to_string(),
        idle_timeout_ms: 1_800_000,
        started_at: "2025-01-01T00:00:00.000Z".to_string(),
    };
    write_dashboard_state(&dead_state).unwrap();
    assert!(get_running_dashboard_info().is_none());
    assert!(
        !dashboard_state_path().exists(),
        "dead PID discovery file must be cleaned up"
    );

    // 3. Reject malicious non-loopback URL even when PID is alive
    let bad_url_state = DashboardStateFile {
        app: DASHBOARD_APP_ID.to_string(),
        pid: std::process::id(),
        port: 3789,
        host: "evil.example".to_string(),
        url: "https://evil.example:3789".to_string(),
        idle_timeout_ms: 1_800_000,
        started_at: "2025-01-01T00:00:00.000Z".to_string(),
    };
    write_dashboard_state(&bad_url_state).unwrap();
    assert!(get_running_dashboard_info().is_none());
    let _ = fs::remove_file(dashboard_state_path());

    // 4. resolve_dashboard_url_for_statusline with auto_start=false returns None when not running,
    //    and reuses running URL when state file has alive PID
    let cfg = PulseConfig::default();
    assert!(resolve_dashboard_url_for_statusline(&cfg, LanguageSetting::En, false).is_none());

    // 5. touch_running_dashboard skip/throttle/ping/failure-cleanup against real DashboardServer
    let mut server = DashboardServer::start(DashboardServerConfig {
        host: "127.0.0.1".to_string(),
        port: 0,
        no_sync: true,
        idle_timeout_ms: 30_000,
        ledger_path: Some(env.cli_data_dir.join("ledger.sqlite")),
        ..Default::default()
    })
    .unwrap();
    let srv_info = server.info().clone();

    let live_state = DashboardStateFile {
        app: DASHBOARD_APP_ID.to_string(),
        pid: std::process::id(),
        port: srv_info.port,
        host: srv_info.host.clone(),
        url: srv_info.url.clone(),
        idle_timeout_ms: 30_000,
        started_at: srv_info.started_at.clone(),
    };
    write_dashboard_state(&live_state).unwrap();

    // Backdate mtime to 2 seconds ago so the next touch is initially throttled (< 10s),
    // then unthrottled when passing a future `now_ms`.
    let old_time = SystemTime::now() - Duration::from_secs(2);
    let sf = fs::OpenOptions::new()
        .write(true)
        .open(dashboard_state_path())
        .unwrap();
    sf.set_modified(old_time).unwrap();
    drop(sf);
    let old_mtime_ms = old_time.duration_since(UNIX_EPOCH).unwrap().as_millis() as i64;

    let discovered = get_running_dashboard_info().expect("discovered live server");
    assert!(discovered.running);
    assert_eq!(discovered.port, srv_info.port);
    assert_eq!(discovered.url, srv_info.url);

    let reused = resolve_dashboard_url_for_statusline(&cfg, LanguageSetting::En, false)
        .expect("reuse running dashboard");
    assert_eq!(reused.url, srv_info.url);

    let not_running = StatusLineDashboardInfo {
        running: false,
        ..discovered.clone()
    };
    assert_eq!(
        touch_running_dashboard(&not_running, &cfg, LanguageSetting::En, false, old_mtime_ms + 50_000),
        TouchDashboardResult::Skipped("not_running")
    );

    let no_idle = StatusLineDashboardInfo {
        idle_timeout_ms: 0,
        ..discovered.clone()
    };
    assert_eq!(
        touch_running_dashboard(&no_idle, &cfg, LanguageSetting::En, false, old_mtime_ms + 50_000),
        TouchDashboardResult::Skipped("no_idle_timeout")
    );

    // Within throttle_ms = (30_000 / 3).clamp(1_000, 300_000) = 10_000ms
    assert_eq!(
        touch_running_dashboard(&discovered, &cfg, LanguageSetting::En, false, discovered.state_mtime_ms + 5_000),
        TouchDashboardResult::Skipped("throttled")
    );

    // Past throttle window -> sends POST /api/ping and refreshes mtime
    let ping_res = touch_running_dashboard(
        &discovered,
        &cfg,
        LanguageSetting::En,
        false,
        discovered.state_mtime_ms + 15_000,
    );
    assert_eq!(ping_res, TouchDashboardResult::Pinged { ok: true });
    let refreshed_mtime = fs::metadata(dashboard_state_path())
        .unwrap()
        .modified()
        .unwrap();
    assert!(
        refreshed_mtime > old_time,
        "expected state file mtime to be refreshed after successful ping"
    );

    // Also validate max clamp at 300_000ms when idle_timeout_ms is large (1_800_000ms)
    let long_idle = StatusLineDashboardInfo {
        idle_timeout_ms: 1_800_000,
        state_mtime_ms: 1_000_000,
        ..discovered.clone()
    };
    assert_eq!(
        touch_running_dashboard(
            &long_idle,
            &cfg,
            LanguageSetting::En,
            false,
            1_000_000 + (DASHBOARD_PING_THROTTLE_MS as i64 - 1)
        ),
        TouchDashboardResult::Skipped("throttled")
    );

    // Shut down server -> ping fails -> clears stale state file
    server.shutdown();
    let failed_res = touch_running_dashboard(
        &discovered,
        &cfg,
        LanguageSetting::En,
        false,
        discovered.state_mtime_ms + 20_000,
    );
    assert_eq!(failed_res, TouchDashboardResult::Pinged { ok: false });
    assert!(
        !dashboard_state_path().exists(),
        "failed ping must clear stale discovery file"
    );
}

#[test]
fn t11_foreground_state_file_and_background_dedupe() {
    // 1. Foreground `dashboard` writes state file and cleans it up on graceful exit
    let env_fg = IsolatedEnv::new();
    let fg_port = free_loopback_port();
    let fg_child = env_fg
        .base_cmd(&agypl_bin())
        .args([
            "dashboard",
            "--no-open",
            "--no-sync",
            "--idle-timeout",
            "1",
            "--port",
            &fg_port.to_string(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let fg_pid = fg_child.id();
    let mut fg_guard = ChildGuard(fg_child);

    assert!(
        wait_until(Duration::from_secs(4), || {
            env_fg.state_file_path().exists()
                && http_get_status_and_body(fg_port, "/api/health")
                    .is_some_and(|(s, _)| s == 200)
        }),
        "foreground dashboard did not start and write state file"
    );
    let fg_state: Value =
        serde_json::from_str(&fs::read_to_string(env_fg.state_file_path()).unwrap()).unwrap();
    assert_eq!(fg_state["pid"], fg_pid);
    assert_eq!(fg_state["port"], fg_port);

    assert!(
        wait_until(Duration::from_secs(5), || {
            matches!(fg_guard.0.try_wait(), Ok(Some(_))) && !env_fg.state_file_path().exists()
        }),
        "foreground dashboard must exit gracefully on idle timeout and clean up state file"
    );

    // 2. Concurrent `--serve-background` probes healthy instance, reuses it, and releases spawn lock
    let env_bg = IsolatedEnv::new();
    let bg_port = free_loopback_port();
    let _cleanup = DashboardDaemonCleanup {
        state_path: env_bg.state_file_path(),
        cli_data_dir: env_bg.cli_data_dir.clone(),
        port: bg_port,
    };

    let open_out = env_bg
        .base_cmd(&agypl_bin())
        .args([
            "dashboard",
            "open",
            "--no-open",
            "--no-sync",
            "--port",
            &bg_port.to_string(),
        ])
        .output()
        .unwrap();
    assert!(open_out.status.success());

    let state_before: Value =
        serde_json::from_str(&fs::read_to_string(env_bg.state_file_path()).unwrap()).unwrap();
    let pid_before = state_before["pid"].as_u64().unwrap();

    fs::write(env_bg.lock_file_path(), r#"{"pid":1,"ts":0}"#).unwrap();
    let dup_out = env_bg
        .base_cmd(&agypl_bin())
        .args([
            "dashboard",
            "--serve-background",
            "--no-open",
            "--no-sync",
            "--port",
            &bg_port.to_string(),
        ])
        .output()
        .unwrap();
    assert!(dup_out.status.success());
    assert!(
        !env_bg.lock_file_path().exists(),
        "duplicate --serve-background must release spawn lock"
    );
    let state_after: Value =
        serde_json::from_str(&fs::read_to_string(env_bg.state_file_path()).unwrap()).unwrap();
    assert_eq!(state_after["pid"].as_u64().unwrap(), pid_before);
}

#[test]
fn t11_cli_statusline_autostart_reuse_and_opt_out() {
    let env = IsolatedEnv::new();
    let port = free_loopback_port();
    let port_str = port.to_string();
    let _cleanup = DashboardDaemonCleanup {
        state_path: env.state_file_path(),
        cli_data_dir: env.cli_data_dir.clone(),
        port,
    };

    // 1. With AGY_PULSE_NO_DASHBOARD_AUTOSTART=1 and no running dashboard, no daemon starts & no URL shown
    let (code_off, out_off, err_off) = env.run_statusline(
        "{}",
        &[
            ("AGY_PULSE_NO_DASHBOARD_AUTOSTART", "1"),
            ("AGY_PULSE_DASHBOARD_PORT", &port_str),
            ("COLUMNS", "160"),
        ],
    );
    assert_eq!(code_off, 0, "stderr: {err_off}");
    assert!(!strip_ansi(&out_off).contains(&format!("http://127.0.0.1:{port}")));
    assert!(!env.state_file_path().exists());

    // 2. First real statusline run silently spawns background daemon and outputs expected URL
    let expected_url = format!("http://127.0.0.1:{port}");
    let (code1, out1, err1) = env.run_statusline(
        "{}",
        &[
            ("AGY_PULSE_NO_DASHBOARD_AUTOSTART", "0"),
            ("AGY_PULSE_DASHBOARD_PORT", &port_str),
            ("AGY_PULSE_DASHBOARD_IDLE_SEC", "30"),
            ("COLUMNS", "160"),
        ],
    );
    assert_eq!(code1, 0, "stderr: {err1}");
    assert!(
        strip_ansi(&out1).contains(&expected_url),
        "expected dashboard URL in first statusline output: {out1}"
    );

    assert!(
        wait_until(Duration::from_secs(5), || {
            env.state_file_path().exists()
                && http_get_status_and_body(port, "/api/health")
                    .is_some_and(|(s, _)| s == 200)
        }),
        "statusline auto-started dashboard did not become healthy"
    );

    let state1: Value =
        serde_json::from_str(&fs::read_to_string(env.state_file_path()).unwrap()).unwrap();
    let daemon_pid = state1["pid"].as_u64().unwrap();
    assert_eq!(state1["port"], port);

    // 3. Second statusline run reuses the exact same daemon PID
    let (code2, out2, err2) = env.run_statusline(
        "{}",
        &[
            ("AGY_PULSE_NO_DASHBOARD_AUTOSTART", "0"),
            ("AGY_PULSE_DASHBOARD_PORT", &port_str),
            ("AGY_PULSE_DASHBOARD_IDLE_SEC", "30"),
            ("COLUMNS", "160"),
        ],
    );
    assert_eq!(code2, 0, "stderr: {err2}");
    assert!(strip_ansi(&out2).contains(&expected_url));
    let state2: Value =
        serde_json::from_str(&fs::read_to_string(env.state_file_path()).unwrap()).unwrap();
    assert_eq!(state2["pid"].as_u64().unwrap(), daemon_pid);

    // 4. Even when AGY_PULSE_NO_DASHBOARD_AUTOSTART=1, a running dashboard URL is still displayed
    let (code3, out3, err3) = env.run_statusline(
        "{}",
        &[
            ("AGY_PULSE_NO_DASHBOARD_AUTOSTART", "1"),
            ("AGY_PULSE_DASHBOARD_PORT", &port_str),
            ("COLUMNS", "160"),
        ],
    );
    assert_eq!(code3, 0, "stderr: {err3}");
    assert!(strip_ansi(&out3).contains(&expected_url));

    // 5. Stop daemon cleanly and verify stale/dead-PID self-heal respawns via CLI
    let stop_out = env
        .base_cmd(&agypl_bin())
        .args(["dashboard", "stop", "--port", &port_str])
        .output()
        .unwrap();
    assert!(stop_out.status.success());

    // Simulate a hung/dead-port state file owned by a live dummy process so ping fails -> respawn
    let dummy_pid = std::process::id();

    let hung_state = DashboardStateFile {
        app: DASHBOARD_APP_ID.to_string(),
        pid: dummy_pid,
        port,
        host: "127.0.0.1".to_string(),
        url: expected_url.clone(),
        idle_timeout_ms: 30_000,
        started_at: "2025-01-01T00:00:00.000Z".to_string(),
    };
    fs::write(
        env.state_file_path(),
        serde_json::to_string_pretty(&hung_state).unwrap(),
    )
    .unwrap();
    let sf = fs::OpenOptions::new()
        .write(true)
        .open(env.state_file_path())
        .unwrap();
    sf.set_modified(SystemTime::now() - Duration::from_secs(20))
        .unwrap();
    drop(sf);

    let (code4, out4, err4) = env.run_statusline(
        "{}",
        &[
            ("AGY_PULSE_NO_DASHBOARD_AUTOSTART", "0"),
            ("AGY_PULSE_DASHBOARD_PORT", &port_str),
            ("AGY_PULSE_DASHBOARD_IDLE_SEC", "30"),
            ("COLUMNS", "160"),
        ],
    );
    assert_eq!(code4, 0, "stderr: {err4}");
    assert!(strip_ansi(&out4).contains(&expected_url));
    assert!(
        wait_until(Duration::from_secs(5), || {
            http_get_status_and_body(port, "/api/health").is_some_and(|(s, _)| s == 200)
        }),
        "respawned dashboard did not become healthy after ping failure"
    );
    let state4: Value =
        serde_json::from_str(&fs::read_to_string(env.state_file_path()).unwrap()).unwrap();
    assert_ne!(state4["pid"].as_u64().unwrap(), u64::from(dummy_pid));
}

#[test]
fn t11_wizard_preview_toggle_save_and_uninstall_cleans_lock() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let env = IsolatedEnv::new();
    let _g_cfg = EnvVarGuard::set("AGY_PULSE_GLOBAL_CONFIG_DIR", &env.global_cfg_dir);
    let _g_data = EnvVarGuard::set("AGY_PULSE_DATA_DIR", &env.cli_data_dir);
    let _g_ide = EnvVarGuard::set("AGY_PULSE_IDE_DATA_DIR", &env.ide_data_dir);

    let global_cfg = env.global_cfg_dir.join("agy-pulse.config.json");
    fs::write(&global_cfg, "{}\n").unwrap();

    // 1. Preview includes http://127.0.0.1:3789 and menu includes Show Dashboard URL (Auto-Start);
    //    MENU_ITEMS has DashboardUrl at index 24 (press '9' -> idx 8, then 16x 'j' -> idx 24, Space -> false, 's' -> save).
    let mut keys = Vec::new();
    keys.push(b'9');
    keys.extend_from_slice(&[b'j'; 16]);
    keys.push(b' ');
    keys.push(b's');

    let opts = WizardRunOptions {
        cwd: env.workspace_dir.clone(),
        terminal_cols: Some(160),
        terminal_rows: Some(45),
        unicode: true,
        locale: Some("en_US.UTF-8".to_string()),
        observed_at_ms: Some(1_700_000_000_000),
        clear_screen: false,
    };

    let mut input = Cursor::new(keys);
    let mut output = Vec::new();
    let outcome = run_wizard_loop(&mut input, &mut output, &opts).unwrap();
    assert_eq!(outcome, WizardRunOutcome::Saved);

    let plain = strip_ansi(&String::from_utf8_lossy(&output));
    assert!(
        plain.contains("Show Dashboard URL (Auto-Start)"),
        "expected wizard menu item, got: {plain}"
    );
    assert!(
        plain.contains("http://127.0.0.1:3789"),
        "expected default dashboard URL in wizard preview: {plain}"
    );

    let saved: Value = serde_json::from_str(&fs::read_to_string(&global_cfg).unwrap()).unwrap();
    assert_eq!(saved["display"]["showDashboardUrl"], false);

    // 2. Uninstall removes both agy-pulse-dashboard.json and agy-pulse-dashboard.lock
    fs::write(env.state_file_path(), r#"{"app":"agy-pulse-dashboard","pid":999999999,"port":3789,"host":"127.0.0.1","url":"http://127.0.0.1:3789"}"#).unwrap();
    fs::write(env.lock_file_path(), r#"{"pid":999999999,"ts":1}"#).unwrap();

    let uninst = env
        .base_cmd(&agypl_bin())
        .arg("uninstall")
        .output()
        .unwrap();
    assert!(
        uninst.status.success(),
        "uninstall failed: {}",
        String::from_utf8_lossy(&uninst.stderr)
    );
    assert!(!env.state_file_path().exists(), "dashboard state file must be removed by uninstall");
    assert!(!env.lock_file_path().exists(), "dashboard spawn lock must be removed by uninstall");
}
