//! Application state and input handling for the TUI.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::audio::{AudioCommand, AudioEvent, AudioHandle, StreamInfo};
use crate::model::{Group, PlayMode, PlaybackStatus, RepeatMode, Track, TrackRef};

use super::keymap::{Action, Context, KeyBinding, Keymap};
use super::theme::{Symbols, Theme};

const VOLUME_STEP: f32 = 0.05;
const SEEK_STEP_SECS: i64 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct Banner {
    pub text: String,
    pub level: BannerLevel,
    pub until: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    None,
    Help { scroll: u16 },
}

/// A row of the playlist view: subgroups are listed before tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Group(usize),
    Track(usize),
}

#[derive(Debug, Clone)]
pub struct NowPlaying {
    pub track: Option<TrackRef>,
    pub path: PathBuf,
    pub info: StreamInfo,
    pub duration: Option<Duration>,
}

pub struct App {
    pub audio: AudioHandle,
    pub keymap: Keymap,
    pub theme: Theme,
    pub symbols: Symbols,
    pub fps: u32,
    pub root: Group,
    /// Path of the group shown in the playlist view.
    pub cwd: Vec<usize>,
    pub cursor: usize,
    pub scroll: usize,
    /// Rows visible in the playlist (updated while drawing; used for paging).
    pub list_height: usize,
    pub playing: Option<TrackRef>,
    /// Last track that played, to resume from after Stop.
    pub last_played: Option<TrackRef>,
    pending_load: Option<TrackRef>,
    pending_queue: Option<TrackRef>,
    pub now: Option<NowPlaying>,
    pub banner: Option<Banner>,
    pub overlay: Overlay,
    pub play_mode: PlayMode,
    pub repeat: RepeatMode,
    pub quit: bool,
    failures: usize,
}

impl App {
    pub fn new(audio: AudioHandle, root: Group) -> Self {
        App {
            audio,
            keymap: Keymap::default(),
            theme: Theme::default(),
            symbols: Symbols::default(),
            fps: 60,
            root,
            cwd: Vec::new(),
            cursor: 0,
            scroll: 0,
            list_height: 10,
            playing: None,
            last_played: None,
            pending_load: None,
            pending_queue: None,
            now: None,
            banner: None,
            overlay: Overlay::None,
            play_mode: PlayMode::default(),
            repeat: RepeatMode::default(),
            quit: false,
            failures: 0,
        }
    }

    /// A playlist with the given files in its root group.
    pub fn root_from_files(files: Vec<PathBuf>) -> Group {
        let mut root = Group::new(0, "Root");
        root.tracks = files.into_iter().map(Track::new).collect();
        root
    }

    pub fn status(&self) -> PlaybackStatus {
        self.audio.shared().status()
    }

    /// Whether the screen changes on its own (progress bar, spectrum).
    pub fn is_animating(&self) -> bool {
        self.status() == PlaybackStatus::Playing
    }

    pub fn current_group(&self) -> &Group {
        self.root.group_at(&self.cwd).unwrap_or(&self.root)
    }

    pub fn rows(&self) -> Vec<Row> {
        let g = self.current_group();
        (0..g.subgroups.len()).map(Row::Group).chain((0..g.tracks.len()).map(Row::Track)).collect()
    }

    /// Names of the groups from the root to the current one.
    pub fn breadcrumb(&self) -> Vec<&str> {
        let mut names = vec![self.root.name.as_str()];
        let mut g = &self.root;
        for &i in &self.cwd {
            match g.subgroups.get(i) {
                Some(sub) => {
                    names.push(sub.name.as_str());
                    g = sub;
                }
                None => break,
            }
        }
        names
    }

    pub fn banner(&mut self, level: BannerLevel, text: impl Into<String>) {
        let secs = match level {
            BannerLevel::Info => 3,
            BannerLevel::Warning => 5,
            BannerLevel::Error => 8,
        };
        self.banner = Some(Banner {
            text: text.into(),
            level,
            until: Some(Instant::now() + Duration::from_secs(secs)),
        });
    }

    /// Expires timed state. Returns true when the screen needs redrawing.
    pub fn tick(&mut self) -> bool {
        if let Some(until) = self.banner.as_ref().and_then(|b| b.until) {
            if Instant::now() >= until {
                self.banner = None;
                return true;
            }
        }
        false
    }

    // ---- input ------------------------------------------------------------------------

