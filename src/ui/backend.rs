//! A crossterm backend that sends less to the terminal than ratatui's `CrosstermBackend`.
//!
//! Colour-heavy frames (the HYPERSPACE visualizer is two 24-bit colours per cell) are
//! dominated by escape sequences, so this backend:
//! - only sends the colour that actually changed (foreground or background), and both in one
//!   sequence when both did;
//! - moves the cursor right with a short relative sequence within a row instead of an
//!   absolute position, tracking the real cursor column (wide characters advance it by two).

use std::io::{self, Write};

use ratatui::backend::{Backend, ClearType, IntoCrossterm, WindowSize};
use ratatui::buffer::Cell;
use ratatui::crossterm::style::{
    Attribute, Color as CtColor, Colors, Print, SetAttribute, SetBackgroundColor, SetColors,
    SetForegroundColor,
};
use ratatui::crossterm::{cursor, execute, queue, terminal};
use ratatui::layout::{Position, Size};
use ratatui::style::{Color, Modifier};
use unicode_width::UnicodeWidthStr;

pub struct FastBackend<W: Write> {
    writer: W,
}

impl<W: Write> FastBackend<W> {
    pub fn new(writer: W) -> Self {
        FastBackend { writer }
    }
}

/// Switches text attributes from `from` to `to` without touching the colours.
fn queue_modifier_change(w: &mut impl Write, from: Modifier, to: Modifier) -> io::Result<()> {
    let removed = from - to;
    let added = to - from;
    if removed.contains(Modifier::REVERSED) {
        queue!(w, SetAttribute(Attribute::NoReverse))?;
    }
    if removed.intersects(Modifier::BOLD | Modifier::DIM) {
        // "Normal intensity" clears both bold and dim; restore the one that stays.
        queue!(w, SetAttribute(Attribute::NormalIntensity))?;
        if to.contains(Modifier::DIM) {
            queue!(w, SetAttribute(Attribute::Dim))?;
        }
        if to.contains(Modifier::BOLD) {
            queue!(w, SetAttribute(Attribute::Bold))?;
        }
    }
    if removed.contains(Modifier::ITALIC) {
        queue!(w, SetAttribute(Attribute::NoItalic))?;
    }
    if removed.contains(Modifier::UNDERLINED) {
        queue!(w, SetAttribute(Attribute::NoUnderline))?;
    }
    if removed.contains(Modifier::CROSSED_OUT) {
        queue!(w, SetAttribute(Attribute::NotCrossedOut))?;
    }
    if removed.intersects(Modifier::SLOW_BLINK | Modifier::RAPID_BLINK) {
        queue!(w, SetAttribute(Attribute::NoBlink))?;
    }
    if removed.contains(Modifier::HIDDEN) {
        queue!(w, SetAttribute(Attribute::NoHidden))?;
    }
    for (flag, attribute) in [
        (Modifier::REVERSED, Attribute::Reverse),
        (Modifier::BOLD, Attribute::Bold),
        (Modifier::DIM, Attribute::Dim),
        (Modifier::ITALIC, Attribute::Italic),
        (Modifier::UNDERLINED, Attribute::Underlined),
        (Modifier::CROSSED_OUT, Attribute::CrossedOut),
        (Modifier::SLOW_BLINK, Attribute::SlowBlink),
        (Modifier::RAPID_BLINK, Attribute::RapidBlink),
        (Modifier::HIDDEN, Attribute::Hidden),
    ] {
        if added.contains(flag) {
            queue!(w, SetAttribute(attribute))?;
        }
    }
    Ok(())
}

