//! Locations of configuration, state and music folders.
//!
//! On Windows the config lives in `%APPDATA%\mp3blaster-rs` and state in
//! `%LOCALAPPDATA%\mp3blaster-rs`; `XDG_CONFIG_HOME` / `XDG_STATE_HOME` override both when set.

use std::env;
use std::path::{Path, PathBuf};

pub const APP_DIR: &str = "mp3blaster-rs";

fn xdg_dir(var: &str) -> Option<PathBuf> {
    env::var_os(var).map(PathBuf::from).filter(|p| p.is_absolute())
}

pub fn config_dir() -> Option<PathBuf> {
    xdg_dir("XDG_CONFIG_HOME").or_else(dirs::config_dir).map(|d| d.join(APP_DIR))
}

pub fn config_file() -> Option<PathBuf> {
    config_dir().map(|d| d.join("config.toml"))
}

/// `dirs::state_dir()` is `None` on Windows and macOS, so fall back to the local data dir.
pub fn state_dir() -> Option<PathBuf> {
    xdg_dir("XDG_STATE_HOME")
        .or_else(dirs::state_dir)
        .or_else(dirs::data_local_dir)
        .map(|d| d.join(APP_DIR))
}

pub fn session_file() -> Option<PathBuf> {
    state_dir().map(|d| d.join("session.json"))
}

pub fn log_file() -> Option<PathBuf> {
    state_dir().map(|d| d.join("mp3blaster.log"))
}

/// Expands a leading `~` to the user's home directory.
pub fn expand_tilde(path: &str) -> PathBuf {
    let rest = path.strip_prefix('~').filter(|r| r.is_empty() || r.starts_with(['/', '\\']));
    match (rest, dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest.trim_start_matches(['/', '\\'])),
        _ => PathBuf::from(path),
    }
}

/// The user's Music folder (follows OneDrive redirection on Windows), else home, else cwd.
pub fn default_music_dir() -> PathBuf {
    dirs::audio_dir()
        .filter(|d| d.is_dir())
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Canonicalises a path without producing Windows `\\?\` verbatim prefixes.
pub fn normalize(path: &Path) -> PathBuf {
    dunce::canonicalize(path).unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            env::current_dir().map(|cwd| cwd.join(path)).unwrap_or_else(|_| path.to_path_buf())
        }
    })
}

/// A path for display, with any `\\?\` prefix removed.
pub fn display(path: &Path) -> String {
    dunce::simplified(path).display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_tilde_only_at_start() {
        let home = dirs::home_dir().expect("home dir");
        assert_eq!(expand_tilde("~"), home);
        assert_eq!(expand_tilde("~/Music"), home.join("Music"));
        assert_eq!(expand_tilde("~\\Music"), home.join("Music"));
        assert_eq!(expand_tilde("~user/x"), PathBuf::from("~user/x"));
        assert_eq!(expand_tilde("a/~"), PathBuf::from("a/~"));
    }

    #[test]
    fn app_dirs_end_with_app_name() {
        assert!(config_file().unwrap().ends_with(Path::new(APP_DIR).join("config.toml")));
        assert!(session_file().unwrap().ends_with(Path::new(APP_DIR).join("session.json")));
    }

    #[cfg(windows)]
    #[test]
    fn normalize_has_no_verbatim_prefix() {
        let p = normalize(Path::new("."));
        assert!(!p.to_string_lossy().starts_with(r"\\?\"));
        assert!(p.is_absolute());
    }
}
