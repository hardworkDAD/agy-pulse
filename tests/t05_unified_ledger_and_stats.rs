#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

use chrono::{Local, TimeZone};
use rusqlite::{Connection, params};
use serde_json::Value;
use tempfile::TempDir;

use agy_pulse::domain::{TokenUsage, calculate_token_cost};
use agy_pulse::ledger::{
    CsvExportKind, LedgerQueryFilter, LedgerSource, LedgerSourceFilter, LedgerStore,
    LedgerSyncOptions, LedgerTimeRange, RollingCostInput, SourceRoot,
};
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
    ide_data_dir: PathBuf,
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
        let ide_data_dir = root.join("ide_data");
        let global_cfg_dir = root.join("global_cfg");
        let xdg_cfg_dir = root.join("xdg_cfg");
        let app_data_dir = root.join("app_data");
        let local_app_data_dir = root.join("local_app_data");
        let workspace_dir = root.join("workspace");
        fs::create_dir_all(&home_dir).unwrap();
        fs::create_dir_all(&cli_data_dir).unwrap();
        fs::create_dir_all(&ide_data_dir).unwrap();
        fs::create_dir_all(&global_cfg_dir).unwrap();
        fs::create_dir_all(&xdg_cfg_dir).unwrap();
        fs::create_dir_all(&app_data_dir).unwrap();
        fs::create_dir_all(&local_app_data_dir).unwrap();
        fs::create_dir_all(&workspace_dir).unwrap();
        Self {
            _temp: temp,
            home_dir,
            cli_data_dir,
            ide_data_dir,
            global_cfg_dir,
            xdg_cfg_dir,
            app_data_dir,
            local_app_data_dir,
            workspace_dir,
        }
    }

    fn base_cmd(&self, bin: &Path, include_ide_env: bool) -> Command {
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
            .env_remove("AGY_PULSE_FORCE_ASCII")
            .env_remove("AGY_PULSE_FORCE_UNICODE")
            .env_remove("AGY_PULSE_LANGUAGE")
            .env_remove("COLUMNS")
            .env("LC_ALL", "en_US.UTF-8")
            .env("LANG", "en_US.UTF-8");
        if include_ide_env {
            cmd.env("AGY_PULSE_IDE_DATA_DIR", &self.ide_data_dir);
        } else {
            cmd.env_remove("AGY_PULSE_IDE_DATA_DIR");
        }
        cmd
    }

    fn run_stdin(
        &self,
        bin: &Path,
        stdin_text: &str,
        include_ide_env: bool,
        extra_env: &[(&str, &str)],
    ) -> (i32, String, String) {
        let mut cmd = self.base_cmd(bin, include_ide_env);
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
        let out = child.wait_with_output().expect("wait output");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn run_args(
        &self,
        bin: &Path,
        args: &[&str],
        include_ide_env: bool,
        extra_env: &[(&str, &str)],
    ) -> (i32, String, String) {
        let mut cmd = self.base_cmd(bin, include_ide_env);
        cmd.args(args);
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let out = cmd.stdin(Stdio::null()).output().expect("run binary");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

// --- Minimal Protobuf encoders for test host conversation DBs ---

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

fn pb_duration_sec(field_num: u32, sec: u64, nanos: u64) -> Vec<u8> {
    let mut inner = Vec::new();
    if sec > 0 {
        inner.extend(pb_varint(1, sec));
    }
    if nanos > 0 {
        inner.extend(pb_varint(2, nanos));
    }
    pb_bytes(field_num, &inner)
}

fn build_step_metadata_blob(ts_ms: i64) -> Vec<u8> {
    pb_timestamp_ms(1, ts_ms)
}

fn build_trajectory_blob(
    workspace_uri: &str,
    branch: &str,
    created_at_ms: i64,
    cascade_id: Option<&str>,
) -> Vec<u8> {
    let mut ws = pb_str(1, workspace_uri);
    ws.extend(pb_str(4, branch));
    let mut out = pb_bytes(1, &ws);
    out.extend(pb_timestamp_ms(2, created_at_ms));
    if let Some(cid) = cascade_id {
        out.extend(pb_str(6, cid));
    }
    out
}

struct GenStepSpec<'a> {
    display_name: &'a str,
    input_tokens: u64,
    cache_read_tokens: u64,
    output_tokens: u64,
    stream_sec: Option<(u64, u64)>,
    ttft_sec: Option<(u64, u64)>,
    parent_cascade_id: Option<&'a str>,
    root_cascade_id: Option<&'a str>,
}

fn build_gen_blob(step_indices: &[u64], entries: &[GenStepSpec<'_>]) -> Vec<u8> {
    let mut out = Vec::new();
    for &idx in step_indices {
        out.extend(pb_varint(2, idx));
    }
    for e in entries {
        let mut f1 = pb_str(21, e.display_name);
        let mut usage = Vec::new();
        if e.input_tokens > 0 {
            usage.extend(pb_varint(2, e.input_tokens));
        }
        if e.output_tokens > 0 {
            usage.extend(pb_varint(3, e.output_tokens));
        }
        if e.cache_read_tokens > 0 {
            usage.extend(pb_varint(5, e.cache_read_tokens));
        }
        f1.extend(pb_bytes(4, &usage));
        if let Some((sec, nanos)) = e.ttft_sec {
            f1.extend(pb_duration_sec(11, sec, nanos));
        }
        if let Some((sec, nanos)) = e.stream_sec {
            f1.extend(pb_duration_sec(12, sec, nanos));
        }
        for (k, v) in [
            ("parent_cascade_id", e.parent_cascade_id),
            ("root_cascade_id", e.root_cascade_id),
        ] {
            if let Some(val) = v {
                let mut kv = pb_str(1, k);
                kv.extend(pb_str(2, val));
                f1.extend(pb_bytes(20, &kv));
            }
        }
        out.extend(pb_bytes(1, &f1));
    }
    out
}

fn write_conv_db(
    db_path: &Path,
    trajectory: Option<&[u8]>,
    steps: &[(i64, Vec<u8>)],
    gen_rows: &[(i64, Vec<u8>)],
) {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let conn = Connection::open(db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS steps (idx INTEGER PRIMARY KEY, metadata BLOB);
         CREATE TABLE IF NOT EXISTS gen_metadata (idx INTEGER PRIMARY KEY, data BLOB);
         CREATE TABLE IF NOT EXISTS trajectory_metadata_blob (id TEXT PRIMARY KEY, data BLOB);",
    )
    .unwrap();
    for (idx, meta) in steps {
        conn.execute(
            "INSERT INTO steps (idx, metadata) VALUES (?1, ?2)",
            params![idx, meta],
        )
        .unwrap();
    }
    for (idx, data) in gen_rows {
        conn.execute(
            "INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)",
            params![idx, data],
        )
        .unwrap();
    }
    if let Some(traj) = trajectory {
        conn.execute(
            "INSERT INTO trajectory_metadata_blob (id, data) VALUES ('main', ?1)",
            params![traj],
        )
        .unwrap();
    }
}

fn local_ms(y: i32, m: u32, d: u32, hh: u32, mm: u32, ss: u32) -> i64 {
    Local
        .with_ymd_and_hms(y, m, d, hh, mm, ss)
        .single()
        .expect("valid local datetime")
        .timestamp_millis()
}

#[test]
fn t05_dual_source_cli_ide_auto_scan_incremental_skip_and_0600_permissions() {
    let env = IsolatedEnv::new();
    let now_ms = local_ms(2026, 11, 20, 14, 0, 0);
    let t1 = now_ms - 3_600_000;
    let t2 = now_ms - 1_800_000;
    let t3 = now_ms - 900_000;

    // CLI root: main conversation + 2-level child cascade
    let cli_main = env.cli_data_dir.join("conversations").join("cli-root-1.db");
    write_conv_db(
        &cli_main,
        Some(&build_trajectory_blob(
            "file:///work/cli-proj",
            "feat/ledger",
            t1,
            Some("cli-root-1"),
        )),
        &[(1, build_step_metadata_blob(t1))],
        &[(
            1,
            build_gen_blob(
                &[1],
                &[GenStepSpec {
                    display_name: "Gemini 4 Carbon (High)",
                    input_tokens: 100_000,
                    cache_read_tokens: 400_000,
                    output_tokens: 10_000,
                    stream_sec: Some((50, 0)), // 200 tok/s
                    ttft_sec: Some((2, 0)),
                    parent_cascade_id: None,
                    root_cascade_id: None,
                }],
            ),
        )],
    );

    let cli_child = env.cli_data_dir.join("conversations").join("cli-child-2.db");
    write_conv_db(
        &cli_child,
        None,
        &[(1, build_step_metadata_blob(t2))],
        &[(
            1,
            build_gen_blob(
                &[1],
                &[GenStepSpec {
                    display_name: "Gemini 4 Neon (High)",
                    input_tokens: 50_000,
                    cache_read_tokens: 0,
                    output_tokens: 5_000,
                    stream_sec: Some((25, 0)), // 200 tok/s
                    ttft_sec: Some((1, 0)),
                    parent_cascade_id: Some("cli-root-1"),
                    root_cascade_id: Some("cli-root-1"),
                }],
            ),
        )],
    );

    // IDE root: standalone IDE conversation
    let ide_main = env.ide_data_dir.join("conversations").join("ide-conv-1.db");
    write_conv_db(
        &ide_main,
        Some(&build_trajectory_blob(
            "file:///work/ide-app",
            "main",
            t3,
            Some("ide-conv-1"),
        )),
        &[(1, build_step_metadata_blob(t3))],
        &[(
            1,
            build_gen_blob(
                &[1],
                &[GenStepSpec {
                    display_name: "Gemini 4 Hydrogen (High)",
                    input_tokens: 200_000,
                    cache_read_tokens: 800_000,
                    output_tokens: 20_000,
                    stream_sec: Some((100, 0)), // 200 tok/s
                    ttft_sec: Some((1, 500_000_000)),
                    parent_cascade_id: None,
                    root_cascade_id: None,
                }],
            ),
        )],
    );

    let ledger_path = env.cli_data_dir.join("agy-pulse-ledger.sqlite");
    let mut store = LedgerStore::open_at(&ledger_path).expect("open ledger");
    let sync_opts = LedgerSyncOptions {
        source_roots: Some(vec![
            SourceRoot {
                root: env.cli_data_dir.clone(),
                source: LedgerSource::Cli,
            },
            SourceRoot {
                root: env.ide_data_dir.clone(),
                source: LedgerSource::Ide,
            },
        ]),
        now_ms: Some(now_ms),
        ..Default::default()
    };

    let cold = store.sync_all(&sync_opts).expect("cold sync");
    assert!(cold.ok);
    assert_eq!(cold.total_dbs, 3);
    assert_eq!(cold.cli_dbs, 2);
    assert_eq!(cold.ide_dbs, 1);
    assert_eq!(cold.newly_synced_dbs, 3);
    assert_eq!(cold.steps_upserted, 3);

    // Secondary sync with unchanged DBs must skip re-parsing
    let warm = store.sync_all(&sync_opts).expect("warm sync");
    assert!(warm.ok);
    assert_eq!(warm.newly_synced_dbs, 0);
    assert_eq!(warm.steps_upserted, 0);

    // Unix permissions must be 0600 on ledger sqlite file
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&ledger_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "ledger sqlite permissions must be 0600");
    }

    // No sidecars left on source DBs
    assert!(!env.cli_data_dir.join("conversations/cli-root-1.db-wal").exists());
    assert!(!env.ide_data_dir.join("conversations/ide-conv-1.db-wal").exists());

    // Overview verifies merged CLI + IDE and Cascade tree folding
    let overview = store
        .query_overview(&LedgerQueryFilter {
            range: LedgerTimeRange::Last7d,
            now_ms: Some(now_ms),
            ..Default::default()
        })
        .expect("query overview");
    assert_eq!(overview.meta.available_sources, vec!["cli", "ide"]);
    assert_eq!(overview.summary.total_steps, 3);
    assert_eq!(overview.summary.root_cascade_count, 2);
    assert_eq!(overview.summary.subagent_conversation_count, 1);
    assert_eq!(overview.cascades.len(), 2);

    let cli_cascade = overview
        .cascades
        .iter()
        .find(|c| c.root_conversation_id == "cli-root-1")
        .expect("cli cascade present");
    assert_eq!(cli_cascade.source, LedgerSource::Cli);
    assert_eq!(cli_cascade.subagent_count, 1);
    assert_eq!(cli_cascade.children.len(), 1);
    assert_eq!(cli_cascade.children[0].conversation_id, "cli-child-2");

    // Export CSV check
    let csv_cascades = store
        .export_csv(
            &LedgerQueryFilter {
                range: LedgerTimeRange::Last7d,
                source: LedgerSourceFilter::Ide,
                now_ms: Some(now_ms),
                ..Default::default()
            },
            CsvExportKind::Cascades,
        )
        .expect("csv export");
    assert!(csv_cascades.contains("ide-conv-1,ide,"));
    assert!(!csv_cascades.contains("cli-root-1"));
}

