#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use rusqlite::{Connection, params};
use tempfile::TempDir;

use agy_pulse::domain::{
    ProviderQuota, ProviderQuotaSummary, QuotaSnapshot, QuotaWindow, SyncDeviceRecord,
};
use agy_pulse::ledger::{LedgerSource, LedgerStore, LedgerSyncOptions, SourceRoot};
use agy_pulse::sync::{RemoteConfig, SyncConfig, load_sync_config_from};

fn agy_pulse_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agy-pulse"))
}

fn agypl_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agypl"))
}

struct IsolatedCliEnv {
    _temp: TempDir,
    home_dir: PathBuf,
    data_dir: PathBuf,
    global_cfg_dir: PathBuf,
    xdg_cfg_dir: PathBuf,
    app_data_dir: PathBuf,
    local_app_data_dir: PathBuf,
    workspace_dir: PathBuf,
    sync_cfg_path: PathBuf,
}

impl IsolatedCliEnv {
    fn new() -> Self {
        let temp = TempDir::new().expect("create temp dir");
        let root = temp.path();
        let home_dir = root.join("home");
        let data_dir = root.join("data");
        let global_cfg_dir = root.join("global_cfg");
        let xdg_cfg_dir = root.join("xdg_cfg");
        let app_data_dir = root.join("app_data");
        let local_app_data_dir = root.join("local_app_data");
        let workspace_dir = root.join("workspace");
        let sync_cfg_path = root.join("sync_cfg/agy-pulse-sync.json");

        fs::create_dir_all(&home_dir).unwrap();
        fs::create_dir_all(&data_dir).unwrap();
        fs::create_dir_all(&global_cfg_dir).unwrap();
        fs::create_dir_all(&xdg_cfg_dir).unwrap();
        fs::create_dir_all(&app_data_dir).unwrap();
        fs::create_dir_all(&local_app_data_dir).unwrap();
        fs::create_dir_all(&workspace_dir).unwrap();
        fs::create_dir_all(sync_cfg_path.parent().unwrap()).unwrap();

        Self {
            _temp: temp,
            home_dir,
            data_dir,
            global_cfg_dir,
            xdg_cfg_dir,
            app_data_dir,
            local_app_data_dir,
            workspace_dir,
            sync_cfg_path,
        }
    }

    fn cmd(&self, bin: &Path) -> Command {
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
            .env("AGY_PULSE_GLOBAL_CONFIG_DIR", &self.global_cfg_dir)
            .env("AGY_PULSE_SYNC_CONFIG", &self.sync_cfg_path)
            .env("AGY_PULSE_NO_DASHBOARD_AUTOSTART", "1")
            .env("AGY_PULSE_DISABLE_UPDATE_CHECK", "1")
            .env_remove("AGY_PULSE_SYNC_TOKEN")
            .env_remove("AGY_PULSE_SYNC_PASSWORD")
            .env_remove("AGY_PULSE_S3_ACCESS_KEY")
            .env_remove("AGY_PULSE_S3_SECRET_KEY")
            .env_remove("AGY_PULSE_NO_SYNC_AUTOSTART")
            .env("LC_ALL", "en_US.UTF-8")
            .env("LANG", "en_US.UTF-8");
        cmd
    }

    fn run_with_stdin(&self, bin: &Path, args: &[&str], stdin_text: &str) -> (i32, String, String) {
        let mut child = self
            .cmd(bin)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn binary");

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(stdin_text.as_bytes()).unwrap();
        }
        let output = child.wait_with_output().expect("wait output");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    fn ledger_path(&self) -> PathBuf {
        self.data_dir.join("agy-pulse-ledger.sqlite")
    }

    fn open_ledger(&self) -> LedgerStore {
        LedgerStore::open_at(&self.ledger_path()).unwrap()
    }
}

#[derive(Debug, Clone)]
struct RecordedRequest {
    method: String,
    target: String,
    headers: BTreeMap<String, String>,
    #[allow(dead_code)]
    body: Vec<u8>,
}

