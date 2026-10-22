//! OS-specific helpers. Windows gets real implementations; other systems simple fallbacks.

use std::fs::Metadata;
use std::path::{Path, PathBuf};

#[cfg(windows)]
mod windows;

/// A filesystem root shown above the top-level directories (drives on Windows).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    pub path: PathBuf,
    /// Short description, e.g. "Local disk" or "Network".
    pub kind: &'static str,
}

/// Drive letters on Windows, `/` elsewhere.
pub fn roots() -> Vec<Root> {
    #[cfg(windows)]
    {
        windows::drives()
    }
    #[cfg(not(windows))]
    {
        vec![Root { path: PathBuf::from("/"), kind: "Root" }]
    }
}

/// Whether a directory entry should be hidden unless "show hidden" is on: the HIDDEN or
/// SYSTEM attribute on Windows, a leading dot elsewhere.
pub fn is_hidden(path: &Path, meta: &Metadata) -> bool {
    #[cfg(windows)]
    {
        let _ = path;
        windows::has_hidden_attribute(meta)
    }
    #[cfg(not(windows))]
    {
        let _ = meta;
        path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.'))
    }
}

/// Registers `on_close` to run when the console window is closed, the user logs off or the
/// system shuts down. Windows gives the process a few seconds; `on_close` should save quickly.
pub fn on_console_close(on_close: impl Fn() + Send + Sync + 'static) {
    #[cfg(windows)]
    windows::on_console_close(Box::new(on_close));
    #[cfg(not(windows))]
    let _ = on_close;
}
