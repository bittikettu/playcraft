//! The terminal user interface (ratatui + crossterm).

pub mod app;
mod editing;
pub mod file_manager;
pub mod keymap;
mod playback;
mod playlist_io;
pub mod prompt;
pub mod theme;
mod views;

use std::time::{Duration, Instant};

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event};

use app::App;
use keymap::KeyBinding;

/// How often to check for audio events while nothing is animating.
const IDLE_POLL: Duration = Duration::from_millis(100);

/// Runs the TUI until the user quits. The terminal is restored on exit and on panic.
pub fn run(app: &mut App) -> anyhow::Result<()> {
    let mut terminal = ratatui::try_init()?;
    let result = event_loop(&mut terminal, app);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut DefaultTerminal, app: &mut App) -> anyhow::Result<()> {
    let frame_interval = Duration::from_secs_f64(1.0 / f64::from(app.fps.clamp(1, 240)));
    let mut dirty = true;
    let mut last_draw = Instant::now();
    while !app.quit {
        dirty |= app.process_audio_events();
        dirty |= app.process_background();
        dirty |= app.tick();
        let animating = app.is_animating();
        if dirty || (animating && last_draw.elapsed() >= frame_interval) {
            terminal.draw(|f| views::draw(f, app))?;
            last_draw = Instant::now();
            dirty = false;
        }

        let timeout = if animating { frame_interval.saturating_sub(last_draw.elapsed()) } else { IDLE_POLL };
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
        let backend = ManualBackend { format: DeviceFormat { sample_rate: 48_000, channels: 2 }, renderers: tx };
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
        for name in ["Led Zeppelin - Black Dog.mp3", "Deep Purple - Highway Star.flac", "Pink Floyd - Money.ogg"] {
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
        while !app.banner.as_ref().is_some_and(|b| b.text.contains("No playable")) {
            app.process_audio_events();
            assert!(Instant::now() < deadline, "never halted; banner: {:?}", app.banner);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(app.root.track_refs().iter().all(|r| app.root.track(r).unwrap().missing));
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
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join(name)
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
            assert!(Instant::now() < deadline, "scan did not finish: {:?}", app.root.tracks);
            std::thread::sleep(Duration::from_millis(5));
        }
        let names: Vec<_> = app.root.tracks.iter().map(|t| t.file_name()).collect();
        assert_eq!(names, vec!["01 tagged.flac", "02 sine.ogg", "single.mp3"]);
        assert_eq!(app.root.tracks[0].display_name(), "Test Artist - Test Title");

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
        assert!(worst < Duration::from_millis(100), "slowest frame took {worst:?}");
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
