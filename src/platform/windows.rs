use std::env;
use std::fs::{self, Metadata, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use windows_sys::Win32::System::Console::{
    CONSOLE_SCREEN_BUFFER_INFO, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT,
    GetConsoleMode, GetConsoleOutputCP, GetConsoleScreenBufferInfo, GetStdHandle, STD_ERROR_HANDLE,
    STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleMode,
};

pub(super) fn detect_unicode_from_os() -> bool {
    // SAFETY: Win32 GetConsoleOutputCP takes no pointers and returns active codepage ID.
    unsafe { GetConsoleOutputCP() == 65001 }
}

pub(super) fn detect_live_tty_size() -> Option<(u16, u16)> {
    for std_handle in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: Querying standard console handle and populating stack-allocated buffer info.
        unsafe {
            let handle = GetStdHandle(std_handle);
            let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
            if GetConsoleScreenBufferInfo(handle, &mut info) != 0 {
                let width = info.srWindow.Right - info.srWindow.Left + 1;
                let height = info.srWindow.Bottom - info.srWindow.Top + 1;
                if width > 0 && height > 0 {
                    return Some((width as u16, height as u16));
                }
            }
        }
    }
    None
}

pub(super) fn detect_live_tty_columns() -> Option<u16> {
    detect_live_tty_size().map(|(cols, _)| cols)
}

pub(super) fn detect_live_tty_rows() -> Option<u16> {
    detect_live_tty_size().map(|(_, rows)| rows)
}

pub(super) fn is_stdin_stdout_tty() -> bool {
    // SAFETY: `GetStdHandle` and `GetConsoleMode` succeed only when attached to console handles.
    unsafe {
        let in_handle = GetStdHandle(STD_INPUT_HANDLE);
        let out_handle = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut in_mode: u32 = 0;
        let mut out_mode: u32 = 0;
        GetConsoleMode(in_handle, &mut in_mode) != 0
            && GetConsoleMode(out_handle, &mut out_mode) != 0
    }
}

pub(super) struct RawTerminalGuardImpl {
    orig_in_mode: u32,
}

impl RawTerminalGuardImpl {
    pub(super) fn enter() -> std::io::Result<Self> {
        // SAFETY: Queries and updates console input mode on `STD_INPUT_HANDLE`.
        unsafe {
            let in_handle = GetStdHandle(STD_INPUT_HANDLE);
            let mut orig_in_mode: u32 = 0;
            if GetConsoleMode(in_handle, &mut orig_in_mode) == 0 {
                return Err(std::io::Error::last_os_error());
            }
            let raw_mode =
                orig_in_mode & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT);
            if SetConsoleMode(in_handle, raw_mode) == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(Self { orig_in_mode })
        }
    }
}

impl Drop for RawTerminalGuardImpl {
    fn drop(&mut self) {
        // SAFETY: Restores original console input mode on `STD_INPUT_HANDLE`.
        unsafe {
            let in_handle = GetStdHandle(STD_INPUT_HANDLE);
            let _ = SetConsoleMode(in_handle, self.orig_in_mode);
        }
    }
}

pub(super) fn write_file_0600_no_symlink(path: &Path, message: &str) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(meta) = fs::symlink_metadata(path)
        && meta.file_type().is_symlink()
    {
        let _ = fs::remove_file(path);
    }
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
    {
        let _ = file.write_all(message.as_bytes());
    }
}

pub(super) fn file_unique_id(_meta: &Metadata) -> u64 {
    0
}

pub(super) fn ensure_dir_0700(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)
}

pub(super) fn set_file_0600(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

pub(super) fn write_atomic_0600(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
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
            .open(&tmp_path)?;
        file.write_all(bytes)?;
        fs::rename(&tmp_path, path)?;
        Ok(())
    })();

    if write_res.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    write_res
}

pub(super) fn resolve_claude_desktop_config_path() -> Option<PathBuf> {
    let appdata = env::var("APPDATA")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| super::home_dir().map(|h| h.join("AppData").join("Roaming")))?;
    Some(appdata.join("Claude").join("claude_desktop_config.json"))
}
