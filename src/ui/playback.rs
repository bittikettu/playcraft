//! Playback control: play order via the engine, and audio events.

use std::path::Path;

use crate::audio::{AudioCommand, AudioEvent};
use crate::model::PlaybackStatus;
use crate::playlist::engine::PlayState;

use super::app::{App, Banner, BannerLevel, NowPlaying, Row, View};

/// Builds the engine's view of the playlist from `App` fields (disjoint borrows).
macro_rules! play_state {
    ($app:expr) => {
        PlayState { root: &$app.root, mode: $app.play_mode, repeat: $app.repeat, version: $app.version }
    };
}

impl App {
    /// Starts `uid`. `user` marks an explicit choice, which re-anchors group playback.
    pub fn play(&mut self, uid: u64, user: bool) {
        let Some(path) = self.root.track_by_uid(uid).map(|t| t.path.clone()) else { return };
        if user {
            self.engine.anchor_at(&self.root, uid);
        }
        self.pending_queue = None;
        self.audio.send(AudioCommand::QueueNext(None));
        self.audio.send(AudioCommand::LoadAndPlay(path));
        self.pending_load = Some(uid);
    }

    pub(crate) fn play_pause(&mut self) {
        match self.status() {
            PlaybackStatus::Playing => self.audio.send(AudioCommand::Pause),
            PlaybackStatus::Paused => self.audio.send(AudioCommand::Resume),
            PlaybackStatus::Stopped => {
                let at_cursor = match (self.view, self.cursor_row()) {
                    (View::Playlist, Some(Row::Track(i))) => self.current_group().tracks.get(i).map(|t| t.uid),
                    _ => None,
                };
                let target = at_cursor
                    .or(self.last_played.filter(|&u| self.root.track_by_uid(u).is_some()))
                    .or_else(|| self.engine.next(&play_state!(self), None));
                match target {
                    Some(uid) => self.play(uid, true),
                    None => self.banner(BannerLevel::Info, "Nothing to play"),
                }
            }
        }
    }

    pub(crate) fn stop(&mut self) {
        self.audio.send(AudioCommand::Stop);
        self.last_played = self.playing.take().or(self.last_played.take());
        self.now = None;
        self.pending_load = None;
        self.pending_queue = None;
    }

    pub(crate) fn next_track(&mut self) {
        let from = self.playing.or(self.last_played);
        match self.engine.next(&play_state!(self), from) {
            Some(uid) => self.play(uid, false),
            None => self.banner(BannerLevel::Info, "End of playlist"),
        }
    }

    pub(crate) fn prev_track(&mut self) {
        let from = self.playing.or(self.last_played);
        if let Some(uid) = self.engine.previous(&play_state!(self), from) {
            self.play(uid, false);
        }
    }

    /// Recomputes the gapless successor after a playlist or mode change.
    pub(crate) fn requeue(&mut self) {
        let Some(playing) = self.playing else { return };
        let next = if self.root.locate(playing).is_some() {
            self.engine.successor(&play_state!(self), playing)
        } else {
            // The playing track was removed: keep what is queued while it still exists.
            self.pending_queue.filter(|&u| self.root.locate(u).is_some())
        };
        if next != self.pending_queue {
            self.pending_queue = next;
            let path = next.and_then(|u| self.root.track_by_uid(u)).map(|t| t.path.clone());
            self.audio.send(AudioCommand::QueueNext(path));
        }
    }

    // ---- audio events -----------------------------------------------------------------

    /// Applies pending audio events. Returns true if anything changed.
    pub fn process_audio_events(&mut self) -> bool {
        let events: Vec<AudioEvent> = self.audio.events().try_iter().collect();
        let changed = !events.is_empty();
        for ev in events {
            self.on_audio_event(ev);
        }
        changed
    }

    /// Maps an event's path back to a track, preferring the ones we asked for.
    fn resolve(&self, path: &Path) -> Option<u64> {
        [self.pending_queue, self.pending_load]
            .into_iter()
            .flatten()
            .find(|&u| self.root.track_by_uid(u).is_some_and(|t| t.path == path))
            .or_else(|| self.root.find_path(path).and_then(|r| self.root.track(&r)).map(|t| t.uid))
    }

    fn on_audio_event(&mut self, ev: AudioEvent) {
        match ev {
            AudioEvent::TrackStarted { path, duration, info } => {
                let track = self.resolve(&path);
                self.failures = 0;
                self.pending_load = None;
                self.playing = track;
                if let Some(uid) = track {
                    self.last_played = Some(uid);
                    self.engine.started(uid);
                }
                self.now = Some(NowPlaying { track, path, info, duration });
                self.pending_queue = None;
                self.requeue_after_start();
            }
            AudioEvent::TrackFailed { path, error, missing } => {
                let track = self.resolve(&path);
                if let Some(t) = track.and_then(|u| self.root.track_by_uid_mut(u)) {
                    t.missing = missing;
                    t.broken = !missing;
                }
                let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
                self.banner(BannerLevel::Warning, format!("{name}: {error}"));
                self.failures += 1;
                // Spec §8.3: give up after as many consecutive failures as there are tracks.
                if self.failures >= self.root.total_tracks().max(1) {
                    self.banner(BannerLevel::Error, "No playable tracks in the playlist");
                    self.pending_load = None;
                    self.pending_queue = None;
                    return;
                }
                let Some(uid) = track else { return };
                let skip_to = self.engine.next(&play_state!(self), Some(uid));
                if self.pending_queue == Some(uid) {
                    self.pending_queue = skip_to;
                    let p = skip_to.and_then(|u| self.root.track_by_uid(u)).map(|t| t.path.clone());
                    self.audio.send(AudioCommand::QueueNext(p));
                } else if self.pending_load == Some(uid) {
                    self.pending_load = None;
                    match skip_to {
                        Some(next) => self.play(next, false),
                        None => self.banner(BannerLevel::Warning, "No playable track to continue with"),
                    }
                }
            }
            AudioEvent::Finished => {
                self.last_played = self.playing.take().or(self.last_played.take());
                self.now = None;
                self.pending_queue = None;
            }
            AudioEvent::DeviceLost(msg) => {
                self.banner = Some(Banner { text: msg, level: BannerLevel::Error, until: None });
            }
            AudioEvent::DeviceRestored(name) => {
                self.banner(BannerLevel::Info, format!("Audio output: {name}"));
            }
            AudioEvent::Warning(msg) => self.banner(BannerLevel::Warning, msg),
        }
    }

    /// Queues the successor of the track that just started (always sends, even `None`).
    fn requeue_after_start(&mut self) {
        let next = self.playing.and_then(|p| self.engine.successor(&play_state!(self), p));
        self.pending_queue = next;
        let path = next.and_then(|u| self.root.track_by_uid(u)).map(|t| t.path.clone());
        self.audio.send(AudioCommand::QueueNext(path));
    }
}
