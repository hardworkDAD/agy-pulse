#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rusqlite::{Connection, params};
use tempfile::TempDir;

use agy_pulse::domain::{
    CostSnapshot, ModelIdentity, PulseSnapshot, TokenUsage, calculate_token_cost,
    resolve_cascade_cost_usd,
};
use agy_pulse::ingest::HostReader;
use agy_pulse::render::strip_ansi;

fn agy_pulse_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agy-pulse"))
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

    fn run_stdin(&self, stdin_text: &str, extra_env: &[(&str, &str)]) -> (i32, String, String) {
        let mut cmd = Command::new(agy_pulse_bin());
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

// --- Minimal Protobuf wire encoders for building real SQLite test fixtures ---

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

fn build_compaction_gen_metadata_blob(max_context_tokens: u64) -> Vec<u8> {
    // f1 -> f9 -> f10 -> f4
    let f10 = pb_varint(4, max_context_tokens);
    let f9 = pb_bytes(10, &f10);
    let f1 = pb_bytes(9, &f9);
    pb_bytes(1, &f1)
}

fn build_compaction_executor_metadata_blob(max_context_tokens: u64) -> Vec<u8> {
    // f10 -> f2 -> f5
    let f2 = pb_varint(5, max_context_tokens);
    let f10 = pb_bytes(2, &f2);
    pb_bytes(10, &f10)
}

#[derive(Default)]
struct GenEntrySpec<'a> {
    system_prompt_tokens: u64,
    raw_input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    ttft_sec: Option<(u64, u64)>,
    stream_sec: Option<(u64, u64)>,
    display_name: Option<&'a str>,
    raw_model: Option<&'a str>,
    kvs: &'a [(&'a str, &'a str)],
    step_indices: &'a [u64],
}

