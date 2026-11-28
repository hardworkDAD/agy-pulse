#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use tempfile::TempDir;

use agy_pulse::config::{
    DisplayConfig, LanguageSetting, PulseConfig, QuotaStyle, ThresholdsConfig,
};
use agy_pulse::domain::{
    CriticalWindow, HostIdentity, ProviderQuota, ProviderQuotaSummary, PulseSnapshot, QuotaWindow,
    QuotaWindowKind, format_quota_percent, format_reset_countdown, format_reset_duration,
    has_valid_provider_quota, is_quota_window_expired, parse_iso8601_to_epoch_ms,
    pick_critical_window,
};
use agy_pulse::ingest::HostReader;
use agy_pulse::platform::TerminalCaps;
use agy_pulse::render::{ansi_display_width, render_statusline, strip_ansi};

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

    fn run_stdin(
        &self,
        bin: &PathBuf,
        stdin_text: &str,
        extra_env: &[(&str, &str)],
    ) -> (i32, String, String) {
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

fn epoch_secs_to_iso8601_utc(epoch_secs: i64) -> String {
    let days = epoch_secs.div_euclid(86_400);
    let secs_of_day = epoch_secs.rem_euclid(86_400);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;

    // Howard Hinnant civil_from_days
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    format!("{year:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}Z")
}

#[test]
fn pure_domain_quota_math_parsing_and_critical_window_selection() {
    // 1) ISO-8601 UTC & offset parsing
    // 2026-09-04T23:36:33Z == 1_788_564_993_000 ms
    assert_eq!(
        parse_iso8601_to_epoch_ms("2026-09-04T23:36:33Z"),
        Some(1_788_564_993_000)
    );
    assert_eq!(
        parse_iso8601_to_epoch_ms("2026-09-05T07:36:33.250+08:00"),
        Some(1_788_564_993_250)
    );
    assert_eq!(parse_iso8601_to_epoch_ms("not-a-date"), None);
    assert_eq!(parse_iso8601_to_epoch_ms("2026-13-01T00:00:00Z"), None);

    // 2) Fraction -> integer percent (fractional usage never rounds up to 100%)
    assert_eq!(format_quota_percent(1.0), 100);
    assert_eq!(format_quota_percent(0.9953883), 99);
    assert_eq!(format_quota_percent(0.9439), 94);
    assert_eq!(format_quota_percent(0.08), 8);
    assert_eq!(format_quota_percent(0.0), 0);
    assert_eq!(format_quota_percent(-0.2), 0);
    assert_eq!(format_quota_percent(1.5), 100);

    // 3) Reset duration formatting
    assert_eq!(format_reset_duration(0), "now");
    assert_eq!(format_reset_duration(22 * 60), "22m");
    assert_eq!(format_reset_duration(4 * 3600 + 59 * 60), "4h59m");
    assert_eq!(format_reset_duration(12 * 3600), "12h");
    assert_eq!(format_reset_duration(4 * 86_400 + 3 * 3600), "4d3h");
    assert_eq!(format_reset_duration(10 * 86_400), "10d");

    let now_ms = 1_788_500_000_000_i64;
    assert_eq!(
        format_reset_countdown(Some(now_ms + 22 * 60 * 1000), now_ms),
        Some("~22m".to_string())
    );
    assert_eq!(format_reset_countdown(Some(now_ms), now_ms), None);
    assert_eq!(format_reset_countdown(Some(now_ms - 1000), now_ms), None);

    // 4) Critical window & expiration
    let expired_win = QuotaWindow {
        remaining_fraction: 0.05,
        reset_time_ms: Some(now_ms - 1),
    };
    assert!(is_quota_window_expired(&expired_win, now_ms));

    let valid_weekly = QuotaWindow {
        remaining_fraction: 0.62,
        reset_time_ms: Some(now_ms + 5 * 86_400 * 1000),
    };
    assert!(!is_quota_window_expired(&valid_weekly, now_ms));

    let pq_expired_five_hour = ProviderQuota {
        five_hour: Some(expired_win),
        weekly: Some(valid_weekly.clone()),
    };
    assert_eq!(
        pick_critical_window(&pq_expired_five_hour, now_ms),
        Some(CriticalWindow {
            kind: QuotaWindowKind::Weekly,
            window: valid_weekly.clone(),
        })
    );

    // Tie breaks in favor of 5h
    let pq_tie = ProviderQuota {
        five_hour: Some(QuotaWindow {
            remaining_fraction: 0.5,
            reset_time_ms: Some(now_ms + 3600 * 1000),
        }),
        weekly: Some(QuotaWindow {
            remaining_fraction: 0.5,
            reset_time_ms: Some(now_ms + 5 * 86_400 * 1000),
        }),
    };
    assert_eq!(
        pick_critical_window(&pq_tie, now_ms).map(|c| c.kind),
        Some(QuotaWindowKind::FiveHour)
    );

    let all_expired_summary = ProviderQuotaSummary {
        google: ProviderQuota {
            five_hour: Some(QuotaWindow {
                remaining_fraction: 0.2,
                reset_time_ms: Some(now_ms),
            }),
            weekly: None,
        },
        ..Default::default()
    };
    assert!(!has_valid_provider_quota(&all_expired_summary, now_ms));
}

#[test]
fn pure_render_quota_table_compact_none_colors_and_ascii_fallback() {
    let now_ms = 1_788_500_000_000_i64;
    let snapshot = PulseSnapshot {
        observed_at_ms: now_ms,
        host: HostIdentity {
            plan_tier: Some("Pro".to_string()),
            ..Default::default()
        },
        quota: Some(ProviderQuotaSummary {
            google: ProviderQuota {
                five_hour: Some(QuotaWindow {
                    remaining_fraction: 0.87,
                    reset_time_ms: Some(now_ms + 22 * 60 * 1000),
                }),
                weekly: Some(QuotaWindow {
                    remaining_fraction: 0.25, // <= 30% -> warning (yellow)
                    reset_time_ms: Some(now_ms + (4 * 86_400 + 3 * 3600) * 1000),
                }),
            },
            claude: ProviderQuota {
                five_hour: Some(QuotaWindow {
                    remaining_fraction: 0.08, // <= 10% -> critical (red)
                    reset_time_ms: Some(now_ms + (4 * 3600 + 59 * 60) * 1000),
                }),
                weekly: Some(QuotaWindow {
                    remaining_fraction: 1.0,
                    reset_time_ms: Some(now_ms + (6 * 86_400 + 23 * 3600) * 1000),
                }),
            },
            argon: ProviderQuota {
                five_hour: None,
                weekly: Some(QuotaWindow {
                    remaining_fraction: 0.65,
                    reset_time_ms: Some(now_ms + 5 * 86_400 * 1000),
                }),
            },
            extras: Vec::new(),
        }),
        ..Default::default()
    };

    let base_cfg = PulseConfig {
        display: DisplayConfig {
            show_current_dir: Some(false),
            show_context_monitor: Some(false),
            show_model_cache: Some(false),
            quota_style: QuotaStyle::Table,
            ..DisplayConfig::builtin_defaults()
        },
        thresholds: ThresholdsConfig {
            warning: 0.7,
            critical: 0.9,
        },
        language: LanguageSetting::En,
        ..Default::default()
    };

    // 1) Unicode Table Mode
    let table_out = render_statusline(
        &snapshot,
        &base_cfg,
        &TerminalCaps {
            unicode: true,
            columns: Some(120),
        },
        LanguageSetting::En,
    );
    let table_plain = strip_ansi(&table_out);
    let lines: Vec<&str> = table_plain.lines().collect();
    assert_eq!(
        lines.len(),
        5,
        "expected line1 + divider + 3 rows:\n{table_plain}"
    );
    assert_eq!(lines[1], format!("  {}", "─".repeat(67)));
    assert_eq!(
        lines[2],
        "  Google 5h   [█████░]  87% ~22m   │ Claude 5h   [░░░░░░]   8% ~4h59m"
    );
    assert_eq!(
        lines[3],
        "  Google week [██░░░░]  25% ~4d3h  │ Claude week [██████] 100% ~6d23h"
    );
    assert_eq!(lines[4], "  Argon week  [████░░]  65% ~5d0h ");
    // Verify warning (yellow \x1b[33m) and critical (red \x1b[31m) ANSI colors applied
    assert!(table_out.contains("\x1b[33m[██░░░░]\x1b[0m \x1b[33m 25%\x1b[0m"));
    assert!(table_out.contains("\x1b[31m[░░░░░░]\x1b[0m \x1b[31m  8%\x1b[0m"));

    // 2) ASCII Table Mode
    let ascii_out = render_statusline(
        &snapshot,
        &base_cfg,
        &TerminalCaps {
            unicode: false,
            columns: Some(120),
        },
        LanguageSetting::En,
    );
    let ascii_plain = strip_ansi(&ascii_out);
    assert!(ascii_plain.contains(&format!("  {}", "-".repeat(67))));
    assert!(
        ascii_plain.contains("Google 5h   [#####-]  87% ~22m   | Claude 5h   [------]   8% ~4h59m")
    );
    assert!(!ascii_plain.contains('│'));
    assert!(!ascii_plain.contains('█'));
    assert!(!ascii_plain.contains('─'));

    // 3) Compact Mode
    let mut compact_cfg = base_cfg.clone();
    compact_cfg.display.quota_style = QuotaStyle::Compact;
    let compact_out = render_statusline(
        &snapshot,
        &compact_cfg,
        &TerminalCaps {
            unicode: true,
            columns: Some(120),
        },
        LanguageSetting::En,
    );
    let compact_plain = strip_ansi(&compact_out);
    assert_eq!(compact_plain, "Pro\nGoogle: 25% │ Claude: 8% │ Argon: 65%");
    assert!(compact_out.contains("\x1b[33m25%\x1b[0m"));
    assert!(compact_out.contains("\x1b[31m8%\x1b[0m"));
    assert!(compact_out.contains("\x1b[32m65%\x1b[0m"));

    // 4) None (Hidden) Mode
    let mut none_cfg = base_cfg.clone();
    none_cfg.display.quota_style = QuotaStyle::None;
    let none_out = render_statusline(
        &snapshot,
        &none_cfg,
        &TerminalCaps {
            unicode: true,
            columns: Some(120),
        },
        LanguageSetting::En,
    );
    assert_eq!(strip_ansi(&none_out), "Pro");

    // 5) Narrow terminal clipping (< 69 cols)
    let narrow_out = render_statusline(
        &snapshot,
        &base_cfg,
        &TerminalCaps {
            unicode: true,
            columns: Some(50),
        },
        LanguageSetting::En,
    );
    for line in narrow_out.lines() {
        assert!(
            ansi_display_width(line) <= 50,
            "table line exceeds width 50: {}",
            strip_ansi(line)
        );
    }
}

#[test]
fn e2e_stdin_multi_provider_quota_aliases_and_openai_filtering() {
    let env = IsolatedEnv::new();
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let g5_iso = epoch_secs_to_iso8601_utc(now_secs + 4 * 3600 + 25 * 60 + 20);
    let gw_epoch_secs = now_secs + 5 * 86_400 + 3 * 3600 + 600;
    let c5_epoch_ms = (now_secs + 2 * 3600 + 15 * 60 + 20) * 1000;
    let aw_iso = epoch_secs_to_iso8601_utc(now_secs + 6 * 86_400 + 2 * 3600 + 600);

    let payload = serde_json::json!({
        "conversation_id": "conv-t02-e2e",
        "plan_tier": "Ultra",
        "quota": {
            "gemini-5h": { "remaining_fraction": 0.9495, "reset_time": g5_iso },
            "gemini-weekly": { "remainingFraction": 0.8252, "resetTime": gw_epoch_secs },
            "3p-5h": { "remaining_fraction": 0.45, "reset_time": c5_epoch_ms },
            "argon-weekly": { "remaining_fraction": 0.70, "reset_time": aw_iso },
            "openai-5h": { "remaining_fraction": 0.12, "reset_time": g5_iso },
            "codex-weekly": { "remaining_fraction": 0.22, "reset_time": aw_iso },
            "gpt-5h": { "remaining_fraction": 0.33, "reset_time": g5_iso }
        }
    })
    .to_string();

    let (code, stdout, stderr) = env.run_stdin(
        &agy_pulse_bin(),
        &payload,
        &[("COLUMNS", "120"), ("AGY_PULSE_FORCE_UNICODE", "1")],
    );
    assert_eq!(code, 0);
    assert_eq!(stderr, "");

    let plain = strip_ansi(&stdout);
    assert!(
        plain.contains("Google 5h   [██████]  94% ~4h25m"),
        "missing Google 5h:\n{plain}"
    );
    assert!(
        plain.contains("Google week [█████░]  82% ~5d3h"),
        "missing Google week:\n{plain}"
    );
    assert!(
        plain.contains("Claude 5h   [███░░░]  45% ~2h15m"),
        "missing Claude 5h:\n{plain}"
    );
    assert!(
        plain.contains("Claude week [------]   --") || plain.contains("Claude week [░░░░░░]   --"),
        "missing Claude week placeholder:\n{plain}"
    );
    assert!(
        plain.contains("Argon week  [████░░]  70% ~6d2h"),
        "missing Argon week:\n{plain}"
    );
    assert!(
        !plain.to_ascii_lowercase().contains("openai"),
        "openai must be filtered:\n{plain}"
    );
    assert!(
        !plain.to_ascii_lowercase().contains("codex"),
        "codex must be filtered:\n{plain}"
    );
    assert!(
        !plain.to_ascii_lowercase().contains("gpt"),
        "gpt must be filtered:\n{plain}"
    );
}

#[test]
fn e2e_quota_styles_models_fallback_compact_none_and_expired_handling() {
    let env = IsolatedEnv::new();
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let future_5h = epoch_secs_to_iso8601_utc(now_secs + 3 * 3600 + 10 * 60 + 20);
    let future_week = epoch_secs_to_iso8601_utc(now_secs + 5 * 86_400 + 4 * 3600 + 600);
    let past_iso = "2020-01-01T00:00:00Z";

    let payload = serde_json::json!({
        "plan_tier": "Pro",
        "quota": {
            "gemini-5h": { "remaining_fraction": 0.90, "reset_time": past_iso },
            "gemini-weekly": { "remaining_fraction": 0.25, "reset_time": future_week },
            "claude-5h": { "remaining_fraction": 0.08, "reset_time": future_5h },
            "anthropic-weekly": { "remaining_fraction": 0.88, "reset_time": future_week }
        }
    })
    .to_string();

    // 1) Retired v1 "quotaStyle": "models" smoothly falls back to Table mode, and expired gemini-5h becomes `--`
    fs::write(
        env.global_cfg_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "display": { "quotaStyle": "models" }
        })
        .to_string(),
    )
    .unwrap();
    let (code1, out1, err1) = env.run_stdin(
        &agypl_bin(),
        &payload,
        &[("COLUMNS", "120"), ("AGY_PULSE_FORCE_UNICODE", "1")],
    );
    assert_eq!(code1, 0);
    assert_eq!(err1, "");
    let plain1 = strip_ansi(&out1);
    assert!(
        plain1.contains("Google 5h   [░░░░░░]   --"),
        "expired 5h must render --:\n{plain1}"
    );
    assert!(
        plain1.contains("Google week [██░░░░]  25% ~5d4h"),
        "valid weekly must render:\n{plain1}"
    );
    assert!(
        plain1.contains("Claude 5h   [░░░░░░]   8% ~3h10m"),
        "critical 5h must render:\n{plain1}"
    );
    assert!(
        out1.contains("\x1b[33m[██░░░░]\x1b[0m \x1b[33m 25%\x1b[0m"),
        "warning color missing:\n{out1:?}"
    );
    assert!(
        out1.contains("\x1b[31m[░░░░░░]\x1b[0m \x1b[31m  8%\x1b[0m"),
        "critical color missing:\n{out1:?}"
    );

    // 2) Compact mode renders single summary line with critical windows
    fs::write(
        env.workspace_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "display": { "quotaStyle": "compact" }
        })
        .to_string(),
    )
    .unwrap();
    let (code2, out2, _) = env.run_stdin(
        &agy_pulse_bin(),
        &payload,
        &[("COLUMNS", "120"), ("AGY_PULSE_FORCE_UNICODE", "1")],
    );
    assert_eq!(code2, 0);
    let plain2 = strip_ansi(&out2);
    assert!(
        plain2.contains("Google: 25% │ Claude: 8%"),
        "compact summary mismatch:\n{plain2}"
    );
    assert!(
        !plain2.contains("───"),
        "compact mode must not render divider:\n{plain2}"
    );

    // 3) None mode hides quota output completely
    fs::write(
        env.workspace_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "display": { "quotaStyle": "none" }
        })
        .to_string(),
    )
    .unwrap();
    let (code3, out3, _) = env.run_stdin(&agy_pulse_bin(), &payload, &[("COLUMNS", "120")]);
    assert_eq!(code3, 0);
    let plain3 = strip_ansi(&out3);
    assert!(
        !plain3.contains("Google"),
        "none mode must hide Google:\n{plain3}"
    );
    assert!(
        !plain3.contains("Claude"),
        "none mode must hide Claude:\n{plain3}"
    );

    // 4) All-expired quota produces no quota panel even in table mode
    fs::write(
        env.workspace_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "display": { "quotaStyle": "table" }
        })
        .to_string(),
    )
    .unwrap();
    let all_expired_payload = serde_json::json!({
        "plan_tier": "Pro",
        "quota": {
            "gemini-5h": { "remaining_fraction": 0.5, "reset_time": past_iso },
            "claude-weekly": { "remaining_fraction": 0.4, "reset_time": past_iso }
        }
    })
    .to_string();
    let (code4, out4, _) = env.run_stdin(&agy_pulse_bin(), &all_expired_payload, &[]);
    assert_eq!(code4, 0);
    let plain4 = strip_ansi(&out4);
    assert!(
        !plain4.contains("Google 5h"),
        "all-expired quota must not render table:\n{plain4}"
    );
}

