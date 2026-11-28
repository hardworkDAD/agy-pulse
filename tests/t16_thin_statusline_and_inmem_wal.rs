#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Write;
use std::net::TcpListener;
#[cfg(unix)]
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::thread;
#[cfg(unix)]
use std::time::UNIX_EPOCH;
use std::time::{Duration, Instant, SystemTime};

use rusqlite::{Connection, params};
use tempfile::TempDir;

use agy_pulse::collector::read_collector_state;
#[cfg(unix)]
use agy_pulse::collector::{
    COLLECTOR_APP_ID, COLLECTOR_SNAPSHOT_SCHEMA_VERSION, CollectedStatuslineSnapshot,
    CollectorStateFile, CollectorWireRequest, CollectorWireResponse, collector_state_path,
    conversation_snapshot_path, read_frame, write_frame,
};
#[cfg(unix)]
use agy_pulse::config::{DisplayConfig, LanguageSetting, PulseConfig, QuotaStyle};
#[cfg(unix)]
use agy_pulse::domain::{CostSnapshot, ModelIdentity, PulseSnapshot, WorkspaceInfo};
use agy_pulse::platform;
use agy_pulse::render::strip_ansi;

static TEST_ENV_MUTEX: Mutex<()> = Mutex::new(());

fn agy_pulse_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agy-pulse"))
}

fn agypl_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agypl"))
}

struct IsolatedEnv {
    _temp: TempDir,
    home_dir: PathBuf,
    data_dir: PathBuf,
    runtime_dir: PathBuf,
    global_cfg_dir: PathBuf,
    xdg_cfg_dir: PathBuf,
    app_data_dir: PathBuf,
    local_app_data_dir: PathBuf,
    workspace_dir: PathBuf,
    tmp_dir: PathBuf,
}

impl IsolatedEnv {
    fn new() -> Self {
        let temp = TempDir::new().expect("create temp dir");
        Self::with_temp(temp)
    }

    fn with_temp(temp: TempDir) -> Self {
        let root = temp.path();
        let home_dir = root.join("home");
        let data_dir = root.join("data");
        let runtime_dir = data_dir.join("runtime");
        let global_cfg_dir = root.join("global_cfg");
        let xdg_cfg_dir = root.join("xdg_cfg");
        let app_data_dir = root.join("app_data");
        let local_app_data_dir = root.join("local_app_data");
        let workspace_dir = root.join("workspace");
        let tmp_dir = root.join("tmp");
        for dir in [
            &home_dir,
            &data_dir,
            &runtime_dir,
            &global_cfg_dir,
            &xdg_cfg_dir,
            &app_data_dir,
            &local_app_data_dir,
            &workspace_dir,
            &tmp_dir,
        ] {
            fs::create_dir_all(dir).unwrap();
        }
        Self {
            _temp: temp,
            home_dir,
            data_dir,
            runtime_dir,
            global_cfg_dir,
            xdg_cfg_dir,
            app_data_dir,
            local_app_data_dir,
            workspace_dir,
            tmp_dir,
        }
    }

    fn env_map(&self) -> HashMap<String, Option<String>> {
        let mut map = HashMap::new();
        map.insert(
            "HOME".to_string(),
            Some(self.home_dir.to_string_lossy().into_owned()),
        );
        map.insert(
            "USERPROFILE".to_string(),
            Some(self.home_dir.to_string_lossy().into_owned()),
        );
        map.insert(
            "XDG_DATA_HOME".to_string(),
            Some(
                self.home_dir
                    .join(".local/share")
                    .to_string_lossy()
                    .into_owned(),
            ),
        );
        map.insert(
            "XDG_CONFIG_HOME".to_string(),
            Some(self.xdg_cfg_dir.to_string_lossy().into_owned()),
        );
        map.insert(
            "APPDATA".to_string(),
            Some(self.app_data_dir.to_string_lossy().into_owned()),
        );
        map.insert(
            "LOCALAPPDATA".to_string(),
            Some(self.local_app_data_dir.to_string_lossy().into_owned()),
        );
        map.insert(
            "AGY_PULSE_DATA_DIR".to_string(),
            Some(self.data_dir.to_string_lossy().into_owned()),
        );
        map.insert(
            "AGY_PULSE_RUNTIME_DIR".to_string(),
            Some(self.runtime_dir.to_string_lossy().into_owned()),
        );
        map.insert(
            "AGY_PULSE_GLOBAL_CONFIG_DIR".to_string(),
            Some(self.global_cfg_dir.to_string_lossy().into_owned()),
        );
        map.insert(
            "TMPDIR".to_string(),
            Some(self.tmp_dir.to_string_lossy().into_owned()),
        );
        map.insert(
            "TMP".to_string(),
            Some(self.tmp_dir.to_string_lossy().into_owned()),
        );
        map.insert(
            "TEMP".to_string(),
            Some(self.tmp_dir.to_string_lossy().into_owned()),
        );
        map
    }

