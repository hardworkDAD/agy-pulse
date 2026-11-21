#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};
use serde_json::Value;
use tempfile::TempDir;

use agy_pulse::render::{ansi_display_width, strip_ansi};

fn agy_pulse_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agy-pulse"))
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
        let workspace_dir = root.join("ws");
        let tmp_dir = root.join("tmp");

        for d in [
            &home_dir,
            &xdg_data_home,
            &xdg_cfg_home,
            &cli_data_dir,
            &ide_data_dir,
            &global_cfg_dir,
            &bin_dir,
            &workspace_dir,
            &tmp_dir,
        ] {
            fs::create_dir_all(d).unwrap();
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
            workspace_dir,
            tmp_dir,
        }
    }

    fn base_cmd(&self, program: &Path) -> Command {
        let mut cmd = Command::new(program);
        cmd.current_dir(&self.workspace_dir)
            .env("HOME", &self.home_dir)
            .env("USERPROFILE", &self.home_dir)
            .env("XDG_DATA_HOME", &self.xdg_data_home)
            .env("XDG_CONFIG_HOME", &self.xdg_cfg_home)
            .env("AGY_PULSE_DATA_DIR", &self.cli_data_dir)
            .env("AGY_PULSE_IDE_DATA_DIR", &self.ide_data_dir)
            .env("AGY_PULSE_GLOBAL_CONFIG_DIR", &self.global_cfg_dir)
            .env("AGY_PULSE_BIN_DIR", &self.bin_dir)
            .env("AGY_PULSE_DISABLE_UPDATE_CHECK", "1")
            .env("AGY_PULSE_NO_OPEN", "1")
            .env("AGY_PULSE_NO_DASHBOARD_AUTOSTART", "1")
            .env("TMPDIR", &self.tmp_dir)
            .env_remove("AGY_PULSE_FORCE_ASCII")
            .env_remove("AGY_PULSE_FORCE_UNICODE")
            .env_remove("AGY_PULSE_LANGUAGE")
            .env("LC_ALL", "en_US.UTF-8")
            .env("LANG", "en_US.UTF-8")
            .env("COLUMNS", "160");
        cmd
    }

    fn run_stdin(&self, bin: &Path, stdin_text: &str, extra_env: &[(&str, &str)]) -> (i32, String, String) {
        let mut cmd = self.base_cmd(bin);
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

// --- Minimal Protobuf wire encoders for golden DB fixtures ---

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

fn pb_executor_limit_blob(max_ctx: u64) -> Vec<u8> {
    let f2 = pb_varint(5, max_ctx);
    let f10 = pb_bytes(2, &f2);
    pb_bytes(10, &f10)
}

#[derive(Default)]
struct GenEntrySpec<'a> {
    sys_tokens: u64,
    raw_in: u64,
    out: u64,
    cache_read: u64,
    ttft: Option<(u64, u64)>,
    stream: Option<(u64, u64)>,
    display_name: Option<&'a str>,
    raw_model: Option<&'a str>,
    kvs: &'a [(&'a str, &'a str)],
    step_indices: &'a [u64],
}

