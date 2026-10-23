//! The terminal user interface (ratatui + crossterm).

pub mod app;
mod backend;
mod editing;
pub mod file_manager;
pub mod keymap;
mod persistence;
mod playback;
mod playlist_io;
pub mod prompt;
mod queue;
pub mod theme;
mod theme_picker;
mod views;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::crossterm::event::{self, Event};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};

use crate::platform::FrameWriter;

use crate::session::{self, CloseSignal};
use app::App;
use keymap::KeyBinding;

/// How often to check for audio events while nothing is animating.
const IDLE_POLL: Duration = Duration::from_millis(100);
/// How often the session is saved (only when it changed).
const AUTOSAVE_INTERVAL: Duration = Duration::from_secs(30);

/// Saves the session periodically, on quit and when the console window is closed.
pub struct Autosave {
    path: PathBuf,
    last_json: String,
    last_check: Instant,
    close: Arc<CloseSignal>,
}

impl Autosave {
    pub fn new(path: PathBuf) -> Self {
        Autosave {
            path,
            last_json: String::new(),
            last_check: Instant::now(),
            close: CloseSignal::install(),
        }
    }

    pub fn save(&mut self, app: &App) {
        let json = app.snapshot().to_json();
        if json == self.last_json {
            return;
        }
        match session::save_json(&self.path, &json) {
            Ok(()) => self.last_json = json,
            Err(e) => log::warn!("cannot save session {}: {e}", self.path.display()),
        }
    }

    fn tick(&mut self, app: &mut App) {
        if self.close.requested() {
            self.save(app);
            self.close.complete();
            app.quit = true;
        } else if self.last_check.elapsed() >= AUTOSAVE_INTERVAL {
            self.last_check = Instant::now();
            self.save(app);
        }
    }
}

type Tui = Terminal<backend::FastBackend<FrameWriter>>;

/// Like `ratatui::try_init`, but every frame reaches the console in one write.
fn init_terminal() -> std::io::Result<Tui> {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        previous(info);
    }));
    enable_raw_mode()?;
    execute!(std::io::stdout(), EnterAlternateScreen)?;
    Terminal::new(backend::FastBackend::new(FrameWriter::stdout()))
}

/// Lowers the frame rate when the terminal cannot keep up. Drawing includes handing the
/// frame to the console, which blocks while the terminal is still busy with earlier ones.
struct FramePacer {
    /// Smoothed time a draw takes, in seconds.
    draw_secs: f64,
}

impl FramePacer {
    /// Drawing may use about half of each frame; the rest is left for input and audio.
    const BUDGET: f64 = 0.5;
    const MIN_FPS: u32 = 10;

    fn record(&mut self, took: Duration) {
        self.draw_secs = self.draw_secs * 0.85 + took.as_secs_f64() * 0.15;
    }

    /// The frame rate to use: `wanted`, or less if draws are slow.
    fn fps(&self, wanted: u32) -> u32 {
        if self.draw_secs <= 0.0 {
            return wanted;
        }
        let sustainable = (Self::BUDGET / self.draw_secs) as u32;
        wanted.min(sustainable.max(Self::MIN_FPS))
    }
}

/// Runs the TUI until the user quits. The terminal is restored on exit and on panic.
pub fn run(app: &mut App, mut autosave: Option<Autosave>) -> anyhow::Result<()> {
    let mut terminal = init_terminal()?;
    let result = event_loop(&mut terminal, app, autosave.as_mut());
    ratatui::restore();
    if let Some(autosave) = autosave.as_mut() {
        autosave.save(app);
    }
    result
}