    fn base_cmd(&self, bin: &Path) -> Command {
        let mut cmd = Command::new(bin);
        cmd.current_dir(&self.workspace_dir)
            .env("HOME", &self.home_dir)
            .env("USERPROFILE", &self.home_dir)
            .env("XDG_DATA_HOME", self.home_dir.join(".local/share"))
            .env("XDG_CONFIG_HOME", &self.xdg_cfg_dir)
            .env("APPDATA", &self.app_data_dir)
            .env("AppData", &self.app_data_dir)
            .env("LOCALAPPDATA", &self.local_app_data_dir)
            .env("LocalAppData", &self.local_app_data_dir)
            .env("AGY_PULSE_DATA_DIR", &self.data_dir)
            .env("AGY_PULSE_RUNTIME_DIR", &self.runtime_dir)
            .env("AGY_PULSE_GLOBAL_CONFIG_DIR", &self.global_cfg_dir)
            .env("AGY_PULSE_DISABLE_UPDATE_CHECK", "1")
            .env("TMPDIR", &self.tmp_dir)
            .env("TMP", &self.tmp_dir)
            .env("TEMP", &self.tmp_dir)
            .env_remove("AGY_PULSE_FORCE_ASCII")
            .env_remove("AGY_PULSE_FORCE_UNICODE")
            .env_remove("AGY_PULSE_LANGUAGE")
            .env_remove("AGY_PULSE_SYNC_MODE")
            .env_remove("AGY_PULSE_COLLECTOR_WAIT_MS")
            .env_remove("AGY_PULSE_COLLECTOR_IDLE_SEC")
            .env_remove("COLUMNS")
            .env("LC_ALL", "en_US.UTF-8")
            .env("LANG", "en_US.UTF-8");
        cmd
    }

    fn run_statusline(
        &self,
        stdin_text: &str,
        extra_env: &[(&str, &str)],
    ) -> (i32, String, String, Duration) {
        let mut cmd = self.base_cmd(&agy_pulse_bin());
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let start = Instant::now();
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn agy-pulse");
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(stdin_text.as_bytes()).expect("write stdin");
        }
        let output = child.wait_with_output().expect("wait agy-pulse");
        let elapsed = start.elapsed();
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
            elapsed,
        )
    }
}

struct CollectorCleanup {
    runtime_dir: PathBuf,
    data_dir: PathBuf,
}