struct HttpResponseSpec {
    status: u16,
    reason: &'static str,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpResponseSpec {
    fn new(status: u16, reason: &'static str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            reason,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

struct MockHttpServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl MockHttpServer {
    fn start<F>(handler: F) -> Self
    where
        F: Fn(&RecordedRequest) -> HttpResponseSpec + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let handler = Arc::new(handler);

        let handle = thread::spawn(move || {
            while !stop_flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                        handle_connection(stream, handler.as_ref());
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            addr,
            stop,
            handle: Some(handle),
        }
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

impl Drop for MockHttpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn handle_connection<F>(mut stream: TcpStream, handler: &F)
where
    F: Fn(&RecordedRequest) -> HttpResponseSpec,
{
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    let parts: Vec<&str> = request_line
        .trim_end_matches(['\r', '\n'])
        .split_whitespace()
        .collect();
    if parts.len() < 2 {
        return;
    }
    let method = parts[0].to_string();
    let target = parts[1].to_string();

    let mut headers = BTreeMap::new();
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            let key_lower = k.trim().to_ascii_lowercase();
            let val = v.trim().to_string();
            if key_lower == "content-length" {
                content_length = val.parse::<usize>().unwrap_or(0);
            }
            headers.insert(key_lower, val);
        }
    }

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        let _ = reader.read_exact(&mut body);
    }

    let req = RecordedRequest {
        method,
        target,
        headers,
        body,
    };
    let resp = handler(&req);
    let mut out = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        resp.status,
        resp.reason,
        resp.body.len()
    );
    for (k, v) in &resp.headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    let _ = stream.write_all(out.as_bytes());
    let _ = stream.write_all(&resp.body);
    let _ = stream.flush();
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

fn pb_duration_sec(field_num: u32, sec: u64) -> Vec<u8> {
    let inner = if sec > 0 {
        pb_varint(1, sec)
    } else {
        Vec::new()
    };
    pb_bytes(field_num, &inner)
}

fn seed_conv(
    root: &Path,
    conv_id: &str,
    workspace: &str,
    branch: &str,
    ts_ms: i64,
    input_tokens: u64,
    output_tokens: u64,
) {
    let db_path = root.join("conversations").join(format!("{conv_id}.db"));
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let conn = Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS steps (idx INTEGER PRIMARY KEY, metadata BLOB);
         CREATE TABLE IF NOT EXISTS gen_metadata (idx INTEGER PRIMARY KEY, data BLOB);
         CREATE TABLE IF NOT EXISTS trajectory_metadata_blob (id TEXT PRIMARY KEY, data BLOB);",
    )
    .unwrap();

    let step_meta = pb_timestamp_ms(1, ts_ms);
    conn.execute(
        "INSERT INTO steps (idx, metadata) VALUES (?1, ?2)",
        params![1i64, step_meta],
    )
    .unwrap();

    let mut gen_blob = pb_varint(2, 1);
    let mut f1 = pb_str(21, "Claude Sonnet 4.5");
    let mut usage = pb_varint(2, input_tokens);
    usage.extend(pb_varint(3, output_tokens));
    f1.extend(pb_bytes(4, &usage));
    f1.extend(pb_duration_sec(11, 1));
    f1.extend(pb_duration_sec(12, 5));
    gen_blob.extend(pb_bytes(1, &f1));
    conn.execute(
        "INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)",
        params![1i64, gen_blob],
    )
    .unwrap();

    let mut ws = pb_str(1, workspace);
    ws.extend(pb_str(4, branch));
    let mut traj = pb_bytes(1, &ws);
    traj.extend(pb_timestamp_ms(2, ts_ms));
    traj.extend(pb_str(6, conv_id));
    conn.execute(
        "INSERT INTO trajectory_metadata_blob (id, data) VALUES ('main', ?1)",
        params![traj],
    )
    .unwrap();
}