fn build_gen_metadata_blob(entries: &[GenEntrySpec<'_>]) -> Vec<u8> {
    let mut out = Vec::new();
    for spec in entries {
        for &idx in spec.step_indices {
            out.extend(pb_varint(2, idx));
        }
        let mut f4 = Vec::new();
        if spec.system_prompt_tokens > 0 {
            f4.extend(pb_varint(1, spec.system_prompt_tokens));
        }
        if spec.raw_input_tokens > 0 {
            f4.extend(pb_varint(2, spec.raw_input_tokens));
        }
        if spec.output_tokens > 0 {
            f4.extend(pb_varint(3, spec.output_tokens));
        }
        if spec.cache_read_tokens > 0 {
            f4.extend(pb_varint(5, spec.cache_read_tokens));
        }

        let mut f1 = Vec::new();
        f1.extend(pb_bytes(4, &f4));
        if let Some((sec, nanos)) = spec.ttft_sec {
            f1.extend(pb_duration(11, sec, nanos));
        }
        if let Some((sec, nanos)) = spec.stream_sec {
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

fn snapshot_dir_bytes(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut map = BTreeMap::new();
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        let bytes = fs::read(entry.path()).unwrap();
        map.insert(name, bytes);
    }
    map
}

#[test]
fn t04_lockfree_db_zero_side_effects_with_active_wal_and_immutable() {
    let env = IsolatedEnv::new();
    fs::write(
        env.global_cfg_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "language": "en",
            "display": {
                "showContextMonitor": true,
                "quotaStyle": "none"
            }
        })
        .to_string(),
    )
    .unwrap();

    let conv_dir = env.data_dir.join("conversations");
    fs::create_dir_all(&conv_dir).unwrap();

    // Case A: Static archive .db (no -wal/-shm) -> opened via immutable=1, zero sidecar creation
    let static_id = "conv-static-01";
    let static_db_path = conv_dir.join(format!("{static_id}.db"));
    {
        let conn = Connection::open(&static_db_path).unwrap();
        conn.execute("CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)",
            params![1i64, build_compaction_gen_metadata_blob(400_000)],
        )
        .unwrap();
    }

    let before_static = snapshot_dir_bytes(&conv_dir);
    assert_eq!(before_static.len(), 1);

    let stdin_static = serde_json::json!({
        "conversation_id": static_id,
        "model": { "id": "gemini-4-carbon-high", "display_name": "Gemini 4 Carbon (High)" },
        "context_window": {
            "context_window_size": 1_000_000,
            "current_usage": { "input_tokens": 200_000, "output_tokens": 0 }
        }
    })
    .to_string();
    let (code_a, out_a, err_a) = env.run_stdin(&stdin_static, &[("COLUMNS", "120")]);
    assert_eq!(code_a, 0);
    assert_eq!(err_a, "");
    let plain_a = strip_ansi(&out_a);
    assert!(
        plain_a.contains("200K/400K") && plain_a.contains("50.0%"),
        "static immutable DB compaction limit not applied:\n{plain_a}"
    );
    let after_static = snapshot_dir_bytes(&conv_dir);
    assert_eq!(
        before_static, after_static,
        "conversations/ must have zero new files or modified bytes in immutable mode"
    );

    // Case B: Active non-empty -wal WITH NO -shm file present (worst-case lock/shm hazard)
    let wal_id = "conv-active-wal-02";
    let wal_db_path = conv_dir.join(format!("{wal_id}.db"));
    {
        let conn = Connection::open(&wal_db_path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute(
            "CREATE TABLE executor_metadata (idx INTEGER PRIMARY KEY, data BLOB)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO executor_metadata (idx, data) VALUES (?1, ?2)",
            params![1i64, build_compaction_executor_metadata_blob(800_000)],
        )
        .unwrap();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
    }

    // Hold a live WAL writer transaction/uncheckpointed frame so -wal is non-empty!
    let writer_conn = Connection::open(&wal_db_path).unwrap();
    writer_conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    writer_conn
        .execute(
            "INSERT INTO executor_metadata (idx, data) VALUES (?1, ?2)",
            params![2i64, build_compaction_executor_metadata_blob(320_000)],
        )
        .unwrap();

    let wal_sidecar = PathBuf::from(format!("{}-wal", wal_db_path.display()));
    assert!(
        fs::metadata(&wal_sidecar).unwrap().len() > 0,
        "-wal sidecar must be non-empty"
    );
    // Delete -shm while keeping writer connection open on Unix; on Windows SQLite mmap locks -shm
    let shm_sidecar = PathBuf::from(format!("{}-shm", wal_db_path.display()));
    #[cfg(unix)]
    {
        let _ = fs::remove_file(&shm_sidecar);
        assert!(!shm_sidecar.exists(), "-shm removed before run");
    }
    #[cfg(windows)]
    let (wal_len_before, shm_existed_before) = (
        fs::metadata(&wal_sidecar).unwrap().len(),
        shm_sidecar.exists(),
    );

    let before_wal = snapshot_dir_bytes(&conv_dir);
    let stdin_wal = serde_json::json!({
        "conversation_id": wal_id,
        "model": { "id": "gemini-4-carbon-high", "display_name": "Gemini 4 Carbon (High)" },
        "context_window": {
            "context_window_size": 1_000_000,
            "current_usage": { "input_tokens": 160_000, "output_tokens": 0 }
        }
    })
    .to_string();
    let (code_b, out_b, err_b) = env.run_stdin(&stdin_wal, &[("COLUMNS", "120")]);
    assert_eq!(code_b, 0);
    assert_eq!(err_b, "");
    let plain_b = strip_ansi(&out_b);
    assert!(
        plain_b.contains("160K/320K") && plain_b.contains("50.0%"),
        "active WAL uncommitted frame (320k) must be read via TempDir copy:\n{plain_b}"
    );
    let after_wal = snapshot_dir_bytes(&conv_dir);
    assert_eq!(
        before_wal, after_wal,
        "conversations/ must have zero -shm creation and zero byte modification during active WAL read"
    );
    #[cfg(windows)]
    {
        assert_eq!(
            fs::metadata(&wal_sidecar).unwrap().len(),
            wal_len_before,
            "-wal size must remain unchanged during active WAL read"
        );
        assert_eq!(
            shm_sidecar.exists(),
            shm_existed_before,
            "-shm existence must remain unchanged during active WAL read"
        );
    }
    drop(writer_conn);
}

#[test]
fn t04_server_compaction_limit_calibrates_context_bar_and_monitor() {
    let env = IsolatedEnv::new();
    fs::write(
        env.global_cfg_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "language": "en",
            "display": {
                "showContextMonitor": true,
                "quotaStyle": "none"
            }
        })
        .to_string(),
    )
    .unwrap();

    let conv_dir = env.data_dir.join("conversations");
    fs::create_dir_all(&conv_dir).unwrap();
    let conv_id = "conv-limit-500k";
    let db_path = conv_dir.join(format!("{conv_id}.db"));
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute("CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)",
            params![1i64, build_compaction_gen_metadata_blob(500_000)],
        )
        .unwrap();
    }

    // Physical window 1,000,000, server compaction threshold 500,000, used 250,000 -> 50% (250k/500k)
    let payload = serde_json::json!({
        "conversation_id": conv_id,
        "model": { "id": "gemini-4-carbon-high", "display_name": "Gemini 4 Carbon (High)" },
        "context_window": {
            "context_window_size": 1_000_000,
            "used_percentage": 25.0,
            "total_input_tokens": 250_000
        }
    })
    .to_string();

    let (code, out, err) = env.run_stdin(&payload, &[("COLUMNS", "120")]);
    assert_eq!(code, 0);
    assert_eq!(err, "");
    let plain = strip_ansi(&out);
    assert!(
        plain.contains("50.0% (250K/500K)") && !plain.contains("25.0%"),
        "Context monitor must calibrate to 50.0% (250K/500K), not 25.0%:\n{plain}"
    );

    let cache_path = env.data_dir.join("agy-pulse-compaction-cache.json");
    assert!(cache_path.is_file(), "compaction cache file must be persisted");
    let cache_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&cache_path).unwrap()).unwrap();
    assert_eq!(
        cache_json["_last_known_limit"].as_u64(),
        Some(500_000),
        "cache must store _last_known_limit=500000"
    );

    // Second run with DB removed should hit _last_known_limit cache and still render 50.0% (250K/500K)
    fs::remove_file(&db_path).unwrap();
    let (code2, out2, _) = env.run_stdin(&payload, &[("COLUMNS", "120")]);
    assert_eq!(code2, 0);
    let plain2 = strip_ansi(&out2);
    assert!(
        plain2.contains("50.0% (250K/500K)"),
        "cached _last_known_limit must calibrate context monitor after DB removal:\n{plain2}"
    );
}

