use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

pub(super) fn detect_unicode_from_os() -> bool {
    let locale = env::var("LC_ALL")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| env::var("LC_CTYPE").ok().filter(|s| !s.is_empty()))
        .or_else(|| env::var("LANG").ok().filter(|s| !s.is_empty()))
        .unwrap_or_default();

    if locale.is_empty() {
        return true;
    }
    let lower = locale.to_ascii_lowercase();
    if lower.contains("utf-8") || lower.contains("utf8") {
        return true;
    }
    if locale.to_ascii_uppercase().contains("POSIX") {
        return false;
    }
    false
}

pub(super) fn detect_live_tty_columns() -> Option<u16> {
    for fd in [std::io::stdout().as_raw_fd(), std::io::stderr().as_raw_fd()] {
        if let Some(cols) = query_fd_winsize(fd) {
            return Some(cols);
        }
    }
    if let Ok(tty) = OpenOptions::new().read(true).open("/dev/tty")
        && let Some(cols) = query_fd_winsize(tty.as_raw_fd())
    {
        return Some(cols);
    }
    None
}

fn query_fd_winsize(fd: i32) -> Option<u16> {
    let mut ws = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `ws` is a valid mutable `libc::winsize` struct passed to TIOCGWINSZ ioctl.
    let rc = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) };
    if rc == 0 && ws.ws_col > 0 {
        Some(ws.ws_col)
    } else {
        None
    }
}

pub(super) fn write_file_0600_no_symlink(path: &Path, message: &str) {
    if let Some(parent) = path.parent() {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        let _ = builder.create(parent);
    }
    if let Ok(meta) = fs::symlink_metadata(path)
        && meta.file_type().is_symlink()
    {
        let _ = fs::remove_file(path);
    }
    let open_res = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path);
    if let Ok(mut file) = open_res {
        let _ = file.write_all(message.as_bytes());
    }
}
