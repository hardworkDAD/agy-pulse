#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

use agy_pulse::domain::{
    CostSnapshot, HostIdentity, ModelIdentity, PulseSnapshot, TokenUsage, WorkspaceInfo,
    is_usage_cache_inclusive, model_includes_cache_in_input, resolve_cascade_cost_usd,
    resolve_native_cost_usd,
};
use agy_pulse::render::{
    ansi_display_width, display_width, render_statusline, strip_ansi,
    truncate_ansi_to_display_width,
};
use agy_pulse::config::{DisplayConfig, LanguageSetting, PulseConfig};
use agy_pulse::platform::TerminalCaps;

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
        let data_dir = root.join("data");
        let global_cfg_dir = root.join("global_cfg");
        let xdg_cfg_dir = root.join("xdg_cfg");
        let app_data_dir = root.join("app_data");
        let local_app_data_dir = root.join("local_app_data");
        let workspace_dir = root.join("workspace");
        fs::create_dir_all(&home_dir).unwrap();
        fs::create_dir_all(&data_dir).unwrap();
        fs::create_dir_all(&global_cfg_dir).unwrap();
        fs::create_dir_all(&xdg_cfg_dir).unwrap();
        fs::create_dir_all(&app_data_dir).unwrap();
        fs::create_dir_all(&local_app_data_dir).unwrap();
        fs::create_dir_all(&workspace_dir).unwrap();
        Self {
            _temp: temp,
            home_dir,
            data_dir,
            global_cfg_dir,
            xdg_cfg_dir,
            app_data_dir,
            local_app_data_dir,
            workspace_dir,
        }
    }

    fn cmd(&self, bin: &PathBuf) -> Command {
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
            .env("AGY_PULSE_NO_DASHBOARD_AUTOSTART", "1")
            .env_remove("AGY_PULSE_FORCE_ASCII")
            .env_remove("AGY_PULSE_FORCE_UNICODE")
            .env_remove("AGY_PULSE_LANGUAGE")
            .env_remove("COLUMNS")
            .env("LC_ALL", "en_US.UTF-8")
            .env("LANG", "en_US.UTF-8");
        cmd
    }

    fn run_stdin(&self, bin: &PathBuf, stdin_text: &str, extra_env: &[(&str, &str)]) -> (i32, String, String) {
        let mut cmd = self.cmd(bin);
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn binary");

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(stdin_text.as_bytes()).expect("write stdin");
        }
        let output = child.wait_with_output().expect("wait output");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }
}

#[test]
fn cli_supports_version_and_help_on_both_binaries() {
    let env = IsolatedEnv::new();
    for bin in [agy_pulse_bin(), agypl_bin()] {
        let ver_out = env.cmd(&bin).arg("--version").output().unwrap();
        assert_eq!(ver_out.status.code(), Some(0));
        let ver_text = String::from_utf8_lossy(&ver_out.stdout);
        assert!(
            ver_text.contains("2.0.0"),
            "version output must contain 2.0.0, got: {ver_text}"
        );

        let help_out = env.cmd(&bin).arg("--help").output().unwrap();
        assert_eq!(help_out.status.code(), Some(0));
        let help_text = String::from_utf8_lossy(&help_out.stdout);
        assert!(help_text.contains("agypl"));
        assert!(help_text.contains("agy-pulse"));
        assert!(
            !help_text.contains("diagnose"),
            "v2 help must not advertise retired diagnose command: {help_text}"
        );
    }
}