#[test]
fn ledger_sync_devices_and_pending_changes_behave_accurately() {
    let env = IsolatedCliEnv::new();
    let store = env.open_ledger();
    let local_id = store.local_device_id().unwrap();

    let initial_devices = store.list_sync_devices().unwrap();
    assert_eq!(initial_devices.len(), 1);
    assert_eq!(initial_devices[0].account_id, "unassigned");
    assert_eq!(initial_devices[0].device_id, local_id);
    assert!(initial_devices[0].is_local);
    assert!(!store.has_pending_local_sync_changes(None).unwrap());
    assert!(
        !store
            .has_pending_local_sync_changes(Some(1_000_000))
            .unwrap()
    );

    let ts1 = 1_795_000_000_000i64;
    store
        .upsert_quota_snapshot(&QuotaSnapshot {
            account_id: "alice@example.com".to_string(),
            device_id: local_id.clone(),
            device_name: "local-box".to_string(),
            plan_tier: Some("Ultra".to_string()),
            observed_at_ms: ts1,
            quota: ProviderQuotaSummary {
                google: ProviderQuota {
                    five_hour: Some(QuotaWindow {
                        remaining_fraction: 0.9,
                        reset_time_ms: Some(ts1 + 3_600_000),
                    }),
                    weekly: None,
                },
                ..Default::default()
            },
        })
        .unwrap();
    store
        .upsert_sync_device(
            "alice@example.com",
            &local_id,
            "local-box",
            true,
            ts1,
            Some("Ultra"),
        )
        .unwrap();
    store
        .upsert_sync_device(
            "alice@example.com",
            "dev_remote_01",
            "remote-box",
            false,
            ts1 - 1000,
            Some("Pro"),
        )
        .unwrap();

    let devices = store.list_sync_devices().unwrap();
    assert_eq!(
        devices,
        vec![
            SyncDeviceRecord {
                account_id: "alice@example.com".to_string(),
                device_id: local_id,
                device_name: "local-box".to_string(),
                is_local: true,
                last_seen_ms: ts1,
                plan_tier: Some("Ultra".to_string()),
            },
            SyncDeviceRecord {
                account_id: "alice@example.com".to_string(),
                device_id: "dev_remote_01".to_string(),
                device_name: "remote-box".to_string(),
                is_local: false,
                last_seen_ms: ts1 - 1000,
                plan_tier: Some("Pro".to_string()),
            },
        ]
    );

    assert!(store.has_pending_local_sync_changes(None).unwrap());
    assert!(!store.has_pending_local_sync_changes(Some(ts1)).unwrap());
    assert!(store.has_pending_local_sync_changes(Some(ts1 - 1)).unwrap());
}

#[test]
fn sync_init_supports_flags_and_interactive_probe_and_protects_existing_on_401() {
    let env = IsolatedCliEnv::new();
    let local_remote_dir = env._temp.path().join("sync-remote");

    // 1) Interactive stdin setup with local-dir
    let interactive_stdin = format!(
        "local-dir\n{}\nlaptop-alpha\nAlice@Example.COM\nwork-prefix\n120\n",
        local_remote_dir.display()
    );
    let (code, stdout, stderr) =
        env.run_with_stdin(&agy_pulse_bin(), &["sync", "init"], &interactive_stdin);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.contains("Sync configuration saved"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("local-dir:"), "stdout: {stdout}");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&env.sync_cfg_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "sync config must be 0600, got {:o}", mode);
    }

    let initial_cfg = load_sync_config_from(&env.sync_cfg_path).unwrap().unwrap();
    assert!(initial_cfg.enabled);
    assert_eq!(initial_cfg.device_name.as_deref(), Some("laptop-alpha"));
    assert_eq!(
        initial_cfg.default_account.as_deref(),
        Some("alice@example.com")
    );
    assert_eq!(initial_cfg.prefix, "work-prefix");
    assert_eq!(initial_cfg.push_interval_sec, 120);
    assert!(initial_cfg.device_id.is_some());

    // 2) Mock CF Worker server: 401 probe failure must NOT overwrite existing config or leak token
    let secret_tok = "tok_super_secret_401_test";
    let fail_server = MockHttpServer::start(|_req| {
        HttpResponseSpec::new(401, "Unauthorized", b"unauthorized".to_vec())
    });
    let fail_out = env
        .cmd(&agy_pulse_bin())
        .args([
            "sync",
            "init",
            "--type",
            "cf-worker",
            "--url",
            &fail_server.base_url(),
            "--token",
            secret_tok,
        ])
        .output()
        .unwrap();
    assert_eq!(fail_out.status.code(), Some(1));
    let fail_stdout = String::from_utf8_lossy(&fail_out.stdout);
    let fail_stderr = String::from_utf8_lossy(&fail_out.stderr);
    assert!(!fail_stdout.contains(secret_tok), "stdout leaked secret");
    assert!(!fail_stderr.contains(secret_tok), "stderr leaked secret");
    assert!(
        fail_stderr.contains("401") || fail_stderr.contains("authentication"),
        "expected auth error, got: {fail_stderr}"
    );

    let after_fail_cfg = load_sync_config_from(&env.sync_cfg_path).unwrap().unwrap();
    assert_eq!(
        after_fail_cfg, initial_cfg,
        "401 must not overwrite existing config"
    );

    // 3) Mock CF Worker server: 200 health probe succeeds and writes 0600 config without leaking token
    let ok_tok = "tok_valid_worker_secret";
    let ok_server = MockHttpServer::start(move |req| {
        if req.method == "GET"
            && req.target == "/v1/health"
            && req.headers.get("authorization").map(String::as_str)
                == Some("Bearer tok_valid_worker_secret")
        {
            HttpResponseSpec::new(200, "OK", br#"{"ok":true,"storage":"r2"}"#.to_vec())
                .with_header("Content-Type", "application/json")
        } else {
            HttpResponseSpec::new(401, "Unauthorized", b"denied".to_vec())
        }
    });

    let ok_out = env
        .cmd(&agypl_bin())
        .args([
            "sync",
            "init",
            "--type",
            "cf-worker",
            "--url",
            &ok_server.base_url(),
            "--token",
            ok_tok,
            "--device-name",
            "worker-node",
            "--default-account",
            "bob@example.com",
        ])
        .output()
        .unwrap();
    assert_eq!(
        ok_out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&ok_out.stderr)
    );
    let ok_stdout = String::from_utf8_lossy(&ok_out.stdout);
    assert!(!ok_stdout.contains(ok_tok), "stdout must never echo token");
    assert!(ok_stdout.contains("cf-worker:"));
    assert!(ok_stdout.contains("probe:       ok"));

    let saved = load_sync_config_from(&env.sync_cfg_path).unwrap().unwrap();
    assert_eq!(
        saved.remote,
        RemoteConfig::CfWorker {
            url: ok_server.base_url(),
            token: Some(ok_tok.to_string()),
            allow_insecure_http: false,
        }
    );
    assert_eq!(saved.device_id, initial_cfg.device_id);
}

