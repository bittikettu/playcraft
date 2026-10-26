//! The jukebox view (F4): the songs coming up as title strips on a jukebox, when each one
//! starts, what plays next, and a record change on the turntable whenever the song changes.
//! The amazing-horse view (F5) shows the same songs as a ride, and shares the list, the
//! selection and the keys.
//!
//! The list is what will really play: the play queue first, then the play order from where
//! it was left (see `App::up_next`).

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::jukebox::{Disc, Jukebox, path_key};
use crate::model::{PlayMode, PlaybackStatus, RepeatMode, Track};
use crate::playlist::engine::{OrderEnd, PlayState};

use super::app::{App, BannerLevel, View};
use super::keymap::Action;

/// Songs worked out ahead; the board scrolls through them.
const LINEUP_LIMIT: usize = 60;
/// Tags arrive in the background and tracks can turn out missing, so the list is also
/// worked out again this often.
const REFRESH: Duration = Duration::from_millis(500);

/// A song coming up.
#[derive(Debug, Clone)]
pub struct Song {
    pub track: Track,
    /// Its selection code, like on a real jukebox: "A1" is the playlist's first song.
    pub code: String,
    /// Its place in the play queue (1-based), when the user queued it.
    pub queued: Option<usize>,
    pub disc: Disc,
}

/// What happens after the last song of the lineup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineupEnd {
    /// More songs follow than are listed.
    More,
    /// The music stops.
    #[default]
    Stops,
    /// A new shuffle starts.
    Reshuffles,
    /// The last song plays again and again (repeat track).
    Repeats,
}

/// The songs coming up, in the order they play.
#[derive(Debug, Clone, Default)]
pub struct Lineup {
    pub songs: Vec<Song>,
    pub end: LineupEnd,
    /// The selection code of the playing song.
    pub playing_code: Option<String>,
}

/// What the lineup depends on, besides tags and missing files.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LineupKey {
    version: u64,
    queue: Vec<u64>,
    playing: Option<u64>,
    last_played: Option<u64>,
    order_pos: Option<u64>,
    mode: PlayMode,
    repeat: RepeatMode,
    stopped: bool,
}

pub struct JukeboxState {
    /// The machine's moving parts.
    pub scene: Jukebox,
    /// The selected title strip, as an index into the lineup.
    pub cursor: usize,
    /// The first song on the board.
    pub scroll: usize,
    /// Cards on the board in the last frame (for paging).
    pub page: usize,
    pub(crate) lineup: Lineup,
    key: Option<LineupKey>,
    refreshed: Instant,
    /// View to return to when the jukebox (or the horse) closes.
    pub(crate) view_before: View,
}

impl Default for JukeboxState {
    fn default() -> Self {
        JukeboxState {
            scene: Jukebox::default(),
            cursor: 0,
            scroll: 0,
            page: 1,
            lineup: Lineup::default(),
            key: None,
            refreshed: Instant::now(),
            view_before: View::Playlist,
        }
    }
}

/// A jukebox selection code: a letter for every ten songs (no I or O, as on the real
/// machines), then a digit, 0 for the tenth. "A1".."A0", "B1".., after "V0" "AA1".
pub fn selection_code(index: usize) -> String {
    const LETTERS: &[u8; 20] = b"ABCDEFGHJKLMNPQRSTUV";
    let mut n = index / 10;
    let mut letters = Vec::new();
    loop {
        letters.push(LETTERS[n % 20]);
        if n < 20 {
            break;
        }
        n = n / 20 - 1;
    }
    letters.reverse();
    let digit = (b'0' + ((index % 10 + 1) % 10) as u8) as char;
    format!("{}{digit}", String::from_utf8_lossy(&letters))
}

impl App {
    /// Opens `view` (the jukebox or the horse), or closes it back to the view it was
    /// opened from. The two switch straight to each other.
    pub(crate) fn toggle_lineup_view(&mut self, view: View) {
        if self.view == view {
            self.view = self.jukebox.view_before;
            return;
        }
        if !self.view.shows_lineup() {
            self.jukebox.view_before = self.view;
            self.jukebox.cursor = 0;
            self.jukebox.scroll = 0;
            self.jukebox.key = None;
        }
        self.view = view;
        // The playing song is already on the turntable (and the horse is already riding to
        // it): no record change or plane trick to begin with.
        let path = self.now.as_ref().map(|n| n.path.as_path());
        self.jukebox.scene.reset(path.map(Disc::for_path));
        self.horse.reset(path.map(path_key));
    }

