#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

static DEVICE_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Supported desktop operating systems for launcher & shortcut path conventions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopOs {
    Linux,
    Macos,
    Windows,
}

/// Return the current compilation target desktop OS family.
pub fn current_desktop_os() -> DesktopOs {
    #[cfg(target_os = "macos")]
    {
        DesktopOs::Macos
    }
    #[cfg(target_os = "windows")]
    {
        DesktopOs::Windows
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        DesktopOs::Linux
    }
}

/// Terminal rendering capabilities probed from OS environment and TTY state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCaps {
    pub unicode: bool,
    pub columns: Option<u16>,
}

/// Adjust live TTY width by reserving 3 columns on wide terminals (>82 cols), matching v1.
pub fn adjust_live_terminal_columns(raw_cols: u16) -> Option<u16> {
    if raw_cols == 0 {
        None
    } else if raw_cols > 82 {
        Some(raw_cols - 3)
    } else {
        Some(raw_cols)
    }
}

/// Detect whether the current terminal supports UTF-8 box-drawing and emoji glyphs.
pub fn detect_unicode_support() -> bool {
    if env::var("AGY_PULSE_FORCE_ASCII").ok().as_deref() == Some("1") {
        return false;
    }
    if env::var("AGY_PULSE_FORCE_UNICODE").ok().as_deref() == Some("1") {
        return true;
    }
    #[cfg(unix)]
    {
        unix::detect_unicode_from_os()
    }
    #[cfg(windows)]
    {
        windows::detect_unicode_from_os()
    }
}

/// Resolve terminal column width and Unicode capability using config, env, and TTY fallback.
pub fn detect_terminal_caps(
    explicit_width: Option<u16>,
    config_width: Option<u16>,
    config_unicode: Option<bool>,
) -> TerminalCaps {
    let unicode = config_unicode.unwrap_or_else(detect_unicode_support);
    let columns = explicit_width
        .filter(|&w| w > 0)
        .or_else(|| config_width.filter(|&w| w > 0))
        .or_else(|| {
            env::var("COLUMNS")
                .ok()
                .and_then(|s| s.trim().parse::<u16>().ok())
                .filter(|&w| w > 0)
        })
        .or_else(|| {
            #[cfg(unix)]
            let raw = unix::detect_live_tty_columns();
            #[cfg(windows)]
            let raw = windows::detect_live_tty_columns();
            raw.and_then(adjust_live_terminal_columns)
        });

    TerminalCaps { unicode, columns }
}

/// Detect live terminal row count from OS TTY/console if available.
pub fn detect_live_tty_rows() -> Option<u16> {
    #[cfg(unix)]
    {
        unix::detect_live_tty_rows()
    }
    #[cfg(windows)]
    {
        windows::detect_live_tty_rows()
    }
}

/// Returns `true` when both standard input and standard output are interactive terminals.
pub fn is_stdin_stdout_tty() -> bool {
    #[cfg(unix)]
    {
        unix::is_stdin_stdout_tty()
    }
    #[cfg(windows)]
    {
        windows::is_stdin_stdout_tty()
    }
}

/// RAII guard that enters raw terminal input mode and restores original settings on `Drop`.
pub struct RawTerminalGuard {
    #[cfg(unix)]
    _inner: unix::RawTerminalGuardImpl,
    #[cfg(windows)]
    _inner: windows::RawTerminalGuardImpl,
}

impl RawTerminalGuard {
    /// Switch standard input into unbuffered raw mode until the guard is dropped.
    pub fn enter() -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self {
                _inner: unix::RawTerminalGuardImpl::enter()?,
            })
        }
        #[cfg(windows)]
        {
            Ok(Self {
                _inner: windows::RawTerminalGuardImpl::enter()?,
            })
        }
    }
}

/// Resolve current user's home directory from environment variables.
pub fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .or_else(|| env::var_os("USERPROFILE").filter(|v| !v.is_empty()))
        .map(PathBuf::from)
}