#[test]
fn t04_recent_dbs_weighted_streaming_speed_and_ttft_and_cache() {
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

    let conv_dir = env.data_dir.join("conversations");
    fs::create_dir_all(&conv_dir).unwrap();

    // Empty DB (must be skipped so it doesn't crowd out the 30 non-empty DB budget)
    {
        let conn = Connection::open(conv_dir.join("empty-00.db")).unwrap();
        conn.execute("CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB)", [])
            .unwrap();
    }

    // DB 1: Carbon step1 (out=200, stream=2s, ttft=1.0s) + noisy invalid step (out=500, stream=0.01s < 0.05s)
    //       + Hydro step (out=400, stream=2s, ttft=0.5s)
    {
        let conn = Connection::open(conv_dir.join("perf-db-1.db")).unwrap();
        conn.execute("CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB)", [])
            .unwrap();
        let blob = build_gen_metadata_blob(&[
            GenEntrySpec {
                raw_input_tokens: 1000,
                output_tokens: 200,
                ttft_sec: Some((1, 0)),
                stream_sec: Some((2, 0)),
                display_name: Some("Gemini 4 Carbon (High)"),
                ..Default::default()
            },
            GenEntrySpec {
                raw_input_tokens: 1000,
                output_tokens: 500,
                stream_sec: Some((0, 10_000_000)), // 0.01s < MIN_VALID_STREAM_SEC (0.05s) -> filtered
                display_name: Some("Gemini 4 Carbon (High)"),
                ..Default::default()
            },
            GenEntrySpec {
                raw_input_tokens: 1000,
                output_tokens: 400,
                ttft_sec: Some((0, 500_000_000)),
                stream_sec: Some((2, 0)),
                display_name: Some("Gemini 4 Hydrogen (High)"),
                ..Default::default()
            },
        ]);
        conn.execute("INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)", params![1i64, blob])
            .unwrap();
    }

    // DB 2: Carbon step2 (out=600, stream=2s, ttft=3.0s)
    // Weighted Carbon speed = (200 + 600) / (2 + 2) = 800 / 4 = 200.0 tok/s
    // Mean Carbon TTFT = (1.0 + 3.0) / 2 = 2.0s
    // Hydro speed = 400 / 2 = 200.0 tok/s, TTFT = 500ms
    {
        let conn = Connection::open(conv_dir.join("perf-db-2.db")).unwrap();
        conn.execute("CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB)", [])
            .unwrap();
        let blob = build_gen_metadata_blob(&[GenEntrySpec {
            raw_input_tokens: 1000,
            output_tokens: 600,
            ttft_sec: Some((3, 0)),
            stream_sec: Some((2, 0)),
            display_name: Some("Gemini 4 Carbon (High)"),
            ..Default::default()
        }]);
        conn.execute("INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)", params![1i64, blob])
            .unwrap();
    }

    let payload = serde_json::json!({
        "conversation_id": "perf-db-2",
        "model": { "id": "gemini-4-carbon-high", "display_name": "Gemini 4 Carbon (High)" }
    })
    .to_string();

    let (code, out, err) = env.run_stdin(&payload, &[("COLUMNS", "160")]);
    assert_eq!(code, 0);
    assert_eq!(err, "");
    let plain = strip_ansi(&out);
    assert!(
        plain.contains("out: 200.0 tok/s (ttft 2.0s)"),
        "main Carbon weighted speed (200.0) and mean TTFT (2.0s) missing:\n{plain}"
    );
    assert!(
        plain.contains("Hydro: 200.0 tok/s (ttft 500ms)"),
        "Hydro speed (200.0) and TTFT (500ms) missing:\n{plain}"
    );

    let cache_path = env.data_dir.join("agy-pulse-compaction-cache.json");
    let cache_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&cache_path).unwrap()).unwrap();
    assert!(
        cache_json["_db_perf"].as_object().is_some_and(|m| m.len() >= 2),
        "_db_perf cache must record scanned DBs: {cache_json}"
    );

    // Second run uses cached _db_perf (verify by truncating gen_metadata in DB 1? No, signature changes if DB modified;
    // simply assert repeated run produces identical output and preserves cache hit)
    let (code2, out2, _) = env.run_stdin(&payload, &[("COLUMNS", "160")]);
    assert_eq!(code2, 0);
    assert_eq!(strip_ansi(&out2), plain);
}

