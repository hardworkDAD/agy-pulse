#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use flate2::Compression;
use flate2::write::GzEncoder;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use url::Url;
use zip::write::SimpleFileOptions;

fn agy_pulse_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agy-pulse"))
}

fn agypl_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agypl"))
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
        let xdg_cfg_home = root.join("xdg_cfg");
        let cli_data_dir = root.join("cli_data");
        let ide_data_dir = root.join("ide_data");
        let global_cfg_dir = root.join("global_cfg");
        let bin_dir = root.join("user_bin");
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

    fn cmd(&self, bin: &Path) -> Command {
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
            .env("AGY_PULSE_NO_DASHBOARD_AUTOSTART", "1")
            .env("TMPDIR", &self.tmp_dir)
            .env("TMP", &self.tmp_dir)
            .env("TEMP", &self.tmp_dir)
            .env("LC_ALL", "en_US.UTF-8")
            .env("LANG", "en_US.UTF-8");
        cmd
    }

    fn settings_path(&self) -> PathBuf {
        self.cli_data_dir.join("settings.json")
    }

    fn update_status_path(&self) -> PathBuf {
        self.cli_data_dir.join("agy-pulse-update-status.json")
    }
}

#[derive(Clone)]
struct MockResponse {
    status: u16,
    reason: &'static str,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl MockResponse {
    fn json(status: u16, body: &str) -> Self {
        Self {
            status,
            reason: if status == 200 { "OK" } else { "Error" },
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            body: body.as_bytes().to_vec(),
        }
    }

    fn bytes(status: u16, body: Vec<u8>) -> Self {
        Self {
            status,
            reason: if status == 200 { "OK" } else { "Error" },
            headers: vec![(
                "Content-Type".to_string(),
                "application/octet-stream".to_string(),
            )],
            body,
        }
    }

    fn redirect(status: u16, location: &str) -> Self {
        Self {
            status,
            reason: "Found",
            headers: vec![("Location".to_string(), location.to_string())],
            body: Vec::new(),
        }
    }
}

struct LocalHttpServer {
    base_url: String,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl LocalHttpServer {
    fn start<F>(handler: F) -> Self
    where
        F: Fn(&str) -> MockResponse + Send + Sync + 'static,
    {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind local http server");
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let base_url = format!("http://127.0.0.1:{port}");
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let handler = Arc::new(handler);

        let handle = thread::spawn(move || {
            while !stop_flag.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let h = Arc::clone(&handler);
                        thread::spawn(move || {
                            handle_http_connection(stream, &*h);
                        });
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            base_url,
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for LocalHttpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn handle_http_connection<F>(mut stream: TcpStream, handler: &F)
where
    F: Fn(&str) -> MockResponse,
{
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() || request_line.is_empty() {
        return;
    }
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .to_string();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
            break;
        }
    }

    let resp = handler(&path);
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        resp.status,
        resp.reason,
        resp.body.len()
    );
    for (k, v) in resp.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&resp.body);
    let _ = stream.flush();
}

fn current_asset_name() -> &'static str {
    agy_pulse::platform::current_release_asset_name()
        .expect("current test platform must be in 5-platform release contract")
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn append_ustar_entry(buf: &mut Vec<u8>, name: &str, mode: u32, data: &[u8]) {
    let mut header = [0u8; 512];
    let name_bytes = name.as_bytes();
    header[..name_bytes.len()].copy_from_slice(name_bytes);
    let mode_str = format!("{mode:07o}\0");
    header[100..108].copy_from_slice(mode_str.as_bytes());
    header[108..116].copy_from_slice(b"0000000\0");
    header[116..124].copy_from_slice(b"0000000\0");
    let size_str = format!("{:011o}\0", data.len());
    header[124..136].copy_from_slice(size_str.as_bytes());
    header[136..148].copy_from_slice(b"00000000000\0");
    header[148..156].copy_from_slice(b"        ");
    header[156] = b'0';
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");

    let cksum: u32 = header.iter().map(|&b| u32::from(b)).sum();
    let cksum_str = format!("{cksum:06o}\0 ");
    header[148..156].copy_from_slice(cksum_str.as_bytes());

    buf.extend_from_slice(&header);
    buf.extend_from_slice(data);
    let rem = data.len() % 512;
    if rem != 0 {
        buf.resize(buf.len() + (512 - rem), 0);
    }
}

fn build_release_archive(asset_name: &str, bin_bytes: &[u8]) -> Vec<u8> {
    let exe_suffix = std::env::consts::EXE_SUFFIX;
    let primary_name = format!("agy-pulse{exe_suffix}");
    let alias_name = format!("agypl{exe_suffix}");

    if asset_name.ends_with(".zip") {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut cursor);
            let opts = SimpleFileOptions::default().unix_permissions(0o755);
            zip.start_file(&primary_name, opts).unwrap();
            zip.write_all(bin_bytes).unwrap();
            zip.start_file(&alias_name, opts).unwrap();
            zip.write_all(bin_bytes).unwrap();
            zip.finish().unwrap();
        }
        cursor.into_inner()
    } else {
        let mut tar_bytes = Vec::new();
        append_ustar_entry(&mut tar_bytes, &primary_name, 0o755, bin_bytes);
        append_ustar_entry(&mut tar_bytes, &alias_name, 0o755, bin_bytes);
        tar_bytes.resize(tar_bytes.len() + 1024, 0);

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&tar_bytes).unwrap();
        encoder.finish().unwrap()
    }
}