#[test]
fn two_machine_cli_sync_dry_run_push_pull_status_and_prune_device() {
    let remote_root = TempDir::new().unwrap();
    let machine_a = IsolatedCliEnv::new();
    let machine_b = IsolatedCliEnv::new();

    // Unconfigured sync returns exit code 1 with guidance
    let uncfg = machine_a
        .cmd(&agy_pulse_bin())
        .arg("sync")
        .output()
        .unwrap();
    assert_eq!(uncfg.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&uncfg.stderr).contains("sync init"),
        "expected sync init guidance"
    );

    // Conflicting --push and --pull returns exit code 2
    let conflict = machine_a
        .cmd(&agy_pulse_bin())
        .args(["sync", "--push", "--pull"])
        .output()
        .unwrap();
    assert_eq!(conflict.status.code(), Some(2));

    // Unknown flag returns exit code 2
    let unknown = machine_a
        .cmd(&agy_pulse_bin())
        .args(["sync", "--bogus-flag"])
        .output()
        .unwrap();
    assert_eq!(unknown.status.code(), Some(2));

    // Initialize both machines pointing to shared local-dir
    for (env, name, email) in [
        (&machine_a, "dev-alpha", "alice@example.com"),
        (&machine_b, "dev-beta", "alice@example.com"),
    ] {
        let out = env
            .cmd(&agy_pulse_bin())
            .args([
                "sync",
                "init",
                "--type",
                "local-dir",
                "--path",
                remote_root.path().to_str().unwrap(),
                "--device-name",
                name,
                "--default-account",
                email,
            ])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0));
    }

    // Seed machine A conversation and quota
    let ts_a = "2026-11-10T10:00:00Z"
        .parse::<Timestamp>()
        .unwrap()
        .as_millisecond();
    seed_conv(
        &machine_a.data_dir,
        "conv-alpha-01",
        "/secret/workspace/alpha",
        "feat/secret",
        ts_a,
        10_000,
        2_000,
    );
    {
        let store_a = machine_a.open_ledger();
        let dev_a = store_a.local_device_id().unwrap();
        store_a
            .upsert_quota_snapshot(&QuotaSnapshot {
                account_id: "alice@example.com".to_string(),
                device_id: dev_a.clone(),
                device_name: "dev-alpha".to_string(),
                plan_tier: Some("Ultra".to_string()),
                observed_at_ms: ts_a,
                quota: ProviderQuotaSummary {
                    google: ProviderQuota {
                        five_hour: Some(QuotaWindow {
                            remaining_fraction: 0.88,
                            reset_time_ms: Some(ts_a + 3_600_000),
                        }),
                        weekly: None,
                    },
                    ..Default::default()
                },
            })
            .unwrap();
        store_a
            .upsert_sync_device(
                "alice@example.com",
                &dev_a,
                "dev-alpha",
                true,
                ts_a,
                Some("Ultra"),
            )
            .unwrap();
        store_a
            .set_last_observed_email("alice@example.com")
            .unwrap();
    }

    // Dry-run push on machine A before local scan must skip local DB write and remote write
    let dry_before_scan = machine_a
        .cmd(&agy_pulse_bin())
        .args(["sync", "--push", "--dry-run"])
        .output()
        .unwrap();
    assert_eq!(dry_before_scan.status.code(), Some(0));
    let dry_before_out = String::from_utf8_lossy(&dry_before_scan.stdout);
    assert!(dry_before_out.contains("[dry-run]"));
    assert!(dry_before_out.contains("chunks uploaded:    0"));
    assert!(!remote_root.path().join("agy-pulse-sync").exists());

    // Ingest local DB on machine A, then verify --dry-run previews chunk upload without writing remote
    {
        let mut store_a = machine_a.open_ledger();
        store_a
            .sync_all(&LedgerSyncOptions {
                source_roots: Some(vec![SourceRoot {
                    root: machine_a.data_dir.clone(),
                    source: LedgerSource::Cli,
                }]),
                default_account_id: Some("alice@example.com".to_string()),
                device_name: Some("dev-alpha".to_string()),
                ..Default::default()
            })
            .unwrap();
    }

    let dry_push = machine_a
        .cmd(&agy_pulse_bin())
        .args(["sync", "--push", "--dry-run"])
        .output()
        .unwrap();
    assert_eq!(dry_push.status.code(), Some(0));
    let dry_out = String::from_utf8_lossy(&dry_push.stdout);
    assert!(dry_out.contains("[dry-run]"));
    assert!(dry_out.contains("chunks uploaded:    1"));
    assert!(!remote_root.path().join("agy-pulse-sync").exists());

    // Real push on machine A
    let push_a = machine_a
        .cmd(&agy_pulse_bin())
        .args(["sync", "--push"])
        .output()
        .unwrap();
    assert_eq!(push_a.status.code(), Some(0));
    let push_a_out = String::from_utf8_lossy(&push_a.stdout);
    assert!(push_a_out.contains("chunks uploaded:    1"));

    // Pull on machine B
    let pull_b = machine_b
        .cmd(&agy_pulse_bin())
        .args(["sync", "--pull"])
        .output()
        .unwrap();
    assert_eq!(pull_b.status.code(), Some(0));
    let pull_b_out = String::from_utf8_lossy(&pull_b.stdout);
    assert!(pull_b_out.contains("chunks downloaded:  1"));
    assert!(pull_b_out.contains("steps imported:     1"));

    // Seed machine B conversation and run bidirectional sync on B, then A
    let ts_b = "2026-11-11T12:00:00Z"
        .parse::<Timestamp>()
        .unwrap()
        .as_millisecond();
    seed_conv(
        &machine_b.data_dir,
        "conv-beta-01",
        "/secret/workspace/beta",
        "main",
        ts_b,
        5_000,
        1_000,
    );
    let sync_b = machine_b.cmd(&agypl_bin()).arg("sync").output().unwrap();
    assert_eq!(sync_b.status.code(), Some(0));
    let sync_a = machine_a.cmd(&agypl_bin()).arg("sync").output().unwrap();
    assert_eq!(sync_a.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&sync_a.stdout).contains("steps imported:     1"));

    // Write a fake secret into machine A's config (as a WebDAV/CF token test via status)
    // First test sync status on machine A with real dual-device ledger
    let status_a = machine_a
        .cmd(&agy_pulse_bin())
        .args(["sync", "status"])
        .output()
        .unwrap();
    assert_eq!(status_a.status.code(), Some(0));
    let status_text = String::from_utf8_lossy(&status_a.stdout);
    assert!(status_text.contains("Enabled:             true"));
    assert!(status_text.contains("dev-alpha"));
    assert!(status_text.contains("dev-beta"));
    assert!(status_text.contains("[local]"));
    assert!(status_text.contains("[plan: Ultra]"));
    assert!(status_text.contains("alice@example.com"));

    // Ensure sync status redacts secrets when using token/password/secret_key
    let secret_env = IsolatedCliEnv::new();
    fs::write(
        &secret_env.sync_cfg_path,
        serde_json::json!({
            "enabled": true,
            "deviceName": "sec-node",
            "remote": {
                "type": "s3",
                "endpoint": "https://user:urlpass@s3.example.com?X-Token=leak",
                "region": "auto",
                "bucket": "pulse-bkt",
                "accessKey": "AKIA_SECRET_KEY_123",
                "secretKey": "SK_SUPER_SECRET_456"
            }
        })
        .to_string(),
    )
    .unwrap();
    let sec_status = secret_env
        .cmd(&agy_pulse_bin())
        .args(["sync", "status"])
        .output()
        .unwrap();
    assert_eq!(sec_status.status.code(), Some(0));
    let sec_out = String::from_utf8_lossy(&sec_status.stdout);
    assert!(!sec_out.contains("AKIA_SECRET_KEY_123"));
    assert!(!sec_out.contains("SK_SUPER_SECRET_456"));
    assert!(!sec_out.contains("urlpass"));
    assert!(sec_out.contains("s3:https://s3.example.com/pulse-bkt"));

    // Prune-device: missing arg -> 2; local device without --force -> 2; remote device -> 0
    let prune_missing = machine_a
        .cmd(&agy_pulse_bin())
        .args(["sync", "prune-device"])
        .output()
        .unwrap();
    assert_eq!(prune_missing.status.code(), Some(2));

    let dev_a = machine_a.open_ledger().local_device_id().unwrap();
    let dev_b = machine_b.open_ledger().local_device_id().unwrap();

    let prune_local_denied = machine_a
        .cmd(&agy_pulse_bin())
        .args(["sync", "prune-device", &dev_a])
        .output()
        .unwrap();
    assert_eq!(prune_local_denied.status.code(), Some(2));

    let prune_remote = machine_a
        .cmd(&agy_pulse_bin())
        .args(["sync", "prune-device", &dev_b])
        .output()
        .unwrap();
    assert_eq!(
        prune_remote.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&prune_remote.stderr)
    );
    let prune_out = String::from_utf8_lossy(&prune_remote.stdout);
    assert!(prune_out.contains("remote objects deleted: 2"));

    let devs_after = machine_a.open_ledger().list_sync_devices().unwrap();
    assert!(devs_after.iter().all(|d| d.device_id != dev_b));
}