impl<W: Write> Backend for FastBackend<W> {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let w = &mut self.writer;
        let mut fg = Color::Reset;
        let mut bg = Color::Reset;
        let mut modifier = Modifier::empty();
        // Where the terminal cursor is after the last character we printed.
        let mut cursor_at: Option<(u16, u16)> = None;
        for (x, y, cell) in content {
            match cursor_at {
                Some((cx, cy)) if cy == y && cx == x => {}
                Some((cx, cy)) if cy == y && cx < x => queue!(w, cursor::MoveRight(x - cx))?,
                _ => queue!(w, cursor::MoveTo(x, y))?,
            }
            if cell.modifier != modifier {
                queue_modifier_change(w, modifier, cell.modifier)?;
                modifier = cell.modifier;
            }
            match (cell.fg != fg, cell.bg != bg) {
                (true, true) => queue!(
                    w,
                    SetColors(Colors::new(
                        cell.fg.into_crossterm(),
                        cell.bg.into_crossterm()
                    ))
                )?,
                (true, false) => queue!(w, SetForegroundColor(cell.fg.into_crossterm()))?,
                (false, true) => queue!(w, SetBackgroundColor(cell.bg.into_crossterm()))?,
                (false, false) => {}
            }
            fg = cell.fg;
            bg = cell.bg;
            let symbol = cell.symbol();
            queue!(w, Print(symbol))?;
            let width = symbol.width();
            // A zero-width symbol leaves the cursor somewhere we cannot be sure of.
            cursor_at = (width > 0).then(|| (x.saturating_add(width as u16), y));
        }
        queue!(
            w,
            SetForegroundColor(CtColor::Reset),
            SetBackgroundColor(CtColor::Reset),
            SetAttribute(Attribute::Reset)
        )
    }

    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        for _ in 0..n {
            queue!(self.writer, Print("\n"))?;
        }
        self.writer.flush()
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        execute!(self.writer, cursor::Hide)
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        execute!(self.writer, cursor::Show)
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        cursor::position().map(|(x, y)| Position { x, y })
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        let p = position.into();
        execute!(self.writer, cursor::MoveTo(p.x, p.y))
    }

    fn clear(&mut self) -> io::Result<()> {
        self.clear_region(ClearType::All)
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        let kind = match clear_type {
            ClearType::All => terminal::ClearType::All,
            ClearType::AfterCursor => terminal::ClearType::FromCursorDown,
            ClearType::BeforeCursor => terminal::ClearType::FromCursorUp,
            ClearType::CurrentLine => terminal::ClearType::CurrentLine,
            ClearType::UntilNewLine => terminal::ClearType::UntilNewLine,
        };
        execute!(self.writer, terminal::Clear(kind))
    }

    fn size(&self) -> io::Result<Size> {
        let (width, height) = terminal::size()?;
        Ok(Size { width, height })
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        let terminal::WindowSize {
            columns,
            rows,
            width,
            height,
        } = terminal::window_size()?;
        Ok(WindowSize {
            columns_rows: Size {
                width: columns,
                height: rows,
            },
            pixels: Size { width, height },
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use ratatui::Terminal;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::{TerminalOptions, Viewport};

    use super::*;
    use crate::model::{Group, Track};
    use crate::ui::{app, tests::test_app, views};

    #[derive(Clone, Default)]
    struct Capture(Rc<RefCell<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// What a terminal shows for a ratatui colour (crossterm's SGR mapping).
    fn expected(color: Color) -> vt100::Color {
        let idx = |i| vt100::Color::Idx(i);
        match color {
            Color::Reset => vt100::Color::Default,
            Color::Black => idx(0),
            Color::Red => idx(1),
            Color::Green => idx(2),
            Color::Yellow => idx(3),
            Color::Blue => idx(4),
            Color::Magenta => idx(5),
            Color::Cyan => idx(6),
            Color::Gray => idx(7),
            Color::DarkGray => idx(8),
            Color::LightRed => idx(9),
            Color::LightGreen => idx(10),
            Color::LightYellow => idx(11),
            Color::LightBlue => idx(12),
            Color::LightMagenta => idx(13),
            Color::LightCyan => idx(14),
            Color::White => idx(15),
            Color::Indexed(i) => idx(i),
            Color::Rgb(r, g, b) => vt100::Color::Rgb(r, g, b),
        }
    }

    /// Asserts that the emulated terminal shows exactly what ratatui intended.
    fn assert_screen_matches(screen: &vt100::Screen, buffer: &Buffer, label: &str) {
        let area = buffer.area;
        for y in 0..area.height {
            for x in 0..area.width {
                let want = &buffer[(x, y)];
                let got = screen.cell(y, x).expect("cell in range");
                if got.is_wide_continuation() {
                    continue;
                }
                let symbol = if want.symbol() == " " {
                    ""
                } else {
                    want.symbol()
                };
                let shown = if got.contents() == " " {
                    ""
                } else {
                    got.contents()
                };
                assert_eq!(shown, symbol, "{label}: text at ({x},{y})");
                assert_eq!(got.fgcolor(), expected(want.fg), "{label}: fg at ({x},{y})");
                assert_eq!(got.bgcolor(), expected(want.bg), "{label}: bg at ({x},{y})");
            }
        }
    }

    #[test]
    fn terminal_shows_exactly_what_ratatui_drew() {
        // Colours are what is being checked, whatever NO_COLOR says in this environment.
        ratatui::crossterm::style::Colored::set_ansi_color_disabled(false);
        let mut root = Group::new(0, "Root");
        for name in [
            "日本語の歌.mp3",
            "Mötley Crüe - Kickstart.flac",
            "plain.ogg",
        ] {
            root.tracks.push(Track::new(name));
        }
        let mut app = test_app(root);
        let (w, h) = (100u16, 30u16);
        let capture = Capture::default();
        let options = TerminalOptions {
            viewport: Viewport::Fixed(Rect::new(0, 0, w, h)),
        };
        let mut terminal =
            Terminal::with_options(FastBackend::new(capture.clone()), options).unwrap();
        let mut parser = vt100::Parser::new(
            std::num::NonZeroU16::new(h).unwrap(),
            std::num::NonZeroU16::new(w).unwrap(),
            0,
        );
        let wave: Vec<f32> = (0..256).map(|i| (i as f32 * 0.2).sin() * 0.5).collect();
        for frame in 0..40 {
            match frame {
                // Playlist (themes with named colours), then HYPERSPACE fullscreen, then bars.
                0..=4 => app.theme = crate::ui::theme::Theme::classic_blue(),
                5 => app.theme = crate::ui::theme::Theme::dracula(),
                10 => {
                    app.visualizer = app::Visualizer::Hyperspace;
                    app.view = app::View::Visualizer;
                }
                30 => app.visualizer = app::Visualizer::Bars,
                _ => {}
            }
            if app.visualizer == app::Visualizer::Hyperspace {
                let level = if frame % 6 == 0 { 0.95 } else { 0.5 };
                app.hyperspace
                    .update(1.0 / 30.0, &[level; 48], &wave, Some((1, "Test")), true);
            }
            app.cursor = frame % 3;
            let completed = terminal.draw(|f| views::draw(f, &mut app)).unwrap();
            let buffer = completed.buffer.clone();
            parser.process(&capture.0.borrow());
            capture.0.borrow_mut().clear();
            assert_screen_matches(parser.screen(), &buffer, &format!("frame {frame}"));
        }
    }

    #[test]
    fn sends_only_the_colour_that_changed() {
        ratatui::crossterm::style::Colored::set_ansi_color_disabled(false);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 3, 1));
        buffer[(0, 0)]
            .set_char('a')
            .set_fg(Color::Rgb(1, 2, 3))
            .set_bg(Color::Rgb(9, 9, 9));
        buffer[(1, 0)]
            .set_char('b')
            .set_fg(Color::Rgb(4, 5, 6))
            .set_bg(Color::Rgb(9, 9, 9));
        buffer[(2, 0)]
            .set_char('c')
            .set_fg(Color::Rgb(4, 5, 6))
            .set_bg(Color::Rgb(7, 7, 7));
        let capture = Capture::default();
        let mut backend = FastBackend::new(capture.clone());
        let cells: Vec<(u16, u16, &Cell)> = (0..3).map(|x| (x, 0, &buffer[(x, 0)])).collect();
        backend.draw(cells.into_iter()).unwrap();
        let out = String::from_utf8(capture.0.borrow().clone()).unwrap();
        assert!(out.contains("\x1b[38;2;1;2;3;48;2;9;9;9m"), "{out:?}");
        assert!(out.contains("a\x1b[38;2;4;5;6mb"), "only fg for b: {out:?}");
        assert!(out.contains("b\x1b[48;2;7;7;7mc"), "only bg for c: {out:?}");
    }
}
