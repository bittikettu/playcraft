//! Colour themes and character sets.
//!
//! The RGB themes follow popular VS Code colour schemes; Dracula is the default.
//! `classic_blue` and `monochrome` use only the 16 ANSI colours and work everywhere.

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
    /// Spectrum gradient: low part of the bars, middle, and tops.
    pub spectrum: Style,
    pub spectrum_mid: Style,
    pub spectrum_hot: Style,
    pub info: Style,
    pub warning: Style,
    pub error: Style,
}

/// A theme as listed in the theme selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeInfo {
    /// Name used in `config.toml`.
    pub name: &'static str,
    pub label: &'static str,
    /// Needs a 24-bit colour terminal.
    pub truecolor: bool,
}

pub const DEFAULT_THEME: &str = "dracula";

pub const THEMES: &[ThemeInfo] = &[
    ThemeInfo {
        name: "dracula",
        label: "Dracula",
        truecolor: true,
    },
    ThemeInfo {
        name: "tokyo_night",
        label: "Tokyo Night",
        truecolor: true,
    },
    ThemeInfo {
        name: "one_dark",
        label: "One Dark",
        truecolor: true,
    },
    ThemeInfo {
        name: "monokai",
        label: "Monokai",
        truecolor: true,
    },
    ThemeInfo {
        name: "nord",
        label: "Nord",
        truecolor: true,
    },
    ThemeInfo {
        name: "catppuccin",
        label: "Catppuccin Mocha",
        truecolor: true,
    },
    ThemeInfo {
        name: "gruvbox",
        label: "Gruvbox Dark",
        truecolor: true,
    },
    ThemeInfo {
        name: "classic_blue",
        label: "Classic Blue (MP3Blaster)",
        truecolor: false,
    },
    ThemeInfo {
        name: "monochrome",
        label: "Monochrome",
        truecolor: false,
    },
];

pub fn theme_info(name: &str) -> Option<&'static ThemeInfo> {
    THEMES.iter().find(|t| t.name == name)
}

/// Whether the terminal can show 24-bit colour. Windows 10+ consoles (Windows Terminal and
/// conhost with VT processing, which crossterm enables) always can; elsewhere `COLORTERM`
/// tells.
pub fn supports_truecolor() -> bool {
    cfg!(windows)
        || std::env::var_os("WT_SESSION").is_some()
        || std::env::var("COLORTERM").is_ok_and(|v| v == "truecolor" || v == "24bit")
}

/// The colours of an RGB theme, as `0xRRGGBB`.
struct Palette {
    bg: u32,
    fg: u32,
    comment: u32,
    border: u32,
    cursor_fg: u32,
    cursor_bg: u32,
    track: u32,
    title: u32,
    key: u32,
    playing: u32,
    group: u32,
    missing: u32,
    selected: u32,
    progress: u32,
    spectrum: u32,
    mid: u32,
    hot: u32,
    info: u32,
    warning: u32,
}

impl Palette {
    fn theme(&self, name: &'static str) -> Theme {
        let c = Color::from_u32;
        let base = Style::new().fg(c(self.fg)).bg(c(self.bg));
        let bold = Modifier::BOLD;
        Theme {
            name,
            base,
            border: base.fg(c(self.border)),
            title: base.fg(c(self.title)).add_modifier(bold),
            key: base.fg(c(self.key)).add_modifier(bold),
            dim: base.fg(c(self.comment)),
            cursor: Style::new()
                .fg(c(self.cursor_fg))
                .bg(c(self.cursor_bg))
                .add_modifier(bold),
            playing: base.fg(c(self.playing)).add_modifier(bold),
            group: base.fg(c(self.group)).add_modifier(bold),
            missing: base.fg(c(self.missing)),
            selected: base.fg(c(self.selected)).add_modifier(bold),
            progress_done: base.fg(c(self.progress)),
            progress_todo: base.fg(c(self.track)),
            spectrum: base.fg(c(self.spectrum)),
            spectrum_mid: base.fg(c(self.mid)),
            spectrum_hot: base.fg(c(self.hot)),
            info: base.fg(c(self.info)),
            warning: base.fg(c(self.warning)).add_modifier(bold),
            error: Style::new()
                .fg(c(self.bg))
                .bg(c(self.missing))
                .add_modifier(bold),
        }
    }
}

