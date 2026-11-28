use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rusqlite::{Connection, ErrorCode, OpenFlags};
use tempfile::TempDir;

const MAX_INMEM_SQLITE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_INMEM_WAL_SCAN_BYTES: u64 = 256 * 1024 * 1024;
const WAL_MAGIC_LITTLE_ENDIAN: u32 = 0x377f_0682;
const WAL_MAGIC_BIG_ENDIAN: u32 = 0x377f_0683;
const WAL_FORMAT_VERSION: u32 = 3_007_000;

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
                b'-' | b'_'
                    | b'.'
                    | b'~'
                    | b'/'
                    | b':'
                    | b'!'
                    | b'$'
                    | b'&'
                    | b'\''
                    | b'('
                    | b')'
                    | b'*'
                    | b'+'
                    | b','
                    | b';'
                    | b'='
                    | b'@'
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
    let abs_lossy = abs.to_string_lossy();
    let stripped = if let Some(unc_rest) = abs_lossy.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc_rest}")
    } else if let Some(rest) = abs_lossy.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        abs_lossy.into_owned()
    };
    let normalized = stripped.replace('\\', "/");
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
    err.to_string()
        .to_ascii_lowercase()
        .contains("no such table")
}

fn is_sqlite_corrupt_or_notadb_error(err: &rusqlite::Error) -> bool {
    if let rusqlite::Error::SqliteFailure(ffi_err, _) = err {
        return matches!(
            ffi_err.code,
            ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase
        );
    }
    let msg = err.to_string().to_ascii_lowercase();
    msg.contains("malformed") || msg.contains("corrupt") || msg.contains("not a database")
}

fn sqlite_wal_checksum(data: &[u8], init: (u32, u32), big_endian: bool) -> (u32, u32) {
    let (mut s0, mut s1) = init;
    let (chunks, _) = data.as_chunks::<8>();
    for chunk in chunks {
        let b0 = [chunk[0], chunk[1], chunk[2], chunk[3]];
        let b1 = [chunk[4], chunk[5], chunk[6], chunk[7]];
        let (w0, w1) = if big_endian {
            (u32::from_be_bytes(b0), u32::from_be_bytes(b1))
        } else {
            (u32::from_le_bytes(b0), u32::from_le_bytes(b1))
        };
        s0 = s0.wrapping_add(w0).wrapping_add(s1);
        s1 = s1.wrapping_add(w1).wrapping_add(s0);
    }
    (s0, s1)
}