#[test]
fn sync_bg_exits_zero_and_records_redacted_error_when_remote_unreachable() {
    let env = IsolatedCliEnv::new();
    // Bind and drop to obtain an unused unreachable loopback port.
    let closed_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_addr = closed_listener.local_addr().unwrap();
    drop(closed_listener);

    let secret_tok = "tok_dead_port_secret_999";
    let mut cfg = SyncConfig {
        enabled: true,
        device_id: None,
        device_name: Some("bg-err-node".to_string()),
        default_account: Some("alice@example.com".to_string()),
        push_interval_sec: 1,
        prefix: "agy-pulse-sync".to_string(),
        remote: RemoteConfig::CfWorker {
            url: format!("http://{dead_addr}"),
            token: Some(secret_tok.to_string()),
            allow_insecure_http: false,
        },
    };
    cfg.normalize();

    let ts = 1_795_100_000_000i64;
    let store = env.open_ledger();
    let dev_id = store.local_device_id().unwrap();
    cfg.device_id = Some(dev_id.clone());
    fs::write(
        &env.sync_cfg_path,
        serde_json::to_string_pretty(&cfg).unwrap(),
    )
    .unwrap();

    store
        .upsert_quota_snapshot(&QuotaSnapshot {
            account_id: "alice@example.com".to_string(),
            device_id: dev_id,
            device_name: "bg-err-node".to_string(),
            plan_tier: Some("Pro".to_string()),
            observed_at_ms: ts,
            quota: ProviderQuotaSummary::default(),
        })
        .unwrap();

    let out = env.cmd(&agy_pulse_bin()).arg("--sync-bg").output().unwrap();
    assert_eq!(out.status.code(), Some(0), "--sync-bg must always exit 0");

    let meta = env.open_ledger().get_sync_meta().unwrap();
    let err_msg = meta
        .last_sync_error
        .expect("last_sync_error must be recorded");
    assert!(
        !err_msg.contains(secret_tok),
        "error leaked secret: {err_msg}"
    );

    let err_log = fs::read_to_string(env.data_dir.join("agy-pulse-error.log")).unwrap_or_default();
    assert!(
        !err_log.contains(secret_tok),
        "log leaked secret: {err_log}"
    );
}