/// Return ordered candidate `antigravity-cli` data directories.
pub fn get_antigravity_roots() -> Vec<PathBuf> {
    if let Ok(override_dir) = env::var("AGY_PULSE_DATA_DIR") {
        let trimmed = override_dir.trim();
        if !trimmed.is_empty() {
            return vec![PathBuf::from(trimmed)];
        }
    }

    let mut roots = Vec::new();
    if let Some(home) = home_dir() {
        roots.push(home.join(".gemini").join("antigravity-cli"));
    }
    for (var, suffix) in [
        ("XDG_DATA_HOME", "antigravity-cli"),
        ("APPDATA", "antigravity-cli"),
        ("LOCALAPPDATA", "antigravity-cli"),
    ] {
        if let Ok(val) = env::var(var) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                roots.push(PathBuf::from(trimmed).join(suffix));
            }
        }
    }
    roots
}

fn normalize_lexical_path(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir().unwrap_or_default().join(path)
    };
    let mut out = PathBuf::new();
    for comp in joined.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Return ordered candidate Antigravity IDE data directories (`~/.gemini/antigravity`, etc.).
/// Returns an empty list when `AGY_PULSE_DATA_DIR` is set without `AGY_PULSE_IDE_DATA_DIR`
/// so isolated sandboxes/tests never accidentally scan the host's real IDE directory.
pub fn get_antigravity_ide_roots() -> Vec<PathBuf> {
    if let Ok(override_dir) = env::var("AGY_PULSE_IDE_DATA_DIR") {
        let trimmed = override_dir.trim();
        if !trimmed.is_empty() {
            return vec![PathBuf::from(trimmed)];
        }
    }

    if let Ok(cli_override) = env::var("AGY_PULSE_DATA_DIR")
        && !cli_override.trim().is_empty()
    {
        return Vec::new();
    }

    let mut raw_candidates = Vec::new();
    if let Some(home) = home_dir() {
        raw_candidates.push(home.join(".gemini").join("antigravity"));
    }
    for (var, suffix) in [
        ("XDG_DATA_HOME", "antigravity"),
        ("APPDATA", "antigravity"),
        ("LOCALAPPDATA", "antigravity"),
    ] {
        if let Ok(val) = env::var(var) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                raw_candidates.push(PathBuf::from(trimmed).join(suffix));
            }
        }
    }

    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut deduped = Vec::new();
    for candidate in raw_candidates {
        let key = fs::canonicalize(&candidate).unwrap_or_else(|_| normalize_lexical_path(&candidate));
        if seen.insert(key) {
            deduped.push(candidate);
        }
    }
    deduped
}

/// Create directory recursively with owner-only (`0700` on Unix) permissions.
pub fn ensure_dir_0700(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        unix::ensure_dir_0700(path)
    }
    #[cfg(windows)]
    {
        windows::ensure_dir_0700(path)
    }
}

/// Restrict file permissions to owner read/write (`0600` on Unix).
pub fn set_file_0600(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        unix::set_file_0600(path)
    }
    #[cfg(windows)]
    {
        windows::set_file_0600(path)
    }
}

/// Resolve a relative file path under the highest-priority existing Antigravity data root.
pub fn resolve_antigravity_path(relative: &str) -> PathBuf {
    let roots = get_antigravity_roots();
    for root in &roots {
        let candidate = root.join(relative);
        if candidate.exists() {
            return candidate;
        }
    }
    if let Some(first) = roots.first() {
        first.join(relative)
    } else {
        PathBuf::from(relative)
    }
}

