#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;
use tempfile::TempDir;

use agy_pulse::config::{WizardRunOptions, WizardRunOutcome, run_wizard_loop};
use agy_pulse::render::strip_ansi;

fn agy_pulse_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agy-pulse"))
}

fn agypl_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agypl"))
}

struct IsolatedEnv {
    _temp: TempDir,
    home_dir: PathBuf,
    cli_data_dir: PathBuf,
    global_cfg_dir: PathBuf,
    xdg_cfg_dir: PathBuf,
    app_data_dir: PathBuf,
    local_app_data_dir: PathBuf,
    workspace_dir: PathBuf,
}

impl IsolatedEnv {
    fn new() -> Self {
        let temp = TempDir::new().expect("create temp dir");
        let root = temp.path();
        let home_dir = root.join("home");
        let cli_data_dir = root.join("cli_data");
        let global_cfg_dir = root.join("global_cfg");
        let xdg_cfg_dir = root.join("xdg_cfg");
        let app_data_dir = root.join("app_data");
        let local_app_data_dir = root.join("local_app_data");
        let workspace_dir = root.join("workspace");
        fs::create_dir_all(&home_dir).unwrap();
        fs::create_dir_all(&cli_data_dir).unwrap();
        fs::create_dir_all(&global_cfg_dir).unwrap();
        fs::create_dir_all(&xdg_cfg_dir).unwrap();
        fs::create_dir_all(&app_data_dir).unwrap();
        fs::create_dir_all(&local_app_data_dir).unwrap();
        fs::create_dir_all(&workspace_dir).unwrap();
        Self {
            _temp: temp,
            home_dir,
            cli_data_dir,
            global_cfg_dir,
            xdg_cfg_dir,
            app_data_dir,
            local_app_data_dir,
            workspace_dir,
        }
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
            .env("AGY_PULSE_DATA_DIR", &self.cli_data_dir)
            .env("AGY_PULSE_GLOBAL_CONFIG_DIR", &self.global_cfg_dir)
            .env("AGY_PULSE_NO_DASHBOARD_AUTOSTART", "1")
            .env_remove("AGY_PULSE_IDE_DATA_DIR")
            .env_remove("AGY_PULSE_FORCE_ASCII")
            .env_remove("AGY_PULSE_FORCE_UNICODE")
            .env_remove("AGY_PULSE_LANGUAGE")
            .env_remove("COLUMNS")
            .env("LC_ALL", "en_US.UTF-8")
            .env("LANG", "en_US.UTF-8");
        cmd
    }

