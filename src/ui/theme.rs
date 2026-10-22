//! Colour themes and character sets.
//!
//! `classic_blue` and `monochrome` use only the 16 ANSI colours so they render in the legacy
//! Windows console; RGB themes are only used when the terminal advertises truecolor.

use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;

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

/// Whether the terminal can show 24-bit colour. Windows Terminal sets `WT_SESSION`.
pub fn supports_truecolor() -> bool {
    std::env::var_os("WT_SESSION").is_some()
        || std::env::var("COLORTERM").is_ok_and(|v| v == "truecolor" || v == "24bit")
}

impl Theme {
    /// Looks up a theme by its config name. RGB themes fall back to `classic_blue` when
    /// truecolor is unavailable; the second value explains why.
    pub fn by_name(name: &str, truecolor: bool) -> (Theme, Option<String>) {
        match name {
            "gruvbox" | "catppuccin" if !truecolor => (
                Theme::classic_blue(),
                Some(format!(
                    "Theme \"{name}\" needs a truecolor terminal (e.g. Windows Terminal); using classic_blue"
                )),
            ),
            "gruvbox" => (Theme::gruvbox(), None),
            "catppuccin" => (Theme::catppuccin(), None),
            "monochrome" => (Theme::monochrome(), None),
            _ => (Theme::classic_blue(), None),
        }
    }

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
            error: Style::new()
                .fg(Color::White)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD),
        }
    }

    pub fn gruvbox() -> Self {
        let rgb = |hex: u32| Color::from_u32(hex);
        let base = Style::new().fg(rgb(0xebdbb2)).bg(rgb(0x282828));
        Theme {
            name: "gruvbox",
            base,
            border: base.fg(rgb(0x665c54)),
            title: base.fg(rgb(0xfabd2f)).add_modifier(Modifier::BOLD),
            key: base.fg(rgb(0xfe8019)).add_modifier(Modifier::BOLD),
            dim: base.fg(rgb(0x928374)),
            cursor: Style::new().fg(rgb(0x282828)).bg(rgb(0x83a598)),
            playing: base.fg(rgb(0xb8bb26)).add_modifier(Modifier::BOLD),
            group: base.fg(rgb(0x8ec07c)).add_modifier(Modifier::BOLD),
            missing: base.fg(rgb(0xfb4934)),
            selected: base.fg(rgb(0xd3869b)).add_modifier(Modifier::BOLD),
            progress_done: base.fg(rgb(0xfabd2f)),
            progress_todo: base.fg(rgb(0x504945)),
            spectrum: base.fg(rgb(0x8ec07c)),
            info: base.fg(rgb(0xb8bb26)),
            warning: base.fg(rgb(0xfe8019)).add_modifier(Modifier::BOLD),
            error: Style::new()
                .fg(rgb(0xfbf1c7))
                .bg(rgb(0xcc241d))
                .add_modifier(Modifier::BOLD),
        }
    }

    /// Catppuccin Mocha.
    pub fn catppuccin() -> Self {
        let rgb = |hex: u32| Color::from_u32(hex);
        let base = Style::new().fg(rgb(0xcdd6f4)).bg(rgb(0x1e1e2e));
        Theme {
            name: "catppuccin",
            base,
            border: base.fg(rgb(0x6c7086)),
            title: base.fg(rgb(0xcba6f7)).add_modifier(Modifier::BOLD),
            key: base.fg(rgb(0xfab387)).add_modifier(Modifier::BOLD),
            dim: base.fg(rgb(0xa6adc8)),
            cursor: Style::new().fg(rgb(0x1e1e2e)).bg(rgb(0x89b4fa)),
            playing: base.fg(rgb(0xa6e3a1)).add_modifier(Modifier::BOLD),
            group: base.fg(rgb(0x74c7ec)).add_modifier(Modifier::BOLD),
            missing: base.fg(rgb(0xf38ba8)),
            selected: base.fg(rgb(0xf5c2e7)).add_modifier(Modifier::BOLD),
            progress_done: base.fg(rgb(0xb4befe)),
            progress_todo: base.fg(rgb(0x45475a)),
            spectrum: base.fg(rgb(0x89b4fa)),
            info: base.fg(rgb(0xa6e3a1)),
            warning: base.fg(rgb(0xf9e2af)).add_modifier(Modifier::BOLD),
            error: Style::new()
                .fg(rgb(0x1e1e2e))
                .bg(rgb(0xf38ba8))
                .add_modifier(Modifier::BOLD),
        }
    }

    /// Terminal default colours; emphasis by bold, underline and reverse video only.
    pub fn monochrome() -> Self {
        let base = Style::new().fg(Color::Reset).bg(Color::Reset);
        let bold = base.add_modifier(Modifier::BOLD);
        Theme {
            name: "monochrome",
            base,
            border: base,
            title: bold,
            key: bold,
            dim: base.add_modifier(Modifier::DIM),
            cursor: base.add_modifier(Modifier::REVERSED),
            playing: bold.add_modifier(Modifier::UNDERLINED),
            group: bold,
            missing: base.add_modifier(Modifier::CROSSED_OUT),
            selected: bold.add_modifier(Modifier::ITALIC),
            progress_done: bold,
            progress_todo: base.add_modifier(Modifier::DIM),
            spectrum: base,
            info: base,
            warning: bold,
            error: base.add_modifier(Modifier::REVERSED | Modifier::BOLD),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Theme::classic_blue()
    }
}

/// Characters used to draw borders, bars and markers.
#[derive(Debug, Clone, Copy)]
pub struct Symbols {
    pub border: border::Set<'static>,
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
        border: border::PLAIN,
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
        border: border::Set {
            top_left: "+",
            top_right: "+",
            bottom_left: "+",
            bottom_right: "+",
            vertical_left: "|",
            vertical_right: "|",
            horizontal_top: "-",
            horizontal_bottom: "-",
        },
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

    pub fn by_name(charset: &str) -> Symbols {
        if charset == "ascii" {
            Symbols::ASCII
        } else {
            Symbols::UNICODE
        }
    }
}

impl Default for Symbols {
    fn default() -> Self {
        Symbols::UNICODE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_themes_need_truecolor() {
        let (t, warn) = Theme::by_name("gruvbox", false);
        assert_eq!(t.name, "classic_blue");
        assert!(warn.is_some());
        let (t, warn) = Theme::by_name("catppuccin", true);
        assert_eq!(t.name, "catppuccin");
        assert!(warn.is_none());
        assert_eq!(Theme::by_name("monochrome", false).0.name, "monochrome");
    }

    #[test]
    fn ascii_symbols_are_ascii() {
        let s = Symbols::ASCII;
        let all = [
            s.border.top_left,
            s.border.horizontal_top,
            s.border.vertical_left,
            s.cursor,
            s.breadcrumb,
            s.ellipsis,
        ];
        assert!(all.iter().chain(s.levels.iter()).all(|x| x.is_ascii()));
    }
}