#[test]
fn e2e_bilingual_zh_en_and_ascii_degradation_with_quota() {
    let env = IsolatedEnv::new();
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let future_5h = epoch_secs_to_iso8601_utc(now_secs + 3600 + 45 * 60 + 20);

    fs::write(
        env.global_cfg_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "display": {
                "showContextMonitor": true,
                "quotaStyle": "table"
            }
        })
        .to_string(),
    )
    .unwrap();

    let payload = serde_json::json!({
        "plan_tier": "Pro",
        "cost": { "total_cost_usd": 1.50 },
        "context_window": { "context_window_size": 200000, "used_percentage": 25.0, "total_input_tokens": 50000 },
        "quota": {
            "gemini-5h": { "remaining_fraction": 0.75, "reset_time": future_5h }
        }
    })
    .to_string();

    // Chinese + forced ASCII
    let (code_zh, out_zh, _) = env.run_stdin(
        &agy_pulse_bin(),
        &payload,
        &[
            ("COLUMNS", "120"),
            ("AGY_PULSE_LANGUAGE", "zh"),
            ("AGY_PULSE_FORCE_ASCII", "1"),
        ],
    );
    assert_eq!(code_zh, 0);
    let plain_zh = strip_ansi(&out_zh);
    assert!(
        plain_zh.contains("费用 $1.50"),
        "zh statusline must localize cost:\n{plain_zh}"
    );
    assert!(
        plain_zh.contains("上下文"),
        "zh statusline must localize context:\n{plain_zh}"
    );
    assert!(
        plain_zh.contains(&format!("  {}", "-".repeat(67))),
        "ASCII divider missing:\n{plain_zh}"
    );
    assert!(
        plain_zh.contains("Google 5h   [#####-]  75% ~1h45m | Claude 5h   [------]   --"),
        "ASCII quota row mismatch:\n{plain_zh}"
    );
    assert!(!plain_zh.contains('│'));
    assert!(!plain_zh.contains('─'));
    assert!(!plain_zh.contains('█'));
    assert!(!plain_zh.contains('░'));

    // English + Unicode
    let (code_en, out_en, _) = env.run_stdin(
        &agy_pulse_bin(),
        &payload,
        &[
            ("COLUMNS", "120"),
            ("AGY_PULSE_LANGUAGE", "en"),
            ("AGY_PULSE_FORCE_UNICODE", "1"),
        ],
    );
    assert_eq!(code_en, 0);
    let plain_en = strip_ansi(&out_en);
    assert!(
        plain_en.contains("Cost $1.50"),
        "en statusline must render Cost:\n{plain_en}"
    );
    assert!(
        plain_en.contains("Ctx"),
        "en statusline must render Ctx:\n{plain_en}"
    );
    assert!(
        plain_en.contains("Google 5h   [█████░]  75% ~1h45m │ Claude 5h   [░░░░░░]   --"),
        "Unicode quota row mismatch:\n{plain_en}"
    );
}

