//! Application state and input dispatch for the TUI.
//!
//! Behaviour is split across `playback.rs` (play order and audio events) and `editing.rs`
//! (playlist editing, the file manager and background work).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;

use crate::audio::{AudioCommand, AudioHandle, StreamInfo};
use crate::config::{Config, ReplayGainMode};
use crate::defrag::Defrag;
use crate::hyperspace::Hyperspace;
use crate::library::scanner::MetadataScanner;
use crate::matrix::MatrixRain;
use crate::model::{Group, PlayMode, PlaybackStatus, RepeatMode, Track, TrackMetadata};
use crate::playlist::engine::Engine;
use crate::spectrum::SpectrumAnalyzer;

use super::file_manager::{FileManager, FmMode};
use super::keymap::{Action, Context, KeyBinding, Keymap};
use super::prompt::{Prompt, PromptKind, PromptResult};
use super::theme::{Symbols, Theme};

pub(crate) const VOLUME_STEP: f32 = 0.05;
/// Scrolling text holds still this long after a track starts, then moves a column a step.
const MARQUEE_PAUSE: Duration = Duration::from_secs(2);
const MARQUEE_STEP: Duration = Duration::from_millis(200);
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
    /// Fullscreen spectrum analyser.
    Visualizer,
}

/// How the spectrum is drawn in the wide strip and the fullscreen view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visualizer {
    Bars,
    /// Matrix-style digital rain of the playing file's bytes, driven by the spectrum.
    Matrix,
    /// Windows 98-style disk defragmenter; fully defragmented when the song ends.
    Defrag,
    /// Audio-reactive 3D demoscene: warp starfield, spectrum sphere, beat shockwaves.
    Hyperspace,
}

impl Visualizer {
    pub fn toggle(self) -> Self {
        match self {
            Visualizer::Bars => Visualizer::Matrix,
            Visualizer::Matrix => Visualizer::Defrag,
            Visualizer::Defrag => Visualizer::Hyperspace,
            Visualizer::Hyperspace => Visualizer::Bars,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Visualizer::Bars => "spectrum bars",
            Visualizer::Matrix => "Matrix rain of the file's bytes",
            Visualizer::Defrag => "disk defragmenter",
            Visualizer::Hyperspace => "HYPERSPACE",
        }
    }
}

/// Where the spectrum analyser is shown outside the fullscreen view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpectrumLayout {
    /// A full-width strip under the playlist and Now Playing panes.
    Wide,
    /// Inside the Now Playing pane.
    Pane,
    Off,
}