#[test]
fn end_to_end_statusline_renders_first_line_git_branch_cost_and_context_bar() {
    let env = IsolatedEnv::new();
    let repo_dir = env.workspace_dir.join("my-project");
    fs::create_dir_all(repo_dir.join(".git/refs/heads")).unwrap();
    fs::write(repo_dir.join(".git/HEAD"), "ref: refs/heads/feat/tracer-bullet\n").unwrap();

    fs::write(
        env.global_cfg_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "display": {
                "showModel": true,
                "showGitBranch": true,
                "showCost": true,
                "showContextBar": true,
                "showContextMonitor": true,
                "showCurrentDir": true
            }
        })
        .to_string(),
    )
    .unwrap();

    let payload = serde_json::json!({
        "conversation_id": "conv-t01",
        "model": {
            "id": "claude-opus-4-6-thinking",
            "display_name": "Claude Opus 4.6",
            "effort": "Thinking"
        },
        "plan_tier": "Pro",
        "cycle_mode": "auto",
        "workspace": {
            "current_dir": repo_dir.to_string_lossy()
        },
        "context_window": {
            "context_window_size": 500000,
            "used_percentage": 42.0,
            "total_input_tokens": 210000,
            "total_output_tokens": 15000
        },
        "cost": {
            "total_cost_usd": 0.45
        }
    })
    .to_string();

    let (code, stdout, stderr) = env.run_stdin(&agy_pulse_bin(), &payload, &[("COLUMNS", "120")]);
    assert_eq!(code, 0);
    assert_eq!(stderr, "");

    let plain = strip_ansi(&stdout);
    let lines: Vec<&str> = plain.lines().collect();
    assert!(lines.len() >= 3, "expected at least 3 lines, got:\n{plain}");

    let line1 = lines[0];
    assert!(line1.contains("my-project"), "line1 missing dir: {line1}");
    assert!(line1.contains("feat/tracer-bullet"), "line1 missing branch: {line1}");
    assert!(line1.contains("Opus 4.6(Th)"), "line1 missing model+effort: {line1}");
    assert!(line1.contains("Pro"), "line1 missing plan tier: {line1}");
    assert!(line1.contains("auto"), "line1 missing cycle mode: {line1}");
    assert!(line1.contains("Cost $0.450"), "line1 missing cost: {line1}");

    let line2 = lines[1];
    assert!(line2.contains("210k/500k"), "line2 missing ctx ratio: {line2}");
    assert!(line2.contains("42%"), "line2 missing ctx pct: {line2}");

    let monitor = lines.last().unwrap();
    assert!(monitor.contains("Claude Opus 4.6 (Thinking)"), "monitor missing full model+effort: {monitor}");
    assert!(monitor.contains("42.0%"), "monitor missing pct: {monitor}");
    assert!(monitor.contains("(210K/500K)"), "monitor missing K ratio: {monitor}");
    assert!(monitor.contains("Cost $0.45"), "monitor missing cost: {monitor}");
}

#[test]
fn git_worktree_dot_git_file_resolves_branch_without_error() {
    let env = IsolatedEnv::new();
    let real_git_dir = env.workspace_dir.join(".real_git_worktree");
    let wt_dir = env.workspace_dir.join("wt-proj");
    fs::create_dir_all(&real_git_dir).unwrap();
    fs::create_dir_all(&wt_dir).unwrap();
    fs::write(real_git_dir.join("HEAD"), "ref: refs/heads/fix/worktree-branch\n").unwrap();
    fs::write(
        wt_dir.join(".git"),
        format!("gitdir: {}\n", real_git_dir.to_string_lossy()),
    )
    .unwrap();
    fs::write(
        env.global_cfg_dir.join("agy-pulse.config.json"),
        r#"{"display":{"showGitBranch":true}}"#,
    )
    .unwrap();

    let payload = serde_json::json!({
        "workspace": { "current_dir": wt_dir.to_string_lossy() }
    })
    .to_string();

    let (code, stdout, stderr) = env.run_stdin(&agypl_bin(), &payload, &[]);
    assert_eq!(code, 0);
    assert_eq!(stderr, "");
    let plain = strip_ansi(&stdout);
    assert!(
        plain.contains("fix/worktree-branch"),
        "expected worktree branch in output, got:\n{plain}"
    );
}