fn build_inmemory_wal_merged_db(db_path: &Path) -> Option<Vec<u8>> {
    let db_meta = fs::metadata(db_path).ok()?;
    if !db_meta.is_file() || !(100..=MAX_INMEM_SQLITE_BYTES).contains(&db_meta.len()) {
        return None;
    }

    let mut db_buf = fs::read(db_path).ok()?;
    let db_len = db_buf.len() as u64;
    if !(100..=MAX_INMEM_SQLITE_BYTES).contains(&db_len) {
        return None;
    }
    if &db_buf[..16] != b"SQLite format 3\0" {
        return None;
    }

    let raw_ps = u16::from_be_bytes([db_buf[16], db_buf[17]]);
    let page_size: usize = if raw_ps == 1 { 65_536 } else { raw_ps as usize };
    if !(512..=65_536).contains(&page_size) || !page_size.is_power_of_two() {
        return None;
    }

    let wal_path = sidecar_path(db_path, "-wal");
    if let Ok(wal_meta) = fs::metadata(&wal_path)
        && wal_meta.is_file()
        && wal_meta.len() > 0
    {
        if !(32..=MAX_INMEM_WAL_SCAN_BYTES).contains(&wal_meta.len()) {
            return None;
        }
        let wal_file = File::open(&wal_path).ok()?;
        let mut reader = BufReader::with_capacity(64 * 1024, wal_file);
        let mut hdr = [0u8; 32];
        reader.read_exact(&mut hdr).ok()?;

        let magic = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
        let big_endian = match magic {
            WAL_MAGIC_LITTLE_ENDIAN => false,
            WAL_MAGIC_BIG_ENDIAN => true,
            _ => return None,
        };
        let format = u32::from_be_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]);
        let wal_page_size = u32::from_be_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]) as usize;
        if format != WAL_FORMAT_VERSION || wal_page_size != page_size {
            return None;
        }
        let salt1 = u32::from_be_bytes([hdr[16], hdr[17], hdr[18], hdr[19]]);
        let salt2 = u32::from_be_bytes([hdr[20], hdr[21], hdr[22], hdr[23]]);
        let hdr_cksum1 = u32::from_be_bytes([hdr[24], hdr[25], hdr[26], hdr[27]]);
        let hdr_cksum2 = u32::from_be_bytes([hdr[28], hdr[29], hdr[30], hdr[31]]);
        if sqlite_wal_checksum(&hdr[0..24], (0, 0), big_endian) != (hdr_cksum1, hdr_cksum2) {
            return None;
        }

        let mut s0 = hdr_cksum1;
        let mut s1 = hdr_cksum2;
        let mut pending: BTreeMap<u32, Vec<u8>> = BTreeMap::new();
        let mut fh = [0u8; 24];
        let mut page_buf = vec![0u8; page_size];

        loop {
            if reader.read_exact(&mut fh).is_err() || reader.read_exact(&mut page_buf).is_err() {
                break;
            }
            let page_no = u32::from_be_bytes([fh[0], fh[1], fh[2], fh[3]]);
            let db_size = u32::from_be_bytes([fh[4], fh[5], fh[6], fh[7]]);
            let f_salt1 = u32::from_be_bytes([fh[8], fh[9], fh[10], fh[11]]);
            let f_salt2 = u32::from_be_bytes([fh[12], fh[13], fh[14], fh[15]]);
            let f_cksum1 = u32::from_be_bytes([fh[16], fh[17], fh[18], fh[19]]);
            let f_cksum2 = u32::from_be_bytes([fh[20], fh[21], fh[22], fh[23]]);

            if page_no == 0 || f_salt1 != salt1 || f_salt2 != salt2 {
                break;
            }

            let cksum = sqlite_wal_checksum(&fh[0..8], (s0, s1), big_endian);
            let cksum = sqlite_wal_checksum(&page_buf, cksum, big_endian);
            if cksum != (f_cksum1, f_cksum2) {
                break;
            }
            (s0, s1) = cksum;

            if !pending.contains_key(&page_no)
                && ((pending.len() + 1) as u64).saturating_mul(page_size as u64)
                    > MAX_INMEM_SQLITE_BYTES
            {
                return None;
            }
            pending.insert(page_no, page_buf.clone());

            if db_size > 0 {
                let new_len = (db_size as usize).checked_mul(page_size)?;
                if new_len as u64 > MAX_INMEM_SQLITE_BYTES || new_len < 100 {
                    return None;
                }
                db_buf.resize(new_len, 0);
                for (pno, pdata) in std::mem::take(&mut pending) {
                    if pno >= 1
                        && let Some(end) = (pno as usize).checked_mul(page_size)
                        && end <= db_buf.len()
                    {
                        let start = end - page_size;
                        db_buf[start..end].copy_from_slice(&pdata);
                    }
                }
            }
        }
    }

    // Clear WAL format flags in the in-memory header so SQLite `memdb` VFS opens it cleanly.
    db_buf[18] = 1;
    db_buf[19] = 1;
    Some(db_buf)
}

struct TempDbCopy {
    _dir: TempDir,
    db_path: PathBuf,
}

