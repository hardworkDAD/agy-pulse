use std::env;
use std::fs::{self, Metadata, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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

pub(super) fn detect_live_tty_size() -> Option<(u16, u16)> {
    for fd in [std::io::stdout().as_raw_fd(), std::io::stderr().as_raw_fd()] {
        if let Some(size) = query_fd_winsize(fd) {
            return Some(size);
        }
    }
    if let Ok(tty) = OpenOptions::new().read(true).open("/dev/tty")
        && let Some(size) = query_fd_winsize(tty.as_raw_fd())
    {
        return Some(size);
    }
    None
}

pub(super) fn detect_live_tty_columns() -> Option<u16> {
    detect_live_tty_size().map(|(cols, _)| cols)
}

pub(super) fn detect_live_tty_rows() -> Option<u16> {
    detect_live_tty_size().map(|(_, rows)| rows)
}

fn query_fd_winsize(fd: i32) -> Option<(u16, u16)> {
    let mut ws = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `ws` is a valid mutable `libc::winsize` struct passed to TIOCGWINSZ ioctl.
    let rc = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) };
    if rc == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
        Some((ws.ws_col, ws.ws_row))
    } else {
        None
    }
}

pub(super) fn is_stdin_stdout_tty() -> bool {
    // SAFETY: `isatty` is safe for valid standard file descriptors.
    unsafe { libc::isatty(libc::STDIN_FILENO) != 0 && libc::isatty(libc::STDOUT_FILENO) != 0 }
}

pub(super) struct RawTerminalGuardImpl {
    fd: i32,
    original: libc::termios,
}

impl RawTerminalGuardImpl {
    pub(super) fn enter() -> std::io::Result<Self> {
        let fd = libc::STDIN_FILENO;
        // SAFETY: `original` is zero-initialized and populated by `tcgetattr` for `STDIN_FILENO`.
        let mut original: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: `tcgetattr` writes to valid stack struct `original`.
        if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut raw = original;
        raw.c_iflag &= !(libc::IXON | libc::ICRNL | libc::BRKINT | libc::INPCK | libc::ISTRIP);
        raw.c_cflag |= libc::CS8;
        raw.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG | libc::IEXTEN);
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        // SAFETY: `tcsetattr` applies valid `termios` struct `raw` to `STDIN_FILENO`.
        if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &raw) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self { fd, original })
    }
}

impl Drop for RawTerminalGuardImpl {
    fn drop(&mut self) {
        // SAFETY: restores original `termios` attributes captured at `enter()`.
        unsafe {
            let _ = libc::tcsetattr(self.fd, libc::TCSAFLUSH, &self.original);
        }
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

pub(super) fn file_unique_id(meta: &Metadata) -> u64 {
    meta.ino()
}

pub(super) fn ensure_dir_0700(path: &Path) -> std::io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(path)
}

pub(super) fn set_file_0600(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    fs::set_permissions(path, perms)
}

pub(super) fn write_atomic_0600(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(parent)?;
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("cache");
    let tmp_name = format!(".{file_name}.{}.{nanos}.tmp", std::process::id());
    let tmp_path = path.with_file_name(tmp_name);

    let write_res = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&tmp_path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp_path, path)?;
        Ok(())
    })();

    if write_res.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    write_res
}

pub(super) fn resolve_claude_desktop_config_path() -> Option<PathBuf> {
    let home = super::home_dir()?;
    #[cfg(target_os = "macos")]
    {
        return Some(
            home.join("Library")
                .join("Application Support")
                .join("Claude")
                .join("claude_desktop_config.json"),
        );
    }
    #[cfg(not(target_os = "macos"))]
    {
        let cfg_root = env::var("XDG_CONFIG_HOME")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        Some(cfg_root.join("Claude").join("claude_desktop_config.json"))
    }
}
