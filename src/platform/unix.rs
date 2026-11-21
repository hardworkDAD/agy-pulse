use std::env;
use std::fs::{self, Metadata, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
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

pub(super) fn create_exclusive_file_0600(path: &Path, bytes: &[u8]) -> std::io::Result<bool> {
    if let Some(parent) = path.parent() {
        ensure_dir_0700(parent)?;
    }
    match OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
    {
        Ok(mut file) => {
            file.write_all(bytes)?;
            Ok(true)
        }
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(err) => Err(err),
    }
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

static SHUTDOWN_SIGNAL_FLAG: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_shutdown_signal(_sig: libc::c_int) {
    SHUTDOWN_SIGNAL_FLAG.store(true, Ordering::Relaxed);
}

pub(super) fn install_shutdown_signal_handler() {
    SHUTDOWN_SIGNAL_FLAG.store(false, Ordering::Release);
    // SAFETY: `handle_shutdown_signal` only executes an async-signal-safe atomic store.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = handle_shutdown_signal as *const () as libc::sighandler_t;
        libc::sigemptyset(&mut sa.sa_mask);
        sa.sa_flags = 0;
        let _ = libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut());
        let _ = libc::sigaction(libc::SIGTERM, &sa, std::ptr::null_mut());
    }
}

pub(super) fn shutdown_signal_requested() -> bool {
    SHUTDOWN_SIGNAL_FLAG.load(Ordering::Acquire)
}

pub(super) fn spawn_detached(program: &Path, args: &[&str]) -> io::Result<u32> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: `libc::setsid()` is async-signal-safe and detaches the child into a new session.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn()?;
    Ok(child.id())
}

pub(super) fn is_process_alive(pid: u32) -> bool {
    let Ok(raw_pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if raw_pid <= 0 {
        return false;
    }
    // SAFETY: `kill(pid, 0)` checks process existence without sending a signal.
    let rc = unsafe { libc::kill(raw_pid, 0) };
    if rc == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

pub(super) fn terminate_process(pid: u32) -> io::Result<()> {
    let raw_pid = libc::pid_t::try_from(pid)
        .ok()
        .filter(|&p| p > 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid PID"))?;
    // SAFETY: sends SIGTERM to the specific target PID.
    let rc = unsafe { libc::kill(raw_pid, libc::SIGTERM) };
    if rc == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(err)
}

pub(super) fn set_executable_0755(path: &Path) -> io::Result<()> {
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)
}

pub(super) fn replace_executable_atomically(source_file: &Path, target_path: &Path) -> io::Result<()> {
    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let target_name = target_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("agy-pulse");
    let tmp_path = target_path.with_file_name(format!(
        ".{target_name}.update.{}.{nanos}.tmp",
        std::process::id()
    ));

    let res = (|| -> io::Result<()> {
        let mut src = fs::File::open(source_file)?;
        let mut dst = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o755)
            .open(&tmp_path)?;
        io::copy(&mut src, &mut dst)?;
        dst.sync_all()?;
        set_executable_0755(&tmp_path)?;
        fs::rename(&tmp_path, target_path)?;
        Ok(())
    })();

    if res.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    res
}

pub(super) fn remove_binary_file_safely(path: &Path) -> io::Result<bool> {
    if fs::symlink_metadata(path).is_err() {
        return Ok(false);
    }
    fs::remove_file(path)?;
    Ok(true)
}
