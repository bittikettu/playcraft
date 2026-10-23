//! Playback control: play order via the queue and the engine, and audio events.

use std::path::Path;

use crate::audio::{AudioCommand, AudioEvent};
use crate::model::{PlaybackStatus, RepeatMode};
use crate::playlist::engine::PlayState;

use super::app::{App, Banner, BannerLevel, NowPlaying, Row, View};

/// Builds the engine's view of the playlist from `App` fields (disjoint borrows).
macro_rules! play_state {
    ($app:expr) => {
        PlayState {
            root: &$app.root,
            mode: $app.play_mode,
            repeat: $app.repeat,
            version: $app.version,
        }
    };
}

impl App {
    /// Starts `uid`. `user` marks an explicit choice, which re-anchors group playback.
    pub fn play(&mut self, uid: u64, user: bool) {
        let Some(path) = self.root.track_by_uid(uid).map(|t| t.path.clone()) else {
            return;
        };
        if user {
            self.engine.anchor_at(&self.root, uid);
            // Picking a queued track plays it now instead of later, as part of normal order.
            self.take_from_queue(uid);
        }
        self.pending_next = None;
        self.audio.send(AudioCommand::QueueNext(None));
        self.audio.send(AudioCommand::LoadAndPlay(path));
        self.pending_load = Some(uid);
    }

