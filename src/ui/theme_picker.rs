//! The colour theme picker: live preview while moving, Enter keeps (and saves to
//! `config.toml`), Esc restores the previous theme.

use super::app::{App, BannerLevel, Overlay};
use super::keymap::Action;
use super::theme::{THEMES, Theme, supports_truecolor, theme_info};

impl App {
    pub(crate) fn open_theme_selector(&mut self) {
        let original = theme_info(self.theme.name).map_or(THEMES[0].name, |t| t.name);
        let cursor = THEMES.iter().position(|t| t.name == original).unwrap_or(0);
        self.overlay = Overlay::Themes { cursor, original };
    }

    fn preview_theme(&mut self, cursor: usize, original: &'static str) {
        self.theme = Theme::by_name(THEMES[cursor].name, supports_truecolor()).0;
        self.overlay = Overlay::Themes { cursor, original };
    }

    pub(crate) fn on_theme_key(&mut self, action: Action, cursor: usize, original: &'static str) {
        let last = THEMES.len() - 1;
        match action {
            Action::CursorUp => {
                self.preview_theme(if cursor == 0 { last } else { cursor - 1 }, original)
            }
            Action::CursorDown => {
                self.preview_theme(if cursor == last { 0 } else { cursor + 1 }, original)
            }
            Action::Home | Action::PageUp => self.preview_theme(0, original),
            Action::End | Action::PageDown => self.preview_theme(last, original),
            Action::Activate => {
                self.overlay = Overlay::None;
                self.keep_theme(THEMES[cursor].name);
            }
            Action::Close | Action::Back | Action::Quit | Action::ThemeSelector | Action::Help => {
                self.theme = Theme::by_name(original, supports_truecolor()).0;
                self.overlay = Overlay::None;
            }
            // Playback keys keep working while the picker is open.
            other => self.on_action(other),
        }
    }

    fn keep_theme(&mut self, name: &'static str) {
        let (theme, warning) = Theme::by_name(name, supports_truecolor());
        self.theme = theme;
        let label = theme_info(name).map_or(name, |t| t.label);
        if let Some(warning) = warning {
            self.banner(BannerLevel::Warning, warning);
            return;
        }
        let Some(path) = self.config_path.clone() else {
            self.banner(
                BannerLevel::Info,
                format!("Theme: {label} (not saved: no config folder)"),
            );
            return;
        };
        match crate::config::set_string(&path, "ui", "color_theme", name) {
            Ok(()) => self.banner(
                BannerLevel::Info,
                format!("Theme: {label}, saved to {}", crate::paths::display(&path)),
            ),
            Err(e) => self.banner(
                BannerLevel::Warning,
                format!("Theme: {label}, not saved: {e}"),
            ),
        }
    }
}
