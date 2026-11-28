use std::ffi::OsStr;
use std::fs::{self, Metadata, OpenOptions};
use std::io::{self, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_SUCCESS, HANDLE, INVALID_HANDLE_VALUE, LocalFree, STILL_ACTIVE,
};
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW,
    SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACL, CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, GetTokenInformation, NO_INHERITANCE,
    OBJECT_INHERIT_ACE, PROTECTED_DACL_SECURITY_INFORMATION, PSID, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ALL_ACCESS, MOVEFILE_DELAY_UNTIL_REBOOT, MoveFileExW,
};
use windows_sys::Win32::System::Console::{
    CONSOLE_SCREEN_BUFFER_INFO, CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT,
    CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT,
    ENABLE_PROCESSED_INPUT, GetConsoleMode, GetConsoleOutputCP, GetConsoleScreenBufferInfo,
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleCtrlHandler,
    SetConsoleMode,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, GetCurrentProcess, GetExitCodeProcess, OpenProcess,
    OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, TerminateProcess,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

static DELETE_STAGE_COUNTER: AtomicU64 = AtomicU64::new(1);
static SHUTDOWN_SIGNAL_FLAG: AtomicBool = AtomicBool::new(false);
static CONSOLE_HANDLER_INSTALLED: AtomicBool = AtomicBool::new(false);

#[inline]
fn is_valid_handle(handle: HANDLE) -> bool {
    !handle.is_null() && handle != INVALID_HANDLE_VALUE
}

struct HandleGuard(HANDLE);

impl Drop for HandleGuard {
    fn drop(&mut self) {
        if is_valid_handle(self.0) {
            // SAFETY: `self.0` is an owned Win32 handle that must be closed on drop.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

struct LocalAclGuard(*mut ACL);

impl Drop for LocalAclGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `self.0` was allocated by `SetEntriesInAclW` and must be freed via `LocalFree`.
            unsafe {
                let _ = LocalFree(self.0.cast::<core::ffi::c_void>());
            }
        }
    }
}

fn os_str_to_wide_null(s: &OsStr) -> io::Result<Vec<u16>> {
    let mut wide: Vec<u16> = Vec::with_capacity(s.len() + 1);
    for unit in s.encode_wide() {
        if unit == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "interior NUL unit in Windows path or argument",
            ));
        }
        wide.push(unit);
    }
    wide.push(0);
    Ok(wide)
}

pub(super) fn detect_unicode_from_os() -> bool {
    // SAFETY: Win32 GetConsoleOutputCP takes no pointers and returns active codepage ID.
    unsafe { GetConsoleOutputCP() == 65001 }
}