    fn run_args(&self, bin: &Path, args: &[&str]) -> (i32, String, String) {
        let out = self
            .base_cmd(bin)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run binary with args");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn run_stdin(&self, bin: &Path, stdin_text: &str) -> (i32, String, String) {
        use std::io::Write;
        let mut child = self
            .base_cmd(bin)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn binary");
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(stdin_text.as_bytes()).expect("write stdin");
        }
        let out = child.wait_with_output().expect("wait binary");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

/// Serialize in-process tests that mutate process-wide env vars (`AGY_PULSE_GLOBAL_CONFIG_DIR`).
static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct EnvVarGuard {
    key: &'static str,
    prev: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, val: &Path) -> Self {
        let prev = std::env::var_os(key);
        // SAFETY: guarded by `ENV_MUTEX` in integration tests.
        unsafe { std::env::set_var(key, val) };
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.prev {
            // SAFETY: guarded by `ENV_MUTEX` in integration tests.
            Some(v) => unsafe { std::env::set_var(self.key, v) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

#[test]
fn t06_non_tty_config_cli_exits_safely_without_mutating_config() {
    let env = IsolatedEnv::new();
    let global_cfg = env.global_cfg_dir.join("agy-pulse.config.json");
    let local_cfg = env.workspace_dir.join("agy-pulse.config.json");

    // Case 1: no config file exists yet -> piped `agypl config` and `agy-pulse --config` must not create one.
    let (code1, stdout1, stderr1) = env.run_args(&agypl_bin(), &["config"]);
    assert_eq!(code1, 0, "non-TTY config should exit cleanly: stderr={stderr1}");
    let combined1 = format!("{stdout1}{stderr1}").to_ascii_lowercase();
    assert!(
        combined1.contains("tty") || combined1.contains("interactive"),
        "expected friendly non-TTY prompt, got: {combined1:?}"
    );
    assert!(!global_cfg.exists(), "must not create global config on non-TTY exit");
    assert!(!local_cfg.exists(), "must not create local config on non-TTY exit");

    // Case 2: existing config file -> piped `agy-pulse --config` must preserve exact bytes.
    let sentinel = "{\n  \"language\": \"zh\",\n  \"customKeep\": 123\n}\n";
    fs::write(&global_cfg, sentinel).unwrap();

    let (code2, stdout2, stderr2) = env.run_args(&agy_pulse_bin(), &["--config"]);
    assert_eq!(code2, 0, "non-TTY --config should exit cleanly: stderr={stderr2}");
    let combined2 = format!("{stdout2}{stderr2}").to_ascii_lowercase();
    assert!(
        combined2.contains("tty") || combined2.contains("interactive"),
        "expected friendly non-TTY prompt, got: {combined2:?}"
    );
    assert_eq!(fs::read_to_string(&global_cfg).unwrap(), sentinel);
}

#[test]
fn t06_wizard_navigation_live_preview_compliance_and_cancel_no_write() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let env = IsolatedEnv::new();
    let _guard = EnvVarGuard::set("AGY_PULSE_GLOBAL_CONFIG_DIR", &env.global_cfg_dir);

    let global_cfg = env.global_cfg_dir.join("agy-pulse.config.json");
    let original_json = r#"{
  "language": "en",
  "theme": "Emerald Green",
  "display": {
    "quotaStyle": "table",
    "showContextMonitor": true,
    "showModelCache": true
  }
}
"#;
    fs::write(&global_cfg, original_json).unwrap();

    // Key sequence:
    // - index 0 is Scope, move down (`\x1b[B`) -> index 1 (Language)
    // - Space (` `) -> `en` -> `zh` (preview renders Chinese `上下文`)
    // - `j` -> index 2 (Theme Preset)
    // - Space (` `) -> `Emerald Green` -> `Ocean Blue` (preview contains `\x1b[34m`)
    // - `\r` (Enter) -> `Ocean Blue` -> `Cyberpunk Magenta` (preview contains `\x1b[35m`)
    // - `j` -> index 3 (Quota Display Mode)
    // - Space (` `) -> `Table` -> `Compact`
    // - Space (` `) -> `Compact` -> `None`
    // - Space (` `) -> `None` -> `Table` (verifying `models` is completely absent!)
    // - `j` -> index 4 (Warning Threshold), Space -> advances warning threshold
    // - `k` -> back up, `q` -> cancel without saving
    let mut keys = Vec::new();
    keys.extend_from_slice(b"\x1b[B "); // Language: en -> zh
    keys.extend_from_slice(b"j \r"); // Theme: Emerald Green -> Ocean Blue -> Cyberpunk Magenta
    keys.extend_from_slice(b"j   "); // QuotaStyle: Table -> Compact -> None -> Table
    keys.extend_from_slice(b"j "); // Warning Threshold toggle
    keys.extend_from_slice(b"kq"); // Up then quit

    let opts = WizardRunOptions {
        cwd: env.workspace_dir.clone(),
        terminal_cols: Some(120),
        terminal_rows: Some(18),
        unicode: true,
        locale: Some("en_US.UTF-8".to_string()),
        observed_at_ms: Some(1_700_000_000_000),
        clear_screen: true,
    };

    let mut input = Cursor::new(keys);
    let mut output = Vec::new();
    let outcome = run_wizard_loop(&mut input, &mut output, &opts).unwrap();
    assert_eq!(outcome, WizardRunOutcome::Cancelled);

    let raw_out = String::from_utf8_lossy(&output).into_owned();
    let plain_out = strip_ansi(&raw_out);

    // Real statusline preview present in frames
    assert!(
        plain_out.contains("agy-pulse Status Line Preview"),
        "expected statusline preview header in wizard output"
    );
    // Viewport scrolling active under 18 rows
    assert!(
        plain_out.contains("more below") || plain_out.contains("more above"),
        "expected viewport scroll indicators for compact height terminal"
    );
    // Language switch reflected in live statusline preview
    assert!(
        plain_out.contains("上下文"),
        "expected Chinese preview text after toggling language to zh"
    );
    // Theme ANSI color changes reflected in live statusline preview
    assert!(
        raw_out.contains("\x1b[34m") && raw_out.contains("\x1b[35m"),
        "expected Ocean Blue (34m) and Cyberpunk Magenta (35m) ANSI colors in live previews"
    );
    // Quota style cycled Compact and None and back to Table
    assert!(plain_out.contains("Quota Display Mode: Compact"));
    assert!(plain_out.contains("Quota Display Mode: None"));
    assert!(plain_out.contains("Quota Display Mode: Table"));
    // Compliance: strictly NO retired cloudQuota or quotaStyle "models"
    assert!(
        !plain_out.to_ascii_lowercase().contains("cloudquota")
            && !plain_out.contains("Quota Display Mode: Models"),
        "wizard must not expose retired cloudQuota or models quotaStyle: {plain_out}"
    );
    // Original config file byte-for-byte untouched on `q`
    assert_eq!(fs::read_to_string(&global_cfg).unwrap(), original_json);

    // Also verify Ctrl-C (`0x03`) and `Esc` (`\x1b`) abort/cancel without modifying config
    let mut ctrl_c_in = Cursor::new(b"\x1b[B \x03".to_vec());
    let mut ctrl_c_out = Vec::new();
    let ctrl_c_outcome = run_wizard_loop(&mut ctrl_c_in, &mut ctrl_c_out, &opts).unwrap();
    assert_eq!(ctrl_c_outcome, WizardRunOutcome::Aborted);
    assert_eq!(fs::read_to_string(&global_cfg).unwrap(), original_json);

    let mut esc_in = Cursor::new(b"\x1b[B \x1b".to_vec());
    let mut esc_out = Vec::new();
    let esc_outcome = run_wizard_loop(&mut esc_in, &mut esc_out, &opts).unwrap();
    assert_eq!(esc_outcome, WizardRunOutcome::Cancelled);
    assert_eq!(fs::read_to_string(&global_cfg).unwrap(), original_json);
}

#[test]
fn t06_wizard_atomic_save_preserves_custom_keys_and_updates_statusline() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let env = IsolatedEnv::new();
    let _guard = EnvVarGuard::set("AGY_PULSE_GLOBAL_CONFIG_DIR", &env.global_cfg_dir);

    let global_cfg = env.global_cfg_dir.join("agy-pulse.config.json");
    let seed_json = r#"{
  "customOwner": "keep-this",
  "customArray": [1, 2, 3],
  "language": "en",
  "theme": {
    "primary": "green",
    "secondary": "gray",
    "warning": "yellow",
    "critical": "red"
  },
  "thresholds": {
    "warning": 0.7,
    "critical": 0.9,
    "customThresholdTag": "preserve"
  },
  "display": {
    "customDisplayFlag": 42,
    "quotaStyle": "table",
    "showModel": false,
    "showContextMonitor": true,
    "showModelCache": true
  }
}
"#;
    fs::write(&global_cfg, seed_json).unwrap();

