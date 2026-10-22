//! Application state and input dispatch for the TUI.
//!
//! Behaviour is split across `playback.rs` (play order and audio events) and `editing.rs`
//! (playlist editing, the file manager and background work).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;

use crate::audio::{AudioCommand, AudioHandle, StreamInfo};
use crate::library::scanner::MetadataScanner;
use crate::model::{Group, PlayMode, PlaybackStatus, RepeatMode, Track, TrackMetadata};
use crate::playlist::engine::Engine;

use super::file_manager::{FileManager, FmMode};
use super::keymap::{Action, Context, KeyBinding, Keymap};
use super::prompt::{Prompt, PromptKind, PromptResult};
use super::theme::{Symbols, Theme};

pub(crate) const VOLUME_STEP: f32 = 0.05;
pub(crate) const SEEK_STEP_SECS: i64 = 5;

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
pub enum View {
    Playlist,
    FileManager,
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
    /// Uid of the playing track, if it is (still) in the playlist.
    pub track: Option<u64>,
    pub path: PathBuf,
    pub info: StreamInfo,
    pub duration: Option<Duration>,
}

/// A recursive folder add running in the background.
pub(crate) struct FolderScan {
    pub files: Receiver<Vec<PathBuf>>,
    pub target: Vec<usize>,
    pub added: usize,
}

pub struct App {
    pub audio: AudioHandle,
    pub keymap: Keymap,
    pub theme: Theme,
    pub symbols: Symbols,
    pub fps: u32,
    pub root: Group,
    /// Bumped on every playlist edit (the play order depends on it).
    pub version: u64,
    /// Path of the group shown in the playlist view.
    pub cwd: Vec<usize>,
    pub cursor: usize,
    pub scroll: usize,
    /// Rows visible in the playlist (updated while drawing; used for paging).
    pub list_height: usize,
    pub view: View,
    pub fm: FileManager,
    pub prompt: Option<Prompt>,
    pub overlay: Overlay,
    /// Uid of the audible track.
    pub playing: Option<u64>,
    /// Last track that played, to resume from after Stop.
    pub last_played: Option<u64>,
    pub(crate) pending_load: Option<u64>,
    pub(crate) pending_queue: Option<u64>,
    pub now: Option<NowPlaying>,
    pub banner: Option<Banner>,
    pub play_mode: PlayMode,
    pub repeat: RepeatMode,
    pub quit: bool,
    pub(crate) failures: usize,
    pub(crate) engine: Engine,
    pub(crate) scanner: MetadataScanner,
    pub(crate) meta_cache: HashMap<PathBuf, TrackMetadata>,
    pub(crate) scans: Vec<FolderScan>,
}

impl App {
    pub fn new(audio: AudioHandle, root: Group) -> Self {
        let mut app = App {
            audio,
            keymap: Keymap::default(),
            theme: Theme::default(),
            symbols: Symbols::default(),
            fps: 60,
            root,
            version: 0,
            cwd: Vec::new(),
            cursor: 0,
            scroll: 0,
            list_height: 10,
            view: View::Playlist,
            fm: FileManager::new(crate::paths::default_music_dir()),
            prompt: None,
            overlay: Overlay::None,
            playing: None,
            last_played: None,
            pending_load: None,
            pending_queue: None,
            now: None,
            banner: None,
            play_mode: PlayMode::default(),
            repeat: RepeatMode::default(),
            quit: false,
            failures: 0,
            engine: Engine::new(None),
            scanner: MetadataScanner::new(),
            meta_cache: HashMap::new(),
            scans: Vec::new(),
        };
        app.request_all_metadata();
        app
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

    pub fn cursor_row(&self) -> Option<Row> {
        self.rows().get(self.cursor).copied()
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

    pub fn is_scanning(&self) -> bool {
        !self.scans.is_empty() || self.scanner.backlog() > 0
    }

    // ---- input ------------------------------------------------------------------------

    pub fn context(&self) -> Context {
        match self.view {
            View::Playlist => Context::Playlist,
            View::FileManager => Context::FileManager,
        }
    }

    pub fn on_key(&mut self, key: KeyBinding) {
        if let Some(prompt) = self.prompt.as_mut() {
            match prompt.handle(key) {
                PromptResult::Editing => {}
                PromptResult::Cancelled => self.prompt = None,
                PromptResult::Submitted(answer) => {
                    if let Some(prompt) = self.prompt.take() {
                        self.on_prompt(prompt.kind, answer);
                    }
                }
            }
            return;
        }
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
        if self.view == View::FileManager && self.on_file_manager_action(action) {
            return;
        }
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
            Action::Stop => self.stop(),
            Action::NextTrack => self.next_track(),
            Action::PrevTrack => self.prev_track(),
            Action::SeekForward => self.audio.send(AudioCommand::SeekRelative(SEEK_STEP_SECS)),
            Action::SeekBackward => self.audio.send(AudioCommand::SeekRelative(-SEEK_STEP_SECS)),
            Action::VolumeUp => self.change_volume(VOLUME_STEP),
            Action::VolumeDown => self.change_volume(-VOLUME_STEP),
            Action::CyclePlayMode => {
                self.play_mode = self.play_mode.cycle();
                self.banner(BannerLevel::Info, format!("Play mode: {}", self.play_mode.label()));
                self.requeue();
            }
            Action::CycleRepeat => {
                self.repeat = self.repeat.cycle();
                self.banner(BannerLevel::Info, format!("Repeat: {}", self.repeat.label()));
                self.requeue();
            }
            Action::ToggleGroupShuffle => self.toggle_group_shuffle(),
            Action::FileManager => self.open_file_manager(FmMode::AddFiles),
            Action::NewGroup => {
                self.prompt = Some(Prompt::new(PromptKind::NewGroup, "New group name: ", ""));
            }
            Action::RenameGroup => self.start_rename(),
            Action::Delete => self.delete_at_cursor(),
            Action::MoveUp => self.move_at_cursor(-1),
            Action::MoveDown => self.move_at_cursor(1),
            Action::Help => self.overlay = Overlay::Help { scroll: 0 },
            Action::Close => self.banner = None,
            #[cfg(debug_assertions)]
            Action::DebugPanic => panic!("debug panic requested from the keyboard"),
            other => {
                self.banner(BannerLevel::Info, format!("{}: not available yet", other.description()))
            }
        }
    }

    fn on_prompt(&mut self, kind: PromptKind, answer: String) {
        match kind {
            PromptKind::SelectGlob => {
                if answer.is_empty() {
                    return;
                }
                match self.fm.select_matching(&answer) {
                    Ok(n) => self.banner(BannerLevel::Info, format!("{n} entries match \"{answer}\"")),
                    Err(e) => self.banner(BannerLevel::Warning, format!("Bad pattern: {e}")),
                }
            }
            PromptKind::NewGroup => self.create_group(answer),
            PromptKind::RenameGroup => self.rename_group(answer),
            PromptKind::Confirm(confirm) => self.confirmed(confirm),
            other => log::debug!("unhandled prompt {other:?}"),
        }
    }

    fn activate(&mut self) {
        match self.cursor_row() {
            Some(Row::Group(i)) => {
                self.cwd.push(i);
                self.cursor = 0;
                self.scroll = 0;
            }
            Some(Row::Track(i)) => {
                if let Some(uid) = self.current_group().tracks.get(i).map(|t| t.uid) {
                    self.play(uid, true);
                }
            }
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

    pub fn open_file_manager(&mut self, mode: FmMode) {
        self.fm.mode = mode;
        self.fm.reload();
        self.view = View::FileManager;
    }
}
