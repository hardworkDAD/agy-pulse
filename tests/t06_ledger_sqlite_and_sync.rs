#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use chrono::{Local, TimeZone};
use rusqlite::{Connection, params};
use tempfile::TempDir;

use agy_pulse::domain::{
    DeviceIdentity, ProviderQuota, ProviderQuotaSummary, QuotaSnapshot, QuotaWindow,
    UNASSIGNED_ACCOUNT_ID, normalize_account_id,
};
use agy_pulse::ledger::{
    LedgerError, LedgerSource, LedgerStore, LedgerSyncOptions, RollingCostInput, SourceRoot,
};
use agy_pulse::platform::{
    current_hostname, generate_device_id, read_sync_identity_hints, resolve_sync_config_path,
};

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

fn build_gen_blob(
    step_indices: &[u64],
    display_name: &str,
    input_tokens: u64,
    cache_read_tokens: u64,
    output_tokens: u64,
) -> Vec<u8> {
    let mut out = Vec::new();
    for &idx in step_indices {
        out.extend(pb_varint(2, idx));
    }
    let mut f1 = pb_str(21, display_name);
    let mut usage = Vec::new();
    if input_tokens > 0 {
        usage.extend(pb_varint(2, input_tokens));
    }
    if output_tokens > 0 {
        usage.extend(pb_varint(3, output_tokens));
    }
    if cache_read_tokens > 0 {
        usage.extend(pb_varint(5, cache_read_tokens));
    }
    f1.extend(pb_bytes(4, &usage));
    f1.extend(pb_duration_sec(11, 1, 0));
    f1.extend(pb_duration_sec(12, 10, 0));
    out.extend(pb_bytes(1, &f1));
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

fn is_valid_device_id(id: &str) -> bool {
    let Some(hex_part) = id.strip_prefix("dev_") else {
        return false;
    };
    hex_part.len() == 16 && hex_part.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[test]
fn t06_domain_sync_types_and_platform_identity() {
    assert_eq!(UNASSIGNED_ACCOUNT_ID, "unassigned");
    assert_eq!(normalize_account_id(""), None);
    assert_eq!(normalize_account_id("   \t\n "), None);
    assert_eq!(
        normalize_account_id("  Alice.Smith+Work@Example.COM "),
        Some("alice.smith+work@example.com".to_string())
    );

    let mut seen = HashSet::new();
    for _ in 0..32 {
        let id = generate_device_id();
        assert!(is_valid_device_id(&id), "invalid device_id format: {id}");
        assert!(seen.insert(id), "consecutive generate_device_id() collided");
    }

    if let Some(host) = current_hostname() {
        assert!(!host.is_empty());
        assert!(host.chars().count() <= 64);
        assert!(!host.chars().any(|c| c.is_control()));
        assert_eq!(host, host.trim());
    }

    let dev = DeviceIdentity {
        device_id: "dev_0123456789abcdef".to_string(),
        device_name: "lab-ubuntu".to_string(),
        is_local: true,
    };
    let dev_json = serde_json::to_string(&dev).unwrap();
    let dev_back: DeviceIdentity = serde_json::from_str(&dev_json).unwrap();
    assert_eq!(dev, dev_back);

    let snap = QuotaSnapshot {
        account_id: "alice@example.com".to_string(),
        device_id: "dev_0123456789abcdef".to_string(),
        device_name: "lab-ubuntu".to_string(),
        plan_tier: Some("Google AI Ultra".to_string()),
        observed_at_ms: 1_795_699_950_000,
        quota: ProviderQuotaSummary {
            google: ProviderQuota {
                five_hour: Some(QuotaWindow {
                    remaining_fraction: 0.97,
                    reset_time_ms: Some(1_795_712_509_000),
                }),
                weekly: Some(QuotaWindow {
                    remaining_fraction: 0.93,
                    reset_time_ms: Some(1_796_281_309_000),
                }),
            },
            claude: ProviderQuota {
                five_hour: Some(QuotaWindow {
                    remaining_fraction: 1.0,
                    reset_time_ms: None,
                }),
                weekly: None,
            },
            argon: ProviderQuota::default(),
            extras: Vec::new(),
        },
    };
    let snap_json = serde_json::to_string(&snap).unwrap();
    let snap_back: QuotaSnapshot = serde_json::from_str(&snap_json).unwrap();
    assert_eq!(snap, snap_back);

    // Omitted optional fields (`plan_tier`, `argon`, `extras`, `weekly`, `reset_time_ms`) must deserialize cleanly.
    let partial_json = r#"{
        "account_id": "alice@example.com",
        "device_id": "dev_0123456789abcdef",
        "device_name": "lab-ubuntu",
        "observed_at_ms": 1795699950000,
        "quota": {
            "google": {
                "five_hour": { "remaining_fraction": 0.5 }
            }
        }
    }"#;
    let partial: QuotaSnapshot = serde_json::from_str(partial_json).unwrap();
    assert_eq!(partial.plan_tier, None);
    assert_eq!(
        partial.quota.google.five_hour,
        Some(QuotaWindow {
            remaining_fraction: 0.5,
            reset_time_ms: None
        })
    );
    assert_eq!(partial.quota.google.weekly, None);
    assert_eq!(partial.quota.argon, ProviderQuota::default());
    assert!(partial.quota.extras.is_empty());
}

#[test]
fn t06_schema_v3_to_v4_migration_backfills_and_is_idempotent() {
    let temp = TempDir::new().unwrap();
    let ledger_path = temp.path().join("agy-pulse-ledger.sqlite");

    // Seed an authentic Schema v3 database without account_id/device_id columns or v4 tables.
    {
        let conn = Connection::open(&ledger_path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute_batch(
            "CREATE TABLE ledger_meta (
               key TEXT PRIMARY KEY,
               value TEXT NOT NULL
             );
             CREATE TABLE ingested_dbs (
               conversation_id TEXT PRIMARY KEY,
               db_path TEXT NOT NULL,
               sig TEXT NOT NULL,
               last_synced_ms INTEGER NOT NULL,
               workspace TEXT,
               branch TEXT,
               created_at_ms INTEGER,
               cascade_id TEXT,
               parent_conversation_id TEXT,
               root_conversation_id TEXT,
               db_mtime_ms INTEGER NOT NULL,
               source TEXT NOT NULL DEFAULT 'cli'
             );
             CREATE TABLE telemetry_steps (
               step_uid TEXT PRIMARY KEY,
               conversation_id TEXT NOT NULL,
               gen_idx INTEGER NOT NULL,
               entry_idx INTEGER NOT NULL,
               step_index INTEGER,
               ts_ms INTEGER NOT NULL,
               model TEXT NOT NULL,
               raw_model TEXT,
               input_tokens INTEGER NOT NULL DEFAULT 0,
               cache_read_tokens INTEGER NOT NULL DEFAULT 0,
               cache_write_tokens INTEGER NOT NULL DEFAULT 0,
               output_tokens INTEGER NOT NULL DEFAULT 0,
               cost_usd REAL NOT NULL DEFAULT 0,
               stream_sec REAL,
               ttft_sec REAL,
               valid_stream INTEGER NOT NULL DEFAULT 0,
               valid_ttft INTEGER NOT NULL DEFAULT 0,
               parent_conversation_id TEXT,
               root_conversation_id TEXT,
               workspace TEXT,
               branch TEXT,
               source TEXT NOT NULL DEFAULT 'cli'
             );
             CREATE TABLE rolling_cost_cursors (
               cursor_id TEXT PRIMARY KEY,
               cascade_root_id TEXT,
               baseline REAL NOT NULL,
               week_baseline REAL NOT NULL,
               week_start_ms INTEGER NOT NULL,
               total REAL NOT NULL,
               last_observed_cost REAL NOT NULL,
               ts_ms INTEGER NOT NULL,
               db_total_anchor REAL NOT NULL DEFAULT 0,
               db_week_total_anchor REAL NOT NULL DEFAULT 0
             );
             INSERT INTO ledger_meta (key, value) VALUES ('schema_version', '3');
             INSERT INTO ingested_dbs (
               conversation_id, db_path, sig, last_synced_ms, workspace, branch,
               created_at_ms, cascade_id, parent_conversation_id, root_conversation_id,
               db_mtime_ms, source
             ) VALUES (
               'v3-conv-1', '/tmp/v3-conv-1.db', '10:10:0:0', 1700000000000, '/work/a', 'main',
               1700000000000, 'v3-conv-1', NULL, NULL, 1700000000000, 'cli'
             );
             INSERT INTO telemetry_steps (
               step_uid, conversation_id, gen_idx, entry_idx, step_index, ts_ms,
               model, raw_model, input_tokens, cache_read_tokens, cache_write_tokens, output_tokens,
               cost_usd, stream_sec, ttft_sec, valid_stream, valid_ttft,
               parent_conversation_id, root_conversation_id, workspace, branch, source
             ) VALUES (
               'v3-conv-1:1:0', 'v3-conv-1', 1, 0, 1, 1700000001000,
               'Carbon', 'Gemini 4 Carbon (High)', 1000, 2000, 0, 100,
               0.42, 2.0, 0.5, 1, 1,
               NULL, NULL, '/work/a', 'main', 'cli'
             );",
        )
        .unwrap();
    }

    // Open through LedgerStore -> upgrades v3 to v4.
    let store = LedgerStore::open_at(&ledger_path).expect("upgrade v3 -> v4");
    let local_id = store.local_device_id().expect("local_device_id");
    assert!(is_valid_device_id(&local_id), "unexpected local_device_id: {local_id}");
    drop(store);

    // Re-open to verify full idempotency and hot-path check stability.
    let store2 = LedgerStore::open_at(&ledger_path).expect("reopen v4");
    assert_eq!(store2.local_device_id().unwrap(), local_id);
    drop(store2);

    let conn = Connection::open(&ledger_path).unwrap();
    let version: String = conn
        .query_row(
            "SELECT value FROM ledger_meta WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(version, "4");

    // Existing rows preserved and backfilled.
    let (db_acct, db_dev): (String, String) = conn
        .query_row(
            "SELECT account_id, device_id FROM ingested_dbs WHERE conversation_id = 'v3-conv-1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(db_acct, "unassigned");
    assert_eq!(db_dev, local_id);

    let (step_acct, step_dev, step_cost): (String, String, f64) = conn
        .query_row(
            "SELECT account_id, device_id, cost_usd FROM telemetry_steps WHERE step_uid = 'v3-conv-1:1:0'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(step_acct, "unassigned");
    assert_eq!(step_dev, local_id);
    assert!((step_cost - 0.42).abs() < 1e-9);

    // New tables & indexes exist, and sync_devices registered local device once.
    for table in ["sync_devices", "quota_snapshots", "sync_chunk_state"] {
        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                params![table],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(exists, 1, "missing table {table}");
    }
    for idx in ["idx_steps_device_ts", "idx_steps_account_device_ts"] {
        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
                params![idx],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(exists, 1, "missing index {idx}");
    }

    let sync_dev_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sync_devices WHERE account_id = 'unassigned' AND device_id = ?1 AND is_local = 1",
            params![&local_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sync_dev_count, 1);
}

#[test]
fn t06_remote_data_isolation_rolling_cost_and_clear_remote_data() {
    let temp = TempDir::new().unwrap();
    let cli_dir = temp.path().join("cli");
    let ledger_path = temp.path().join("agy-pulse-ledger.sqlite");
    let now_ms = local_ms(2026, 11, 26, 14, 0, 0);

    // 1. Seed a local conversation DB costing exactly $3.00 (1M input * $2/M + 100k output * $10/M on Carbon).
    write_conv_db(
        &cli_dir.join("conversations").join("local-conv-1.db"),
        Some(&build_trajectory_blob(
            "file:///work/local",
            "main",
            now_ms - 120_000,
            Some("local-conv-1"),
        )),
        &[(1, build_step_metadata_blob(now_ms - 60_000))],
        &[(
            1,
            build_gen_blob(&[1], "Gemini 4 Carbon (High)", 1_000_000, 0, 100_000),
        )],
    );

    let mut store = LedgerStore::open_at(&ledger_path).unwrap();
    let local_id = store.local_device_id().unwrap();
    let sync_opts = LedgerSyncOptions {
        source_roots: Some(vec![SourceRoot {
            root: cli_dir.clone(),
            source: LedgerSource::Cli,
        }]),
        now_ms: Some(now_ms),
        ..Default::default()
    };
    let report = store.sync_all(&sync_opts).unwrap();
    assert_eq!(report.newly_synced_dbs, 1);
    assert_eq!(report.steps_upserted, 1);

    // Simulate local conversation already bound to alice@example.com, then force re-sync:
    // sync_all/sync_conversation must preserve non-'unassigned' account_id and keep local device_id.
    {
        let conn = Connection::open(&ledger_path).unwrap();
        conn.execute(
            "UPDATE ingested_dbs SET account_id = 'alice@example.com' WHERE conversation_id = 'local-conv-1'",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE telemetry_steps SET account_id = 'alice@example.com' WHERE conversation_id = 'local-conv-1'",
            [],
        )
        .unwrap();
    }
    let forced = store
        .sync_all(&LedgerSyncOptions {
            force: true,
            ..sync_opts.clone()
        })
        .unwrap();
    assert_eq!(forced.newly_synced_dbs, 1);
    assert_eq!(forced.steps_upserted, 1);

    // 2. Insert remote device rows (including high-cost today/week steps and pull chunk state) + local push state.
    let remote_id = "dev_remote_0001";
    store
        .upsert_sync_device(
            "alice@example.com",
            remote_id,
            "remote-workstation",
            false,
            now_ms - 5_000,
            Some("Google AI Ultra"),
        )
        .unwrap();
    {
        let conn = Connection::open(&ledger_path).unwrap();
        let (local_db_acct, local_db_dev): (String, String) = conn
            .query_row(
                "SELECT account_id, device_id FROM ingested_dbs WHERE conversation_id = 'local-conv-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(local_db_acct, "alice@example.com");
        assert_eq!(local_db_dev, local_id);

        conn.execute(
            "INSERT INTO ingested_dbs (
               conversation_id, db_path, sig, last_synced_ms, workspace, branch,
               created_at_ms, cascade_id, parent_conversation_id, root_conversation_id,
               db_mtime_ms, source, account_id, device_id
             ) VALUES (
               'remote-conv-1', '', 'remote', ?1, NULL, NULL,
               ?1, 'remote-conv-1', NULL, NULL, ?1, 'cli', 'alice@example.com', ?2
             )",
            params![now_ms - 30_000, remote_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO telemetry_steps (
               step_uid, conversation_id, gen_idx, entry_idx, step_index, ts_ms,
               model, raw_model, input_tokens, cache_read_tokens, cache_write_tokens, output_tokens,
               cost_usd, stream_sec, ttft_sec, valid_stream, valid_ttft,
               parent_conversation_id, root_conversation_id, workspace, branch, source,
               account_id, device_id
             ) VALUES (
               'remote-conv-1:1:0', 'remote-conv-1', 1, 0, 1, ?1,
               'Carbon', 'Gemini 4 Carbon (High)', 100000, 0, 0, 10000,
               500.0, 5.0, 0.5, 1, 1,
               NULL, NULL, NULL, NULL, 'cli', 'alice@example.com', ?2
             )",
            params![now_ms - 30_000, remote_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO quota_snapshots (account_id, device_id, device_name, plan_tier, observed_at_ms, quota_json)
             VALUES ('alice@example.com', ?1, 'remote-workstation', 'Google AI Ultra', ?2, '{}')",
            params![remote_id, now_ms - 10_000],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sync_chunk_state (account_id, device_id, month_key, direction, content_sha256, updated_at_ms)
             VALUES ('alice@example.com', ?1, '2026-11', 'pull', 'abc123', ?2)",
            params![remote_id, now_ms],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sync_chunk_state (account_id, device_id, month_key, direction, content_sha256, updated_at_ms)
             VALUES ('alice@example.com', ?1, '2026-11', 'push', 'def456', ?2)",
            params![&local_id, now_ms],
        )
        .unwrap();
    }

    // 3. Local sync_all must not overwrite or delete remote rows.
    let re_sync = store
        .sync_all(&LedgerSyncOptions {
            force: true,
            ..sync_opts
        })
        .unwrap();
    assert_eq!(re_sync.newly_synced_dbs, 1);

    // 4. Statusline rolling costs must strictly exclude remote $500.00 and reflect only local $3.00.
    let totals = store
        .record_rolling_costs(&RollingCostInput {
            cursor_id: "local-conv-1",
            cascade_root_id: Some("local-conv-1"),
            live_cost_usd: Some(3.00),
            seven_day_reset_at_ms: None,
            now_ms,
        })
        .unwrap()
        .unwrap();
    assert!(
        (totals.today_usd - 3.00).abs() < 1e-9,
        "today_usd leaked remote cost: {}",
        totals.today_usd
    );
    assert!(
        (totals.week_usd - 3.00).abs() < 1e-9,
        "week_usd leaked remote cost: {}",
        totals.week_usd
    );

    // 5. clear_remote_data() removes all remote rows and pull states while leaving local rows and push states intact.
    let deleted_steps = store.clear_remote_data().unwrap();
    assert_eq!(deleted_steps, 1);

    let conn = Connection::open(&ledger_path).unwrap();
    let count_query = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(
        count_query(&format!(
            "SELECT COUNT(*) FROM telemetry_steps WHERE device_id != '{local_id}'"
        )),
        0
    );
    assert_eq!(
        count_query(&format!(
            "SELECT COUNT(*) FROM telemetry_steps WHERE device_id = '{local_id}'"
        )),
        1
    );
    assert_eq!(
        count_query(&format!(
            "SELECT COUNT(*) FROM ingested_dbs WHERE device_id != '{local_id}'"
        )),
        0
    );
    assert_eq!(
        count_query(&format!(
            "SELECT COUNT(*) FROM ingested_dbs WHERE device_id = '{local_id}'"
        )),
        1
    );
    assert_eq!(
        count_query(&format!(
            "SELECT COUNT(*) FROM sync_devices WHERE device_id != '{local_id}'"
        )),
        0
    );
    assert_eq!(
        count_query(&format!(
            "SELECT COUNT(*) FROM sync_devices WHERE device_id = '{local_id}'"
        )),
        1
    );
    assert_eq!(count_query("SELECT COUNT(*) FROM quota_snapshots"), 0);
    assert_eq!(
        count_query("SELECT COUNT(*) FROM sync_chunk_state WHERE direction = 'pull'"),
        0
    );
    assert_eq!(
        count_query("SELECT COUNT(*) FROM sync_chunk_state WHERE direction = 'push'"),
        1
    );
}

static SYNC_ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvVarGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &Path) -> Self {
        let prev = std::env::var(key).ok();
        unsafe { std::env::set_var(key, value) };
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => unsafe { std::env::set_var(self.key, v) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

fn agy_pulse_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agy-pulse"))
}

fn seed_conv(dir: &Path, cid: &str, ws: &str, ts_ms: i64) {
    write_conv_db(
        &dir.join("conversations").join(format!("{cid}.db")),
        Some(&build_trajectory_blob(ws, "main", ts_ms - 10_000, Some(cid))),
        &[(1, build_step_metadata_blob(ts_ms))],
        &[(
            1,
            build_gen_blob(&[1], "Gemini 4 Carbon (High)", 100_000, 0, 10_000),
        )],
    );
}

#[test]
fn t06_sync_identity_hints_and_last_observed_email() {
    let _lock = SYNC_ENV_LOCK.lock().unwrap();
    let temp = TempDir::new().unwrap();
    let sync_cfg = temp.path().join("agy-pulse-sync.json");
    let _guard = EnvVarGuard::set("AGY_PULSE_SYNC_CONFIG", &sync_cfg);

    assert_eq!(resolve_sync_config_path(), sync_cfg);
    assert_eq!(read_sync_identity_hints(), (None, None));

    // Malformed JSON silently yields (None, None).
    fs::write(&sync_cfg, "{not valid json").unwrap();
    assert_eq!(read_sync_identity_hints(), (None, None));

    // Valid JSON normalizes defaultAccount and strips control chars from deviceName.
    fs::write(
        &sync_cfg,
        r#"{"defaultAccount":"  Alice.Work@Example.COM ","deviceName":"  lab-\u0007ubuntu \n"}"#,
    )
    .unwrap();
    assert_eq!(
        read_sync_identity_hints(),
        (
            Some("alice.work@example.com".to_string()),
            Some("lab-ubuntu".to_string())
        )
    );

    // last_observed_email normalizes and skips redundant writes.
    let ledger_path = temp.path().join("agy-pulse-ledger.sqlite");
    let store = LedgerStore::open_at(&ledger_path).unwrap();
    assert_eq!(store.last_observed_email().unwrap(), None);
    store.set_last_observed_email("  Work@Company.COM ").unwrap();
    assert_eq!(
        store.last_observed_email().unwrap(),
        Some("work@company.com".to_string())
    );
    store.set_last_observed_email("work@company.com").unwrap();
    assert_eq!(
        store.last_observed_email().unwrap(),
        Some("work@company.com".to_string())
    );
    // "unassigned" or empty must not overwrite an observed email.
    store.set_last_observed_email("unassigned").unwrap();
    assert_eq!(
        store.last_observed_email().unwrap(),
        Some("work@company.com".to_string())
    );
}

#[test]
fn t06_dual_account_locking_and_sync_devices_multi_account() {
    let temp = TempDir::new().unwrap();
    let cli_dir = temp.path().join("cli");
    let ledger_path = temp.path().join("agy-pulse-ledger.sqlite");
    let now_ms = local_ms(2026, 11, 26, 15, 0, 0);

    seed_conv(&cli_dir, "conv-work", "file:///work/repo-a", now_ms - 5_000);
    seed_conv(
        &cli_dir,
        "conv-personal",
        "file:///home/personal",
        now_ms - 3_000,
    );
    seed_conv(&cli_dir, "conv-upgrade", "file:///work/repo-b", now_ms - 2_000);

    let mut store = LedgerStore::open_at(&ledger_path).unwrap();
    let local_id = store.local_device_id().unwrap();
    let roots = Some(vec![SourceRoot {
        root: cli_dir.clone(),
        source: LedgerSource::Cli,
    }]);

    // 1. Sync conv-work under work@company.com.
    let r1 = store
        .sync_conversation(
            "conv-work",
            &[],
            &LedgerSyncOptions {
                source_roots: roots.clone(),
                now_ms: Some(now_ms),
                active_account_id: Some("  Work@Company.com ".to_string()),
                device_name: Some("devbox-01".to_string()),
                plan_tier: Some("Enterprise".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(r1.synced_dbs, 1);

    // 2. Switch to personal@gmail.com and sync conv-personal.
    let r2 = store
        .sync_conversation(
            "conv-personal",
            &[],
            &LedgerSyncOptions {
                source_roots: roots.clone(),
                now_ms: Some(now_ms + 1_000),
                active_account_id: Some("Personal@Gmail.com".to_string()),
                device_name: Some("devbox-01".to_string()),
                plan_tier: Some("Google AI Ultra".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(r2.synced_dbs, 1);

    // 3. Re-sync conv-work with personal@gmail.com active + force=true -> must stay locked to work@company.com!
    let r3 = store
        .sync_conversation(
            "conv-work",
            &[],
            &LedgerSyncOptions {
                source_roots: roots.clone(),
                force: true,
                now_ms: Some(now_ms + 2_000),
                active_account_id: Some("personal@gmail.com".to_string()),
                device_name: Some("devbox-01".to_string()),
                plan_tier: Some("Google AI Ultra".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(r3.synced_dbs, 1);

    // Force full scan with defaultAccount set -> locked conversations must NOT be overwritten.
    store
        .sync_all(&LedgerSyncOptions {
            source_roots: roots.clone(),
            force: true,
            now_ms: Some(now_ms + 3_000),
            default_account_id: Some("fallback@example.com".to_string()),
            ..Default::default()
        })
        .unwrap();

    // 4. Sync conv-upgrade initially unassigned (no active/default/last_observed), then upgrade once via active_account_id
    // within throttle window and without DB file modification.
    let mut store2 = LedgerStore::open_at(&temp.path().join("upgrade-ledger.sqlite")).unwrap();
    store2
        .sync_conversation(
            "conv-upgrade",
            &[],
            &LedgerSyncOptions {
                source_roots: roots.clone(),
                now_ms: Some(now_ms),
                ..Default::default()
            },
        )
        .unwrap();
    let upgraded = store2
        .sync_conversation(
            "conv-upgrade",
            &[],
            &LedgerSyncOptions {
                source_roots: roots,
                now_ms: Some(now_ms + 100), // within 15s throttle and unchanged DB
                active_account_id: Some("work@company.com".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(upgraded.synced_dbs, 1);

    let conn = Connection::open(&ledger_path).unwrap();
    let get_acct = |table: &str, cid: &str| -> String {
        conn.query_row(
            &format!("SELECT account_id FROM {table} WHERE conversation_id = ?1"),
            params![cid],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(get_acct("ingested_dbs", "conv-work"), "work@company.com");
    assert_eq!(get_acct("telemetry_steps", "conv-work"), "work@company.com");
    assert_eq!(
        get_acct("ingested_dbs", "conv-personal"),
        "personal@gmail.com"
    );
    assert_eq!(
        get_acct("telemetry_steps", "conv-personal"),
        "personal@gmail.com"
    );

    // Same local_device_id registered under both accounts with independent plan_tiers.
    let dev_accounts: Vec<(String, i64, Option<String>)> = {
        let mut stmt = conn
            .prepare(
                "SELECT account_id, is_local, plan_tier
                 FROM sync_devices
                 WHERE device_id = ?1 AND account_id IN ('work@company.com', 'personal@gmail.com')
                 ORDER BY account_id ASC",
            )
            .unwrap();
        stmt.query_map(params![&local_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap()
        .map(|x| x.unwrap())
        .collect()
    };
    assert_eq!(
        dev_accounts,
        vec![
            (
                "personal@gmail.com".to_string(),
                1,
                Some("Google AI Ultra".to_string())
            ),
            ("work@company.com".to_string(), 1, Some("Enterprise".to_string())),
        ]
    );

    let conn2 = Connection::open(temp.path().join("upgrade-ledger.sqlite")).unwrap();
    let upgraded_db_acct: String = conn2
        .query_row(
            "SELECT account_id FROM ingested_dbs WHERE conversation_id = 'conv-upgrade'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let upgraded_step_acct: String = conn2
        .query_row(
            "SELECT account_id FROM telemetry_steps WHERE conversation_id = 'conv-upgrade'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(upgraded_db_acct, "work@company.com");
    assert_eq!(upgraded_step_acct, "work@company.com");
}

#[test]
fn t06_ide_and_sync_all_account_fallback_chain() {
    let temp = TempDir::new().unwrap();
    let cli_dir = temp.path().join("cli");
    let ide_dir = temp.path().join("ide");
    let ledger_path = temp.path().join("agy-pulse-ledger.sqlite");
    let now_ms = local_ms(2026, 11, 26, 15, 30, 0);

    seed_conv(&ide_dir, "ide-unassigned", "file:///work/ide1", now_ms);
    seed_conv(&ide_dir, "ide-last-obs", "file:///work/ide2", now_ms + 1_000);
    seed_conv(&ide_dir, "ide-default", "file:///work/ide3", now_ms + 2_000);
    seed_conv(&cli_dir, "cli-bg", "file:///work/cli1", now_ms + 3_000);

    let mut store = LedgerStore::open_at(&ledger_path).unwrap();
    let ide_roots = Some(vec![SourceRoot {
        root: ide_dir.clone(),
        source: LedgerSource::Ide,
    }]);

    // 1. IDE conversation via sync_conversation must ignore active_account_id -> unassigned when no default/last_observed.
    store
        .sync_conversation(
            "ide-unassigned",
            &[],
            &LedgerSyncOptions {
                source_roots: ide_roots.clone(),
                now_ms: Some(now_ms),
                active_account_id: Some("cli-active@example.com".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

    // 2. Set last_observed_email -> IDE conversation falls back to last_observed_email (still ignoring active_account_id).
    store.set_last_observed_email("last@example.com").unwrap();
    store
        .sync_conversation(
            "ide-last-obs",
            &[],
            &LedgerSyncOptions {
                source_roots: ide_roots,
                now_ms: Some(now_ms + 1_000),
                active_account_id: Some("cli-active@example.com".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

    // 3. sync_all with default_account_id overrides last_observed_email and upgrades prior unassigned conversations,
    // while keeping locked 'last@example.com' untouched and ignoring active_account_id.
    store
        .sync_all(&LedgerSyncOptions {
            source_roots: Some(vec![
                SourceRoot {
                    root: cli_dir,
                    source: LedgerSource::Cli,
                },
                SourceRoot {
                    root: ide_dir,
                    source: LedgerSource::Ide,
                },
            ]),
            now_ms: Some(now_ms + 2_000),
            active_account_id: Some("cli-active@example.com".to_string()),
            default_account_id: Some("Default.Owner@Example.com".to_string()),
            ..Default::default()
        })
        .unwrap();

    let conn = Connection::open(&ledger_path).unwrap();
    let get_acct = |cid: &str| -> String {
        conn.query_row(
            "SELECT account_id FROM ingested_dbs WHERE conversation_id = ?1",
            params![cid],
            |r| r.get(0),
        )
        .unwrap()
    };
    // Previously unassigned 'ide-unassigned' upgraded during sync_all when default_account_id became available.
    assert_eq!(get_acct("ide-unassigned"), "default.owner@example.com");
    // Previously locked 'ide-last-obs' remained 'last@example.com'.
    assert_eq!(get_acct("ide-last-obs"), "last@example.com");
    assert_eq!(get_acct("ide-default"), "default.owner@example.com");
    assert_eq!(get_acct("cli-bg"), "default.owner@example.com");
}

#[test]
fn t06_quota_snapshots_upsert_and_query_latest_per_account() {
    let temp = TempDir::new().unwrap();
    let ledger_path = temp.path().join("agy-pulse-ledger.sqlite");
    let store = LedgerStore::open_at(&ledger_path).unwrap();
    let now_ms = local_ms(2026, 11, 26, 16, 0, 0);

    let make_quota = |frac: f64| ProviderQuotaSummary {
        google: ProviderQuota {
            five_hour: Some(QuotaWindow {
                remaining_fraction: frac,
                reset_time_ms: Some(now_ms + 3_600_000),
            }),
            weekly: None,
        },
        ..Default::default()
    };

    // Account A across two devices at different timestamps.
    store
        .upsert_quota_snapshot(&QuotaSnapshot {
            account_id: "Alice@Example.com".to_string(),
            device_id: "dev_b".to_string(),
            device_name: "laptop".to_string(),
            plan_tier: Some("Pro".to_string()),
            observed_at_ms: now_ms + 500,
            quota: make_quota(0.45),
        })
        .unwrap();
    store
        .upsert_quota_snapshot(&QuotaSnapshot {
            account_id: "alice@example.com".to_string(),
            device_id: "dev_a".to_string(),
            device_name: "workstation".to_string(),
            plan_tier: Some("Google AI Ultra".to_string()),
            observed_at_ms: now_ms + 2_000,
            quota: make_quota(0.88),
        })
        .unwrap();
    // Stale overwrite for dev_a must be ignored because observed_at_ms is older.
    store
        .upsert_quota_snapshot(&QuotaSnapshot {
            account_id: "alice@example.com".to_string(),
            device_id: "dev_a".to_string(),
            device_name: "workstation".to_string(),
            plan_tier: None,
            observed_at_ms: now_ms + 1_000,
            quota: make_quota(0.11),
        })
        .unwrap();

    // Account B with equal observed_at_ms -> device_id ASC tie-breaker picks dev_01 over dev_02.
    store
        .upsert_quota_snapshot(&QuotaSnapshot {
            account_id: "bob@example.com".to_string(),
            device_id: "dev_02".to_string(),
            device_name: "bob-2".to_string(),
            plan_tier: None,
            observed_at_ms: now_ms + 3_000,
            quota: make_quota(0.20),
        })
        .unwrap();
    store
        .upsert_quota_snapshot(&QuotaSnapshot {
            account_id: "bob@example.com".to_string(),
            device_id: "dev_01".to_string(),
            device_name: "bob-1".to_string(),
            plan_tier: Some("Pro".to_string()),
            observed_at_ms: now_ms + 3_000,
            quota: make_quota(0.77),
        })
        .unwrap();

    // Insert corrupted quota_json row with newer timestamp -> must be gracefully skipped.
    {
        let conn = Connection::open(&ledger_path).unwrap();
        conn.execute(
            "INSERT INTO quota_snapshots (account_id, device_id, device_name, plan_tier, observed_at_ms, quota_json)
             VALUES ('bob@example.com', 'dev_00_corrupt', 'corrupt', NULL, ?1, '{not-json')",
            params![now_ms + 9_999],
        )
        .unwrap();
    }

    let latest = store.query_latest_account_quotas().unwrap();
    assert_eq!(latest.len(), 2);
    assert_eq!(latest[0].account_id, "alice@example.com");
    assert_eq!(latest[0].device_id, "dev_a");
    assert_eq!(latest[0].observed_at_ms, now_ms + 2_000);
    assert!(
        (latest[0]
            .quota
            .google
            .five_hour
            .as_ref()
            .unwrap()
            .remaining_fraction
            - 0.88)
            .abs()
            < 1e-9
    );

    assert_eq!(latest[1].account_id, "bob@example.com");
    assert_eq!(latest[1].device_id, "dev_01");
    assert_eq!(latest[1].observed_at_ms, now_ms + 3_000);
}

#[test]
fn t06_statusline_e2e_persists_last_observed_email_and_quota_snapshot() {
    let temp = TempDir::new().unwrap();
    let home_dir = temp.path().join("home");
    let data_dir = temp.path().join("data");
    let global_cfg_dir = temp.path().join("global_cfg");
    let xdg_cfg_dir = temp.path().join("xdg_cfg");
    let app_data_dir = temp.path().join("app_data");
    let local_app_data_dir = temp.path().join("local_app_data");
    let workspace_dir = temp.path().join("workspace");
    for d in [
        &home_dir,
        &data_dir,
        &global_cfg_dir,
        &xdg_cfg_dir,
        &app_data_dir,
        &local_app_data_dir,
        &workspace_dir,
    ] {
        fs::create_dir_all(d).unwrap();
    }

    let sync_cfg = temp.path().join("sync-cfg.json");
    fs::write(&sync_cfg, r#"{"deviceName":"e2e-linux-box"}"#).unwrap();

    let conv_id = "conv-e2e-quota-1";
    let now_ms = chrono::Utc::now().timestamp_millis();
    seed_conv(&data_dir, conv_id, "file:///work/e2e", now_ms - 5_000);

    let payload = serde_json::json!({
        "conversation_id": conv_id,
        "email": "StatusLine.User@Example.COM",
        "plan_tier": "Google AI Ultra",
        "model": { "id": "claude-opus-4-6-thinking", "display_name": "Claude Opus 4.6" },
        "workspace": { "current_dir": workspace_dir.to_string_lossy() },
        "quota": {
            "gemini-5h": { "remaining_fraction": 0.91, "reset_in_seconds": 3600 },
            "claude-weekly": { "remaining_fraction": 0.64, "reset_in_seconds": 86400 }
        }
    })
    .to_string();

    let mut child = Command::new(agy_pulse_bin())
        .current_dir(&workspace_dir)
        .env("HOME", &home_dir)
        .env("USERPROFILE", &home_dir)
        .env("XDG_DATA_HOME", home_dir.join(".local/share"))
        .env("XDG_CONFIG_HOME", &xdg_cfg_dir)
        .env("APPDATA", &app_data_dir)
        .env("AppData", &app_data_dir)
        .env("LOCALAPPDATA", &local_app_data_dir)
        .env("LocalAppData", &local_app_data_dir)
        .env("AGY_PULSE_DATA_DIR", &data_dir)
        .env("AGY_PULSE_GLOBAL_CONFIG_DIR", &global_cfg_dir)
        .env("AGY_PULSE_SYNC_CONFIG", &sync_cfg)
        .env("AGY_PULSE_NO_DASHBOARD_AUTOSTART", "1")
        .env("AGY_PULSE_DISABLE_UPDATE_CHECK", "1")
        .env("AGY_PULSE_DISABLE_BG_LEDGER_SYNC", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0));

    let ledger_path = data_dir.join("agy-pulse-ledger.sqlite");
    let store = LedgerStore::open_at(&ledger_path).unwrap();
    assert_eq!(
        store.last_observed_email().unwrap(),
        Some("statusline.user@example.com".to_string())
    );
    let quotas = store.query_latest_account_quotas().unwrap();
    assert_eq!(quotas.len(), 1);
    assert_eq!(quotas[0].account_id, "statusline.user@example.com");
    assert_eq!(quotas[0].device_name, "e2e-linux-box");
    assert_eq!(quotas[0].plan_tier.as_deref(), Some("Google AI Ultra"));
    assert!(
        (quotas[0]
            .quota
            .google
            .five_hour
            .as_ref()
            .unwrap()
            .remaining_fraction
            - 0.91)
            .abs()
            < 1e-9
    );

    let conn = Connection::open(&ledger_path).unwrap();
    let conv_acct: String = conn
        .query_row(
            "SELECT account_id FROM ingested_dbs WHERE conversation_id = ?1",
            params![conv_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(conv_acct, "statusline.user@example.com");
}

#[test]
fn t06_cli_account_assign_dry_run_workspace_filter_and_validation() {
    let temp = TempDir::new().unwrap();
    let home_dir = temp.path().join("home");
    let data_dir = temp.path().join("data");
    let global_cfg_dir = temp.path().join("global_cfg");
    let xdg_cfg_dir = temp.path().join("xdg_cfg");
    let app_data_dir = temp.path().join("app_data");
    let local_app_data_dir = temp.path().join("local_app_data");
    let workspace_dir = temp.path().join("workspace");
    for d in [
        &home_dir,
        &data_dir,
        &global_cfg_dir,
        &xdg_cfg_dir,
        &app_data_dir,
        &local_app_data_dir,
        &workspace_dir,
    ] {
        fs::create_dir_all(d).unwrap();
    }

    let now_ms = local_ms(2026, 11, 26, 17, 0, 0);
    // Use literal '%' in workspace path to ensure Rust substring match does not behave like SQL LIKE wildcard.
    seed_conv(&data_dir, "unassigned-ws-a", "/work/100%_alpha", now_ms);
    seed_conv(&data_dir, "unassigned-ws-b", "/work/1000_beta", now_ms + 1_000);
    seed_conv(&data_dir, "locked-existing", "/work/locked", now_ms + 2_000);

    let ledger_path = data_dir.join("agy-pulse-ledger.sqlite");
    let mut store = LedgerStore::open_at(&ledger_path).unwrap();
    let local_id = store.local_device_id().unwrap();
    store
        .sync_all(&LedgerSyncOptions {
            source_roots: Some(vec![SourceRoot {
                root: data_dir.clone(),
                source: LedgerSource::Cli,
            }]),
            now_ms: Some(now_ms),
            ..Default::default()
        })
        .unwrap();

    // Lock one local conversation to bob@company.com and insert a remote unassigned row.
    {
        let conn = Connection::open(&ledger_path).unwrap();
        conn.execute(
            "UPDATE ingested_dbs SET account_id = 'bob@company.com' WHERE conversation_id = 'locked-existing'",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE telemetry_steps SET account_id = 'bob@company.com' WHERE conversation_id = 'locked-existing'",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ingested_dbs (
               conversation_id, db_path, sig, last_synced_ms, workspace, branch,
               created_at_ms, cascade_id, parent_conversation_id, root_conversation_id,
               db_mtime_ms, source, account_id, device_id
             ) VALUES (
               'remote-unassigned', '', 'remote', ?1, '/work/100%_alpha', NULL,
               ?1, 'remote-unassigned', NULL, NULL, ?1, 'cli', 'unassigned', 'dev_remote_xyz'
             )",
            params![now_ms],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO telemetry_steps (
               step_uid, conversation_id, gen_idx, entry_idx, step_index, ts_ms,
               model, raw_model, input_tokens, cache_read_tokens, cache_write_tokens, output_tokens,
               cost_usd, stream_sec, ttft_sec, valid_stream, valid_ttft,
               parent_conversation_id, root_conversation_id, workspace, branch, source,
               account_id, device_id
             ) VALUES (
               'remote-unassigned:1:0', 'remote-unassigned', 1, 0, 1, ?1,
               'Carbon', 'Gemini 4 Carbon (High)', 100, 0, 0, 10,
               0.01, 1.0, 0.2, 1, 1,
               NULL, NULL, NULL, NULL, 'cli', 'unassigned', 'dev_remote_xyz'
             )",
            params![now_ms],
        )
        .unwrap();
    }

    // Invalid email via store API returns InvalidAccountEmail.
    assert!(matches!(
        store.assign_unassigned_account("not-an-email", None, false),
        Err(LedgerError::InvalidAccountEmail(_))
    ));
    drop(store);

    let run_cli = |args: &[&str]| {
        Command::new(agy_pulse_bin())
            .args(args)
            .current_dir(&workspace_dir)
            .env("HOME", &home_dir)
            .env("USERPROFILE", &home_dir)
            .env("XDG_DATA_HOME", home_dir.join(".local/share"))
            .env("XDG_CONFIG_HOME", &xdg_cfg_dir)
            .env("APPDATA", &app_data_dir)
            .env("AppData", &app_data_dir)
            .env("LOCALAPPDATA", &local_app_data_dir)
            .env("LocalAppData", &local_app_data_dir)
            .env("AGY_PULSE_DATA_DIR", &data_dir)
            .env("AGY_PULSE_GLOBAL_CONFIG_DIR", &global_cfg_dir)
            .env("AGY_PULSE_NO_DASHBOARD_AUTOSTART", "1")
            .env("AGY_PULSE_DISABLE_UPDATE_CHECK", "1")
            .output()
            .unwrap()
    };

    // 1. Help & invalid invocations.
    let help_out = run_cli(&["account", "assign", "--help"]);
    assert_eq!(help_out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&help_out.stdout).contains("--workspace"));

    for bad_args in [
        &["account", "assign", "invalid-email"][..],
        &["account", "assign", "@example.com"][..],
        &["account", "assign", "alice@"][..],
        &["account", "assign", "alice@gmail.com", "--bogus"][..],
        &["account", "assign"][..],
    ] {
        let bad = run_cli(bad_args);
        assert_eq!(bad.status.code(), Some(2), "expected exit 2 for {bad_args:?}");
        assert!(!bad.stderr.is_empty());
    }

    // 2. Dry-run must NOT modify the database.
    let dry = run_cli(&["account", "assign", "Alice@Gmail.com", "--dry-run"]);
    assert_eq!(dry.status.code(), Some(0));
    let dry_stdout = String::from_utf8_lossy(&dry.stdout);
    assert!(dry_stdout.contains("2 conversation(s)"));
    assert!(dry_stdout.contains("2 step(s)"));
    assert!(dry_stdout.contains("alice@gmail.com"));

    let check_acct = |cid: &str| -> String {
        let conn = Connection::open(&ledger_path).unwrap();
        conn.query_row(
            "SELECT account_id FROM ingested_dbs WHERE conversation_id = ?1",
            params![cid],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(check_acct("unassigned-ws-a"), "unassigned");
    assert_eq!(check_acct("unassigned-ws-b"), "unassigned");

    // 3. --workspace "100%_" matches ONLY unassigned-ws-a (not unassigned-ws-b!).
    let ws_assign = run_cli(&[
        "account",
        "assign",
        "Alice@Gmail.com",
        "--workspace",
        "100%_",
    ]);
    assert_eq!(ws_assign.status.code(), Some(0));
    assert_eq!(check_acct("unassigned-ws-a"), "alice@gmail.com");
    assert_eq!(check_acct("unassigned-ws-b"), "unassigned");
    assert_eq!(check_acct("locked-existing"), "bob@company.com");
    assert_eq!(check_acct("remote-unassigned"), "unassigned");

    // 4. --all-unassigned assigns remaining local unassigned (unassigned-ws-b), still leaving locked & remote untouched.
    let all_assign = run_cli(&["account", "assign", "alice@gmail.com", "--all-unassigned"]);
    assert_eq!(all_assign.status.code(), Some(0));
    assert_eq!(check_acct("unassigned-ws-b"), "alice@gmail.com");
    assert_eq!(check_acct("locked-existing"), "bob@company.com");
    assert_eq!(check_acct("remote-unassigned"), "unassigned");

    let conn = Connection::open(&ledger_path).unwrap();
    let remote_step_acct: String = conn
        .query_row(
            "SELECT account_id FROM telemetry_steps WHERE step_uid = 'remote-unassigned:1:0'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(remote_step_acct, "unassigned");

    let locked_step_acct: String = conn
        .query_row(
            "SELECT account_id FROM telemetry_steps WHERE conversation_id = 'locked-existing'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(locked_step_acct, "bob@company.com");

    let alice_dev_registered: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sync_devices WHERE account_id = 'alice@gmail.com' AND device_id = ?1 AND is_local = 1",
            params![&local_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(alice_dev_registered, 1);
}