    // Navigate from index 0 (Scope = Global):
    // '2' -> index 1 (Language): Space -> en -> zh
    // '3' -> index 2 (Theme Preset): Space -> Emerald Green -> Ocean Blue
    // '4' -> index 3 (Quota Display Mode): Space -> Table -> Compact
    // '5' -> index 4 (Warning Threshold): Space -> 70% -> 75% (or next value != 0.7)
    // Move down to `Show Model (Line 1)` (index 11): press '5' then 7x 'j' -> wait, let's use exact index or `j`!
    // Better: jump with '9' (index 8) then 3x 'j' (index 11) -> toggle Show Model (Line 1) -> then press 's' to save.
    // Wait: let's inspect exact menu item index or navigate cleanly using label checks?
    // Even simpler: use `j` from index 4:
    // 0: Scope
    // 1: Language
    // 2: Theme Preset
    // 3: Quota Display Mode
    // 4: Warning Threshold
    // 5: Critical Threshold
    // 6: Icon & Font Set
    // 7: Show Context Monitor
    // 8: Show Model Cache Line
    // 9: Show Current Directory
    // 10: Show Cycle Mode (Shift+Tab)
    // 11: Show Model (Line 1)
    let mut keys = Vec::new();
    keys.extend_from_slice(b"2 "); // index 1: Language -> zh
    keys.extend_from_slice(b"3 "); // index 2: Theme Preset -> Ocean Blue
    keys.extend_from_slice(b"4 "); // index 3: Quota Display Mode -> Compact
    keys.extend_from_slice(b"5 "); // index 4: Warning Threshold -> next preset (!= 0.7)
    keys.extend_from_slice(b"jjjjjjj "); // index 4 + 7 = 11: Show Model (Line 1) -> Enabled
    keys.extend_from_slice(b"s"); // Save & Exit

    let opts = WizardRunOptions {
        cwd: env.workspace_dir.clone(),
        terminal_cols: Some(120),
        terminal_rows: Some(40),
        unicode: true,
        locale: Some("en_US.UTF-8".to_string()),
        observed_at_ms: Some(1_700_000_000_000),
        clear_screen: false,
    };

