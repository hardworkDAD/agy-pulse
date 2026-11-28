use std::fs::{self, Metadata, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static ATOMIC_TMP_COUNTER: AtomicU64 = AtomicU64::new(1);

pub(super) fn detect_unicode_from_os() -> bool {
    let locale = super::env_var("LC_ALL")
        .filter(|s| !s.is_empty())
        .or_else(|| super::env_var("LC_CTYPE").filter(|s| !s.is_empty()))
        .or_else(|| super::env_var("LANG").filter(|s| !s.is_empty()))
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
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        let _ = ensure_dir_0700(parent);
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
        let _ = file.set_permissions(fs::Permissions::from_mode(0o600));
        let _ = file.write_all(message.as_bytes());
    }
}

pub(super) fn append_file_0600_no_symlink(path: &Path, message: &str) {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        let _ = ensure_dir_0700(parent);
    }
    if let Ok(meta) = fs::symlink_metadata(path)
        && meta.file_type().is_symlink()
    {
        return;
    }
    let open_res = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path);
    if let Ok(mut file) = open_res {
        let _ = file.set_permissions(fs::Permissions::from_mode(0o600));
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
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    fs::set_permissions(path, perms)
}

pub(super) fn create_exclusive_file_0600(path: &Path, bytes: &[u8]) -> std::io::Result<bool> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        ensure_dir_0700(parent)?;
    }
    match OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
    {
        Ok(mut file) => {
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            file.write_all(bytes)?;
            Ok(true)
        }
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(err) => Err(err),
    }
}

pub(super) fn write_atomic_0600(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    write_atomic_0600_inner(path, bytes, true)
}

pub(super) fn write_cache_atomic_0600(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    write_atomic_0600_inner(path, bytes, false)
}

fn write_atomic_0600_inner(path: &Path, bytes: &[u8], sync_disk: bool) -> std::io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        ensure_dir_0700(parent)?;
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = ATOMIC_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("cache");
    let tmp_name = format!(".{file_name}.{}.{nanos}.{seq}.tmp", std::process::id());
    let tmp_path = path.with_file_name(tmp_name);

    let write_res = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&tmp_path)?;
        file.write_all(bytes)?;
        if sync_disk {
            file.sync_all()?;
        }
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        drop(file);
        fs::rename(&tmp_path, path)?;
        let mode = fs::symlink_metadata(path)?.permissions().mode() & 0o777;
        if mode != 0o600 {
            set_file_0600(path)?;
        }
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
        Some(
            home.join("Library")
                .join("Application Support")
                .join("Claude")
                .join("claude_desktop_config.json"),
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        let cfg_root = super::env_var("XDG_CONFIG_HOME")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        Some(cfg_root.join("Claude").join("claude_desktop_config.json"))
    }
}

pub(super) fn current_euid() -> u32 {
    // SAFETY: `geteuid()` has no preconditions and always succeeds.
    unsafe { libc::geteuid() }
}

pub(super) fn is_owned_dir_no_symlink(path: &Path, euid: u32) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|m| !m.file_type().is_symlink() && m.is_dir() && m.uid() == euid)
}

pub(super) fn ensure_owned_dir_0700(path: &Path, euid: u32) -> bool {
    if let Ok(meta) = fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() || !meta.is_dir() || meta.uid() != euid {
            return false;
        }
        if (meta.permissions().mode() & 0o077) != 0 {
            let mut perms = meta.permissions();
            perms.set_mode(0o700);
            if fs::set_permissions(path, perms).is_err() {
                return false;
            }
        }
        return true;
    }
    if ensure_dir_0700(path).is_err() {
        return false;
    }
    let Ok(meta) = fs::symlink_metadata(path) else {
        return false;
    };
    if meta.file_type().is_symlink() || !meta.is_dir() || meta.uid() != euid {
        return false;
    }
    if (meta.permissions().mode() & 0o077) != 0 {
        let mut perms = meta.permissions();
        perms.set_mode(0o700);
        if fs::set_permissions(path, perms).is_err() {
            return false;
        }
    }
    true
}