fn create_temp_db_copy(db_path: &Path) -> std::io::Result<TempDbCopy> {
    let fast_dir = crate::platform::resolve_fast_temp_dir();
    let dir = tempfile::Builder::new()
        .prefix("agy-pulse-db-")
        .tempdir_in(&fast_dir)
        .or_else(|_| tempfile::Builder::new().prefix("agy-pulse-db-").tempdir())?;
    let file_name = db_path.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing db filename")
    })?;
    let tmp_db_path = dir.path().join(file_name);
    fs::copy(db_path, &tmp_db_path)?;

    let wal_src = sidecar_path(db_path, "-wal");
    if fs::metadata(&wal_src).is_ok_and(|m| m.is_file()) {
        let mut dst_os = OsString::from(tmp_db_path.as_os_str());
        dst_os.push("-wal");
        let _ = fs::copy(&wal_src, PathBuf::from(dst_os));
    }

    Ok(TempDbCopy {
        _dir: dir,
        db_path: tmp_db_path,
    })
}

fn with_temp_copy_fallback<T, F>(db_path: &Path, query_fn: &F) -> Option<T>
where
    F: Fn(&Connection) -> Result<T, rusqlite::Error>,
{
    let copy = create_temp_db_copy(db_path).ok()?;
    // On the isolated TempDir copy (without copied -shm), RW open cleanly rebuilds
    // the transient WAL index and replays all uncommitted -wal frames without touching
    // the host conversations/ directory, while disabling close checkpointing and fsync.
    let rw_flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    if let Ok(conn) = Connection::open_with_flags(&copy.db_path, rw_flags) {
        let _ = conn.set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
            true,
        );
        let _ = conn.execute_batch(
            "PRAGMA synchronous = OFF; PRAGMA wal_autocheckpoint = 0; PRAGMA temp_store = MEMORY; PRAGMA query_only = ON;",
        );
        if let Ok(val) = query_fn(&conn) {
            return Some(val);
        }
    }

    let ro_flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(&copy.db_path, ro_flags).ok()?;
    query_fn(&conn).ok()
}

/// Lock-free read-only SQLite opener for host `conversations/*.db` files.
///
/// - When no non-empty `-wal` exists, opens `file:...?immutable=1` with
///   `SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_URI | SQLITE_OPEN_NO_MUTEX`.
/// - When a non-empty `-wal` exists or immutable access hits a lock/access error,
///   first merges committed WAL frames purely in memory and loads via
///   `sqlite3_deserialize` (`memdb`), avoiding temp directory / fsync / checkpoint overhead.
/// - If in-memory merging cannot proceed (e.g., oversized DB/WAL or concurrent header race),
///   falls back to an isolated `TempDir` copy with `SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE` and
///   `synchronous = OFF`, guaranteeing zero bytes written in `conversations/`.
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

    if let Some(db_buf) = build_inmemory_wal_merged_db(db_path)
        && let Ok(mut conn) = Connection::open_in_memory()
        && conn
            .deserialize_read_exact(rusqlite::MAIN_DB, &db_buf[..], db_buf.len(), true)
            .is_ok()
    {
        match query_fn(&conn) {
            Ok(val) => return Some(val),
            Err(err)
                if !is_sqlite_open_or_access_error(&err)
                    && !is_sqlite_corrupt_or_notadb_error(&err) =>
            {
                return None;
            }
            Err(_) => {}
        }
    }

    with_temp_copy_fallback(db_path, &query_fn)
}