impl Drop for CollectorCleanup {
    fn drop(&mut self) {
        let mut ov = HashMap::new();
        ov.insert(
            "AGY_PULSE_RUNTIME_DIR".to_string(),
            Some(self.runtime_dir.to_string_lossy().into_owned()),
        );
        ov.insert(
            "AGY_PULSE_DATA_DIR".to_string(),
            Some(self.data_dir.to_string_lossy().into_owned()),
        );
        platform::with_env_overrides(&ov, || {
            if let Some(state) = read_collector_state()
                && platform::is_process_alive(state.pid)
            {
                let _ = platform::terminate_process(state.pid);
            }
        });
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

fn pb_duration(field_num: u32, sec: u64, nanos: u64) -> Vec<u8> {
    let mut inner = Vec::new();
    if sec > 0 {
        inner.extend(pb_varint(1, sec));
    }
    if nanos > 0 {
        inner.extend(pb_varint(2, nanos));
    }
    pb_bytes(field_num, &inner)
}

fn build_compaction_gen_metadata_blob(max_context_tokens: u64) -> Vec<u8> {
    let f10 = pb_varint(4, max_context_tokens);
    let f9 = pb_bytes(10, &f10);
    let f1 = pb_bytes(9, &f9);
    pb_bytes(1, &f1)
}

fn build_perf_gen_metadata_blob(
    step_idx: u64,
    output_tokens: u64,
    stream_sec: (u64, u64),
    ttft_sec: (u64, u64),
    raw_model: &str,
    display_name: &str,
) -> Vec<u8> {
    let mut out = pb_varint(2, step_idx);
    let mut f4 = Vec::new();
    f4.extend(pb_varint(1, 10_000));
    f4.extend(pb_varint(2, 2_000));
    f4.extend(pb_varint(3, output_tokens));
    let mut f1 = pb_bytes(4, &f4);
    f1.extend(pb_duration(11, ttft_sec.0, ttft_sec.1));
    f1.extend(pb_duration(12, stream_sec.0, stream_sec.1));
    f1.extend(pb_str(19, raw_model));
    f1.extend(pb_str(21, display_name));
    out.extend(pb_bytes(1, &f1));
    out
}

fn snapshot_dir_bytes(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut map = BTreeMap::new();
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        let bytes = fs::read(entry.path()).unwrap();
        map.insert(name, bytes);
    }
    map
}

#[cfg(unix)]
fn exe_mtime_ms(path: &Path) -> i64 {
    fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn wait_until(timeout: Duration, mut pred: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if pred() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn t16_active_wal_statusline_creates_zero_temp_db_dirs() {
    let env = IsolatedEnv::new();
    let _cleanup = CollectorCleanup {
        runtime_dir: env.runtime_dir.clone(),
        data_dir: env.data_dir.clone(),
    };
    fs::write(
        env.global_cfg_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "language": "en",
            "display": {
                "showContextMonitor": true,
                "showSpeed": true,
                "quotaStyle": "none"
            }
        })
        .to_string(),
    )
    .unwrap();

    let conv_id = "conv-t16-inmem-wal";
    let conv_dir = env.data_dir.join("conversations");
    fs::create_dir_all(&conv_dir).unwrap();
    let db_path = conv_dir.join(format!("{conv_id}.db"));

    // Keep an active uncheckpointed WAL connection open and remove -shm.
    let writer = Connection::open(&db_path).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    writer
        .execute(
            "CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB)",
            [],
        )
        .unwrap();
    writer
        .execute(
            "INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)",
            params![1i64, build_compaction_gen_metadata_blob(180_000)],
        )
        .unwrap();
    writer
        .execute(
            "INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)",
            params![
                2i64,
                build_perf_gen_metadata_blob(
                    1,
                    500,
                    (2, 0),
                    (0, 300_000_000),
                    "claude-opus-4-6-thinking",
                    "Claude Opus 4.6 (Thinking)",
                )
            ],
        )
        .unwrap();
    let shm_path = PathBuf::from(format!("{}-shm", db_path.display()));
    let _ = fs::remove_file(&shm_path);

    let before_conv_dir = snapshot_dir_bytes(&conv_dir);
    let stdin_json = serde_json::json!({
        "conversation_id": conv_id,
        "model": {
            "id": "claude-opus-4-6-thinking",
            "display_name": "Claude Opus 4.6 (Thinking)"
        },
        "context_window": {
            "context_window_size": 200_000,
            "used_percentage": 25.0,
            "total_input_tokens": 50_000,
            "total_output_tokens": 1_000
        }
    })
    .to_string();

    let (code, stdout, stderr, _) = env.run_statusline(
        &stdin_json,
        &[
            ("AGY_PULSE_NO_DASHBOARD_AUTOSTART", "1"),
            ("COLUMNS", "160"),
        ],
    );
    let after_conv_dir = snapshot_dir_bytes(&conv_dir);
    drop(writer);

    assert_eq!(code, 0, "stderr: {stderr}");
    let plain = strip_ansi(&stdout);
    assert!(
        plain.contains("180K"),
        "expected compaction limit 180K from uncheckpointed WAL: {plain}"
    );
    assert!(
        plain.contains("250.0 tok/s"),
        "expected output speed 250.0 tok/s from uncheckpointed WAL: {plain}"
    );

    assert_eq!(
        before_conv_dir, after_conv_dir,
        "conversations/ directory must remain untouched"
    );

    let temp_db_dirs: Vec<_> = fs::read_dir(&env.tmp_dir)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("agy-pulse-db-"))
        .collect();
    assert!(
        temp_db_dirs.is_empty(),
        "expected zero agy-pulse-db-* temp dirs in TMPDIR, found: {temp_db_dirs:?}"
    );
}

