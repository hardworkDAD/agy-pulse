use std::fs::{self, Metadata, OpenOptions};
use std::io::{self, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, STILL_ACTIVE, SetHandleInformation,
};
use windows_sys::Win32::System::Console::{
    CONSOLE_SCREEN_BUFFER_INFO, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT,
    GetConsoleMode, GetConsoleOutputCP, GetConsoleScreenBufferInfo, GetStdHandle, STD_ERROR_HANDLE,
    STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleMode,
};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    TerminateProcess,
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
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
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

pub(super) fn create_exclusive_file_0600(path: &Path, bytes: &[u8]) -> std::io::Result<bool> {
    if let Some(parent) = path.parent() {
        ensure_dir_0700(parent)?;
    }
    match OpenOptions::new()
        .create_new(true)
        .write(true)
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
    write_atomic_0600_inner(path, bytes, true)
}

pub(super) fn write_cache_atomic_0600(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    write_atomic_0600_inner(path, bytes, false)
}

fn write_atomic_0600_inner(path: &Path, bytes: &[u8], sync_disk: bool) -> std::io::Result<()> {
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
        {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&tmp_path)?;
            file.write_all(bytes)?;
            if sync_disk {
                file.sync_all()?;
            }
        }
        fs::rename(&tmp_path, path)?;
        Ok(())
    })();

    if write_res.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    write_res
}

pub(super) fn resolve_claude_desktop_config_path() -> Option<PathBuf> {
    let appdata = super::env_var("APPDATA")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| super::home_dir().map(|h| h.join("AppData").join("Roaming")))?;
    Some(appdata.join("Claude").join("claude_desktop_config.json"))
}

const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub(super) fn install_shutdown_signal_handler() {}

pub(super) fn shutdown_signal_requested() -> bool {
    false
}

pub(super) fn spawn_detached(program: &Path, args: &[&str]) -> io::Result<u32> {
    // Clear inheritance on standard handles before spawning so a detached child
    // never keeps parent pipe write ends open (which would block `Command::output()`).
    for std_handle in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: Queries standard handles of the current process and clears `HANDLE_FLAG_INHERIT` when valid.
        unsafe {
            let handle = GetStdHandle(std_handle);
            if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
                let _ = SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    super::apply_thread_env_overrides(&mut cmd);
    let child = cmd.spawn()?;
    Ok(child.id())
}

pub(super) fn is_process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: `OpenProcess` queries process exit code and `CloseHandle` frees the handle.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut exit_code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &mut exit_code);
        CloseHandle(handle);
        ok != 0 && exit_code == STILL_ACTIVE as u32
    }
}

pub(super) fn terminate_process(pid: u32) -> io::Result<()> {
    if pid == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid PID"));
    }
    // SAFETY: opens target process with `PROCESS_TERMINATE` and closes handle afterwards.
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let ok = TerminateProcess(handle, 0);
        let err = io::Error::last_os_error();
        CloseHandle(handle);
        if ok == 0 {
            return Err(err);
        }
    }
    Ok(())
}

pub(super) fn set_executable_0755(_path: &Path) -> io::Result<()> {
    Ok(())
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
        .unwrap_or("agy-pulse.exe");
    let tmp_path = target_path.with_file_name(format!(
        ".{target_name}.update.{}.{nanos}.tmp",
        std::process::id()
    ));

    let copy_res = (|| -> io::Result<()> {
        let mut src = fs::File::open(source_file)?;
        let mut dst = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp_path)?;
        io::copy(&mut src, &mut dst)?;
        dst.sync_all()?;
        Ok(())
    })();

    if let Err(err) = copy_res {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }

    if fs::symlink_metadata(target_path).is_ok() {
        let primary_old = target_path.with_file_name(format!("{target_name}.old"));
        if fs::symlink_metadata(&primary_old).is_ok() {
            let _ = fs::remove_file(&primary_old);
        }
        let old_path = if fs::symlink_metadata(&primary_old).is_err() {
            primary_old
        } else {
            target_path.with_file_name(format!("{target_name}.{}.{nanos}.old", std::process::id()))
        };

        if let Err(err) = fs::rename(target_path, &old_path) {
            let _ = fs::remove_file(&tmp_path);
            return Err(err);
        }
        if let Err(err) = fs::rename(&tmp_path, target_path) {
            let _ = fs::rename(&old_path, target_path);
            let _ = fs::remove_file(&tmp_path);
            return Err(err);
        }
        let _ = fs::remove_file(&old_path);
        Ok(())
    } else {
        if let Err(err) = fs::rename(&tmp_path, target_path) {
            let _ = fs::remove_file(&tmp_path);
            return Err(err);
        }
        Ok(())
    }
}

pub(super) fn remove_binary_file_safely(path: &Path) -> io::Result<bool> {
    if fs::symlink_metadata(path).is_err() {
        return Ok(false);
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(_) => {
            let path_str = path.to_string_lossy().replace('"', "\"\"");
            let script = format!("ping -n 2 127.0.0.1 >nul & del /f /q \"{path_str}\"");
            let _ = spawn_detached(Path::new("cmd.exe"), &["/c", &script]);
            Ok(true)
        }
    }
}
