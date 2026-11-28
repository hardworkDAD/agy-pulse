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

    fs::create_dir_all(env.workspace_dir.join(".git")).unwrap();
    fs::write(
        env.workspace_dir.join(".git/HEAD"),
        "ref: refs/heads/feat/wizard-branch\n",
    )
    .unwrap();

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
    "showGitBranch": false,
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
    // '5' -> index 4 (Warning Threshold): Space -> next preset (!= 0.7)
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
    // 11: Show Git Branch
    let mut keys = Vec::new();
    keys.extend_from_slice(b"2 "); // index 1: Language -> zh
    keys.extend_from_slice(b"3 "); // index 2: Theme Preset -> Ocean Blue
    keys.extend_from_slice(b"4 "); // index 3: Quota Display Mode -> Compact
    keys.extend_from_slice(b"5 "); // index 4: Warning Threshold -> next preset (!= 0.7)
    keys.extend_from_slice(b"jjjjjjj "); // index 4 + 7 = 11: Show Git Branch -> Enabled
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
    assert_eq!(saved["display"]["showGitBranch"], true);
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
    assert!(plain.contains("feat/wizard-branch"), "expected git branch on Line 1 after showGitBranch=true: {plain}");
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

struct MultiEnvGuard {
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl MultiEnvGuard {
    fn new() -> Self {
        Self { saved: Vec::new() }
    }

    fn set(&mut self, key: &'static str, val: &str) {
        if !self.saved.iter().any(|(k, _)| *k == key) {
            self.saved.push((key, std::env::var_os(key)));
        }
        // SAFETY: guarded by `ENV_MUTEX` in integration tests.
        unsafe { std::env::set_var(key, val) };
    }

    fn remove(&mut self, key: &'static str) {
        if !self.saved.iter().any(|(k, _)| *k == key) {
            self.saved.push((key, std::env::var_os(key)));
        }
        // SAFETY: guarded by `ENV_MUTEX` in integration tests.
        unsafe { std::env::remove_var(key) };
    }
}

impl Drop for MultiEnvGuard {
    fn drop(&mut self) {
        for (key, prev) in self.saved.iter().rev() {
            match prev {
                // SAFETY: guarded by `ENV_MUTEX` in integration tests.
                Some(v) => unsafe { std::env::set_var(key, v) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
    }
}

#[test]
fn t06_pulse_config_snapshot_validation_preserving_save_and_preview() {
    use agy_pulse::config::{
        ABSENT_CONFIG_VERSION, CacheHitThresholdsConfig, IconsConfig, LanguageSetting, PulseConfig,
        THEME_PRESETS, ThemeColors, ThemeConfig, read_global_config_snapshot,
        read_global_config_snapshot_at, read_pulse_env_overrides, render_statusline_preview,
        save_global_pulse_config_preserving_unknown, validate_pulse_config,
    };
    use sha2::{Digest, Sha256};

    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let env = IsolatedEnv::new();
    let mut env_guard = MultiEnvGuard::new();
    env_guard.set(
        "AGY_PULSE_GLOBAL_CONFIG_DIR",
        &env.global_cfg_dir.to_string_lossy(),
    );
    env_guard.set("AGY_PULSE_LANGUAGE", "zh");
    env_guard.set("AGY_PULSE_DASHBOARD_PORT", "4999");
    env_guard.set("AGY_PULSE_DASHBOARD_IDLE_SEC", "900");
    env_guard.set("AGY_PULSE_FORCE_ASCII", "1");
    env_guard.set("AGY_PULSE_FORCE_UNICODE", "1");

    // 1. Absent file snapshot
    let absent_snap = read_global_config_snapshot().unwrap();
    assert!(!absent_snap.exists);
    assert_eq!(absent_snap.version, ABSENT_CONFIG_VERSION);
    assert!(absent_snap.raw_object.is_none());
    assert!(absent_snap.parse_error.is_none());
    assert_eq!(absent_snap.config.language, LanguageSetting::Auto);

    // 2. Env overrides metadata (FORCE_ASCII wins over FORCE_UNICODE)
    let overrides = read_pulse_env_overrides();
    assert_eq!(
        overrides.language.as_ref().map(|o| (o.env_var, o.value.as_str())),
        Some(("AGY_PULSE_LANGUAGE", "zh"))
    );
    assert_eq!(
        overrides
            .dashboard_port
            .as_ref()
            .map(|o| (o.env_var, o.value.as_str())),
        Some(("AGY_PULSE_DASHBOARD_PORT", "4999"))
    );
    assert_eq!(
        overrides
            .dashboard_idle_timeout_sec
            .as_ref()
            .map(|o| (o.env_var, o.value.as_str())),
        Some(("AGY_PULSE_DASHBOARD_IDLE_SEC", "900"))
    );
    assert_eq!(
        overrides.unicode.as_ref().map(|o| (o.env_var, o.value.as_str())),
        Some(("AGY_PULSE_FORCE_ASCII", "false"))
    );
    env_guard.remove("AGY_PULSE_FORCE_ASCII");
    let overrides_unicode = read_pulse_env_overrides();
    assert_eq!(
        overrides_unicode
            .unicode
            .as_ref()
            .map(|o| (o.env_var, o.value.as_str())),
        Some(("AGY_PULSE_FORCE_UNICODE", "true"))
    );

    // 3. Snapshot ignores AGY_PULSE_LANGUAGE and records parse_error for invalid JSON
    let global_cfg_path = env.global_cfg_dir.join("agy-pulse.config.json");
    fs::write(&global_cfg_path, "{broken json").unwrap();
    let bad_snap = read_global_config_snapshot_at(&global_cfg_path).unwrap();
    assert!(bad_snap.exists);
    assert!(bad_snap.parse_error.is_some());
    assert!(bad_snap.raw_object.is_none());

    // 4. Preserving unknown fields (columnWidth, custom top/nested keys, unmanaged icons.*) & clearing optional fields
    let initial_json = serde_json::json!({
        "language": "en",
        "customTop": { "keep": true },
        "display": {
            "columnWidth": 24,
            "customDisplay": "stay",
            "dashboardPort": 3888,
            "dashboardIdleTimeoutSec": 120,
            "unicode": true,
            "terminalWidth": 110,
            "username": "old-user",
            "cacheHitThresholds": { "excellent": 80.0, "partial": 40.0 },
            "modelSpeedExpireTurns": 3,
            "modelSpeedExpireMinutes": 5.0
        },
        "icons": {
            "branch": "B",
            "token": "T",
            "plan": "P",
            "customIcon": "X"
        }
    });
    fs::write(&global_cfg_path, initial_json.to_string()).unwrap();

    let snap = read_global_config_snapshot_at(&global_cfg_path).unwrap();
    assert!(snap.exists);
    assert!(snap.parse_error.is_none());
    assert_eq!(snap.config.language, LanguageSetting::En); // NOT overridden by AGY_PULSE_LANGUAGE=zh
    assert_eq!(snap.config.display.dashboard_port, Some(3888));

    let mut updated = snap.config.clone();
    updated.language = LanguageSetting::Zh;
    updated.display.dashboard_port = None;
    updated.display.dashboard_idle_timeout_sec = None;
    updated.display.unicode = None;
    updated.display.terminal_width = None;
    updated.display.username = Some("   ".to_string());
    updated.display.cache_hit_thresholds = None;
    updated.display.model_speed_expire_turns = None;
    updated.display.model_speed_expire_minutes = None;
    updated.icons = Some(IconsConfig {
        branch: None,
        ..Default::default()
    });
    assert!(validate_pulse_config(&updated).is_ok());

    let new_ver = save_global_pulse_config_preserving_unknown(
        &global_cfg_path,
        snap.raw_object.as_ref(),
        &updated,
    )
    .unwrap();
    let saved_bytes = fs::read(&global_cfg_path).unwrap();
    assert_eq!(new_ver, hex::encode(Sha256::digest(&saved_bytes)));
    let saved_val: Value = serde_json::from_slice(&saved_bytes).unwrap();
    assert_eq!(saved_val["customTop"]["keep"], true);
    assert_eq!(saved_val["display"]["columnWidth"], 24);
    assert_eq!(saved_val["display"]["customDisplay"], "stay");
    for removed in [
        "dashboardPort",
        "dashboardIdleTimeoutSec",
        "unicode",
        "terminalWidth",
        "username",
        "cacheHitThresholds",
        "modelSpeedExpireTurns",
        "modelSpeedExpireMinutes",
    ] {
        assert!(
            saved_val["display"].get(removed).is_none(),
            "cleared optional key {removed} must be removed"
        );
    }
    assert!(saved_val["icons"].get("branch").is_none());
    assert_eq!(saved_val["icons"]["token"], "T");
    assert_eq!(saved_val["icons"]["plan"], "P");
    assert_eq!(saved_val["icons"]["customIcon"], "X");

    // When all managed + unmanaged icons are cleared, "icons" object itself is removed
    updated.icons = None;
    let empty_raw = serde_json::Map::new();
    save_global_pulse_config_preserving_unknown(&global_cfg_path, Some(&empty_raw), &updated)
        .unwrap();
    let saved_no_icons: Value =
        serde_json::from_str(&fs::read_to_string(&global_cfg_path).unwrap()).unwrap();
    assert!(saved_no_icons.get("icons").is_none());

    // 5. Validation failures
    let mut bad = PulseConfig::default();
    bad.thresholds.warning = 0.95;
    bad.thresholds.critical = 0.8;
    assert_eq!(validate_pulse_config(&bad).unwrap_err().0, "thresholds.warning");

    let bad_theme = PulseConfig {
        theme: ThemeConfig::Colors(ThemeColors {
            primary: "neon-pink".to_string(),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(validate_pulse_config(&bad_theme).unwrap_err().0, "theme.primary");

    let ok_preset = PulseConfig {
        theme: ThemeConfig::Preset(THEME_PRESETS[1].name.to_string()),
        ..Default::default()
    };
    assert!(validate_pulse_config(&ok_preset).is_ok());

    let mut bad_port = PulseConfig::default();
    bad_port.display.dashboard_port = Some(0);
    assert_eq!(
        validate_pulse_config(&bad_port).unwrap_err().0,
        "display.dashboardPort"
    );

    let mut bad_width = PulseConfig::default();
    bad_width.display.terminal_width = Some(19);
    assert_eq!(
        validate_pulse_config(&bad_width).unwrap_err().0,
        "display.terminalWidth"
    );

    let mut bad_cht = PulseConfig::default();
    bad_cht.display.cache_hit_thresholds = Some(CacheHitThresholdsConfig {
        excellent: Some(40.0),
        partial: Some(60.0),
    });
    assert_eq!(
        validate_pulse_config(&bad_cht).unwrap_err().0,
        "display.cacheHitThresholds.partial"
    );

    let bad_icon = PulseConfig {
        icons: Some(IconsConfig {
            branch: Some("123456789".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(validate_pulse_config(&bad_icon).unwrap_err().0, "icons.branch");

    // 6. Preview helper consistency (disabled, empty, active)
    let disabled_cfg = PulseConfig {
        enabled: false,
        ..Default::default()
    };
    assert!(render_statusline_preview(&disabled_cfg, true, Some(100), Some("en_US.UTF-8"))
        .contains("disabled"));

    let mut empty_cfg = PulseConfig::default();
    empty_cfg.display.show_context_monitor = Some(false);
    empty_cfg.display.show_model_cache = Some(false);
    empty_cfg.display.show_current_dir = Some(false);
    empty_cfg.display.show_cycle_mode = Some(false);
    empty_cfg.display.show_speed = Some(false);
    empty_cfg.display.show_dashboard_url = Some(false);
    empty_cfg.display.quota_style = agy_pulse::config::QuotaStyle::None;
    let all_off_rendered = strip_ansi(&render_statusline_preview(
        &empty_cfg,
        true,
        Some(100),
        Some("en_US.UTF-8"),
    ));
    assert!(all_off_rendered.contains("Pro"));
    assert!(!all_off_rendered.contains("Context"));
    assert!(!all_off_rendered.contains("agy-pulse"));
    assert!(!all_off_rendered.contains("Google"));

    let zh_cfg = PulseConfig {
        language: LanguageSetting::Zh,
        theme: ThemeConfig::Preset("Ocean Blue".to_string()),
        ..Default::default()
    };
    let rendered = render_statusline_preview(&zh_cfg, true, Some(120), Some("en_US.UTF-8"));
    let plain = strip_ansi(&rendered);
    assert!(rendered.contains("\x1b[34m"));
    assert!(plain.contains("上下文"));
}

#[test]
fn t06_icons_all_five_fields_validated_and_persisted() {
    use agy_pulse::config::{
        IconsConfig, PulseConfig, read_global_config_snapshot_at,
        save_global_pulse_config_preserving_unknown, validate_pulse_config,
    };

    let env = IsolatedEnv::new();
    let cfg_path = env.global_cfg_dir.join("agy-pulse.config.json");

    // 1. All 5 icon fields must reject >8 columns and control characters
    for (field, mk_bad_width, mk_bad_ctrl) in [
        (
            "icons.branch",
            IconsConfig {
                branch: Some("123456789".to_string()),
                ..Default::default()
            },
            IconsConfig {
                branch: Some("a\tb".to_string()),
                ..Default::default()
            },
        ),
        (
            "icons.plan",
            IconsConfig {
                plan: Some("123456789".to_string()),
                ..Default::default()
            },
            IconsConfig {
                plan: Some("a\nb".to_string()),
                ..Default::default()
            },
        ),
        (
            "icons.step",
            IconsConfig {
                step: Some("123456789".to_string()),
                ..Default::default()
            },
            IconsConfig {
                step: Some("a\rb".to_string()),
                ..Default::default()
            },
        ),
        (
            "icons.task",
            IconsConfig {
                task: Some("123456789".to_string()),
                ..Default::default()
            },
            IconsConfig {
                task: Some("a\x1bb".to_string()),
                ..Default::default()
            },
        ),
        (
            "icons.model",
            IconsConfig {
                model: Some("123456789".to_string()),
                ..Default::default()
            },
            IconsConfig {
                model: Some("a\0b".to_string()),
                ..Default::default()
            },
        ),
    ] {
        let cfg_w = PulseConfig {
            icons: Some(mk_bad_width),
            ..Default::default()
        };
        assert_eq!(validate_pulse_config(&cfg_w).unwrap_err().0, field);

        let cfg_c = PulseConfig {
            icons: Some(mk_bad_ctrl),
            ..Default::default()
        };
        assert_eq!(validate_pulse_config(&cfg_c).unwrap_err().0, field);
    }

    // 2. Saving all 5 fields persists trimmed values while keeping unknown icon keys
    fs::write(
        &cfg_path,
        serde_json::json!({ "icons": { "customIcon": "X" } }).to_string(),
    )
    .unwrap();
    let snap0 = read_global_config_snapshot_at(&cfg_path).unwrap();
    let mut cfg = snap0.config.clone();
    cfg.icons = Some(IconsConfig {
        branch: Some(" B ".to_string()),
        plan: Some(" P ".to_string()),
        step: Some(" S ".to_string()),
        task: Some(" T ".to_string()),
        model: Some(" M ".to_string()),
    });
    assert!(validate_pulse_config(&cfg).is_ok());
    save_global_pulse_config_preserving_unknown(&cfg_path, snap0.raw_object.as_ref(), &cfg)
        .unwrap();

    let saved: Value = serde_json::from_str(&fs::read_to_string(&cfg_path).unwrap()).unwrap();
    assert_eq!(saved["icons"]["branch"], "B");
    assert_eq!(saved["icons"]["plan"], "P");
    assert_eq!(saved["icons"]["step"], "S");
    assert_eq!(saved["icons"]["task"], "T");
    assert_eq!(saved["icons"]["model"], "M");
    assert_eq!(saved["icons"]["customIcon"], "X");

    let snap1 = read_global_config_snapshot_at(&cfg_path).unwrap();
    assert_eq!(
        snap1.config.icons,
        Some(IconsConfig {
            branch: Some("B".to_string()),
            plan: Some("P".to_string()),
            step: Some("S".to_string()),
            task: Some("T".to_string()),
            model: Some("M".to_string()),
        })
    );
}

#[test]
fn t06_partial_type_error_preserves_valid_fields_and_reports_field_warnings() {
    use agy_pulse::config::{
        CacheHitThresholdsConfig, IconsConfig, LanguageSetting, ThemeColors, ThemeConfig,
        load_config, read_global_config_snapshot_at,
    };

    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let env = IsolatedEnv::new();
    let mut env_guard = MultiEnvGuard::new();
    env_guard.set(
        "AGY_PULSE_GLOBAL_CONFIG_DIR",
        &env.global_cfg_dir.to_string_lossy(),
    );
    env_guard.remove("AGY_PULSE_LANGUAGE");

    let global_cfg = env.global_cfg_dir.join("agy-pulse.config.json");
    let mixed_json = serde_json::json!({
        "enabled": "yes",
        "language": "zh",
        "theme": "Ocean Blue",
        "thresholds": {
            "warning": 0.65,
            "critical": "bad"
        },
        "display": {
            "showGitBranch": true,
            "dashboardPort": "3789",
            "breadcrumbCount": 5,
            "cacheHitThresholds": {
                "excellent": "nope",
                "partial": 45.0
            }
        },
        "icons": {
            "branch": 123,
            "plan": "P"
        }
    });
    fs::write(&global_cfg, mixed_json.to_string()).unwrap();

    let loaded = load_config(&env.workspace_dir);
    assert!(loaded.enabled);
    assert_eq!(loaded.language, LanguageSetting::Zh);
    assert_eq!(loaded.theme, ThemeConfig::Preset("Ocean Blue".to_string()));
    assert!((loaded.thresholds.warning - 0.65).abs() < 1e-6);
    assert!((loaded.thresholds.critical - 0.9).abs() < 1e-6);
    assert_eq!(loaded.display.show_git_branch, Some(true));
    assert_eq!(loaded.display.dashboard_port, None);
    assert_eq!(loaded.display.breadcrumb_count, Some(5));
    assert_eq!(
        loaded.display.cache_hit_thresholds,
        Some(CacheHitThresholdsConfig {
            excellent: None,
            partial: Some(45.0),
        })
    );
    assert_eq!(
        loaded.icons,
        Some(IconsConfig {
            branch: None,
            plan: Some("P".to_string()),
            ..Default::default()
        })
    );

    let snap = read_global_config_snapshot_at(&global_cfg).unwrap();
    assert!(snap.parse_error.is_none());
    assert!(snap.raw_object.is_some());
    assert_eq!(snap.config, loaded);
    for expected_path in [
        "enabled",
        "thresholds.critical",
        "display.dashboardPort",
        "display.cacheHitThresholds.excellent",
        "icons.branch",
    ] {
        assert!(
            snap.field_warnings
                .iter()
                .any(|w| w.contains(expected_path)),
            "expected warning for {expected_path}, got: {:?}",
            snap.field_warnings
        );
    }

    // Object theme with partial invalid color preserves sibling colors and warns on bad field
    let theme_json = serde_json::json!({
        "language": "en",
        "theme": {
            "primary": 123,
            "secondary": "cyan"
        }
    });
    fs::write(&global_cfg, theme_json.to_string()).unwrap();
    let theme_snap = read_global_config_snapshot_at(&global_cfg).unwrap();
    assert_eq!(theme_snap.config.language, LanguageSetting::En);
    assert_eq!(
        theme_snap.config.theme,
        ThemeConfig::Colors(ThemeColors {
            secondary: "cyan".to_string(),
            ..ThemeColors::default()
        })
    );
    assert!(
        theme_snap
            .field_warnings
            .iter()
            .any(|w| w.contains("theme.primary")),
        "expected theme.primary warning: {:?}",
        theme_snap.field_warnings
    );

    // Non-object display/thresholds groups fall back cleanly without discarding language
    let group_json = serde_json::json!({
        "language": "zh",
        "thresholds": 42,
        "display": "not-an-object"
    });
    fs::write(&global_cfg, group_json.to_string()).unwrap();
    let group_snap = read_global_config_snapshot_at(&global_cfg).unwrap();
    assert_eq!(group_snap.config.language, LanguageSetting::Zh);
    assert!(
        group_snap.field_warnings.iter().any(|w| w.contains("thresholds"))
            && group_snap.field_warnings.iter().any(|w| w.contains("display")),
        "expected group warnings: {:?}",
        group_snap.field_warnings
    );
}

#[test]
fn t06_wizard_escape_timeout_cancels_without_eof_while_preserving_arrows() {
    use std::io::{self, Read};

    struct TimeoutAfterEscReader {
        chunks: Vec<io::Result<u8>>,
        pos: usize,
    }

    impl Read for TimeoutAfterEscReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if buf.is_empty() {
                return Ok(0);
            }
            if self.pos >= self.chunks.len() {
                return Ok(0);
            }
            let item = std::mem::replace(&mut self.chunks[self.pos], Ok(0));
            self.pos += 1;
            match item {
                Ok(b) => {
                    buf[0] = b;
                    Ok(1)
                }
                Err(e) => Err(e),
            }
        }
    }

    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let env = IsolatedEnv::new();
    let _guard = EnvVarGuard::set("AGY_PULSE_GLOBAL_CONFIG_DIR", &env.global_cfg_dir);

    let mut chunks: Vec<io::Result<u8>> = b"\x1b[B \x1b".iter().copied().map(Ok).collect();
    chunks.push(Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "escape followup timeout",
    )));

    let mut input = TimeoutAfterEscReader { chunks, pos: 0 };
    let mut output = Vec::new();
    let opts = WizardRunOptions {
        cwd: env.workspace_dir.clone(),
        terminal_cols: Some(120),
        terminal_rows: Some(40),
        unicode: true,
        locale: Some("en_US.UTF-8".to_string()),
        observed_at_ms: Some(1_700_000_000_000),
        clear_screen: false,
    };

    let outcome = run_wizard_loop(&mut input, &mut output, &opts).unwrap();
    assert_eq!(outcome, WizardRunOutcome::Cancelled);
}

#[test]
fn t06_wizard_rejects_invalid_thresholds_on_save_and_keeps_editing() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let env = IsolatedEnv::new();
    let _guard = EnvVarGuard::set("AGY_PULSE_GLOBAL_CONFIG_DIR", &env.global_cfg_dir);

    let global_cfg = env.global_cfg_dir.join("agy-pulse.config.json");
    let seed_json = "{\n  \"language\": \"en\",\n  \"thresholds\": {\n    \"warning\": 0.85,\n    \"critical\": 0.85\n  }\n}\n";
    fs::write(&global_cfg, seed_json).unwrap();

    let opts = WizardRunOptions {
        cwd: env.workspace_dir.clone(),
        terminal_cols: Some(120),
        terminal_rows: Some(40),
        unicode: true,
        locale: Some("en_US.UTF-8".to_string()),
        observed_at_ms: Some(1_700_000_000_000),
        clear_screen: true,
    };

    // Case 1: Enter on Save item when warning (0.85) > critical (0.80) refuses to save, keeps editing, then 'q' exits untouched.
    // '6' selects index 5 (Critical Threshold); 3 spaces cycle 0.85 -> 0.90 -> 0.95 -> 0.80.
    // Then navigate to Save (last - 1) via 'k'/'k' from wrapped top or pressing 's', plus MenuItem::Save via digits/j.
    let mut keys_abort = Vec::new();
    keys_abort.extend_from_slice(b"6   "); // critical -> 0.80 (80% < 85%)
    keys_abort.extend_from_slice(b"s"); // rejected!
    // Also move down to MenuItem::Save (17 items below index 5 -> index 21 is Save, total 23 items)
    keys_abort.extend_from_slice(&[b'j'; 16]);
    keys_abort.extend_from_slice(b"\r"); // Enter on Save -> also rejected!
    keys_abort.extend_from_slice(b"q"); // Cancel without saving

    let mut in1 = Cursor::new(keys_abort);
    let mut out1 = Vec::new();
    let outcome1 = run_wizard_loop(&mut in1, &mut out1, &opts).unwrap();
    assert_eq!(outcome1, WizardRunOutcome::Cancelled);
    assert_eq!(fs::read_to_string(&global_cfg).unwrap(), seed_json);

    let plain1 = strip_ansi(&String::from_utf8_lossy(&out1));
    assert!(
        plain1.contains("thresholds.warning"),
        "expected validation error naming thresholds.warning in wizard frame: {plain1}"
    );

    // Case 2: Invalid save rejected, user stays in wizard, edits Language -> zh, fixes Critical -> 0.85, then 's' saves!
    let mut keys_fix = Vec::new();
    keys_fix.extend_from_slice(b"6   "); // critical -> 0.80 (invalid)
    keys_fix.extend_from_slice(b"s"); // rejected, stays in wizard
    keys_fix.extend_from_slice(b"2 "); // Language: en -> zh (still in wizard)
    keys_fix.extend_from_slice(b"6 "); // critical: 0.80 -> 0.85 (now valid)
    keys_fix.extend_from_slice(b"s"); // succeeds!

    let mut in2 = Cursor::new(keys_fix);
    let mut out2 = Vec::new();
    let outcome2 = run_wizard_loop(&mut in2, &mut out2, &opts).unwrap();
    assert_eq!(outcome2, WizardRunOutcome::Saved);

    let saved: Value = serde_json::from_str(&fs::read_to_string(&global_cfg).unwrap()).unwrap();
    assert_eq!(saved["language"], "zh");
    assert!((saved["thresholds"]["warning"].as_f64().unwrap() - 0.85).abs() < 1e-6);
    assert!((saved["thresholds"]["critical"].as_f64().unwrap() - 0.85).abs() < 1e-6);
}

#[test]
fn t06_wizard_scope_when_cwd_is_global_dir_and_scope_toggle_preview() {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let env = IsolatedEnv::new();
    let _guard = EnvVarGuard::set("AGY_PULSE_GLOBAL_CONFIG_DIR", &env.global_cfg_dir);

    let global_cfg = env.global_cfg_dir.join("agy-pulse.config.json");
    fs::write(
        &global_cfg,
        serde_json::json!({
            "language": "en",
            "theme": "Emerald Green"
        })
        .to_string(),
    )
    .unwrap();

    // Part A: cwd == global config directory (with non-normalized `.`) must default to Global, not Local.
    let opts_same = WizardRunOptions {
        cwd: env.global_cfg_dir.join("."),
        terminal_cols: Some(120),
        terminal_rows: Some(40),
        unicode: true,
        locale: Some("en_US.UTF-8".to_string()),
        observed_at_ms: Some(1_700_000_000_000),
        clear_screen: true,
    };
    let mut in_same = Cursor::new(b"q".to_vec());
    let mut out_same = Vec::new();
    let outcome_same = run_wizard_loop(&mut in_same, &mut out_same, &opts_same).unwrap();
    assert_eq!(outcome_same, WizardRunOutcome::Cancelled);
    let plain_same = strip_ansi(&String::from_utf8_lossy(&out_same));
    assert!(
        plain_same.contains("Configuration Scope: Global (All Projects)"),
        "expected Global scope when cwd == global config dir, got: {plain_same}"
    );
    assert!(!plain_same.contains("Configuration Scope: Local"));

    // Part B: Workspace has Local overrides (zh + Ocean Blue). Toggling Scope Local -> Global
    // must refresh both menu items and live preview to Global (en + Emerald Green) without workspace leakage.
    let local_cfg = env.workspace_dir.join("agy-pulse.config.json");
    fs::write(
        &local_cfg,
        serde_json::json!({
            "language": "zh",
            "theme": "Ocean Blue",
            "display": { "showGitBranch": true }
        })
        .to_string(),
    )
    .unwrap();

    let opts_local = WizardRunOptions {
        cwd: env.workspace_dir.clone(),
        terminal_cols: Some(120),
        terminal_rows: Some(40),
        unicode: true,
        locale: Some("en_US.UTF-8".to_string()),
        observed_at_ms: Some(1_700_000_000_000),
        clear_screen: true,
    };
    let mut in_toggle = Cursor::new(b" q".to_vec());
    let mut out_toggle = Vec::new();
    let outcome_toggle = run_wizard_loop(&mut in_toggle, &mut out_toggle, &opts_local).unwrap();
    assert_eq!(outcome_toggle, WizardRunOutcome::Cancelled);

    let raw = String::from_utf8_lossy(&out_toggle).into_owned();
    let frames: Vec<&str> = raw.split("\x1b[H\x1b[J").filter(|s| !s.is_empty()).collect();
    assert!(frames.len() >= 2, "expected initial frame and toggled frame, got {}", frames.len());

    let frame0_plain = strip_ansi(frames[0]);
    assert!(frame0_plain.contains("Configuration Scope: Local (Current Project Workspace)"));
    assert!(frame0_plain.contains("Language: zh"));
    assert!(frame0_plain.contains("Theme Preset: Ocean Blue"));
    assert!(frame0_plain.contains("上下文"));

    let frame1_plain = strip_ansi(frames[1]);
    assert!(
        frame1_plain.contains("Configuration Scope: Global (All Projects)"),
        "expected Global scope in frame 1: {frame1_plain}"
    );
    assert!(
        frame1_plain.contains("Language: en"),
        "expected Global language=en after toggling to Global: {frame1_plain}"
    );
    assert!(
        frame1_plain.contains("Theme Preset: Emerald Green"),
        "expected Global theme=Emerald Green after toggling to Global: {frame1_plain}"
    );
    assert!(
        !frame1_plain.contains("上下文") && frame1_plain.contains("Context"),
        "expected Global English preview without local zh leakage: {frame1_plain}"
    );
}
