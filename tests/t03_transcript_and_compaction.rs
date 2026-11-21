#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use tempfile::TempDir;

use agy_pulse::config::{DisplayConfig, LanguageSetting, PulseConfig, QuotaStyle};
use agy_pulse::domain::{
    ChildModelSpeed, ModelIdentity, ModelTokenTotals, PulseSnapshot, TokenUsage, TurnPeakByMode,
    WorkspaceInfo, WorkspaceMetadata, calculate_turn_cache_metrics,
};
use agy_pulse::platform::TerminalCaps;
use agy_pulse::render::{render_statusline, strip_ansi};

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

fn now_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn epoch_ms_to_iso8601(epoch_ms: i64) -> String {
    let epoch_secs = epoch_ms.div_euclid(1000);
    let millis = epoch_ms.rem_euclid(1000);
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

    format!("{year:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

#[test]
fn t03_incremental_transcript_offset_cache_invalidation_and_pruning() {
    let env = IsolatedEnv::new();
    fs::write(
        env.global_cfg_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "language": "en",
            "display": {
                "showModel": true,
                "showContextMonitor": true,
                "showModelCache": true,
                "showTokenBar": true,
                "showContextBar": true,
                "quotaStyle": "none"
            }
        })
        .to_string(),
    )
    .unwrap();

    let transcript_path = env.workspace_dir.join("transcript.jsonl");
    let turn1 = serde_json::json!({
        "step_index": 1,
        "type": "PLANNER_RESPONSE",
        "created_at": "2026-11-20T10:00:00Z",
        "model": "gemini-4-carbon-high",
        "context_window": {
            "context_window_size": 500000,
            "current_usage": {
                "input_tokens": 10000,
                "output_tokens": 100,
                "cache_read_input_tokens": 4000,
                "cache_creation_input_tokens": 0
            }
        }
    });
    let line1 = format!("{turn1}\n");
    fs::write(&transcript_path, &line1).unwrap();

    let stdin_payload = serde_json::json!({
        "conversation_id": "conv-t03-inc",
        "transcript_path": transcript_path.to_string_lossy(),
        "model": { "id": "gemini-4-carbon-high", "display_name": "Gemini 4 Carbon (High)" }
    })
    .to_string();

    let (code1, out1, err1) = env.run_stdin(&agy_pulse_bin(), &stdin_payload, &[("COLUMNS", "120")]);
    assert_eq!(code1, 0);
    assert_eq!(err1, "");
    let plain1 = strip_ansi(&out1);
    assert!(plain1.contains("Out 100"), "turn 1 output missing:\n{plain1}");
    assert!(plain1.contains("Carbon 40%"), "turn 1 cache pct missing:\n{plain1}");

    let cache_dir = env.data_dir.join("transcript-cache");
    let cache_files: Vec<_> = fs::read_dir(&cache_dir)
        .expect("transcript-cache dir must exist")
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .collect();
    assert_eq!(cache_files.len(), 1, "expected 1 transcript cache file");
    let cached_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(cache_files[0].path()).unwrap()).unwrap();
    assert_eq!(cached_json["offset"].as_u64(), Some(line1.len() as u64));

    // Append partial line without trailing newline -> offset must stay at line1.len()
    let turn2 = serde_json::json!({
        "step_index": 2,
        "type": "PLANNER_RESPONSE",
        "created_at": "2026-11-20T10:01:00Z",
        "model": "gemini-4-carbon-high",
        "context_window": {
            "context_window_size": 500000,
            "current_usage": {
                "input_tokens": 20000,
                "output_tokens": 250,
                "cache_read_input_tokens": 15000,
                "cache_creation_input_tokens": 0
            }
        }
    })
    .to_string();
    let half = turn2.len() / 2;
    {
        let mut f = OpenOptions::new().append(true).open(&transcript_path).unwrap();
        f.write_all(&turn2.as_bytes()[..half]).unwrap();
    }
    let (_, out_partial, _) = env.run_stdin(&agy_pulse_bin(), &stdin_payload, &[("COLUMNS", "120")]);
    assert!(
        strip_ansi(&out_partial).contains("Out 100"),
        "partial line must not be parsed prematurely"
    );

    // Finish line 2 with trailing newline -> incremental scan updates totals and offset
    {
        let mut f = OpenOptions::new().append(true).open(&transcript_path).unwrap();
        f.write_all(format!("{}\n", &turn2[half..]).as_bytes()).unwrap();
    }
    let (_, out2, _) = env.run_stdin(&agy_pulse_bin(), &stdin_payload, &[("COLUMNS", "120")]);
    let plain2 = strip_ansi(&out2);
    assert!(plain2.contains("Out 350"), "turn 2 output must accumulate to 350:\n{plain2}");
    // (4000 + 15000) / (10000 + 20000) = 19000 / 30000 = 63.3% -> 63%
    assert!(plain2.contains("Carbon 63%"), "turn 2 cumulative cache must be 63%:\n{plain2}");

    // Same-inode in-place overwrite with larger content -> headHash mismatch invalidates cache and rescans from 0
    let overwritten = serde_json::json!({
        "step_index": 1,
        "type": "PLANNER_RESPONSE",
        "created_at": "2026-11-20T11:00:00Z",
        "model": "gemini-4-carbon-high",
        "note": "padding-to-preserve-inode-and-exceed-previous-file-size-completely-for-head-hash-check",
        "context_window": {
            "context_window_size": 500000,
            "current_usage": {
                "input_tokens": 8000,
                "output_tokens": 999,
                "cache_read_input_tokens": 4000,
                "cache_creation_input_tokens": 0
            }
        }
    });
    let overwritten_line = format!("{overwritten}\n");
    {
        let mut f = OpenOptions::new().write(true).open(&transcript_path).unwrap();
        f.seek(SeekFrom::Start(0)).unwrap();
        f.write_all(overwritten_line.as_bytes()).unwrap();
        f.set_len(overwritten_line.len() as u64).unwrap();
    }
    let (_, out_overwrite, _) = env.run_stdin(&agy_pulse_bin(), &stdin_payload, &[("COLUMNS", "120")]);
    let plain_overwrite = strip_ansi(&out_overwrite);
    assert!(
        plain_overwrite.contains("Out 999"),
        "headHash mismatch must rescan from 0 to Out 999:\n{plain_overwrite}"
    );
    assert!(
        plain_overwrite.contains("Carbon 50%"),
        "rescan must recalculate Carbon 50%:\n{plain_overwrite}"
    );

    // Truncate/rotate to smaller file -> offset > size triggers full rescan
    let rotated = serde_json::json!({
        "step_index": 1,
        "type": "PLANNER_RESPONSE",
        "model": "gemini-4-carbon-high",
        "context_window": {
            "context_window_size": 500000,
            "current_usage": {
                "input_tokens": 4000,
                "output_tokens": 77,
                "cache_read_input_tokens": 3000,
                "cache_creation_input_tokens": 0
            }
        }
    });
    fs::write(&transcript_path, format!("{rotated}\n")).unwrap();
    let (_, out_rot, _) = env.run_stdin(&agy_pulse_bin(), &stdin_payload, &[("COLUMNS", "120")]);
    assert!(
        strip_ansi(&out_rot).contains("Out 77"),
        "truncated/rotated file must rescan from 0:\n{}",
        strip_ansi(&out_rot)
    );

    // Prune transcript-cache directory to at most 32 files
    for i in 0..35 {
        let p = env.workspace_dir.join(format!("bulk-{i}.jsonl"));
        fs::write(&p, format!("{{\"step_index\":{}}}\n", i + 1)).unwrap();
        let payload = serde_json::json!({
            "transcript_path": p.to_string_lossy()
        })
        .to_string();
        env.run_stdin(&agy_pulse_bin(), &payload, &[]);
    }
    let final_count = fs::read_dir(&cache_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .count();
    assert!(
        final_count <= 32,
        "transcript-cache must be pruned to <= 32 files, got {final_count}"
    );
}

