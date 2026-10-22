//! Saving and restoring the session from the UI.

use std::path::PathBuf;
use std::time::Duration;

use crate::audio::AudioCommand;
use crate::library::fs_browser::Location;
use crate::playlist::io::blst;
use crate::session::Session;

use super::app::App;

impl App {
    /// The current state as a session.
    pub fn snapshot(&self) -> Session {
        let mut s = Session::new(
            blst::to_json_value(&self.root),
            self.audio.shared().volume(),
        );
        s.playlist_path = self.playlist_path.clone();
        s.cwd = self.cwd.clone();
        s.cursor = self.cursor;
        let track = self.playing.or(self.last_played);
        s.track = track.and_then(|u| self.root.locate(u));
        s.track_path = track
            .and_then(|u| self.root.track_by_uid(u))
            .map(|t| t.path.clone());
        if self.playing.is_some() {
            s.position_ms = self.audio.shared().position().as_millis() as u64;
        }
        s.play_mode = self.play_mode;
        s.repeat = self.repeat;
        if let Location::Dir(d) = &self.fm.location {
            s.file_manager_dir = Some(d.clone());
        }
        s
    }

    /// Restores a saved session. With `keep_playlist`, the current playlist (from the command
    /// line) stays and only settings are restored. With `resume`, the last track is loaded
    /// paused at its saved position.
    pub fn restore(&mut self, session: Session, keep_playlist: bool, resume: bool, volume: bool) {
        self.play_mode = session.play_mode;
        self.repeat = session.repeat;
        if volume {
            let v = session.volume.clamp(0.0, 1.0);
            self.audio.shared().set_volume(v);
            self.audio.send(AudioCommand::SetVolume(v));
        }
        if let Some(dir) = session.file_manager_dir.filter(|d| d.is_dir()) {
            self.fm.go_to(Location::Dir(dir), None);
        }
        if keep_playlist {
            return;
        }
        let Some(root) = blst::from_json_value(session.playlist) else {
            return;
        };
        self.root = root;
        self.root.name = "Root".into();
        self.version += 1;
        self.playlist_path = session.playlist_path;
        if self.root.group_at(&session.cwd).is_some() {
            self.cwd = session.cwd;
        }
        self.cursor = session.cursor;
        self.request_all_metadata();

        let track = session
            .track
            .and_then(|r| self.root.track(&r))
            .filter(|t| Some(&t.path) == session.track_path.as_ref())
            .map(|t| (t.uid, t.path.clone()));
        if let Some((uid, path)) = track {
            self.last_played = Some(uid);
            self.engine.anchor_at(&self.root, uid);
            if resume && path.exists() {
                self.resume_paused(uid, path, Duration::from_millis(session.position_ms));
            }
        }
    }

    fn resume_paused(&mut self, uid: u64, path: PathBuf, position: Duration) {
        self.audio.send(AudioCommand::LoadAndPlay(path));
        if !position.is_zero() {
            self.audio.send(AudioCommand::SeekAbsolute(position));
        }
        self.audio.send(AudioCommand::Pause);
        self.pending_load = Some(uid);
    }
}
