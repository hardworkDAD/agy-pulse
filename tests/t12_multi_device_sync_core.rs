#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{TimeZone, Utc};
use flate2::{Compression, GzBuilder};
use rusqlite::{Connection, params};
use tempfile::TempDir;

use agy_pulse::domain::{
    ProviderQuota, ProviderQuotaSummary, QuotaSnapshot, QuotaWindow, UNASSIGNED_ACCOUNT_ID,
};
use agy_pulse::ledger::{
    LedgerSource, LedgerStore, LedgerSyncOptions, RollingCostInput, SourceRoot,
};
use agy_pulse::sync::{
    ChunkRecord, DeviceManifest, LocalDirRemote, RemoteConfig, SyncConfig, SyncError,
    SyncOutcomeKind, SyncRemote, build_remote, decode_account_key, decode_chunk_gz,
    encode_account_key, load_sync_config, load_sync_config_from, prune_device, pull, push,
    save_sync_config_to, sync_both, validate_relative_key,
};

static ENV_MUTEX: Mutex<()> = Mutex::new(());

struct EnvGuard {
    saved: Vec<(String, Option<std::ffi::OsString>)>,
}

impl EnvGuard {
    fn new() -> Self {
        Self { saved: Vec::new() }
    }

    fn set(&mut self, key: &str, val: &str) {
        if !self.saved.iter().any(|(k, _)| k == key) {
            self.saved.push((key.to_string(), env::var_os(key)));
        }
        // SAFETY: guarded by `ENV_MUTEX` in tests.
        unsafe { env::set_var(key, val) };
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, orig) in self.saved.iter().rev() {
            match orig {
                Some(v) => unsafe { env::set_var(key, v) },
                None => unsafe { env::remove_var(key) },
            }
        }
    }
}

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

fn pb_duration_sec(field_num: u32, sec: u64) -> Vec<u8> {
    let inner = if sec > 0 { pb_varint(1, sec) } else { Vec::new() };
    pb_bytes(field_num, &inner)
}

fn seed_conv(
    root: &Path,
    conv_id: &str,
    workspace: &str,
    branch: &str,
    ts_ms: i64,
    input_tokens: u64,
    output_tokens: u64,
) {
    let db_path = root.join("conversations").join(format!("{conv_id}.db"));
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let conn = Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS steps (idx INTEGER PRIMARY KEY, metadata BLOB);
         CREATE TABLE IF NOT EXISTS gen_metadata (idx INTEGER PRIMARY KEY, data BLOB);
         CREATE TABLE IF NOT EXISTS trajectory_metadata_blob (id TEXT PRIMARY KEY, data BLOB);",
    )
    .unwrap();

    let step_meta = pb_timestamp_ms(1, ts_ms);
    conn.execute(
        "INSERT INTO steps (idx, metadata) VALUES (?1, ?2)",
        params![1i64, step_meta],
    )
    .unwrap();

    let mut gen_blob = pb_varint(2, 1);
    let mut f1 = pb_str(21, "Claude Sonnet 4.5");
    let mut usage = pb_varint(2, input_tokens);
    usage.extend(pb_varint(3, output_tokens));
    f1.extend(pb_bytes(4, &usage));
    f1.extend(pb_duration_sec(11, 1));
    f1.extend(pb_duration_sec(12, 5));
    gen_blob.extend(pb_bytes(1, &f1));
    conn.execute(
        "INSERT INTO gen_metadata (idx, data) VALUES (?1, ?2)",
        params![1i64, gen_blob],
    )
    .unwrap();

    let mut ws = pb_str(1, workspace);
    ws.extend(pb_str(4, branch));
    let mut traj = pb_bytes(1, &ws);
    traj.extend(pb_timestamp_ms(2, ts_ms));
    traj.extend(pb_str(6, conv_id));
    conn.execute(
        "INSERT INTO trajectory_metadata_blob (id, data) VALUES ('main', ?1)",
        params![traj],
    )
    .unwrap();
}

struct IsolatedMachine {
    _temp: TempDir,
    cli_root: PathBuf,
    ide_root: PathBuf,
    ledger_path: PathBuf,
}

impl IsolatedMachine {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let cli_root = temp.path().join("cli");
        let ide_root = temp.path().join("ide");
        fs::create_dir_all(&cli_root).unwrap();
        fs::create_dir_all(&ide_root).unwrap();
        let ledger_path = cli_root.join("agy-pulse-ledger.sqlite");
        Self {
            _temp: temp,
            cli_root,
            ide_root,
            ledger_path,
        }
    }

    fn open_store(&self) -> LedgerStore {
        LedgerStore::open_at(&self.ledger_path).unwrap()
    }

    fn sync_config(&self, remote_dir: &Path, device_name: &str, prefix: &str) -> SyncConfig {
        SyncConfig {
            enabled: true,
            device_id: None,
            device_name: Some(device_name.to_string()),
            default_account: None,
            push_interval_sec: 10,
            prefix: prefix.to_string(),
            remote: RemoteConfig::LocalDir {
                path: remote_dir.to_string_lossy().into_owned(),
            },
        }
    }
}