#[test]
fn t04_unified_pricing_cascade_cost_tree_and_trailing_transcript_backfill() {
    let env = IsolatedEnv::new();
    fs::write(
        env.global_cfg_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "language": "en",
            "display": {
                "showContextMonitor": true,
                "showModelCache": false,
                "quotaStyle": "none"
            }
        })
        .to_string(),
    )
    .unwrap();

    let main_id = "main-cascade-01";
    let child1_id = "child-agent-01";
    let child2_id = "grandchild-agent-02";
    let unrelated_id = "unrelated-cascade-99";

    let conv_dir = env.data_dir.join("conversations");
    fs::create_dir_all(&conv_dir).unwrap();

    // Main DB step 1 (Gemini 3.1 Pro):
    // f4.f1 (system prompt) = 150,000 + f4.f2 (raw input) = 40,000 -> input_tokens = 190,000
    // f4.f5 (cache_read) = 20,000 -> total_prompt = 210,000 > 200,000 -> >200k long tier!
    // Long tier rates for Gemini 3.1 Pro: input $4.00/M, output $18.00/M, cache_read $0.40/M
    // Cost = 190k * 4/1M (0.7600) + 1k * 18/1M (0.0180) + 20k * 0.4/1M (0.0080) = $0.7860
    {
        let conn = Connection::open(conv_dir.join(format!("{main_id}.db"))).unwrap();
        conn.execute("CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB)", [])
            .unwrap();
        let blob = build_gen_metadata_blob(&[GenEntrySpec {
            system_prompt_tokens: 150_000,
            raw_input_tokens: 40_000,
            output_tokens: 1_000,
            cache_read_tokens: 20_000,
            display_name: Some("Gemini 3.1 Pro (High)"),
            step_indices: &[1],
            kvs: &[("last_step_index", "1")],
            ..Default::default()
        }]);
        conn.execute("INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)", params![1i64, blob])
            .unwrap();
    }

    // Child 1 DB (parent_cascade_id = main_id, root_cascade_id = main_id):
    // Gemini 4 Hydrogen (High): input $1.50/M, output $7.50/M, cache_read $0.075/M
    // f4.f1 = 50,000, f4.f2 = 50,000 -> 100,000 input ($0.1500)
    // f4.f3 = 2,000 output ($0.0150)
    // f4.f5 = 100,000 cache_read ($0.0075)
    // Total Child 1 = $0.1725
    {
        let conn = Connection::open(conv_dir.join(format!("{child1_id}.db"))).unwrap();
        conn.execute("CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB)", [])
            .unwrap();
        let blob = build_gen_metadata_blob(&[GenEntrySpec {
            system_prompt_tokens: 50_000,
            raw_input_tokens: 50_000,
            output_tokens: 2_000,
            cache_read_tokens: 100_000,
            display_name: Some("Gemini 4 Hydrogen (High)"),
            kvs: &[
                ("parent_cascade_id", main_id),
                ("root_cascade_id", main_id),
            ],
            ..Default::default()
        }]);
        conn.execute("INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)", params![1i64, blob])
            .unwrap();
    }

    // Child 2 / Grandchild DB (parent_cascade_id = child1_id, NO root_cascade_id -> tests transitive lineage!):
    // Gemini 3.8 Flash (Medium): input $0.75/M, output $3.75/M
    // f4.f1 = 20,000, f4.f2 = 80,000 -> 100,000 input ($0.0750)
    // f4.f3 = 4,000 output ($0.0150)
    // Total Child 2 = $0.0900
    {
        let conn = Connection::open(conv_dir.join(format!("{child2_id}.db"))).unwrap();
        conn.execute("CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB)", [])
            .unwrap();
        let blob = build_gen_metadata_blob(&[GenEntrySpec {
            system_prompt_tokens: 20_000,
            raw_input_tokens: 80_000,
            output_tokens: 4_000,
            cache_read_tokens: 0,
            display_name: Some("Gemini 3.8 Flash (Medium)"),
            kvs: &[("parent_cascade_id", child1_id)],
            ..Default::default()
        }]);
        conn.execute("INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)", params![1i64, blob])
            .unwrap();
    }

    // Unrelated DB (must NOT be aggregated!)
    {
        let conn = Connection::open(conv_dir.join(format!("{unrelated_id}.db"))).unwrap();
        conn.execute("CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB)", [])
            .unwrap();
        let blob = build_gen_metadata_blob(&[GenEntrySpec {
            raw_input_tokens: 5_000_000,
            output_tokens: 500_000,
            display_name: Some("Claude Opus 4.6 (Thinking)"),
            kvs: &[("root_cascade_id", "someone-else")],
            ..Default::default()
        }]);
        conn.execute("INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)", params![1i64, blob])
            .unwrap();
    }

    // Main transcript.jsonl with step 1 (persisted, lacks system prompt tokens) + trailing unpersisted step 2
    // Step 2 (Gemini 3.1 Pro, <=200k tier: input $2.00/M, output $12.00/M):
    // input = 10,000 ($0.0200), output = 500 ($0.0060) -> $0.0260
    // Expected Cascade Total = 0.7860 + 0.0260 + 0.1725 + 0.0900 = $1.0745 -> formatted "$1.07"
    let transcript_dir = env
        .data_dir
        .join("brain")
        .join(main_id)
        .join(".system_generated")
        .join("logs");
    fs::create_dir_all(&transcript_dir).unwrap();
    let transcript_path = transcript_dir.join("transcript.jsonl");
    let t_step1 = serde_json::json!({
        "step_index": 1,
        "type": "PLANNER_RESPONSE",
        "model": "gemini-3.1-pro",
        "input_tokens": 40_000,
        "output_tokens": 1_000,
        "cache_read_tokens": 20_000
    });
    let t_step2 = serde_json::json!({
        "step_index": 2,
        "type": "PLANNER_RESPONSE",
        "model": "gemini-3.1-pro",
        "input_tokens": 10_000,
        "output_tokens": 500,
        "cache_read_tokens": 0
    });
    fs::write(&transcript_path, format!("{t_step1}\n{t_step2}\n")).unwrap();

    let payload = serde_json::json!({
        "conversation_id": main_id,
        "transcript_path": transcript_path.to_string_lossy(),
        "model": { "id": "gemini-3.1-pro", "display_name": "Gemini 3.1 Pro (High)" }
    })
    .to_string();

    let (code, out, err) = env.run_stdin(&payload, &[("COLUMNS", "160")]);
    assert_eq!(code, 0);
    assert_eq!(err, "");
    let plain = strip_ansi(&out);
    assert!(
        plain.contains("Cost $1.07"),
        "Cascade total cost must equal $1.07 ($0.7860+$0.0260+$0.1725+$0.0900=$1.0745):\n{plain}"
    );
    assert!(
        !plain.contains(child1_id) && !plain.contains(child2_id),
        "Statusline must remain minimal and not list child conversation IDs:\n{plain}"
    );
}