impl Theme {
    /// Looks up a theme by its config name. RGB themes fall back to `classic_blue` when
    /// truecolor is unavailable; the second value explains why.
    pub fn by_name(name: &str, truecolor: bool) -> (Theme, Option<String>) {
        let needs_rgb = theme_info(name).is_some_and(|t| t.truecolor);
        if needs_rgb && !truecolor {
            return (
                Theme::classic_blue(),
                Some(format!(
                    "Theme \"{name}\" needs a truecolor terminal (e.g. Windows Terminal); using classic_blue"
                )),
            );
        }
        let theme = match name {
            "dracula" => Theme::dracula(),
            "tokyo_night" => Theme::tokyo_night(),
            "one_dark" => Theme::one_dark(),
            "monokai" => Theme::monokai(),
            "nord" => Theme::nord(),
            "catppuccin" => Theme::catppuccin(),
            "gruvbox" => Theme::gruvbox(),
            "monochrome" => Theme::monochrome(),
            _ => Theme::classic_blue(),
        };
        (theme, None)
    }

    /// Dracula: the "vampire" theme familiar from VS Code.
    pub fn dracula() -> Self {
        Palette {
            bg: 0x282a36,
            fg: 0xf8f8f2,
            comment: 0x6272a4,
            border: 0x6272a4,
            cursor_fg: 0xf8f8f2,
            cursor_bg: 0x44475a,
            track: 0x44475a,
            title: 0xbd93f9,
            key: 0xff79c6,
            playing: 0x50fa7b,
            group: 0x8be9fd,
            missing: 0xff5555,
            selected: 0xffb86c,
            progress: 0xff79c6,
            spectrum: 0xbd93f9,
            mid: 0xff79c6,
            hot: 0xff5555,
            info: 0x50fa7b,
            warning: 0xf1fa8c,
        }
        .theme("dracula")
    }

    pub fn tokyo_night() -> Self {
        Palette {
            bg: 0x1a1b26,
            fg: 0xc0caf5,
            comment: 0x565f89,
            border: 0x3b4261,
            cursor_fg: 0xc0caf5,
            cursor_bg: 0x283457,
            track: 0x292e42,
            title: 0x7aa2f7,
            key: 0xff9e64,
            playing: 0x9ece6a,
            group: 0x7dcfff,
            missing: 0xf7768e,
            selected: 0xbb9af7,
            progress: 0x7aa2f7,
            spectrum: 0x7aa2f7,
            mid: 0xbb9af7,
            hot: 0xf7768e,
            info: 0x9ece6a,
            warning: 0xe0af68,
        }
        .theme("tokyo_night")
    }

    pub fn one_dark() -> Self {
        Palette {
            bg: 0x282c34,
            fg: 0xabb2bf,
            comment: 0x5c6370,
            border: 0x4b5263,
            cursor_fg: 0xe6e6e6,
            cursor_bg: 0x3e4451,
            track: 0x3e4451,
            title: 0x61afef,
            key: 0xe5c07b,
            playing: 0x98c379,
            group: 0x56b6c2,
            missing: 0xe06c75,
            selected: 0xc678dd,
            progress: 0x61afef,
            spectrum: 0x61afef,
            mid: 0xc678dd,
            hot: 0xe06c75,
            info: 0x98c379,
            warning: 0xd19a66,
        }
        .theme("one_dark")
    }

    pub fn monokai() -> Self {
        Palette {
            bg: 0x272822,
            fg: 0xf8f8f2,
            comment: 0x75715e,
            border: 0x75715e,
            cursor_fg: 0xf8f8f2,
            cursor_bg: 0x49483e,
            track: 0x3e3d32,
            title: 0xf92672,
            key: 0xe6db74,
            playing: 0xa6e22e,
            group: 0x66d9ef,
            missing: 0xf92672,
            selected: 0xae81ff,
            progress: 0xfd971f,
            spectrum: 0x66d9ef,
            mid: 0xae81ff,
            hot: 0xf92672,
            info: 0xa6e22e,
            warning: 0xfd971f,
        }
        .theme("monokai")
    }