    pub fn context(&self) -> Context {
        Context::Playlist
    }

    pub fn on_key(&mut self, key: KeyBinding) {
        let Some(action) = self.keymap.lookup(self.context(), &key) else { return };
        if let Overlay::Help { scroll } = self.overlay {
            let scroll = match action {
                Action::CursorUp => scroll.saturating_sub(1),
                Action::CursorDown => scroll.saturating_add(1),
                Action::PageUp => scroll.saturating_sub(10),
                Action::PageDown => scroll.saturating_add(10),
                Action::Home => 0,
                Action::Close | Action::Help | Action::Quit | Action::Activate | Action::Back => {
                    self.overlay = Overlay::None;
                    return;
                }
                _ => {
                    self.on_action(action);
                    return;
                }
            };
            self.overlay = Overlay::Help { scroll };
            return;
        }
        self.on_action(action);
    }

    pub fn on_action(&mut self, action: Action) {
        let rows = self.rows().len();
        let page = self.list_height.max(1);
        match action {
            Action::Quit => self.quit = true,
            Action::CursorUp => self.cursor = self.cursor.saturating_sub(1),
            Action::CursorDown => self.cursor = (self.cursor + 1).min(rows.saturating_sub(1)),
            Action::PageUp => self.cursor = self.cursor.saturating_sub(page),
            Action::PageDown => self.cursor = (self.cursor + page).min(rows.saturating_sub(1)),
            Action::Home => self.cursor = 0,
            Action::End => self.cursor = rows.saturating_sub(1),
            Action::Activate => self.activate(),
            Action::Back => self.leave_group(),
            Action::PlayPause => self.play_pause(),
            Action::Stop => {
                self.audio.send(AudioCommand::Stop);
                self.last_played = self.playing.take().or(self.last_played.take());
                self.now = None;
                self.pending_load = None;
                self.pending_queue = None;
            }
            Action::NextTrack => {
                let from = self.playing.clone().or_else(|| self.last_played.clone());
                match from.and_then(|r| self.next_after(&r)) {
                    Some(next) => self.play(next),
                    None => self.banner(BannerLevel::Info, "End of playlist"),
                }
            }
            Action::PrevTrack => {
                let from = self.playing.clone().or_else(|| self.last_played.clone());
                if let Some(prev) = from.and_then(|r| self.prev_before(&r)) {
                    self.play(prev);
                }
            }
            Action::SeekForward => self.audio.send(AudioCommand::SeekRelative(SEEK_STEP_SECS)),
            Action::SeekBackward => self.audio.send(AudioCommand::SeekRelative(-SEEK_STEP_SECS)),
            Action::VolumeUp => self.change_volume(VOLUME_STEP),
            Action::VolumeDown => self.change_volume(-VOLUME_STEP),
            Action::CyclePlayMode => {
                self.play_mode = self.play_mode.cycle();
                self.banner(BannerLevel::Info, format!("Play mode: {}", self.play_mode.label()));
            }
            Action::CycleRepeat => {
                self.repeat = self.repeat.cycle();
                self.banner(BannerLevel::Info, format!("Repeat: {}", self.repeat.label()));
            }
            Action::ToggleGroupShuffle => {
                let cwd = self.cwd.clone();
                if let Some(g) = self.root.group_at_mut(&cwd) {
                    g.shuffle = !g.shuffle;
                    let text = format!("Shuffle {}: {}", g.name, if g.shuffle { "ON" } else { "OFF" });
                    self.banner(BannerLevel::Info, text);
                }
            }
            Action::Help => self.overlay = Overlay::Help { scroll: 0 },
            Action::Close => self.banner = None,
            #[cfg(debug_assertions)]
            Action::DebugPanic => panic!("debug panic requested from the keyboard"),
            other => {
                self.banner(BannerLevel::Info, format!("{}: not available yet", other.description()))
            }
        }
    }

    fn activate(&mut self) {
        match self.rows().get(self.cursor).copied() {
            Some(Row::Group(i)) => {
                self.cwd.push(i);
                self.cursor = 0;
                self.scroll = 0;
            }
            Some(Row::Track(i)) => self.play(TrackRef::new(self.cwd.clone(), i)),
            None => {}
        }
    }

    fn leave_group(&mut self) {
        if let Some(i) = self.cwd.pop() {
            self.cursor = i;
            self.scroll = 0;
        }
    }