#[test]
fn e2e_unstarted_epoch_zero_reset_time_not_expired_and_rendered() {
    let env = IsolatedEnv::new();
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let future_5h = epoch_secs_to_iso8601_utc(now_secs + 4 * 3600 + 25 * 60 + 20);
    let future_week = epoch_secs_to_iso8601_utc(now_secs + 5 * 86_400 + 3 * 3600 + 600);

    // 1) Realistic agy 1.5.18 payload: 3p-5h has remaining_fraction=1, reset_time=1970-01-01T00:00:00Z, reset_in_seconds=0
    let payload_mixed = serde_json::json!({
        "plan_tier": "Google AI Ultra",
        "quota": {
            "gemini-5h": { "remaining_fraction": 0.96, "reset_time": future_5h, "reset_in_seconds": 15920 },
            "gemini-weekly": { "remaining_fraction": 0.88, "reset_time": future_week, "reset_in_seconds": 443400 },
            "3p-5h": { "remaining_fraction": 1, "reset_time": "1970-01-01T00:00:00Z", "reset_in_seconds": 0 },
            "3p-weekly": { "remaining_fraction": 0.92, "reset_time": future_week, "reset_in_seconds": 443400 }
        }
    })
    .to_string();

    let (code1, out1, err1) = env.run_stdin(
        &agy_pulse_bin(),
        &payload_mixed,
        &[("COLUMNS", "120"), ("AGY_PULSE_FORCE_UNICODE", "1")],
    );
    assert_eq!(code1, 0);
    assert_eq!(err1, "");
    let plain1 = strip_ansi(&out1);
    assert!(
        plain1.contains("Claude 5h   [██████] 100%"),
        "3p-5h with 1970 epoch-zero reset_time must not be treated as expired:\n{plain1}"
    );
    assert!(
        !plain1.contains("Claude 5h   [░░░░░░]   --"),
        "3p-5h must not render -- placeholder:\n{plain1}"
    );

    // 2) All 4 windows at 1970-01-01T00:00:00Z + remaining_fraction=1 must still render the full quota table
    let payload_all_unstarted = serde_json::json!({
        "plan_tier": "Google AI Ultra",
        "quota": {
            "gemini-5h": { "remaining_fraction": 1, "reset_time": "1970-01-01T00:00:00Z", "reset_in_seconds": 0 },
            "gemini-weekly": { "remaining_fraction": 1, "reset_time": "1970-01-01T00:00:00Z", "reset_in_seconds": 0 },
            "3p-5h": { "remaining_fraction": 1, "reset_time": "1970-01-01T00:00:00Z", "reset_in_seconds": 0 },
            "3p-weekly": { "remaining_fraction": 1, "reset_time": "1970-01-01T00:00:00Z", "reset_in_seconds": 0 }
        }
    })
    .to_string();

    let (code2, out2, err2) = env.run_stdin(
        &agy_pulse_bin(),
        &payload_all_unstarted,
        &[("COLUMNS", "120"), ("AGY_PULSE_FORCE_UNICODE", "1")],
    );
    assert_eq!(code2, 0);
    assert_eq!(err2, "");
    let plain2 = strip_ansi(&out2);
    assert!(
        plain2.contains("Google 5h   [██████] 100%"),
        "missing Google 5h:\n{plain2}"
    );
    assert!(
        plain2.contains("Claude 5h   [██████] 100%"),
        "missing Claude 5h:\n{plain2}"
    );
    assert!(
        plain2.contains("Google week [██████] 100%"),
        "missing Google week:\n{plain2}"
    );
    assert!(
        plain2.contains("Claude week [██████] 100%"),
        "missing Claude week:\n{plain2}"
    );
    assert!(
        !plain2.contains("--"),
        "unstarted windows must not render --:\n{plain2}"
    );

    // 3) Zero-date 0001-01-01T00:00:00Z (negative epoch ms) is also treated as unstarted countdown
    let payload_zero_date = serde_json::json!({
        "plan_tier": "Pro",
        "quota": {
            "3p-5h": { "remaining_fraction": 1, "reset_time": "0001-01-01T00:00:00Z", "reset_in_seconds": 0 }
        }
    })
    .to_string();
    let (snap, _) = HostReader::parse_stdin_fallback(&payload_zero_date, &env.workspace_dir);
    let win = snap
        .quota
        .as_ref()
        .and_then(|q| q.claude.five_hour.as_ref())
        .expect("3p-5h window must be parsed");
    assert_eq!(win.reset_time_ms, None);
    assert!(!is_quota_window_expired(win, snap.observed_at_ms));

    let (code3, out3, err3) = env.run_stdin(
        &agy_pulse_bin(),
        &payload_zero_date,
        &[("COLUMNS", "120"), ("AGY_PULSE_FORCE_UNICODE", "1")],
    );
    assert_eq!(code3, 0);
    assert_eq!(err3, "");
    let plain3 = strip_ansi(&out3);
    assert!(
        plain3.contains("Claude 5h   [██████] 100%"),
        "0001-01-01T00:00:00Z must render 100% without --:\n{plain3}"
    );
}