    pub fn nord() -> Self {
        Palette {
            bg: 0x2e3440,
            fg: 0xd8dee9,
            comment: 0x616e88,
            border: 0x4c566a,
            cursor_fg: 0xeceff4,
            cursor_bg: 0x434c5e,
            track: 0x3b4252,
            title: 0x88c0d0,
            key: 0xebcb8b,
            playing: 0xa3be8c,
            group: 0x8fbcbb,
            missing: 0xbf616a,
            selected: 0xb48ead,
            progress: 0x88c0d0,
            spectrum: 0x81a1c1,
            mid: 0xb48ead,
            hot: 0xbf616a,
            info: 0xa3be8c,
            warning: 0xd08770,
        }
        .theme("nord")
    }

    /// Catppuccin Mocha.
    pub fn catppuccin() -> Self {
        Palette {
            bg: 0x1e1e2e,
            fg: 0xcdd6f4,
            comment: 0xa6adc8,
            border: 0x6c7086,
            cursor_fg: 0x1e1e2e,
            cursor_bg: 0x89b4fa,
            track: 0x45475a,
            title: 0xcba6f7,
            key: 0xfab387,
            playing: 0xa6e3a1,
            group: 0x74c7ec,
            missing: 0xf38ba8,
            selected: 0xf5c2e7,
            progress: 0xb4befe,
            spectrum: 0x89b4fa,
            mid: 0xcba6f7,
            hot: 0xf38ba8,
            info: 0xa6e3a1,
            warning: 0xf9e2af,
        }
        .theme("catppuccin")
    }

    pub fn gruvbox() -> Self {
        Palette {
            bg: 0x282828,
            fg: 0xebdbb2,
            comment: 0x928374,
            border: 0x665c54,
            cursor_fg: 0x282828,
            cursor_bg: 0x83a598,
            track: 0x504945,
            title: 0xfabd2f,
            key: 0xfe8019,
            playing: 0xb8bb26,
            group: 0x8ec07c,
            missing: 0xfb4934,
            selected: 0xd3869b,
            progress: 0xfabd2f,
            spectrum: 0x8ec07c,
            mid: 0xfabd2f,
            hot: 0xfb4934,
            info: 0xb8bb26,
            warning: 0xfe8019,
        }
        .theme("gruvbox")
    }

    /// The original MP3Blaster look, in the 16 ANSI colours.
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
            spectrum_mid: base.fg(Color::Yellow),
            spectrum_hot: base.fg(Color::LightRed),
            info: base.fg(Color::LightGreen),
            warning: base.fg(Color::Yellow).add_modifier(Modifier::BOLD),
            error: Style::new()
                .fg(Color::White)
                .bg(Color::Red)
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
            spectrum_mid: bold,
            spectrum_hot: bold,
            info: base,
            warning: bold,
            error: base.add_modifier(Modifier::REVERSED | Modifier::BOLD),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Theme::by_name(DEFAULT_THEME, supports_truecolor()).0
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
    fn every_listed_theme_resolves_to_itself() {
        for info in THEMES {
            let (theme, warning) = Theme::by_name(info.name, true);
            assert_eq!(theme.name, info.name);
            assert!(warning.is_none());
        }
        assert!(theme_info(DEFAULT_THEME).is_some());
    }

    #[test]
    fn rgb_themes_need_truecolor() {
        for info in THEMES {
            let (theme, warning) = Theme::by_name(info.name, false);
            if info.truecolor {
                assert_eq!(theme.name, "classic_blue", "{}", info.name);
                assert!(warning.is_some());
            } else {
                assert_eq!(theme.name, info.name);
            }
        }
    }

    #[test]
    fn dracula_uses_the_dracula_palette() {
        let t = Theme::dracula();
        assert_eq!(t.base.bg, Some(Color::Rgb(0x28, 0x2a, 0x36)));
        assert_eq!(t.base.fg, Some(Color::Rgb(0xf8, 0xf8, 0xf2)));
        assert_eq!(t.title.fg, Some(Color::Rgb(0xbd, 0x93, 0xf9)));
        assert_eq!(t.key.fg, Some(Color::Rgb(0xff, 0x79, 0xc6)));
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
