use std::path::Path;
use std::thread;
use std::time::Duration;

use rusqlite::{Connection, ErrorCode, params};

use crate::platform;

use super::LedgerError;

pub(super) const DEFAULT_LEDGER_FILENAME: &str = "agy-pulse-ledger.sqlite";
pub(super) const LEDGER_SCHEMA_VERSION: &str = "3";

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
  source TEXT NOT NULL DEFAULT 'cli'
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
  source TEXT NOT NULL DEFAULT 'cli'
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
";

fn is_sqlite_busy(err: &rusqlite::Error) -> bool {
    matches!(
        err.sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

pub(super) fn run_with_busy_retry<T, F>(mut op: F) -> Result<T, rusqlite::Error>
where
    F: FnMut() -> Result<T, rusqlite::Error>,
{
    let mut attempt = 0u32;
    loop {
        match op() {
            Ok(val) => return Ok(val),
            Err(err) if is_sqlite_busy(&err) && attempt < 15 => {
                attempt += 1;
                thread::sleep(Duration::from_millis(20 + u64::from(attempt) * 10));
            }
            Err(err) => return Err(err),
        }
    }
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

fn ensure_schema(conn: &Connection) -> Result<(), rusqlite::Error> {
    // Enable WAL and busy_timeout first; retry if multiple cold-start processes race on init.
    conn.busy_timeout(Duration::from_millis(3000))?;
    run_with_busy_retry(|| {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(LEDGER_SCHEMA_SQL)?;
        if !has_table_column(conn, "ingested_dbs", "source")? {
            conn.execute_batch(
                "ALTER TABLE ingested_dbs ADD COLUMN source TEXT NOT NULL DEFAULT 'cli';",
            )?;
        }
        if !has_table_column(conn, "telemetry_steps", "source")? {
            conn.execute_batch(
                "ALTER TABLE telemetry_steps ADD COLUMN source TEXT NOT NULL DEFAULT 'cli';",
            )?;
        }
        conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_steps_source ON telemetry_steps(source);")?;
        conn.execute(
            "INSERT OR REPLACE INTO ledger_meta (key, value) VALUES (?1, ?2)",
            params!["schema_version", LEDGER_SCHEMA_VERSION],
        )?;
        Ok(())
    })
}

pub(super) fn open_ledger_connection(ledger_path: &Path) -> Result<Connection, LedgerError> {
    if let Some(parent) = ledger_path.parent()
        && !parent.as_os_str().is_empty()
    {
        platform::ensure_dir_0700(parent)?;
    }
    let conn = Connection::open(ledger_path)?;
    ensure_schema(&conn)?;
    let _ = platform::set_file_0600(ledger_path);
    Ok(conn)
}