#[test]
fn install_registers_statusline_repairs_corrupt_settings_and_never_touches_official_agy_dir() {
    let env = IsolatedEnv::new();

    // Simulate official agy installation directory under LOCALAPPDATA/agy/bin
    let fake_agy_dir = env.local_app_data.join("agy").join("bin");
    fs::create_dir_all(&fake_agy_dir).unwrap();
    let fake_agy_bin = fake_agy_dir.join(format!("agy{}", std::env::consts::EXE_SUFFIX));
    fs::write(&fake_agy_bin, b"official-agy-binary").unwrap();

    // Seed valid settings.json with unrelated fields and statusLine.stack_with_default
    fs::write(
        env.settings_path(),
        serde_json::to_string_pretty(&serde_json::json!({
            "theme": "solarized-dark",
            "telemetry": false,
            "nested": { "keep": 42 },
            "statusLine": {
                "enabled": true,
                "stack_with_default": true,
                "type": "command",
                "command": "legacy-command"
            }
        }))
        .unwrap(),
    )
    .unwrap();

    // Seed stale v1 plugin artifacts that should be cleaned on install
    let stale_plugin_dir = env.cli_data_dir.join("plugins").join("agy-pulse");
    fs::create_dir_all(stale_plugin_dir.join("skills").join("hud-config")).unwrap();
    fs::write(stale_plugin_dir.join("hooks.json"), b"{}").unwrap();
    fs::write(stale_plugin_dir.join("mcp_config.json"), b"{}").unwrap();
    fs::write(
        stale_plugin_dir.join("skills/hud-config/SKILL.md"),
        b"legacy",
    )
    .unwrap();

    let out = env.cmd(&agypl_bin()).arg("install").output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "install failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let deployed_primary = env
        .bin_dir
        .join(format!("agy-pulse{}", std::env::consts::EXE_SUFFIX));
    let deployed_alias = env
        .bin_dir
        .join(format!("agypl{}", std::env::consts::EXE_SUFFIX));
    assert!(deployed_primary.is_file(), "agy-pulse must be deployed to bin_dir");
    assert!(deployed_alias.is_file(), "agypl must be deployed to bin_dir");

    let ver_out = env.cmd(&deployed_alias).arg("--version").output().unwrap();
    assert_eq!(ver_out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&ver_out.stdout).contains("2.0.0"));

    // Verify settings.json preserved unrelated keys & stack_with_default
    let settings: Value =
        serde_json::from_str(&fs::read_to_string(env.settings_path()).unwrap()).unwrap();
    assert_eq!(settings["theme"], "solarized-dark");
    assert_eq!(settings["telemetry"], false);
    assert_eq!(settings["nested"]["keep"], 42);
    assert_eq!(settings["statusLine"]["enabled"], true);
    assert_eq!(settings["statusLine"]["stack_with_default"], true);
    assert_eq!(settings["statusLine"]["type"], "command");
    let cmd_str = settings["statusLine"]["command"].as_str().unwrap();
    assert!(
        cmd_str.contains(&deployed_primary.to_string_lossy().to_string()),
        "statusLine.command must point to deployed native binary, got: {cmd_str}"
    );

    // Verify stale plugin artifacts were cleaned
    assert!(!stale_plugin_dir.join("hooks.json").exists());
    assert!(!stale_plugin_dir.join("mcp_config.json").exists());
    assert!(!stale_plugin_dir.join("skills/hud-config").exists());

    // Compliance red line: official agy/bin directory must remain completely untouched
    let agy_entries: Vec<String> = fs::read_dir(&fake_agy_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        agy_entries,
        vec![format!("agy{}", std::env::consts::EXE_SUFFIX)],
        "must never write shims or binaries into official agy/bin dir"
    );

    // Subcase B: Corrupt settings.json must be backed up to settings.json.bak before repair
    let corrupt_payload = "{ broken json ::: not valid";
    fs::write(env.settings_path(), corrupt_payload).unwrap();

    let out2 = env.cmd(&agy_pulse_bin()).arg("--install").output().unwrap();
    assert_eq!(out2.status.code(), Some(0));

    let bak_path = env.cli_data_dir.join("settings.json.bak");
    assert!(bak_path.is_file(), "corrupt settings must be backed up to .bak");
    assert_eq!(fs::read_to_string(&bak_path).unwrap(), corrupt_payload);

    let repaired: Value =
        serde_json::from_str(&fs::read_to_string(env.settings_path()).unwrap()).unwrap();
    assert_eq!(repaired["statusLine"]["type"], "command");
    assert!(repaired["statusLine"]["command"]
        .as_str()
        .unwrap()
        .contains("agy-pulse"));
}