#[test]
fn t03_truncated_fields_backfills_from_transcript_full_jsonl() {
    let env = IsolatedEnv::new();
    fs::write(
        env.global_cfg_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "language": "en",
            "display": {
                "showSpeed": true,
                "showContextBar": true,
                "showContextMonitor": false,
                "showModelCache": false,
                "quotaStyle": "none"
            }
        })
        .to_string(),
    )
    .unwrap();

    let now = now_epoch_ms();
    let brain_dir = env.data_dir.join("brain");
    let main_id = "11111111-1111-1111-1111-111111111111";
    let child_id = "22222222-2222-2222-2222-222222222222";
    let main_logs = brain_dir.join(main_id).join(".system_generated").join("logs");
    let child_logs = brain_dir.join(child_id).join(".system_generated").join("logs");
    fs::create_dir_all(&main_logs).unwrap();
    fs::create_dir_all(&child_logs).unwrap();

    fs::write(
        child_logs.join("transcript.jsonl"),
        format!(
            "{}\n{}\n",
            serde_json::json!({
                "step_index": 1,
                "type": "PLANNER_RESPONSE",
                "created_at": epoch_ms_to_iso8601(now - 10_000),
                "model": "gemini-4-hydrogen-high",
                "output_tokens": 500
            }),
            serde_json::json!({
                "step_index": 2,
                "type": "GENERIC",
                "created_at": epoch_ms_to_iso8601(now - 8_000)
            })
        ),
    )
    .unwrap();

    let future_reset = epoch_ms_to_iso8601(now + 2 * 3_600_000);
    let main_transcript = main_logs.join("transcript.jsonl");
    let main_full = main_logs.join("transcript_full.jsonl");

    // Step 1 in transcript.jsonl has truncated content hiding swarm worker registration
    // Step 2 has truncated content hiding 429 RESOURCE_EXHAUSTED quotaResetTimeStamp
    fs::write(
        &main_transcript,
        format!(
            "{}\n{}\n",
            serde_json::json!({
                "step_index": 1,
                "type": "GENERIC",
                "created_at": epoch_ms_to_iso8601(now - 12_000),
                "content": "...truncated...",
                "truncated_fields": ["content"]
            }),
            serde_json::json!({
                "step_index": 2,
                "status": "ERROR",
                "created_at": epoch_ms_to_iso8601(now - 5_000),
                "content": "...error truncated...",
                "truncated_fields": ["content"]
            })
        ),
    )
    .unwrap();

    fs::write(
        &main_full,
        format!(
            "{}\n{}\n",
            serde_json::json!({
                "step_index": 1,
                "type": "GENERIC",
                "created_at": epoch_ms_to_iso8601(now - 12_000),
                "content": serde_json::json!({
                    "status": "added",
                    "conversation_id": child_id,
                    "name": "worker-hydro",
                    "model_display_name": "Gemini 4 Hydrogen (High)"
                }).to_string()
            }),
            serde_json::json!({
                "step_index": 2,
                "status": "ERROR",
                "created_at": epoch_ms_to_iso8601(now - 5_000),
                "content": "HTTP 429 RESOURCE_EXHAUSTED",
                "error": {
                    "code": 429,
                    "status": "RESOURCE_EXHAUSTED",
                    "details": [{ "quotaResetTimeStamp": future_reset }]
                }
            })
        ),
    )
    .unwrap();

    let stdin_payload = serde_json::json!({
        "conversation_id": main_id,
        "transcript_path": main_transcript.to_string_lossy(),
        "model": { "id": "gemini-4-carbon-high", "display_name": "Gemini 4 Carbon (High)" }
    })
    .to_string();

    let (code, stdout, stderr) = env.run_stdin(&agypl_bin(), &stdin_payload, &[("COLUMNS", "120")]);
    assert_eq!(code, 0);
    assert_eq!(stderr, "");
    let plain = strip_ansi(&stdout);
    assert!(
        plain.contains("Hydro: 250.0 tok/s"),
        "truncated GENERIC entry must backfill from transcript_full.jsonl to expose child speed:\n{plain}"
    );
    assert!(
        plain.contains("Image Quota Exhausted"),
        "truncated ERROR entry must backfill from transcript_full.jsonl:\n{plain}"
    );
}