#[test]
fn t04_domain_routed_native_plus_children_cost_and_pure_db_record_export() {
    // 1) native_total_usd + children_cost_usd vs breakdown total
    let snap_native_plus_children = PulseSnapshot {
        model: Some(ModelIdentity {
            id: "gemini-4-carbon-high".to_string(),
            display_name: "Gemini 4 Carbon (High)".to_string(),
            effort: None,
        }),
        cost: CostSnapshot {
            native_total_usd: Some(0.40),
            children_cost_usd: Some(0.25),
            cascade_usd: Some(0.55),
            ..Default::default()
        },
        ..Default::default()
    };
    let resolved = resolve_cascade_cost_usd(&snap_native_plus_children, false).unwrap();
    assert!((resolved - 0.65).abs() < 1e-9, "expected max(0.55, 0.40+0.25)=0.65, got {resolved}");

    let snap_breakdown_higher = PulseSnapshot {
        cost: CostSnapshot {
            native_total_usd: Some(0.40),
            children_cost_usd: Some(0.25),
            cascade_usd: Some(0.80),
            ..Default::default()
        },
        ..snap_native_plus_children.clone()
    };
    let resolved2 = resolve_cascade_cost_usd(&snap_breakdown_higher, false).unwrap();
    assert!((resolved2 - 0.80).abs() < 1e-9, "expected max(0.80, 0.65)=0.80, got {resolved2}");

    // 2) Routed Bedrock/Vertex model ignores native_total_usd when allow_routed_cost=false
    let snap_routed = PulseSnapshot {
        model: Some(ModelIdentity {
            id: "anthropic.claude-3-5-sonnet-20241022-v2:0".to_string(),
            display_name: "Claude Sonnet 3.5".to_string(),
            effort: None,
        }),
        cost: CostSnapshot {
            native_total_usd: Some(9.99),
            children_cost_usd: Some(0.25),
            cascade_usd: Some(0.55),
            ..Default::default()
        },
        ..Default::default()
    };
    let routed_blocked = resolve_cascade_cost_usd(&snap_routed, false).unwrap();
    assert!((routed_blocked - 0.55).abs() < 1e-9);
    let routed_allowed = resolve_cascade_cost_usd(&snap_routed, true).unwrap();
    assert!((routed_allowed - 10.24).abs() < 1e-9);

    // 3) Unified calculate_token_cost >200k tiered pricing vs normal tier
    let pro_long = TokenUsage {
        input_tokens: 190_000,
        output_tokens: 1_000,
        cache_read_input_tokens: 20_000,
        cache_creation_input_tokens: 0,
        cache_exclusive: true,
    };
    let cost_long = calculate_token_cost("Gemini 3.1 Pro (High)", &pro_long, false, false).unwrap();
    assert!((cost_long - 0.786).abs() < 1e-9);

    // 4) Pure-data HostReader::read_conversation_db exports decoded steps without leaking SQLite handles
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("pure-export.db");
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute("CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB)", [])
            .unwrap();
        let blob = build_gen_metadata_blob(&[GenEntrySpec {
            system_prompt_tokens: 15_000,
            raw_input_tokens: 5_000,
            output_tokens: 300,
            cache_read_tokens: 2_000,
            ttft_sec: Some((1, 500_000_000)),
            stream_sec: Some((2, 0)),
            display_name: Some("Gemini 4 Carbon (High)"),
            step_indices: &[1],
            kvs: &[
                ("parent_cascade_id", "root-parent"),
                ("root_cascade_id", "root-main"),
                ("last_step_index", "1"),
            ],
            ..Default::default()
        }]);
        conn.execute("INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)", params![1i64, blob])
            .unwrap();
    }
    let rec = HostReader::read_conversation_db(&db_path).expect("must decode DB record");
    assert_eq!(rec.conversation_id, "pure-export");
    assert_eq!(rec.parent_cascade_id.as_deref(), Some("root-parent"));
    assert_eq!(rec.root_cascade_id.as_deref(), Some("root-main"));
    assert_eq!(rec.gen_rows.len(), 1);
    assert_eq!(rec.gen_rows[0].steps.len(), 1);
    assert_eq!(rec.gen_rows[0].steps[0].input_tokens, 20_000);
    assert_eq!(rec.gen_rows[0].steps[0].output_tokens, 300);
    assert_eq!(rec.gen_rows[0].steps[0].cache_read_tokens, 2_000);
}

#[test]
fn t04_lineage_child_db_perf_survives_top30_eviction_and_stale_turn_lockin() {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    let env = IsolatedEnv::new();
    fs::write(
        env.global_cfg_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "language": "en",
            "display": {
                "showSpeed": true,
                "showContextMonitor": false,
                "showModelCache": true,
                "quotaStyle": "none"
            }
        })
        .to_string(),
    )
    .unwrap();

    let conv_dir = env.data_dir.join("conversations");
    fs::create_dir_all(&conv_dir).unwrap();

    let main_id = "aaaa1111-0000-0000-0000-000000000001";
    let old_child_id = "aaaa1111-0000-0000-0000-000000000002";
    let fresh_child_id = "aaaa1111-0000-0000-0000-000000000003";

    // Main DB with Carbon streaming stats
    {
        let conn = Connection::open(conv_dir.join(format!("{main_id}.db"))).unwrap();
        conn.execute("CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB)", [])
            .unwrap();
        let blob = build_gen_metadata_blob(&[GenEntrySpec {
            raw_input_tokens: 5_000,
            output_tokens: 300,
            cache_read_tokens: 10_000,
            ttft_sec: Some((2, 0)),
            stream_sec: Some((3, 0)),
            display_name: Some("Gemini 4 Carbon (High)"),
            ..Default::default()
        }]);
        conn.execute("INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)", params![1i64, blob])
            .unwrap();
    }

    // Active lineage child DB with Hydro streaming stats (200.0 tok/s, TTFT 500ms),
    // backdated slightly by 10 seconds so 35 newer unrelated DBs sort ahead of it in all_dbs!
    let fresh_child_db = conv_dir.join(format!("{fresh_child_id}.db"));
    {
        let conn = Connection::open(&fresh_child_db).unwrap();
        conn.execute("CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB)", [])
            .unwrap();
        let blob = build_gen_metadata_blob(&[GenEntrySpec {
            raw_input_tokens: 10_000,
            output_tokens: 400,
            cache_read_tokens: 90_000,
            ttft_sec: Some((0, 500_000_000)),
            stream_sec: Some((2, 0)),
            display_name: Some("Gemini 4 Hydrogen (High)"),
            kvs: &[
                ("parent_cascade_id", main_id),
                ("root_cascade_id", main_id),
            ],
            ..Default::default()
        }]);
        conn.execute("INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)", params![1i64, blob])
            .unwrap();
    }
    let child_mtime = SystemTime::now() - Duration::from_secs(10);
    {
        let f = fs::OpenOptions::new().write(true).open(&fresh_child_db).unwrap();
        f.set_modified(child_mtime).unwrap();
    }

    // Create 35 newer unrelated DBs (all using Neon) so fresh_child_db is evicted from top-30 global all_dbs
    for i in 0..35 {
        let p = conv_dir.join(format!("unrelated-newer-{i:02}.db"));
        let conn = Connection::open(&p).unwrap();
        conn.execute("CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB)", [])
            .unwrap();
        let blob = build_gen_metadata_blob(&[GenEntrySpec {
            raw_input_tokens: 1_000,
            output_tokens: 100,
            stream_sec: Some((1, 0)),
            display_name: Some("Gemini 4 Neon (High)"),
            ..Default::default()
        }]);
        conn.execute("INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)", params![1i64, blob])
            .unwrap();
    }

    // Build a main transcript with 35 USER_INPUT turns where an OLD Hydro subagent ran at turn 1
    // (producing stale current_session_model_turns_ago["Hydro"] = 34 > 30), while fresh_child_id
    // ran during the latest turn (mtime >= last_user_input_ms) and was discovered via DB lineage.
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let iso_at = |epoch_secs: i64| -> String {
        let days = epoch_secs.div_euclid(86_400);
        let sod = epoch_secs.rem_euclid(86_400);
        let h = sod / 3600;
        let m = (sod % 3600) / 60;
        let s = sod % 60;
        let z = days + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let mo = if mp < 10 { mp + 3 } else { mp - 9 };
        let yr = if mo <= 2 { y + 1 } else { y };
        format!("{yr:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
    };

    let main_logs = env
        .data_dir
        .join("brain")
        .join(main_id)
        .join(".system_generated")
        .join("logs");
    fs::create_dir_all(&main_logs).unwrap();
    let main_transcript = main_logs.join("transcript.jsonl");

    let mut lines = vec![
        serde_json::json!({
            "step_index": 1,
            "type": "USER_INPUT",
            "created_at": iso_at(now_secs - 120)
        })
        .to_string(),
        serde_json::json!({
            "step_index": 2,
            "type": "GENERIC",
            "created_at": iso_at(now_secs - 115),
            "content": serde_json::json!({
                "status": "added",
                "conversation_id": old_child_id,
                "name": "old-hydro",
                "model_display_name": "Gemini 4 Hydrogen (High)"
            })
            .to_string()
        })
        .to_string(),
    ];
    for t in 2..=35 {
        lines.push(
            serde_json::json!({
                "step_index": t + 1,
                "type": "USER_INPUT",
                "created_at": iso_at(now_secs - 90 + t)
            })
            .to_string(),
        );
    }
    fs::write(&main_transcript, format!("{}\n", lines.join("\n"))).unwrap();

    let payload = serde_json::json!({
        "conversation_id": main_id,
        "transcript_path": main_transcript.to_string_lossy(),
        "model": { "id": "gemini-4-carbon-high", "display_name": "Gemini 4 Carbon (High)" }
    })
    .to_string();

    let (code, out, err) = env.run_stdin(&payload, &[("COLUMNS", "180")]);
    assert_eq!(code, 0, "stderr: {err}");
    let plain = strip_ansi(&out);
    assert!(
        plain.contains("Hydro 90%"),
        "Session cache line must include lineage Hydro DB:\n{plain}"
    );
    assert!(
        plain.contains("Hydro: 200.0 tok/s (ttft 500ms)"),
        "Lineage Hydro child DB must survive top-30 eviction and >30-turn stale lock-in:\n{plain}"
    );
}

