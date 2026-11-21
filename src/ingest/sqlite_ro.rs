use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, ErrorCode, OpenFlags};
use tempfile::TempDir;

fn sidecar_path(db_path: &Path, suffix: &str) -> PathBuf {
    let mut os = OsString::from(db_path.as_os_str());
    os.push(suffix);
    PathBuf::from(os)
}

pub(super) fn has_non_empty_wal(db_path: &Path) -> bool {
    let wal = sidecar_path(db_path, "-wal");
    fs::metadata(&wal).is_ok_and(|m| m.is_file() && m.len() > 0)
}

fn percent_encode_sqlite_uri_path(path_str: &str) -> String {
    let mut out = String::with_capacity(path_str.len());
    for byte in path_str.as_bytes() {
        let keep = byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'_' | b'.' | b'~' | b'/' | b':' | b'!' | b'$' | b'&' | b'\'' | b'(' | b')'
                    | b'*' | b'+' | b',' | b';' | b'=' | b'@'
            );
        if keep {
            out.push(*byte as char);
        } else {
            out.push('%');
            let hi = (byte >> 4) & 0x0f;
            let lo = byte & 0x0f;
            let hex_digit = |n: u8| -> char {
                if n < 10 {
                    (b'0' + n) as char
                } else {
                    (b'A' + (n - 10)) as char
                }
            };
            out.push(hex_digit(hi));
            out.push(hex_digit(lo));
        }
    }
    out
}

pub(super) fn to_immutable_sqlite_uri(db_path: &Path) -> String {
    let abs = std::path::absolute(db_path).unwrap_or_else(|_| db_path.to_path_buf());
    let normalized = abs.to_string_lossy().replace('\\', "/");
    let encoded = percent_encode_sqlite_uri_path(&normalized);
    if normalized.starts_with('/') {
        format!("file:{encoded}?immutable=1")
    } else {
        format!("file:///{encoded}?immutable=1")
    }
}

pub(super) fn is_sqlite_open_or_access_error(err: &rusqlite::Error) -> bool {
    if let rusqlite::Error::SqliteFailure(ffi_err, _) = err {
        return matches!(
            ffi_err.code,
            ErrorCode::CannotOpen
                | ErrorCode::DatabaseBusy
                | ErrorCode::DatabaseLocked
                | ErrorCode::SystemIoFailure
                | ErrorCode::PermissionDenied
                | ErrorCode::ReadOnly
        );
    }
    let msg = err.to_string().to_ascii_lowercase();
    msg.contains("unable to open")
        || msg.contains("cantopen")
        || msg.contains("busy")
        || msg.contains("locked")
        || msg.contains("readonly")
        || msg.contains("permission")
}

pub(super) fn is_no_such_table_error(err: &rusqlite::Error) -> bool {
    err.to_string().to_ascii_lowercase().contains("no such table")
}

struct TempDbCopy {
    _dir: TempDir,
    db_path: PathBuf,
}

fn create_temp_db_copy(db_path: &Path) -> std::io::Result<TempDbCopy> {
    let dir = tempfile::Builder::new()
        .prefix("agy-pulse-db-")
        .tempdir()?;
    let file_name = db_path
        .file_name()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing db filename"))?;
    let tmp_db_path = dir.path().join(file_name);
    fs::copy(db_path, &tmp_db_path)?;

    for suffix in ["-wal", "-shm"] {
        let src_sidecar = sidecar_path(db_path, suffix);
        if fs::metadata(&src_sidecar).is_ok_and(|m| m.is_file()) {
            let mut dst_os = OsString::from(tmp_db_path.as_os_str());
            dst_os.push(suffix);
            let _ = fs::copy(&src_sidecar, PathBuf::from(dst_os));
        }
    }

    Ok(TempDbCopy {
        _dir: dir,
        db_path: tmp_db_path,
    })
}

/// Lock-free read-only SQLite opener for host `conversations/*.db` files.
///
/// - When no non-empty `-wal` exists, opens `file:...?immutable=1` with
///   `SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_URI | SQLITE_OPEN_NO_MUTEX`.
/// - When a non-empty `-wal` exists or immutable access hits a lock/access error,
///   copies `.db` + `-wal` + `-shm` into an isolated `TempDir` and queries the copy,
///   guaranteeing zero bytes written and zero `-shm`/`-journal` creation in `conversations/`.
pub(super) fn with_readonly_db<T, F>(db_path: &Path, query_fn: F) -> Option<T>
where
    F: Fn(&Connection) -> Result<T, rusqlite::Error>,
{
    if !fs::metadata(db_path).is_ok_and(|m| m.is_file()) {
        return None;
    }

    if !has_non_empty_wal(db_path) {
        let uri = to_immutable_sqlite_uri(db_path);
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        if let Ok(conn) = Connection::open_with_flags(&uri, flags) {
            match query_fn(&conn) {
                Ok(val) => return Some(val),
                Err(err) if !is_sqlite_open_or_access_error(&err) => return None,
                Err(_) => {}
            }
        }
    }

    let copy = create_temp_db_copy(db_path).ok()?;
    let ro_flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    if let Ok(conn) = Connection::open_with_flags(&copy.db_path, ro_flags) {
        match query_fn(&conn) {
            Ok(val) => return Some(val),
            Err(err) if !is_sqlite_open_or_access_error(&err) => return None,
            Err(_) => {}
        }
    }

    // Only on the isolated TempDir copy: WAL without -shm may need RW recovery to replay -wal frames.
    let rw_flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(&copy.db_path, rw_flags).ok()?;
    query_fn(&conn).ok()
}
