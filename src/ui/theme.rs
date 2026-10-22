//! Colour themes and character sets.
//!
//! `classic_blue` and `monochrome` use only the 16 ANSI colours so they render in the legacy
//! Windows console; RGB themes are only used when the terminal advertises truecolor.

use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone)]
pub struct Theme {
    pub name: &'static str,
    pub base: Style,
    pub border: Style,
    pub title: Style,
    pub key: Style,
    pub dim: Style,
    pub cursor: Style,
    pub playing: Style,
    pub group: Style,
    pub missing: Style,
    pub selected: Style,
    pub progress_done: Style,
    pub progress_todo: Style,
    pub spectrum: Style,
    pub info: Style,
    pub warning: Style,
    pub error: Style,
}

impl Theme {
    pub fn classic_blue() -> Self {
        let base = Style::new().fg(Color::White).bg(Color::Blue);
        Theme {
            name: "classic_blue",
            base,
            border: base.fg(Color::Cyan),
            title: base.fg(Color::LightCyan).add_modifier(Modifier::BOLD),
            key: base.fg(Color::Yellow).add_modifier(Modifier::BOLD),
            dim: base.fg(Color::Gray),
            cursor: Style::new().fg(Color::Black).bg(Color::Cyan),
            playing: base.fg(Color::Yellow).add_modifier(Modifier::BOLD),
            group: base.fg(Color::LightGreen).add_modifier(Modifier::BOLD),
            missing: base.fg(Color::LightRed),
            selected: base.fg(Color::LightMagenta).add_modifier(Modifier::BOLD),
            progress_done: base.fg(Color::Yellow),
            progress_todo: base.fg(Color::Gray),
            spectrum: base.fg(Color::LightCyan),
            info: base.fg(Color::LightGreen),
            warning: base.fg(Color::Yellow).add_modifier(Modifier::BOLD),
            error: Style::new().fg(Color::White).bg(Color::Red).add_modifier(Modifier::BOLD),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Theme::classic_blue()
    }
}

/// Characters used to draw bars and markers.
#[derive(Debug, Clone, Copy)]
pub struct Symbols {
    pub cursor: &'static str,
    pub progress_done: &'static str,
    pub progress_head: &'static str,
    pub progress_todo: &'static str,
    pub volume_on: &'static str,
    pub volume_off: &'static str,
    /// Spectrum bar levels from lowest to full.
    pub levels: [&'static str; 8],
    pub breadcrumb: &'static str,
    pub ellipsis: &'static str,
}

impl Symbols {
    pub const UNICODE: Symbols = Symbols {
        cursor: ">",
        progress_done: "=",
        progress_head: ">",
        progress_todo: "-",
        volume_on: "|",
        volume_off: "-",
        levels: ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"],
        breadcrumb: " › ",
        ellipsis: "…",
    };

    pub const ASCII: Symbols = Symbols {
        cursor: ">",
        progress_done: "=",
        progress_head: ">",
        progress_todo: "-",
        volume_on: "|",
        volume_off: "-",
        levels: [".", ".", ":", ":", "|", "|", "#", "#"],
        breadcrumb: " > ",
        ellipsis: "~",
    };
}

impl Default for Symbols {
    fn default() -> Self {
        Symbols::UNICODE
    }
}