#[test]
fn t03_turn_cache_hit_rate_and_output_speed_noise_and_spike_filtering() {
    let env = IsolatedEnv::new();
    fs::write(
        env.global_cfg_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "language": "en",
            "display": {
                "showModel": true,
                "showTokenBar": true,
                "showTurnCacheHitRate": true,
                "showContextBar": true,
                "showSpeed": true,
                "showContextMonitor": false,
                "showModelCache": false,
                "quotaStyle": "table"
            }
        })
        .to_string(),
    )
    .unwrap();

    let transcript_path = env.workspace_dir.join("speed-session.jsonl");
    fs::write(&transcript_path, "{}\n").unwrap();

    let frame = |total_out: u64, api_ms: u64, in_tok: u64, c_read: u64, c_write: u64| {
        serde_json::json!({
            "transcript_path": transcript_path.to_string_lossy(),
            "model": { "id": "claude-sonnet-4-6", "display_name": "Claude Sonnet 4.6 (Thinking)" },
            "cost": { "total_api_duration_ms": api_ms },
            "context_window": {
                "context_window_size": 1000000,
                "total_input_tokens": in_tok,
                "total_output_tokens": total_out,
                "used_percentage": 1.0,
                "current_usage": {
                    "input_tokens": in_tok,
                    "output_tokens": 100,
                    "cache_read_input_tokens": c_read,
                    "cache_creation_input_tokens": c_write
                }
            }
        })
        .to_string()
    };

    // Frame 1: baseline speed + 60.0% turn cache hit rate (3k / 5k)
    let (_, out1, _) = env.run_stdin(
        &agy_pulse_bin(),
        &frame(500, 10_000, 5000, 3000, 1000),
        &[("COLUMNS", "140")],
    );
    let plain1 = strip_ansi(&out1);
    assert!(
        plain1.contains("cache 60.0% (3k/5k)"),
        "frame 1 missing turn cache hit rate:\n{plain1}"
    );
    assert!(!plain1.contains("tok/s"), "baseline frame must not render speed:\n{plain1}");

    // Frame 2: valid 3000ms delta, +300 tokens -> 100.0 tok/s
    let (_, out2, _) = env.run_stdin(
        &agy_pulse_bin(),
        &frame(800, 13_000, 5000, 3000, 1000),
        &[("COLUMNS", "140")],
    );
    let plain2 = strip_ansi(&out2);
    assert!(
        plain2.contains("out: 100.0 tok/s"),
        "frame 2 must render 100.0 tok/s:\n{plain2}"
    );

    // Frame 3: < 500ms noise window (200ms) -> filtered out
    let (_, out3, _) = env.run_stdin(
        &agy_pulse_bin(),
        &frame(1000, 13_200, 5000, 3000, 1000),
        &[("COLUMNS", "140")],
    );
    let plain3 = strip_ansi(&out3);
    assert!(
        !plain3.contains("tok/s"),
        "frame 3 <500ms glitch must be filtered:\n{plain3}"
    );

    // Frame 4: > 5000 tok/s spike (9000 tokens / 1000ms = 9000 tok/s) -> filtered out
    let (_, out4, _) = env.run_stdin(
        &agy_pulse_bin(),
        &frame(10_000, 14_200, 5000, 3000, 1000),
        &[("COLUMNS", "140")],
    );
    let plain4 = strip_ansi(&out4);
    assert!(
        !plain4.contains("tok/s"),
        "frame 4 >5000 tok/s spike must be filtered:\n{plain4}"
    );

    // Frame 5: valid recovery (400 tokens / 2000ms = 200.0 tok/s)
    let (_, out5, _) = env.run_stdin(
        &agy_pulse_bin(),
        &frame(10_400, 16_200, 5000, 3000, 1000),
        &[("COLUMNS", "140")],
    );
    let plain5 = strip_ansi(&out5);
    assert!(
        plain5.contains("out: 200.0 tok/s"),
        "frame 5 must recover to 200.0 tok/s:\n{plain5}"
    );

    // Compact mode omits absolute ratio on cache badge
    fs::write(
        env.workspace_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "display": { "quotaStyle": "compact" }
        })
        .to_string(),
    )
    .unwrap();
    let (_, out_compact, _) = env.run_stdin(
        &agy_pulse_bin(),
        &frame(10_400, 16_200, 5000, 3000, 1000),
        &[("COLUMNS", "140")],
    );
    let plain_compact = strip_ansi(&out_compact);
    assert!(
        plain_compact.contains("cache 60.0%") && !plain_compact.contains("cache 60.0% ("),
        "compact mode must omit absolute counts in cache badge:\n{plain_compact}"
    );

    // Prune speed-cache directory to <= 32 files
    for i in 0..35 {
        let p = env.workspace_dir.join(format!("s-{i}.jsonl"));
        let f = serde_json::json!({
            "transcript_path": p.to_string_lossy(),
            "cost": { "total_api_duration_ms": 1000 },
            "context_window": { "total_output_tokens": 200 }
        })
        .to_string();
        env.run_stdin(&agy_pulse_bin(), &f, &[]);
    }
    let speed_cache_dir = env.data_dir.join("speed-cache");
    let speed_files = fs::read_dir(&speed_cache_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .count();
    assert!(
        speed_files <= 32,
        "speed-cache must be pruned to <= 32 files, got {speed_files}"
    );
}

