use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use super::format_time;
use crate::model::PlaybackStatus;
use crate::progress_fx::ProgressStyle;
use crate::ui::app::App;

/// The ghost turns blue (Pac-Man is about to eat the power pellet) this close to the end.
const FRIGHTENED: Duration = Duration::from_secs(10);

pub fn draw(f: &mut Frame<'_>, app: &App, area: Rect, compact: bool) {
    let theme = &app.theme;
    let inner = if compact {
        area
    } else {
        let block = Block::bordered()
            .border_set(app.symbols.border)
            .border_style(theme.border)
            .style(theme.base);
        let inner = block.inner(area);
        f.render_widget(block, area);
        inner
    };
    let shared = app.audio.shared();
    let position = shared.position();
    let duration = app
        .now
        .as_ref()
        .and_then(|n| n.duration)
        .unwrap_or(shared.duration());
    let time = format!(" {} / {}", format_time(position), format_time(duration));

    let width = inner.width as usize;
    let bar_width = width.saturating_sub(time.len() + 2);
    let ratio = if duration.is_zero() || app.now.is_none() {
        0.0
    } else {
        (position.as_secs_f64() / duration.as_secs_f64()).clamp(0.0, 1.0)
    };
    let bar = Bar {
        app,
        width: bar_width,
        ratio,
        // The cell of the moving character.
        head: (ratio * bar_width.saturating_sub(1) as f64).round() as usize,
        playing: app.status() == PlaybackStatus::Playing,
        left: duration.saturating_sub(position),
    };
    let mut spans = vec![Span::styled("[", theme.border)];
    spans.extend(match app.progress_style {
        ProgressStyle::PacMan if bar_width >= 4 => bar.pacman(),
        ProgressStyle::Nyan if bar_width >= 8 => bar.nyan(),
        ProgressStyle::Waveform if bar_width >= 2 => bar.waveform(),
        _ => bar.classic(),
    });
    spans.push(Span::styled("]", theme.border));
    spans.push(Span::styled(time, theme.title));
    f.render_widget(Paragraph::new(Line::from(spans)), inner);
}

/// The bar between the brackets, `width` cells.
struct Bar<'a> {
    app: &'a App,
    width: usize,
    ratio: f64,
    head: usize,
    playing: bool,
    left: Duration,
}