/// Resolve the global multi-device sync configuration file path (`agy-pulse-sync.json`).
/// Honors `AGY_PULSE_SYNC_CONFIG` when set and non-empty; never reads workspace directories.
pub fn resolve_sync_config_path() -> PathBuf {
    if let Ok(override_path) = env::var("AGY_PULSE_SYNC_CONFIG") {
        let trimmed = override_path.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    resolve_antigravity_path("agy-pulse-sync.json")
}

/// Sanitize a custom or host device name by stripping control characters and trimming whitespace.
pub fn sanitize_device_name(raw: &str) -> Option<String> {
    let cleaned: String = raw.chars().filter(|c| !c.is_control()).collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Read optional `(defaultAccount, deviceName)` identity hints from `agy-pulse-sync.json`.
/// Missing files, I/O errors, or malformed JSON silently return `(None, None)`.
pub fn read_sync_identity_hints() -> (Option<String>, Option<String>) {
    let path = resolve_sync_config_path();
    let Ok(raw) = fs::read_to_string(&path) else {
        return (None, None);
    };
    let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return (None, None);
    };
    let Some(obj) = val.as_object() else {
        return (None, None);
    };

    let default_account = obj
        .get("defaultAccount")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase)
        .filter(|s| s != "unassigned");

    let device_name = obj
        .get("deviceName")
        .and_then(|v| v.as_str())
        .and_then(sanitize_device_name);

    (default_account, device_name)
}

