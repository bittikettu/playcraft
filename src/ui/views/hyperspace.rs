//! Draws the HYPERSPACE framebuffer: two pixels per terminal cell using the upper-half
//! block (foreground = top pixel, background = bottom pixel) in 24-bit colour.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::hyperspace::{Palette, Rgb};
use crate::ui::app::App;
use crate::ui::theme::Theme;

/// Spectrum bands the scene is driven by.
const BANDS: usize = 48;

fn rgb_of(color: Option<Color>) -> Option<Rgb> {
    match color? {
        Color::Rgb(r, g, b) => Some([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0]),
        _ => None,
    }
}

/// The theme's accent colours as neon, on a background darker than the theme's own.
fn palette(theme: &Theme) -> Palette {
    let fg = |s: Style| rgb_of(s.fg);
    let accents = [
        fg(theme.title),
        fg(theme.key),
        fg(theme.group),
        fg(theme.playing),
        fg(theme.warning),
        fg(theme.spectrum_hot),
    ];
    let default = Palette::default();
    if accents.iter().any(Option::is_none) {
        return default;
    }
    let bg = rgb_of(theme.base.bg).map_or(default.bg, |c| [c[0] * 0.45, c[1] * 0.45, c[2] * 0.45]);
    Palette {
        bg,
        colors: accents.map(|c| c.unwrap_or([1.0; 3])),
    }
}

/// Title text with the live tempo.
pub fn status(app: &App) -> String {
    match app.hyperspace.bpm() {
        Some(bpm) => format!(" >> HYPERSPACE << {bpm:.0} BPM "),
        None => " >> HYPERSPACE << ".to_string(),
    }
}

pub fn draw(f: &mut Frame<'_>, app: &mut App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    // ASCII mode cannot use half blocks: one pixel per cell, background colour only.
    let half_blocks = !app.symbols.ellipsis.is_ascii();
    let rows_per_cell = if half_blocks { 2 } else { 1 };
    app.spectrum.set_band_count(BANDS);
    app.hyperspace.palette = palette(&app.theme);
    app.hyperspace
        .resize(area.width as usize, area.height as usize * rows_per_cell);
    app.hyperspace.render();

    let hs = &app.hyperspace;
    let buf = f.buffer_mut();
    for cy in 0..area.height {
        for cx in 0..area.width {
            let (x, y) = (cx as usize, cy as usize * rows_per_cell);
            let top = hs.pixel(x, y);
            let bottom = if half_blocks { hs.pixel(x, y + 1) } else { top };
            let cell = &mut buf[(area.x + cx, area.y + cy)];
            let to_color = |p: [u8; 3]| Color::Rgb(p[0], p[1], p[2]);
            if top == bottom {
                cell.set_char(' ')
                    .set_fg(to_color(top))
                    .set_bg(to_color(top));
            } else {
                cell.set_char('▀')
                    .set_fg(to_color(top))
                    .set_bg(to_color(bottom));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_follows_rgb_themes_and_falls_back_for_ansi_ones() {
        let dracula = palette(&Theme::dracula());
        assert_eq!(
            dracula.colors[0],
            rgb_of(Some(Color::Rgb(0xbd, 0x93, 0xf9))).unwrap()
        );
        assert!(
            dracula.bg[0] < 0x28 as f32 / 255.0,
            "background is darkened"
        );
        assert_eq!(palette(&Theme::classic_blue()), Palette::default());
    }
}
