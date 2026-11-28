#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

use agy_pulse::domain::{
    ContextWindowSnapshot, CostSnapshot, HostIdentity, ModelIdentity, ModelTokenTotals,
    PulseSnapshot, TokenUsage, WorkspaceInfo, WorkspaceMetadata, is_usage_cache_inclusive,
    model_includes_cache_in_input, resolve_cascade_cost_usd, resolve_native_cost_usd,
};
use agy_pulse::render::{
    ansi_display_width, display_width, render_statusline, strip_ansi,
    truncate_ansi_to_display_width,
};
use agy_pulse::config::{
    CacheHitThresholdsConfig, DisplayConfig, LanguageSetting, PulseConfig, ThemeConfig,
};
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
            .env("AGY_PULSE_DISABLE_UPDATE_CHECK", "1")
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
        let expected_ver = env!("CARGO_PKG_VERSION");
        assert!(
            ver_text.contains(expected_ver),
            "version output must contain {expected_ver}, got: {ver_text}"
        );

        let help_out = env.cmd(&bin).arg("--help").output().unwrap();
        assert_eq!(help_out.status.code(), Some(0));
        let help_text = String::from_utf8_lossy(&help_out.stdout);
        assert!(help_text.contains("agypl"));
        assert!(help_text.contains("agy-pulse"));
        assert!(help_text.contains("account assign"));
        assert!(help_text.contains("sync [init|status|prune-device]"));
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
                "showGitBranch": true,
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
    assert!(lines.len() >= 2, "expected at least 2 lines, got:\n{plain}");

    let line1 = lines[0];
    assert!(line1.contains("my-project"), "line1 missing dir: {line1}");
    assert!(line1.contains("feat/tracer-bullet"), "line1 missing branch: {line1}");
    assert!(line1.contains("Pro"), "line1 missing plan tier: {line1}");
    assert!(line1.contains("auto"), "line1 missing cycle mode: {line1}");
    assert!(!line1.contains("Opus"), "line1 must not duplicate model name: {line1}");
    assert!(!line1.contains("Cost"), "line1 must not duplicate conversation cost: {line1}");

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

    // Global config with retired v1 & retired display fields + Ocean Blue preset + showContextMonitor=true
    fs::write(
        env.global_cfg_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "enabled": true,
            "theme": "Ocean Blue",
            "language": "en",
            "display": {
                "showModel": true,
                "showTokenBar": true,
                "showContextBar": true,
                "showTurnCacheHitRate": true,
                "showCost": true,
                "showContextMonitor": true,
                "showCurrentDir": false,
                "quotaStyle": "models"
            },
            "icons": {
                "token": "T",
                "ctx": "C"
            },
            "cloudQuota": { "enabled": true, "endpoints": ["https://legacy.example"] },
            "retiredUnknownField": "keep-ignored"
        })
        .to_string(),
    )
    .unwrap();

    // Workspace config overrides language to zh and enables currentDir, while inheriting showContextMonitor=true
    fs::write(
        env.workspace_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "language": "zh",
            "display": {
                "showCurrentDir": true
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
    assert!(plain.contains("Gemini 3.1 Pro (High)"), "global showContextMonitor=true should render model on monitor: {plain}");
    assert!(plain.contains("上下文"), "workspace language=zh should override global en: {plain}");
    let first_line = plain.lines().next().unwrap_or("");
    assert!(
        !first_line.contains("Gemini") && !first_line.contains("费用") && !first_line.contains("$1.25"),
        "retired showModel/showCost must stay silently ignored on line 1: {plain}"
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
        &[("COLUMNS", "120"), ("LC_ALL", "zh_CN.UTF-8"), ("AGY_PULSE_FORCE_ASCII", "1")],
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
                "showGitBranch": true,
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
fn post_stdin_hard_cap_triggers_in_memory_fallback_and_exits_zero() {
    let env = IsolatedEnv::new();
    let proj_dir = env.workspace_dir.join("fallback-proj");
    fs::create_dir_all(&proj_dir).unwrap();

    // Seed several dummy conversation DBs so enriched ingest takes >1ms.
    let conv_dir = env.data_dir.join("conversations");
    fs::create_dir_all(&conv_dir).unwrap();
    for i in 0..8 {
        fs::write(conv_dir.join(format!("dummy-{i:02}.db")), b"not-a-db").unwrap();
    }

    let payload = serde_json::json!({
        "conversation_id": "conv-hard-cap",
        "model": {
            "id": "gemini-4-hydrogen-high",
            "display_name": "Gemini 4 Hydrogen (High)"
        },
        "plan_tier": "Pro",
        "cycle_mode": "auto",
        "workspace": {
            "current_dir": proj_dir.to_string_lossy()
        },
        "context_window": {
            "context_window_size": 500000,
            "used_percentage": 25.0,
            "total_input_tokens": 125000
        }
    })
    .to_string();

    let start = Instant::now();
    let (code, stdout, stderr) = env.run_stdin(
        &agy_pulse_bin(),
        &payload,
        &[
            ("COLUMNS", "120"),
            ("AGY_PULSE_POST_STDIN_HARD_CAP_MS", "1"),
        ],
    );
    let elapsed = start.elapsed();

    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stderr, "");
    assert!(
        elapsed < Duration::from_millis(500),
        "expected prompt hard-cap exit (<500ms), took {elapsed:?}"
    );
    let plain = strip_ansi(&stdout);
    assert!(
        plain.contains("fallback-proj") && plain.contains("Pro"),
        "expected valid stdin fallback statusline output, got:\n{plain}"
    );
}

#[cfg(unix)]
#[test]
fn statusline_exits_before_host_timeout_when_stdout_pipe_is_full() {
    use std::io::{ErrorKind, pipe};
    use std::os::fd::AsRawFd;
    use std::thread;

    let env = IsolatedEnv::new();
    let (_reader, mut writer) = pipe().expect("create stdout pipe");
    let fd = writer.as_raw_fd();
    // SAFETY: writer owns an open file descriptor for the duration of these calls.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    assert!(flags >= 0);
    // SAFETY: fd still belongs to writer and both flag values come from fcntl.
    assert_eq!(
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    let buffer = [b'x'; 4096];
    // Fill any space left after a larger write gets EAGAIN.
    for chunk in [buffer.as_slice(), &buffer[..1]] {
        loop {
            match writer.write(chunk) {
                Ok(bytes) => assert!(bytes > 0),
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(err) => panic!("fill stdout pipe: {err}"),
            }
        }
    }
    // SAFETY: Restore the original flags while writer still owns fd.
    assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFL, flags) }, 0);

    let profile_log = env.data_dir.join("statusline-profile.log");
    let host_timeout = Duration::from_secs(5);
    let start = Instant::now();
    let mut child = env
        .cmd(&agy_pulse_bin())
        .env("AGY_PULSE_POST_STDIN_HARD_CAP_MS", "1")
        .env("AGY_PULSE_PROFILE_LOG", &profile_log)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(writer))
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn statusline with a full stdout pipe");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"plan_tier":"Pro"}"#)
        .unwrap();
    // Keep the pipe reader open without draining it, as a stalled host would.
    while start.elapsed() < host_timeout {
        if let Some(status) = child.try_wait().expect("poll statusline") {
            assert!(status.success(), "statusline exit status {status}");
            let log = fs::read_to_string(&profile_log).expect("read statusline profile");
            assert!(
                log.lines()
                    .any(|line| line.contains(" STALL ") && line.ends_with(" stage=stdout")),
                "expected the profiler to identify blocked output: {log}"
            );
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    child.kill().ok();
    child.wait().expect("reap blocked statusline");
    panic!("statusline exceeded the 5-second host timeout with blocked stdout");
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
            show_git_branch: Some(true),
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
    assert_eq!(plain, "pure-repo │ ⎇ main │ Ultra │ plan");
}

#[test]
fn profile_log_creates_nested_parent_dirs_and_refuses_symlink_without_tampering_target() {
    let env = IsolatedEnv::new();
    let nested_log = env
        .data_dir
        .join("profile_logs")
        .join("nested")
        .join("agy-pulse-prof.log");
    assert!(!nested_log.parent().unwrap().exists());

    let payload = serde_json::json!({
        "model": { "display_name": "Claude Opus 4.6 (Thinking)" },
        "plan_tier": "Pro"
    })
    .to_string();

    let (code, _out, err) = env.run_stdin(
        &agy_pulse_bin(),
        &payload,
        &[
            ("AGY_PULSE_PROFILE_LOG", nested_log.to_string_lossy().as_ref()),
            ("AGY_PULSE_PROFILE_MIN_MS", "0"),
        ],
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert!(nested_log.exists(), "profile log should be created with missing nested parents");
    let content = fs::read_to_string(&nested_log).unwrap();
    assert!(
        content.contains("DONE total="),
        "expected DONE profile record, got: {content}"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let parent_mode = fs::metadata(nested_log.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(parent_mode & 0o077, 0, "parent dir must be 0o700, got {parent_mode:o}");
        let file_mode = fs::symlink_metadata(&nested_log)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode & 0o177, 0, "profile log must be 0o600, got {file_mode:o}");

        let sentinel = env.data_dir.join("prof_sentinel.txt");
        fs::write(&sentinel, "untouched").unwrap();
        let symlink_log = env.data_dir.join("prof_symlink.log");
        symlink(&sentinel, &symlink_log).unwrap();

        let (code2, _out2, err2) = env.run_stdin(
            &agy_pulse_bin(),
            &payload,
            &[
                ("AGY_PULSE_PROFILE_LOG", symlink_log.to_string_lossy().as_ref()),
                ("AGY_PULSE_PROFILE_MIN_MS", "0"),
            ],
        );
        assert_eq!(code2, 0, "stderr: {err2}");
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "untouched");
        assert!(
            fs::symlink_metadata(&symlink_log)
                .unwrap()
                .file_type()
                .is_symlink(),
            "profile log symlink must not be deleted or replaced"
        );
    }
}

#[test]
fn bg_ledger_sync_lock_recovers_dead_pid_immediately_and_preserves_live_pid_over_30s() {
    use std::time::{SystemTime, UNIX_EPOCH};

    let env = IsolatedEnv::new();
    fs::write(
        env.global_cfg_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "display": { "showDailyCost": true }
        })
        .to_string(),
    )
    .unwrap();

    let lock_path = env.data_dir.join("agy-pulse-ledger-sync.lock");
    let live_pid = std::process::id();
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;

    // 1. Live worker PID lock older than 30s (60s ago) MUST NOT be stolen or deleted.
    fs::write(
        &lock_path,
        serde_json::json!({
            "pid": live_pid,
            "role": "worker",
            "token": "live-owner-tok",
            "ts": now_ms - 60_000
        })
        .to_string(),
    )
    .unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&lock_path)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(60))
        .unwrap();

    let payload = serde_json::json!({
        "conversation_id": "conv-bg-lock",
        "cost": { "total_cost_usd": 0.5 }
    })
    .to_string();

    let (code1, _out1, err1) = env.run_stdin(&agy_pulse_bin(), &payload, &[]);
    assert_eq!(code1, 0, "stderr: {err1}");
    assert!(lock_path.exists(), "live >30s worker lock must be preserved");
    let raw1 = fs::read_to_string(&lock_path).unwrap();
    assert!(
        raw1.contains("live-owner-tok"),
        "live >30s worker lock must not be overwritten, got: {raw1}"
    );

    // 2. Dead worker PID lock with fresh mtime MUST be reclaimed immediately without waiting 30s.
    let mut dead_child = env
        .cmd(&agy_pulse_bin())
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn short-lived process");
    let dead_pid = dead_child.id();
    let _ = dead_child.wait().expect("wait short-lived process");

    fs::write(
        &lock_path,
        serde_json::json!({
            "pid": dead_pid,
            "role": "worker",
            "token": "dead-owner-tok",
            "ts": now_ms
        })
        .to_string(),
    )
    .unwrap();

    let (code2, _out2, err2) = env.run_stdin(&agy_pulse_bin(), &payload, &[]);
    assert_eq!(code2, 0, "stderr: {err2}");

    let deadline = Instant::now() + Duration::from_secs(2);
    while lock_path.exists() && Instant::now() < deadline {
        if let Ok(raw) = fs::read_to_string(&lock_path) {
            assert!(
                !raw.contains("dead-owner-tok"),
                "dead PID lock must be evicted immediately, got: {raw}"
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if lock_path.exists() {
        let raw2 = fs::read_to_string(&lock_path).unwrap();
        assert!(
            !raw2.contains("dead-owner-tok"),
            "dead PID lock remained present: {raw2}"
        );
    }
}

#[test]
fn bg_ledger_sync_lock_different_token_never_deletes_foreign_lock() {
    use std::time::{SystemTime, UNIX_EPOCH};

    let env = IsolatedEnv::new();
    let lock_path = env.data_dir.join("agy-pulse-ledger-sync.lock");
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;

    fs::write(
        &lock_path,
        serde_json::json!({
            "pid": std::process::id(),
            "role": "worker",
            "token": "foreign-owner-tok",
            "ts": now_ms
        })
        .to_string(),
    )
    .unwrap();

    let out_wrong = env
        .cmd(&agy_pulse_bin())
        .args(["--sync-ledger-bg", "--lock-token", "wrong-tok"])
        .output()
        .unwrap();
    assert_eq!(out_wrong.status.code(), Some(0));
    assert!(lock_path.exists(), "foreign lock must not be deleted by mismatched token");
    let raw = fs::read_to_string(&lock_path).unwrap();
    assert!(
        raw.contains("foreign-owner-tok"),
        "foreign lock contents must remain untouched, got: {raw}"
    );

    // Matching token updates & cleans up its own lock on exit.
    fs::write(
        &lock_path,
        serde_json::json!({
            "pid": std::process::id(),
            "role": "spawning",
            "token": "own-tok-123",
            "ts": now_ms
        })
        .to_string(),
    )
    .unwrap();
    let out_own = env
        .cmd(&agy_pulse_bin())
        .args(["--sync-ledger-bg", "--lock-token=own-tok-123"])
        .output()
        .unwrap();
    assert_eq!(out_own.status.code(), Some(0));
    assert!(
        !lock_path.exists(),
        "worker owning matching lock token must release its lock on exit"
    );
}

#[test]
fn cache_hit_thresholds_drive_monitor_and_model_cache_coloring() {
    let make_snapshot = |cache_read: u64, input_tokens: u64| PulseSnapshot {
        has_conversation_identity: true,
        model: Some(ModelIdentity {
            id: "gemini-4-carbon-high".to_string(),
            display_name: "Gemini 4 Carbon (High)".to_string(),
            effort: None,
        }),
        host: HostIdentity {
            plan_tier: Some("Pro".to_string()),
            ..Default::default()
        },
        workspace: WorkspaceInfo {
            current_dir: "cache-thresh".to_string(),
            git_branch: None,
        },
        context: Some(ContextWindowSnapshot {
            context_window_size: 500_000,
            used_percentage: Some(20.0),
            total_input_tokens: input_tokens,
            total_output_tokens: 1_000,
            total_cache_read_tokens: Some(cache_read),
            total_cache_creation_tokens: Some(0),
            current_usage: Some(TokenUsage {
                input_tokens,
                output_tokens: 500,
                cache_read_input_tokens: cache_read,
                cache_creation_input_tokens: 0,
                cache_exclusive: false,
            }),
            has_total_input: true,
        }),
        model_token_totals: vec![ModelTokenTotals {
            model_key: "Carbon".to_string(),
            input_tokens: input_tokens.saturating_sub(cache_read),
            cache_read_tokens: cache_read,
            cache_write_tokens: 0,
            output_tokens: 1_000,
            steps: 1,
        }],
        ..Default::default()
    };

    // Ocean Blue: primary = \x1b[34m, warning = \x1b[33m; BOLD = \x1b[1m, DIM = \x1b[2m
    let cfg = PulseConfig {
        theme: ThemeConfig::Preset("Ocean Blue".to_string()),
        language: LanguageSetting::En,
        display: DisplayConfig {
            cache_hit_thresholds: Some(CacheHitThresholdsConfig {
                excellent: Some(90.0),
                partial: Some(70.0),
            }),
            show_context_monitor: Some(true),
            show_model_cache: Some(true),
            ..DisplayConfig::builtin_defaults()
        },
        ..Default::default()
    };
    let caps = TerminalCaps {
        unicode: true,
        columns: Some(140),
    };

    // 1) 92% >= excellent (90) -> both lines use primary + BOLD (\x1b[34m\x1b[1m)
    let out_high = render_statusline(&make_snapshot(92_000, 100_000), &cfg, &caps, LanguageSetting::En);
    let high_lines: Vec<&str> = out_high.lines().collect();
    let cache_high = high_lines.iter().find(|l| l.contains("Session Cache")).unwrap();
    let mon_high = high_lines.iter().find(|l| l.contains("Ctx")).unwrap();
    assert!(
        cache_high.contains("Carbon \x1b[34m\x1b[1m92%\x1b[0m"),
        "expected primary+BOLD on model cache >= 90%: {cache_high:?}"
    );
    assert!(
        mon_high.contains("\x1b[34m\x1b[1m92%\x1b[0m"),
        "expected primary+BOLD on context monitor >= 90%: {mon_high:?}"
    );

    // 2) 80% in [70, 90) -> partial tier: warning (\x1b[33m), NOT primary+BOLD (would be excellent under 80/50 defaults)
    let out_mid = render_statusline(&make_snapshot(80_000, 100_000), &cfg, &caps, LanguageSetting::En);
    let mid_lines: Vec<&str> = out_mid.lines().collect();
    let cache_mid = mid_lines.iter().find(|l| l.contains("Session Cache")).unwrap();
    let mon_mid = mid_lines.iter().find(|l| l.contains("Ctx")).unwrap();
    assert!(
        cache_mid.contains("Carbon \x1b[33m80%\x1b[0m"),
        "expected warning color on model cache 80% with partial=70, excellent=90: {cache_mid:?}"
    );
    assert!(
        mon_mid.contains("\x1b[33m80%\x1b[0m"),
        "expected warning color on context monitor 80% with partial=70, excellent=90: {mon_mid:?}"
    );

    // 3) 60% < partial (70) -> low tier: DIM (\x1b[2m), NOT warning (would be warning under 80/50 defaults)
    let out_low = render_statusline(&make_snapshot(60_000, 100_000), &cfg, &caps, LanguageSetting::En);
    let low_lines: Vec<&str> = out_low.lines().collect();
    let cache_low = low_lines.iter().find(|l| l.contains("Session Cache")).unwrap();
    let mon_low = low_lines.iter().find(|l| l.contains("Ctx")).unwrap();
    assert!(
        cache_low.contains("Carbon \x1b[2m60%\x1b[0m"),
        "expected DIM color on model cache 60% with partial=70: {cache_low:?}"
    );
    assert!(
        mon_low.contains("\x1b[2m60%\x1b[0m"),
        "expected DIM color on context monitor 60% with partial=70: {mon_low:?}"
    );
}

#[test]
fn display_config_none_fallbacks_match_builtin_defaults() {
    let snapshot = PulseSnapshot {
        has_conversation_identity: true,
        model: Some(ModelIdentity {
            id: "gemini-4-carbon-high".to_string(),
            display_name: "Gemini 4 Carbon (High)".to_string(),
            effort: None,
        }),
        host: HostIdentity {
            plan_tier: Some("Pro".to_string()),
            cycle_mode: Some("auto".to_string()),
            ..Default::default()
        },
        workspace: WorkspaceInfo {
            current_dir: "repo-none-test".to_string(),
            git_branch: Some("feat/should-stay-hidden".to_string()),
        },
        workspace_meta: WorkspaceMetadata {
            memory_file: None,
            breadcrumbs: vec!["hidden_crumb.rs".to_string()],
            rules_count: 0,
            mcp_count: 0,
            hooks_count: 0,
        },
        context: Some(ContextWindowSnapshot {
            context_window_size: 500_000,
            used_percentage: Some(25.0),
            total_input_tokens: 125_000,
            total_output_tokens: 2_000,
            total_cache_read_tokens: Some(100_000),
            total_cache_creation_tokens: Some(0),
            current_usage: Some(TokenUsage {
                input_tokens: 125_000,
                output_tokens: 1_000,
                cache_read_input_tokens: 100_000,
                cache_creation_input_tokens: 0,
                cache_exclusive: false,
            }),
            has_total_input: true,
        }),
        model_token_totals: vec![ModelTokenTotals {
            model_key: "Carbon".to_string(),
            input_tokens: 25_000,
            cache_read_tokens: 100_000,
            cache_write_tokens: 0,
            output_tokens: 2_000,
            steps: 1,
        }],
        ..Default::default()
    };

    let caps = TerminalCaps {
        unicode: true,
        columns: Some(140),
    };

    let all_none_cfg = PulseConfig {
        display: DisplayConfig::default(),
        language: LanguageSetting::En,
        ..Default::default()
    };
    let builtin_cfg = PulseConfig {
        display: DisplayConfig::builtin_defaults(),
        language: LanguageSetting::En,
        ..Default::default()
    };

    let out_none = render_statusline(&snapshot, &all_none_cfg, &caps, LanguageSetting::En);
    let out_builtin = render_statusline(&snapshot, &builtin_cfg, &caps, LanguageSetting::En);
    assert_eq!(
        out_none, out_builtin,
        "all-None DisplayConfig must render identically to builtin_defaults()"
    );

    let plain = strip_ansi(&out_none);
    assert!(plain.contains("Session Cache"), "showModelCache=None must default to true: {plain}");
    assert!(plain.contains("Ctx"), "showContextMonitor=None must default to true: {plain}");
    assert!(!plain.contains("feat/should-stay-hidden"), "showGitBranch=None must default to false: {plain}");
    assert!(!plain.contains("hidden_crumb.rs"), "showBreadcrumbs=None must default to false: {plain}");

    // showModelCache=None must remain independent even when showContextMonitor=false
    let independent_cfg = PulseConfig {
        display: DisplayConfig {
            show_context_monitor: Some(false),
            show_model_cache: None,
            ..DisplayConfig::default()
        },
        language: LanguageSetting::En,
        ..Default::default()
    };
    let out_independent = strip_ansi(&render_statusline(
        &snapshot,
        &independent_cfg,
        &caps,
        LanguageSetting::En,
    ));
    assert!(
        out_independent.contains("Session Cache") && !out_independent.contains("Ctx"),
        "showModelCache=None must not depend on showContextMonitor: {out_independent}"
    );
}