#[test]
fn statusline_spawns_sync_bg_respecting_throttle_and_no_autostart_env() {
    let env = IsolatedCliEnv::new();
    let remote_dir = env._temp.path().join("statusline-sync-remote");

    let init_out = env
        .cmd(&agy_pulse_bin())
        .args([
            "sync",
            "init",
            "--type",
            "local-dir",
            "--path",
            remote_dir.to_str().unwrap(),
            "--device-name",
            "status-box",
            "--default-account",
            "alice@example.com",
            "--push-interval",
            "1",
        ])
        .output()
        .unwrap();
    assert_eq!(init_out.status.code(), Some(0));

    let local_device_id = env.open_ledger().local_device_id().unwrap();
    let manifest_path = remote_dir
        .join("agy-pulse-sync/accounts/alice@example.com/devices")
        .join(&local_device_id)
        .join("manifest.json");

    let now_ts = Timestamp::now().as_millisecond();
    seed_conv(
        &env.data_dir,
        "conv-status-bg",
        "/tmp/proj",
        "main",
        now_ts,
        1_000,
        200,
    );

    let payload = serde_json::json!({
        "conversation_id": "conv-status-bg",
        "email": "alice@example.com",
        "plan_tier": "Ultra",
        "model": { "display_name": "Claude Opus 4.6 (Thinking)" },
        "cost": { "total_cost_usd": 0.12 },
        "quota": {
            "google": {
                "five_hour": { "remaining_fraction": 0.95, "reset_seconds": 3600 }
            }
        }
    })
    .to_string();

    // 1) Auto-push enabled (AGY_PULSE_NO_SYNC_AUTOSTART is NOT set): statusline returns fast with exit 0
    // and detached --sync-bg pushes manifest.json into remote_dir.
    let started = Instant::now();
    let (code, stdout, stderr) = env.run_with_stdin(&agy_pulse_bin(), &[], &payload);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(!stdout.trim().is_empty());
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "statusline must remain fast and non-blocking"
    );

    let wait_deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < wait_deadline {
        if manifest_path.is_file() {
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        manifest_path.is_file(),
        "expected background --sync-bg to push {}",
        manifest_path.display()
    );

    // 2) Within throttle window (pushIntervalSec = 3600), deleting manifest & sending new statusline
    // must NOT trigger another --sync-bg push.
    let mut cfg = load_sync_config_from(&env.sync_cfg_path).unwrap().unwrap();
    cfg.push_interval_sec = 3600;
    fs::write(
        &env.sync_cfg_path,
        serde_json::to_string_pretty(&cfg).unwrap(),
    )
    .unwrap();
    fs::remove_file(&manifest_path).unwrap();

    let (code2, _, _) = env.run_with_stdin(&agy_pulse_bin(), &[], &payload);
    assert_eq!(code2, 0);
    thread::sleep(Duration::from_millis(350));
    assert!(
        !manifest_path.exists(),
        "throttled window must not spawn --sync-bg"
    );

    // 3) Even with pushIntervalSec=1 and cleared last_sync_push_ms, AGY_PULSE_NO_SYNC_AUTOSTART=1
    // prevents spawning --sync-bg.
    cfg.push_interval_sec = 1;
    fs::write(
        &env.sync_cfg_path,
        serde_json::to_string_pretty(&cfg).unwrap(),
    )
    .unwrap();
    env.open_ledger()
        .set_sync_meta_field("last_sync_push_ms", None)
        .unwrap();

    let mut child = env
        .cmd(&agy_pulse_bin())
        .env("AGY_PULSE_NO_SYNC_AUTOSTART", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out3 = child.wait_with_output().unwrap();
    assert_eq!(out3.status.code(), Some(0));
    thread::sleep(Duration::from_millis(350));
    assert!(
        !manifest_path.exists(),
        "AGY_PULSE_NO_SYNC_AUTOSTART=1 must suppress --sync-bg"
    );
}

#[test]
fn cli_subcommand_routing_isolates_flags_per_subcommand() {
    let env = IsolatedCliEnv::new();

    // 1. Subcommands must not be hijacked by trailing global flags like `--stats` or `--version`.
    let sync_stats = env
        .cmd(&agypl_bin())
        .args(["sync", "--stats"])
        .output()
        .unwrap();
    assert_ne!(
        sync_stats.status.code(),
        Some(0),
        "agypl sync --stats must route to sync and fail, not stats; stdout={}",
        String::from_utf8_lossy(&sync_stats.stdout)
    );
    let sync_stats_err = String::from_utf8_lossy(&sync_stats.stderr);
    assert!(
        sync_stats_err.contains("agy-pulse sync"),
        "expected sync error message, got stderr: {sync_stats_err}"
    );

    let acct_ver = env
        .cmd(&agypl_bin())
        .args(["account", "--version"])
        .output()
        .unwrap();
    assert_eq!(
        acct_ver.status.code(),
        Some(2),
        "agypl account --version must route to account, stdout={}",
        String::from_utf8_lossy(&acct_ver.stdout)
    );
    assert!(
        String::from_utf8_lossy(&acct_ver.stderr).contains("agy-pulse account error"),
        "expected account error, got stderr: {}",
        String::from_utf8_lossy(&acct_ver.stderr)
    );

    // 2. Top-level version / help / compatibility flags still route as expected.
    for flag in ["--version", "-V", "-v", "version"] {
        let out = env.cmd(&agypl_bin()).arg(flag).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "flag {flag} failed");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.starts_with("agy-pulse v"),
            "expected version output for {flag}, got: {stdout}"
        );
    }

    let dash_help = env
        .cmd(&agy_pulse_bin())
        .args(["--dashboard", "--help"])
        .output()
        .unwrap();
    assert_eq!(dash_help.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&dash_help.stdout).contains("dashboard"),
        "expected dashboard help: {}",
        String::from_utf8_lossy(&dash_help.stdout)
    );
}