fn pb_gen_metadata_blob(entries: &[GenEntrySpec<'_>]) -> Vec<u8> {
    let mut out = Vec::new();
    for spec in entries {
        for &idx in spec.step_indices {
            out.extend(pb_varint(2, idx));
        }
        let mut f4 = Vec::new();
        if spec.sys_tokens > 0 {
            f4.extend(pb_varint(1, spec.sys_tokens));
        }
        if spec.raw_in > 0 {
            f4.extend(pb_varint(2, spec.raw_in));
        }
        if spec.out > 0 {
            f4.extend(pb_varint(3, spec.out));
        }
        if spec.cache_read > 0 {
            f4.extend(pb_varint(5, spec.cache_read));
        }

        let mut f1 = pb_bytes(4, &f4);
        if let Some((sec, nanos)) = spec.ttft {
            f1.extend(pb_duration(11, sec, nanos));
        }
        if let Some((sec, nanos)) = spec.stream {
            f1.extend(pb_duration(12, sec, nanos));
        }
        if let Some(raw) = spec.raw_model {
            f1.extend(pb_str(19, raw));
        }
        for &(k, v) in spec.kvs {
            let mut kv = pb_str(1, k);
            kv.extend(pb_str(2, v));
            f1.extend(pb_bytes(20, &kv));
        }
        if let Some(disp) = spec.display_name {
            f1.extend(pb_str(21, disp));
        }
        out.extend(pb_bytes(1, &f1));
    }
    out
}

fn epoch_secs_to_iso8601_utc(epoch_secs: i64) -> String {
    let days = epoch_secs.div_euclid(86_400);
    let secs_of_day = epoch_secs.rem_euclid(86_400);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;

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
fn t10_golden_parity_single_turn_compaction_cascade_and_fallback() {
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    // Scenario 1: Single-turn normal conversation (git branch, rules, tokens, cache hit rate, cost, context bar, model cache, context monitor)
    {
        let env = IsolatedEnv::new();
        fs::create_dir_all(env.workspace_dir.join(".git/refs/heads/feat")).unwrap();
        fs::write(
            env.workspace_dir.join(".git/HEAD"),
            "ref: refs/heads/feat/rust-v2\n",
        )
        .unwrap();
        fs::write(
            env.workspace_dir.join(".git/refs/heads/feat/rust-v2"),
            "0123456789abcdef\n",
        )
        .unwrap();
        fs::create_dir_all(env.workspace_dir.join(".gemini/rules")).unwrap();
        fs::write(env.workspace_dir.join(".gemini/rules/r1.md"), "# rule\n").unwrap();
        fs::write(
            env.global_cfg_dir.join("agy-pulse.config.json"),
            serde_json::json!({
                "language": "en",
                "display": {
                    "quotaStyle": "none",
                    "showModel": true,
                    "showGitBranch": true,
                    "showTokenBar": true,
                    "showContextBar": true,
                    "showTurnCacheHitRate": true,
                    "showCost": true,
                    "showBreadcrumbs": true,
                    "showContextMonitor": true,
                    "showModelCache": true
                }
            })
            .to_string(),
        )
        .unwrap();

        let conv_id = "conv-single-1";
        let tp = env
            .cli_data_dir
            .join(format!("brain/{conv_id}/.system_generated/logs/transcript.jsonl"));
        fs::create_dir_all(tp.parent().unwrap()).unwrap();
        let t0 = epoch_secs_to_iso8601_utc(now_secs - 4);
        let t1 = epoch_secs_to_iso8601_utc(now_secs - 2);
        fs::write(
            &tp,
            format!(
                "{}\n{}\n",
                serde_json::json!({ "step_index": 0, "type": "USER_INPUT", "created_at": t0, "content": "hello" }),
                serde_json::json!({
                    "step_index": 1,
                    "type": "PLANNER_RESPONSE",
                    "created_at": t1,
                    "model": "claude-sonnet-4-6",
                    "input_tokens": 3000,
                    "output_tokens": 600,
                    "cache_read_tokens": 8000,
                    "cache_creation_tokens": 1000
                })
            ),
        )
        .unwrap();

        let stdin = serde_json::json!({
            "conversation_id": conv_id,
            "model": { "id": "claude-sonnet-4-6", "display_name": "Claude Sonnet 4.6" },
            "plan_tier": "Pro",
            "cycle_mode": "auto",
            "workspace": { "current_dir": env.workspace_dir },
            "context_window": {
                "context_window_size": 200_000,
                "total_input_tokens": 12_000,
                "total_output_tokens": 600,
                "current_usage": {
                    "input_tokens": 12_000,
                    "output_tokens": 600,
                    "cache_read_input_tokens": 8_000,
                    "cache_creation_input_tokens": 1_000
                }
            }
        })
        .to_string();

        let (code, out, err) = env.run_stdin(&agy_pulse_bin(), &stdin, &[]);
        assert_eq!(code, 0, "stderr: {err}");
        let plain = strip_ansi(&out);
        let lines: Vec<&str> = plain.lines().collect();
        assert_eq!(lines.len(), 5, "expected 5 lines, got:\n{plain}");
        assert!(lines[0].starts_with("ws │ ⎇ feat/rust-v2 │ Claude Sonnet 4.6 │ Pro │ auto │ Cost $0.0242 │ out: "));
        assert_eq!(
            lines[1],
            "⚿ Tokens 12.6k (in: 3k, out: 600, cache: 9k) │ cache 66.7% (8k/12k) │ ⛁ 12.6k/200k [█░░░░░░░░░] 6%"
        );
        assert_eq!(lines[2], "1 rules");
        assert_eq!(
            lines[3],
            "⟳ Session Cache · Sonnet 67% (hit 8K · write 1K / in 12K)"
        );
        assert_eq!(
            lines[4],
            "◈ Claude Sonnet 4.6 · Ctx ▓··········· 6.3% (12.6K/200K) · Cache 67% (8K) · Out 600 · Cost $0.02"
        );
    }

    // Scenario 2: Multi-turn CHECKPOINT compaction & historical peak preservation
    {
        let env = IsolatedEnv::new();
        fs::write(
            env.global_cfg_dir.join("agy-pulse.config.json"),
            serde_json::json!({
                "language": "en",
                "display": {
                    "quotaStyle": "none",
                    "showModel": true,
                    "showTokenBar": true,
                    "showContextBar": true,
                    "showContextMonitor": true,
                    "showModelCache": true
                }
            })
            .to_string(),
        )
        .unwrap();

        let tp = env
            .cli_data_dir
            .join("brain/conv-compact/.system_generated/logs/transcript.jsonl");
        fs::create_dir_all(tp.parent().unwrap()).unwrap();
        let entries = [
            serde_json::json!({ "step_index": 0, "type": "USER_INPUT", "created_at": epoch_secs_to_iso8601_utc(now_secs - 30), "content": "turn 1" }),
            serde_json::json!({ "step_index": 1, "type": "PLANNER_RESPONSE", "created_at": epoch_secs_to_iso8601_utc(now_secs - 25), "model": "claude-sonnet-4-6", "input_tokens": 45_000, "output_tokens": 2_000, "cache_read_tokens": 120_000 }),
            serde_json::json!({ "step_index": 2, "type": "CHECKPOINT", "created_at": epoch_secs_to_iso8601_utc(now_secs - 15), "content": "compacted summary" }),
            serde_json::json!({ "step_index": 3, "type": "USER_INPUT", "created_at": epoch_secs_to_iso8601_utc(now_secs - 10), "content": "turn 2" }),
            serde_json::json!({ "step_index": 4, "type": "PLANNER_RESPONSE", "created_at": epoch_secs_to_iso8601_utc(now_secs - 5), "model": "claude-sonnet-4-6", "input_tokens": 13_000, "output_tokens": 1_000, "cache_read_tokens": 15_000 }),
        ];
        let body = entries
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        fs::write(&tp, body).unwrap();

        let stdin = serde_json::json!({
            "conversation_id": "conv-compact",
            "model": { "id": "claude-sonnet-4-6", "display_name": "Claude Sonnet 4.6" },
            "context_window": { "context_window_size": 200_000 }
        })
        .to_string();

        let (code, out, err) = env.run_stdin(&agy_pulse_bin(), &stdin, &[]);
        assert_eq!(code, 0, "stderr: {err}");
        let plain = strip_ansi(&out);
        let lines: Vec<&str> = plain.lines().collect();
        assert_eq!(lines.len(), 4, "got:\n{plain}");
        assert!(lines[0].starts_with("ws │ Claude Sonnet 4.6 │ Free │ out: "));
        assert_eq!(
            lines[1],
            "⚿ Tokens 31k (in: 13k, out: 3k, cache: 15k) │ ⛁ 29k/200k [█░░░░░░░░░] 15%"
        );
        assert_eq!(
            lines[2],
            "⟳ Session Cache · Sonnet 70% (hit 135K / in 193K)"
        );
        assert_eq!(
            lines[3],
            "◈ Claude Sonnet 4.6 · Ctx █▒·········· 14.5% (29K/200K) · Compacted×1(peak 167K) · Cache 54% (15K) · Out 3K · Cost $0.26"
        );
    }

    // Scenario 3: Subagent .db + Cascade cost tree (including unified f4.f1 system prompt billing)
    {
        let env = IsolatedEnv::new();
        fs::write(
            env.global_cfg_dir.join("agy-pulse.config.json"),
            serde_json::json!({
                "language": "en",
                "display": {
                    "quotaStyle": "none",
                    "showModel": true,
                    "showTokenBar": true,
                    "showContextBar": true,
                    "showCost": true,
                    "showSpeed": true,
                    "showContextMonitor": true,
                    "showModelCache": true
                }
            })
            .to_string(),
        )
        .unwrap();

        let main_id = "main-cascade-1";
        let child_id = "child-sub-1";
        let main_tp = env
            .cli_data_dir
            .join(format!("brain/{main_id}/.system_generated/logs/transcript.jsonl"));
        let child_tp = env
            .cli_data_dir
            .join(format!("brain/{child_id}/.system_generated/logs/transcript.jsonl"));
        fs::create_dir_all(main_tp.parent().unwrap()).unwrap();
        fs::create_dir_all(child_tp.parent().unwrap()).unwrap();

        fs::write(
            &main_tp,
            format!(
                "{}\n{}\n{}\n",
                serde_json::json!({ "step_index": 0, "type": "USER_INPUT", "created_at": epoch_secs_to_iso8601_utc(now_secs - 20), "content": "analyze" }),
                serde_json::json!({
                    "step_index": 1,
                    "type": "PLANNER_RESPONSE",
                    "created_at": epoch_secs_to_iso8601_utc(now_secs - 18),
                    "model": "claude-sonnet-4-6",
                    "input_tokens": 5_000,
                    "output_tokens": 400,
                    "cache_read_tokens": 15_000,
                    "tool_calls": [{ "name": "invoke_subagent", "arguments": { "subagent_id": child_id } }]
                }),
                serde_json::json!({
                    "step_index": 2,
                    "type": "GENERIC",
                    "created_at": epoch_secs_to_iso8601_utc(now_secs - 12),
                    "content": format!("subagent {child_id}"),
                    "subagent_id": child_id,
                    "conversation_id": child_id
                })
            ),
        )
        .unwrap();

        fs::write(
            &child_tp,
            format!(
                "{}\n{}\n{}\n",
                serde_json::json!({ "step_index": 0, "type": "USER_INPUT", "created_at": epoch_secs_to_iso8601_utc(now_secs - 15), "content": "subtask" }),
                serde_json::json!({
                    "step_index": 1,
                    "type": "PLANNER_RESPONSE",
                    "created_at": epoch_secs_to_iso8601_utc(now_secs - 14),
                    "model": "gemini-3-flash-agent",
                    "input_tokens": 4_000,
                    "output_tokens": 800,
                    "cache_read_tokens": 6_000
                }),
                serde_json::json!({ "step_index": 2, "type": "GENERIC", "created_at": epoch_secs_to_iso8601_utc(now_secs - 10), "content": "done" })
            ),
        )
        .unwrap();

        let conv_dir = env.cli_data_dir.join("conversations");
        fs::create_dir_all(&conv_dir).unwrap();
        {
            let conn = Connection::open(conv_dir.join(format!("{main_id}.db"))).unwrap();
            conn.execute("CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB)", [])
                .unwrap();
            conn.execute("CREATE TABLE executor_metadata (idx INTEGER PRIMARY KEY, data BLOB)", [])
                .unwrap();
            conn.execute(
                "INSERT INTO executor_metadata (idx, data) VALUES (?1, ?2)",
                params![1i64, pb_executor_limit_blob(180_000)],
            )
            .unwrap();
            let blob = pb_gen_metadata_blob(&[GenEntrySpec {
                sys_tokens: 1_000,
                raw_in: 20_000,
                out: 400,
                cache_read: 15_000,
                ttft: Some((0, 400_000_000)),
                stream: Some((4, 0)),
                display_name: Some("Claude Sonnet 4.6"),
                raw_model: Some("claude-sonnet-4-6"),
                kvs: &[("last_step_index", "1")],
                step_indices: &[1],
            }]);
            conn.execute("INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)", params![1i64, blob])
                .unwrap();
        }
        {
            let conn = Connection::open(conv_dir.join(format!("{child_id}.db"))).unwrap();
            conn.execute("CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB)", [])
                .unwrap();
            conn.execute("CREATE TABLE executor_metadata (idx INTEGER PRIMARY KEY, data BLOB)", [])
                .unwrap();
            conn.execute(
                "INSERT INTO executor_metadata (idx, data) VALUES (?1, ?2)",
                params![1i64, pb_executor_limit_blob(512_000)],
            )
            .unwrap();
            let blob = pb_gen_metadata_blob(&[GenEntrySpec {
                sys_tokens: 500,
                raw_in: 10_000,
                out: 800,
                cache_read: 6_000,
                ttft: Some((0, 200_000_000)),
                stream: Some((4, 0)),
                display_name: Some("Gemini 3 Flash"),
                raw_model: Some("gemini-3-flash-agent"),
                kvs: &[("parent_cascade_id", main_id), ("root_cascade_id", main_id)],
                step_indices: &[],
            }]);
            conn.execute("INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)", params![1i64, blob])
                .unwrap();
        }

        let stdin = serde_json::json!({
            "conversation_id": main_id,
            "model": { "id": "claude-sonnet-4-6", "display_name": "Claude Sonnet 4.6" },
            "plan_tier": "Pro",
            "context_window": {
                "context_window_size": 200_000,
                "total_input_tokens": 20_000,
                "total_output_tokens": 400,
                "current_usage": {
                    "input_tokens": 20_000,
                    "output_tokens": 400,
                    "cache_read_input_tokens": 15_000
                }
            }
        })
        .to_string();

        let (code, out, err) = env.run_stdin(&agy_pulse_bin(), &stdin, &[]);
        assert_eq!(code, 0, "stderr: {err}");
        let plain = strip_ansi(&out);
        let lines: Vec<&str> = plain.lines().collect();
        assert_eq!(
            lines,
            vec![
                "ws │ Claude Sonnet 4.6 │ Pro │ Cost $0.0848 │ out: 100.0 tok/s (ttft 400ms) │ ⚡ Flash: 200.0 tok/s (ttft 200ms)",
                "⚿ Tokens 20.4k (in: 5k, out: 400, cache: 15k) │ ⛁ 20.4k/180k [█░░░░░░░░░] 11%",
                "⟳ Session Cache · Sonnet 42% (hit 15K / in 36K) · Flash 36% (hit 6K / in 16.5K)",
                "◈ Claude Sonnet 4.6 · Ctx █░·········· 11.3% (20.4K/180K) · Cache 75% (15K) · Out 400 · Cost $0.08",
            ]
        );
    }

    // Scenario 6: Empty & malformed JSON graceful fallback
    {
        let env = IsolatedEnv::new();
        fs::write(
            env.global_cfg_dir.join("agy-pulse.config.json"),
            serde_json::json!({
                "language": "en",
                "display": { "quotaStyle": "none", "showContextMonitor": true }
            })
            .to_string(),
        )
        .unwrap();

        let (c_empty, out_empty, err_empty) = env.run_stdin(&agy_pulse_bin(), "", &[]);
        assert_eq!(c_empty, 0);
        assert_eq!(err_empty, "");
        assert_eq!(
            strip_ansi(&out_empty),
            "ws │ Free\n◈ Unknown Model · Ctx ············ 0.0% (limit 512K)"
        );

        let (c_bad, out_bad, err_bad) = env.run_stdin(&agy_pulse_bin(), "{not valid json", &[]);
        assert_eq!(c_bad, 0);
        assert_eq!(err_bad, "");
        assert_eq!(strip_ansi(&out_bad), strip_ansi(&out_empty));
    }
}

#[test]
fn t10_golden_parity_quota_modes_narrow_columns_ascii_and_bilingual() {
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let future_5h = epoch_secs_to_iso8601_utc(now_secs + 2 * 3600 + 15 * 60 + 20);
    let future_wk = epoch_secs_to_iso8601_utc(now_secs + 3 * 86_400 + 4 * 3600 + 20);

    // Scenario 4A: Multi-provider quota `table` mode (plus legacy `models` fallback -> `table`)
    for style in ["table", "models"] {
        let env = IsolatedEnv::new();
        fs::write(
            env.global_cfg_dir.join("agy-pulse.config.json"),
            serde_json::json!({
                "language": "en",
                "display": {
                    "quotaStyle": style,
                    "showModel": true,
                    "showTokenBar": true,
                    "showContextBar": true,
                    "showContextMonitor": true
                }
            })
            .to_string(),
        )
        .unwrap();

        let stdin = serde_json::json!({
            "model": { "id": "gemini-3.1-pro", "display_name": "Gemini 3.1 Pro" },
            "plan_tier": "Ultra",
            "quota": {
                "gemini-5h": { "remaining_fraction": 0.85, "reset_time": future_5h },
                "gemini-weekly": { "remaining_fraction": 0.42, "reset_time": future_wk },
                "anthropic-5h": { "remaining_fraction": 0.15, "reset_time": future_5h },
                "anthropic-weekly": { "remaining_fraction": 0.05, "reset_time": future_wk },
                "argon-5h": { "remaining_fraction": 0.60, "reset_time": future_5h },
                "openai-5h": { "remaining_fraction": 0.10, "reset_time": future_5h }
            },
            "context_window": { "context_window_size": 512_000, "total_input_tokens": 5_000, "total_output_tokens": 300 }
        })
        .to_string();

        let (code, out, err) = env.run_stdin(&agy_pulse_bin(), &stdin, &[]);
        assert_eq!(code, 0, "stderr: {err}");
        let plain = strip_ansi(&out);
        let lines: Vec<&str> = plain.lines().collect();
        assert_eq!(lines.len(), 7, "style={style} got:\n{plain}");
        assert_eq!(lines[0], "ws │ Gemini 3.1 Pro │ Ultra");
        assert_eq!(lines[1], "⚿ Tokens 5.3k (in: 5k, out: 300) │ ⛁ 5k/512k [░░░░░░░░░░] 0%");
        assert_eq!(lines[2], "  ───────────────────────────────────────────────────────────────────");
        assert_eq!(lines[3], "  Google 5h   [█████░]  85% ~2h15m │ Claude 5h   [█░░░░░]  15% ~2h15m");
        assert_eq!(lines[4], "  Google week [███░░░]  42% ~3d4h  │ Claude week [░░░░░░]   5% ~3d4h ");
        assert_eq!(lines[5], "  Argon 5h    [████░░]  60% ~2h15m");
        assert!(!plain.contains("Openai") && !plain.contains("OpenAI"));
        assert_eq!(lines[6], "◈ Gemini 3.1 Pro · Ctx ············ 0.0% (limit 512K) · Out 300 · Cost $0.01");
    }

    // Scenario 4B: Multi-provider quota `compact` mode
    {
        let env = IsolatedEnv::new();
        fs::write(
            env.global_cfg_dir.join("agy-pulse.config.json"),
            serde_json::json!({
                "language": "en",
                "display": {
                    "quotaStyle": "compact",
                    "showModel": true,
                    "showTokenBar": true,
                    "showContextBar": true,
                    "showContextMonitor": true
                }
            })
            .to_string(),
        )
        .unwrap();

        let stdin = serde_json::json!({
            "model": { "id": "gemini-3.1-pro", "display_name": "Gemini 3.1 Pro" },
            "plan_tier": "Ultra",
            "quota": {
                "gemini-5h": { "remaining_fraction": 0.85, "reset_time": future_5h },
                "gemini-weekly": { "remaining_fraction": 0.42, "reset_time": future_wk },
                "anthropic-5h": { "remaining_fraction": 0.15, "reset_time": future_5h },
                "anthropic-weekly": { "remaining_fraction": 0.05, "reset_time": future_wk },
                "argon-5h": { "remaining_fraction": 0.60, "reset_time": future_5h }
            },
            "context_window": { "context_window_size": 512_000, "total_input_tokens": 5_000, "total_output_tokens": 300 }
        })
        .to_string();

        let (code, out, err) = env.run_stdin(&agy_pulse_bin(), &stdin, &[]);
        assert_eq!(code, 0, "stderr: {err}");
        assert_eq!(
            strip_ansi(&out),
            "ws │ Gemini 3.1 Pro │ Ultra\n\
             ⚿ Tokens 5.3k │ ⛁ 5k/512k [░░░░░░░░░░] 0%\n\
             Google: 42% │ Claude: 5% │ Argon: 60%\n\
             ◈ Gemini 3.1 Pro · Ctx ············ 0.0% (limit 512K) · Out 300 · Cost $0.01"
        );
    }

    // Scenario 5: Narrow terminal COLUMNS=64/42 + AGY_PULSE_FORCE_ASCII=1 + ZH/EN bilingual switching
    let narrow_stdin = serde_json::json!({
        "model": { "id": "claude-opus-4-6", "display_name": "Claude Opus 4.6" },
        "plan_tier": "Pro",
        "cycle_mode": "accept-edits",
        "quota": {
            "gemini-5h": { "remaining_fraction": 0.72, "reset_time": future_5h },
            "gemini-weekly": { "remaining_fraction": 0.55, "reset_time": future_wk },
            "anthropic-5h": { "remaining_fraction": 0.30, "reset_time": future_5h },
            "anthropic-weekly": { "remaining_fraction": 0.25, "reset_time": future_wk }
        },
        "context_window": {
            "context_window_size": 200_000,
            "total_input_tokens": 45_000,
            "total_output_tokens": 3_200,
            "current_usage": {
                "input_tokens": 45_000,
                "output_tokens": 3_200,
                "cache_read_input_tokens": 30_000,
                "cache_creation_input_tokens": 5_000
            }
        }
    })
    .to_string();

    // 5A: COLUMNS=64, UTF-8, ZH
    {
        let env = IsolatedEnv::new();
        fs::write(
            env.global_cfg_dir.join("agy-pulse.config.json"),
            serde_json::json!({
                "language": "zh",
                "display": {
                    "quotaStyle": "table",
                    "showModel": true,
                    "showTokenBar": true,
                    "showContextBar": true,
                    "showCost": true,
                    "showContextMonitor": true,
                    "showModelCache": true
                }
            })
            .to_string(),
        )
        .unwrap();
        let (code, out, _) = env.run_stdin(&agy_pulse_bin(), &narrow_stdin, &[("COLUMNS", "64")]);
        assert_eq!(code, 0);
        let plain = strip_ansi(&out);
        for line in plain.lines() {
            assert!(
                ansi_display_width(line) <= 64,
                "line exceeded 64 cols: {line}"
            );
        }
        assert!(plain.contains("费用 $0.176"));
        assert!(plain.contains("累计缓存"));
        assert!(plain.contains("上下文 █▓······ 24.1% (48.2K/200K) · 缓存 67%"));
    }

    // 5B: COLUMNS=42, AGY_PULSE_FORCE_ASCII=1, ZH
    {
        let env = IsolatedEnv::new();
        fs::write(
            env.global_cfg_dir.join("agy-pulse.config.json"),
            serde_json::json!({
                "language": "zh",
                "display": {
                    "quotaStyle": "table",
                    "showModel": true,
                    "showTokenBar": true,
                    "showContextBar": true,
                    "showCost": true,
                    "showContextMonitor": true,
                    "showModelCache": true
                }
            })
            .to_string(),
        )
        .unwrap();
        let (code, out, _) = env.run_stdin(
            &agy_pulse_bin(),
            &narrow_stdin,
            &[("COLUMNS", "42"), ("AGY_PULSE_FORCE_ASCII", "1")],
        );
        assert_eq!(code, 0);
        let plain = strip_ansi(&out);
        for line in plain.lines() {
            assert!(
                ansi_display_width(line) <= 42,
                "line exceeded 42 cols: {line}"
            );
        }
        assert!(plain.contains("~ 累计缓存 | Opus 67% (30K+5K/45K)"));
        assert!(plain.contains("上下文 =... 24.1% (48.2K/200K) | 缓存 67%"));
        assert!(!plain.contains('│') && !plain.contains('─') && !plain.contains('…'));
    }

    // 5C: COLUMNS=64, AGY_PULSE_FORCE_ASCII=1, EN
    {
        let env = IsolatedEnv::new();
        fs::write(
            env.global_cfg_dir.join("agy-pulse.config.json"),
            serde_json::json!({
                "language": "en",
                "display": {
                    "quotaStyle": "table",
                    "showModel": true,
                    "showTokenBar": true,
                    "showContextBar": true,
                    "showCost": true,
                    "showContextMonitor": true,
                    "showModelCache": true
                }
            })
            .to_string(),
        )
        .unwrap();
        let (code, out, _) = env.run_stdin(
            &agy_pulse_bin(),
            &narrow_stdin,
            &[("COLUMNS", "64"), ("AGY_PULSE_FORCE_ASCII", "1")],
        );
        assert_eq!(code, 0);
        let plain = strip_ansi(&out);
        for line in plain.lines() {
            assert!(
                ansi_display_width(line) <= 64,
                "line exceeded 64 cols: {line}"
            );
        }
        assert!(plain.contains("ws | Claude Opus 4.6 | Pro | accept-edits | Cost $0.176"));
        assert!(plain.contains("~ Session Cache | Opus 67% (hit 30K, write 5K / in 45K)"));
        assert!(plain.contains("Ctx #=...... 24.1% (48.2K/200K) | Cache 67%"));
    }
}

#[cfg(unix)]
mod unix_packaging_e2e {
    use std::io::{BufRead, BufReader, Read};
    use std::net::{TcpListener, TcpStream};
    use std::process::Child;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    use flate2::Compression;
    use flate2::write::GzEncoder;
    use sha2::{Digest, Sha256};

    use super::*;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    fn agypl_bin() -> PathBuf {
        PathBuf::from(env!("CARGO_BIN_EXE_agypl"))
    }

    fn preferred_release_or_test_bins() -> (PathBuf, PathBuf) {
        let release_dir = repo_root().join("target").join("release");
        let exe_suffix = std::env::consts::EXE_SUFFIX;
        let rel_primary = release_dir.join(format!("agy-pulse{exe_suffix}"));
        let rel_alias = release_dir.join(format!("agypl{exe_suffix}"));
        if rel_primary.is_file() && rel_alias.is_file() {
            (rel_primary, rel_alias)
        } else {
            (agy_pulse_bin(), agypl_bin())
        }
    }

    struct MockResponse {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
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
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            listener.set_nonblocking(true).unwrap();
            let port = listener.local_addr().unwrap().port();
            let base_url = format!("http://127.0.0.1:{port}");
            let stop = Arc::new(AtomicBool::new(false));
            let stop_flag = Arc::clone(&stop);
            let handler = Arc::new(handler);

            let handle = thread::spawn(move || {
                while !stop_flag.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let h = Arc::clone(&handler);
                            thread::spawn(move || {
                                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                                let mut reader = BufReader::new(stream.try_clone().unwrap());
                                let mut req_line = String::new();
                                if reader.read_line(&mut req_line).is_err() || req_line.is_empty() {
                                    return;
                                }
                                let path = req_line
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
                                let resp = h(&path);
                                let reason = if resp.status == 200 { "OK" } else { "Not Found" };
                                let mut head = format!(
                                    "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
                                    resp.status,
                                    reason,
                                    resp.body.len()
                                );
                                for (k, v) in resp.headers {
                                    head.push_str(&format!("{k}: {v}\r\n"));
                                }
                                head.push_str("\r\n");
                                let _ = stream.write_all(head.as_bytes());
                                let _ = stream.write_all(&resp.body);
                                let _ = stream.flush();
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

    fn append_ustar_entry(buf: &mut Vec<u8>, name: &str, mode: u32, data: &[u8]) {
        let mut header = [0u8; 512];
        let name_bytes = name.as_bytes();
        header[..name_bytes.len()].copy_from_slice(name_bytes);
        header[100..108].copy_from_slice(format!("{mode:07o}\0").as_bytes());
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        header[124..136].copy_from_slice(format!("{:011o}\0", data.len()).as_bytes());
        header[136..148].copy_from_slice(b"00000000000\0");
        header[148..156].copy_from_slice(b"        ");
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let cksum: u32 = header.iter().map(|&b| u32::from(b)).sum();
        header[148..156].copy_from_slice(format!("{cksum:06o}\0 ").as_bytes());
        buf.extend_from_slice(&header);
        buf.extend_from_slice(data);
        let rem = data.len() % 512;
        if rem != 0 {
            buf.resize(buf.len() + (512 - rem), 0);
        }
    }

    fn build_tar_gz(primary_bytes: &[u8], alias_bytes: &[u8]) -> Vec<u8> {
        let mut tar_bytes = Vec::new();
        append_ustar_entry(&mut tar_bytes, "agy-pulse", 0o755, primary_bytes);
        append_ustar_entry(&mut tar_bytes, "agypl", 0o755, alias_bytes);
        tar_bytes.resize(tar_bytes.len() + 1024, 0);
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&tar_bytes).unwrap();
        encoder.finish().unwrap()
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hex::encode(hasher.finalize())
    }

    fn free_loopback_port() -> u16 {
        let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        l.local_addr().unwrap().port()
    }

    fn send_http(port: u16, method: &str, path: &str, headers: &[(&str, &str)]) -> std::io::Result<(u16, String)> {
        let mut stream = TcpStream::connect(("127.0.0.1", port))?;
        stream.set_read_timeout(Some(Duration::from_secs(3)))?;
        let mut req = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n");
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("\r\n");
        stream.write_all(req.as_bytes())?;
        stream.flush()?;
        let mut raw = String::new();
        stream.read_to_string(&mut raw)?;
        let status = raw
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(0);
        let body = raw.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
        Ok((status, body))
    }

    struct ChildGuard(Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn t10_release_packaging_install_live_workflow_update_and_uninstall_e2e() {
        let env = IsolatedEnv::new();
        let (primary_src, alias_src) = preferred_release_or_test_bins();
        let primary_bytes = fs::read(&primary_src).unwrap();
        let alias_bytes = fs::read(&alias_src).unwrap();

        let asset_name = agy_pulse::platform::current_release_asset_name()
            .expect("Unix test host must match a release archive asset");
        let archive = build_tar_gz(&primary_bytes, &alias_bytes);
        let sum_line = format!("{}  {asset_name}\n", sha256_hex(&archive));

        let archive_clone = archive.clone();
        let sum_clone = sum_line.clone();
        let server = LocalHttpServer::start(move |path| {
            if path == "/latest.json" {
                MockResponse {
                    status: 200,
                    headers: vec![("Content-Type".to_string(), "application/json".to_string())],
                    body: br#"{"tag_name":"v2.1.0"}"#.to_vec(),
                }
            } else if path == "/SHA256SUMS" {
                MockResponse {
                    status: 200,
                    headers: vec![("Content-Type".to_string(), "text/plain".to_string())],
                    body: sum_clone.as_bytes().to_vec(),
                }
            } else if path == format!("/{asset_name}") {
                MockResponse {
                    status: 200,
                    headers: vec![("Content-Type".to_string(), "application/octet-stream".to_string())],
                    body: archive_clone.clone(),
                }
            } else {
                MockResponse {
                    status: 404,
                    headers: Vec::new(),
                    body: b"not found".to_vec(),
                }
            }
        });

        // 1. Run bash scripts/install.sh without Node.js or agy in PATH (clean sandbox PATH)
        let install_script = repo_root().join("scripts/install.sh");
        let install_out = env
            .base_cmd(Path::new("bash"))
            .arg(&install_script)
            .env("AGY_PULSE_RELEASE_BASE_URL", &server.base_url)
            .env("PATH", "/usr/bin:/bin:/usr/local/bin")
            .output()
            .unwrap();
        assert_eq!(
            install_out.status.code(),
            Some(0),
            "install.sh failed:\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&install_out.stdout),
            String::from_utf8_lossy(&install_out.stderr)
        );

        let installed_primary = env.bin_dir.join("agy-pulse");
        let installed_alias = env.bin_dir.join("agypl");
        assert!(installed_primary.is_file());
        assert!(installed_alias.is_file());

        let settings_path = env.cli_data_dir.join("settings.json");
        let settings_json: Value =
            serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
        assert_eq!(settings_json["statusLine"]["type"], "command");
        assert_eq!(
            settings_json["statusLine"]["command"],
            installed_primary.to_string_lossy().as_ref()
        );

        // 2. Seed real conversation + run installed statusline binary
        let conv_id = "e2e-conv-1";
        let now_secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
        let tp = env
            .cli_data_dir
            .join(format!("brain/{conv_id}/.system_generated/logs/transcript.jsonl"));
        fs::create_dir_all(tp.parent().unwrap()).unwrap();
        fs::write(
            &tp,
            format!(
                "{}\n{}\n",
                serde_json::json!({ "step_index": 0, "type": "USER_INPUT", "created_at": epoch_secs_to_iso8601_utc(now_secs - 5), "content": "test" }),
                serde_json::json!({
                    "step_index": 1,
                    "type": "PLANNER_RESPONSE",
                    "created_at": epoch_secs_to_iso8601_utc(now_secs - 2),
                    "model": "claude-sonnet-4-6",
                    "input_tokens": 4_000,
                    "output_tokens": 500,
                    "cache_read_tokens": 6_000
                })
            ),
        )
        .unwrap();

        let conv_dir = env.cli_data_dir.join("conversations");
        fs::create_dir_all(&conv_dir).unwrap();
        {
            let conn = Connection::open(conv_dir.join(format!("{conv_id}.db"))).unwrap();
            conn.execute("CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB)", [])
                .unwrap();
            let blob = pb_gen_metadata_blob(&[GenEntrySpec {
                sys_tokens: 1_000,
                raw_in: 4_000,
                out: 500,
                cache_read: 6_000,
                display_name: Some("Claude Sonnet 4.6"),
                raw_model: Some("claude-sonnet-4-6"),
                kvs: &[("last_step_index", "1")],
                step_indices: &[1],
                ..Default::default()
            }]);
            conn.execute("INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)", params![1i64, blob])
                .unwrap();
        }

        let stdin_payload = serde_json::json!({
            "conversation_id": conv_id,
            "model": { "id": "claude-sonnet-4-6", "display_name": "Claude Sonnet 4.6" },
            "plan_tier": "Pro",
            "workspace": { "current_dir": env.workspace_dir },
            "context_window": {
                "context_window_size": 200_000,
                "total_input_tokens": 10_000,
                "total_output_tokens": 500
            }
        })
        .to_string();
        let (sl_code, sl_out, sl_err) = env.run_stdin(&installed_primary, &stdin_payload, &[]);
        assert_eq!(sl_code, 0, "statusline stderr: {sl_err}");
        let sl_plain = strip_ansi(&sl_out);
        assert!(sl_plain.contains("Claude Sonnet 4.6") && sl_plain.contains("Pro"), "got: {sl_plain}");
        let ledger_path = env.cli_data_dir.join("agy-pulse-ledger.sqlite");
        assert!(ledger_path.is_file(), "statusline must populate SQLite ledger");

        // 3. Verify `agypl stats --json`
        let stats_out = env
            .base_cmd(&installed_alias)
            .args(["stats", "all", "--json"])
            .output()
            .unwrap();
        assert_eq!(stats_out.status.code(), Some(0));
        let stats_json: Value = serde_json::from_slice(&stats_out.stdout).unwrap();
        assert!(
            stats_json["summary"]["totalSteps"].as_u64().unwrap_or(0) >= 1,
            "unexpected stats json: {stats_json}"
        );

        // 4. Verify `agypl config` in non-TTY exits safely without mutating config
        let cfg_out = env
            .base_cmd(&installed_alias)
            .arg("config")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(cfg_out.status.code(), Some(0));

        // 5. Verify `agypl dashboard` (/api/overview + keepalive + clean exit)
        let dash_port = free_loopback_port();
        let dash_child = env
            .base_cmd(&installed_alias)
            .env("AGY_PULSE_DASHBOARD_IDLE_SEC", "5")
            .env("AGY_PULSE_DASHBOARD_CLOSE_GRACE_MS", "200")
            .args([
                "dashboard",
                "--serve-background",
                "--no-open",
                "--port",
                &dash_port.to_string(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut dash_guard = ChildGuard(dash_child);

        let deadline = Instant::now() + Duration::from_secs(4);
        let mut ready = false;
        while Instant::now() < deadline {
            if let Ok((200, _)) = send_http(dash_port, "GET", "/api/health", &[]) {
                ready = true;
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        assert!(ready, "installed dashboard failed to start");

        let (ov_status, ov_body) = send_http(dash_port, "GET", "/api/overview?range=all", &[]).unwrap();
        assert_eq!(ov_status, 200);
        let ov_json: Value = serde_json::from_str(&ov_body).unwrap();
        assert!(
            ov_json["summary"]["totalSteps"].as_u64().unwrap_or(0) >= 1,
            "unexpected overview json: {ov_json}"
        );

        // Hold keepalive SSE stream, then disconnect and observe graceful auto-exit
        let mut sse = TcpStream::connect(("127.0.0.1", dash_port)).unwrap();
        sse.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        sse.write_all(
            format!(
                "GET /api/keepalive?client=e2e-tab HTTP/1.1\r\nHost: 127.0.0.1:{dash_port}\r\nAccept: text/event-stream\r\n\r\n"
            )
            .as_bytes(),
        )
        .unwrap();
        sse.flush().unwrap();
        let mut head_buf = [0u8; 256];
        let _ = sse.read(&mut head_buf);
        let (disc_status, _) = send_http(
            dash_port,
            "POST",
            "/api/disconnect?client=e2e-tab",
            &[("Origin", &format!("http://127.0.0.1:{dash_port}"))],
        )
        .unwrap();
        assert_eq!(disc_status, 200);
        drop(sse);

        let exit_deadline = Instant::now() + Duration::from_secs(3);
        let mut exited = false;
        while Instant::now() < exit_deadline {
            if matches!(dash_guard.0.try_wait(), Ok(Some(_))) {
                exited = true;
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        assert!(exited, "dashboard daemon must exit cleanly after page close grace");

        // 6. Verify `agypl update` against local release feed
        let update_out = env
            .base_cmd(&installed_alias)
            .arg("update")
            .env("AGY_PULSE_UPDATE_URL", format!("{}/latest.json", server.base_url))
            .env("AGY_PULSE_RELEASE_BASE_URL", &server.base_url)
            .output()
            .unwrap();
        assert_eq!(
            update_out.status.code(),
            Some(0),
            "update failed: {}",
            String::from_utf8_lossy(&update_out.stderr)
        );
        assert!(String::from_utf8_lossy(&update_out.stdout).contains("Updated agy-pulse"));

        // 7. Pre-seed plugin dir and run `bash uninstall.sh` -> removes binaries, settings, plugin dir, preserves ledger
        let plugin_dir = env.xdg_cfg_home.join("gemini/plugins/agy-pulse");
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::write(plugin_dir.join("plugin.json"), b"{}").unwrap();

        let uninstall_script = repo_root().join("uninstall.sh");
        let uninstall_out = env
            .base_cmd(Path::new("bash"))
            .arg(&uninstall_script)
            .env("PATH", "/usr/bin:/bin:/usr/local/bin")
            .output()
            .unwrap();
        assert_eq!(
            uninstall_out.status.code(),
            Some(0),
            "uninstall.sh failed:\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&uninstall_out.stdout),
            String::from_utf8_lossy(&uninstall_out.stderr)
        );

        assert!(!installed_primary.exists(), "agy-pulse binary must be removed");
        assert!(!installed_alias.exists(), "agypl binary must be removed");
        assert!(!plugin_dir.exists(), "plugin dir must be cleaned by uninstall.sh");
        let settings_after: Value =
            serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
        assert!(settings_after.get("statusLine").is_none(), "statusLine must be cleared");
        assert!(ledger_path.is_file(), "ledger SQLite must be preserved by default");
    }
}