#[test]
fn e2e_reset_in_seconds_fallback_and_disabled_window_handling() {
    let env = IsolatedEnv::new();

    let payload = serde_json::json!({
        "plan_tier": "Google AI Ultra",
        "quota": {
            "gemini-5h": { "remaining_fraction": 0.90, "reset_in_seconds": 3600 },
            "gemini-weekly": { "remaining_fraction": 0.80, "reset_time": "1970-01-01T00:00:00Z", "reset_in_seconds": 3600 },
            "3p-5h": { "remaining_fraction": 0.50, "reset_in_seconds": 3600, "disabled": true },
            "3p-weekly": { "remaining_fraction": 0.75, "reset_in_seconds": 7200 }
        }
    })
    .to_string();

    // 1) Unit-level parse verification via HostReader::parse_stdin_fallback
    let (snap, _) = HostReader::parse_stdin_fallback(&payload, &env.workspace_dir);
    let q = snap.quota.as_ref().expect("quota summary expected");
    assert_eq!(
        q.google.five_hour.as_ref().and_then(|w| w.reset_time_ms),
        Some(snap.observed_at_ms + 3_600_000)
    );
    assert_eq!(
        q.google.weekly.as_ref().and_then(|w| w.reset_time_ms),
        Some(snap.observed_at_ms + 3_600_000)
    );
    assert_eq!(q.claude.five_hour, None, "disabled window must be ignored");
    assert_eq!(
        q.claude.weekly.as_ref().and_then(|w| w.reset_time_ms),
        Some(snap.observed_at_ms + 7_200_000)
    );

    // 2) E2E render verification
    let (code, out, err) = env.run_stdin(
        &agy_pulse_bin(),
        &payload,
        &[("COLUMNS", "120"), ("AGY_PULSE_FORCE_UNICODE", "1")],
    );
    assert_eq!(code, 0);
    assert_eq!(err, "");
    let plain = strip_ansi(&out);
    assert!(
        plain.contains("Google 5h   [█████░]  90% ~1h0m"),
        "reset_in_seconds fallback without reset_time failed:\n{plain}"
    );
    assert!(
        plain.contains("Google week [█████░]  80% ~1h0m"),
        "reset_in_seconds fallback with 1970 reset_time failed:\n{plain}"
    );
    assert!(
        plain.contains("Claude 5h   [░░░░░░]   --"),
        "disabled 3p-5h must be omitted (rendered as -- placeholder):\n{plain}"
    );
    assert!(
        plain.contains("Claude week [█████░]  75% ~2h0m"),
        "active 3p-weekly must render:\n{plain}"
    );

    // 3) All disabled windows -> not counted as valid quota, table hidden
    let all_disabled_payload = serde_json::json!({
        "plan_tier": "Pro",
        "quota": {
            "gemini-5h": { "remaining_fraction": 0.9, "reset_in_seconds": 3600, "disabled": true },
            "3p-5h": { "remaining_fraction": 1.0, "reset_time": "1970-01-01T00:00:00Z", "disabled": true }
        }
    })
    .to_string();
    let (snap_disabled, _) =
        HostReader::parse_stdin_fallback(&all_disabled_payload, &env.workspace_dir);
    assert!(snap_disabled.quota.is_none());

    let (code_dis, out_dis, err_dis) = env.run_stdin(
        &agy_pulse_bin(),
        &all_disabled_payload,
        &[("COLUMNS", "120"), ("AGY_PULSE_FORCE_UNICODE", "1")],
    );
    assert_eq!(code_dis, 0);
    assert_eq!(err_dis, "");
    let plain_dis = strip_ansi(&out_dis);
    assert!(
        !plain_dis.contains("Google 5h"),
        "all disabled must hide quota table:\n{plain_dis}"
    );
    assert!(
        !plain_dis.contains("Claude 5h"),
        "all disabled must hide quota table:\n{plain_dis}"
    );
}