#[test]
fn config_merges_priority_ignores_retired_v1_fields_and_respects_enabled_false() {
    let env = IsolatedEnv::new();

    // Global config with retired v1 fields + Ocean Blue preset + showModel=true
    fs::write(
        env.global_cfg_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "enabled": true,
            "theme": "Ocean Blue",
            "language": "en",
            "display": {
                "showModel": true,
                "showCost": true,
                "quotaStyle": "models"
            },
            "cloudQuota": { "enabled": true, "endpoints": ["https://legacy.example"] },
            "retiredUnknownField": "keep-ignored"
        })
        .to_string(),
    )
    .unwrap();

    // Workspace config overrides language to zh and disables cost, while inheriting showModel=true
    fs::write(
        env.workspace_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "language": "zh",
            "display": {
                "showCost": false
            }
        })
        .to_string(),
    )
    .unwrap();

    let payload = serde_json::json!({
        "model": { "id": "gemini-3.1-pro", "display_name": "Gemini 3.1 Pro (High)" },
        "plan_tier": "Ultra",
        "cost": { "total_cost_usd": 1.25 },
        "context_window": { "context_window_size": 1000000, "used_percentage": 10.0, "total_input_tokens": 100000 }
    })
    .to_string();

    let (code, stdout, stderr) = env.run_stdin(&agy_pulse_bin(), &payload, &[("COLUMNS", "120")]);
    assert_eq!(code, 0);
    assert_eq!(stderr, "");
    let plain = strip_ansi(&stdout);
    assert!(plain.contains("Gemini 3.1 Pro(H)"), "global showModel=true should persist: {plain}");
    assert!(plain.contains("上下文"), "workspace language=zh should override global en: {plain}");
    let first_line = plain.lines().next().unwrap_or("");
    assert!(
        !first_line.contains("费用") && !first_line.contains("$1.25"),
        "workspace showCost=false should override global on line 1: {plain}"
    );
    assert!(stdout.contains("\x1b[34m"), "Ocean Blue theme primary should apply blue ANSI: {stdout:?}");

    // Now disable via workspace config -> silent exit 0
    fs::write(
        env.workspace_dir.join("agy-pulse.config.json"),
        r#"{"enabled": false}"#,
    )
    .unwrap();
    let (code2, stdout2, stderr2) = env.run_stdin(&agy_pulse_bin(), &payload, &[]);
    assert_eq!(code2, 0);
    assert_eq!(stderr2, "");
    assert_eq!(stdout2, "");
}

#[test]
fn language_auto_detection_and_ascii_fallback() {
    let env = IsolatedEnv::new();
    fs::write(
        env.global_cfg_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "language": "auto",
            "display": {
                "showModel": true,
                "showCost": true,
                "showContextBar": true,
                "showContextMonitor": true
            }
        })
        .to_string(),
    )
    .unwrap();

    let payload = serde_json::json!({
        "model": { "display_name": "Claude Sonnet 4.6 (High)" },
        "plan_tier": "Pro",
        "cost": { "total_cost_usd": 2.5 },
        "context_window": {
            "context_window_size": 200000,
            "used_percentage": 50.0,
            "total_input_tokens": 100000
        }
    })
    .to_string();

    // Chinese locale + forced ASCII
    let (code, stdout, _) = env.run_stdin(
        &agy_pulse_bin(),
        &payload,
        &[("LC_ALL", "zh_CN.UTF-8"), ("AGY_PULSE_FORCE_ASCII", "1")],
    );
    assert_eq!(code, 0);
    let plain = strip_ansi(&stdout);
    assert!(plain.contains("费用 $2.50"), "zh locale must render 费用: {plain}");
    assert!(plain.contains("上下文"), "zh locale must render 上下文: {plain}");
    assert!(plain.contains(" | "), "ASCII mode must use | divider: {plain}");
    assert!(!plain.contains('│'), "ASCII mode must not emit │: {plain}");
    assert!(!plain.contains('█'), "ASCII mode must not emit █: {plain}");
    assert!(!plain.contains('◈'), "ASCII mode must not emit ◈: {plain}");

    // Force Unicode even under POSIX/C locale
    let (code2, stdout2, _) = env.run_stdin(
        &agy_pulse_bin(),
        &payload,
        &[("LC_ALL", "C"), ("LANG", "C"), ("AGY_PULSE_FORCE_UNICODE", "1")],
    );
    assert_eq!(code2, 0);
    let plain2 = strip_ansi(&stdout2);
    assert!(plain2.contains(" │ "), "forced Unicode must emit │: {plain2}");
    assert!(plain2.contains('◈'), "forced Unicode must emit ◈: {plain2}");
}