#[test]
fn t05_rolling_daily_weekly_cost_and_compaction_rollback_protection() {
    let env = IsolatedEnv::new();
    let ledger_path = env.cli_data_dir.join("agy-pulse-ledger.sqlite");
    let mut store = LedgerStore::open_at(&ledger_path).expect("open ledger");

    let day_ms = 24 * 60 * 60 * 1000;
    let day0 = local_ms(2026, 11, 10, 12, 0, 0);

    // 1. Day 0: first sighting at $1.00 -> baseline=1.00 (delta 0.00), then grows to $4.00 (+$3.00)
    let r1 = store
        .record_rolling_costs(&RollingCostInput {
            cursor_id: "sess-compact",
            cascade_root_id: Some("sess-compact"),
            live_cost_usd: Some(1.00),
            seven_day_reset_at_ms: None,
            now_ms: day0,
        })
        .unwrap()
        .unwrap();
    assert!((r1.today_usd - 0.0).abs() < 1e-9);
    assert!((r1.week_usd - 0.0).abs() < 1e-9);

    let r2 = store
        .record_rolling_costs(&RollingCostInput {
            cursor_id: "sess-compact",
            cascade_root_id: Some("sess-compact"),
            live_cost_usd: Some(4.00),
            seven_day_reset_at_ms: None,
            now_ms: day0 + 1_000,
        })
        .unwrap()
        .unwrap();
    assert!((r2.today_usd - 3.00).abs() < 1e-9);
    assert!((r2.week_usd - 3.00).abs() < 1e-9);

    // 2. Day 1: midnight rollover -> today resets to 0, week retains $3.00; grows $4.00 -> $6.00 (+$2.00)
    let day1 = day0 + day_ms;
    let r3 = store
        .record_rolling_costs(&RollingCostInput {
            cursor_id: "sess-compact",
            cascade_root_id: Some("sess-compact"),
            live_cost_usd: Some(6.00),
            seven_day_reset_at_ms: None,
            now_ms: day1,
        })
        .unwrap()
        .unwrap();
    assert!((r3.today_usd - 2.00).abs() < 1e-9);
    assert!((r3.week_usd - 5.00).abs() < 1e-9);

    // 3. Context compaction / reset (>50% drop: $6.00 -> $1.20): MUST NOT subtract, advances cursor!
    let r4 = store
        .record_rolling_costs(&RollingCostInput {
            cursor_id: "sess-compact",
            cascade_root_id: Some("sess-compact"),
            live_cost_usd: Some(1.20),
            seven_day_reset_at_ms: None,
            now_ms: day1 + 2_000,
        })
        .unwrap()
        .unwrap();
    assert!((r4.today_usd - 2.00).abs() < 1e-9);
    assert!((r4.week_usd - 5.00).abs() < 1e-9);

    // Post-compaction growth $1.20 -> $2.70 (+$1.50) adds cleanly on top -> today=$3.50, week=$6.50
    let r5 = store
        .record_rolling_costs(&RollingCostInput {
            cursor_id: "sess-compact",
            cascade_root_id: Some("sess-compact"),
            live_cost_usd: Some(2.70),
            seven_day_reset_at_ms: None,
            now_ms: day1 + 3_000,
        })
        .unwrap()
        .unwrap();
    assert!((r5.today_usd - 3.50).abs() < 1e-9);
    assert!((r5.week_usd - 6.50).abs() < 1e-9);

    // 4. Keep cursor fresh across days 2..6 and verify 7-day rolling window rollover on day 7
    for d in 2..=6 {
        store
            .record_rolling_costs(&RollingCostInput {
                cursor_id: "sess-compact",
                cascade_root_id: Some("sess-compact"),
                live_cost_usd: Some(2.70),
                seven_day_reset_at_ms: None,
                now_ms: day0 + d * day_ms + 3_000,
            })
            .unwrap();
    }
    let day7 = day0 + 7 * day_ms + 3_000;
    let r_week_reset = store
        .record_rolling_costs(&RollingCostInput {
            cursor_id: "sess-compact",
            cascade_root_id: Some("sess-compact"),
            live_cost_usd: Some(3.95),
            seven_day_reset_at_ms: None,
            now_ms: day7,
        })
        .unwrap()
        .unwrap();
    assert!((r_week_reset.today_usd - 1.25).abs() < 1e-9);
    assert!((r_week_reset.week_usd - 1.25).abs() < 1e-9);

    // 5. End-to-end statusline binary renders Today / Week & 今日 / 本周 without regression on compaction
    let env_e2e = IsolatedEnv::new();
    fs::write(
        env_e2e.global_cfg_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "language": "en",
            "display": {
                "showCost": true,
                "showDailyCost": true,
                "showWeeklyCost": true
            }
        })
        .to_string(),
    )
    .unwrap();

    // Pre-existing DB step for today so Turn 1 has immediate DB cost AND zero double-counting
    let now_ts = Local::now().timestamp_millis();
    let pre_db = env_e2e.cli_data_dir.join("conversations").join("conv-live-1.db");
    let expected_step_cost = calculate_token_cost(
        "Gemini 4 Carbon (High)",
        &TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 100_000,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_exclusive: false,
        },
        false,
        false,
    )
    .unwrap();
    write_conv_db(
        &pre_db,
        Some(&build_trajectory_blob(
            "file:///work/live",
            "main",
            now_ts - 60_000,
            Some("conv-live-1"),
        )),
        &[(1, build_step_metadata_blob(now_ts - 60_000))],
        &[(
            1,
            build_gen_blob(
                &[1],
                &[GenStepSpec {
                    display_name: "Gemini 4 Carbon (High)",
                    input_tokens: 1_000_000,
                    cache_read_tokens: 0,
                    output_tokens: 100_000,
                    stream_sec: Some((50, 0)),
                    ttft_sec: Some((1, 0)),
                    parent_cascade_id: None,
                    root_cascade_id: None,
                }],
            ),
        )],
    );

    // Turn 1: DB already has expected_step_cost ($3.00 for Carbon: 1M*2.0 + 0.1M*10.0 = $3.00)
    assert!((expected_step_cost - 3.00).abs() < 1e-6);
    let p1 = serde_json::json!({
        "conversation_id": "conv-live-1",
        "cost": { "total_cost_usd": 3.00 }
    })
    .to_string();
    let (c1, out1, err1) = env_e2e.run_stdin(&agy_pulse_bin(), &p1, false, &[("COLUMNS", "140")]);
    assert_eq!(c1, 0, "{err1}");
    let plain1 = strip_ansi(&out1);
    assert!(
        plain1.contains("Today $3.00") && plain1.contains("Week $3.00"),
        "expected Today $3.00 and Week $3.00 without double-counting, got:\n{plain1}"
    );

    // Turn 2: live cost grows to $4.50 (+$1.50)
    let p2 = serde_json::json!({
        "conversation_id": "conv-live-1",
        "cost": { "total_cost_usd": 4.50 }
    })
    .to_string();
    let (c2, out2, _) = env_e2e.run_stdin(&agy_pulse_bin(), &p2, false, &[("COLUMNS", "140")]);
    assert_eq!(c2, 0);
    let plain2 = strip_ansi(&out2);
    assert!(
        plain2.contains("Today $4.50") && plain2.contains("Week $4.50"),
        "expected Today $4.50 and Week $4.50, got:\n{plain2}"
    );

    // Turn 3: compaction resets live cost from $4.50 down to $1.00 -> Today/Week MUST stay $4.50!
    let p3 = serde_json::json!({
        "conversation_id": "conv-live-1",
        "cost": { "total_cost_usd": 1.00 }
    })
    .to_string();
    let (c3, out3, _) = env_e2e.run_stdin(&agy_pulse_bin(), &p3, false, &[("COLUMNS", "140")]);
    assert_eq!(c3, 0);
    let plain3 = strip_ansi(&out3);
    assert!(
        plain3.contains("Today $4.50") && plain3.contains("Week $4.50"),
        "compaction must not regress Today/Week, got:\n{plain3}"
    );

    // Turn 4: post-compaction growth $1.00 -> $2.25 (+$1.25) with zh language -> 今日 $5.75 | 本周 $5.75
    let p4 = serde_json::json!({
        "conversation_id": "conv-live-1",
        "cost": { "total_cost_usd": 2.25 }
    })
    .to_string();
    let (c4, out4, _) = env_e2e.run_stdin(
        &agy_pulse_bin(),
        &p4,
        false,
        &[("COLUMNS", "140"), ("AGY_PULSE_LANGUAGE", "zh")],
    );
    assert_eq!(c4, 0);
    let plain4 = strip_ansi(&out4);
    assert!(
        plain4.contains("今日 $5.75") && plain4.contains("本周 $5.75"),
        "expected 今日 $5.75 and 本周 $5.75 after post-compaction growth, got:\n{plain4}"
    );
}