/// A cheap hash for twinkling stars: the same cell and moment always look the same.
fn twinkle(cell: usize, moment: u64) -> u64 {
    let mut x = (cell as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ moment;
    x ^= x >> 31;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^ (x >> 29)
}

impl Bar<'_> {
    fn unicode(&self) -> bool {
        self.app.symbols.unicode
    }

    fn classic(&self) -> Vec<Span<'static>> {
        let theme = &self.app.theme;
        let s = &self.app.symbols;
        let done = (self.ratio * self.width as f64).round() as usize;
        let (done_str, head) = if done == 0 {
            (String::new(), String::new())
        } else if done >= self.width {
            (s.progress_done.repeat(self.width), String::new())
        } else {
            (
                s.progress_done.repeat(done - 1),
                s.progress_head.to_string(),
            )
        };
        let todo = self.width - done.min(self.width);
        vec![
            Span::styled(done_str, theme.progress_done),
            Span::styled(head, theme.progress_done),
            Span::styled(s.progress_todo.repeat(todo), theme.progress_todo),
        ]
    }

    /// Pac-Man chomps his way to the power pellet at the end of the song, eating the
    /// pellets on the way, with a ghost on his heels. He chomps faster when the music is
    /// louder, and the pellets light up on the beat.
    fn pacman(&self) -> Vec<Span<'static>> {
        let theme = &self.app.theme;
        let fx = &self.app.progress_fx;
        let uni = self.unicode();
        let open = self.playing && fx.phase.fract() < 0.5;
        let pac = match (uni, open) {
            (true, true) => "ᗧ",
            (true, false) => "●",
            (false, true) => "C",
            (false, false) => "c",
        };
        // The ghost floats three or four cells behind.
        let wobble = usize::from((fx.phase * 0.5).fract() < 0.5);
        let ghost = self.head.checked_sub(3 + wobble);
        let frightened = self.left < FRIGHTENED && self.app.now.is_some();
        let ghost_style = if frightened { theme.info } else { theme.error };
        let pellet_style = if fx.pulse() > 0.5 {
            theme.progress_done
        } else {
            theme.progress_todo
        };
        let last = self.width - 1;
        (0..self.width)
            .map(|i| {
                if i == self.head {
                    Span::styled(pac, theme.warning)
                } else if Some(i) == ghost {
                    Span::styled(if uni { "ᗣ" } else { "M" }, ghost_style)
                } else if i < self.head {
                    Span::raw(" ")
                } else if i == last {
                    Span::styled(if uni { "●" } else { "O" }, pellet_style)
                } else if i % 2 == 0 {
                    Span::styled(if uni { "·" } else { "." }, pellet_style)
                } else {
                    Span::raw(" ")
                }
            })
            .collect()
    }

    /// A cat flies ahead of a rainbow that ripples behind it, into twinkling stars. It blinks
    /// now and then, and sleeps while the music is paused.
    fn nyan(&self) -> Vec<Span<'static>> {
        let theme = &self.app.theme;
        let fx = &self.app.progress_fx;
        let uni = self.unicode();
        let cat = if !self.playing {
            "=-.-="
        } else if fx.time % 3.0 < 0.15 {
            "=^-^="
        } else {
            "=^.^="
        };
        let start = self.head.saturating_sub(4).min(self.width - 5);
        let rainbow: [Style; 6] = [
            theme.error,
            theme.warning,
            theme.playing,
            theme.info,
            theme.title,
            theme.spectrum_hot,
        ];
        let ripple = (fx.time * 6.0) as usize;
        let flow = (fx.time * 4.0) as usize;
        let moment = (fx.time * 3.0) as u64;
        let bright = fx.pulse() > 0.5;
        let mut spans = Vec::with_capacity(self.width);
        for i in 0..start {
            let wave = (i + ripple).is_multiple_of(2);
            let glyph = match (uni, wave) {
                (true, true) => "▀",
                (true, false) => "▄",
                (false, true) => "~",
                (false, false) => "=",
            };
            let colour = rainbow[(i + self.width - flow % self.width) % rainbow.len()];
            spans.push(Span::styled(glyph, colour));
        }
        spans.push(Span::styled(cat, theme.base.patch(theme.title)));
        for i in start + 5..self.width {
            let glyph = match (twinkle(i, moment) % 14, uni) {
                (0, true) => "✦",
                (0, false) => "*",
                (1 | 2, true) => "·",
                (1 | 2, false) => ".",
                _ => " ",
            };
            let style = if bright { theme.title } else { theme.dim };
            spans.push(Span::styled(glyph, style));
        }
        spans
    }

    /// The loudness of the song so far, coloured like the spectrum, with a note at the
    /// playing position that flips on the beat.
    fn waveform(&self) -> Vec<Span<'static>> {
        let theme = &self.app.theme;
        let fx = &self.app.progress_fx;
        let uni = self.unicode();
        let levels = self.app.symbols.levels;
        let peak = fx.peak().max(0.05);
        let cell = 1.0 / self.width as f32;
        (0..self.width)
            .map(|i| {
                if i == self.head {
                    let note = match (uni, fx.pulse() > 0.5) {
                        (true, true) => "♫",
                        (true, false) => "♪",
                        (false, true) => "*",
                        (false, false) => "o",
                    };
                    return Span::styled(note, theme.base.patch(theme.title));
                }
                if i > self.head {
                    return Span::styled(if uni { "─" } else { "-" }, theme.progress_todo);
                }
                match fx.loudness(i as f32 * cell, (i + 1) as f32 * cell) {
                    Some(v) => {
                        let level = ((v / peak) * 7.0).round().clamp(0.0, 7.0) as usize;
                        let style = match level {
                            0..=2 => theme.spectrum,
                            3..=5 => theme.spectrum_mid,
                            _ => theme.spectrum_hot,
                        };
                        Span::styled(levels[level], style)
                    }
                    // Skipped over, or played before the player started.
                    None => Span::styled(if uni { "·" } else { "." }, theme.dim),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Group, Track};
    use crate::ui::tests::{render, test_app};
    use unicode_width::UnicodeWidthStr;

    /// The progress line of a rendered 100x30 screen.
    fn progress_line(app: &mut App) -> String {
        let screen = render(app, 100, 30);
        screen
            .lines()
            .find(|l| l.contains(" / "))
            .unwrap()
            .to_string()
    }

    #[test]
    fn every_style_fills_the_bar_exactly() {
        let mut root = Group::new(0, "Root");
        root.tracks.push(Track::new("a.mp3"));
        let mut app = test_app(root);
        for unicode in [true, false] {
            app.symbols = if unicode {
                crate::ui::theme::Symbols::UNICODE
            } else {
                crate::ui::theme::Symbols::ASCII
            };
            for style in ProgressStyle::ALL {
                app.progress_style = style;
                let line = progress_line(&mut app);
                assert_eq!(line.width(), 100, "{style:?}: {line}");
                assert!(line.contains("00:00 / 00:00"), "{line}");
            }
        }
        // Stopped, Pac-Man waits at the start with his mouth closed, facing the pellets.
        app.symbols = crate::ui::theme::Symbols::UNICODE;
        app.progress_style = ProgressStyle::PacMan;
        let line = progress_line(&mut app);
        assert!(line.contains("[● · · ·"), "{line}");
        assert!(line.contains("●] 00:00"), "the power pellet: {line}");
        app.progress_style = ProgressStyle::Nyan;
        assert!(progress_line(&mut app).contains("[=-.-="), "asleep");
        app.progress_style = ProgressStyle::Waveform;
        assert!(progress_line(&mut app).contains("[♪──"));
    }

    /// The app 40 % into a two-minute song, after a while of music.
    fn mid_song() -> App {
        use crate::audio::StreamInfo;
        use crate::ui::app::NowPlaying;
        let mut root = Group::new(0, "Root");
        root.tracks.push(Track::new("a.mp3"));
        let mut app = test_app(root);
        app.now = Some(NowPlaying {
            track: None,
            path: "a.mp3".into(),
            info: StreamInfo {
                codec: "mp3".into(),
                sample_rate: 44_100,
                channels: 2,
            },
            duration: Some(Duration::from_secs(120)),
        });
        app.audio.shared().set_position(Duration::from_secs(48));
        // A song that gets louder, heard up to here.
        for i in 0..400 {
            let at = i as f32 / 1000.0;
            let level = 0.2 + at;
            app.progress_fx
                .update(0.02, &[level; 16], Some(at), Some(1), true);
        }
        app
    }

    #[test]
    fn styles_mid_song() {
        let mut app = mid_song();
        for style in ProgressStyle::ALL {
            app.progress_style = style;
            let line = progress_line(&mut app);
            println!("{line}");
            assert_eq!(line.width(), 100);
            assert!(line.contains("00:48 / 02:00"), "{line}");
        }
        // Pac-Man is 40 % along, with a ghost behind him and pellets ahead; what he ate is
        // gone.
        app.progress_style = ProgressStyle::PacMan;
        let line = progress_line(&mut app);
        let bar = &line[line.find('[').unwrap() + 1..line.find(']').unwrap()];
        let cells: Vec<char> = bar.chars().collect();
        let pac = cells.iter().position(|&c| c == '●' || c == 'ᗧ').unwrap();
        assert!(
            (pac as f32 / cells.len() as f32 - 0.4).abs() < 0.02,
            "{bar}"
        );
        assert!(cells[..pac].contains(&'ᗣ'), "{bar}");
        assert!(!cells[..pac].contains(&'·'), "{bar}");
        assert!(cells[pac..].contains(&'·'), "{bar}");
        // The waveform rises with the song (a third of the peak at the start).
        app.progress_style = ProgressStyle::Waveform;
        let line = progress_line(&mut app);
        assert!(line.contains("[▃▄"), "{line}");
        assert!(line.contains("█♪─"), "{line}");
    }
}