#[test]
fn bg_ledger_sync_lock_recovers_from_future_timestamp_clock_rollback() {
    let env = IsolatedCliEnv::new();
    fs::write(
        env.global_cfg_dir.join("agy-pulse.config.json"),
        r#"{"display":{"showDailyCost":true}}"#,
    )
    .unwrap();
    let lock_path = env.data_dir.join("agy-pulse-ledger-sync.lock");
    let now_ms = Timestamp::now().as_millisecond();
    let future_ms = now_ms + 120_000;
    let fake_lock = serde_json::json!({
        "pid": std::process::id(),
        "role": "worker",
        "token": "future-clock-rollback-token",
        "ts": future_ms
    });
    fs::write(&lock_path, fake_lock.to_string()).unwrap();
    let f = fs::OpenOptions::new().write(true).open(&lock_path).unwrap();
    f.set_modified(std::time::SystemTime::now() + Duration::from_secs(120))
        .unwrap();
    drop(f);

    let payload = serde_json::json!({
        "conversation_id": "conv-clock-rollback",
        "plan_tier": "Pro",
        "model": { "display_name": "Gemini 4 Carbon (High)" }
    })
    .to_string();

    let mut child = env
        .cmd(&agy_pulse_bin())
        .env("AGY_PULSE_SYNC_MODE", "1")
        .env("AGY_PULSE_NO_SYNC_AUTOSTART", "1")
        .env("AGY_PULSE_NO_DASHBOARD_AUTOSTART", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Wait for the reclaimed lock to be taken over and released by the spawned `--sync-ledger-bg` worker.
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut reclaimed = false;
    while Instant::now() < deadline {
        match fs::read_to_string(&lock_path) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                reclaimed = true;
                break;
            }
            Ok(raw) if !raw.contains("future-clock-rollback-token") => {
                reclaimed = true;
                break;
            }
            _ => thread::sleep(Duration::from_millis(25)),
        }
    }
    assert!(
        reclaimed,
        "future-timestamp bg ledger sync lock must be reclaimed rather than deadlocking"
    );
}