/// Resolve the directory containing the global `agy-pulse.config.json`.
pub fn resolve_global_config_dir() -> PathBuf {
    if let Ok(override_dir) = env::var("AGY_PULSE_GLOBAL_CONFIG_DIR") {
        let trimmed = override_dir.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    let roots = get_antigravity_roots();
    for root in &roots {
        for rel in ["agy-pulse-runtime/runtime", "agy-pulse-runtime", ""] {
            let dir = if rel.is_empty() {
                root.clone()
            } else {
                root.join(rel)
            };
            if dir.join("agy-pulse.config.json").exists() {
                return dir;
            }
        }
    }
    if let Some(first) = roots.first() {
        first.join("agy-pulse-runtime")
    } else {
        PathBuf::from(".")
    }
}

/// Read current locale string from `LC_ALL`, `LC_CTYPE`, or `LANG`.
pub fn read_locale_env() -> Option<String> {
    for var in ["LC_ALL", "LC_CTYPE", "LANG"] {
        if let Ok(val) = env::var(var) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

/// Read optional language override from `AGY_PULSE_LANGUAGE`.
pub fn read_language_env_override() -> Option<String> {
    env::var("AGY_PULSE_LANGUAGE")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Read OS username fallback from environment.
pub fn current_username() -> Option<String> {
    for var in ["USER", "USERNAME", "LOGNAME"] {
        if let Ok(val) = env::var(var) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn sanitize_hostname(raw: &str) -> Option<String> {
    let cleaned = sanitize_device_name(raw)?;
    let truncated: String = cleaned.chars().take(64).collect();
    let final_trimmed = truncated.trim();
    if final_trimmed.is_empty() {
        None
    } else {
        Some(final_trimmed.to_string())
    }
}

/// Detect the current machine hostname, stripped of control characters and capped at 64 chars.
pub fn current_hostname() -> Option<String> {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        // SAFETY: `buf` is a valid mutable stack buffer passed to POSIX `gethostname`.
        let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast::<libc::c_char>(), buf.len()) };
        if rc == 0 {
            let raw = match std::ffi::CStr::from_bytes_until_nul(&buf) {
                Ok(cstr) => cstr.to_string_lossy().into_owned(),
                Err(_) => String::from_utf8_lossy(&buf).into_owned(),
            };
            if let Some(cleaned) = sanitize_hostname(&raw) {
                return Some(cleaned);
            }
        }
        if let Ok(val) = env::var("HOSTNAME")
            && let Some(cleaned) = sanitize_hostname(&val)
        {
            return Some(cleaned);
        }
        if let Ok(contents) = fs::read_to_string("/etc/hostname")
            && let Some(cleaned) = sanitize_hostname(&contents)
        {
            return Some(cleaned);
        }
        None
    }
    #[cfg(windows)]
    {
        env::var("COMPUTERNAME")
            .ok()
            .and_then(|val| sanitize_hostname(&val))
    }
}

/// Generate an immutable random device identifier formatted as `dev_<16 hex>`.
pub fn generate_device_id() -> String {
    let seq = DEVICE_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut hasher = Sha256::new();

    #[cfg(unix)]
    let used_urandom = {
        use std::io::Read;
        let mut buf = [0u8; 16];
        if let Ok(mut f) = fs::File::open("/dev/urandom")
            && f.read_exact(&mut buf).is_ok()
        {
            hasher.update(buf);
            hasher.update(seq.to_le_bytes());
            true
        } else {
            false
        }
    };
    #[cfg(not(unix))]
    let used_urandom = false;

    if !used_urandom {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        hasher.update(nanos.to_le_bytes());
        hasher.update(std::process::id().to_le_bytes());
        hasher.update(seq.to_le_bytes());
        if let Some(host) = current_hostname() {
            hasher.update(host.as_bytes());
        }
    }

    let digest = hasher.finalize();
    format!("dev_{}", hex::encode(&digest[..8]))
}

/// Write a diagnostic line to `agy-pulse-error.log` safely without following symlinks.
pub fn write_error_log_safely(message: &str) {
    let log_path = resolve_antigravity_path("agy-pulse-error.log");
    #[cfg(unix)]
    unix::write_file_0600_no_symlink(&log_path, message);
    #[cfg(windows)]
    windows::write_file_0600_no_symlink(&log_path, message);
}

/// Append `message` to `path` with `0600` permissions (Unix) without following symlinks.
/// Creates missing parent directories with `0700` permissions (Unix) and skips writing
/// if `path` is already a symlink.
pub fn append_file_0600_no_symlink(path: &Path, message: &str) {
    #[cfg(unix)]
    unix::append_file_0600_no_symlink(path, message);
    #[cfg(windows)]
    windows::append_file_0600_no_symlink(path, message);
}

/// Return a platform-specific unique file identifier (inode on Unix, 0 fallback on Windows).
pub fn file_unique_id(meta: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        unix::file_unique_id(meta)
    }
    #[cfg(windows)]
    {
        windows::file_unique_id(meta)
    }
}

/// Atomically create a new file with `0600` permissions (Unix) using `O_CREAT | O_EXCL` semantics.
/// Returns `Ok(true)` if created, `Ok(false)` if the file already exists.
pub fn create_exclusive_file_0600(path: &Path, bytes: &[u8]) -> io::Result<bool> {
    #[cfg(unix)]
    {
        unix::create_exclusive_file_0600(path, bytes)
    }
    #[cfg(windows)]
    {
        windows::create_exclusive_file_0600(path, bytes)
    }
}

/// Refresh a file's modification timestamp (`mtime`) to the current wall-clock time.
pub fn touch_file_mtime(path: &Path) -> io::Result<()> {
    let file = fs::OpenOptions::new().write(true).open(path)?;
    file.set_modified(std::time::SystemTime::now())
}

/// Atomically write a durable file with restricted permissions via sibling temporary file + `fsync` + rename.
pub fn write_atomic_0600(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        unix::write_atomic_0600(path, bytes)
    }
    #[cfg(windows)]
    {
        windows::write_atomic_0600(path, bytes)
    }
}

/// Atomically write an ephemeral cache file with restricted permissions without calling `fsync`.
pub fn write_cache_atomic_0600(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        unix::write_cache_atomic_0600(path, bytes)
    }
    #[cfg(windows)]
    {
        windows::write_cache_atomic_0600(path, bytes)
    }
}

/// Resolve platform-appropriate `claude_desktop_config.json` path if discoverable.
pub fn resolve_claude_desktop_config_path() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        unix::resolve_claude_desktop_config_path()
    }
    #[cfg(windows)]
    {
        windows::resolve_claude_desktop_config_path()
    }
}