#[cfg(unix)]
#[test]
fn t16_thin_statusline_exits_promptly_when_collector_stalls() {
    let _lock = TEST_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let env = IsolatedEnv::new();
    let env_map = env.env_map();
    let bin = agy_pulse_bin();
    let conv_key = "conv-t16-stall";

    let (sock_path, snap_path, state_path) = platform::with_env_overrides(&env_map, || {
        (
            platform::resolve_ipc_socket_path(),
            conversation_snapshot_path(conv_key),
            collector_state_path(),
        )
    });
    if let Some(parent) = sock_path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let _ = fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path).unwrap();

    let server_thread = thread::spawn(move || {
        if let Ok((_stream, _)) = listener.accept() {
            // Simulate a stalled collector / D-state worker by sleeping without replying.
            thread::sleep(Duration::from_secs(3));
        }
    });

    let token = "stall-test-token-123".to_string();
    let state = CollectorStateFile {
        app: COLLECTOR_APP_ID.to_string(),
        pid: std::process::id(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        exe_mtime_ms: exe_mtime_ms(&bin),
        mode: "collector".to_string(),
        socket_path: Some(sock_path.to_string_lossy().into_owned()),
        tcp_port: None,
        token,
        started_at_ms: 1_700_000_000_000,
    };
    fs::write(&state_path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();

    let warm = CollectedStatuslineSnapshot {
        schema_version: COLLECTOR_SNAPSHOT_SCHEMA_VERSION,
        updated_at_ms: 1_700_000_000_000,
        conversation_key: conv_key.to_string(),
        workspace_dir: env.workspace_dir.clone(),
        enabled: true,
        snapshot: PulseSnapshot {
            has_conversation_identity: true,
            model: Some(ModelIdentity {
                id: "old-model".to_string(),
                display_name: "Old Model".to_string(),
                effort: None,
            }),
            workspace: WorkspaceInfo {
                current_dir: "workspace".to_string(),
                git_branch: None,
            },
            cost: CostSnapshot {
                today_usd: Some(3.45),
                ..Default::default()
            },
            server_compaction_limit: Some(180_000),
            ..Default::default()
        },
        config: PulseConfig {
            language: LanguageSetting::En,
            display: DisplayConfig {
                show_context_monitor: Some(true),
                show_daily_cost: Some(true),
                quota_style: QuotaStyle::None,
                ..DisplayConfig::builtin_defaults()
            },
            ..Default::default()
        },
    };
    fs::write(&snap_path, serde_json::to_vec(&warm).unwrap()).unwrap();

    let live_stdin = serde_json::json!({
        "conversation_id": conv_key,
        "model": {
            "id": "gemini-4-carbon-high",
            "display_name": "Gemini 4 Carbon (High)"
        },
        "workspace": {
            "current_dir": env.workspace_dir
        },
        "context_window": {
            "context_window_size": 200_000,
            "used_percentage": 60.0,
            "total_input_tokens": 120_000,
            "total_output_tokens": 500
        }
    })
    .to_string();

    let (code, stdout, stderr, elapsed) = env.run_statusline(
        &live_stdin,
        &[
            ("AGY_PULSE_NO_DASHBOARD_AUTOSTART", "1"),
            ("AGY_PULSE_COLLECTOR_WAIT_MS", "150"),
            ("COLUMNS", "160"),
        ],
    );

    let _ = server_thread.join();
    let _ = fs::remove_file(&sock_path);

    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        elapsed < Duration::from_millis(800),
        "thin statusline must exit <800ms on stalled collector, took {elapsed:?}"
    );
    let plain = strip_ansi(&stdout);
    assert!(
        plain.contains("Gemini 4 Carbon"),
        "expected live model from stdin: {plain}"
    );
    assert!(
        plain.contains("120K/180K"),
        "expected live 120K tokens + cached 180K compaction limit: {plain}"
    );
    assert!(
        plain.contains("$3.45"),
        "expected Today cost from warm snapshot: {plain}"
    );
    assert!(
        plain.contains("180K"),
        "expected compaction limit from warm snapshot: {plain}"
    );
}