fn event_loop(
    terminal: &mut Tui,
    app: &mut App,
    mut autosave: Option<&mut Autosave>,
) -> anyhow::Result<()> {
    let mut dirty = true;
    let mut last_draw = Instant::now();
    let mut pacer = FramePacer { draw_secs: 0.0 };
    while !app.quit {
        // The frame rate depends on what is on screen (the HYPERSPACE visualizer is capped)
        // and on how fast the terminal keeps up.
        let fps = pacer.fps(app.effective_fps()).clamp(1, 240);
        let frame_interval = Duration::from_secs_f64(1.0 / f64::from(fps));
        if let Some(autosave) = autosave.as_deref_mut() {
            autosave.tick(app);
        }
        app.update_spectrum();
        dirty |= app.process_audio_events();
        dirty |= app.process_background();
        dirty |= app.tick();
        let animating = app.is_animating();
        if dirty || (animating && last_draw.elapsed() >= frame_interval) {
            let started = Instant::now();
            terminal.draw(|f| views::draw(f, app))?;
            pacer.record(started.elapsed());
            last_draw = Instant::now();
            dirty = false;
        }

        let timeout = if animating {
            frame_interval.saturating_sub(last_draw.elapsed())
        } else {
            // Wake up exactly when scrolling text moves, so it scrolls evenly.
            app.marquee_wait()
                .map_or(IDLE_POLL, |wait| wait.min(IDLE_POLL))
        };
        if event::poll(timeout)? {
            // Handle everything that is queued before the next redraw.
            loop {
                match event::read()? {
                    Event::Key(key) => {
                        if let Some(binding) = KeyBinding::from_event(&key) {
                            app.on_key(binding);
                            dirty = true;
                        }
                    }
                    Event::Resize(..) => dirty = true,
                    _ => {}
                }
                if app.quit || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
    }
    app.audio.send(crate::audio::AudioCommand::Stop);
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::audio::controller::ManualBackend;
    use crate::audio::output::DeviceFormat;
    use crate::audio::{AudioConfig, AudioHandle};
    use crate::model::{Group, Track};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    pub(crate) fn test_app(root: Group) -> App {
        let (tx, rx) = crossbeam_channel::unbounded();
        let backend = ManualBackend {
            format: DeviceFormat {
                sample_rate: 48_000,
                channels: 2,
            },
            renderers: tx,
        };
        let audio = AudioHandle::spawn_with_backend(AudioConfig::default(), Box::new(backend));
        // Keep the renderer alive for the lifetime of the test.
        std::mem::forget(rx.recv().ok());
        App::new(audio, root)
    }

    pub(crate) fn render(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| views::draw(f, app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..height {
            for x in 0..width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn sample_root() -> Group {
        let mut root = Group::new(0, "Root");
        let mut prog = Group::new(1, "70s Prog Rock");
        prog.tracks.push(Track::new("yes.flac"));
        root.subgroups.push(prog);
        for name in [
            "Led Zeppelin - Black Dog.mp3",
            "Deep Purple - Highway Star.flac",
            "Pink Floyd - Money.ogg",
        ] {
            root.tracks.push(Track::new(name));
        }
        root.tracks[2].missing = true;
        root
    }

    #[test]
    fn renders_classic_layout() {
        let mut app = test_app(sample_root());
        app.cursor = 1;
        let screen = render(&mut app, 100, 24);
        println!("{screen}");
        assert!(screen.contains("[F1] Files"));
        assert!(screen.contains("Now Playing"));
        assert!(screen.contains("Playlist: Root (4 tracks)"));
        assert!(screen.contains("01. [GRP] 70s Prog Rock"));
        assert!(screen.contains(">02. Led Zeppelin - Black Dog.mp3"));
        assert!(screen.contains("[MISSING]"));
        assert!(screen.contains("00:00 / 00:00"));
        assert!(screen.contains("Status: [STOPPED]"));
        assert!(screen.contains("Vol: ["));
    }

    #[test]
    fn narrow_terminal_hides_now_playing() {
        let mut app = test_app(sample_root());
        let screen = render(&mut app, 60, 12);
        println!("{screen}");
        assert!(!screen.contains("Now Playing"));
        assert!(screen.contains("Led Zeppelin"));
    }

    #[test]
    fn help_overlay_lists_bindings() {
        let mut app = test_app(sample_root());
        app.on_action(keymap::Action::Help);
        let screen = render(&mut app, 100, 40);
        println!("{screen}");
        assert!(screen.contains("Play / pause"));
        assert!(screen.contains("Space, c, 5"));
    }

    #[test]
    fn all_missing_playlist_halts_after_trying_every_track() {
        let mut app = test_app(sample_root());
        // With repeat-all a playlist of missing files would otherwise cycle forever.
        app.repeat = crate::model::RepeatMode::RepeatAll;
        let first = app.root.tracks[0].uid;
        app.play(first, true);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !app
            .banner
            .as_ref()
            .is_some_and(|b| b.text.contains("No playable"))
        {
            app.process_audio_events();
            assert!(
                Instant::now() < deadline,
                "never halted; banner: {:?}",
                app.banner
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            app.root
                .track_refs()
                .iter()
                .all(|r| app.root.track(r).unwrap().missing)
        );
    }

    #[test]
    fn group_editing_round_trip() {
        let mut app = test_app(sample_root());
        // Create two groups in the root.
        type_keys(&mut app, &["g", "S", "y", "n", "t", "h", "Enter"]);
        type_keys(&mut app, &["F2", "J", "a", "z", "z", "Enter"]);
        let names: Vec<_> = app.root.subgroups.iter().map(|g| g.name.clone()).collect();
        assert_eq!(names, vec!["70s Prog Rock", "Synth", "Jazz"]);
        assert_eq!(app.cursor, 2, "cursor follows the new group");

        // Move "Jazz" up, rename it, then delete "Synth" after confirming.
        type_keys(&mut app, &["K"]);
        assert_eq!(app.root.subgroups[1].name, "Jazz");
        type_keys(&mut app, &["R", "Ctrl+u", "B", "e", "b", "o", "p", "Enter"]);
        assert_eq!(app.root.subgroups[1].name, "Bebop");
        app.cursor = 2;
        type_keys(&mut app, &["d", "n"]);
        assert_eq!(app.root.subgroups.len(), 3, "answering no keeps the group");
        type_keys(&mut app, &["d", "y"]);
        let names: Vec<_> = app.root.subgroups.iter().map(|g| g.name.clone()).collect();
        assert_eq!(names, vec!["70s Prog Rock", "Bebop"]);

        // Tracks: move down and delete.
        let first = app.root.tracks[0].uid;
        app.cursor = 2; // first track row (after two groups)
        type_keys(&mut app, &["J"]);
        assert_eq!(app.root.tracks[1].uid, first);
        type_keys(&mut app, &["d"]);
        assert!(app.root.tracks.iter().all(|t| t.uid != first));
        assert_eq!(app.root.tracks.len(), 2);

        // Group shuffle flag shows in the listing.
        app.cursor = 0;
        type_keys(&mut app, &["Enter", "z", "Backspace"]);
        assert!(app.root.subgroups[0].shuffle);
        assert!(render(&mut app, 100, 20).contains("~shuffle"));
    }

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    fn type_keys(app: &mut App, keys: &[&str]) {
        for k in keys {
            app.on_key(KeyBinding::parse(k).unwrap());
        }
    }

    #[test]
    fn file_manager_adds_files_and_folders_with_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let album = dir.path().join("Album");
        std::fs::create_dir(&album).unwrap();
        std::fs::copy(fixture("tagged.flac"), album.join("01 tagged.flac")).unwrap();
        std::fs::copy(fixture("sine_44k_stereo.ogg"), album.join("02 sine.ogg")).unwrap();
        std::fs::copy(fixture("tagged.mp3"), dir.path().join("single.mp3")).unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"x").unwrap();

        let mut app = test_app(Group::new(0, "Root"));
        app.fm = file_manager::FileManager::new(dir.path().to_path_buf());
        type_keys(&mut app, &["F1"]);
        assert_eq!(app.view, app::View::FileManager);
        let screen = render(&mut app, 100, 20);
        println!("{screen}");
        assert!(screen.contains("[DIR] Album"));
        assert!(screen.contains("single.mp3"));
        assert!(!screen.contains("notes.txt"));

        // Select everything with a pattern, then add: the folder is scanned in the background.
        type_keys(&mut app, &["*", "Ctrl+u", "*", "Enter", "a"]);
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.is_scanning() || app.root.tracks.iter().any(|t| t.metadata.is_none()) {
            app.process_background();
            assert!(
                Instant::now() < deadline,
                "scan did not finish: {:?}",
                app.root.tracks
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let names: Vec<_> = app.root.tracks.iter().map(|t| t.file_name()).collect();
        assert_eq!(names, vec!["01 tagged.flac", "02 sine.ogg", "single.mp3"]);
        assert_eq!(
            app.root.tracks[0].display_name(),
            "Test Artist - Test Title"
        );

        type_keys(&mut app, &["Esc"]);
        assert_eq!(app.view, app::View::Playlist);
        let screen = render(&mut app, 100, 20);
        println!("{screen}");
        assert!(screen.contains("Test Artist - Test Title"));
        assert!(screen.contains("[00:02]"));
    }

    #[test]
    fn ten_thousand_tracks_stay_responsive() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..10_000 {
            std::fs::write(dir.path().join(format!("{i:05}.mp3")), b"").unwrap();
        }
        let mut app = test_app(Group::new(0, "Root"));
        app.fm = file_manager::FileManager::new(dir.path().parent().unwrap().to_path_buf());
        app.fm.selected.insert(dir.path().to_path_buf());
        app.fm.entries.retain(|e| e.path == dir.path());
        app.view = app::View::FileManager;
        app.on_action(keymap::Action::AddSelected);
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut worst = Duration::ZERO;
        while app.is_scanning() {
            let t = Instant::now();
            app.process_background();
            render(&mut app, 120, 40);
            worst = worst.max(t.elapsed());
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(app.root.tracks.len(), 10_000);
        // A frame (background work + full redraw) must stay well under 100 ms even in debug builds.
        assert!(
            worst < Duration::from_millis(100),
            "slowest frame took {worst:?}"
        );
    }

    #[test]
    fn save_and_load_playlist_through_the_ui() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = test_app(sample_root());
        app.fm = file_manager::FileManager::new(dir.path().to_path_buf());
        // F4, type a relative name without extension: saved as .blst in the browsed folder.
        type_keys(&mut app, &["F4", "Ctrl+u", "m", "i", "x", "Enter"]);
        let saved = dir.path().join("mix.blst");
        assert!(saved.exists(), "banner: {:?}", app.banner);

        // Saving again under a different existing name asks first.
        std::fs::write(dir.path().join("other.m3u8"), "#EXTM3U\n").unwrap();
        type_keys(
            &mut app,
            &[
                "F4", "Ctrl+u", "o", "t", "h", "e", "r", ".", "m", "3", "u", "8", "Enter",
            ],
        );
        assert!(app.prompt.as_ref().is_some_and(|p| p.is_confirm()));
        type_keys(&mut app, &["y"]);
        assert!(
            std::fs::read_to_string(dir.path().join("other.m3u8"))
                .unwrap()
                .contains("#EXTGRP:70s Prog Rock")
        );

        // Load replaces the playlist; import adds it as a group.
        let mut app = test_app(Group::new(0, "Root"));
        app.load_playlist(&saved);
        assert_eq!(app.root.total_tracks(), 4);
        assert_eq!(app.root.subgroups[0].name, "70s Prog Rock");
        app.import_playlist(&dir.path().join("other.m3u8"));
        assert_eq!(app.root.subgroups.len(), 2);
        assert_eq!(app.root.subgroups[1].name, "other");
        assert_eq!(app.root.total_tracks(), 8);
    }

    #[test]
    fn search_filters_live_and_escape_clears() {
        let mut app = test_app(sample_root());
        type_keys(&mut app, &["/", "p", "u", "r"]);
        assert_eq!(app.filter.as_deref(), Some("pur"));
        let screen = render(&mut app, 100, 20);
        assert!(screen.contains("Deep Purple"));
        assert!(!screen.contains("Led Zeppelin"));
        assert!(screen.contains("[filter: pur]"));
        type_keys(&mut app, &["Enter"]);
        assert!(app.prompt.is_none());
        assert_eq!(app.rows().len(), 1);
        type_keys(&mut app, &["Esc"]);
        assert!(app.filter.is_none());
        assert_eq!(app.rows().len(), 4);
        // Escape while typing cancels the filter too.
        type_keys(&mut app, &["/", "z", "Esc"]);
        assert!(app.filter.is_none());
    }

    #[test]
    fn config_applies_theme_charset_and_keys() {
        let (cfg, warnings) = crate::config::parse(
            "[ui]\ncharset = \"ascii\"\ncolor_theme = \"monochrome\"\n[keybindings]\nhelp = \"F10\"\n",
        );
        assert!(warnings.is_empty());
        let mut app = test_app(sample_root());
        assert!(app.apply_config(&cfg).is_empty());
        assert_eq!(app.theme.name, "monochrome");
        type_keys(&mut app, &["F10"]);
        assert!(matches!(app.overlay, app::Overlay::Help { .. }));
        type_keys(&mut app, &["Esc"]);
        let screen = render(&mut app, 100, 20);
        println!("{screen}");
        assert!(
            screen.is_ascii(),
            "ascii charset must not draw box characters"
        );
        assert!(screen.contains("[F10] Help"));
    }

    #[test]
    fn session_round_trip_restores_playlist_and_resumes_paused() {
        let mut root = sample_root();
        root.subgroups[0].tracks[0].path = fixture("sine_48k_stereo.flac");
        root.subgroups[0].shuffle = true;
        let mut app = test_app(root);
        let uid = app.root.subgroups[0].tracks[0].uid;
        app.last_played = Some(uid);
        app.cwd = vec![0];
        app.repeat = crate::model::RepeatMode::RepeatAll;
        app.audio.shared().set_volume(0.35);
        let json = app.snapshot().to_json();

        let session: crate::session::Session = serde_json::from_str(&json).unwrap();
        let mut restored = test_app(Group::new(0, "Root"));
        restored.restore(session, false, true, true);
        assert_eq!(restored.root.total_tracks(), 4);
        assert!(restored.root.subgroups[0].shuffle);
        assert_eq!(restored.cwd, vec![0]);
        assert_eq!(restored.repeat, crate::model::RepeatMode::RepeatAll);
        assert!((restored.audio.shared().volume() - 0.35).abs() < 1e-6);
        let last = restored
            .last_played
            .and_then(|u| restored.root.track_by_uid(u))
            .unwrap();
        assert_eq!(last.path, fixture("sine_48k_stereo.flac"));

        // The resumed track starts paused.
        let deadline = Instant::now() + Duration::from_secs(5);
        while restored.status() != crate::model::PlaybackStatus::Paused
            || restored.playing.is_none()
        {
            restored.process_audio_events();
            assert!(Instant::now() < deadline, "status {:?}", restored.status());
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(restored.playing, restored.last_played);
    }

    #[test]
    fn queue_shows_in_the_playlist_and_is_edited_in_its_list() {
        let mut app = test_app(sample_root());
        let (black_dog, highway) = (app.root.tracks[0].uid, app.root.tracks[1].uid);
        // Rows: the group, Black Dog, Highway Star, Money (missing).
        app.cursor = 2;
        type_keys(&mut app, &["e", "Up", "Insert", "Down", "Down", "e"]);
        assert_eq!(
            app.queue,
            [highway, black_dog],
            "the missing track is not queued"
        );
        app.pending_next = Some(highway);
        let screen = render(&mut app, 100, 24);
        println!("{screen}");
        assert!(screen.contains("Q2 "), "queue position next to Black Dog");
        assert!(screen.contains("Q1 "));
        assert!(screen.contains("Next:   Deep Purple"));

        type_keys(&mut app, &["u"]);
        let screen = render(&mut app, 100, 24);
        println!("{screen}");
        assert!(screen.contains("Play queue (2)"));
        assert!(screen.contains(" 1. Deep Purple - Highway Star"));
        assert!(screen.contains("continues in normal order"));

        // Reorder, remove, and close; playlist keys work in the list.
        type_keys(&mut app, &["J"]);
        assert_eq!(app.queue, [black_dog, highway]);
        assert_eq!(app.overlay, app::Overlay::Queue { cursor: 1 });
        type_keys(&mut app, &["d"]);
        assert_eq!(app.queue, [black_dog]);
        type_keys(&mut app, &["X"]);
        assert!(app.queue.is_empty());
        let screen = render(&mut app, 100, 24);
        assert!(screen.contains("The queue is empty"));
        type_keys(&mut app, &["Esc"]);
        assert_eq!(app.overlay, app::Overlay::None);
    }

    #[test]
    fn now_playing_is_a_third_wide_and_scrolls_long_text() {
        let mut app = test_app(sample_root());
        let name = "Godspeed You! Black Emperor - Storm (Lift Your Skinny Fists).flac";
        app.now = Some(app::NowPlaying {
            track: None,
            path: name.into(),
            info: crate::audio::StreamInfo {
                codec: "flac".into(),
                sample_rate: 44_100,
                channels: 2,
            },
            duration: None,
        });
        let screen = render(&mut app, 120, 30);
        println!("{screen}");
        // 40 of 120 columns: the pane's border starts at column 80.
        let top = screen.lines().next().unwrap();
        assert_eq!(top.chars().position(|c| c == '┐'), Some(79));
        // Held still at first, cut off with an ellipsis.
        let room = 40 - 2 - "Title:  ".len();
        assert!(screen.contains(&format!("Title:  {}", &name[..room - 1])));
        assert!(app.marquee_scrolling);

        // After the pause it moves a column every step, and redraws on its own while stopped.
        app.marquee_since = Instant::now()
            .checked_sub(Duration::from_millis(2900))
            .unwrap();
        assert_eq!(app.marquee_step(), 5);
        assert!(app.tick(), "scrolling text needs a redraw");
        assert!(!app.tick(), "but only once per step");
        let screen = render(&mut app, 120, 30);
        let moved = views::marquee(name, room, 5, app.symbols.marquee_gap, "…");
        assert!(moved.starts_with("eed You!"), "{moved:?}");
        assert!(screen.contains(&format!("Title:  {moved}")));
        assert!(app.marquee_wait().unwrap() <= Duration::from_millis(200));

        // Short text stands still.
        app.now.as_mut().unwrap().path = "Money.ogg".into();
        render(&mut app, 120, 30);
        assert!(!app.marquee_scrolling);
        assert_eq!(app.marquee_wait(), None);
    }

    #[test]
    fn the_queue_survives_a_restart() {
        let mut app = test_app(sample_root());
        let (black_dog, yes) = (app.root.tracks[0].uid, app.root.subgroups[0].tracks[0].uid);
        app.queue = vec![yes, black_dog];
        let json = app.snapshot().to_json();
        let session: crate::session::Session = serde_json::from_str(&json).unwrap();
        let mut restored = test_app(Group::new(0, "Root"));
        restored.restore(session, false, false, false);
        let paths: Vec<_> = restored
            .queue
            .iter()
            .map(|&u| restored.root.track_by_uid(u).unwrap().path.clone())
            .collect();
        assert_eq!(
            paths,
            [
                std::path::PathBuf::from("yes.flac"),
                "Led Zeppelin - Black Dog.mp3".into()
            ]
        );
    }

    #[test]
    fn spectrum_bars_and_log_overlay_render() {
        let mut app = test_app(sample_root());
        let tone: Vec<f32> = (0..4096)
            .map(|i| 0.5 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 48_000.0).sin())
            .collect();
        app.spectrum.push_samples(&tone);
        app.spectrum.update(Duration::ZERO);
        let screen = render(&mut app, 100, 30);
        println!("{screen}");
        assert!(screen.contains('█'), "no full spectrum bar drawn");
        assert!(
            app.is_animating(),
            "falling bars keep the display animating"
        );

        // The global logger may not be installed yet when this test runs first.
        crate::log_ring::init(log::LevelFilter::Debug, None);
        log::warn!("spectrum test warning");
        type_keys(&mut app, &["L"]);
        // Tall, because tests running in parallel log into the same ring meanwhile.
        let screen = render(&mut app, 100, 300);
        assert!(screen.contains("Message log"));
        assert!(screen.contains("spectrum test warning"));
        type_keys(&mut app, &["Esc"]);
        assert_eq!(app.overlay, app::Overlay::None);
    }

    fn feed_tone(app: &mut App, hz: f32) {
        let tone: Vec<f32> = (0..8192)
            .map(|i| 0.5 * (2.0 * std::f32::consts::PI * hz * i as f32 / 48_000.0).sin())
            .collect();
        app.spectrum.push_samples(&tone);
        app.spectrum.update(Duration::ZERO);
    }

    #[test]
    fn wide_spectrum_strip_spans_the_screen() {
        let mut app = test_app(sample_root());
        render(&mut app, 120, 34); // sets the band count for this width
        feed_tone(&mut app, 1000.0);
        let screen = render(&mut app, 120, 34);
        println!("{screen}");
        assert!(screen.contains("Spectrum"));
        assert!(screen.contains("f: fullscreen"));
        assert!(
            app.spectrum.band_count() >= 36,
            "{} bands",
            app.spectrum.band_count()
        );
        let axis = screen
            .lines()
            .find(|l| l.contains("1k"))
            .expect("frequency axis");
        assert!(axis.contains("100") && axis.contains("10k"), "{axis}");
        // Wide bars: two block characters side by side.
        assert!(screen.contains("██"));

        // On a short terminal the spectrum falls back into the Now Playing pane.
        render(&mut app, 100, 20);
        assert_eq!(app.spectrum.band_count(), views::spectrum_pane_bands());
    }

    #[test]
    fn fullscreen_visualizer_toggles_and_keeps_playback_keys() {
        let mut app = test_app(sample_root());
        type_keys(&mut app, &["f"]);
        assert_eq!(app.view, app::View::Visualizer);
        render(&mut app, 100, 30);
        feed_tone(&mut app, 300.0);
        let screen = render(&mut app, 100, 30);
        println!("{screen}");
        assert!(screen.contains("Nothing playing"));
        assert!(screen.contains("f/Esc: back"));
        assert!(
            !screen.contains("Playlist: Root"),
            "playlist must be hidden"
        );
        assert!(screen.contains("f:Back"));
        // Volume works in the visualizer; Esc returns to the playlist.
        let before = app.audio.shared().volume();
        type_keys(&mut app, &["+"]);
        assert!(app.audio.shared().volume() > before);
        type_keys(&mut app, &["Esc"]);
        assert_eq!(app.view, app::View::Playlist);

        // From the file manager, f and back again returns to the file manager.
        type_keys(&mut app, &["F1", "f"]);
        assert_eq!(app.view, app::View::Visualizer);
        type_keys(&mut app, &["f"]);
        assert_eq!(app.view, app::View::FileManager);
    }

    #[test]
    fn matrix_rain_shows_the_files_bytes() {
        let path = fixture("tagged.mp3");
        let mut app = test_app(sample_root());
        app.now = Some(app::NowPlaying {
            track: None,
            path: path.clone(),
            info: crate::audio::StreamInfo {
                codec: "mp3".into(),
                sample_rate: 44_100,
                channels: 2,
            },
            duration: Some(Duration::from_secs(2)),
        });
        type_keys(&mut app, &["v"]);
        assert_eq!(app.visualizer, app::Visualizer::Matrix);
        assert!(
            app.banner
                .as_ref()
                .is_some_and(|b| b.text.contains("Matrix"))
        );
        type_keys(&mut app, &["f"]);
        app.rain.feed.set_source(Some(&path), 0.0);
        render(&mut app, 100, 30); // sizes the rain to the screen
        for _ in 0..90 {
            feed_tone(&mut app, 1000.0);
            let bands = app.spectrum.bands().to_vec();
            app.rain.update(1.0 / 60.0, &bands);
        }
        let screen = render(&mut app, 100, 30);
        println!("{screen}");
        assert!(
            screen.contains("bytes 0x"),
            "byte offset missing from the title"
        );
        // Inside the visualizer box every visible character is a hex digit.
        let inner: String = screen
            .lines()
            .skip(1)
            .take(22)
            .flat_map(|l| l.chars().skip(1).take(98))
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(inner.len() > 50, "rain too sparse: {inner}");
        assert!(inner.chars().all(|c| c.is_ascii_hexdigit()), "{inner}");
        assert!(app.rain.is_active());
        // The first bytes of this file are its ID3 tag.
        let first = std::fs::read(&path).unwrap()[0];
        assert_eq!(
            app.rain.glyph(0, 0),
            format!("{first:02X}").chars().next().unwrap()
        );

        type_keys(&mut app, &["v"]);
        assert_eq!(app.visualizer, app::Visualizer::Defrag);
    }

    #[test]
    fn defrag_visualizer_finishes_with_the_song() {
        let mut app = test_app(sample_root());
        app.now = Some(app::NowPlaying {
            track: None,
            path: std::path::PathBuf::from(r"D:\Music\Pink Floyd - Money.mp3"),
            info: crate::audio::StreamInfo {
                codec: "mp3".into(),
                sample_rate: 44_100,
                channels: 2,
            },
            duration: Some(Duration::from_secs(100)),
        });
        type_keys(&mut app, &["v", "v", "f"]);
        assert_eq!(app.visualizer, app::Visualizer::Defrag);
        let shared = app.audio.shared().clone();

        shared.set_position(Duration::ZERO);
        render(&mut app, 100, 30);
        let screen = render(&mut app, 100, 30);
        println!("{screen}");
        assert!(screen.contains("Defragmenting Drive"), "title missing");
        assert!(screen.contains("0% complete"));
        assert!(
            screen.contains("Fragmented") && screen.contains("Free space"),
            "legend missing"
        );
        assert!(!app.defrag.is_complete());

        shared.set_position(Duration::from_secs(50));
        render(&mut app, 100, 30);
        assert_eq!(app.defrag.percent(), 50);
        assert!(render(&mut app, 100, 30).contains("50% complete"));

        shared.set_position(Duration::from_secs(100));
        render(&mut app, 100, 30);
        let screen = render(&mut app, 100, 30);
        println!("{screen}");
        assert!(app.defrag.is_complete());
        assert!(screen.contains("is complete"));

        // The wide strip shows the same defragmenter.
        type_keys(&mut app, &["f"]);
        assert!(render(&mut app, 120, 34).contains("complete"));
        // v moves on to the next visualizer.
        type_keys(&mut app, &["v"]);
        assert_eq!(app.visualizer, app::Visualizer::Hyperspace);
    }

    #[test]
    fn hyperspace_fills_the_screen_with_pixels() {
        let mut app = test_app(sample_root());
        app.now = Some(app::NowPlaying {
            track: None,
            path: std::path::PathBuf::from("Daft Punk - One More Time.flac"),
            info: crate::audio::StreamInfo {
                codec: "flac".into(),
                sample_rate: 44_100,
                channels: 2,
            },
            duration: Some(Duration::from_secs(320)),
        });
        type_keys(&mut app, &["v", "v", "v"]);
        assert_eq!(app.visualizer, app::Visualizer::Hyperspace);
        assert_eq!(app.effective_fps(), 30, "wide strip shows it: capped");
        type_keys(&mut app, &["f"]);
        render(&mut app, 100, 30);
        for _ in 0..20 {
            feed_tone(&mut app, 120.0);
            let bands = app.spectrum.bands().to_vec();
            let wave = app.spectrum.waveform(256);
            app.hyperspace.update(
                1.0 / 30.0,
                &bands,
                &wave,
                Some((9, "Daft Punk - One More Time")),
                true,
            );
            render(&mut app, 100, 30);
        }
        let screen = render(&mut app, 100, 30);
        println!("{screen}");
        assert!(screen.contains("HYPERSPACE"));
        let half_blocks = screen.chars().filter(|&c| c == '▀').count();
        assert!(half_blocks > 200, "only {half_blocks} half-block pixels");
        assert!(app.is_animating());

        // ASCII charset: one pixel per cell, no half blocks.
        app.symbols = theme::Symbols::ASCII;
        let screen = render(&mut app, 100, 30);
        assert_eq!(screen.chars().filter(|&c| c == '▀').count(), 0);
        type_keys(&mut app, &["v"]);
        assert_eq!(app.visualizer, app::Visualizer::Bars);
        assert_eq!(app.effective_fps(), app.fps);
    }

    #[test]
    fn frame_pacer_slows_down_for_a_slow_terminal() {
        let mut pacer = FramePacer { draw_secs: 0.0 };
        assert_eq!(pacer.fps(30), 30);
        for _ in 0..50 {
            pacer.record(Duration::from_millis(2));
        }
        assert_eq!(pacer.fps(30), 30, "fast draws keep the full rate");
        for _ in 0..50 {
            pacer.record(Duration::from_millis(40));
        }
        assert_eq!(pacer.fps(30), 12, "40 ms draws: about half of each frame");
        for _ in 0..50 {
            pacer.record(Duration::from_millis(500));
        }
        assert_eq!(pacer.fps(30), FramePacer::MIN_FPS);
    }

    /// Counts what a terminal backend would send to the console.
    #[derive(Default, Clone)]
    struct CountingWriter {
        bytes: std::rc::Rc<std::cell::Cell<usize>>,
        writes: std::rc::Rc<std::cell::Cell<usize>>,
        colours: std::rc::Rc<std::cell::Cell<usize>>,
    }

    impl std::io::Write for CountingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.bytes.set(self.bytes.get() + buf.len());
            self.writes.set(self.writes.get() + 1);
            // 24-bit colours sent (foreground or background).
            let n = buf
                .windows(5)
                .filter(|w| *w == b"38;2;" || *w == b"48;2;")
                .count();
            self.colours.set(self.colours.get() + n);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Terminal output per HYPERSPACE frame: the old path (ratatui's crossterm backend, no
    /// tuning, stdout's 1 KiB line buffer) against what the app uses now.
    /// `cargo test --release --lib hyperspace_terminal_load -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn hyperspace_terminal_load() {
        use crate::hyperspace::Tuning;
        let preset = |name| Tuning::preset(name).unwrap();
        let untuned = Tuning {
            hysteresis: 0,
            quant_shift: 2,
            ..preset("balanced")
        };
        let rows = [
            ("old path", untuned, false),
            ("new, high", preset("high"), true),
            ("new, balanced", preset("balanced"), true),
            ("new, fast", preset("fast"), true),
        ];
        for (w, h) in [(120u16, 34u16), (200, 55)] {
            for (name, tuning, fast) in rows {
                let (bytes, writes, colours) = terminal_load(tuning, w, h, fast);
                println!(
                    "{w}x{h} {name:<14} {bytes:>7} B/frame {colours:>5} colours sent {writes:>4} writes  ~{:.2} MB/s @30fps",
                    (bytes * 30) as f64 / 1e6
                );
            }
        }
    }

    /// Busy synthetic music for frame `i`: a kick every half second at 30 fps.
    fn busy_frame(app: &mut App, i: usize) {
        let kick = if i.is_multiple_of(15) { 0.4 } else { 0.0 };
        let bands: Vec<f32> = (0..48)
            .map(|k| {
                let wobble = 0.25 * (i as f32 * 0.37 + k as f32 * 0.7).sin();
                (0.55 + wobble + if k < 8 { kick } else { 0.0 }).clamp(0.0, 1.0)
            })
            .collect();
        let wave: Vec<f32> = (0..256)
            .map(|n| (n as f32 * 0.13 + i as f32 * 0.9).sin() * 0.6)
            .collect();
        app.hyperspace.update(
            1.0 / 30.0,
            &bands,
            &wave,
            Some((1, "Benchmark Artist - Benchmark")),
            true,
        );
    }

    /// Steady-state terminal output per frame: (bytes, console writes, colours sent).
    /// `fast` selects the app's backend and frame-sized writes; otherwise ratatui's crossterm
    /// backend behind a 1 KiB line buffer like `std::io::stdout()`.
    fn terminal_load(
        tuning: crate::hyperspace::Tuning,
        w: u16,
        h: u16,
        fast: bool,
    ) -> (usize, usize, usize) {
        use ratatui::backend::{Backend, CrosstermBackend};
        use ratatui::layout::Rect;
        use ratatui::{TerminalOptions, Viewport};

        fn run<B: Backend>(
            terminal: &mut Terminal<B>,
            app: &mut App,
            counter: &CountingWriter,
            frames: usize,
        ) -> (usize, usize, usize) {
            let mut start = (0, 0, 0);
            for i in 0..(frames + 60) {
                busy_frame(app, i);
                terminal.draw(|f| views::draw(f, app)).ok();
                if i == 59 {
                    // Skip the opening hyperspace jump: measure the steady state.
                    start = (
                        counter.bytes.get(),
                        counter.writes.get(),
                        counter.colours.get(),
                    );
                }
            }
            (
                (counter.bytes.get() - start.0) / frames,
                (counter.writes.get() - start.1) / frames,
                (counter.colours.get() - start.2) / frames,
            )
        }

        ratatui::crossterm::style::Colored::set_ansi_color_disabled(false);
        let mut app = test_app(sample_root());
        app.visualizer = app::Visualizer::Hyperspace;
        app.view = app::View::Visualizer;
        app.hyperspace.tuning = tuning;
        let options = TerminalOptions {
            viewport: Viewport::Fixed(Rect::new(0, 0, w, h)),
        };
        let counter = CountingWriter::default();
        if fast {
            let writer = std::io::BufWriter::with_capacity(1 << 20, counter.clone());
            let mut terminal =
                Terminal::with_options(backend::FastBackend::new(writer), options).unwrap();
            run(&mut terminal, &mut app, &counter, 150)
        } else {
            let writer = std::io::LineWriter::with_capacity(1024, counter.clone());
            let mut terminal =
                Terminal::with_options(CrosstermBackend::new(writer), options).unwrap();
            run(&mut terminal, &mut app, &counter, 150)
        }
    }

    /// `cargo test --release --lib hyperspace_frame_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn hyperspace_frame_cost() {
        let mut app = test_app(sample_root());
        app.visualizer = app::Visualizer::Hyperspace;
        app.view = app::View::Visualizer;
        let wave: Vec<f32> = (0..256).map(|i| (i as f32 * 0.1).sin()).collect();
        render(&mut app, 200, 60);
        let start = Instant::now();
        for i in 0..300 {
            let level = if i % 15 == 0 { 0.95 } else { 0.6 };
            app.hyperspace.update(
                1.0 / 30.0,
                &[level; 48],
                &wave,
                Some((1, "Benchmark")),
                true,
            );
            render(&mut app, 200, 60);
        }
        let per_frame = start.elapsed() / 300;
        println!("200x60 cells (200x120 pixels): {per_frame:?} per frame");
        assert!(per_frame < Duration::from_millis(30));

        // Terminal load in steady state: cells that change between consecutive frames.
        let mut previous = render(&mut app, 120, 34);
        let mut changed = 0usize;
        for i in 0..90 {
            let level = if i % 15 == 0 { 0.95 } else { 0.6 };
            app.hyperspace.update(
                1.0 / 30.0,
                &[level; 48],
                &wave,
                Some((1, "Benchmark")),
                true,
            );
            let screen = render(&mut app, 120, 34);
            changed += screen
                .chars()
                .zip(previous.chars())
                .filter(|(a, b)| a != b)
                .count();
            previous = screen;
        }
        println!(
            "120x34: ~{} glyph changes per frame (colour-only changes not counted)",
            changed / 90
        );
    }

    #[test]
    fn spectrum_layout_cycles_wide_pane_off() {
        let mut app = test_app(sample_root());
        assert_eq!(app.spectrum_layout, app::SpectrumLayout::Wide);
        type_keys(&mut app, &["w"]);
        assert_eq!(app.spectrum_layout, app::SpectrumLayout::Pane);
        assert!(!render(&mut app, 120, 34).contains(" Spectrum "));
        type_keys(&mut app, &["w"]);
        assert_eq!(app.spectrum_layout, app::SpectrumLayout::Off);
        assert!(!app.spectrum_visible());
        type_keys(&mut app, &["w"]);
        assert_eq!(app.spectrum_layout, app::SpectrumLayout::Wide);
    }

    #[test]
    fn theme_picker_previews_reverts_and_saves() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(&config, "# keep me\n[ui]\ncolor_theme = \"dracula\"\n").unwrap();
        let mut app = test_app(sample_root());
        app.config_path = Some(config.clone());
        app.theme = theme::Theme::dracula();

        type_keys(&mut app, &["t"]);
        let screen = render(&mut app, 100, 30);
        println!("{screen}");
        assert!(screen.contains("Colour theme"));
        assert!(screen.contains("Dracula") && screen.contains("Tokyo Night"));

        // Moving previews immediately; Esc restores the original theme.
        type_keys(&mut app, &["Down"]);
        assert_eq!(app.theme.name, "tokyo_night");
        type_keys(&mut app, &["Esc"]);
        assert_eq!(app.theme.name, "dracula");
        assert_eq!(app.overlay, app::Overlay::None);

        // Up wraps around to the last theme; Enter keeps it and saves it to the config.
        type_keys(&mut app, &["t", "Up", "Up", "Enter"]);
        assert_eq!(app.theme.name, "classic_blue");
        let text = std::fs::read_to_string(&config).unwrap();
        assert!(text.contains("color_theme = \"classic_blue\""), "{text}");
        assert!(text.contains("# keep me"));
        assert!(
            app.banner
                .as_ref()
                .is_some_and(|b| b.text.contains("saved"))
        );
    }

    #[test]
    fn navigating_into_a_group_and_back() {
        let mut app = test_app(sample_root());
        app.on_action(keymap::Action::Activate);
        assert_eq!(app.cwd, vec![0]);
        assert!(render(&mut app, 100, 20).contains("Root › 70s Prog Rock"));
        app.on_action(keymap::Action::Back);
        assert!(app.cwd.is_empty());
        assert_eq!(app.cursor, 0);
    }
}