/// Run a local command with a hard timeout, returning trimmed stdout if exit code is 0.
pub fn run_command_with_timeout(
    program: &str,
    args: &[&str],
    cwd: &Path,
    timeout: Duration,
) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let out = child.wait_with_output().ok()?;
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                return if s.is_empty() { None } else { Some(s) };
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(_) => return None,
        }
    }
}

/// Spawn a detached background child process with stdio closed, returning its PID.
pub fn spawn_detached(program: &Path, args: &[&str]) -> io::Result<u32> {
    #[cfg(unix)]
    {
        unix::spawn_detached(program, args)
    }
    #[cfg(windows)]
    {
        windows::spawn_detached(program, args)
    }
}

/// Check whether an OS process with the given PID currently exists.
pub fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unix::is_process_alive(pid)
    }
    #[cfg(windows)]
    {
        windows::is_process_alive(pid)
    }
}

/// Exit without running cleanup that may wait on a blocked thread or stream.
pub(crate) fn exit_immediately(code: i32) -> ! {
    #[cfg(unix)]
    // SAFETY: _exit terminates the current process without invoking exit handlers.
    unsafe {
        libc::_exit(code);
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};

        // SAFETY: The pseudo-handle refers to the current process and needs no cleanup.
        unsafe { TerminateProcess(GetCurrentProcess(), code as u32) };
        std::process::exit(code);
    }
}

/// Request graceful termination (SIGTERM on Unix, TerminateProcess on Windows) for a PID.
pub fn terminate_process(pid: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        unix::terminate_process(pid)
    }
    #[cfg(windows)]
    {
        windows::terminate_process(pid)
    }
}

/// Install lightweight SIGINT/SIGTERM shutdown handler for long-running server processes.
pub fn install_shutdown_signal_handler() {
    #[cfg(unix)]
    unix::install_shutdown_signal_handler();
    #[cfg(windows)]
    windows::install_shutdown_signal_handler();
}

/// Return `true` if a process shutdown signal (SIGINT/SIGTERM) has been observed.
pub fn shutdown_signal_requested() -> bool {
    #[cfg(unix)]
    {
        unix::shutdown_signal_requested()
    }
    #[cfg(windows)]
    {
        windows::shutdown_signal_requested()
    }
}

/// Set executable (`0755`) permissions on Unix; no-op on Windows.
pub fn set_executable_0755(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        unix::set_executable_0755(path)
    }
    #[cfg(windows)]
    {
        windows::set_executable_0755(path)
    }
}

/// Atomically replace an executable at `target_path` using `source_file`
/// (POSIX `0755` sibling temp + rename; Windows rename-running-`.exe`-to-`.old` + rename).
pub fn replace_executable_atomically(source_file: &Path, target_path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        unix::replace_executable_atomically(source_file, target_path)
    }
    #[cfg(windows)]
    {
        windows::replace_executable_atomically(source_file, target_path)
    }
}

/// Remove a binary or shim file safely, scheduling delayed removal on Windows if locked by self.
pub fn remove_binary_file_safely(path: &Path) -> io::Result<bool> {
    #[cfg(unix)]
    {
        unix::remove_binary_file_safely(path)
    }
    #[cfg(windows)]
    {
        windows::remove_binary_file_safely(path)
    }
}

/// Return the standard 5-platform release archive filename for the current build target.
pub fn current_release_asset_name() -> Option<&'static str> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        Some("agy-pulse-linux-x86_64.tar.gz")
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        Some("agy-pulse-linux-aarch64.tar.gz")
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        Some("agy-pulse-macos-x86_64.tar.gz")
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        Some("agy-pulse-macos-aarch64.tar.gz")
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        Some("agy-pulse-windows-x86_64.zip")
    }
    #[cfg(not(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "windows", target_arch = "x86_64"),
    )))]
    {
        None
    }
}