pub(super) fn resolve_non_linux_runtime_dir() -> PathBuf {
    let euid = current_euid();
    if let Some(xdg) = super::env_var("XDG_RUNTIME_DIR") {
        let trimmed = xdg.trim();
        if !trimmed.is_empty() {
            let xdg_path = PathBuf::from(trimmed);
            if is_owned_dir_no_symlink(&xdg_path, euid) {
                let candidate = xdg_path.join("agy-pulse");
                if ensure_owned_dir_0700(&candidate, euid) {
                    return candidate;
                }
            }
        }
    }
    let temp_candidate = super::temp_dir().join(format!("agy-pulse-{euid}"));
    if ensure_owned_dir_0700(&temp_candidate, euid) {
        return temp_candidate;
    }
    if let Some(home) = super::home_dir() {
        let private_candidate = home.join(".agy-pulse").join("runtime");
        if ensure_owned_dir_0700(&private_candidate, euid) {
            return private_candidate;
        }
    }
    if let Some(root) = super::get_antigravity_roots().first() {
        let root_candidate = root.join("runtime");
        if ensure_owned_dir_0700(&root_candidate, euid) {
            return root_candidate;
        }
    }
    let fallback_suffix = super::random_hex(4).unwrap_or_else(|_| std::process::id().to_string());
    let unique_temp = super::temp_dir().join(format!("agy-pulse-{euid}-{fallback_suffix}"));
    let _ = ensure_owned_dir_0700(&unique_temp, euid);
    unique_temp
}

#[cfg(target_os = "linux")]
pub(super) fn resolve_linux_runtime_dir() -> PathBuf {
    let euid = current_euid();
    if let Some(xdg) = super::env_var("XDG_RUNTIME_DIR") {
        let trimmed = xdg.trim();
        if !trimmed.is_empty() {
            let xdg_path = PathBuf::from(trimmed);
            if is_owned_dir_no_symlink(&xdg_path, euid) {
                let candidate = xdg_path.join("agy-pulse");
                if ensure_owned_dir_0700(&candidate, euid) {
                    return candidate;
                }
            }
        }
    }
    if fs::metadata("/dev/shm").is_ok_and(|m| m.is_dir()) {
        let candidate = PathBuf::from(format!("/dev/shm/agy-pulse-{euid}"));
        if ensure_owned_dir_0700(&candidate, euid) {
            return candidate;
        }
    }
    resolve_non_linux_runtime_dir()
}

#[cfg(target_os = "linux")]
pub(super) fn resolve_linux_fast_temp_dir() -> PathBuf {
    let euid = current_euid();
    if let Some(xdg) = super::env_var("XDG_RUNTIME_DIR") {
        let trimmed = xdg.trim();
        if !trimmed.is_empty() {
            let xdg_path = PathBuf::from(trimmed);
            if is_owned_dir_no_symlink(&xdg_path, euid) {
                let candidate = xdg_path.join("agy-pulse-tmp");
                if ensure_owned_dir_0700(&candidate, euid) {
                    return candidate;
                }
            }
        }
    }
    if fs::metadata("/dev/shm").is_ok_and(|m| m.is_dir()) {
        let candidate = PathBuf::from(format!("/dev/shm/agy-pulse-tmp-{euid}"));
        if ensure_owned_dir_0700(&candidate, euid) {
            return candidate;
        }
    }
    super::temp_dir()
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
    super::apply_thread_env_overrides(&mut cmd);
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

#[cfg(target_os = "linux")]
fn is_linux_zombie_or_dead(raw_pid: libc::pid_t) -> bool {
    fs::read_to_string(format!("/proc/{raw_pid}/stat")).is_ok_and(|stat| {
        stat.rfind(')')
            .and_then(|rparen| stat[rparen + 1..].trim_start().as_bytes().first())
            .is_some_and(|state| matches!(*state, b'Z' | b'X'))
    })
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
    let exists = rc == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
    #[cfg(target_os = "linux")]
    {
        exists && !is_linux_zombie_or_dead(raw_pid)
    }
    #[cfg(not(target_os = "linux"))]
    {
        exists
    }
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
