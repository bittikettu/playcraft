//! The terminal user interface (ratatui + crossterm).

pub mod app;
mod editing;
pub mod file_manager;
pub mod keymap;
mod persistence;
mod playback;
mod playlist_io;
pub mod prompt;
pub mod theme;
mod theme_picker;
mod views;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event};

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

/// Runs the TUI until the user quits. The terminal is restored on exit and on panic.
pub fn run(app: &mut App, mut autosave: Option<Autosave>) -> anyhow::Result<()> {
    let mut terminal = ratatui::try_init()?;
    let result = event_loop(&mut terminal, app, autosave.as_mut());
    ratatui::restore();
    if let Some(autosave) = autosave.as_mut() {
        autosave.save(app);
    }
    result
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    mut autosave: Option<&mut Autosave>,
) -> anyhow::Result<()> {
    let frame_interval = Duration::from_secs_f64(1.0 / f64::from(app.fps.clamp(1, 240)));
    let mut dirty = true;
    let mut last_draw = Instant::now();
    while !app.quit {
        if let Some(autosave) = autosave.as_deref_mut() {
            autosave.tick(app);
        }
        app.update_spectrum();
        dirty |= app.process_audio_events();
        dirty |= app.process_background();
        dirty |= app.tick();
        let animating = app.is_animating();
        if dirty || (animating && last_draw.elapsed() >= frame_interval) {
            terminal.draw(|f| views::draw(f, app))?;
            last_draw = Instant::now();
            dirty = false;
        }

        let timeout = if animating {
            frame_interval.saturating_sub(last_draw.elapsed())
        } else {
            IDLE_POLL
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
        let screen = render(&mut app, 100, 30);
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
        assert_eq!(app.visualizer, app::Visualizer::Bars);
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