#[cfg(unix)]
#[test]
fn t16_thin_statusline_returns_immediately_on_busy_collector() {
    let _lock = TEST_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let env = IsolatedEnv::new();
    let env_map = env.env_map();
    let bin = agy_pulse_bin();
    let conv_key = "conv-t16-busy";

    let (sock_path, snap_path, state_path) = platform::with_env_overrides(&env_map, || {
        (
            platform::resolve_ipc_socket_path(),
            conversation_snapshot_path(conv_key),
            collector_state_path(),
        )
    });
    if let Some(parent) = sock_path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let _ = fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path).unwrap();

    let server_thread = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let _req: CollectorWireRequest = read_frame(&mut stream).unwrap();
            let _ = write_frame(&mut stream, &CollectorWireResponse::Busy);
        }
    });

    let state = CollectorStateFile {
        app: COLLECTOR_APP_ID.to_string(),
        pid: std::process::id(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        exe_mtime_ms: exe_mtime_ms(&bin),
        mode: "collector".to_string(),
        socket_path: Some(sock_path.to_string_lossy().into_owned()),
        tcp_port: None,
        token: "busy-test-token-456".to_string(),
        started_at_ms: 1_700_000_000_000,
    };
    fs::write(&state_path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();

    let warm = CollectedStatuslineSnapshot {
        schema_version: COLLECTOR_SNAPSHOT_SCHEMA_VERSION,
        updated_at_ms: 1_700_000_000_000,
        conversation_key: conv_key.to_string(),
        workspace_dir: env.workspace_dir.clone(),
        enabled: true,
        snapshot: PulseSnapshot {
            has_conversation_identity: true,
            workspace: WorkspaceInfo {
                current_dir: "workspace".to_string(),
                git_branch: None,
            },
            cost: CostSnapshot {
                today_usd: Some(7.89),
                ..Default::default()
            },
            server_compaction_limit: Some(160_000),
            ..Default::default()
        },
        config: PulseConfig {
            language: LanguageSetting::En,
            display: DisplayConfig {
                show_context_monitor: Some(true),
                show_daily_cost: Some(true),
                quota_style: QuotaStyle::None,
                ..DisplayConfig::builtin_defaults()
            },
            ..Default::default()
        },
    };
    fs::write(&snap_path, serde_json::to_vec(&warm).unwrap()).unwrap();

    let live_stdin = serde_json::json!({
        "conversation_id": conv_key,
        "model": {
            "id": "claude-sonnet-4-6",
            "display_name": "Claude Sonnet 4.6 (Thinking)"
        },
        "workspace": {
            "current_dir": env.workspace_dir
        },
        "context_window": {
            "context_window_size": 200_000,
            "used_percentage": 35.0,
            "total_input_tokens": 70_000,
            "total_output_tokens": 400
        }
    })
    .to_string();

    let (code, stdout, stderr, elapsed) = env.run_statusline(
        &live_stdin,
        &[
            ("AGY_PULSE_NO_DASHBOARD_AUTOSTART", "1"),
            ("COLUMNS", "160"),
        ],
    );

    let _ = server_thread.join();
    let _ = fs::remove_file(&sock_path);

    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        elapsed < Duration::from_millis(500),
        "thin statusline must return <500ms on Busy collector, took {elapsed:?}"
    );
    let plain = strip_ansi(&stdout);
    assert!(
        plain.contains("Claude Sonnet 4.6"),
        "expected live model from stdin: {plain}"
    );
    assert!(
        plain.contains("70K/160K"),
        "expected live 70K tokens + cached 160K compaction limit: {plain}"
    );
    assert!(
        plain.contains("$7.89"),
        "expected Today cost from warm snapshot: {plain}"
    );
    assert!(
        plain.contains("160K"),
        "expected compaction limit from warm snapshot: {plain}"
    );
}