    pub(crate) fn play_pause(&mut self) {
        match self.status() {
            PlaybackStatus::Playing => self.audio.send(AudioCommand::Pause),
            PlaybackStatus::Paused => self.audio.send(AudioCommand::Resume),
            PlaybackStatus::Stopped => {
                self.prune_queue();
                if let Some(&queued) = self.queue.first() {
                    return self.play(queued, false);
                }
                let at_cursor = match (self.view, self.cursor_row()) {
                    (View::Playlist, Some(Row::Track(i))) => {
                        self.current_group().tracks.get(i).map(|t| t.uid)
                    }
                    _ => None,
                };
                let target = at_cursor
                    .or(self
                        .last_played
                        .filter(|&u| self.root.track_by_uid(u).is_some()))
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
        self.pending_next = None;
    }

    pub(crate) fn next_track(&mut self) {
        let from = self.playing.or(self.last_played);
        match self.up_next(from, false) {
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

    /// What plays after `current`: the queue first, then the play order from where it was
    /// left before the queue. `auto` means `current` ends by itself (repeat-track applies).
    pub(crate) fn up_next(&mut self, current: Option<u64>, auto: bool) -> Option<u64> {
        self.prune_queue();
        if let Some(&queued) = self.queue.first() {
            return Some(queued);
        }
        if auto && self.repeat == RepeatMode::RepeatTrack {
            return current.and_then(|c| self.engine.successor(&play_state!(self), c));
        }
        let from = self
            .order_pos
            .filter(|&u| self.root.locate(u).is_some())
            .or(current);
        self.engine.next(&play_state!(self), from)
    }

    /// Drops queued tracks that left the playlist or cannot be played.
    pub(crate) fn prune_queue(&mut self) {
        let root = &self.root;
        self.queue
            .retain(|&u| root.track_by_uid(u).is_some_and(|t| t.is_playable()));
    }

    /// Removes `uid` from the queue. Returns whether it was queued.
    pub(crate) fn take_from_queue(&mut self, uid: u64) -> bool {
        let found = self.queue.iter().position(|&u| u == uid);
        if let Some(i) = found {
            self.queue.remove(i);
        }
        found.is_some()
    }

    /// Recomputes the gapless successor after a playlist, queue or mode change.
    pub(crate) fn requeue(&mut self) {
        let Some(playing) = self.playing else { return };
        self.prune_queue();
        let next = if self.root.locate(playing).is_some() || !self.queue.is_empty() {
            self.up_next(Some(playing), true)
        } else {
            // The playing track was removed: keep what is queued while it still exists.
            self.pending_next.filter(|&u| self.root.locate(u).is_some())
        };
        if next != self.pending_next {
            self.pending_next = next;
            let path = next
                .and_then(|u| self.root.track_by_uid(u))
                .map(|t| t.path.clone());
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
        [self.pending_next, self.pending_load]
            .into_iter()
            .flatten()
            .find(|&u| self.root.track_by_uid(u).is_some_and(|t| t.path == path))
            .or_else(|| {
                self.root
                    .find_path(path)
                    .and_then(|r| self.root.track(&r))
                    .map(|t| t.uid)
            })
    }

    fn on_audio_event(&mut self, ev: AudioEvent) {
        match ev {
            AudioEvent::TrackStarted {
                path,
                duration,
                info,
            } => {
                let track = self.resolve(&path);
                self.failures = 0;
                self.pending_load = None;
                self.playing = track;
                if let Some(uid) = track {
                    self.last_played = Some(uid);
                    self.engine.started(uid);
                    // A queued track is now played; anything else moves the place in order.
                    if !self.take_from_queue(uid) {
                        self.order_pos = Some(uid);
                    }
                }
                self.now = Some(NowPlaying {
                    track,
                    path,
                    info,
                    duration,
                });
                self.marquee_since = std::time::Instant::now();
                self.pending_next = None;
                self.requeue_after_start();
            }
            AudioEvent::TrackFailed {
                path,
                error,
                missing,
            } => {
                let track = self.resolve(&path);
                if let Some(t) = track.and_then(|u| self.root.track_by_uid_mut(u)) {
                    t.missing = missing;
                    t.broken = !missing;
                }
                let name = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                self.banner(BannerLevel::Warning, format!("{name}: {error}"));
                self.failures += 1;
                // Spec §8.3: give up after as many consecutive failures as there are tracks.
                if self.failures >= self.root.total_tracks().max(1) {
                    self.banner(BannerLevel::Error, "No playable tracks in the playlist");
                    self.pending_load = None;
                    self.pending_next = None;
                    return;
                }
                let Some(uid) = track else { return };
                // A failed queued track leaves the queue; one in normal order counts as reached.
                if !self.take_from_queue(uid) {
                    self.order_pos = Some(uid);
                }
                let skip_to = self.up_next(Some(uid), false);
                if self.pending_next == Some(uid) {
                    self.pending_next = skip_to;
                    let p = skip_to
                        .and_then(|u| self.root.track_by_uid(u))
                        .map(|t| t.path.clone());
                    self.audio.send(AudioCommand::QueueNext(p));
                } else if self.pending_load == Some(uid) {
                    self.pending_load = None;
                    match skip_to {
                        Some(next) => self.play(next, false),
                        None => {
                            self.banner(BannerLevel::Warning, "No playable track to continue with")
                        }
                    }
                }
            }
            AudioEvent::Finished => {
                self.last_played = self.playing.take().or(self.last_played.take());
                self.now = None;
                self.pending_next = None;
            }
            AudioEvent::DeviceLost(msg) => {
                self.banner = Some(Banner {
                    text: msg,
                    level: BannerLevel::Error,
                    until: None,
                });
            }
            AudioEvent::DeviceRestored(name) => {
                self.banner(BannerLevel::Info, format!("Audio output: {name}"));
            }
            AudioEvent::Warning(msg) => self.banner(BannerLevel::Warning, msg),
        }
    }

    /// Queues the successor of the track that just started (always sends, even `None`).
    fn requeue_after_start(&mut self) {
        let next = self.playing.and_then(|p| self.up_next(Some(p), true));
        self.pending_next = next;
        let path = next
            .and_then(|u| self.root.track_by_uid(u))
            .map(|t| t.path.clone());
        self.audio.send(AudioCommand::QueueNext(path));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::StreamInfo;
    use crate::model::{Group, Track};
    use crate::ui::keymap::Action;
    use crate::ui::tests::test_app;

    fn album(n: usize) -> Group {
        let mut root = Group::new(0, "Root");
        root.tracks = (0..n).map(|i| Track::new(format!("t{i}.flac"))).collect();
        root
    }

    fn uids(app: &App) -> Vec<u64> {
        app.root.tracks.iter().map(|t| t.uid).collect()
    }

    /// What the audio thread reports when `uid` becomes audible.
    fn started(app: &mut App, uid: u64) {
        let path = app.root.track_by_uid(uid).unwrap().path.clone();
        let info = StreamInfo {
            codec: "flac".into(),
            sample_rate: 44_100,
            channels: 2,
        };
        app.on_audio_event(AudioEvent::TrackStarted {
            path,
            duration: None,
            info,
        });
    }

    fn enqueue_row(app: &mut App, row: usize) {
        app.cursor = row;
        app.on_action(Action::Enqueue);
    }

    #[test]
    fn queued_tracks_play_first_then_normal_order_resumes() {
        let mut app = test_app(album(6));
        let t = uids(&app);
        app.play(t[0], true);
        started(&mut app, t[0]);
        assert_eq!(app.pending_next, Some(t[1]));

        enqueue_row(&mut app, 3);
        enqueue_row(&mut app, 5);
        assert_eq!(app.queue, [t[3], t[5]]);
        assert_eq!(app.pending_next, Some(t[3]), "the queue goes first");
        started(&mut app, t[3]);
        assert_eq!(app.queue, [t[5]]);
        assert_eq!(app.pending_next, Some(t[5]));
        started(&mut app, t[5]);
        assert!(app.queue.is_empty());
        // Not "end of playlist" after t5: the order continues where it was before the queue.
        assert_eq!(app.pending_next, Some(t[1]));
        started(&mut app, t[1]);
        assert_eq!(app.pending_next, Some(t[2]));
    }

    #[test]
    fn enqueue_toggles_and_next_takes_the_head_of_the_queue() {
        let mut app = test_app(album(5));
        let t = uids(&app);
        app.play(t[0], true);
        started(&mut app, t[0]);
        enqueue_row(&mut app, 4);
        enqueue_row(&mut app, 4);
        assert!(app.queue.is_empty(), "queueing twice takes it out again");
        assert_eq!(app.pending_next, Some(t[1]));

        enqueue_row(&mut app, 4);
        app.on_action(Action::NextTrack);
        assert_eq!(app.pending_load, Some(t[4]));
        started(&mut app, t[4]);
        assert!(app.queue.is_empty());
        assert_eq!(app.pending_next, Some(t[1]));
    }

    #[test]
    fn picking_a_queued_track_by_hand_moves_the_play_order_there() {
        let mut app = test_app(album(5));
        let t = uids(&app);
        app.play(t[0], true);
        started(&mut app, t[0]);
        enqueue_row(&mut app, 3);
        app.play(t[3], true);
        assert!(app.queue.is_empty());
        started(&mut app, t[3]);
        assert_eq!(app.pending_next, Some(t[4]));
    }

    #[test]
    fn a_group_queues_all_its_playable_tracks_once() {
        let mut root = album(1);
        let mut a = Group::new(1, "A");
        a.tracks = vec![Track::new("a0.flac"), Track::new("a1.flac")];
        a.tracks[1].missing = true;
        let mut a1 = Group::new(2, "A1");
        a1.tracks = vec![Track::new("b0.flac")];
        a.subgroups.push(a1);
        root.subgroups.push(a);
        let mut app = test_app(root);
        let (r0, a0, b0) = (
            app.root.tracks[0].uid,
            app.root.subgroups[0].tracks[0].uid,
            app.root.subgroups[0].subgroups[0].tracks[0].uid,
        );
        // Rows: the group, then the root's track.
        enqueue_row(&mut app, 1);
        enqueue_row(&mut app, 0);
        assert_eq!(app.queue, [r0, a0, b0], "the missing a1 is left out");
        enqueue_row(&mut app, 0);
        assert_eq!(
            app.queue.len(),
            3,
            "already queued tracks are not added twice"
        );
        assert_eq!(app.queue_position(b0), Some(3));
    }

    #[test]
    fn failed_or_removed_queued_tracks_drop_out() {
        let mut app = test_app(album(6));
        let t = uids(&app);
        app.play(t[0], true);
        started(&mut app, t[0]);
        for row in [2, 3, 4] {
            enqueue_row(&mut app, row);
        }
        let path = app.root.tracks[2].path.clone();
        app.on_audio_event(AudioEvent::TrackFailed {
            path,
            error: "not found".into(),
            missing: true,
        });
        assert_eq!(app.queue, [t[3], t[4]]);
        assert_eq!(app.pending_next, Some(t[3]));

        // Removing t3 from the playlist removes it from the queue too.
        app.cursor = 3;
        app.on_action(Action::Delete);
        assert_eq!(app.queue, [t[4]]);
        assert_eq!(app.pending_next, Some(t[4]));
    }

    #[test]
    fn play_while_stopped_starts_the_queue() {
        let mut app = test_app(album(4));
        let t = uids(&app);
        enqueue_row(&mut app, 2);
        app.cursor = 0;
        app.on_action(Action::PlayPause);
        assert_eq!(app.pending_load, Some(t[2]));
    }
}