pub(super) fn detect_live_tty_size() -> Option<(u16, u16)> {
    for std_handle in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: Querying standard console handle and populating stack-allocated buffer info after null/invalid check.
        unsafe {
            let handle = GetStdHandle(std_handle);
            if !is_valid_handle(handle) {
                continue;
            }
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
    // SAFETY: `GetStdHandle` and `GetConsoleMode` succeed only when attached to valid console handles.
    unsafe {
        let in_handle = GetStdHandle(STD_INPUT_HANDLE);
        let out_handle = GetStdHandle(STD_OUTPUT_HANDLE);
        if !is_valid_handle(in_handle) || !is_valid_handle(out_handle) {
            return false;
        }
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
        // SAFETY: Queries and updates console input mode on `STD_INPUT_HANDLE` after validating handle.
        unsafe {
            let in_handle = GetStdHandle(STD_INPUT_HANDLE);
            if !is_valid_handle(in_handle) {
                return Err(io::Error::new(
                    io::ErrorKind::NotConnected,
                    "standard input console handle is unavailable",
                ));
            }
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
        // SAFETY: Restores original console input mode on `STD_INPUT_HANDLE` when handle is valid.
        unsafe {
            let in_handle = GetStdHandle(STD_INPUT_HANDLE);
            if is_valid_handle(in_handle) {
                let _ = SetConsoleMode(in_handle, self.orig_in_mode);
            }
        }
    }
}

fn restrict_path_to_current_user(path: &Path, is_dir: bool) -> std::io::Result<()> {
    let mut raw_token: HANDLE = std::ptr::null_mut();
    // SAFETY: `GetCurrentProcess()` returns a pseudo-handle that must not be closed;
    // `OpenProcessToken` stores an owned token handle into `raw_token` when successful.
    let ok = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw_token) };
    if ok == 0 || !is_valid_handle(raw_token) {
        return Err(io::Error::last_os_error());
    }
    let token = HandleGuard(raw_token);

    let mut needed: u32 = 0;
    // SAFETY: Querying required buffer length for `TokenUser` with null buffer.
    unsafe {
        let _ = GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &mut needed);
    }
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }

    let words = (needed as usize)
        .div_ceil(std::mem::size_of::<u64>())
        .max(1);
    let mut buf = vec![0u64; words];
    let buf_len = u32::try_from(buf.len().saturating_mul(std::mem::size_of::<u64>()))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "token buffer size overflow"))?;
    // SAFETY: `buf` is 8-byte aligned for `TOKEN_USER` and has capacity `buf_len >= needed`.
    let ok = unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buf.as_mut_ptr().cast::<core::ffi::c_void>(),
            buf_len,
            &mut needed,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: `GetTokenInformation(TokenUser)` succeeded and initialized `TOKEN_USER` at the start of `buf`.
    let token_user = unsafe { &*(buf.as_ptr().cast::<TOKEN_USER>()) };
    let user_sid: PSID = token_user.User.Sid;
    if user_sid.is_null() {
        return Err(io::Error::other("TokenUser returned null SID"));
    }

    let ea = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_ALL_ACCESS,
        grfAccessMode: SET_ACCESS,
        grfInheritance: if is_dir {
            CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE
        } else {
            NO_INHERITANCE
        },
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: user_sid.cast::<u16>(),
        },
    };

    let mut raw_acl: *mut ACL = std::ptr::null_mut();
    // SAFETY: `ea` references `user_sid` inside `buf` which outlives `SetEntriesInAclW`.
    let status = unsafe { SetEntriesInAclW(1, &ea, std::ptr::null(), &mut raw_acl) };
    if status != ERROR_SUCCESS || raw_acl.is_null() {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let dacl = LocalAclGuard(raw_acl);

    let wide_path = os_str_to_wide_null(path.as_os_str())?;
    // SAFETY: `wide_path` is NUL-terminated UTF-16 and `dacl.0` is a valid ACL owned by `dacl`;
    // `PROTECTED_DACL_SECURITY_INFORMATION` blocks inherited ACEs so only the current user SID has access.
    let status = unsafe {
        SetNamedSecurityInfoW(
            wide_path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            dacl.0,
            std::ptr::null(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    Ok(())
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
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
    {
        let _ = file.write_all(message.as_bytes());
        let _ = set_file_0600(path);
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
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(message.as_bytes());
        let _ = set_file_0600(path);
    }
}

pub(super) fn file_unique_id(_meta: &Metadata) -> u64 {
    0
}

pub(super) fn ensure_dir_0700(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)?;
    restrict_path_to_current_user(path, true)
}

pub(super) fn set_file_0600(path: &Path) -> std::io::Result<()> {
    restrict_path_to_current_user(path, false)
}

pub(super) fn create_exclusive_file_0600(path: &Path, bytes: &[u8]) -> std::io::Result<bool> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        ensure_dir_0700(parent)?;
    }
    match OpenOptions::new().create_new(true).write(true).open(path) {
        Ok(mut file) => {
            file.write_all(bytes)?;
            set_file_0600(path)?;
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
    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("cache");
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
        set_file_0600(&tmp_path)?;
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

unsafe extern "system" fn console_ctrl_handler(ctrl_type: u32) -> windows_sys::core::BOOL {
    match ctrl_type {
        CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT
        | CTRL_SHUTDOWN_EVENT => {
            SHUTDOWN_SIGNAL_FLAG.store(true, Ordering::Release);
            1
        }
        _ => 0,
    }
}

pub(super) fn install_shutdown_signal_handler() {
    SHUTDOWN_SIGNAL_FLAG.store(false, Ordering::Release);
    if !CONSOLE_HANDLER_INSTALLED.swap(true, Ordering::AcqRel) {
        // SAFETY: `console_ctrl_handler` matches `PHANDLER_ROUTINE` and only performs
        // an atomic store; registered at most once for the process lifetime.
        let ok = unsafe { SetConsoleCtrlHandler(Some(console_ctrl_handler), 1) };
        if ok == 0 {
            CONSOLE_HANDLER_INSTALLED.store(false, Ordering::Release);
        }
    }
}

pub(super) fn shutdown_signal_requested() -> bool {
    SHUTDOWN_SIGNAL_FLAG.load(Ordering::Acquire)
}

pub(super) fn spawn_detached(program: &Path, args: &[&str]) -> io::Result<u32> {
    // Rust's `std::process::Command` on Windows opens `NUL` for `Stdio::null()` and
    // restricts handle inheritance via `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`, so we do
    // not mutate the current process's global `STD_*_HANDLE` inheritance flags.
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
        if !is_valid_handle(handle) {
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
        if !is_valid_handle(handle) {
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

pub(super) fn replace_executable_atomically(
    source_file: &Path,
    target_path: &Path,
) -> io::Result<()> {
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

fn schedule_delete_on_reboot(path: &Path) -> io::Result<()> {
    let wide_path = os_str_to_wide_null(path.as_os_str())?;
    // SAFETY: `wide_path` is a NUL-terminated UTF-16 path and `lpNewFileName` is NULL,
    // which instructs Windows to delete the file on next reboot via `MOVEFILE_DELAY_UNTIL_REBOOT`.
    let ok = unsafe {
        MoveFileExW(
            wide_path.as_ptr(),
            std::ptr::null(),
            MOVEFILE_DELAY_UNTIL_REBOOT,
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(super) fn open_url_default_browser(url: &str) -> bool {
    let Ok(wide_url) = os_str_to_wide_null(OsStr::new(url)) else {
        return false;
    };
    let wide_open: [u16; 5] = [b'o' as u16, b'p' as u16, b'e' as u16, b'n' as u16, 0];
    // SAFETY: `wide_open` and `wide_url` are NUL-terminated UTF-16 strings; `ShellExecuteW`
    // delegates directly to the Windows Shell without invoking `cmd.exe`.
    let hinst = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            wide_open.as_ptr(),
            wide_url.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    (hinst as isize) > 32
}

pub(super) fn remove_binary_file_safely(path: &Path) -> io::Result<bool> {
    if fs::symlink_metadata(path).is_err() {
        return Ok(false);
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(remove_err) => {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let seq = DELETE_STAGE_COUNTER.fetch_add(1, Ordering::Relaxed);
            let file_name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("agy-pulse.exe");
            let staged = path.with_file_name(format!(
                ".{file_name}.delete.{}.{nanos}.{seq}.old",
                std::process::id()
            ));
            if fs::rename(path, &staged).is_ok() {
                if fs::remove_file(&staged).is_err() {
                    let _ = schedule_delete_on_reboot(&staged);
                }
                return Ok(true);
            }
            if schedule_delete_on_reboot(path).is_ok() {
                return Ok(true);
            }
            Err(remove_err)
        }
    }
}