    fn change_volume(&mut self, delta: f32) {
        let v = (self.audio.shared().volume() + delta).clamp(0.0, 1.0);
        // Snap to the 5% grid so repeated steps land on round numbers.
        let v = (v * 20.0).round() / 20.0;
        self.audio.send(AudioCommand::SetVolume(v));
        self.audio.shared().set_volume(v);
    }

    fn play_pause(&mut self) {
        match self.status() {
            PlaybackStatus::Playing => self.audio.send(AudioCommand::Pause),
            PlaybackStatus::Paused => self.audio.send(AudioCommand::Resume),
            PlaybackStatus::Stopped => {
                let target = match self.rows().get(self.cursor) {
                    Some(Row::Track(i)) => Some(TrackRef::new(self.cwd.clone(), *i)),
                    _ => self.last_played.clone().or_else(|| self.root.track_refs().into_iter().next()),
                };
                if let Some(target) = target {
                    self.play(target);
                }
            }
        }
    }

    pub fn play(&mut self, track: TrackRef) {
        let Some(path) = self.root.track(&track).map(|t| t.path.clone()) else { return };
        self.pending_queue = None;
        self.audio.send(AudioCommand::QueueNext(None));
        self.audio.send(AudioCommand::LoadAndPlay(path));
        self.pending_load = Some(track);
    }

    // ---- playlist order (simple in-group order; the full engine arrives with groups) ----

    fn next_after(&self, r: &TrackRef) -> Option<TrackRef> {
        let g = self.root.group_at(&r.group)?;
        if r.index + 1 < g.tracks.len() {
            Some(TrackRef::new(r.group.clone(), r.index + 1))
        } else if self.repeat == RepeatMode::RepeatAll || self.repeat == RepeatMode::RepeatGroup {
            (!g.tracks.is_empty()).then(|| TrackRef::new(r.group.clone(), 0))
        } else {
            None
        }
    }

    fn prev_before(&self, r: &TrackRef) -> Option<TrackRef> {
        (r.index > 0).then(|| TrackRef::new(r.group.clone(), r.index - 1))
    }

    fn successor(&self, r: &TrackRef) -> Option<TrackRef> {
        if self.repeat == RepeatMode::RepeatTrack {
            return Some(r.clone());
        }
        self.next_after(r)
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

    fn resolve(&self, path: &Path) -> Option<TrackRef> {
        [&self.pending_queue, &self.pending_load]
            .into_iter()
            .flatten()
            .find(|r| self.root.track(r).is_some_and(|t| t.path == path))
            .cloned()
            .or_else(|| self.root.find_path(path))
    }

    fn on_audio_event(&mut self, ev: AudioEvent) {
        match ev {
            AudioEvent::TrackStarted { path, duration, info } => {
                let track = self.resolve(&path);
                self.failures = 0;
                self.pending_load = None;
                self.playing = track.clone();
                if track.is_some() {
                    self.last_played = track.clone();
                }
                self.now = Some(NowPlaying { track: track.clone(), path, info, duration });
                self.pending_queue = track.and_then(|r| self.successor(&r));
                let next_path = self.pending_queue.as_ref().and_then(|r| self.root.track(r)).map(|t| t.path.clone());
                self.audio.send(AudioCommand::QueueNext(next_path));
            }
            AudioEvent::TrackFailed { path, error, missing } => {
                let track = self.resolve(&path);
                if let Some(t) = track.as_ref().and_then(|r| self.root.track_mut(r)) {
                    t.missing = missing;
                    t.broken = !missing;
                }
                let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
                self.banner(BannerLevel::Warning, format!("{name}: {error}"));
                self.failures += 1;
                if self.failures >= self.root.total_tracks().max(1) {
                    self.banner(BannerLevel::Error, "No playable tracks in the playlist");
                    self.pending_load = None;
                    self.pending_queue = None;
                    return;
                }
                let Some(track) = track else { return };
                let skip_to = self.next_after(&track);
                if self.pending_queue.as_ref() == Some(&track) {
                    self.pending_queue = skip_to.clone();
                    let p = skip_to.and_then(|r| self.root.track(&r)).map(|t| t.path.clone());
                    self.audio.send(AudioCommand::QueueNext(p));
                } else if self.pending_load.as_ref() == Some(&track) {
                    self.pending_load = None;
                    if let Some(next) = skip_to {
                        self.play(next);
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
}