#[test]
fn e2e_stdin_email_and_username_priority_rendering() {
    let env = IsolatedEnv::new();

    // 1) Only email present in stdin
    let payload_email_only = serde_json::json!({
        "conversation_id": "conv-email-only",
        "email": "alice@example.com",
        "plan_tier": "Google AI Ultra"
    })
    .to_string();

    let (snap1, _) = HostReader::parse_stdin_fallback(&payload_email_only, &env.workspace_dir);
    assert_eq!(snap1.host.email.as_deref(), Some("alice@example.com"));
    assert_eq!(snap1.host.username.as_deref(), Some("alice@example.com"));

    // Default config keeps showUsername=false
    let (code_def, out_def, err_def) = env.run_stdin(
        &agy_pulse_bin(),
        &payload_email_only,
        &[("COLUMNS", "120"), ("AGY_PULSE_FORCE_UNICODE", "1")],
    );
    assert_eq!(code_def, 0);
    assert_eq!(err_def, "");
    let plain_def = strip_ansi(&out_def);
    assert!(
        !plain_def.contains("alice@example.com"),
        "default config must keep showUsername=false:\n{plain_def}"
    );

    // Enabling showUsername renders email on Line 1
    fs::write(
        env.global_cfg_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "display": { "showUsername": true }
        })
        .to_string(),
    )
    .unwrap();
    let (code_em, out_em, err_em) = env.run_stdin(
        &agy_pulse_bin(),
        &payload_email_only,
        &[("COLUMNS", "120"), ("AGY_PULSE_FORCE_UNICODE", "1")],
    );
    assert_eq!(code_em, 0);
    assert_eq!(err_em, "");
    let plain_em = strip_ansi(&out_em);
    let first_line_em = plain_em.lines().next().unwrap_or("");
    assert!(
        first_line_em.contains("alice@example.com"),
        "Line 1 must render email when showUsername=true:\n{plain_em}"
    );

    // 2) Both username and email present -> host.username prefers username, host.email keeps email
    let payload_both = serde_json::json!({
        "conversation_id": "conv-both",
        "username": "bob",
        "email": "alice@example.com",
        "plan_tier": "Pro"
    })
    .to_string();
    let (snap2, _) = HostReader::parse_stdin_fallback(&payload_both, &env.workspace_dir);
    assert_eq!(snap2.host.username.as_deref(), Some("bob"));
    assert_eq!(snap2.host.email.as_deref(), Some("alice@example.com"));

    let (code_both, out_both, _) = env.run_stdin(
        &agy_pulse_bin(),
        &payload_both,
        &[("COLUMNS", "120"), ("AGY_PULSE_FORCE_UNICODE", "1")],
    );
    assert_eq!(code_both, 0);
    let plain_both = strip_ansi(&out_both);
    let first_line_both = plain_both.lines().next().unwrap_or("");
    assert!(
        first_line_both.contains("bob"),
        "Line 1 must prefer stdin.username over stdin.email:\n{plain_both}"
    );
    assert!(
        !first_line_both.contains("alice@example.com"),
        "Line 1 must not show email when stdin.username is set:\n{plain_both}"
    );

    // 3) display.username overrides stdin.username and stdin.email
    fs::write(
        env.workspace_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "display": {
                "showUsername": true,
                "username": "override-name"
            }
        })
        .to_string(),
    )
    .unwrap();
    let (code_ovr, out_ovr, _) = env.run_stdin(
        &agy_pulse_bin(),
        &payload_both,
        &[("COLUMNS", "120"), ("AGY_PULSE_FORCE_UNICODE", "1")],
    );
    assert_eq!(code_ovr, 0);
    let plain_ovr = strip_ansi(&out_ovr);
    let first_line_ovr = plain_ovr.lines().next().unwrap_or("");
    assert!(
        first_line_ovr.contains("override-name"),
        "Line 1 must prefer display.username override:\n{plain_ovr}"
    );
    assert!(
        !first_line_ovr.contains("bob") && !first_line_ovr.contains("alice@example.com"),
        "Line 1 must not show stdin username/email when display.username is configured:\n{plain_ovr}"
    );
}