#[test]
fn t03_compaction_checkpoint_resets_active_context_and_preserves_peak_and_count() {
    let env = IsolatedEnv::new();
    fs::write(
        env.global_cfg_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "language": "en",
            "display": {
                "showModel": true,
                "showContextBar": true,
                "showContextMonitor": true,
                "showModelCache": true,
                "quotaStyle": "none"
            }
        })
        .to_string(),
    )
    .unwrap();

    let transcript_path = env.workspace_dir.join("compaction.jsonl");
    let turn1 = serde_json::json!({
        "step_index": 1,
        "type": "PLANNER_RESPONSE",
        "created_at": "2026-11-20T10:00:00Z",
        "model": "gemini-4-carbon-high",
        "context_window": {
            "context_window_size": 512000,
            "total_input_tokens": 400000,
            "total_output_tokens": 5000,
            "current_usage": {
                "input_tokens": 400000,
                "output_tokens": 5000,
                "cache_read_input_tokens": 350000,
                "cache_creation_input_tokens": 10000
            }
        }
    });
    fs::write(&transcript_path, format!("{turn1}\n")).unwrap();

    let stdin_payload = serde_json::json!({
        "conversation_id": "conv-compact",
        "transcript_path": transcript_path.to_string_lossy(),
        "model": { "id": "gemini-4-carbon-high", "display_name": "Gemini 4 Carbon (High)" }
    })
    .to_string();

    let (_, out_before, _) = env.run_stdin(&agy_pulse_bin(), &stdin_payload, &[("COLUMNS", "120")]);
    let plain_before = strip_ansi(&out_before);
    assert!(
        plain_before.contains("405K/512K"),
        "before compaction must show 405K/512K:\n{plain_before}"
    );
    assert!(
        !plain_before.contains("Compacted"),
        "before compaction must not show Compacted:\n{plain_before}"
    );

    // Append CHECKPOINT compaction event
    let ckpt = serde_json::json!({
        "step_index": 2,
        "type": "CHECKPOINT",
        "created_at": "2026-11-20T10:02:00Z"
    });
    {
        let mut f = OpenOptions::new().append(true).open(&transcript_path).unwrap();
        writeln!(f, "{ckpt}").unwrap();
    }

    let (_, out_after_ckpt, _) = env.run_stdin(
        &agy_pulse_bin(),
        &stdin_payload,
        &[("COLUMNS", "120"), ("AGY_PULSE_FORCE_UNICODE", "1")],
    );
    let plain_after_ckpt = strip_ansi(&out_after_ckpt);
    assert!(
        plain_after_ckpt.contains("0/512k"),
        "context bar must reset to 0/512k after CHECKPOINT:\n{plain_after_ckpt}"
    );
    assert!(
        plain_after_ckpt.contains("0.0%"),
        "monitor must reset to 0.0% after CHECKPOINT:\n{plain_after_ckpt}"
    );
    assert!(
        plain_after_ckpt.contains("Compacted×1(peak 405K)"),
        "monitor must preserve peak 405K and show Compacted×1:\n{plain_after_ckpt}"
    );
    assert!(
        plain_after_ckpt.contains("Out 5K"),
        "cumulative output must stay 5K across compaction:\n{plain_after_ckpt}"
    );

    // Post-compaction turn at smaller active context
    let turn2 = serde_json::json!({
        "step_index": 3,
        "type": "PLANNER_RESPONSE",
        "created_at": "2026-11-20T10:03:00Z",
        "model": "gemini-4-carbon-high",
        "context_window": {
            "context_window_size": 512000,
            "current_usage": {
                "input_tokens": 30000,
                "output_tokens": 400,
                "cache_read_input_tokens": 20000,
                "cache_creation_input_tokens": 1000
            }
        }
    });
    {
        let mut f = OpenOptions::new().append(true).open(&transcript_path).unwrap();
        writeln!(f, "{turn2}").unwrap();
    }

    let (_, out_post_turn, _) = env.run_stdin(
        &agy_pulse_bin(),
        &stdin_payload,
        &[("COLUMNS", "120"), ("AGY_PULSE_FORCE_UNICODE", "1")],
    );
    let plain_post_turn = strip_ansi(&out_post_turn);
    assert!(
        plain_post_turn.contains("30.4K/512K"),
        "post-compaction active context must be 30.4K/512K:\n{plain_post_turn}"
    );
    assert!(
        plain_post_turn.contains("Compacted×1(peak 405K)"),
        "historical peak 405K must remain preserved:\n{plain_post_turn}"
    );
    assert!(
        plain_post_turn.contains("Out 5.4K"),
        "cumulative output must advance to 5.4K:\n{plain_post_turn}"
    );
}