#[test]
fn t16_collector_cleans_stale_temp_db_dirs_and_exits_when_data_dir_removed() {
    let _lock = TEST_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let data_temp = TempDir::new().unwrap();
    let scratch_temp = TempDir::new().unwrap();

    let data_dir = data_temp.path().join("data");
    let runtime_dir = data_dir.join("runtime");
    let home_dir = scratch_temp.path().join("home");
    let global_cfg_dir = scratch_temp.path().join("global_cfg");
    let xdg_cfg_dir = scratch_temp.path().join("xdg_cfg");
    let app_data_dir = scratch_temp.path().join("app_data");
    let local_app_data_dir = scratch_temp.path().join("local_app_data");
    let workspace_dir = scratch_temp.path().join("workspace");
    let tmp_dir = scratch_temp.path().join("tmp");
    for d in [
        &data_dir,
        &runtime_dir,
        &home_dir,
        &global_cfg_dir,
        &xdg_cfg_dir,
        &app_data_dir,
        &local_app_data_dir,
        &workspace_dir,
        &tmp_dir,
    ] {
        fs::create_dir_all(d).unwrap();
    }

    let stale = tmp_dir.join("agy-pulse-db-stale");
    let fresh = tmp_dir.join("agy-pulse-db-fresh");
    fs::create_dir_all(&stale).unwrap();
    fs::write(stale.join("copy.db"), b"stale").unwrap();
    fs::create_dir_all(&fresh).unwrap();
    fs::write(fresh.join("copy.db"), b"fresh").unwrap();

    if let Ok(dir_handle) = fs::OpenOptions::new().read(true).open(&stale) {
        let _ = dir_handle.set_modified(SystemTime::now() - Duration::from_secs(900));
    }
    let stale_is_old = fs::metadata(&stale)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age > Duration::from_secs(600));

    let mut cmd = Command::new(agy_pulse_bin());
    cmd.current_dir(&workspace_dir)
        .env("HOME", &home_dir)
        .env("USERPROFILE", &home_dir)
        .env("XDG_DATA_HOME", home_dir.join(".local/share"))
        .env("XDG_CONFIG_HOME", &xdg_cfg_dir)
        .env("APPDATA", &app_data_dir)
        .env("LOCALAPPDATA", &local_app_data_dir)
        .env("AGY_PULSE_DATA_DIR", &data_dir)
        .env("AGY_PULSE_RUNTIME_DIR", &runtime_dir)
        .env("AGY_PULSE_GLOBAL_CONFIG_DIR", &global_cfg_dir)
        .env("AGY_PULSE_NO_DASHBOARD_AUTOSTART", "1")
        .env("AGY_PULSE_DISABLE_UPDATE_CHECK", "1")
        .env("AGY_PULSE_COLLECTOR_IDLE_SEC", "10")
        .env("TMPDIR", &tmp_dir)
        .env("TMP", &tmp_dir)
        .env("TEMP", &tmp_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"plan_tier":"Pro"}"#)
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let mut ov = HashMap::new();
    ov.insert(
        "AGY_PULSE_RUNTIME_DIR".to_string(),
        Some(runtime_dir.to_string_lossy().into_owned()),
    );
    let collector_state = platform::with_env_overrides(&ov, read_collector_state)
        .expect("collector.json must exist after statusline run");
    let pid = collector_state.pid;
    assert!(platform::is_process_alive(pid));

    if stale_is_old {
        assert!(
            wait_until(Duration::from_secs(3), || !stale.exists()),
            "collector must clean stale agy-pulse-db-* directory"
        );
    }
    assert!(
        fresh.exists(),
        "fresh agy-pulse-db-* directory must be preserved"
    );

    // Deleting AGY_PULSE_DATA_DIR must cause collector to exit promptly (<3s) instead of idling 10s.
    fs::remove_dir_all(&data_dir).unwrap();
    assert!(
        wait_until(Duration::from_secs(3), || !platform::is_process_alive(pid)),
        "collector PID {pid} must exit promptly after AGY_PULSE_DATA_DIR removal"
    );
}

#[test]
fn t16_unified_dashboard_takes_over_headless_collector() {
    let _lock = TEST_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let env = IsolatedEnv::new();
    let _cleanup = CollectorCleanup {
        runtime_dir: env.runtime_dir.clone(),
        data_dir: env.data_dir.clone(),
    };
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let port_str = port.to_string();
    let env_map = env.env_map();

    // 1. Start headless collector via statusline with dashboard autostart disabled.
    let (code1, _out1, err1, _) = env.run_statusline(
        r#"{"plan_tier":"Pro"}"#,
        &[
            ("AGY_PULSE_NO_DASHBOARD_AUTOSTART", "1"),
            ("AGY_PULSE_DASHBOARD_PORT", &port_str),
            ("AGY_PULSE_COLLECTOR_IDLE_SEC", "30"),
            ("COLUMNS", "160"),
        ],
    );
    assert_eq!(code1, 0, "stderr: {err1}");

    let state1 = platform::with_env_overrides(&env_map, read_collector_state)
        .expect("headless collector.json must exist");
    assert_eq!(state1.mode, "collector");
    let pid1 = state1.pid;
    assert!(platform::is_process_alive(pid1));

    // 2. Enable dashboard autostart -> dashboard --serve-background takes over collector.json.
    let (code2, out2, err2, _) = env.run_statusline(
        r#"{"plan_tier":"Pro"}"#,
        &[
            ("AGY_PULSE_NO_DASHBOARD_AUTOSTART", "0"),
            ("AGY_PULSE_NO_OPEN", "1"),
            ("AGY_PULSE_DASHBOARD_PORT", &port_str),
            ("AGY_PULSE_DASHBOARD_IDLE_SEC", "5"),
            ("COLUMNS", "160"),
        ],
    );
    assert_eq!(code2, 0, "stderr: {err2}");
    assert!(
        strip_ansi(&out2).contains(&format!("http://127.0.0.1:{port}")),
        "expected dashboard URL in output: {out2}"
    );

    let took_over = wait_until(Duration::from_secs(5), || {
        platform::with_env_overrides(&env_map, read_collector_state).is_some_and(|s| {
            s.mode == "dashboard" && s.pid != pid1 && platform::is_process_alive(s.pid)
        }) && !platform::is_process_alive(pid1)
    });
    assert!(
        took_over,
        "unified dashboard must take over collector.json (mode=dashboard) and retire headless PID {pid1}"
    );

    let state2 = platform::with_env_overrides(&env_map, read_collector_state).unwrap();
    let pid2 = state2.pid;

    // 3. Stop via `agypl dashboard stop` and verify clean exit & state cleanup.
    let stop_out = env
        .base_cmd(&agypl_bin())
        .args(["dashboard", "stop", "--port", &port_str])
        .output()
        .unwrap();
    assert!(
        stop_out.status.success(),
        "dashboard stop failed: {}",
        String::from_utf8_lossy(&stop_out.stderr)
    );
    assert!(
        wait_until(Duration::from_secs(3), || {
            !platform::is_process_alive(pid2)
                && platform::with_env_overrides(&env_map, read_collector_state).is_none()
        }),
        "unified dashboard PID {pid2} and collector.json must clean up after dashboard stop"
    );
}