#[test]
fn narrow_terminal_truncates_cjk_emoji_and_ansi_safely() {
    let env = IsolatedEnv::new();
    let cjk_dir = env.workspace_dir.join("前端项目🚀测试目录");
    fs::create_dir_all(cjk_dir.join(".git/refs/heads")).unwrap();
    fs::write(cjk_dir.join(".git/HEAD"), "ref: refs/heads/功能/长分支名称测试\n").unwrap();

    fs::write(
        env.global_cfg_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "display": {
                "showModel": true,
                "showGitBranch": true,
                "showCost": true,
                "showContextMonitor": true
            }
        })
        .to_string(),
    )
    .unwrap();

    let payload = serde_json::json!({
        "model": { "display_name": "Claude Opus 4.6 (Thinking)" },
        "plan_tier": "Pro",
        "workspace": { "current_dir": cjk_dir.to_string_lossy() },
        "cost": { "total_cost_usd": 1.23 },
        "context_window": { "context_window_size": 200000, "used_percentage": 35.0, "total_input_tokens": 70000 }
    })
    .to_string();

    let (code, stdout, stderr) = env.run_stdin(
        &agy_pulse_bin(),
        &payload,
        &[("COLUMNS", "36")],
    );
    assert_eq!(code, 0);
    assert_eq!(stderr, "");
    for line in stdout.lines() {
        let w = ansi_display_width(line);
        assert!(
            w <= 36,
            "rendered line exceeds terminal width 36 (got {w}): {:?}",
            strip_ansi(line)
        );
        // Ensure no dangling CSI escape sequence
        let esc_count = line.matches('\x1b').count();
        if esc_count > 0 {
            assert!(
                line.ends_with("\x1b[0m") || !line.rsplit('\x1b').next().unwrap().starts_with('['),
                "broken ANSI sequence in line: {line:?}"
            );
        }
    }
}

#[test]
fn empty_and_malformed_stdin_exit_zero_and_log_safely_without_following_symlink() {
    let env = IsolatedEnv::new();

    // 1) Empty stdin exits 0 without writing error log
    let (code_empty, out_empty, err_empty) = env.run_stdin(&agy_pulse_bin(), "", &[]);
    assert_eq!(code_empty, 0);
    assert_eq!(err_empty, "");
    assert!(strip_ansi(&out_empty).contains("Free"));
    assert!(!env.data_dir.join("agy-pulse-error.log").exists());

    // 2) Plant a symlink at agy-pulse-error.log pointing to sentinel file
    #[cfg(unix)]
    {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let sentinel = env.data_dir.join("sentinel.txt");
        fs::write(&sentinel, "untouched").unwrap();
        let log_path = env.data_dir.join("agy-pulse-error.log");
        symlink(&sentinel, &log_path).unwrap();

        let (code_bad, out_bad, err_bad) = env.run_stdin(&agy_pulse_bin(), "{not valid json", &[]);
        assert_eq!(code_bad, 0);
        assert_eq!(err_bad, "");
        assert!(strip_ansi(&out_bad).contains("Free"));

        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "untouched");
        let meta = fs::symlink_metadata(&log_path).unwrap();
        assert!(!meta.file_type().is_symlink(), "error log symlink must be replaced");
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode & 0o177, 0, "error log must be 0o600, got {:o}", mode);
        let log_content = fs::read_to_string(&log_path).unwrap();
        assert!(!log_content.is_empty(), "diagnostic info should be logged on malformed JSON");
    }
}

#[test]
fn hanging_stdin_respects_1500ms_hard_timeout_and_exits_zero() {
    let env = IsolatedEnv::new();
    let start = Instant::now();
    let mut child = env
        .cmd(&agy_pulse_bin())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn binary");

    // Hold stdin open without writing EOF
    let _held_stdin = child.stdin.take().unwrap();
    let output = child.wait_with_output().expect("wait with timeout");
    let elapsed = start.elapsed();

    assert_eq!(output.status.code(), Some(0));
    assert!(
        elapsed >= Duration::from_millis(1300) && elapsed < Duration::from_millis(2600),
        "expected ~1500ms timeout exit, took {:?}",
        elapsed
    );
}