#[test]
fn t03_child_and_swarm_active_model_speeds_capped_at_three_and_expire_on_idle() {
    let env = IsolatedEnv::new();
    fs::write(
        env.global_cfg_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "language": "en",
            "display": {
                "showSpeed": true,
                "showContextMonitor": false,
                "showModelCache": false,
                "quotaStyle": "none"
            }
        })
        .to_string(),
    )
    .unwrap();

    let now = now_epoch_ms();
    let brain_dir = env.data_dir.join("brain");
    let main_id = "aaaa0000-0000-0000-0000-000000000001";
    let w_hydro_1 = "bbbb0000-0000-0000-0000-000000000001";
    let w_hydro_2 = "bbbb0000-0000-0000-0000-000000000002";
    let sub_flash = "cccc0000-0000-0000-0000-000000000001";
    let w_argon = "dddd0000-0000-0000-0000-000000000001";
    let w_neon = "eeee0000-0000-0000-0000-000000000001";

    let log_path = |id: &str| brain_dir.join(id).join(".system_generated").join("logs").join("transcript.jsonl");
    for id in [main_id, w_hydro_1, w_hydro_2, sub_flash, w_argon, w_neon] {
        fs::create_dir_all(log_path(id).parent().unwrap()).unwrap();
    }

    // Write 4 distinct child models (Hydro oldest, Flash, Argon, Neon newest) + duplicate Hydro
    fs::write(
        log_path(w_hydro_1),
        format!(
            "{}\n{}\n",
            serde_json::json!({ "step_index": 1, "type": "PLANNER_RESPONSE", "created_at": epoch_ms_to_iso8601(now - 50_000), "model": "gemini-4-hydrogen-high", "output_tokens": 200 }),
            serde_json::json!({ "step_index": 2, "type": "GENERIC", "created_at": epoch_ms_to_iso8601(now - 48_000) })
        ),
    )
    .unwrap();
    fs::write(
        log_path(w_hydro_2),
        format!(
            "{}\n{}\n",
            serde_json::json!({ "step_index": 1, "type": "PLANNER_RESPONSE", "created_at": epoch_ms_to_iso8601(now - 42_000), "model": "gemini-4-hydrogen-high", "output_tokens": 360 }),
            serde_json::json!({ "step_index": 2, "type": "GENERIC", "created_at": epoch_ms_to_iso8601(now - 40_000) })
        ),
    )
    .unwrap();
    fs::write(
        log_path(sub_flash),
        format!(
            "{}\n{}\n",
            serde_json::json!({ "step_index": 1, "type": "PLANNER_RESPONSE", "created_at": epoch_ms_to_iso8601(now - 32_000), "output_tokens": 600 }),
            serde_json::json!({ "step_index": 2, "type": "GENERIC", "created_at": epoch_ms_to_iso8601(now - 30_000) })
        ),
    )
    .unwrap();
    fs::write(
        log_path(w_argon),
        format!(
            "{}\n{}\n",
            serde_json::json!({ "step_index": 1, "type": "PLANNER_RESPONSE", "created_at": epoch_ms_to_iso8601(now - 22_000), "model": "gemini-4-argon-high", "output_tokens": 440 }),
            serde_json::json!({ "step_index": 2, "type": "GENERIC", "created_at": epoch_ms_to_iso8601(now - 20_000) })
        ),
    )
    .unwrap();
    fs::write(
        log_path(w_neon),
        format!(
            "{}\n{}\n",
            serde_json::json!({ "step_index": 1, "type": "PLANNER_RESPONSE", "created_at": epoch_ms_to_iso8601(now - 12_000), "model": "gemini-4-neon-high", "output_tokens": 500 }),
            serde_json::json!({ "step_index": 2, "type": "GENERIC", "created_at": epoch_ms_to_iso8601(now - 10_000) })
        ),
    )
    .unwrap();

    let main_transcript = log_path(main_id);
    fs::write(
        &main_transcript,
        format!(
            "{}\n{}\n{}\n{}\n",
            serde_json::json!({ "step_index": 1, "type": "USER_INPUT", "created_at": epoch_ms_to_iso8601(now - 60_000) }),
            serde_json::json!({
                "step_index": 2,
                "type": "GENERIC",
                "created_at": epoch_ms_to_iso8601(now - 58_000),
                "content": serde_json::json!({
                    "status": "added",
                    "members": [
                        { "conversation_id": w_hydro_1, "name": "w-h1", "model_display_name": "Gemini 4 Hydrogen (High)" },
                        { "conversation_id": w_hydro_2, "name": "w-h2", "model_display_name": "Gemini 4 Hydrogen (High)" },
                        { "conversation_id": w_argon, "name": "w-argon", "model_display_name": "Gemini 4 Argon (High)" },
                        { "conversation_id": w_neon, "name": "w-neon", "model_display_name": "Gemini 4 Neon (High)" }
                    ]
                }).to_string()
            }),
            serde_json::json!({
                "step_index": 3,
                "type": "PLANNER_RESPONSE",
                "created_at": epoch_ms_to_iso8601(now - 55_000),
                "tool_calls": [{ "name": "invoke_subagent", "arguments": { "Model": "flash", "Role": "finder" } }]
            }),
            serde_json::json!({
                "step_index": 4,
                "type": "GENERIC",
                "created_at": epoch_ms_to_iso8601(now - 54_000),
                "content": serde_json::json!({ "conversationId": sub_flash }).to_string()
            })
        ),
    )
    .unwrap();

    let stdin_payload = serde_json::json!({
        "conversation_id": main_id,
        "transcript_path": main_transcript.to_string_lossy(),
        "model": { "id": "gemini-4-carbon-high", "display_name": "Gemini 4 Carbon (High)" }
    })
    .to_string();

    let (_, out1, _) = env.run_stdin(
        &agy_pulse_bin(),
        &stdin_payload,
        &[("COLUMNS", "200"), ("AGY_PULSE_FORCE_UNICODE", "1")],
    );
    let line1 = strip_ansi(&out1).lines().next().unwrap_or("").to_string();
    // Top 3 most recent distinct models: Flash (300.0), Argon (220.0), Neon (250.0); oldest Hydro is capped out!
    assert!(line1.contains("⚡ "), "missing ⚡ child speed prefix:\n{line1}");
    assert!(line1.contains("Flash: 300.0 tok/s"), "missing Flash speed:\n{line1}");
    assert!(line1.contains("Argon: 220.0 tok/s"), "missing Argon speed:\n{line1}");
    assert!(line1.contains("Neon: 250.0 tok/s"), "missing Neon speed:\n{line1}");
    assert!(!line1.contains("Hydro:"), "oldest 4th model Hydro must be capped out:\n{line1}");

    // Advance 31 USER_INPUT turns, refreshing only Argon via SYSTEM_MESSAGE sender -> Flash & Neon expire (>30 turns), Argon stays, Hydro also >30 turns
    {
        let mut f = OpenOptions::new().append(true).open(&main_transcript).unwrap();
        let mut step = 5;
        for t in 2..=32 {
            writeln!(
                f,
                "{}",
                serde_json::json!({
                    "step_index": step,
                    "type": "USER_INPUT",
                    "created_at": epoch_ms_to_iso8601(now - 5_000 + t * 50)
                })
            )
            .unwrap();
            step += 1;
            if t == 25 {
                writeln!(
                    f,
                    "{}",
                    serde_json::json!({
                        "step_index": step,
                        "type": "SYSTEM_MESSAGE",
                        "created_at": epoch_ms_to_iso8601(now - 5_000 + t * 50 + 10),
                        "content": format!("[Message] sender={w_argon} content=ping")
                    })
                )
                .unwrap();
                step += 1;
            }
        }
    }

    let (_, out2, _) = env.run_stdin(&agy_pulse_bin(), &stdin_payload, &[("COLUMNS", "200")]);
    let line2 = strip_ansi(&out2).lines().next().unwrap_or("").to_string();
    assert!(line2.contains("Argon: 220.0 tok/s"), "refreshed Argon must stay visible:\n{line2}");
    assert!(!line2.contains("Flash:"), "stale Flash (>30 turns) must expire:\n{line2}");
    assert!(!line2.contains("Neon:"), "stale Neon (>30 turns) must expire:\n{line2}");

    // Time-based expiry (> 5 minutes): use a separate stale transcript with timestamps > 6 minutes ago
    let stale_main_id = "ffff0000-0000-0000-0000-000000000001";
    let stale_child_id = "ffff0000-0000-0000-0000-000000000002";
    fs::create_dir_all(log_path(stale_main_id).parent().unwrap()).unwrap();
    fs::create_dir_all(log_path(stale_child_id).parent().unwrap()).unwrap();
    fs::write(
        log_path(stale_child_id),
        format!(
            "{}\n{}\n",
            serde_json::json!({ "step_index": 1, "type": "PLANNER_RESPONSE", "created_at": epoch_ms_to_iso8601(now - 420_000), "model": "gemini-4-argon-high", "output_tokens": 400 }),
            serde_json::json!({ "step_index": 2, "type": "GENERIC", "created_at": epoch_ms_to_iso8601(now - 418_000) })
        ),
    )
    .unwrap();
    fs::write(
        log_path(stale_main_id),
        format!(
            "{}\n{}\n",
            serde_json::json!({ "step_index": 1, "type": "USER_INPUT", "created_at": epoch_ms_to_iso8601(now - 425_000) }),
            serde_json::json!({
                "step_index": 2,
                "type": "GENERIC",
                "created_at": epoch_ms_to_iso8601(now - 422_000),
                "content": serde_json::json!({
                    "status": "added",
                    "conversation_id": stale_child_id,
                    "name": "w-stale",
                    "model_display_name": "Gemini 4 Argon (High)"
                }).to_string()
            })
        ),
    )
    .unwrap();

    let stale_payload = serde_json::json!({
        "conversation_id": stale_main_id,
        "transcript_path": log_path(stale_main_id).to_string_lossy(),
        "model": { "id": "gemini-4-carbon-high", "display_name": "Gemini 4 Carbon (High)" }
    })
    .to_string();
    let (_, out_stale, _) = env.run_stdin(&agy_pulse_bin(), &stale_payload, &[("COLUMNS", "200")]);
    let line_stale = strip_ansi(&out_stale).lines().next().unwrap_or("").to_string();
    assert!(
        !line_stale.contains("Argon:"),
        "child idle >5 minutes must be hidden:\n{line_stale}"
    );
}