fn utc_ms(y: i32, m: u32, d: u32, hh: u32, mm: u32) -> i64 {
    Utc.with_ymd_and_hms(y, m, d, hh, mm, 0)
        .single()
        .unwrap()
        .timestamp_millis()
}

fn sample_quota(frac: f64, reset_ms: i64) -> ProviderQuotaSummary {
    ProviderQuotaSummary {
        google: ProviderQuota {
            five_hour: Some(QuotaWindow {
                remaining_fraction: frac,
                reset_time_ms: Some(reset_ms),
            }),
            weekly: None,
        },
        ..Default::default()
    }
}

#[test]
fn t12_two_machine_multi_account_sync_privacy_and_statusline_isolation() {
    let remote_dir = TempDir::new().unwrap();
    let remote = LocalDirRemote::new(remote_dir.path().to_path_buf());
    let probe = remote.probe().unwrap();
    assert_eq!(probe.backend, "local-dir");

    let machine_a = IsolatedMachine::new();
    let machine_b = IsolatedMachine::new();
    let ts_oct = utc_ms(2026, 10, 15, 12, 0);
    let ts_nov = utc_ms(2026, 11, 20, 9, 30);

    // Seed Machine A with two accounts + sensitive workspace/branch + quota snapshots.
    seed_conv(
        &machine_a.cli_root,
        "conv-a-work",
        "file:///secret/company/top-secret-repo",
        "feat/confidential-launch",
        ts_oct,
        2_000,
        500,
    );
    seed_conv(
        &machine_a.ide_root,
        "conv-a-personal",
        "file:///home/alice/private-diary-app",
        "personal/wip-branch",
        ts_nov,
        1_500,
        300,
    );

    let mut store_a = machine_a.open_store();
    let dev_a = store_a.local_device_id().unwrap();
    store_a
        .sync_conversation(
            "conv-a-work",
            &[],
            &LedgerSyncOptions {
                source_roots: Some(vec![SourceRoot {
                    root: machine_a.cli_root.clone(),
                    source: LedgerSource::Cli,
                }]),
                now_ms: Some(ts_oct + 1_000),
                active_account_id: Some("work@ex.com".to_string()),
                device_name: Some("laptop-a".to_string()),
                plan_tier: Some("Enterprise".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
    store_a
        .sync_all(&LedgerSyncOptions {
            source_roots: Some(vec![SourceRoot {
                root: machine_a.ide_root.clone(),
                source: LedgerSource::Ide,
            }]),
            now_ms: Some(ts_nov + 1_000),
            default_account_id: Some("personal@ex.com".to_string()),
            device_name: Some("laptop-a".to_string()),
            plan_tier: Some("Google AI Ultra".to_string()),
            ..Default::default()
        })
        .unwrap();

    store_a
        .upsert_quota_snapshot(&QuotaSnapshot {
            account_id: "work@ex.com".to_string(),
            device_id: dev_a.clone(),
            device_name: "laptop-a".to_string(),
            plan_tier: Some("Enterprise".to_string()),
            observed_at_ms: ts_nov + 2_000,
            quota: sample_quota(0.84, ts_nov + 3_600_000),
        })
        .unwrap();
    store_a
        .upsert_quota_snapshot(&QuotaSnapshot {
            account_id: "personal@ex.com".to_string(),
            device_id: dev_a.clone(),
            device_name: "laptop-a".to_string(),
            plan_tier: Some("Google AI Ultra".to_string()),
            observed_at_ms: ts_nov + 3_000,
            quota: sample_quota(0.41, ts_nov + 7_200_000),
        })
        .unwrap();

    let cfg_a = machine_a.sync_config(remote_dir.path(), "laptop-a", "/agy-pulse-sync/");
    let push_out = push(&store_a, &remote, &cfg_a, false).unwrap();
    assert_eq!(push_out.kind, SyncOutcomeKind::Completed);
    assert!(!push_out.locked);
    assert_eq!(push_out.accounts_processed, 2);
    assert_eq!(push_out.chunks_uploaded, 2);
    assert_eq!(push_out.manifests_uploaded, 2);

    // Inspect remote objects directly: zero workspace, branch, db_path, or sig leaks.
    let all_keys = remote.list("agy-pulse-sync/accounts").unwrap();
    assert_eq!(all_keys.len(), 4);
    for key in &all_keys {
        if key.ends_with(".jsonl.gz") {
            let gz = remote.get(key).unwrap().unwrap();
            let records = decode_chunk_gz(&gz).unwrap();
            assert!(!records.is_empty());
            let mut decoder = flate2::read::GzDecoder::new(&gz[..]);
            let mut jsonl = String::new();
            std::io::Read::read_to_string(&mut decoder, &mut jsonl).unwrap();
            for forbidden in [
                "workspace",
                "branch",
                "db_path",
                "sig",
                "top-secret-repo",
                "confidential-launch",
                "private-diary-app",
                "wip-branch",
            ] {
                assert!(
                    !jsonl.contains(forbidden),
                    "remote chunk {key} leaked '{forbidden}': {jsonl}"
                );
            }
            for rec in records {
                match rec {
                    ChunkRecord::Conv { source, .. } | ChunkRecord::Step { source, .. } => {
                        assert!(source == "cli" || source == "ide");
                    }
                }
            }
        }
    }

    // Dry-run pull on Machine B must leave Machine B's SQLite untouched.
    let mut store_b = machine_b.open_store();
    let cfg_b = machine_b.sync_config(remote_dir.path(), "desktop-b", "agy-pulse-sync");
    let dry_pull = pull(&store_b, &remote, &cfg_b, true).unwrap();
    assert_eq!(dry_pull.kind, SyncOutcomeKind::DryRun);
    assert!(dry_pull.dry_run);
    assert_eq!(dry_pull.devices_synced, 2);
    assert_eq!(dry_pull.chunks_downloaded, 2);
    assert_eq!(dry_pull.steps_imported, 0);
    assert!(store_b.query_latest_account_quotas().unwrap().is_empty());

    // Real pull on Machine B merges both accounts cleanly.
    let pull_out = pull(&store_b, &remote, &cfg_b, false).unwrap();
    assert_eq!(pull_out.kind, SyncOutcomeKind::Completed);
    assert_eq!(pull_out.accounts_processed, 2);
    assert_eq!(pull_out.devices_synced, 2);
    assert_eq!(pull_out.chunks_downloaded, 2);
    assert_eq!(pull_out.steps_imported, 2);

    let conn_b = Connection::open(&machine_b.ledger_path).unwrap();
    let mut conv_stmt = conn_b
        .prepare(
            "SELECT conversation_id, account_id, device_id, db_path, sig, workspace, branch
             FROM ingested_dbs
             ORDER BY conversation_id ASC",
        )
        .unwrap();
    type ConvRow = (
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
    );
    let conv_rows: Vec<ConvRow> = conv_stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(conv_rows.len(), 2);
    assert_eq!(conv_rows[0].0, "conv-a-personal");
    assert_eq!(conv_rows[0].1, "personal@ex.com");
    assert_eq!(conv_rows[0].2, dev_a);
    assert_eq!(conv_rows[0].3, "");
    assert_eq!(conv_rows[0].4, "remote");
    assert_eq!(conv_rows[0].5, None);
    assert_eq!(conv_rows[0].6, None);

    assert_eq!(conv_rows[1].0, "conv-a-work");
    assert_eq!(conv_rows[1].1, "work@ex.com");
    assert_eq!(conv_rows[1].2, dev_a);
    assert_eq!(conv_rows[1].5, None);
    assert_eq!(conv_rows[1].6, None);

    let step_leaks: i64 = conn_b
        .query_row(
            "SELECT COUNT(*) FROM telemetry_steps WHERE workspace IS NOT NULL OR branch IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(step_leaks, 0);

    let quotas_b = store_b.query_latest_account_quotas().unwrap();
    assert_eq!(quotas_b.len(), 2);
    assert_eq!(quotas_b[0].account_id, "personal@ex.com");
    assert_eq!(quotas_b[1].account_id, "work@ex.com");

    // Statusline rolling costs on Machine B MUST NOT leak Machine A's spend.
    let rolling_b = store_b
        .record_rolling_costs(&RollingCostInput {
            cursor_id: "b-local-live",
            cascade_root_id: None,
            live_cost_usd: None,
            seven_day_reset_at_ms: None,
            now_ms: ts_nov + 5_000,
        })
        .unwrap();
    assert_eq!(rolling_b, None);
}

#[test]
fn t12_concurrent_same_account_push_and_sha256_zero_redundancy_skip() {
    let remote_dir = TempDir::new().unwrap();
    let remote = LocalDirRemote::new(remote_dir.path().to_path_buf());
    let machine_a = IsolatedMachine::new();
    let machine_b = IsolatedMachine::new();
    let ts = utc_ms(2026, 11, 10, 14, 0);

    seed_conv(
        &machine_a.cli_root,
        "shared-conv-a",
        "file:///a",
        "main",
        ts,
        1_000,
        200,
    );
    seed_conv(
        &machine_b.cli_root,
        "shared-conv-b",
        "file:///b",
        "main",
        ts + 5_000,
        1_200,
        250,
    );

    let mut store_a = machine_a.open_store();
    let mut store_b = machine_b.open_store();
    store_a
        .sync_all(&LedgerSyncOptions {
            source_roots: Some(vec![SourceRoot {
                root: machine_a.cli_root.clone(),
                source: LedgerSource::Cli,
            }]),
            now_ms: Some(ts),
            default_account_id: Some("shared@ex.com".to_string()),
            device_name: Some("box-a".to_string()),
            ..Default::default()
        })
        .unwrap();
    store_b
        .sync_all(&LedgerSyncOptions {
            source_roots: Some(vec![SourceRoot {
                root: machine_b.cli_root.clone(),
                source: LedgerSource::Cli,
            }]),
            now_ms: Some(ts + 5_000),
            default_account_id: Some("shared@ex.com".to_string()),
            device_name: Some("box-b".to_string()),
            ..Default::default()
        })
        .unwrap();

    let cfg_a = machine_a.sync_config(remote_dir.path(), "box-a", "agy-pulse-sync");
    let cfg_b = machine_b.sync_config(remote_dir.path(), "box-b", "agy-pulse-sync");
    let dev_a = store_a.local_device_id().unwrap();
    let dev_b = store_b.local_device_id().unwrap();
    assert_ne!(dev_a, dev_b);

    // Concurrent pushes to the same account from two threads never overwrite each other.
    let remote_dir_path = remote_dir.path().to_path_buf();
    let ledger_a_path = machine_a.ledger_path.clone();
    let ledger_b_path = machine_b.ledger_path.clone();
    let cfg_a_c = cfg_a.clone();
    let cfg_b_c = cfg_b.clone();

    let t1 = std::thread::spawn({
        let r_dir = remote_dir_path.clone();
        move || {
            let s = LedgerStore::open_at(&ledger_a_path).unwrap();
            let r = LocalDirRemote::new(r_dir);
            push(&s, &r, &cfg_a_c, false).unwrap()
        }
    });
    let t2 = std::thread::spawn(move || {
        let s = LedgerStore::open_at(&ledger_b_path).unwrap();
        let r = LocalDirRemote::new(remote_dir_path);
        push(&s, &r, &cfg_b_c, false).unwrap()
    });
    let out_a = t1.join().unwrap();
    let out_b = t2.join().unwrap();
    assert_eq!(out_a.chunks_uploaded, 1);
    assert_eq!(out_b.chunks_uploaded, 1);

    // Both devices' manifests exist independently under shared@ex.com.
    let keys = remote.list("agy-pulse-sync/accounts/shared@ex.com/devices").unwrap();
    assert_eq!(keys.len(), 4);
    assert!(keys.iter().any(|k| k.contains(&format!("/{dev_a}/manifest.json"))));
    assert!(keys.iter().any(|k| k.contains(&format!("/{dev_b}/manifest.json"))));

    // Repeat Push with no changes -> 0 chunks uploaded, 0 manifests uploaded, 1 chunk skipped.
    let push_again = push(&store_a, &remote, &cfg_a, false).unwrap();
    assert_eq!(push_again.chunks_uploaded, 0);
    assert_eq!(push_again.manifests_uploaded, 0);
    assert_eq!(push_again.chunks_skipped, 1);

    // Machine B pulls Machine A's data once, then repeats pull -> 0 downloaded, 1 skipped.
    let pull_1 = pull(&store_b, &remote, &cfg_b, false).unwrap();
    assert_eq!(pull_1.chunks_downloaded, 1);
    assert_eq!(pull_1.steps_imported, 1);
    let pull_2 = pull(&store_b, &remote, &cfg_b, false).unwrap();
    assert_eq!(pull_2.chunks_downloaded, 0);
    assert_eq!(pull_2.steps_imported, 0);
    assert_eq!(pull_2.chunks_skipped, 1);

    // Update only device_name on Machine A -> chunk is skipped, manifest IS updated, Machine B picks up rename.
    let cfg_a_renamed = machine_a.sync_config(remote_dir.path(), "box-a-renamed", "agy-pulse-sync");
    let push_rename = push(&store_a, &remote, &cfg_a_renamed, false).unwrap();
    assert_eq!(push_rename.chunks_uploaded, 0);
    assert_eq!(push_rename.chunks_skipped, 1);
    assert_eq!(push_rename.manifests_uploaded, 1);

    let pull_rename = pull(&store_b, &remote, &cfg_b, false).unwrap();
    assert_eq!(pull_rename.chunks_downloaded, 0);
    assert_eq!(pull_rename.chunks_skipped, 1);
    let conn_b = Connection::open(&machine_b.ledger_path).unwrap();
    let dev_a_name: String = conn_b
        .query_row(
            "SELECT device_name FROM sync_devices WHERE account_id = 'shared@ex.com' AND device_id = ?1",
            params![&dev_a],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(dev_a_name, "box-a-renamed");
}

#[test]
fn t12_remote_fingerprint_switch_auto_clears_remote_cache_preserving_local() {
    let remote_1_dir = TempDir::new().unwrap();
    let remote_2_dir = TempDir::new().unwrap();
    let remote_1 = LocalDirRemote::new(remote_1_dir.path().to_path_buf());
    let remote_2 = LocalDirRemote::new(remote_2_dir.path().to_path_buf());

    let machine_a = IsolatedMachine::new();
    let machine_b = IsolatedMachine::new();
    let ts = utc_ms(2026, 11, 21, 10, 0);

    seed_conv(&machine_a.cli_root, "conv-a", "file:///a", "main", ts, 500, 100);
    seed_conv(&machine_b.cli_root, "conv-b", "file:///b", "main", ts, 700, 150);

    let mut store_a = machine_a.open_store();
    let mut store_b = machine_b.open_store();
    store_a
        .sync_all(&LedgerSyncOptions {
            source_roots: Some(vec![SourceRoot {
                root: machine_a.cli_root.clone(),
                source: LedgerSource::Cli,
            }]),
            now_ms: Some(ts),
            default_account_id: Some("user@ex.com".to_string()),
            ..Default::default()
        })
        .unwrap();
    store_b
        .sync_all(&LedgerSyncOptions {
            source_roots: Some(vec![SourceRoot {
                root: machine_b.cli_root.clone(),
                source: LedgerSource::Cli,
            }]),
            now_ms: Some(ts),
            default_account_id: Some("user@ex.com".to_string()),
            ..Default::default()
        })
        .unwrap();

    let cfg_a_1 = machine_a.sync_config(remote_1_dir.path(), "a", "agy-pulse-sync");
    let cfg_b_1 = machine_b.sync_config(remote_1_dir.path(), "b", "agy-pulse-sync");
    push(&store_a, &remote_1, &cfg_a_1, false).unwrap();
    let sync1 = sync_both(&store_b, &remote_1, &cfg_b_1, false).unwrap();
    assert!(!sync1.remote_cache_cleared);
    assert_eq!(sync1.steps_imported, 1);
    assert_eq!(sync1.chunks_uploaded, 1);

    // Switch Machine B to Remote 2 (different directory -> different fingerprint).
    let cfg_b_2 = machine_b.sync_config(remote_2_dir.path(), "b", "agy-pulse-sync");
    assert_ne!(cfg_b_1.remote_fingerprint(), cfg_b_2.remote_fingerprint());

    let sync2 = sync_both(&store_b, &remote_2, &cfg_b_2, false).unwrap();
    assert!(sync2.remote_cache_cleared);
    // Switching remote also clears push state so local chunks are pushed to the new remote!
    assert_eq!(sync2.chunks_uploaded, 1);

    let dev_a = store_a.local_device_id().unwrap();
    let dev_b = store_b.local_device_id().unwrap();
    let conn_b = Connection::open(&machine_b.ledger_path).unwrap();
    let remote_steps_left: i64 = conn_b
        .query_row(
            "SELECT COUNT(*) FROM telemetry_steps WHERE device_id = ?1",
            params![&dev_a],
            |r| r.get(0),
        )
        .unwrap();
    let local_steps_left: i64 = conn_b
        .query_row(
            "SELECT COUNT(*) FROM telemetry_steps WHERE device_id = ?1",
            params![&dev_b],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(remote_steps_left, 0);
    assert_eq!(local_steps_left, 1);
}

#[test]
fn t12_prune_device_protects_local_and_cleans_remote_and_cache() {
    let remote_dir = TempDir::new().unwrap();
    let remote = LocalDirRemote::new(remote_dir.path().to_path_buf());
    let machine_a = IsolatedMachine::new();
    let machine_b = IsolatedMachine::new();
    let ts = utc_ms(2026, 11, 22, 8, 0);

    seed_conv(&machine_a.cli_root, "a-work", "file:///w", "main", ts, 400, 80);
    seed_conv(&machine_a.ide_root, "a-pers", "file:///p", "main", ts, 300, 60);
    seed_conv(&machine_b.cli_root, "b-work", "file:///wb", "main", ts, 200, 40);

    let mut store_a = machine_a.open_store();
    let mut store_b = machine_b.open_store();
    let dev_a = store_a.local_device_id().unwrap();
    let dev_b = store_b.local_device_id().unwrap();

    store_a
        .sync_conversation(
            "a-work",
            &[],
            &LedgerSyncOptions {
                source_roots: Some(vec![SourceRoot {
                    root: machine_a.cli_root.clone(),
                    source: LedgerSource::Cli,
                }]),
                now_ms: Some(ts),
                active_account_id: Some("work@ex.com".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
    store_a
        .sync_all(&LedgerSyncOptions {
            source_roots: Some(vec![SourceRoot {
                root: machine_a.ide_root.clone(),
                source: LedgerSource::Ide,
            }]),
            now_ms: Some(ts),
            default_account_id: Some("personal@ex.com".to_string()),
            ..Default::default()
        })
        .unwrap();
    store_b
        .sync_all(&LedgerSyncOptions {
            source_roots: Some(vec![SourceRoot {
                root: machine_b.cli_root.clone(),
                source: LedgerSource::Cli,
            }]),
            now_ms: Some(ts),
            default_account_id: Some("work@ex.com".to_string()),
            ..Default::default()
        })
        .unwrap();

    let cfg_a = machine_a.sync_config(remote_dir.path(), "a", "agy-pulse-sync");
    let cfg_b = machine_b.sync_config(remote_dir.path(), "b", "agy-pulse-sync");
    push(&store_a, &remote, &cfg_a, false).unwrap();
    sync_both(&store_b, &remote, &cfg_b, false).unwrap();

    // 1. Prune local device without --force must fail and delete nothing.
    let err = prune_device(&store_a, &remote, &cfg_a, &dev_a, false, false).unwrap_err();
    assert!(matches!(err, SyncError::Config(_)));
    assert_eq!(remote.list("agy-pulse-sync/accounts").unwrap().len(), 6);

    // 2. Dry-run prune of dev_a from Machine B counts 4 objects and deletes nothing.
    let dry_prune = prune_device(&store_b, &remote, &cfg_b, &dev_a, false, true).unwrap();
    assert!(dry_prune.dry_run);
    assert!(!dry_prune.is_local);
    assert_eq!(dry_prune.remote_objects_deleted, 4);
    assert_eq!(dry_prune.local_rows_deleted, 0);
    assert_eq!(remote.list("agy-pulse-sync/accounts").unwrap().len(), 6);

    // 3. Real prune of dev_a from Machine B deletes dev_a remote objects + Machine B's local cache for dev_a.
    let real_prune = prune_device(&store_b, &remote, &cfg_b, &dev_a, false, false).unwrap();
    assert_eq!(real_prune.remote_objects_deleted, 4);
    assert!(real_prune.local_rows_deleted >= 4);

    let remaining_keys = remote.list("agy-pulse-sync/accounts").unwrap();
    assert_eq!(remaining_keys.len(), 2);
    assert!(remaining_keys.iter().all(|k| k.contains(&dev_b)));

    let conn_b = Connection::open(&machine_b.ledger_path).unwrap();
    let dev_a_rows: i64 = conn_b
        .query_row(
            "SELECT COUNT(*) FROM telemetry_steps WHERE device_id = ?1",
            params![&dev_a],
            |r| r.get(0),
        )
        .unwrap();
    let dev_b_rows: i64 = conn_b
        .query_row(
            "SELECT COUNT(*) FROM telemetry_steps WHERE device_id = ?1",
            params![&dev_b],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(dev_a_rows, 0);
    assert_eq!(dev_b_rows, 1);

    // 4. Prune local device dev_b with --force deletes dev_b remote objects & push state, keeping local ledger rows!
    let force_prune = prune_device(&store_b, &remote, &cfg_b, &dev_b, true, false).unwrap();
    assert!(force_prune.is_local);
    assert_eq!(force_prune.remote_objects_deleted, 2);
    assert!(remote.list("agy-pulse-sync/accounts").unwrap().is_empty());
    let dev_b_rows_after: i64 = conn_b
        .query_row(
            "SELECT COUNT(*) FROM telemetry_steps WHERE device_id = ?1",
            params![&dev_b],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(dev_b_rows_after, 1);
    assert_eq!(
        store_b.get_sync_chunk_hash("work@ex.com", &dev_b, "2026-11", "push").unwrap(),
        None
    );
}

#[test]
fn t12_config_0600_env_overrides_redaction_and_security_boundaries() {
    let _env_lock = ENV_MUTEX.lock().unwrap();
    let mut env_guard = EnvGuard::new();
    let temp = TempDir::new().unwrap();
    let sync_cfg_path = temp.path().join("private").join("agy-pulse-sync.json");

    // Workspace config file must NEVER be read as sync config.
    let ws_dir = temp.path().join("workspace");
    fs::create_dir_all(&ws_dir).unwrap();
    fs::write(
        ws_dir.join("agy-pulse.config.json"),
        r#"{"sync":{"enabled":true}}"#,
    )
    .unwrap();
    env_guard.set("AGY_PULSE_SYNC_CONFIG", sync_cfg_path.to_str().unwrap());
    assert_eq!(load_sync_config().unwrap(), None);

    // Save config and verify 0600 permissions + normalized prefix.
    let s3_cfg = SyncConfig {
        enabled: true,
        device_id: Some("dev_1234567890abcdef".to_string()),
        device_name: Some("lab-box".to_string()),
        default_account: Some(" Alice@Example.com ".to_string()),
        push_interval_sec: 120,
        prefix: "/custom/prefix/".to_string(),
        remote: RemoteConfig::S3 {
            endpoint: "https://user:urlsecret@s3.example.com/".to_string(),
            region: "auto".to_string(),
            bucket: "my-bucket".to_string(),
            access_key: Some("AKIA_FILE_SECRET".to_string()),
            secret_key: Some("S3_FILE_SECRET".to_string()),
            path_style: true,
            allow_insecure_http: false,
        },
    };
    save_sync_config_to(&sync_cfg_path, &s3_cfg).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let file_mode = fs::metadata(&sync_cfg_path).unwrap().permissions().mode() & 0o777;
        let dir_mode = fs::metadata(sync_cfg_path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600);
        assert_eq!(dir_mode, 0o700);
    }

    // Debug, redacted_target, and remote_fingerprint must NEVER leak secrets.
    let debug_str = format!("{s3_cfg:?}");
    let redacted = s3_cfg.redacted_target();
    let fp_before = s3_cfg.remote_fingerprint();
    for secret in ["AKIA_FILE_SECRET", "S3_FILE_SECRET", "urlsecret"] {
        assert!(!debug_str.contains(secret), "Debug leaked {secret}: {debug_str}");
        assert!(!redacted.contains(secret), "redacted_target leaked {secret}: {redacted}");
    }
    assert_eq!(redacted, "s3:https://s3.example.com/my-bucket (region=auto, prefix=custom/prefix)");

    // Env overrides supersede file secrets without changing remote_fingerprint.
    env_guard.set("AGY_PULSE_S3_ACCESS_KEY", "AKIA_ENV_OVERRIDE");
    env_guard.set("AGY_PULSE_S3_SECRET_KEY", "S3_ENV_SECRET_OVERRIDE");
    let loaded_s3 = load_sync_config().unwrap().unwrap();
    assert_eq!(loaded_s3.prefix, "custom/prefix");
    assert_eq!(loaded_s3.default_account.as_deref(), Some("alice@example.com"));
    assert_eq!(loaded_s3.remote_fingerprint(), fp_before);
    match &loaded_s3.remote {
        RemoteConfig::S3 {
            access_key,
            secret_key,
            ..
        } => {
            assert_eq!(access_key.as_deref(), Some("AKIA_ENV_OVERRIDE"));
            assert_eq!(secret_key.as_deref(), Some("S3_ENV_SECRET_OVERRIDE"));
        }
        other => panic!("unexpected remote variant: {other:?}"),
    }
    assert!(matches!(
        build_remote(&loaded_s3),
        Err(SyncError::UnsupportedRemote(ref b)) if b == "s3"
    ));

    // WebDAV and CfWorker env overrides + unsupported checks + serde tag names.
    let parsed_dav_json: SyncConfig = serde_json::from_str(
        r#"{"enabled":true,"pushIntervalSec":0,"remote":{"type":"webdav","url":"https://dav.example.com"}}"#,
    )
    .unwrap();
    assert!(matches!(parsed_dav_json.remote, RemoteConfig::WebDav { .. }));
    let parsed_cf_json: SyncConfig = serde_json::from_str(
        r#"{"enabled":true,"remote":{"type":"cf-worker","url":"https://sync.example.workers.dev"}}"#,
    )
    .unwrap();
    assert!(matches!(parsed_cf_json.remote, RemoteConfig::CfWorker { .. }));

    let webdav_path = temp.path().join("webdav.json");
    save_sync_config_to(
        &webdav_path,
        &SyncConfig {
            push_interval_sec: 0,
            remote: RemoteConfig::WebDav {
                url: "https://dav.example.com/remote.php/dav".to_string(),
                username: Some("alice".to_string()),
                password: Some("file_pw".to_string()),
                token: Some("file_tok".to_string()),
                allow_insecure_http: false,
            },
            ..Default::default()
        },
    )
    .unwrap();
    env_guard.set("AGY_PULSE_SYNC_PASSWORD", "env_pw_override");
    env_guard.set("AGY_PULSE_SYNC_TOKEN", "env_tok_override");
    let loaded_dav = load_sync_config_from(&webdav_path).unwrap().unwrap();
    assert_eq!(loaded_dav.push_interval_sec, 1);
    let dav_dbg = format!("{loaded_dav:?}");
    assert!(!dav_dbg.contains("env_pw_override"));
    assert!(!dav_dbg.contains("env_tok_override"));
    match &loaded_dav.remote {
        RemoteConfig::WebDav {
            password, token, ..
        } => {
            assert_eq!(password.as_deref(), Some("env_pw_override"));
            assert_eq!(token.as_deref(), Some("env_tok_override"));
        }
        other => panic!("unexpected remote variant: {other:?}"),
    }
    assert!(matches!(
        build_remote(&loaded_dav),
        Err(SyncError::UnsupportedRemote(ref b)) if b == "webdav"
    ));

    // Key & account_key path traversal rejections.
    for bad_key in [
        "",
        "/etc/passwd",
        "a/b/",
        "../escape",
        "a/../b",
        "a/./b",
        "a\\b",
        "a//b",
        "bad\nkey",
    ] {
        assert!(
            matches!(validate_relative_key(bad_key), Err(SyncError::InvalidKey(_))),
            "expected InvalidKey for {bad_key:?}"
        );
    }
    assert!(encode_account_key("..").is_err());
    assert!(encode_account_key("a/b").is_err());
    assert!(decode_account_key("%2e%2e").is_err());
    assert!(decode_account_key("a%2Fb").is_err());
    assert!(decode_account_key("%00").is_err());

    let encoded = encode_account_key("Alice+Work@Example.COM").unwrap();
    assert_eq!(decode_account_key(&encoded).unwrap(), "Alice+Work@Example.COM");

    let remote_dir = temp.path().join("remote");
    let local_remote = LocalDirRemote::new(remote_dir.clone());
    assert_eq!(local_remote.get("nonexistent/key.json").unwrap(), None);
    local_remote
        .put("nested/dir/test.txt", b"hello", "text/plain")
        .unwrap();
    local_remote.delete("nested/dir/test.txt").unwrap();
    local_remote.delete("nested/dir/test.txt").unwrap();
    assert!(remote_dir.exists());
    assert!(!remote_dir.join("nested").exists());
    assert!(matches!(
        local_remote.get("../escape"),
        Err(SyncError::InvalidKey(_))
    ));
    assert!(matches!(
        local_remote.put("../escape", b"x", "text/plain"),
        Err(SyncError::InvalidKey(_))
    ));
    assert!(matches!(
        local_remote.list("../escape"),
        Err(SyncError::InvalidKey(_))
    ));
    assert!(matches!(
        local_remote.delete("../escape"),
        Err(SyncError::InvalidKey(_))
    ));

    // Gzip bomb (> 64 MiB decompressed) rejected.
    let mut enc = GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), Compression::fast());
    let ones_mb = vec![b' '; 1024 * 1024];
    for _ in 0..65 {
        enc.write_all(&ones_mb).unwrap();
    }
    let bomb_gz = enc.finish().unwrap();
    assert!(matches!(
        decode_chunk_gz(&bomb_gz),
        Err(SyncError::Protocol(_))
    ));

    // Tampered manifest with mismatched account_id/device_id is rejected before DB import.
    let machine_b = IsolatedMachine::new();
    let store_b = machine_b.open_store();
    let cfg_b = machine_b.sync_config(&remote_dir, "b", "agy-pulse-sync");
    let tampered = DeviceManifest {
        format_version: 1,
        account_id: "victim@ex.com".to_string(),
        device_id: "dev_evil".to_string(),
        device_name: "evil".to_string(),
        plan_tier: None,
        updated_at_ms: 1_700_000_000_000,
        quota_snapshot: None,
        chunks: Default::default(),
    };
    local_remote
        .put(
            "agy-pulse-sync/accounts/attacker@ex.com/devices/dev_evil/manifest.json",
            &serde_json::to_vec(&tampered).unwrap(),
            "application/json",
        )
        .unwrap();
    assert!(matches!(
        pull(&store_b, &local_remote, &cfg_b, false),
        Err(SyncError::Protocol(_))
    ));

    // Active lock held by a live PID causes AlreadyRunning without error; stale lock is recovered.
    let lock_path = machine_b.ledger_path.parent().unwrap().join("agy-pulse-sync.lock");
    let live_lock = serde_json::json!({
        "pid": std::process::id(),
        "token": "external-live-lock",
        "ts": 1_700_000_000_000i64
    });
    fs::write(&lock_path, live_lock.to_string()).unwrap();
    let locked_out = push(&store_b, &local_remote, &cfg_b, false).unwrap();
    assert!(locked_out.locked);
    assert_eq!(locked_out.kind, SyncOutcomeKind::AlreadyRunning);

    let stale_lock = serde_json::json!({
        "pid": 999_999_999u32,
        "token": "dead-pid-lock",
        "ts": 1_700_000_000_000i64
    });
    fs::write(&lock_path, stale_lock.to_string()).unwrap();
    let recovered_out = push(&store_b, &local_remote, &cfg_b, false).unwrap();
    assert!(!recovered_out.locked);
    assert_eq!(recovered_out.kind, SyncOutcomeKind::Completed);
    assert!(!lock_path.exists());
    assert_eq!(UNASSIGNED_ACCOUNT_ID, "unassigned");
}