    /// The bass loudness (0-1), for lights and gallops.
    fn bass(&self) -> f32 {
        let bands = self.spectrum.bands();
        let low = &bands[..(bands.len() / 6).max(1).min(bands.len())];
        low.iter().sum::<f32>() / low.len().max(1) as f32
    }

    /// Moves the machine on by `dt`. Call once per frame while the jukebox is open.
    pub(crate) fn update_jukebox(&mut self, dt: Duration) {
        let disc = self.now.as_ref().map(|n| Disc::for_path(&n.path));
        let playing = self.status() == PlaybackStatus::Playing;
        let bass = self.bass();
        self.jukebox
            .scene
            .update(dt.as_secs_f32(), disc, playing, bass);
    }

    /// Moves the horse on by `dt`. Call once per frame while it is on screen.
    pub(crate) fn update_horse(&mut self, dt: Duration) {
        let song = self.now.as_ref().map(|n| path_key(&n.path));
        let playing = self.status() == PlaybackStatus::Playing;
        let bass = self.bass();
        self.horse.update(dt.as_secs_f32(), song, playing, bass);
    }

    /// The songs coming up, worked out again when anything they depend on changed.
    pub fn lineup(&mut self) -> &Lineup {
        let key = LineupKey {
            version: self.version,
            queue: self.queue.clone(),
            playing: self.playing,
            last_played: self.last_played,
            order_pos: self.order_pos,
            mode: self.play_mode,
            repeat: self.repeat,
            stopped: self.now.is_none(),
        };
        if self.jukebox.key.as_ref() != Some(&key) || self.jukebox.refreshed.elapsed() >= REFRESH {
            // The selection stays on its song while the list moves, unless it is on the
            // next song: then it stays on whatever is next.
            let selected = self
                .jukebox
                .lineup
                .songs
                .get(self.jukebox.cursor)
                .filter(|_| self.jukebox.cursor > 0)
                .map(|s| s.track.uid);
            self.jukebox.lineup = self.build_lineup();
            self.jukebox.key = Some(key);
            self.jukebox.refreshed = Instant::now();
            let songs = &self.jukebox.lineup.songs;
            if let Some(pos) = selected.and_then(|u| songs.iter().position(|s| s.track.uid == u)) {
                self.jukebox.cursor = pos;
            }
            self.jukebox.cursor = self.jukebox.cursor.min(songs.len().saturating_sub(1));
        }
        &self.jukebox.lineup
    }

