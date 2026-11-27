use std::path::Path;
use std::thread;
use std::time::Duration;

use rusqlite::{Connection, ErrorCode, OptionalExtension, params};

use crate::domain::{UNASSIGNED_ACCOUNT_ID, normalize_account_id};
use crate::platform;

use super::LedgerError;

pub(super) const DEFAULT_LEDGER_FILENAME: &str = "agy-pulse-ledger.sqlite";
pub const LEDGER_SCHEMA_VERSION: &str = "4";
pub(super) const DEFAULT_BUSY_TIMEOUT_MS: u64 = 3000;
pub(super) const STATUSLINE_BUSY_TIMEOUT_MS: u64 = 100;
pub(super) const DEFAULT_BUSY_RETRIES: u32 = 15;
pub(super) const STATUSLINE_BUSY_RETRIES: u32 = 2;

const LEDGER_SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS ledger_meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS ingested_dbs (
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
  source TEXT NOT NULL DEFAULT 'cli',
  account_id TEXT NOT NULL DEFAULT 'unassigned',
  device_id TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS telemetry_steps (
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
  source TEXT NOT NULL DEFAULT 'cli',
  account_id TEXT NOT NULL DEFAULT 'unassigned',
  device_id TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_steps_ts ON telemetry_steps(ts_ms);
CREATE INDEX IF NOT EXISTS idx_steps_conv ON telemetry_steps(conversation_id);
CREATE INDEX IF NOT EXISTS idx_steps_root ON telemetry_steps(root_conversation_id);
CREATE INDEX IF NOT EXISTS idx_steps_model ON telemetry_steps(model);
CREATE TABLE IF NOT EXISTS rolling_cost_cursors (
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
CREATE TABLE IF NOT EXISTS sync_devices (
  account_id TEXT NOT NULL,
  device_id TEXT NOT NULL,
  device_name TEXT NOT NULL,
  is_local INTEGER NOT NULL DEFAULT 0,
  last_seen_ms INTEGER NOT NULL DEFAULT 0,
  plan_tier TEXT,
  PRIMARY KEY (account_id, device_id)
);
CREATE TABLE IF NOT EXISTS quota_snapshots (
  account_id TEXT NOT NULL,
  device_id TEXT NOT NULL,
  device_name TEXT NOT NULL,
  plan_tier TEXT,
  observed_at_ms INTEGER NOT NULL,
  quota_json TEXT NOT NULL,
  PRIMARY KEY (account_id, device_id)
);
CREATE TABLE IF NOT EXISTS sync_chunk_state (
  account_id TEXT NOT NULL,
  device_id TEXT NOT NULL,
  month_key TEXT NOT NULL,
  direction TEXT NOT NULL,
  content_sha256 TEXT NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY (account_id, device_id, month_key, direction)
);
";

pub(super) fn is_sqlite_busy(err: &rusqlite::Error) -> bool {
    matches!(
        err.sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

pub(super) fn run_with_busy_retry_n<T, F>(max_retries: u32, mut op: F) -> Result<T, rusqlite::Error>
where
    F: FnMut() -> Result<T, rusqlite::Error>,
{
    let mut attempt = 0u32;
    loop {
        match op() {
            Ok(val) => return Ok(val),
            Err(err) if is_sqlite_busy(&err) && attempt < max_retries => {
                attempt += 1;
                thread::sleep(Duration::from_millis(20 + u64::from(attempt) * 10));
            }
            Err(err) => return Err(err),
        }
    }
}

pub(super) fn run_with_busy_retry<T, F>(op: F) -> Result<T, rusqlite::Error>
where
    F: FnMut() -> Result<T, rusqlite::Error>,
{
    run_with_busy_retry_n(DEFAULT_BUSY_RETRIES, op)
}

fn has_table_column(conn: &Connection, table_name: &str, col_name: &str) -> Result<bool, rusqlite::Error> {
    let sql = format!("PRAGMA table_info({table_name})");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == col_name {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn read_local_device_id(conn: &Connection) -> Result<String, rusqlite::Error> {
    let raw: String = conn.query_row(
        "SELECT value FROM ledger_meta WHERE key = 'local_device_id'",
        [],
        |r| r.get(0),
    )?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        Err(rusqlite::Error::QueryReturnedNoRows)
    } else {
        Ok(trimmed.to_string())
    }
}

fn is_schema_already_initialized(conn: &Connection) -> bool {
    let version_ok = conn
        .query_row(
            "SELECT value FROM ledger_meta WHERE key = 'schema_version'",
            [],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .is_some_and(|v| v == LEDGER_SCHEMA_VERSION);
    if !version_ok {
        return false;
    }
    let wal_ok = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
        .ok()
        .is_some_and(|jm| jm.eq_ignore_ascii_case("wal"));
    if !wal_ok {
        return false;
    }
    read_local_device_id(conn).is_ok()
}

fn ensure_schema(
    conn: &mut Connection,
    busy_timeout_ms: u64,
    max_retries: u32,
) -> Result<(), rusqlite::Error> {
    // Enable busy_timeout first; avoid acquiring write locks on hot-path opens when schema is already current.
    conn.busy_timeout(Duration::from_millis(busy_timeout_ms))?;
    if is_schema_already_initialized(conn) {
        let _ = conn.pragma_update(None, "synchronous", "NORMAL");
        return Ok(());
    }

    run_with_busy_retry_n(max_retries, || {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;

        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute_batch(LEDGER_SCHEMA_SQL)?;

        for (table, col, col_def) in [
            ("ingested_dbs", "source", "TEXT NOT NULL DEFAULT 'cli'"),
            (
                "ingested_dbs",
                "account_id",
                "TEXT NOT NULL DEFAULT 'unassigned'",
            ),
            ("ingested_dbs", "device_id", "TEXT NOT NULL DEFAULT ''"),
            ("telemetry_steps", "source", "TEXT NOT NULL DEFAULT 'cli'"),
            (
                "telemetry_steps",
                "account_id",
                "TEXT NOT NULL DEFAULT 'unassigned'",
            ),
            ("telemetry_steps", "device_id", "TEXT NOT NULL DEFAULT ''"),
        ] {
            if !has_table_column(&tx, table, col)? {
                let alter_sql = format!("ALTER TABLE {table} ADD COLUMN {col} {col_def};");
                tx.execute_batch(&alter_sql)?;
            }
        }

        tx.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_steps_source ON telemetry_steps(source);
             CREATE INDEX IF NOT EXISTS idx_steps_device_ts ON telemetry_steps(device_id, ts_ms);
             CREATE INDEX IF NOT EXISTS idx_steps_account_device_ts ON telemetry_steps(account_id, device_id, ts_ms);",
        )?;

        let existing_device_id = tx
            .query_row(
                "SELECT value FROM ledger_meta WHERE key = 'local_device_id'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let local_device_id = match existing_device_id {
            Some(id) => id,
            None => {
                let id = platform::generate_device_id();
                tx.execute(
                    "INSERT OR REPLACE INTO ledger_meta (key, value) VALUES ('local_device_id', ?1)",
                    params![&id],
                )?;
                id
            }
        };

        tx.execute(
            "UPDATE ingested_dbs SET device_id = ?1 WHERE device_id = '' OR device_id IS NULL",
            params![&local_device_id],
        )?;
        tx.execute(
            "UPDATE telemetry_steps SET device_id = ?1 WHERE device_id = '' OR device_id IS NULL",
            params![&local_device_id],
        )?;

        let device_name = platform::current_hostname().unwrap_or_else(|| "local".to_string());
        let mut account_ids: Vec<String> = Vec::new();
        {
            let mut stmt = tx.prepare(
                "SELECT DISTINCT account_id FROM ingested_dbs WHERE device_id = ?1",
            )?;
            let mut rows = stmt.query(params![&local_device_id])?;
            while let Some(row) = rows.next()? {
                let raw: String = row.get(0)?;
                let normalized = normalize_account_id(&raw)
                    .unwrap_or_else(|| UNASSIGNED_ACCOUNT_ID.to_string());
                if !account_ids.contains(&normalized) {
                    account_ids.push(normalized);
                }
            }
        }
        for acct in &account_ids {
            tx.execute(
                "INSERT INTO sync_devices (account_id, device_id, device_name, is_local, last_seen_ms, plan_tier)
                 VALUES (?1, ?2, ?3, 1, 0, NULL)
                 ON CONFLICT(account_id, device_id) DO UPDATE SET is_local = 1",
                params![acct, &local_device_id, &device_name],
            )?;
        }
        let has_local_sync_device: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sync_devices WHERE device_id = ?1)",
            params![&local_device_id],
            |r| r.get::<_, i64>(0),
        )? != 0;
        if !has_local_sync_device {
            tx.execute(
                "INSERT INTO sync_devices (account_id, device_id, device_name, is_local, last_seen_ms, plan_tier)
                 VALUES (?1, ?2, ?3, 1, 0, NULL)
                 ON CONFLICT(account_id, device_id) DO UPDATE SET is_local = 1",
                params![UNASSIGNED_ACCOUNT_ID, &local_device_id, &device_name],
            )?;
        }

        tx.execute(
            "INSERT OR REPLACE INTO ledger_meta (key, value) VALUES (?1, ?2)",
            params!["schema_version", LEDGER_SCHEMA_VERSION],
        )?;
        tx.commit()?;
        Ok(())
    })
}

pub(super) fn open_ledger_connection_with_options(
    ledger_path: &Path,
    busy_timeout_ms: u64,
    max_retries: u32,
) -> Result<Connection, LedgerError> {
    if let Some(parent) = ledger_path.parent()
        && !parent.as_os_str().is_empty()
    {
        platform::ensure_dir_0700(parent)?;
    }
    let mut conn = Connection::open(ledger_path)?;
    ensure_schema(&mut conn, busy_timeout_ms, max_retries)?;
    let _ = platform::set_file_0600(ledger_path);
    Ok(conn)
}

pub(super) fn open_ledger_connection(ledger_path: &Path) -> Result<Connection, LedgerError> {
    open_ledger_connection_with_options(ledger_path, DEFAULT_BUSY_TIMEOUT_MS, DEFAULT_BUSY_RETRIES)
}