#[test]
fn t05_concurrent_statusline_and_stats_processes_have_no_sqlite_busy_failures() {
    let env = IsolatedEnv::new();
    fs::write(
        env.global_cfg_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "display": { "showDailyCost": true, "showWeeklyCost": true }
        })
        .to_string(),
    )
    .unwrap();

    let now_ts = Local::now().timestamp_millis();
    for i in 0..4 {
        let db_path = env
            .cli_data_dir
            .join("conversations")
            .join(format!("conc-{i}.db"));
        write_conv_db(
            &db_path,
            Some(&build_trajectory_blob(
                "file:///work/conc",
                "main",
                now_ts - 10_000,
                None,
            )),
            &[(1, build_step_metadata_blob(now_ts - 5_000))],
            &[(
                1,
                build_gen_blob(
                    &[1],
                    &[GenStepSpec {
                        display_name: "Gemini 4 Carbon (High)",
                        input_tokens: 10_000,
                        cache_read_tokens: 90_000,
                        output_tokens: 1_000,
                        stream_sec: Some((5, 0)),
                        ttft_sec: Some((1, 0)),
                        parent_cascade_id: None,
                        root_cascade_id: None,
                    }],
                ),
            )],
        );
    }

    let mut handles = Vec::new();
    for i in 0..10 {
        let home = env.home_dir.clone();
        let cli_dir = env.cli_data_dir.clone();
        let ide_dir = env.ide_data_dir.clone();
        let gcfg = env.global_cfg_dir.clone();
        let xcfg = env.xdg_cfg_dir.clone();
        let app_data = env.app_data_dir.clone();
        let local_app_data = env.local_app_data_dir.clone();
        let ws = env.workspace_dir.clone();
        handles.push(thread::spawn(move || {
            let is_stats = i % 3 == 0;
            let bin = if is_stats { agypl_bin() } else { agy_pulse_bin() };
            let mut cmd = Command::new(bin);
            cmd.current_dir(&ws)
                .env("HOME", &home)
                .env("USERPROFILE", &home)
                .env("XDG_DATA_HOME", home.join(".local/share"))
                .env("XDG_CONFIG_HOME", &xcfg)
                .env("APPDATA", &app_data)
                .env("AppData", &app_data)
                .env("LOCALAPPDATA", &local_app_data)
                .env("LocalAppData", &local_app_data)
                .env("AGY_PULSE_DATA_DIR", &cli_dir)
                .env("AGY_PULSE_IDE_DATA_DIR", &ide_dir)
                .env("AGY_PULSE_GLOBAL_CONFIG_DIR", &gcfg)
                .env("AGY_PULSE_NO_DASHBOARD_AUTOSTART", "1");

            if is_stats {
                let out = cmd
                    .args(["stats", "7d", "--json"])
                    .stdin(Stdio::null())
                    .output()
                    .unwrap();
                assert_eq!(
                    out.status.code(),
                    Some(0),
                    "stats process {i} failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            } else {
                let payload = serde_json::json!({
                    "conversation_id": format!("conc-{}", i % 4),
                    "cost": { "total_cost_usd": 1.0 + (i as f64) * 0.25 }
                })
                .to_string();
                let mut child = cmd
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap();
                if let Some(mut stdin) = child.stdin.take() {
                    stdin.write_all(payload.as_bytes()).unwrap();
                }
                let out = child.wait_with_output().unwrap();
                assert_eq!(
                    out.status.code(),
                    Some(0),
                    "statusline process {i} failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
        }));
    }

    for h in handles {
        h.join().expect("thread join");
    }

    let err_log = env.cli_data_dir.join("agy-pulse-error.log");
    if err_log.exists() {
        let text = fs::read_to_string(&err_log).unwrap();
        assert!(
            !text.contains("SQLITE_BUSY") && !text.contains("database is locked"),
            "unexpected SQLite lock contention in error log:\n{text}"
        );
    }
}

#[test]
fn t05_stats_subcommand_cli_ide_json_bilingual_and_exact_metrics() {
    let env = IsolatedEnv::new();
    let now_ms = Local::now().timestamp_millis();

    // CLI DB: Gemini 4 Carbon (High): 100k input + 400k cache_read + 10k output, stream=50s (200.0 tok/s)
    // Pricing for Carbon: input $2.00/M, output $10.00/M, cache_read $0.10/M
    // Cost = 0.1 * 2.00 + 0.4 * 0.10 + 0.01 * 10.00 = 0.20 + 0.04 + 0.10 = $0.34
    write_conv_db(
        &env.cli_data_dir.join("conversations").join("cli-stats-1.db"),
        Some(&build_trajectory_blob(
            "file:///repos/cli-app",
            "main",
            now_ms - 120_000,
            Some("cli-stats-1"),
        )),
        &[(1, build_step_metadata_blob(now_ms - 60_000))],
        &[(
            1,
            build_gen_blob(
                &[1],
                &[GenStepSpec {
                    display_name: "Gemini 4 Carbon (High)",
                    input_tokens: 100_000,
                    cache_read_tokens: 400_000,
                    output_tokens: 10_000,
                    stream_sec: Some((50, 0)),
                    ttft_sec: Some((2, 0)),
                    parent_cascade_id: None,
                    root_cascade_id: None,
                }],
            ),
        )],
    );

    // IDE DB: Gemini 4 Hydrogen (High): 200k input + 800k cache_read + 20k output, stream=100s (200.0 tok/s)
    // Pricing for Hydrogen: input $1.50/M, output $7.50/M, cache_read $0.075/M
    // Cost = 0.2 * 1.50 + 0.8 * 0.075 + 0.02 * 7.50 = 0.30 + 0.06 + 0.15 = $0.51
    write_conv_db(
        &env.ide_data_dir.join("conversations").join("ide-stats-1.db"),
        Some(&build_trajectory_blob(
            "file:///repos/ide-app",
            "feat/ide",
            now_ms - 90_000,
            Some("ide-stats-1"),
        )),
        &[(1, build_step_metadata_blob(now_ms - 30_000))],
        &[(
            1,
            build_gen_blob(
                &[1],
                &[GenStepSpec {
                    display_name: "Gemini 4 Hydrogen (High)",
                    input_tokens: 200_000,
                    cache_read_tokens: 800_000,
                    output_tokens: 20_000,
                    stream_sec: Some((100, 0)),
                    ttft_sec: Some((1, 0)),
                    parent_cascade_id: None,
                    root_cascade_id: None,
                }],
            ),
        )],
    );

    // 1. `agypl stats 7d --en`
    let (c_en, out_en, err_en) =
        env.run_args(&agypl_bin(), &["stats", "7d", "--en"], true, &[("NO_COLOR", "1")]);
    assert_eq!(c_en, 0, "{err_en}");
    let plain_en = strip_ansi(&out_en);
    assert!(plain_en.contains("agy-pulse Telemetry Stats"), "{plain_en}");
    assert!(plain_en.contains("$0.850"), "expected $0.850 total cost:\n{plain_en}");
    assert!(plain_en.contains("1.5M"), "expected 1.5M tokens:\n{plain_en}");
    assert!(plain_en.contains("New 300k | Read 1.2M | Out 30k"), "{plain_en}");
    assert!(plain_en.contains("200.0 tok/s"), "expected 200.0 tok/s:\n{plain_en}");
    assert!(plain_en.contains("Carbon"), "{plain_en}");
    assert!(plain_en.contains("Hydro"), "{plain_en}");
    assert!(plain_en.contains("[CLI] cli-app"), "{plain_en}");
    assert!(plain_en.contains("[IDE] ide-app"), "{plain_en}");

    // 2. `agypl stats 7d --zh`
    let (c_zh, out_zh, err_zh) =
        env.run_args(&agypl_bin(), &["stats", "7d", "--zh"], true, &[("NO_COLOR", "1")]);
    assert_eq!(c_zh, 0, "{err_zh}");
    let plain_zh = strip_ansi(&out_zh);
    assert!(plain_zh.contains("agy-pulse 遥测统计报表"), "{plain_zh}");
    assert!(plain_zh.contains("核心指标总览"), "{plain_zh}");
    assert!(plain_zh.contains("模型明细"), "{plain_zh}");
    assert!(plain_zh.contains("$0.850"), "{plain_zh}");

    // 3. `agy-pulse --stats --source ide --json`
    let (c_json, out_json, err_json) = env.run_args(
        &agy_pulse_bin(),
        &["--stats", "--source", "ide", "--json"],
        true,
        &[],
    );
    assert_eq!(c_json, 0, "{err_json}");
    let parsed: Value = serde_json::from_str(&out_json).expect("valid stats JSON");
    assert_eq!(parsed["meta"]["source"], "ide");
    assert_eq!(parsed["meta"]["availableSources"], serde_json::json!(["cli", "ide"]));
    assert_eq!(parsed["summary"]["totalSteps"], 1);
    let ide_cost = parsed["summary"]["totalCostUsd"].as_f64().unwrap();
    assert!((ide_cost - 0.51).abs() < 1e-9, "expected 0.51, got {ide_cost}");
    assert_eq!(parsed["summary"]["inputTokens"], 200_000);
    assert_eq!(parsed["summary"]["cacheReadTokens"], 800_000);
    assert_eq!(parsed["summary"]["outputTokens"], 20_000);
    let ide_speed = parsed["summary"]["avgSpeed"].as_f64().unwrap();
    assert!((ide_speed - 200.0).abs() < 1e-9, "expected 200.0, got {ide_speed}");
    assert_eq!(parsed["byModel"][0]["model"], "Hydro");
    assert_eq!(parsed["sessions"][0]["rootConversationId"], "ide-stats-1");
    assert_eq!(parsed["sessions"][0]["source"], "ide");

    // 4. `agypl stats --help` routes to stats help, and `--csv=models` exports CSV
    let (c_help, out_help, _) = env.run_args(&agypl_bin(), &["stats", "--help"], false, &[]);
    assert_eq!(c_help, 0);
    assert!(out_help.contains("--source"));
    let (c_csv, out_csv, _) =
        env.run_args(&agypl_bin(), &["stats", "--csv=models", "--no-sync"], true, &[]);
    assert_eq!(c_csv, 0);
    assert!(out_csv.starts_with("model,costUsd,costSharePct,"));
    assert!(out_csv.contains("Hydro,0.51,60,"));
}