/// Remove stale `agy-pulse-db-*` temporary directories left behind by crashed runs.
pub(crate) fn cleanup_stale_temp_db_dirs(max_age: Duration) -> usize {
    let mut dirs_to_scan = vec![crate::platform::temp_dir()];
    let fast_temp = crate::platform::resolve_fast_temp_dir();
    if fast_temp != dirs_to_scan[0] {
        dirs_to_scan.push(fast_temp);
    }

    let now = SystemTime::now();
    let mut removed = 0usize;

    for base_dir in dirs_to_scan {
        let Ok(entries) = fs::read_dir(&base_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else {
                continue;
            };
            if !name_str.starts_with("agy-pulse-db-") {
                continue;
            }
            let path = entry.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() || !meta.is_dir() {
                continue;
            }
            let Ok(modified) = meta.modified() else {
                continue;
            };
            let age = now.duration_since(modified).unwrap_or(Duration::ZERO);
            if age >= max_age && fs::remove_dir_all(&path).is_ok() {
                removed += 1;
            }
        }
    }

    removed
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use rusqlite::params;
    use std::collections::HashMap;
    use std::io::Write;

    fn count_temp_db_dirs(base: &Path) -> usize {
        fs::read_dir(base)
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|s| s.starts_with("agy-pulse-db-"))
            })
            .count()
    }

    #[test]
    fn inmemory_wal_merge_reads_uncheckpointed_frames_without_temp_dirs_or_shm() {
        let root = tempfile::tempdir().expect("tempdir");
        let isolated_data = root.path().join("data");
        let conv_dir = isolated_data.join("conversations");
        fs::create_dir_all(&conv_dir).expect("mkdir conversations");
        let db_path = conv_dir.join("active-wal.db");

        let writer = Connection::open(&db_path).expect("open writer");
        writer
            .pragma_update(None, "journal_mode", "WAL")
            .expect("set wal");
        writer
            .pragma_update(None, "wal_autocheckpoint", 0)
            .expect("disable autocheckpoint");
        writer
            .execute("CREATE TABLE items (id INTEGER PRIMARY KEY, val TEXT)", [])
            .expect("create table");
        writer
            .execute("INSERT INTO items (id, val) VALUES (1, 'v1')", [])
            .expect("insert v1");
        writer
            .execute("UPDATE items SET val = 'v2' WHERE id = 1", [])
            .expect("update v2");
        writer
            .execute("INSERT INTO items (id, val) VALUES (2, 'second')", [])
            .expect("insert v2");

        // Append an uncommitted dirty frame at the end of -wal (db_size=0, valid checksum or garbage).
        let wal_path = sidecar_path(&db_path, "-wal");
        assert!(
            fs::metadata(&wal_path).expect("wal metadata").len() > 32,
            "expected non-empty wal"
        );
        // Append a truncated/corrupt dirty tail frame that must be discarded.
        {
            let mut wf = fs::OpenOptions::new()
                .append(true)
                .open(&wal_path)
                .expect("open wal for append");
            wf.write_all(&[0xAAu8; 24 + 128])
                .expect("append dirty tail");
        }

        // Remove -shm to verify our reader works even without -shm (Windows locks -shm while writer is open).
        let shm_path = sidecar_path(&db_path, "-shm");
        let shm_removed = if shm_path.exists() {
            fs::remove_file(&shm_path).is_ok()
        } else {
            true
        };
        #[cfg(unix)]
        assert!(shm_removed, "expected -shm removal to succeed on Unix");

        let mut env_ov = HashMap::new();
        env_ov.insert(
            "AGY_PULSE_DATA_DIR".to_string(),
            Some(isolated_data.to_string_lossy().into_owned()),
        );
        for var in ["TMPDIR", "TMP", "TEMP"] {
            env_ov.insert(
                var.to_string(),
                Some(root.path().to_string_lossy().into_owned()),
            );
        }

        let rows = crate::platform::with_env_overrides(&env_ov, || {
            with_readonly_db(&db_path, |conn| {
                let mut stmt = conn.prepare("SELECT id, val FROM items ORDER BY id ASC")?;
                let mut rows = stmt.query([])?;
                let mut out = Vec::new();
                while let Some(r) = rows.next()? {
                    out.push((r.get::<_, i64>(0)?, r.get::<_, String>(1)?));
                }
                Ok(out)
            })
        });
        let temp_dirs = count_temp_db_dirs(root.path());

        assert_eq!(
            rows,
            Some(vec![(1, "v2".to_string()), (2, "second".to_string())])
        );
        if shm_removed {
            assert!(!shm_path.exists(), "source -shm must not be recreated");
        }
        assert_eq!(
            temp_dirs, 0,
            "in-memory WAL merge must create 0 agy-pulse-db-* temp dirs"
        );
    }

    #[test]
    fn wal_checksum_big_endian_and_salt_mismatch_boundary() {
        // Test big-endian checksum calculation on an 8-byte block.
        let data = [0x01u8, 0x02, 0x03, 0x04, 0x10, 0x20, 0x30, 0x40];
        let w0 = u32::from_be_bytes([0x01, 0x02, 0x03, 0x04]);
        let w1 = u32::from_be_bytes([0x10, 0x20, 0x30, 0x40]);
        let expected_s0 = w0;
        let expected_s1 = w1.wrapping_add(expected_s0);
        assert_eq!(
            sqlite_wal_checksum(&data, (0, 0), true),
            (expected_s0, expected_s1)
        );

        // Build a synthetic DB + big-endian WAL where frame 1 commits and frame 2 has mismatched salt.
        let root = tempfile::tempdir().expect("tempdir");
        let db_path = root.path().join("be.db");
        {
            let conn = Connection::open(&db_path).expect("open");
            conn.execute("CREATE TABLE t (x INTEGER)", [])
                .expect("create");
            conn.execute("INSERT INTO t VALUES (42)", [])
                .expect("insert");
        }
        let mut base_bytes = fs::read(&db_path).expect("read db");
        assert!(base_bytes.len() >= 4096 * 2);

        // Create a modified page 2 in memory by inserting 99 into a clone, then extract page 2.
        let mod_path = root.path().join("be_mod.db");
        fs::write(&mod_path, &base_bytes).expect("write clone");
        {
            let conn = Connection::open(&mod_path).expect("open clone");
            conn.execute("UPDATE t SET x = 99", []).expect("update");
        }
        let mod_bytes = fs::read(&mod_path).expect("read mod");
        let ps = 4096usize;
        let db_pages = (mod_bytes.len() / ps) as u32;

        // Write a big-endian WAL header + frame 1 (valid commit) + frame 2 (salt mismatch).
        let salt1 = 0x1122_3344u32;
        let salt2 = 0x5566_7788u32;
        let mut hdr = [0u8; 32];
        hdr[0..4].copy_from_slice(&WAL_MAGIC_BIG_ENDIAN.to_be_bytes());
        hdr[4..8].copy_from_slice(&WAL_FORMAT_VERSION.to_be_bytes());
        hdr[8..12].copy_from_slice(&(ps as u32).to_be_bytes());
        hdr[12..16].copy_from_slice(&1u32.to_be_bytes());
        hdr[16..20].copy_from_slice(&salt1.to_be_bytes());
        hdr[20..24].copy_from_slice(&salt2.to_be_bytes());
        let (h_s0, h_s1) = sqlite_wal_checksum(&hdr[0..24], (0, 0), true);
        hdr[24..28].copy_from_slice(&h_s0.to_be_bytes());
        hdr[28..32].copy_from_slice(&h_s1.to_be_bytes());

        let mut fh1 = [0u8; 24];
        fh1[0..4].copy_from_slice(&2u32.to_be_bytes());
        fh1[4..8].copy_from_slice(&db_pages.to_be_bytes());
        fh1[8..12].copy_from_slice(&salt1.to_be_bytes());
        fh1[12..16].copy_from_slice(&salt2.to_be_bytes());
        let c1 = sqlite_wal_checksum(&fh1[0..8], (h_s0, h_s1), true);
        let c1 = sqlite_wal_checksum(&mod_bytes[ps..2 * ps], c1, true);
        fh1[16..20].copy_from_slice(&c1.0.to_be_bytes());
        fh1[20..24].copy_from_slice(&c1.1.to_be_bytes());

        // Frame 2: wrong salt1 -> must stop and keep frame 1's committed state (99).
        let mut fh2 = [0u8; 24];
        fh2[0..4].copy_from_slice(&2u32.to_be_bytes());
        fh2[4..8].copy_from_slice(&db_pages.to_be_bytes());
        fh2[8..12].copy_from_slice(&(salt1 ^ 1).to_be_bytes());
        fh2[12..16].copy_from_slice(&salt2.to_be_bytes());

        base_bytes[18] = 2;
        base_bytes[19] = 2;
        fs::write(&db_path, &base_bytes).expect("write db");
        let wal_path = sidecar_path(&db_path, "-wal");
        let mut wf = File::create(&wal_path).expect("create wal");
        wf.write_all(&hdr).expect("hdr");
        wf.write_all(&fh1).expect("fh1");
        wf.write_all(&mod_bytes[ps..2 * ps]).expect("page2");
        wf.write_all(&fh2).expect("fh2");
        wf.write_all(&base_bytes[ps..2 * ps]).expect("stale page2");
        drop(wf);

        let val = with_readonly_db(&db_path, |conn| {
            conn.query_row("SELECT x FROM t", [], |r| r.get::<_, i64>(0))
        });
        assert_eq!(val, Some(99));
    }

    #[test]
    fn temp_copy_fallback_reads_active_wal_with_no_ckpt_on_close() {
        let root = tempfile::tempdir().expect("tempdir");
        let db_path = root.path().join("fallback.db");
        let writer = Connection::open(&db_path).expect("open");
        writer
            .pragma_update(None, "journal_mode", "WAL")
            .expect("wal");
        writer
            .pragma_update(None, "wal_autocheckpoint", 0)
            .expect("no autockpt");
        writer
            .execute("CREATE TABLE kv (k TEXT PRIMARY KEY, v INTEGER)", [])
            .expect("create");
        writer
            .execute("INSERT INTO kv (k, v) VALUES ('alpha', 777)", [])
            .expect("insert");

        let val = with_temp_copy_fallback(&db_path, &|conn| {
            conn.query_row("SELECT v FROM kv WHERE k = ?1", params!["alpha"], |r| {
                r.get::<_, i64>(0)
            })
        });
        assert_eq!(val, Some(777));
    }

    #[test]
    fn cleanup_stale_temp_db_dirs_removes_stale_and_preserves_fresh() {
        let root = tempfile::tempdir().expect("tempdir");
        let mut env_ov = HashMap::new();
        env_ov.insert(
            "AGY_PULSE_DATA_DIR".to_string(),
            Some(root.path().join("data").to_string_lossy().into_owned()),
        );
        for var in ["TMPDIR", "TMP", "TEMP"] {
            env_ov.insert(
                var.to_string(),
                Some(root.path().to_string_lossy().into_owned()),
            );
        }

        let stale = root.path().join("agy-pulse-db-stale-test");
        let fresh = root.path().join("agy-pulse-db-fresh-test");
        let unrelated = root.path().join("other-dir-test");
        fs::create_dir_all(&stale).expect("stale");
        fs::write(stale.join("x.db"), b"123").expect("write stale file");
        fs::create_dir_all(&fresh).expect("fresh");
        fs::create_dir_all(&unrelated).expect("unrelated");

        // Set mtime via `filetime` isn't a dep; use `set_modified` on directory handle on Unix.
        #[cfg(unix)]
        if let Ok(stale_file) = fs::OpenOptions::new().read(true).open(&stale) {
            let old_time = SystemTime::now() - Duration::from_secs(600);
            let _ = stale_file.set_modified(old_time);
        }

        // If the platform doesn't allow setting directory mtime via File::set_modified,
        // verify at least via `max_age = Duration::from_secs(60)` vs `Duration::ZERO`.
        let stale_mtime = fs::metadata(&stale).unwrap().modified().unwrap();
        let stale_is_old = SystemTime::now()
            .duration_since(stale_mtime)
            .unwrap_or_default()
            >= Duration::from_secs(300);

        let removed = crate::platform::with_env_overrides(&env_ov, || {
            if stale_is_old {
                cleanup_stale_temp_db_dirs(Duration::from_secs(300))
            } else {
                0
            }
        });
        if stale_is_old {
            assert_eq!(removed, 1);
            assert!(!stale.exists());
            assert!(fresh.exists());
            assert!(unrelated.exists());
        } else {
            let removed_all = crate::platform::with_env_overrides(&env_ov, || {
                cleanup_stale_temp_db_dirs(Duration::ZERO)
            });
            assert!(removed_all >= 2);
            assert!(!stale.exists());
            assert!(!fresh.exists());
            assert!(unrelated.exists());
        }
    }
}