#[test]
fn t16_sequential_warm_statusline_requests_never_get_spurious_busy() {
    let env = IsolatedEnv::new();
    let _cleanup = CollectorCleanup {
        runtime_dir: env.runtime_dir.clone(),
        data_dir: env.data_dir.clone(),
    };
    fs::write(
        env.global_cfg_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "language": "en",
            "display": {
                "showContextMonitor": true,
                "showModelCache": true,
                "quotaStyle": "none"
            }
        })
        .to_string(),
    )
    .unwrap();

    let transcript_path = env.workspace_dir.join("rapid-seq.jsonl");
    let stdin_payload = serde_json::json!({
        "conversation_id": "conv-t16-rapid-seq",
        "transcript_path": transcript_path.to_string_lossy(),
        "model": { "id": "gemini-4-carbon-high", "display_name": "Gemini 4 Carbon (High)" }
    })
    .to_string();

    for turn in 1..=12u64 {
        let entry = serde_json::json!({
            "step_index": turn,
            "type": "PLANNER_RESPONSE",
            "created_at": format!("2026-11-28T00:00:{turn:02}Z"),
            "model": "gemini-4-carbon-high",
            "context_window": {
                "context_window_size": 500_000,
                "current_usage": {
                    "input_tokens": 10_000,
                    "output_tokens": 75,
                    "cache_read_input_tokens": 5_000,
                    "cache_creation_input_tokens": 0
                }
            }
        });
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&transcript_path)
            .unwrap();
        writeln!(f, "{entry}").unwrap();
        drop(f);

        let (code, stdout, stderr, _) = env.run_statusline(
            &stdin_payload,
            &[
                ("AGY_PULSE_NO_DASHBOARD_AUTOSTART", "1"),
                ("COLUMNS", "140"),
            ],
        );
        assert_eq!(code, 0, "turn {turn} stderr: {stderr}");
        let plain = strip_ansi(&stdout);
        let expected_out = format!("Out {}", turn * 75);
        assert!(
            plain.contains(&expected_out),
            "turn {turn} must not fall back to stale warm snapshot (expected {expected_out}):\n{plain}"
        );
    }
}