    fn build_lineup(&mut self) -> Lineup {
        let root = &self.root;
        let mut ok = HashSet::new();
        root.walk(&mut |g| ok.extend(g.tracks.iter().filter(|t| t.is_playable()).map(|t| t.uid)));
        let playable = |u: u64| ok.contains(&u);
        let stopped = self.now.is_none();
        let current = if stopped { None } else { self.playing };
        let mut uids: Vec<(u64, Option<usize>)> = self
            .queue
            .iter()
            .enumerate()
            .filter(|&(_, &u)| playable(u))
            .map(|(i, &u)| (u, Some(i + 1)))
            .collect();
        let st = PlayState {
            root: &self.root,
            mode: self.play_mode,
            repeat: self.repeat,
            version: self.version,
        };
        let end = if !stopped && self.repeat == RepeatMode::RepeatTrack {
            // The playing song (or the last queued one) plays again after the queue.
            if uids.is_empty()
                && let Some(u) = current
            {
                uids.push((u, None));
            }
            if uids.is_empty() {
                LineupEnd::Stops
            } else {
                LineupEnd::Repeats
            }
        } else {
            let from = if stopped && uids.is_empty() {
                // Play starts again with the last song played, or the first in order.
                let first = self
                    .last_played
                    .filter(|&u| playable(u))
                    .or_else(|| self.engine.upcoming(&st, None, 1).0.first().copied());
                uids.extend(first.map(|u| (u, None)));
                first
            } else {
                self.order_pos
                    .filter(|&u| root.locate(u).is_some())
                    .or(uids.last().map(|u| u.0))
                    .or(current)
            };
            let room = LINEUP_LIMIT.saturating_sub(uids.len());
            let (more, end) = self.engine.upcoming(&st, from, room);
            uids.extend(more.into_iter().map(|u| (u, None)));
            match end {
                OrderEnd::Continues => LineupEnd::More,
                OrderEnd::Stops => LineupEnd::Stops,
                OrderEnd::Reshuffles => LineupEnd::Reshuffles,
            }
        };

        // Selection codes come from the songs' places in the playlist.
        let wanted: HashSet<u64> = uids.iter().map(|u| u.0).chain(current).collect();
        let mut found: HashMap<u64, (usize, Track)> = HashMap::new();
        let mut index = 0;
        root.walk(&mut |g| {
            for t in &g.tracks {
                if wanted.contains(&t.uid) {
                    found.entry(t.uid).or_insert_with(|| (index, t.clone()));
                }
                index += 1;
            }
        });
        let songs = uids
            .into_iter()
            .filter_map(|(uid, queued)| {
                let (i, track) = found.get(&uid)?;
                Some(Song {
                    disc: Disc::for_path(&track.path),
                    code: selection_code(*i),
                    queued,
                    track: track.clone(),
                })
            })
            .collect();
        Lineup {
            songs,
            end,
            playing_code: current
                .and_then(|u| found.get(&u))
                .map(|(i, _)| selection_code(*i)),
        }
    }

    /// When each song of the lineup starts, from now: `None` while stopped, and from the
    /// first song whose length is unknown on.
    pub fn lineup_start_times(&self) -> Vec<Option<Duration>> {
        let shared = self.audio.shared();
        let mut at = self.now.as_ref().and_then(|now| {
            let length = now.duration.unwrap_or(shared.duration());
            (!length.is_zero()).then(|| length.saturating_sub(shared.position()))
        });
        self.jukebox
            .lineup
            .songs
            .iter()
            .map(|song| {
                let starts = at;
                at = at.zip(song.track.duration()).map(|(a, d)| a + d);
                starts
            })
            .collect()
    }