#[test]
fn t04_many_uncached_wal_dbs_are_bounded_per_statusline_call() {
    use std::time::{Duration, Instant};

    let env = IsolatedEnv::new();
    fs::write(
        env.global_cfg_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "language": "en",
            "display": {
                "showSpeed": true,
                "quotaStyle": "none"
            }
        })
        .to_string(),
    )
    .unwrap();

    let conv_dir = env.data_dir.join("conversations");
    fs::create_dir_all(&conv_dir).unwrap();

    let mut open_conns = Vec::new();
    for i in 0..20 {
        let db_path = conv_dir.join(format!("wal-heavy-{i:02}.db"));
        let conn = Connection::open(&db_path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        conn.execute("CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB)", [])
            .unwrap();
        let blob = build_gen_metadata_blob(&[GenEntrySpec {
            raw_input_tokens: 1_000,
            output_tokens: 100,
            stream_sec: Some((1, 0)),
            display_name: Some("Gemini 4 Neon (High)"),
            ..Default::default()
        }]);
        conn.execute("INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)", params![1i64, blob])
            .unwrap();
        // Keep connections open so SQLite preserves non-empty -wal files during statusline run.
        assert!(conv_dir.join(format!("wal-heavy-{i:02}.db-wal")).exists());
        open_conns.push(conn);
    }

    let payload = serde_json::json!({
        "conversation_id": "wal-heavy-00",
        "model": { "id": "gemini-4-neon-high", "display_name": "Gemini 4 Neon (High)" }
    })
    .to_string();

    let start = Instant::now();
    let (code, out, err) = env.run_stdin(&payload, &[("COLUMNS", "140")]);
    let elapsed = start.elapsed();
    drop(open_conns);

    assert_eq!(code, 0, "stderr: {err}");
    assert!(!strip_ansi(&out).trim().is_empty());
    assert!(
        elapsed < Duration::from_millis(2000),
        "bounded WAL-copy statusline call took too long: {elapsed:?}"
    );

    let cache_path = env.data_dir.join("agy-pulse-compaction-cache.json");
    let cache_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&cache_path).unwrap()).unwrap();
    let perf_count = cache_json["_db_perf"].as_object().map(|m| m.len()).unwrap_or(0);
    assert!(
        perf_count <= 10,
        "expected WAL-copy DB scan per statusline call to be bounded (<= 10), got {perf_count}"
    );
}

fn pb_timestamp(field_num: u32, epoch_sec: u64, nanos: u64) -> Vec<u8> {
    pb_duration(field_num, epoch_sec, nanos)
}

fn build_step_metadata_blob(epoch_sec: u64, bot_uuid: Option<&str>) -> Vec<u8> {
    let mut out = pb_timestamp(1, epoch_sec, 0);
    if let Some(bot) = bot_uuid {
        let f9 = pb_str(7, bot);
        out.extend(pb_bytes(9, &f9));
    }
    out
}

