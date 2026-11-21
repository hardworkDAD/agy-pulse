#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

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

/// Write a diagnostic line to `agy-pulse-error.log` safely without following symlinks.
pub fn write_error_log_safely(message: &str) {
    let log_path = resolve_antigravity_path("agy-pulse-error.log");
    #[cfg(unix)]
    unix::write_file_0600_no_symlink(&log_path, message);
    #[cfg(windows)]
    windows::write_file_0600_no_symlink(&log_path, message);
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

/// Atomically write a cache file with restricted permissions via sibling temporary file + rename.
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