#[test]
fn background_update_check_writes_status_and_statusline_renders_prompt() {
    let env = IsolatedEnv::new();
    let server = LocalHttpServer::start(|path| {
        if path == "/latest.json" {
            MockResponse::json(200, r#"{"version":"2.1.0"}"#)
        } else {
            MockResponse::json(404, "{}")
        }
    });

    // 1. Direct --check-update-bg invocation writes agy-pulse-update-status.json
    let out = env
        .cmd(&agypl_bin())
        .arg("--check-update-bg")
        .env("AGY_PULSE_UPDATE_URL", format!("{}/latest.json", server.base_url))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));

    let status_val: Value =
        serde_json::from_str(&fs::read_to_string(env.update_status_path()).unwrap()).unwrap();
    assert_eq!(status_val["updateAvailable"], true);
    assert_eq!(status_val["latestVersion"], "2.1.0");
    assert_eq!(status_val["localVersion"], "2.0.0");
    assert!(status_val["lastCheck"].as_i64().unwrap() > 0);

    // 2. Statusline hot path reads local update status and renders quiet update prompt
    let stdin_payload = serde_json::json!({
        "model": { "id": "gemini-3-flash-agent", "display_name": "Gemini 3.5 Flash" },
        "workspace": { "current_dir": env.workspace_dir }
    })
    .to_string();

    let mut child = env
        .cmd(&agy_pulse_bin())
        .env("AGY_PULSE_DISABLE_UPDATE_CHECK", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin_payload.as_bytes())
        .unwrap();
    let statusline_out = child.wait_with_output().unwrap();
    assert_eq!(statusline_out.status.code(), Some(0));
    let rendered = String::from_utf8_lossy(&statusline_out.stdout);
    assert!(
        rendered.contains("v2.1.0") && !rendered.contains("vv2.1.0"),
        "statusline must render v2.1.0 update hint cleanly, got: {rendered}"
    );

    // 3. After 24h cooldown expires, statusline spawns detached --check-update-bg asynchronously
    let server2 = LocalHttpServer::start(|path| {
        if path == "/latest.json" {
            MockResponse::json(200, r#"{"tag_name":"v2.2.0"}"#)
        } else {
            MockResponse::json(404, "{}")
        }
    });
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let stale_ms = now_ms - (25 * 60 * 60 * 1000);
    fs::write(
        env.update_status_path(),
        serde_json::to_string_pretty(&serde_json::json!({
            "lastCheck": stale_ms,
            "updateAvailable": false,
            "latestVersion": "2.0.0",
            "localVersion": "2.0.0"
        }))
        .unwrap(),
    )
    .unwrap();

    let started = Instant::now();
    let mut child2 = env
        .cmd(&agy_pulse_bin())
        .env("AGY_PULSE_UPDATE_URL", format!("{}/latest.json", server2.base_url))
        .env_remove("AGY_PULSE_DISABLE_UPDATE_CHECK")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child2
        .stdin
        .take()
        .unwrap()
        .write_all(stdin_payload.as_bytes())
        .unwrap();
    let out2 = child2.wait_with_output().unwrap();
    assert_eq!(out2.status.code(), Some(0));
    assert!(
        started.elapsed() < Duration::from_millis(1400),
        "statusline must not block on background update check"
    );

    // Wait up to 3s for detached child to finish writing 2.2.0
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut observed_2_2_0 = false;
    while Instant::now() < deadline {
        if let Ok(raw) = fs::read_to_string(env.update_status_path())
            && let Ok(val) = serde_json::from_str::<Value>(&raw)
            && val["latestVersion"] == "2.2.0"
            && val["updateAvailable"] == true
        {
            observed_2_2_0 = true;
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        observed_2_2_0,
        "detached --check-update-bg must update agy-pulse-update-status.json to 2.2.0"
    );
}

#[test]
fn manual_update_verifies_sha256_rejects_downgrade_and_atomically_replaces_binary() {
    // Protocol & anti-downgrade validation contract
    assert!(
        agy_pulse::ops::validate_release_url(
            "https://github.com/hardworkDAD/agy-pulse/releases/download/v2.1.0/SHA256SUMS"
        )
        .is_ok()
    );
    assert!(agy_pulse::ops::validate_release_url("http://127.0.0.1:8080/SHA256SUMS").is_ok());
    assert!(
        agy_pulse::ops::validate_release_url("http://example.com/SHA256SUMS").is_err(),
        "non-loopback http:// must be rejected"
    );
    let https_origin =
        Url::parse("https://github.com/hardworkDAD/agy-pulse/releases/download/v2.1.0/pkg.tar.gz")
            .unwrap();
    assert!(
        agy_pulse::ops::validate_redirect_url(&https_origin, "http://127.0.0.1:8080/pkg.tar.gz")
            .is_err(),
        "https -> http downgrade redirect must be forbidden"
    );
    assert!(
        agy_pulse::ops::validate_redirect_url(&https_origin, "http://cdn.example.com/pkg.tar.gz")
            .is_err()
    );
    assert!(
        agy_pulse::ops::validate_redirect_url(
            &https_origin,
            "https://objects.githubusercontent.com/pkg.tar.gz"
        )
        .is_ok()
    );

    let env = IsolatedEnv::new();
    let target_bin = env
        .bin_dir
        .join(format!("agypl{}", std::env::consts::EXE_SUFFIX));
    let companion_bin = env
        .bin_dir
        .join(format!("agy-pulse{}", std::env::consts::EXE_SUFFIX));
    fs::copy(agypl_bin(), &target_bin).unwrap();
    fs::copy(agy_pulse_bin(), &companion_bin).unwrap();
    agy_pulse::platform::set_executable_0755(&target_bin).unwrap();
    agy_pulse::platform::set_executable_0755(&companion_bin).unwrap();
    let original_bytes = fs::read(&target_bin).unwrap();

    // Reject non-loopback http:// via CLI
    let insecure_out = env
        .cmd(&target_bin)
        .arg("update")
        .env("AGY_PULSE_UPDATE_URL", "http://example.com/latest.json")
        .output()
        .unwrap();
    assert_ne!(insecure_out.status.code(), Some(0));
    assert_eq!(fs::read(&target_bin).unwrap(), original_bytes);

    // Reject redirect from local loopback to non-loopback http:// via real HTTP server
    let redirect_evil_server = LocalHttpServer::start(|path| {
        if path == "/latest.json" {
            MockResponse::redirect(302, "http://example.com/latest.json")
        } else {
            MockResponse::json(404, "{}")
        }
    });
    let redirect_evil_out = env
        .cmd(&target_bin)
        .arg("update")
        .env(
            "AGY_PULSE_UPDATE_URL",
            format!("{}/latest.json", redirect_evil_server.base_url),
        )
        .output()
        .unwrap();
    assert_ne!(redirect_evil_out.status.code(), Some(0));
    assert_eq!(fs::read(&target_bin).unwrap(), original_bytes);

    // Prepare updated binary payload for archive
    #[cfg(unix)]
    let updated_bin_bytes: Vec<u8> =
        b"#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo \"agy-pulse v2.1.0\"; exit 0; fi\necho \"updated-v2.1.0\"\n"
            .to_vec();
    #[cfg(windows)]
    let updated_bin_bytes: Vec<u8> = {
        let mut buf = fs::read(agypl_bin()).unwrap();
        // Append benign trailing padding so SHA-256 & byte comparison verify replacement while PE executes
        buf.extend_from_slice(b"\nAGY_PULSE_TEST_2_1_0\n");
        buf
    };

    let asset_name = current_asset_name();
    let archive_bytes = build_release_archive(asset_name, &updated_bin_bytes);
    let valid_sha256 = sha256_hex(&archive_bytes);

    // 1. Tampered SHA256SUMS must reject replacement and preserve original binary
    let bad_archive = archive_bytes.clone();
    let bad_server = LocalHttpServer::start(move |path| {
        if path == "/latest.json" {
            MockResponse::json(200, r#"{"tag_name":"v2.1.0"}"#)
        } else if path == "/releases/SHA256SUMS" {
            MockResponse::bytes(
                200,
                format!("{}  {asset_name}\n", "0".repeat(64)).into_bytes(),
            )
        } else if path == format!("/releases/{asset_name}") {
            MockResponse::bytes(200, bad_archive.clone())
        } else {
            MockResponse::json(404, "{}")
        }
    });

    let bad_out = env
        .cmd(&target_bin)
        .arg("update")
        .env(
            "AGY_PULSE_UPDATE_URL",
            format!("{}/latest.json", bad_server.base_url),
        )
        .env(
            "AGY_PULSE_RELEASE_BASE_URL",
            format!("{}/releases", bad_server.base_url),
        )
        .output()
        .unwrap();
    assert_ne!(
        bad_out.status.code(),
        Some(0),
        "tampered SHA256SUMS must cause update to fail"
    );
    assert_eq!(
        fs::read(&target_bin).unwrap(),
        original_bytes,
        "original binary must remain untouched on checksum failure"
    );

    // 2. Valid SHA256SUMS (with safe loopback redirect) atomically replaces binary
    let good_archive = archive_bytes.clone();
    let good_sum = format!("{valid_sha256}  {asset_name}\n");
    let good_server = LocalHttpServer::start(move |path| {
        if path == "/latest.json" {
            MockResponse::json(200, r#"{"tag_name":"v2.1.0"}"#)
        } else if path == "/releases/SHA256SUMS" {
            MockResponse::bytes(200, good_sum.as_bytes().to_vec())
        } else if path == format!("/releases/{asset_name}") {
            MockResponse::redirect(302, &format!("/cdn/{asset_name}"))
        } else if path == format!("/cdn/{asset_name}") {
            MockResponse::bytes(200, good_archive.clone())
        } else {
            MockResponse::json(404, "{}")
        }
    });

    let good_out = env
        .cmd(&target_bin)
        .arg("update")
        .env(
            "AGY_PULSE_UPDATE_URL",
            format!("{}/latest.json", good_server.base_url),
        )
        .env(
            "AGY_PULSE_RELEASE_BASE_URL",
            format!("{}/releases", good_server.base_url),
        )
        .output()
        .unwrap();
    assert_eq!(
        good_out.status.code(),
        Some(0),
        "valid update failed: {}",
        String::from_utf8_lossy(&good_out.stderr)
    );

    assert_eq!(fs::read(&target_bin).unwrap(), updated_bin_bytes);
    assert_eq!(fs::read(&companion_bin).unwrap(), updated_bin_bytes);

    let new_ver_out = env.cmd(&target_bin).arg("--version").output().unwrap();
    assert_eq!(new_ver_out.status.code(), Some(0));
    #[cfg(unix)]
    assert!(
        String::from_utf8_lossy(&new_ver_out.stdout).contains("2.1.0"),
        "replaced binary must execute new version"
    );
}

#[test]
fn uninstall_protects_foreign_statusline_cleans_artifacts_and_preserves_ledger_by_default() {
    let env = IsolatedEnv::new();

    // 1. Foreign statusLine must never be deleted
    let foreign_settings = serde_json::json!({
        "theme": "nord",
        "statusLine": {
            "type": "command",
            "command": "/usr/local/bin/third-party-statusline --json"
        }
    });
    fs::write(
        env.settings_path(),
        serde_json::to_string_pretty(&foreign_settings).unwrap(),
    )
    .unwrap();

    let foreign_out = env.cmd(&agypl_bin()).arg("uninstall").output().unwrap();
    assert_eq!(foreign_out.status.code(), Some(0));
    let after_foreign: Value =
        serde_json::from_str(&fs::read_to_string(env.settings_path()).unwrap()).unwrap();
    assert_eq!(
        after_foreign["statusLine"]["command"],
        "/usr/local/bin/third-party-statusline --json"
    );
    assert_eq!(after_foreign["theme"], "nord");

    // 2. Full install + dashboard shortcut + caches + ledger + global config
    let install_out = env.cmd(&agypl_bin()).arg("install").output().unwrap();
    assert_eq!(install_out.status.code(), Some(0));

    let shortcut_out = env
        .cmd(&agypl_bin())
        .args(["dashboard", "install-shortcut"])
        .output()
        .unwrap();
    assert_eq!(shortcut_out.status.code(), Some(0));

    // Add an unrelated shortcut/entry file that must NOT be removed
    #[cfg(target_os = "linux")]
    let (installed_shortcuts, foreign_shortcut) = {
        let apps_dir = env.xdg_data_home.join("applications");
        fs::create_dir_all(&apps_dir).unwrap();
        let foreign = apps_dir.join("other-unrelated-app.desktop");
        fs::write(&foreign, "[Desktop Entry]\nName=Other\n").unwrap();
        (vec![apps_dir.join("agy-pulse-dashboard.desktop")], foreign)
    };
    #[cfg(target_os = "macos")]
    let (installed_shortcuts, foreign_shortcut) = {
        let apps_dir = env.home_dir.join("Applications");
        let foreign = apps_dir.join("OtherApp.app/Contents/Info.plist");
        fs::create_dir_all(foreign.parent().unwrap()).unwrap();
        fs::write(&foreign, "<plist>Other</plist>\n").unwrap();
        (
            vec![
                apps_dir.join("agy-pulse Dashboard.app"),
                apps_dir.join("agy-pulse Dashboard.app/Contents/Info.plist"),
            ],
            foreign,
        )
    };
    #[cfg(windows)]
    let (installed_shortcuts, foreign_shortcut) = {
        let start_dir = env
            .app_data
            .join("Microsoft/Windows/Start Menu/Programs");
        fs::create_dir_all(&start_dir).unwrap();
        let foreign = start_dir.join("other-unrelated-app.lnk");
        fs::write(&foreign, b"foreign-lnk").unwrap();
        (
            vec![
                env.local_app_data
                    .join("agy-pulse/agy-pulse-dashboard.vbs"),
                env.local_app_data
                    .join("agy-pulse/agy-pulse-dashboard.svg"),
                start_dir.join("agy-pulse Dashboard.lnk"),
            ],
            foreign,
        )
    };
    for path in &installed_shortcuts {
        assert!(
            path.exists(),
            "expected installed shortcut artifact: {}",
            path.display()
        );
    }

    // Seed caches, legacy runtime dir, ledger + WAL/SHM, and global config
    let legacy_runtime = env.cli_data_dir.join("agy-pulse-runtime");
    fs::create_dir_all(&legacy_runtime).unwrap();
    fs::write(legacy_runtime.join("old.js"), b"legacy").unwrap();
    let error_log = env.cli_data_dir.join("agy-pulse-error.log");
    let update_status = env.update_status_path();
    let compaction_cache = env.cli_data_dir.join("agy-pulse-compaction-cache.json");
    let speed_cache_dir = env.cli_data_dir.join("speed-cache");
    let transcript_cache_dir = env.cli_data_dir.join("transcript-cache");
    fs::create_dir_all(&speed_cache_dir).unwrap();
    fs::create_dir_all(&transcript_cache_dir).unwrap();
    fs::write(&error_log, b"log").unwrap();
    fs::write(&update_status, b"{}").unwrap();
    fs::write(&compaction_cache, b"{}").unwrap();
    fs::write(speed_cache_dir.join("s.json"), b"{}").unwrap();
    fs::write(transcript_cache_dir.join("t.json"), b"{}").unwrap();

    let ledger_db = env.cli_data_dir.join("agy-pulse-ledger.sqlite");
    let ledger_wal = env.cli_data_dir.join("agy-pulse-ledger.sqlite-wal");
    let ledger_shm = env.cli_data_dir.join("agy-pulse-ledger.sqlite-shm");
    fs::write(&ledger_db, b"sqlite-ledger-data").unwrap();
    fs::write(&ledger_wal, b"wal-data").unwrap();
    fs::write(&ledger_shm, b"shm-data").unwrap();

    let global_cfg = env.global_cfg_dir.join("agy-pulse.config.json");
    fs::write(&global_cfg, br#"{"language":"zh"}"#).unwrap();

    // Default uninstall (NO --purge-ledger)
    let uninstall_out = env.cmd(&agypl_bin()).arg("uninstall").output().unwrap();
    assert_eq!(
        uninstall_out.status.code(),
        Some(0),
        "uninstall failed: {}",
        String::from_utf8_lossy(&uninstall_out.stderr)
    );

    let after_settings: Value =
        serde_json::from_str(&fs::read_to_string(env.settings_path()).unwrap()).unwrap();
    assert!(
        after_settings.get("statusLine").is_none(),
        "our statusLine entry must be removed"
    );
    assert_eq!(after_settings["theme"], "nord");
    assert!(env.cli_data_dir.join("settings.json.bak").is_file());

    let deployed_primary = env
        .bin_dir
        .join(format!("agy-pulse{}", std::env::consts::EXE_SUFFIX));
    let deployed_alias = env
        .bin_dir
        .join(format!("agypl{}", std::env::consts::EXE_SUFFIX));
    assert!(!deployed_primary.exists());
    assert!(!deployed_alias.exists());

    // Fingerprinted shortcut removed, unrelated shortcut file preserved
    for path in &installed_shortcuts {
        assert!(
            !path.exists(),
            "shortcut artifact was not removed: {}",
            path.display()
        );
    }
    assert!(foreign_shortcut.is_file());

    // Caches and runtime dir removed
    assert!(!legacy_runtime.exists());
    assert!(!error_log.exists());
    assert!(!update_status.exists());
    assert!(!compaction_cache.exists());
    assert!(!speed_cache_dir.exists());
    assert!(!transcript_cache_dir.exists());

    // CRITICAL: ledger & global config MUST be preserved by default
    assert!(ledger_db.is_file(), "ledger db must be preserved by default");
    assert!(ledger_wal.is_file(), "ledger wal must be preserved by default");
    assert!(ledger_shm.is_file(), "ledger shm must be preserved by default");
    assert!(global_cfg.is_file(), "global config must be preserved");

    // 3. Explicit --purge-ledger deletes ledger files while preserving global config
    let purge_out = env
        .cmd(&agy_pulse_bin())
        .args(["uninstall", "--purge-ledger"])
        .output()
        .unwrap();
    assert_eq!(purge_out.status.code(), Some(0));
    assert!(!ledger_db.exists(), "--purge-ledger must delete ledger db");
    assert!(!ledger_wal.exists(), "--purge-ledger must delete ledger wal");
    assert!(!ledger_shm.exists(), "--purge-ledger must delete ledger shm");
    assert!(global_cfg.is_file(), "global config still preserved");

    // 4. Platform uninstall script forwards --purge-ledger to the native binary in AGY_PULSE_BIN_DIR
    #[cfg(unix)]
    {
        let script_install = env.cmd(&agypl_bin()).arg("install").output().unwrap();
        assert_eq!(script_install.status.code(), Some(0));
        fs::write(&ledger_db, b"sqlite-ledger-data-2").unwrap();
        let uninstall_sh = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("uninstall.sh");
        let sh_out = env
            .cmd(Path::new("bash"))
            .arg(&uninstall_sh)
            .output()
            .unwrap();
        assert_eq!(
            sh_out.status.code(),
            Some(0),
            "uninstall.sh failed: {}",
            String::from_utf8_lossy(&sh_out.stderr)
        );
        assert!(!deployed_primary.exists());
        assert!(ledger_db.is_file(), "uninstall.sh without --purge-ledger preserves ledger");
    }
}
