use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use windows_sys::Win32::System::Console::{
    CONSOLE_SCREEN_BUFFER_INFO, GetConsoleOutputCP, GetConsoleScreenBufferInfo, GetStdHandle,
    STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
};

pub(super) fn detect_unicode_from_os() -> bool {
    // SAFETY: Win32 GetConsoleOutputCP takes no pointers and returns active codepage ID.
    unsafe { GetConsoleOutputCP() == 65001 }
}

pub(super) fn detect_live_tty_columns() -> Option<u16> {
    for std_handle in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: Querying standard console handle and populating stack-allocated buffer info.
        unsafe {
            let handle = GetStdHandle(std_handle);
            let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
            if GetConsoleScreenBufferInfo(handle, &mut info) != 0 {
                let width = info.srWindow.Right - info.srWindow.Left + 1;
                if width > 0 {
                    return Some(width as u16);
                }
            }
        }
    }
    None
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