#[test]
fn t04_same_session_model_switch_expires_old_model_even_when_db_mtime_refreshes() {
    use std::time::{SystemTime, UNIX_EPOCH};

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

    let conv_dir = env.data_dir.join("conversations");
    fs::create_dir_all(&conv_dir).unwrap();

    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    // Case 1: Same-session DB WITH `steps` table:
    // Model A (Hydro) ran at step 1 (10 minutes ago), then switched to Model B (Carbon) for steps 2..=34 (10s ago).
    // Even though the .db file mtime is brand new (now), Hydro's per-model timestamp comes from `steps` (now - 600s)
    // and is NOT overwritten by Carbon's DB mtime.
    let conv_with_steps = "same-session-switch-steps-01";
    {
        let conn = Connection::open(conv_dir.join(format!("{conv_with_steps}.db"))).unwrap();
        conn.execute(
            "CREATE TABLE steps (idx INTEGER PRIMARY KEY, metadata BLOB)",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB)",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO steps (idx, metadata) VALUES (?1, ?2)",
            params![1i64, build_step_metadata_blob(now_secs - 600, Some("bot-hydro"))],
        )
        .unwrap();
        let hydro_blob = build_gen_metadata_blob(&[GenEntrySpec {
            raw_input_tokens: 1_000,
            output_tokens: 400,
            stream_sec: Some((2, 0)),
            ttft_sec: Some((0, 500_000_000)),
            display_name: Some("Gemini 4 Hydrogen (High)"),
            step_indices: &[1],
            ..Default::default()
        }]);
        conn.execute(
            "INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)",
            params![1i64, hydro_blob],
        )
        .unwrap();

        for step in 2..=34i64 {
            conn.execute(
                "INSERT INTO steps (idx, metadata) VALUES (?1, ?2)",
                params![step, build_step_metadata_blob(now_secs - 10, Some("bot-carbon"))],
            )
            .unwrap();
            let carbon_blob = build_gen_metadata_blob(&[GenEntrySpec {
                raw_input_tokens: 1_000,
                output_tokens: 300,
                stream_sec: Some((2, 0)),
                display_name: Some("Gemini 4 Carbon (High)"),
                step_indices: &[step as u64],
                ..Default::default()
            }]);
            conn.execute(
                "INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)",
                params![step, carbon_blob],
            )
            .unwrap();
        }
    }

    let payload_steps = serde_json::json!({
        "conversation_id": conv_with_steps,
        "model": { "id": "gemini-4-carbon-high", "display_name": "Gemini 4 Carbon (High)" }
    })
    .to_string();
    let (code1, out1, err1) = env.run_stdin(&payload_steps, &[("COLUMNS", "160")]);
    assert_eq!(code1, 0, "stderr: {err1}");
    let plain1 = strip_ansi(&out1);
    assert!(
        plain1.contains("out: 150.0 tok/s"),
        "active model Carbon must render its speed:\n{plain1}"
    );
    assert!(
        !plain1.contains("Hydro:"),
        "old model Hydro (>5m old in steps) must NOT be refreshed by .db mtime:\n{plain1}"
    );

    // Case 2: Minimal multi-model DB WITHOUT `steps` table + transcript with >30 turns:
    // Model A (Argon) only appeared at gen_metadata idx=1, Model B (Carbon) ran at idx=2..=34.
    // Even without `steps` table, Argon (max_gen_idx=1 < record.max_gen_idx=34) must NOT receive
    // the fresh .db mtime or have its turns_ago reset to 0.
    let env2 = IsolatedEnv::new();
    fs::write(
        env2.global_cfg_dir.join("agy-pulse.config.json"),
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
    let conv_dir2 = env2.data_dir.join("conversations");
    fs::create_dir_all(&conv_dir2).unwrap();

    let conv_no_steps = "same-session-switch-nosteps-02";
    {
        let conn = Connection::open(conv_dir2.join(format!("{conv_no_steps}.db"))).unwrap();
        conn.execute(
            "CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB)",
            [],
        )
        .unwrap();
        let argon_blob = build_gen_metadata_blob(&[GenEntrySpec {
            raw_input_tokens: 1_000,
            output_tokens: 440,
            stream_sec: Some((2, 0)),
            display_name: Some("Gemini 4 Argon (High)"),
            ..Default::default()
        }]);
        conn.execute(
            "INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)",
            params![1i64, argon_blob],
        )
        .unwrap();

        for idx in 2..=34i64 {
            let carbon_blob = build_gen_metadata_blob(&[GenEntrySpec {
                raw_input_tokens: 1_000,
                output_tokens: 360,
                stream_sec: Some((2, 0)),
                display_name: Some("Gemini 4 Carbon (High)"),
                ..Default::default()
            }]);
            conn.execute(
                "INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)",
                params![idx, carbon_blob],
            )
            .unwrap();
        }
    }

    let iso_at = |epoch_secs: u64| -> String {
        let days = (epoch_secs as i64).div_euclid(86_400);
        let sod = (epoch_secs as i64).rem_euclid(86_400);
        let h = sod / 3600;
        let m = (sod % 3600) / 60;
        let s = sod % 60;
        let z = days + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let mo = if mp < 10 { mp + 3 } else { mp - 9 };
        let yr = if mo <= 2 { y + 1 } else { y };
        format!("{yr:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
    };

    let main_logs = env2
        .data_dir
        .join("brain")
        .join(conv_no_steps)
        .join(".system_generated")
        .join("logs");
    fs::create_dir_all(&main_logs).unwrap();
    let transcript_path = main_logs.join("transcript.jsonl");
    let mut t_lines = vec![
        serde_json::json!({
            "step_index": 1,
            "type": "USER_INPUT",
            "created_at": iso_at(now_secs - 700)
        })
        .to_string(),
        serde_json::json!({
            "step_index": 2,
            "type": "PLANNER_RESPONSE",
            "created_at": iso_at(now_secs - 690),
            "model": "gemini-4-argon-high"
        })
        .to_string(),
    ];
    let mut step = 3u64;
    for t in 2..=34u64 {
        t_lines.push(
            serde_json::json!({
                "step_index": step,
                "type": "USER_INPUT",
                "created_at": iso_at(now_secs - 100 + t)
            })
            .to_string(),
        );
        step += 1;
    }
    t_lines.push(
        serde_json::json!({
            "step_index": step,
            "type": "PLANNER_RESPONSE",
            "created_at": iso_at(now_secs - 5),
            "model": "gemini-4-carbon-high"
        })
        .to_string(),
    );
    fs::write(&transcript_path, format!("{}\n", t_lines.join("\n"))).unwrap();

    let payload_no_steps = serde_json::json!({
        "conversation_id": conv_no_steps,
        "transcript_path": transcript_path.to_string_lossy(),
        "model": { "id": "gemini-4-carbon-high", "display_name": "Gemini 4 Carbon (High)" }
    })
    .to_string();
    let (code2, out2, err2) = env2.run_stdin(&payload_no_steps, &[("COLUMNS", "160")]);
    assert_eq!(code2, 0, "stderr: {err2}");
    let plain2 = strip_ansi(&out2);
    assert!(
        plain2.contains("out: 180.0 tok/s"),
        "active model Carbon must render 180.0 tok/s:\n{plain2}"
    );
    assert!(
        !plain2.contains("Argon:"),
        "old model Argon in multi-model DB without steps table must stay hidden:\n{plain2}"
    );
}

#[test]
fn t04_same_session_model_switch_refreshes_compaction_limit_immediately() {
    let env = IsolatedEnv::new();
    fs::write(
        env.global_cfg_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "language": "en",
            "display": {
                "showContextMonitor": true,
                "quotaStyle": "none"
            }
        })
        .to_string(),
    )
    .unwrap();

    let conv_dir = env.data_dir.join("conversations");
    fs::create_dir_all(&conv_dir).unwrap();
    let conv_id = "conv-switch-limit-01";
    let db_path = conv_dir.join(format!("{conv_id}.db"));

    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)",
            params![1i64, build_compaction_gen_metadata_blob(500_000)],
        )
        .unwrap();
    }

    let make_payload = |id: &str, disp: &str| {
        serde_json::json!({
            "conversation_id": conv_id,
            "model": { "id": id, "display_name": disp },
            "context_window": {
                "context_window_size": 1_000_000,
                "current_usage": { "input_tokens": 100_000, "output_tokens": 0 }
            }
        })
        .to_string()
    };

    // 1) Initial model Carbon -> 100K / 500K (20.0%)
    let (code1, out1, err1) = env.run_stdin(
        &make_payload("gemini-4-carbon-high", "Gemini 4 Carbon (High)"),
        &[("COLUMNS", "120")],
    );
    assert_eq!(code1, 0, "stderr: {err1}");
    let plain1 = strip_ansi(&out1);
    assert!(
        plain1.contains("100K/500K") && plain1.contains("20.0%"),
        "initial compaction limit must be 500K:\n{plain1}"
    );

    // 2) Update DB with a new gen_metadata row carrying 200k compaction limit
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)",
            params![2i64, build_compaction_gen_metadata_blob(200_000)],
        )
        .unwrap();
    }

    // Same model within 5-minute window reuses cached 500k limit
    let (code2, out2, err2) = env.run_stdin(
        &make_payload("gemini-4-carbon-high", "Gemini 4 Carbon (High)"),
        &[("COLUMNS", "120")],
    );
    assert_eq!(code2, 0, "stderr: {err2}");
    let plain2 = strip_ansi(&out2);
    assert!(
        plain2.contains("100K/500K") && plain2.contains("20.0%"),
        "same model within recheck window should reuse cached 500K:\n{plain2}"
    );

    // 3) Switch model to Claude Sonnet 4.6 -> must bypass 5-minute cache and refresh to 200K (50.0%) immediately!
    let (code3, out3, err3) = env.run_stdin(
        &make_payload("claude-sonnet-4-6", "Claude Sonnet 4.6 (Thinking)"),
        &[("COLUMNS", "120")],
    );
    assert_eq!(code3, 0, "stderr: {err3}");
    let plain3 = strip_ansi(&out3);
    assert!(
        plain3.contains("100K/200K") && plain3.contains("50.0%"),
        "switching model in same session must immediately refresh compaction limit to 200K:\n{plain3}"
    );
}