    /// Keys of the jukebox and horse views. Returns false for keys they leave to the player
    /// (playback, volume, other views).
    pub(crate) fn on_lineup_action(&mut self, action: Action) -> bool {
        let count = self.lineup().songs.len();
        let last = count.saturating_sub(1);
        let page = self.jukebox.page.max(1);
        let cursor = self.jukebox.cursor.min(last);
        let selected = self
            .jukebox
            .lineup
            .songs
            .get(cursor)
            .map(|s| (s.track.uid, s.queued));
        match action {
            Action::Jukebox => self.toggle_lineup_view(View::Jukebox),
            Action::Horse => self.toggle_lineup_view(View::Horse),
            Action::Close | Action::Back => self.toggle_lineup_view(self.view),
            Action::CursorUp => self.jukebox.cursor = cursor.saturating_sub(1),
            Action::CursorDown => self.jukebox.cursor = (cursor + 1).min(last),
            Action::PageUp => self.jukebox.cursor = cursor.saturating_sub(page),
            Action::PageDown => self.jukebox.cursor = (cursor + page).min(last),
            Action::Home => self.jukebox.cursor = 0,
            Action::End => self.jukebox.cursor = last,
            // Plays the selected song now. A queued one plays out of turn, like in the queue
            // list; any other moves the play order there, like picking it in the playlist.
            Action::Activate => {
                if let Some((uid, queued)) = selected {
                    self.play(uid, queued.is_none());
                }
            }
            Action::Enqueue => {
                if let Some((uid, _)) = selected {
                    self.toggle_queued(uid);
                }
            }
            Action::Delete => match selected {
                Some((uid, Some(_))) => self.toggle_queued(uid),
                Some(_) => self.banner(
                    BannerLevel::Info,
                    "Only queued songs can be taken off the jukebox",
                ),
                None => {}
            },
            // Reorders queued songs.
            Action::MoveUp | Action::MoveDown => {
                if let Some((_, Some(pos))) = selected {
                    let (from, up) = (pos - 1, action == Action::MoveUp);
                    let to = if up {
                        from.checked_sub(1)
                    } else {
                        Some(from + 1)
                    };
                    if let Some(to) = to.filter(|&t| t < self.queue.len()) {
                        self.queue.swap(from, to);
                        self.requeue();
                        self.jukebox.cursor = if up { cursor - 1 } else { cursor + 1 };
                        // Follow the song, not the place.
                        self.jukebox.key = None;
                    }
                }
            }
            // Playlist edits that would act on the hidden playlist.
            Action::RenameGroup | Action::NewGroup | Action::Search => {}
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{AudioEvent, StreamInfo};
    use crate::model::{Group, TrackMetadata};
    use crate::ui::tests::{render, test_app};

    fn album(n: usize) -> Group {
        let mut root = Group::new(0, "Root");
        root.tracks = (0..n)
            .map(|i| {
                let mut t = Track::new(format!("t{i}.flac"));
                t.metadata = Some(TrackMetadata {
                    title: Some(format!("Song {i}")),
                    artist: Some("The Band".into()),
                    duration: Duration::from_secs(60 * (i as u64 + 1)),
                    ..Default::default()
                });
                t
            })
            .collect();
        root
    }

    fn started(app: &mut App, i: usize) {
        let path = app.root.tracks[i].path.clone();
        app.on_audio_event(AudioEvent::TrackStarted {
            path,
            duration: Some(Duration::from_secs(60 * (i as u64 + 1))),
            info: StreamInfo {
                codec: "flac".into(),
                sample_rate: 44_100,
                channels: 2,
            },
        });
    }

    fn uids(app: &App) -> Vec<u64> {
        app.root.tracks.iter().map(|t| t.uid).collect()
    }

    fn lineup_uids(app: &mut App) -> Vec<u64> {
        app.lineup().songs.iter().map(|s| s.track.uid).collect()
    }

    #[test]
    fn selection_codes_count_like_a_jukebox() {
        let codes: Vec<_> = [0, 1, 9, 10, 19, 199, 200, 209, 210]
            .into_iter()
            .map(selection_code)
            .collect();
        assert_eq!(
            codes,
            ["A1", "A2", "A0", "B1", "B0", "V0", "AA1", "AA0", "AB1"]
        );
    }

    #[test]
    fn the_lineup_is_the_queue_then_the_play_order() {
        let mut app = test_app(album(6));
        let t = uids(&app);
        app.play(t[0], true);
        started(&mut app, 0);
        app.queue = vec![t[4], t[2]];
        app.requeue();
        assert_eq!(
            lineup_uids(&mut app),
            [t[4], t[2], t[1], t[2], t[3], t[4], t[5]]
        );
        let lineup = app.lineup();
        assert_eq!(lineup.end, LineupEnd::Stops);
        assert_eq!(lineup.songs[0].queued, Some(1));
        assert_eq!(lineup.songs[2].queued, None);
        assert_eq!(lineup.songs[0].code, "A5");
        assert_eq!(lineup.playing_code.as_deref(), Some("A1"));
        // The first song of the lineup is what plays next.
        assert_eq!(app.pending_next, Some(t[4]));

        // Start times add up the lengths, from what is left of the playing song.
        app.audio.shared().set_position(Duration::from_secs(20));
        let starts = app.lineup_start_times();
        assert_eq!(starts[0], Some(Duration::from_secs(40)));
        assert_eq!(starts[1], Some(Duration::from_secs(40 + 300)));
        assert_eq!(starts[2], Some(Duration::from_secs(40 + 300 + 180)));

        // A song without a known length makes the later times unknown.
        app.root.tracks[2].metadata = None;
        app.jukebox.key = None;
        app.lineup();
        let starts = app.lineup_start_times();
        assert!(starts[1].is_some() && starts[2].is_none() && starts[6].is_none());
    }

    #[test]
    fn repeat_modes_show_how_the_lineup_ends() {
        let mut app = test_app(album(3));
        let t = uids(&app);
        app.play(t[1], true);
        started(&mut app, 1);
        app.repeat = RepeatMode::RepeatAll;
        let songs = lineup_uids(&mut app);
        assert_eq!(songs.len(), LINEUP_LIMIT);
        assert_eq!(songs[..4], [t[2], t[0], t[1], t[2]]);
        assert_eq!(app.lineup().end, LineupEnd::More);

        app.repeat = RepeatMode::RepeatTrack;
        assert_eq!(lineup_uids(&mut app), [t[1]]);
        assert_eq!(app.lineup().end, LineupEnd::Repeats);

        app.repeat = RepeatMode::Off;
        app.play_mode = PlayMode::AllTracksShuffled;
        app.repeat = RepeatMode::RepeatAll;
        let end = app.lineup().end;
        assert!(
            matches!(end, LineupEnd::Reshuffles | LineupEnd::More),
            "{end:?}"
        );
    }

    #[test]
    fn while_stopped_the_lineup_starts_with_what_play_would_start() {
        let mut app = test_app(album(4));
        let t = uids(&app);
        assert_eq!(lineup_uids(&mut app), t);
        app.last_played = Some(t[2]);
        assert_eq!(lineup_uids(&mut app), [t[2], t[3]]);
        assert!(app.lineup_start_times().iter().all(Option::is_none));
    }

    #[test]
    fn f4_opens_the_jukebox_and_its_keys_pick_and_queue_songs() {
        let mut app = test_app(album(5));
        let t = uids(&app);
        app.play(t[0], true);
        started(&mut app, 0);
        app.on_key(crate::ui::keymap::KeyBinding::parse("F4").unwrap());
        assert_eq!(app.view, View::Jukebox);
        render(&mut app, 120, 34);

        // Down to the third song and queue it: it moves to the front.
        app.on_action(Action::CursorDown);
        app.on_action(Action::CursorDown);
        app.on_action(Action::Enqueue);
        assert_eq!(app.queue, [t[3]]);
        assert_eq!(lineup_uids(&mut app)[0], t[3]);
        assert_eq!(app.jukebox.cursor, 0, "the selection follows the song");
        // Delete takes it off again; on other songs it does nothing.
        app.on_action(Action::Delete);
        assert!(app.queue.is_empty());
        app.on_action(Action::Delete);
        assert_eq!(
            app.root.tracks.len(),
            5,
            "the playlist is never edited from here"
        );

        // Enter plays the selected song now.
        app.jukebox.cursor = 1;
        app.on_action(Action::Activate);
        assert_eq!(app.pending_load, Some(t[2]));

        // On the next song, the selection stays on whatever is next.
        app.repeat = RepeatMode::RepeatAll;
        app.jukebox.cursor = 0;
        app.lineup();
        started(&mut app, 2);
        assert_eq!(lineup_uids(&mut app)[app.jukebox.cursor], t[3]);
        app.repeat = RepeatMode::Off;

        // Reordering queued songs.
        app.queue = vec![t[3], t[4]];
        app.lineup();
        app.jukebox.cursor = 0;
        app.on_action(Action::MoveDown);
        assert_eq!(app.queue, [t[4], t[3]]);
        assert_eq!(app.jukebox.cursor, 1);

        // Esc (or F4 again) goes back to where it was opened from.
        app.on_action(Action::Close);
        assert_eq!(app.view, View::Playlist);
        app.on_action(Action::FileManager);
        app.on_action(Action::Jukebox);
        assert_eq!(app.view, View::Jukebox);
        app.on_action(Action::Jukebox);
        assert_eq!(app.view, View::FileManager);
    }

    #[test]
    fn a_song_change_spins_the_turntable_into_a_record_change() {
        let mut app = test_app(album(3));
        let t = uids(&app);
        app.play(t[0], true);
        started(&mut app, 0);
        app.on_action(Action::Jukebox);
        app.update_jukebox(Duration::from_millis(16));
        assert!(
            !app.jukebox.scene.is_changing(),
            "opening shows the record at once"
        );
        started(&mut app, 1);
        app.update_jukebox(Duration::from_millis(16));
        assert!(app.jukebox.scene.is_changing());
        assert!(app.is_animating());
        assert_eq!(app.effective_fps(), 30);
    }
}