impl SpectrumLayout {
    pub fn cycle(self) -> Self {
        match self {
            SpectrumLayout::Wide => SpectrumLayout::Pane,
            SpectrumLayout::Pane => SpectrumLayout::Off,
            SpectrumLayout::Off => SpectrumLayout::Wide,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SpectrumLayout::Wide => "wide",
            SpectrumLayout::Pane => "in Now Playing",
            SpectrumLayout::Off => "off",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    None,
    Help {
        scroll: u16,
    },
    Log {
        scroll: u16,
    },
    /// Theme picker: `cursor` indexes `theme::THEMES`; `original` is restored on cancel.
    Themes {
        cursor: usize,
        original: &'static str,
    },
    /// The play queue; `cursor` indexes `App::queue`.
    Queue {
        cursor: usize,
    },
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
    /// The track sent to the audio thread to follow the playing one gaplessly.
    pub(crate) pending_next: Option<u64>,
    /// Tracks the user queued to play next, in order; normal play order resumes after them.
    pub queue: Vec<u64>,
    /// The last track played in normal order (or picked by the user). Queued tracks do not
    /// move it, so the play order continues from here once the queue is empty.
    pub(crate) order_pos: Option<u64>,
    pub now: Option<NowPlaying>,
    /// Scrolling text in Now Playing starts from here (reset when a track starts).
    pub(crate) marquee_since: Instant,
    /// Whether text was scrolling in the last drawn frame, and at which step.
    pub(crate) marquee_scrolling: bool,
    marquee_drawn: u64,
    pub banner: Option<Banner>,
    pub play_mode: PlayMode,
    pub repeat: RepeatMode,
    /// Playlist file last loaded or saved (default for the save prompt).
    pub playlist_path: Option<PathBuf>,
    /// The config file (it may not exist yet); the theme picker saves its choice there.
    pub config_path: Option<PathBuf>,
    /// Live search filter for the playlist view.
    pub filter: Option<String>,
    pub spectrum_layout: SpectrumLayout,
    pub visualizer: Visualizer,
    /// Digital rain made of the playing file's bytes (the Matrix visualizer).
    pub rain: MatrixRain,
    /// The disk being defragmented (the Defrag visualizer).
    pub defrag: Defrag,
    /// The HYPERSPACE demoscene visualizer.
    pub hyperspace: Hyperspace,
    /// View to return to when the fullscreen visualizer closes.
    pub(crate) view_before_visualizer: View,
    pub spectrum: SpectrumAnalyzer,
    last_spectrum_update: Instant,
    pub replaygain: ReplayGainMode,
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
            pending_next: None,
            queue: Vec::new(),
            order_pos: None,
            now: None,
            marquee_since: Instant::now(),
            marquee_scrolling: false,
            marquee_drawn: 0,
            banner: None,
            play_mode: PlayMode::default(),
            repeat: RepeatMode::default(),
            playlist_path: None,
            config_path: crate::paths::config_file(),
            filter: None,
            spectrum_layout: SpectrumLayout::Wide,
            visualizer: Visualizer::Bars,
            rain: MatrixRain::default(),
            defrag: Defrag::new(),
            hyperspace: Hyperspace::default(),
            view_before_visualizer: View::Playlist,
            spectrum: SpectrumAnalyzer::new(),
            last_spectrum_update: Instant::now(),
            replaygain: ReplayGainMode::Off,
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
            || (self.spectrum_visible()
                && (self.spectrum.is_active()
                    || self.rain.is_active()
                    || (self.visualizer == Visualizer::Hyperspace && self.hyperspace.is_active())))
    }

    pub fn current_group(&self) -> &Group {
        self.root.group_at(&self.cwd).unwrap_or(&self.root)
    }

    /// Rows of the current group, narrowed by the search filter if one is active.
    pub fn rows(&self) -> Vec<Row> {
        let g = self.current_group();
        let groups = (0..g.subgroups.len()).map(Row::Group);
        let tracks = (0..g.tracks.len()).map(Row::Track);
        match self.filter.as_deref().map(str::to_lowercase) {
            None => groups.chain(tracks).collect(),
            Some(needle) => groups
                .chain(tracks)
                .filter(|row| match *row {
                    Row::Group(i) => g.subgroups[i].name.to_lowercase().contains(&needle),
                    Row::Track(i) => {
                        let t = &g.tracks[i];
                        t.display_name().to_lowercase().contains(&needle)
                            || t.file_name().to_lowercase().contains(&needle)
                            || t.metadata
                                .as_ref()
                                .and_then(|m| m.album.as_ref())
                                .is_some_and(|a| a.to_lowercase().contains(&needle))
                    }
                })
                .collect(),
        }
    }

    /// Applies settings from `config.toml`. Returns warnings to show the user.
    pub fn apply_config(&mut self, config: &Config) -> Vec<String> {
        let (keymap, mut warnings) = config.keymap();
        self.keymap = keymap;
        let (theme, warning) =
            Theme::by_name(&config.ui.color_theme, super::theme::supports_truecolor());
        self.theme = theme;
        warnings.extend(warning);
        self.symbols = Symbols::by_name(&config.ui.charset);
        self.fps = config.ui.fps;
        self.spectrum_layout = match (config.ui.show_spectrum, config.ui.spectrum_layout.as_str()) {
            (false, _) => SpectrumLayout::Off,
            (true, "pane") => SpectrumLayout::Pane,
            _ => SpectrumLayout::Wide,
        };
        self.visualizer = match config.ui.visualizer.as_str() {
            "matrix" => Visualizer::Matrix,
            "defrag" => Visualizer::Defrag,
            "hyperspace" => Visualizer::Hyperspace,
            _ => Visualizer::Bars,
        };
        if let Some(tuning) = crate::hyperspace::Tuning::preset(&config.ui.hyperspace_quality) {
            self.hyperspace.tuning = tuning;
        }
        self.replaygain = config.general.replaygain;
        self.audio
            .send(AudioCommand::SetReplayGain(config.general.replaygain));
        self.fm.show_hidden = config.ui.show_hidden;
        self.fm.go_to(
            crate::library::fs_browser::Location::Dir(config.music_dir()),
            None,
        );
        warnings
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
        if let Some(until) = self.banner.as_ref().and_then(|b| b.until)
            && Instant::now() >= until
        {
            self.banner = None;
            return true;
        }
        // Scrolling text moves on its own (while playing, frames are drawn anyway).
        if self.marquee_scrolling && !self.is_animating() {
            let step = self.marquee_step();
            if step != self.marquee_drawn {
                self.marquee_drawn = step;
                return true;
            }
        }
        false
    }

    /// How many columns scrolling text has moved (0 = not yet).
    pub fn marquee_step(&self) -> u64 {
        let elapsed = self.marquee_since.elapsed();
        match elapsed.checked_sub(MARQUEE_PAUSE) {
            Some(moving) => 1 + (moving.as_millis() / MARQUEE_STEP.as_millis()) as u64,
            None => 0,
        }
    }

    /// Time until scrolling text moves again, if any is scrolling.
    pub fn marquee_wait(&self) -> Option<Duration> {
        if !self.marquee_scrolling {
            return None;
        }
        let elapsed = self.marquee_since.elapsed();
        Some(match elapsed.checked_sub(MARQUEE_PAUSE) {
            Some(moving) => {
                let step = MARQUEE_STEP.as_nanos();
                Duration::from_nanos((step - moving.as_nanos() % step) as u64)
            }
            None => MARQUEE_PAUSE - elapsed,
        })
    }

    /// Feeds the spectrum analyser from the audio tap. Call once per loop iteration.
    pub fn update_spectrum(&mut self) {
        if let Some(tap) = self.audio.take_spectrum_tap() {
            self.spectrum
                .set_tap(tap, self.audio.shared().device_rate());
        }
        let dt = self.last_spectrum_update.elapsed();
        self.last_spectrum_update = Instant::now();
        if self.spectrum_visible() {
            self.spectrum.update(dt);
            if self.visualizer == Visualizer::Matrix {
                let shared = self.audio.shared();
                let duration = shared.duration().as_secs_f64();
                let fraction = if duration > 0.0 {
                    shared.position().as_secs_f64() / duration
                } else {
                    0.0
                };
                let path = self.now.as_ref().map(|n| n.path.as_path());
                self.rain.feed.set_source(path, fraction);
                self.rain.update(dt.as_secs_f32(), self.spectrum.bands());
            }
            if self.visualizer == Visualizer::Hyperspace {
                let wave = self.spectrum.waveform(256);
                let title = self.now_playing_title();
                let track = self.now.as_ref().map(|n| {
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    std::hash::Hash::hash(&n.path, &mut h);
                    std::hash::Hasher::finish(&h)
                });
                let playing = self.status() == PlaybackStatus::Playing;
                self.hyperspace.update(
                    dt.as_secs_f32(),
                    self.spectrum.bands(),
                    &wave,
                    track.zip(title.as_deref()),
                    playing,
                );
            }
        }
    }

    /// "Artist - Title" (or the file name) of the playing track.
    pub fn now_playing_title(&self) -> Option<String> {
        let now = self.now.as_ref()?;
        let from_tags = now
            .track
            .and_then(|u| self.root.track_by_uid(u))
            .map(|t| t.display_name());
        Some(from_tags.unwrap_or_else(|| {
            now.path
                .file_stem()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        }))
    }

    /// Frames per second for the current view. The HYPERSPACE framebuffer sends a lot of
    /// colour data to the terminal, so it is capped at 30.
    pub fn effective_fps(&self) -> u32 {
        let hyperspace_on_screen = self.visualizer == Visualizer::Hyperspace
            && (self.view == View::Visualizer
                || (self.view == View::Playlist && self.spectrum_layout == SpectrumLayout::Wide));
        if hyperspace_on_screen {
            self.fps.min(30)
        } else {
            self.fps
        }
    }

    /// Playback state for this frame, including the spectrum (spec §4 `AudioTelemetry`).
    pub fn telemetry(&self) -> crate::audio::AudioTelemetry {
        let mut t = crate::audio::AudioTelemetry::from_shared(self.audio.shared());
        t.spectrum_bins = self.spectrum.bins16();
        if let Some(meta) = self
            .playing
            .and_then(|u| self.root.track_by_uid(u))
            .and_then(|t| t.metadata.as_ref())
        {
            t.current_bitrate = meta.bitrate_kbps;
        }
        t
    }

    pub fn is_scanning(&self) -> bool {
        !self.scans.is_empty() || self.scanner.backlog() > 0
    }

    // ---- input ------------------------------------------------------------------------

    pub fn context(&self) -> Context {
        match self.view {
            View::Playlist => Context::Playlist,
            View::FileManager => Context::FileManager,
            View::Visualizer => Context::Playlist,
        }
    }

    /// Whether a spectrum is on screen (so the analyser needs updating).
    pub fn spectrum_visible(&self) -> bool {
        self.view == View::Visualizer || self.spectrum_layout != SpectrumLayout::Off
    }

    fn toggle_visualizer(&mut self) {
        if self.view == View::Visualizer {
            self.view = self.view_before_visualizer;
        } else {
            self.view_before_visualizer = self.view;
            self.view = View::Visualizer;
        }
    }

    pub fn on_key(&mut self, key: KeyBinding) {
        if let Some(prompt) = self.prompt.as_mut() {
            let searching = prompt.kind == PromptKind::Search;
            match prompt.handle(key) {
                PromptResult::Editing => {
                    if searching {
                        // Filter as you type.
                        let input = prompt.input.clone();
                        self.filter = Some(input).filter(|s| !s.is_empty());
                        self.cursor = 0;
                        self.scroll = 0;
                    }
                }
                PromptResult::Cancelled => {
                    self.prompt = None;
                    if searching {
                        self.filter = None;
                    }
                }
                PromptResult::Submitted(answer) => {
                    if let Some(prompt) = self.prompt.take() {
                        self.on_prompt(prompt.kind, answer);
                    }
                }
            }
            return;
        }
        // The queue is edited with the playlist's keys, whichever view it was opened from.
        let context = match self.overlay {
            Overlay::Queue { .. } => Context::Playlist,
            _ => self.context(),
        };
        let Some(action) = self.keymap.lookup(context, &key) else {
            return;
        };
        let (scroll, is_log) = match self.overlay {
            Overlay::None => return self.on_action(action),
            Overlay::Themes { cursor, original } => {
                return self.on_theme_key(action, cursor, original);
            }
            Overlay::Queue { cursor } => return self.on_queue_key(action, cursor),
            Overlay::Help { scroll } => (scroll, false),
            // The log scrolls up from its newest line, so "up" means older.
            Overlay::Log { scroll } => (scroll, true),
        };
        let (back, forward) = if is_log {
            (Action::CursorDown, Action::CursorUp)
        } else {
            (Action::CursorUp, Action::CursorDown)
        };
        let scroll = match action {
            a if a == back => scroll.saturating_sub(1),
            a if a == forward => scroll.saturating_add(1),
            Action::PageUp if is_log => scroll.saturating_add(10),
            Action::PageDown if is_log => scroll.saturating_sub(10),
            Action::PageUp => scroll.saturating_sub(10),
            Action::PageDown => scroll.saturating_add(10),
            Action::Home | Action::End => 0,
            Action::Close
            | Action::Help
            | Action::ShowLog
            | Action::Quit
            | Action::Activate
            | Action::Back => {
                self.overlay = Overlay::None;
                return;
            }
            _ => return self.on_action(action),
        };
        self.overlay = if is_log {
            Overlay::Log { scroll }
        } else {
            Overlay::Help { scroll }
        };
    }

    pub fn on_action(&mut self, action: Action) {
        if self.view == View::FileManager && self.on_file_manager_action(action) {
            return;
        }
        if self.view == View::Visualizer
            && matches!(action, Action::Close | Action::Back | Action::Activate)
        {
            self.toggle_visualizer();
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
                self.banner(
                    BannerLevel::Info,
                    format!("Play mode: {}", self.play_mode.label()),
                );
                self.requeue();
            }
            Action::CycleRepeat => {
                self.repeat = self.repeat.cycle();
                self.banner(
                    BannerLevel::Info,
                    format!("Repeat: {}", self.repeat.label()),
                );
                self.requeue();
            }
            Action::ToggleGroupShuffle => self.toggle_group_shuffle(),
            Action::Enqueue => self.enqueue_at_cursor(),
            Action::ShowQueue => self.overlay = Overlay::Queue { cursor: 0 },
            Action::ClearQueue => self.clear_queue(),
            Action::FileManager => self.open_file_manager(FmMode::AddFiles),
            Action::NewGroup => {
                self.prompt = Some(Prompt::new(PromptKind::NewGroup, "New group name: ", ""));
            }
            Action::LoadPlaylist => self.open_file_manager(FmMode::PickPlaylist),
            Action::SavePlaylist => self.start_save(),
            Action::RenameGroup => self.start_rename(),
            Action::Delete => self.delete_at_cursor(),
            Action::MoveUp => self.move_at_cursor(-1),
            Action::MoveDown => self.move_at_cursor(1),
            Action::Search => {
                let current = self.filter.clone().unwrap_or_default();
                self.prompt = Some(Prompt::new(PromptKind::Search, "Search: ", current));
            }
            Action::Help => self.overlay = Overlay::Help { scroll: 0 },
            Action::ShowLog => self.overlay = Overlay::Log { scroll: 0 },
            Action::ThemeSelector => self.open_theme_selector(),
            Action::SpectrumFullscreen => self.toggle_visualizer(),
            Action::VisualizerStyle => {
                self.visualizer = self.visualizer.toggle();
                self.banner(
                    BannerLevel::Info,
                    format!("Visualizer: {}", self.visualizer.label()),
                );
            }
            Action::SpectrumLayout => {
                self.spectrum_layout = self.spectrum_layout.cycle();
                self.banner(
                    BannerLevel::Info,
                    format!("Spectrum: {}", self.spectrum_layout.label()),
                );
            }
            Action::Close => {
                if self.filter.take().is_none() {
                    self.banner = None;
                }
            }
            #[cfg(debug_assertions)]
            Action::DebugPanic => panic!("debug panic requested from the keyboard"),
            other => self.banner(
                BannerLevel::Info,
                format!("{}: not available yet", other.description()),
            ),
        }
    }

    fn on_prompt(&mut self, kind: PromptKind, answer: String) {
        match kind {
            PromptKind::SelectGlob => {
                if answer.is_empty() {
                    return;
                }
                match self.fm.select_matching(&answer) {
                    Ok(n) => {
                        self.banner(BannerLevel::Info, format!("{n} entries match \"{answer}\""))
                    }
                    Err(e) => self.banner(BannerLevel::Warning, format!("Bad pattern: {e}")),
                }
            }
            PromptKind::NewGroup => self.create_group(answer),
            PromptKind::RenameGroup => self.rename_group(answer),
            PromptKind::SavePlaylist => self.save_answer(answer),
            PromptKind::Search => {
                self.filter = Some(answer).filter(|s| !s.is_empty());
                self.cursor = 0;
            }
            PromptKind::Confirm(confirm) => self.confirmed(confirm),
        }
    }

    fn activate(&mut self) {
        match self.cursor_row() {
            Some(Row::Group(i)) => {
                self.cwd.push(i);
                self.cursor = 0;
                self.scroll = 0;
                self.filter = None;
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
            self.filter = None;
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
