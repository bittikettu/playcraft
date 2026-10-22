use std::fs::Metadata;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::OnceLock;

use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_SYSTEM, GetDriveTypeW, GetLogicalDrives,
};
use windows_sys::Win32::System::Console::{
    CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT, SetConsoleCtrlHandler,
};
use windows_sys::core::BOOL;

use super::Root;

// GetDriveTypeW results (WinBase.h).
const DRIVE_NO_ROOT_DIR: u32 = 1;
const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_REMOTE: u32 = 4;
const DRIVE_CDROM: u32 = 5;
const DRIVE_RAMDISK: u32 = 6;

pub fn drives() -> Vec<Root> {
    // SAFETY: GetLogicalDrives takes no arguments and only returns a bitmask.
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .filter_map(|i| {
            let root = format!("{}:\\", char::from(b'A' + i));
            let wide: Vec<u16> = std::ffi::OsStr::new(&root)
                .encode_wide()
                .chain([0])
                .collect();
            // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives the call.
            let kind = match unsafe { GetDriveTypeW(wide.as_ptr()) } {
                DRIVE_NO_ROOT_DIR => return None,
                DRIVE_REMOVABLE => "Removable",
                DRIVE_REMOTE => "Network",
                DRIVE_CDROM => "CD/DVD",
                DRIVE_RAMDISK => "RAM disk",
                _ => "Local disk",
            };
            Some(Root {
                path: PathBuf::from(root),
                kind,
            })
        })
        .collect()
}

pub fn has_hidden_attribute(meta: &Metadata) -> bool {
    meta.file_attributes() & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0
}

type CloseCallback = Box<dyn Fn() + Send + Sync>;
static CLOSE_CALLBACK: OnceLock<CloseCallback> = OnceLock::new();

unsafe extern "system" fn console_handler(ctrl_type: u32) -> BOOL {
    match ctrl_type {
        CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => {
            if let Some(cb) = CLOSE_CALLBACK.get() {
                cb();
            }
            // Returning TRUE lets Windows terminate us once the callback has finished.
            1
        }
        _ => 0,
    }
}

pub fn on_console_close(callback: CloseCallback) {
    if CLOSE_CALLBACK.set(callback).is_ok() {
        // SAFETY: `console_handler` is a valid `extern "system"` function for the process lifetime.
        unsafe {
            SetConsoleCtrlHandler(Some(console_handler), 1);
        }
    }
}