    let mut input = Cursor::new(keys);
    let mut output = Vec::new();
    let outcome = run_wizard_loop(&mut input, &mut output, &opts).unwrap();
    assert_eq!(outcome, WizardRunOutcome::Saved);

    // No stray .tmp files left behind
    let tmp_entries: Vec<_> = fs::read_dir(&env.global_cfg_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
        .collect();
    assert!(tmp_entries.is_empty(), "stray temp files found: {tmp_entries:?}");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&global_cfg).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "saved config must have 0600 permissions");
    }

    let saved: Value = serde_json::from_str(&fs::read_to_string(&global_cfg).unwrap()).unwrap();
    // Custom unknown keys preserved intact
    assert_eq!(saved["customOwner"], "keep-this");
    assert_eq!(saved["customArray"], serde_json::json!([1, 2, 3]));
    assert_eq!(saved["display"]["customDisplayFlag"], 42);
    assert_eq!(saved["thresholds"]["customThresholdTag"], "preserve");

    // Modified fields persisted
    assert_eq!(saved["language"], "zh");
    assert_eq!(saved["theme"]["primary"], "blue");
    assert_eq!(saved["display"]["quotaStyle"], "compact");
    assert_eq!(saved["display"]["showModel"], true);
    let warn_val = saved["thresholds"]["warning"].as_f64().unwrap();
    assert!((warn_val - 0.7).abs() > 1e-6, "warning threshold should have updated: {warn_val}");

    // Now execute the real `agy-pulse` binary and confirm statusline reflects the saved config!
    let stdin_payload = serde_json::json!({
        "model": { "id": "gemini-4-carbon-high", "display_name": "Gemini 4 Carbon (High)" },
        "workspace": { "current_dir": env.workspace_dir.to_string_lossy() },
        "context_window": {
            "context_window_size": 1000000,
            "used_percentage": 38.4,
            "total_input_tokens": 196600,
            "total_output_tokens": 13900
        },
        "quota": {
            "gemini-5h": { "remaining_fraction": 0.92 },
            "claude-5h": { "remaining_fraction": 0.70 }
        }
    })
    .to_string();

    let (code, stdout, stderr) = env.run_stdin(&agy_pulse_bin(), &stdin_payload);
    assert_eq!(code, 0, "stderr: {stderr}");
    let plain = strip_ansi(&stdout);
    assert!(stdout.contains("\x1b[34m"), "expected Ocean Blue ANSI primary color in statusline: {stdout:?}");
    assert!(plain.contains("上下文"), "expected Chinese statusline label after saving language=zh: {plain}");
    assert!(plain.contains("Google:"), "expected compact quota line after saving quotaStyle=compact: {plain}");
    assert!(plain.contains("Gemini 4 Carbon"), "expected model on Line 1 after showModel=true: {plain}");
}

#[test]
fn t06_wizard_scope_toggle_saves_local_when_selected() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let env = IsolatedEnv::new();
    let _guard = EnvVarGuard::set("AGY_PULSE_GLOBAL_CONFIG_DIR", &env.global_cfg_dir);

    let global_cfg = env.global_cfg_dir.join("agy-pulse.config.json");
    let local_cfg = env.workspace_dir.join("agy-pulse.config.json");
    fs::write(&global_cfg, "{\n  \"language\": \"en\"\n}\n").unwrap();

    // Index 0 is Scope (initially Global since local doesn't exist).
    // Press Space -> toggles Scope to Local, then '2' + Space -> Language: zh, then 's' -> Save & Exit.
    let keys = b" 2 s".to_vec();
    let opts = WizardRunOptions {
        cwd: env.workspace_dir.clone(),
        terminal_cols: Some(120),
        terminal_rows: Some(40),
        unicode: true,
        locale: Some("en_US.UTF-8".to_string()),
        observed_at_ms: Some(1_700_000_000_000),
        clear_screen: false,
    };

    let mut input = Cursor::new(keys);
    let mut output = Vec::new();
    let outcome = run_wizard_loop(&mut input, &mut output, &opts).unwrap();
    assert_eq!(outcome, WizardRunOutcome::Saved);

    assert!(local_cfg.exists(), "local config should be created when Scope toggled to Local");
    let local_val: Value = serde_json::from_str(&fs::read_to_string(&local_cfg).unwrap()).unwrap();
    assert_eq!(local_val["language"], "zh");
    // Global config remained untouched
    let global_val: Value = serde_json::from_str(&fs::read_to_string(&global_cfg).unwrap()).unwrap();
    assert_eq!(global_val["language"], "en");
}