#[cfg(unix)]
#[test]
fn t16_embedded_collector_takes_over_existing_embedded_instance() {
    use std::sync::mpsc;

    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let _lock = TEST_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let env = IsolatedEnv::new();
    let env_map = env.env_map();
    let _cleanup = CollectorCleanup {
        runtime_dir: env.runtime_dir.clone(),
        data_dir: env.data_dir.clone(),
    };
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let port_str = port.to_string();

    let dummy = ChildGuard(Command::new("sleep").arg("10").spawn().unwrap());
    let dummy_pid = dummy.0.id();

    let old_sock = env.runtime_dir.join("old-embedded.sock");
    let _ = fs::remove_file(&old_sock);
    let listener = UnixListener::bind(&old_sock).unwrap();
    let (tx, rx) = mpsc::channel::<bool>();
    let old_server = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept()
            && let Ok(CollectorWireRequest::Shutdown { token }) = read_frame(&mut stream)
        {
            let _ = write_frame(&mut stream, &CollectorWireResponse::Ack);
            let _ = tx.send(token == "old-embedded-token");
        }
    });

    let state_path = platform::with_env_overrides(&env_map, collector_state_path);
    let old_state = CollectorStateFile {
        app: COLLECTOR_APP_ID.to_string(),
        pid: dummy_pid,
        version: env!("CARGO_PKG_VERSION").to_string(),
        exe_mtime_ms: 0,
        mode: "dashboard".to_string(),
        socket_path: Some(old_sock.to_string_lossy().into_owned()),
        tcp_port: None,
        token: "old-embedded-token".to_string(),
        started_at_ms: 1_700_000_000_000,
    };
    fs::write(&state_path, serde_json::to_vec_pretty(&old_state).unwrap()).unwrap();

    let serve_out = env
        .base_cmd(&agy_pulse_bin())
        .args([
            "dashboard",
            "--serve-background",
            "--port",
            &port_str,
            "--idle-timeout",
            "10",
            "--no-open",
        ])
        .env("AGY_PULSE_NO_OPEN", "1")
        .output()
        .unwrap();
    assert!(
        serve_out.status.success(),
        "dashboard --serve-background failed: {}",
        String::from_utf8_lossy(&serve_out.stderr)
    );

    let got_shutdown = rx.recv_timeout(Duration::from_secs(2)).unwrap_or(false);
    let _ = env
        .base_cmd(&agypl_bin())
        .args(["dashboard", "stop", "--port", &port_str])
        .output();
    let _ = fs::remove_file(&old_sock);
    drop(old_server);

    assert!(
        got_shutdown,
        "start_embedded must send Shutdown takeover to an alive existing embedded collector"
    );
}

#[cfg(unix)]
#[test]
fn t16_slow_ipc_clients_do_not_block_normal_ping_requests() {
    use std::os::unix::net::UnixStream;

    let _lock = TEST_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let env = IsolatedEnv::new();
    let env_map = env.env_map();
    let _cleanup = CollectorCleanup {
        runtime_dir: env.runtime_dir.clone(),
        data_dir: env.data_dir.clone(),
    };

    let (code, _out, err, _) = env.run_statusline(
        r#"{"plan_tier":"Pro"}"#,
        &[
            ("AGY_PULSE_NO_DASHBOARD_AUTOSTART", "1"),
            ("AGY_PULSE_COLLECTOR_IDLE_SEC", "10"),
            ("COLUMNS", "160"),
        ],
    );
    assert_eq!(code, 0, "stderr: {err}");

    let state = platform::with_env_overrides(&env_map, read_collector_state)
        .expect("collector.json must exist");
    let sock_path = state
        .socket_path
        .as_deref()
        .expect("unix collector state must have socket_path");

    // Open two stalled slow clients that send a frame length prefix without completing the body.
    let mut slow_streams = Vec::new();
    for _ in 0..2 {
        let mut s = UnixStream::connect(sock_path).unwrap();
        s.write_all(&128u32.to_be_bytes()).unwrap();
        s.write_all(b"{").unwrap();
        s.flush().unwrap();
        slow_streams.push(s);
    }

    // Give the accept loop a moment to accept the slow clients, then send a normal Ping.
    thread::sleep(Duration::from_millis(20));
    let ping_start = Instant::now();
    let mut ping_stream = UnixStream::connect(sock_path).unwrap();
    ping_stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    ping_stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    write_frame(
        &mut ping_stream,
        &CollectorWireRequest::Ping {
            token: state.token.clone(),
        },
    )
    .unwrap();
    let pong: CollectorWireResponse = read_frame(&mut ping_stream).unwrap();
    let ping_elapsed = ping_start.elapsed();

    drop(slow_streams);
    let mut shutdown_stream = UnixStream::connect(sock_path).unwrap();
    let _ = write_frame(
        &mut shutdown_stream,
        &CollectorWireRequest::Shutdown {
            token: state.token.clone(),
        },
    );

    assert!(
        matches!(pong, CollectorWireResponse::Pong { .. }),
        "expected Pong response, got {pong:?}"
    );
    assert!(
        ping_elapsed < Duration::from_millis(120),
        "normal Ping must not be blocked behind stalled slow IPC clients (took {ping_elapsed:?})"
    );
}
