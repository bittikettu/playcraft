//! `session.json` (spec §7): playlist, position and volume remembered between runs.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use serde::{Deserialize, Serialize};

use crate::model::{PlayMode, RepeatMode, TrackRef};

const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub version: u32,
    /// The group tree, `.blst` style with absolute paths.
    pub playlist: serde_json::Value,
    #[serde(default)]
    pub playlist_path: Option<PathBuf>,
    #[serde(default)]
    pub cwd: Vec<usize>,
    #[serde(default)]
    pub cursor: usize,
    /// The track that was playing (or last played) and where in it.
    #[serde(default)]
    pub track: Option<TrackRef>,
    #[serde(default)]
    pub track_path: Option<PathBuf>,
    #[serde(default)]
    pub position_ms: u64,
    pub volume: f32,
    #[serde(default)]
    pub play_mode: PlayMode,
    #[serde(default)]
    pub repeat: RepeatMode,
    #[serde(default)]
    pub file_manager_dir: Option<PathBuf>,
}

impl Session {
    pub fn new(playlist: serde_json::Value, volume: f32) -> Self {
        Session {
            version: VERSION,
            playlist,
            playlist_path: None,
            cwd: Vec::new(),
            cursor: 0,
            track: None,
            track_path: None,
            position_ms: 0,
            volume,
            play_mode: PlayMode::default(),
            repeat: RepeatMode::default(),
            file_manager_dir: None,
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Reads a session; any problem (missing, corrupt, newer version) just means "no session".
pub fn load(path: &Path) -> Option<Session> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str::<Session>(&text) {
        Ok(s) if s.version <= VERSION => Some(s),
        Ok(_) => None,
        Err(e) => {
            log::warn!("ignoring unreadable session {}: {e}", path.display());
            None
        }
    }
}

/// Writes atomically (temp file + rename) so a crash or forced close never corrupts it.
pub fn save_json(path: &Path, json: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)
}

/// Coordinates a last-moment save when the console window is closed (Windows gives the
/// process a few seconds): the OS handler thread raises the flag and waits; the UI loop
/// saves and calls `complete`.
pub struct CloseSignal {
    requested: AtomicBool,
    done: Mutex<bool>,
    cv: Condvar,
}

impl CloseSignal {
    /// Creates the signal and registers it with the OS close handler.
    pub fn install() -> Arc<CloseSignal> {
        let signal = Arc::new(CloseSignal {
            requested: AtomicBool::new(false),
            done: Mutex::new(false),
            cv: Condvar::new(),
        });
        let handler = signal.clone();
        crate::platform::on_console_close(move || {
            handler.requested.store(true, Ordering::SeqCst);
            let done = handler.done.lock().unwrap_or_else(|e| e.into_inner());
            let _ =
                handler
                    .cv
                    .wait_timeout_while(done, std::time::Duration::from_secs(3), |done| !*done);
        });
        signal
    }

    pub fn requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }

    pub fn complete(&self) {
        *self.done.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.cv.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_signal_handshake() {
        let signal = Arc::new(CloseSignal {
            requested: AtomicBool::new(false),
            done: Mutex::new(false),
            cv: Condvar::new(),
        });
        let s = signal.clone();
        let waiter = std::thread::spawn(move || {
            s.requested.store(true, Ordering::SeqCst);
            let done = s.done.lock().unwrap();
            let (done, timeout) =
                s.cv.wait_timeout_while(done, std::time::Duration::from_secs(5), |d| !*d)
                    .unwrap();
            *done && !timeout.timed_out()
        });
        while !signal.requested() {
            std::thread::yield_now();
        }
        signal.complete();
        assert!(waiter.join().unwrap());
    }

    #[test]
    fn round_trips_and_tolerates_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state").join("session.json");
        let mut s = Session::new(serde_json::json!({"name": "Root"}), 0.65);
        s.track = Some(TrackRef::new(vec![1], 3));
        s.position_ms = 61_500;
        s.repeat = RepeatMode::RepeatAll;
        save_json(&path, &s.to_json()).unwrap();
        assert_eq!(load(&path), Some(s));

        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(load(&path), None);
        assert_eq!(load(&dir.path().join("missing.json")), None);
    }
}
