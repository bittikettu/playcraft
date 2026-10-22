//! The terminal user interface (ratatui + crossterm).

pub mod app;
pub mod keymap;
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
        app.play(crate::model::TrackRef::new(vec![], 0));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !app.banner.as_ref().is_some_and(|b| b.text.contains("No playable tracks")) {
            app.process_audio_events();
            assert!(Instant::now() < deadline, "never halted; banner: {:?}", app.banner);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(app.root.tracks.iter().all(|t| t.missing));
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