#[test]
fn t04_malformed_percent_workspace_uri_in_db_does_not_panic() {
    let env = IsolatedEnv::new();
    fs::write(
        env.global_cfg_dir.join("agy-pulse.config.json"),
        serde_json::json!({
            "language": "en",
            "display": {
                "showContextMonitor": true,
                "quotaStyle": "none"
            }
        })
        .to_string(),
    )
    .unwrap();

    let conv_dir = env.data_dir.join("conversations");
    fs::create_dir_all(&conv_dir).unwrap();
    let conv_id = "conv-malformed-percent-uri";
    let db_path = conv_dir.join(format!("{conv_id}.db"));

    // trajectory_metadata_blob: f1 (submessage) -> f1 ("file:///path/c++/%a€"), f6 (cascade_id)
    let mut traj_blob = pb_bytes(1, &pb_str(1, "file:///path/c++/%a€"));
    traj_blob.extend(pb_str(6, conv_id));

    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE trajectory_metadata_blob (id TEXT PRIMARY KEY, data BLOB)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO trajectory_metadata_blob (id, data) VALUES (?1, ?2)",
            params!["main", traj_blob],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)",
            params![1i64, build_compaction_gen_metadata_blob(500_000)],
        )
        .unwrap();
    }

    // 1) HostReader::read_conversation_db must not panic on "%a€" and must preserve '+'
    let rec = HostReader::read_conversation_db(&db_path).expect("must decode DB record");
    assert_eq!(rec.cascade_id.as_deref(), Some(conv_id));
    assert_eq!(
        rec.trajectory.as_ref().and_then(|t| t.workspace.as_deref()),
        Some("/path/c++/%a€")
    );

    // 2) Full CLI subprocess reading the same host DB must exit 0 without panic
    let payload = serde_json::json!({
        "conversation_id": conv_id,
        "model": { "id": "gemini-4-carbon-high", "display_name": "Gemini 4 Carbon (High)" },
        "context_window": {
            "context_window_size": 1_000_000,
            "current_usage": { "input_tokens": 100_000, "output_tokens": 0 }
        }
    })
    .to_string();
    let (code, out, err) = env.run_stdin(&payload, &[("COLUMNS", "120")]);
    assert_eq!(code, 0, "CLI must not panic, stderr: {err}");
    assert!(
        !err.contains("panicked"),
        "CLI stderr must not contain panic: {err}"
    );
    let plain = strip_ansi(&out);
    assert!(
        plain.contains("100K/500K"),
        "CLI output must still read compaction limit from DB:\n{plain}"
    );
}