#[test]
fn pure_domain_and_render_boundary_invariants() {
    // 1) Routed cost filtering (Bedrock & Vertex)
    assert_eq!(
        resolve_native_cost_usd(Some(1.23), "anthropic.claude-3-5-sonnet-20241022-v2:0", false),
        None
    );
    assert_eq!(
        resolve_native_cost_usd(Some(1.23), "claude-3-5-sonnet-v2@20241022", false),
        None
    );
    assert_eq!(
        resolve_native_cost_usd(Some(1.23), "claude-3-5-sonnet-v2@20241022", true),
        Some(1.23)
    );
    assert_eq!(
        resolve_native_cost_usd(Some(1.23), "claude-opus-4-6-thinking", false),
        Some(1.23)
    );

    // 1b) Cache-inclusive unification across Anthropic vs Gemini and cascade_usd fallback
    assert!(!model_includes_cache_in_input("claude-opus-4-6-thinking"));
    assert!(model_includes_cache_in_input("gemini-3.1-pro"));
    let anthropic_folded = TokenUsage {
        input_tokens: 1000,
        output_tokens: 50,
        cache_read_input_tokens: 600,
        cache_creation_input_tokens: 100,
        cache_exclusive: false,
    };
    assert!(is_usage_cache_inclusive("Claude Opus 4.6", &anthropic_folded));
    assert!(!is_usage_cache_inclusive(
        "Claude Opus 4.6",
        &TokenUsage {
            cache_exclusive: true,
            ..anthropic_folded.clone()
        }
    ));
    assert!(!is_usage_cache_inclusive(
        "Claude Opus 4.6",
        &TokenUsage {
            input_tokens: 500,
            ..anthropic_folded
        }
    ));
    let cascade_fallback = PulseSnapshot {
        cost: CostSnapshot {
            cascade_usd: Some(0.77),
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(resolve_cascade_cost_usd(&cascade_fallback, false), Some(0.77));

    // 2) Display width & ANSI truncation
    assert_eq!(display_width("中文🚀abc"), 9);
    let colored = "\x1b[32m中文🚀abc\x1b[0m";
    assert_eq!(ansi_display_width(colored), 9);
    let truncated = truncate_ansi_to_display_width(colored, 6, "…");
    assert!(ansi_display_width(&truncated) <= 6);
    assert_eq!(strip_ansi(&truncated), "中文…");
    assert!(truncated.ends_with("\x1b[0m"));

    // 3) Pure render_statusline call without filesystem/env
    let snapshot = PulseSnapshot {
        has_conversation_identity: true,
        model: Some(ModelIdentity {
            id: "claude-opus-4-6-thinking".to_string(),
            display_name: "Claude Opus 4.6 (Thinking)".to_string(),
            effort: None,
        }),
        host: HostIdentity {
            plan_tier: Some("Ultra".to_string()),
            cycle_mode: Some("plan".to_string()),
            ..Default::default()
        },
        workspace: WorkspaceInfo {
            current_dir: "pure-repo".to_string(),
            git_branch: Some("main".to_string()),
        },
        cost: CostSnapshot {
            native_total_usd: Some(0.88),
            ..Default::default()
        },
        ..Default::default()
    };
    let cfg = PulseConfig {
        display: DisplayConfig {
            show_model: Some(true),
            show_git_branch: Some(true),
            show_cost: Some(true),
            show_context_monitor: Some(false),
            show_model_cache: Some(false),
            ..DisplayConfig::builtin_defaults()
        },
        language: LanguageSetting::En,
        ..Default::default()
    };

    let rendered = render_statusline(
        &snapshot,
        &cfg,
        &TerminalCaps {
            unicode: true,
            columns: Some(120),
        },
        LanguageSetting::En,
    );
    let plain = strip_ansi(&rendered);
    assert_eq!(
        plain,
        "pure-repo │ ⎇ main │ Opus 4.6(Th) │ Ultra │ plan │ Cost $0.880"
    );
}
