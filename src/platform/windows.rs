use std::fs::Metadata;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_SYSTEM, GetDriveTypeW, GetLogicalDrives,
};
use windows_sys::Win32::System::Console::{
    CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT, GetConsoleMode, GetStdHandle,
    STD_OUTPUT_HANDLE, SetConsoleCtrlHandler, WriteConsoleW,
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

/// The console behind stdout, written with one `WriteConsoleW` call per frame.
/// (Rust's stdout splits console output into 4 KiB pieces; each piece is a round trip
/// through the console host.)
pub struct Console {
    handle: HANDLE,
    utf16: Vec<u16>,
}

impl Console {
    /// `None` when stdout is not a console (redirected to a file or pipe).
    pub fn stdout() -> Option<Console> {
        // SAFETY: GetStdHandle has no preconditions.
        let handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut mode = 0;
        // SAFETY: `handle` is a valid standard handle and `mode` a valid out pointer.
        if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
            return None;
        }
        Some(Console {
            handle,
            utf16: Vec::new(),
        })
    }

    pub fn write_all(&mut self, utf8: &[u8]) -> std::io::Result<()> {
        self.utf16.clear();
        self.utf16
            .extend(String::from_utf8_lossy(utf8).encode_utf16());
        let mut rest = &self.utf16[..];
        while !rest.is_empty() {
            let len = rest.len().min(u32::MAX as usize) as u32;
            let mut written = 0u32;
            // SAFETY: `rest` points to `len` valid u16s; `written` is a valid out pointer.
            let ok = unsafe {
                WriteConsoleW(
                    self.handle,
                    rest.as_ptr().cast(),
                    len,
                    &mut written,
                    std::ptr::null(),
                )
            };
            if ok == 0 {
                return Err(std::io::Error::last_os_error());
            }
            if written == 0 {
                return Err(std::io::ErrorKind::WriteZero.into());
            }
            rest = &rest[(written as usize).min(rest.len())..];
        }
        Ok(())
    }
}