#[test]
fn quota_table_divider_never_exceeds_ultra_narrow_terminal_columns() {
    let now_ms = 1_760_000_000_000_i64;
    let snapshot = PulseSnapshot {
        has_conversation_identity: true,
        observed_at_ms: now_ms,
        host: HostIdentity {
            plan_tier: Some("Pro".to_string()),
            ..Default::default()
        },
        quota: Some(ProviderQuotaSummary {
            google: ProviderQuota {
                five_hour: Some(QuotaWindow {
                    remaining_fraction: 0.85,
                    reset_time_ms: Some(now_ms + 3_600_000),
                }),
                weekly: None,
            },
            claude: ProviderQuota {
                five_hour: Some(QuotaWindow {
                    remaining_fraction: 0.50,
                    reset_time_ms: Some(now_ms + 3_600_000),
                }),
                weekly: None,
            },
            argon: ProviderQuota::default(),
            extras: Vec::new(),
        }),
        ..Default::default()
    };
    let cfg = PulseConfig {
        display: DisplayConfig {
            show_current_dir: Some(false),
            show_context_monitor: Some(false),
            show_model_cache: Some(false),
            quota_style: QuotaStyle::Table,
            ..DisplayConfig::builtin_defaults()
        },
        language: LanguageSetting::En,
        ..Default::default()
    };

    for unicode in [true, false] {
        for cols in [1u16, 2, 3, 5, 8, 11, 12, 40, 68, 69] {
            let rendered = render_statusline(
                &snapshot,
                &cfg,
                &TerminalCaps {
                    unicode,
                    columns: Some(cols),
                },
                LanguageSetting::En,
            );
            for (idx, line) in rendered.lines().enumerate() {
                let width = ansi_display_width(line);
                assert!(
                    width <= cols as usize,
                    "unicode={unicode} cols={cols} line[{idx}] exceeded columns: width={width}, plain={:?}",
                    strip_ansi(line)
                );
            }
            let plain_lines: Vec<&str> = rendered.lines().collect();
            let divider_line = plain_lines[1];
            let expected_char = if unicode { '─' } else { '-' };
            assert!(
                strip_ansi(divider_line).contains(expected_char),
                "unicode={unicode} cols={cols} divider must still render horizontal rule: {:?}",
                strip_ansi(divider_line)
            );
        }
    }
}