/// Primary binary filename for the active OS (`agy-pulse` or `agy-pulse.exe`).
pub fn primary_binary_name() -> &'static str {
    if current_desktop_os() == DesktopOs::Windows {
        "agy-pulse.exe"
    } else {
        "agy-pulse"
    }
}

/// Alias binary filename for the active OS (`agypl` or `agypl.exe`).
pub fn alias_binary_name() -> &'static str {
    if current_desktop_os() == DesktopOs::Windows {
        "agypl.exe"
    } else {
        "agypl"
    }
}

/// Resolve the isolated user CLI binary directory (`AGY_PULSE_BIN_DIR`, else `~/.local/bin` or `%LOCALAPPDATA%\agy-pulse\bin`).
/// Never resolves to official `agy` program directories.
pub fn resolve_cli_bin_dir(explicit: Option<&Path>) -> PathBuf {
    if let Some(dir) = explicit {
        return dir.to_path_buf();
    }
    if let Ok(val) = env::var("AGY_PULSE_BIN_DIR") {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    if current_desktop_os() == DesktopOs::Windows {
        return windows_local_app_data()
            .or_else(|| home_dir().map(|h| h.join("AppData").join("Local")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("agy-pulse")
            .join("bin");
    }
    home_dir()
        .map(|h| h.join(".local").join("bin"))
        .unwrap_or_else(|| PathBuf::from(".local").join("bin"))
}

/// Return deduplicated candidate directories where user-installed `agy-pulse` / `agypl` binaries may live.
pub fn cli_shim_candidate_dirs(explicit: Option<&Path>) -> Vec<PathBuf> {
    let mut raw = Vec::new();
    if let Some(dir) = explicit {
        raw.push(dir.to_path_buf());
    }
    if let Ok(val) = env::var("AGY_PULSE_BIN_DIR") {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            raw.push(PathBuf::from(trimmed));
        }
    }
    if let Some(home) = home_dir() {
        raw.push(home.join(".local").join("bin"));
    }
    if current_desktop_os() == DesktopOs::Windows
        && let Some(local_app) = windows_local_app_data()
    {
        raw.push(local_app.join("agy-pulse").join("bin"));
    }

    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for dir in raw {
        let key = fs::canonicalize(&dir).unwrap_or_else(|_| normalize_lexical_path(&dir));
        if seen.insert(key) {
            out.push(dir);
        }
    }
    out
}

/// Resolve an executable on `PATH` safely without searching relative directories such as `.`.
pub fn resolve_safe_executable(name: &str) -> Option<PathBuf> {
    let path_os = env::var_os("PATH")?;
    let exts: &[&str] = if current_desktop_os() == DesktopOs::Windows {
        &["", ".exe", ".cmd", ".bat"]
    } else {
        &[""]
    };

    for dir in env::split_paths(&path_os) {
        if dir.as_os_str().is_empty() || dir == Path::new(".") || !dir.is_absolute() {
            continue;
        }
        for ext in exts {
            let candidate = dir.join(format!("{name}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Resolve `$XDG_DATA_HOME` or fallback `$HOME/.local/share`.
pub fn xdg_data_home() -> Option<PathBuf> {
    if let Ok(val) = env::var("XDG_DATA_HOME") {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }
    home_dir().map(|h| h.join(".local").join("share"))
}

/// Resolve Windows `%LOCALAPPDATA%` or fallback `%USERPROFILE%\AppData\Local`.
pub fn windows_local_app_data() -> Option<PathBuf> {
    for var in ["LOCALAPPDATA", "LocalAppData"] {
        if let Ok(val) = env::var(var) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Some(PathBuf::from(trimmed));
            }
        }
    }
    home_dir().map(|h| h.join("AppData").join("Local"))
}

/// Resolve Windows `%APPDATA%` or fallback `%USERPROFILE%\AppData\Roaming`.
pub fn windows_app_data() -> Option<PathBuf> {
    for var in ["APPDATA", "AppData"] {
        if let Ok(val) = env::var(var) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Some(PathBuf::from(trimmed));
            }
        }
    }
    home_dir().map(|h| h.join("AppData").join("Roaming"))
}

/// Probe installed Chromium-family browsers capable of `--app=<url>` standalone windows.
pub fn detect_app_window_browser_path(os: DesktopOs) -> Option<(String, PathBuf)> {
    match os {
        DesktopOs::Macos => {
            let mut dirs = vec![PathBuf::from("/Applications")];
            if let Some(home) = home_dir() {
                dirs.push(home.join("Applications"));
            }
            for app_name in [
                "Google Chrome.app",
                "Microsoft Edge.app",
                "Brave Browser.app",
                "Chromium.app",
            ] {
                for dir in &dirs {
                    let app_path = dir.join(app_name);
                    if app_path.exists() {
                        let short = app_name.strip_suffix(".app").unwrap_or(app_name);
                        return Some((short.to_string(), app_path));
                    }
                }
            }
            None
        }
        DesktopOs::Windows => {
            let mut roots = Vec::new();
            for var in [
                "PROGRAMFILES",
                "ProgramFiles",
                "PROGRAMFILES(X86)",
                "ProgramFiles(x86)",
                "LOCALAPPDATA",
                "LocalAppData",
            ] {
                if let Ok(val) = env::var(var) {
                    let trimmed = val.trim();
                    if !trimmed.is_empty() {
                        roots.push(PathBuf::from(trimmed));
                    }
                }
            }
            let browsers: [(&str, &[&str]); 3] = [
                ("chrome", &["Google", "Chrome", "Application", "chrome.exe"]),
                (
                    "msedge",
                    &["Microsoft", "Edge", "Application", "msedge.exe"],
                ),
                (
                    "brave",
                    &["BraveSoftware", "Brave-Browser", "Application", "brave.exe"],
                ),
            ];
            for (name, segments) in browsers {
                for root in &roots {
                    let mut p = root.clone();
                    for seg in segments {
                        p.push(seg);
                    }
                    if p.is_file() {
                        return Some((name.to_string(), p));
                    }
                }
            }
            None
        }
        DesktopOs::Linux => {
            for name in [
                "google-chrome",
                "google-chrome-stable",
                "chromium",
                "chromium-browser",
                "microsoft-edge",
                "microsoft-edge-stable",
                "brave-browser",
                "vivaldi",
            ] {
                if let Some(resolved) = resolve_safe_executable(name) {
                    return Some((name.to_string(), resolved));
                }
                let usr_bin = PathBuf::from(format!("/usr/bin/{name}"));
                if usr_bin.is_file() {
                    return Some((name.to_string(), usr_bin));
                }
            }
            for (name, opt_path) in [
                ("google-chrome", "/opt/google/chrome/google-chrome"),
                ("microsoft-edge", "/opt/microsoft/msedge/microsoft-edge"),
            ] {
                let p = PathBuf::from(opt_path);
                if p.is_file() {
                    return Some((name.to_string(), p));
                }
            }
            None
        }
    }
}

/// Open a URL in the host operating system's default browser without blocking.
pub fn open_url_default_browser(os: DesktopOs, url: &str) -> bool {
    let res = match os {
        DesktopOs::Macos => spawn_detached(Path::new("open"), &[url]),
        DesktopOs::Windows => spawn_detached(Path::new("cmd"), &["/c", "start", "", url]),
        DesktopOs::Linux => spawn_detached(Path::new("xdg-open"), &[url]),
    };
    res.is_ok()
}

/// Run a non-critical desktop integration helper silently (e.g. `update-desktop-database` or `powershell.exe`).
pub fn run_silent_helper(program: &str, args: &[&str]) {
    let Some(resolved) = resolve_safe_executable(program) else {
        return;
    };
    if let Ok(mut child) = Command::new(resolved)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        let _ = child.wait();
    }
}