#[test]
fn t03_workspace_rules_memory_hooks_and_mcp_offline_scan_and_render() {
    let env = IsolatedEnv::new();
    let proj = env.workspace_dir.join("meta-proj");
    fs::create_dir_all(&proj).unwrap();

    // Memory files: AGENTS.md takes priority over CLAUDE.md
    fs::write(proj.join("AGENTS.md"), "# Agents\n").unwrap();
    fs::write(proj.join("CLAUDE.md"), "# Claude\n").unwrap();

    // Rules across .claude, .cursor, .github, .gemini, .antigravity
    for (dir, file) in [
        (".claude/rules", "r1.md"),
        (".cursor/rules", "r2.md"),
        (".github/rules", "r3.md"),
        (".gemini/rules", "r4.md"),
        (".antigravity/rules", "r5.md"),
    ] {
        let p = proj.join(dir);
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join(file), "# rule\n").unwrap();
    }
    // Non-.md file must be ignored
    fs::write(proj.join(".antigravity/rules/ignore.txt"), "not a rule").unwrap();

    // Git hooks (1 active, 1 .sample ignored)
    let hooks_dir = proj.join(".git/hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
    fs::write(proj.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    fs::write(hooks_dir.join("pre-commit"), "#!/bin/sh\n").unwrap();
    fs::write(hooks_dir.join("pre-push.sample"), "#!/bin/sh\n").unwrap();

    // MCP servers across settings.json, mcp_config.json, and claude_desktop_config.json
    fs::write(
        env.data_dir.join("settings.json"),
        serde_json::json!({
            "mcpServers": {
                "github": {},
                "filesystem": {}
            }
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        env.data_dir.join("mcp_config.json"),
        serde_json::json!({
            "mcpServers": {
                "postgres": {}
            }
        })
        .to_string(),
    )
    .unwrap();
    #[cfg(target_os = "macos")]
    let claude_config_path = env
        .home_dir
        .join("Library/Application Support/Claude/claude_desktop_config.json");
    #[cfg(windows)]
    let claude_config_path = env.app_data_dir.join("Claude/claude_desktop_config.json");
    #[cfg(all(unix, not(target_os = "macos")))]
    let claude_config_path = env.xdg_cfg_dir.join("Claude/claude_desktop_config.json");
    if let Some(claude_dir) = claude_config_path.parent() {
        fs::create_dir_all(claude_dir).unwrap();
    }
    fs::write(
        &claude_config_path,
        serde_json::json!({
            "mcpServers": {
                "memory": {}
            }
        })
        .to_string(),
    )
    .unwrap();

    fs::write(
        env.global_cfg_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "language": "en",
            "display": {
                "showBreadcrumbs": true,
                "showContextMonitor": false,
                "showModelCache": false,
                "quotaStyle": "none"
            }
        })
        .to_string(),
    )
    .unwrap();

    let payload = serde_json::json!({
        "workspace": { "current_dir": proj.to_string_lossy() }
    })
    .to_string();

    let (code, stdout, stderr) = env.run_stdin(&agy_pulse_bin(), &payload, &[("COLUMNS", "120")]);
    assert_eq!(code, 0);
    assert_eq!(stderr, "");
    let plain = strip_ansi(&stdout);
    assert!(plain.contains("1 AGENTS.md"), "missing AGENTS.md breadcrumb:\n{plain}");
    assert!(plain.contains("5 rules"), "expected 5 rules:\n{plain}");
    assert!(plain.contains("4 MCPs"), "expected 4 MCPs:\n{plain}");
    assert!(plain.contains("1 hooks"), "expected 1 hooks:\n{plain}");

    // With showBreadcrumbs=false, 1 AGENTS.md is hidden while 5 rules / 4 MCPs / 1 hooks stay visible
    fs::write(
        proj.join("agy-pulse.config.json"),
        serde_json::json!({
            "display": { "showBreadcrumbs": false }
        })
        .to_string(),
    )
    .unwrap();
    let (_, stdout2, _) = env.run_stdin(&agy_pulse_bin(), &payload, &[("COLUMNS", "120")]);
    let plain2 = strip_ansi(&stdout2);
    assert!(!plain2.contains("AGENTS.md"), "breadcrumbs disabled must hide AGENTS.md:\n{plain2}");
    assert!(plain2.contains("5 rules"), "rules must still show:\n{plain2}");
    assert!(plain2.contains("4 MCPs"), "MCPs must still show:\n{plain2}");
    assert!(plain2.contains("1 hooks"), "hooks must still show:\n{plain2}");
}

#[test]
fn t03_pure_domain_and_render_turn_cache_child_speeds_compaction_and_metadata() {
    let usage = TokenUsage {
        input_tokens: 5000,
        output_tokens: 100,
        cache_read_input_tokens: 3000,
        cache_creation_input_tokens: 1000,
        cache_exclusive: false,
    };
    let inclusive_metrics = calculate_turn_cache_metrics(&usage, "Claude Sonnet 4.6 (Thinking)");
    assert!(inclusive_metrics.available);
    assert_eq!(inclusive_metrics.cache_read, 3000);
    assert_eq!(inclusive_metrics.total_prompt, 5000);
    assert!((inclusive_metrics.hit_rate - 60.0).abs() < 1e-9);

    let exclusive_metrics = calculate_turn_cache_metrics(&usage, "CustomExclusiveModel");
    assert!(exclusive_metrics.available);
    assert_eq!(exclusive_metrics.total_prompt, 9000);
    assert!((exclusive_metrics.hit_rate - (3000.0 * 100.0 / 9000.0)).abs() < 1e-9);

    let now_ms = 1_800_000_000_000_i64;
    let snapshot = PulseSnapshot {
        has_conversation_identity: true,
        observed_at_ms: now_ms,
        model: Some(ModelIdentity {
            id: "gemini-4-carbon-high".to_string(),
            display_name: "Gemini 4 Carbon (High)".to_string(),
            effort: None,
        }),
        workspace: WorkspaceInfo {
            current_dir: "demo-repo".to_string(),
            git_branch: Some("main".to_string()),
        },
        workspace_meta: WorkspaceMetadata {
            memory_file: Some("AGENTS.md".to_string()),
            breadcrumbs: Vec::new(),
            rules_count: 3,
            mcp_count: 2,
            hooks_count: 1,
        },
        compaction_count: 1,
        server_compaction_limit: Some(512_000),
        turn_history_max_by_mode: TurnPeakByMode {
            inclusive_peak: 420_000,
            exclusive_peak: 780_000,
        },
        output_speed_tok_s: Some(112.5),
        main_model_last_active_ms: Some(now_ms - 5_000),
        child_model_speeds: vec![
            ChildModelSpeed {
                model: "Hydro".to_string(),
                speed_tok_s: 240.0,
                ttft_sec: None,
                turns_ago: 1,
                last_active_ms: now_ms - 10_000,
            },
            ChildModelSpeed {
                model: "Flash".to_string(),
                speed_tok_s: 310.5,
                ttft_sec: None,
                turns_ago: 35, // > 30 idle turns -> filtered out by render
                last_active_ms: now_ms - 10_000,
            },
        ],
        model_token_totals: vec![
            ModelTokenTotals {
                model_key: "Carbon".to_string(),
                input_tokens: 100_000,
                cache_read_tokens: 400_000,
                cache_write_tokens: 0,
                output_tokens: 1_000,
                steps: 2,
            },
            ModelTokenTotals {
                model_key: "Hydro".to_string(),
                input_tokens: 50_000,
                cache_read_tokens: 450_000,
                cache_write_tokens: 0,
                output_tokens: 2_000,
                steps: 3,
            },
        ],
        ..Default::default()
    };

    let cfg = PulseConfig {
        language: LanguageSetting::Zh,
        display: DisplayConfig {
            show_model: Some(true),
            show_speed: Some(true),
            show_breadcrumbs: Some(true),
            show_context_monitor: Some(true),
            show_model_cache: Some(true),
            quota_style: QuotaStyle::None,
            ..DisplayConfig::builtin_defaults()
        },
        ..Default::default()
    };

    let rendered = render_statusline(
        &snapshot,
        &cfg,
        &TerminalCaps {
            unicode: true,
            columns: Some(140),
        },
        LanguageSetting::Zh,
    );
    let plain = strip_ansi(&rendered);
    assert!(plain.contains("输出: 112.5 tok/s"), "missing main speed:\n{plain}");
    assert!(plain.contains("⚡ Hydro: 240.0 tok/s"), "missing active child speed:\n{plain}");
    assert!(!plain.contains("Flash:"), "expired child (>30 turns) must be filtered:\n{plain}");
    assert!(plain.contains("1 AGENTS.md │ 3 rules │ 2 MCPs │ 1 hooks"), "missing metadata line:\n{plain}");
    assert!(
        plain.contains("⟳ 累计缓存 · Carbon 80% (命中 400K / 总入 500K) · Hydro 90% (命中 450K / 总入 500K)"),
        "missing multi-model cache line:\n{plain}"
    );
    assert!(plain.contains("压缩×1(峰420K)"), "missing zh compaction peak:\n{plain}");
}
