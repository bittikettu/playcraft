//! Spectrum bars: the wide strip under the panes, the fullscreen visualizer, and the small
//! version inside Now Playing.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use ratatui::style::Modifier;

use super::truncate;
use crate::matrix::Lit;
use crate::spectrum::{MAX_BANDS, band_position};
use crate::ui::app::{App, Visualizer};
use crate::ui::keymap::{Action, Context};

fn human_size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.0} KB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}

/// The Matrix rain: falling hex digits of the playing file over a faint spectrum made of
/// the same digits.
fn matrix_lines(app: &App) -> Vec<Line<'static>> {
    let (width, height) = app.rain.size();
    let bands = app.spectrum.bands();
    let theme = &app.theme;
    let head = theme.base.add_modifier(Modifier::BOLD);
    let near = theme.info.add_modifier(Modifier::BOLD);
    let faint = |s: Style| s.add_modifier(Modifier::DIM);
    (0..height)
        .map(|y| {
            let from_bottom = height - 1 - y;
            let bar_style = faint(row_style(app, from_bottom, height));
            let mut spans: Vec<Span<'static>> = Vec::new();
            let mut run = String::new();
            let mut run_style = theme.base;
            for x in 0..width {
                let level = if bands.is_empty() {
                    0.0
                } else {
                    bands[x * bands.len() / width]
                };
                let in_bar = (from_bottom as f32) < (level * height as f32).round();
                let (ch, style) = match app.rain.lit(x, y) {
                    Some(Lit::Head) => (app.rain.glyph(x, y), head),
                    Some(Lit::Trail(t)) if t < 0.3 => (app.rain.glyph(x, y), near),
                    Some(Lit::Trail(t)) if t < 0.7 => (app.rain.glyph(x, y), theme.info),
                    Some(Lit::Trail(_)) => (app.rain.glyph(x, y), theme.dim),
                    None if in_bar => (app.rain.glyph(x, y), bar_style),
                    None => (' ', theme.base),
                };
                if style != run_style && !run.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut run), run_style));
                }
                run_style = style;
                run.push(ch);
            }
            if !run.is_empty() {
                spans.push(Span::styled(run, run_style));
            }
            Line::from(spans)
        })
        .collect()
}

/// Bands shown inside the Now Playing pane.
pub const PANE_BANDS: usize = 16;
const AXIS_LABELS: &[(f32, &str)] = &[
    (50.0, "50"),
    (100.0, "100"),
    (200.0, "200"),
    (500.0, "500"),
    (1000.0, "1k"),
    (2000.0, "2k"),
    (5000.0, "5k"),
    (10_000.0, "10k"),
];

/// How `n` bars fit into `width` columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub bands: usize,
    pub bar: usize,
    pub gap: usize,
    pub pad: usize,
}

impl Geometry {
    pub fn new(width: usize, bands: usize) -> Self {
        let n = bands.clamp(1, width.max(1));
        let (bar, gap) = if width + 1 >= n * 3 {
            ((width + 1) / n - 1, 1)
        } else if width + 1 >= n * 2 {
            (1, 1)
        } else {
            (1, 0)
        };
        let used = n * bar + (n - 1) * gap;
        Geometry {
            bands: n,
            bar,
            gap,
            pad: width.saturating_sub(used) / 2,
        }
    }

    /// As many 2-column bars (with 1-column gaps) as fit, but at least 16.
    pub fn wide(width: usize) -> Self {
        Self::new(width, ((width + 1) / 3).clamp(PANE_BANDS, MAX_BANDS))
    }

    /// Column of the left edge of band position `pos` (fractional bands allowed).
    fn column(&self, pos: f32) -> usize {
        self.pad + (pos * (self.bar + self.gap) as f32).round() as usize
    }
}

/// Gradient colour for a row `from_bottom` of `rows`.
fn row_style(app: &App, from_bottom: usize, rows: usize) -> Style {
    if rows < 3 {
        return app.theme.spectrum;
    }
    let frac = (from_bottom as f32 + 0.5) / rows as f32;
    if frac > 0.75 {
        app.theme.spectrum_hot
    } else if frac > 0.45 {
        app.theme.spectrum_mid
    } else {
        app.theme.spectrum
    }
}

/// The bars as `rows` lines of text. `peaks` adds the falling peak markers.
pub fn bar_lines(app: &App, geo: Geometry, rows: usize, peaks: bool) -> Vec<Line<'static>> {
    let bands = app.spectrum.bands();
    let peak_levels = app.spectrum.peaks();
    let levels = app.symbols.levels;
    let cap = if app.symbols.ellipsis.is_ascii() {
        "-"
    } else {
        "▔"
    };
    let base = app.theme.base;
    let eighths = |level: f32| (level * (rows * 8) as f32).round() as isize;
    (0..rows)
        .map(|row| {
            let from_bottom = rows - 1 - row;
            let style = row_style(app, from_bottom, rows);
            let mut spans = vec![Span::styled(" ".repeat(geo.pad), base)];
            for i in 0..geo.bands {
                let level = bands.get(i).copied().unwrap_or(0.0);
                let fill = eighths(level) - (from_bottom * 8) as isize;
                let cell = match fill {
                    e if e >= 8 => Some(levels[7]),
                    e if e <= 0 => None,
                    e => Some(levels[e as usize - 1]),
                };
                let text = match cell {
                    Some(c) => Span::styled(c.repeat(geo.bar), style),
                    None => {
                        // A peak marker sits in the cell holding the peak, above the bar.
                        let peak = eighths(peak_levels.get(i).copied().unwrap_or(0.0));
                        let peak_cell = (peak - 1).div_euclid(8);
                        if peaks && peak > 0 && peak_cell == from_bottom as isize {
                            Span::styled(cap.repeat(geo.bar), app.theme.spectrum_hot)
                        } else {
                            Span::styled(" ".repeat(geo.bar), base)
                        }
                    }
                };
                spans.push(text);
                if geo.gap > 0 && i + 1 < geo.bands {
                    spans.push(Span::styled(" ".repeat(geo.gap), base));
                }
            }
            Line::from(spans)
        })
        .collect()
}

/// Frequency labels under the bars.
fn axis_line(app: &App, geo: Geometry, width: usize) -> Line<'static> {
    let mut chars = vec![' '; width];
    let mut free_from = 0;
    for &(hz, label) in AXIS_LABELS {
        let center = geo.column(band_position(hz, geo.bands)) + geo.bar / 2;
        let start = center.saturating_sub(label.len() / 2);
        if start < free_from || start + label.len() > width {
            continue;
        }
        for (i, c) in label.chars().enumerate() {
            chars[start + i] = c;
        }
        free_from = start + label.len() + 1;
    }
    Line::styled(chars.into_iter().collect::<String>(), app.theme.dim)
}

fn hint(app: &App) -> String {
    let km = &app.keymap;
    format!(
        " {}: fullscreen · {}: layout ",
        km.hint(Context::Playlist, Action::SpectrumFullscreen),
        km.hint(Context::Playlist, Action::SpectrumLayout)
    )
}

/// Bars and (when there is room) the frequency axis inside `inner`.
fn draw_bars(f: &mut Frame<'_>, app: &mut App, inner: Rect) {
    let width = inner.width as usize;
    let height = inner.height as usize;
    if width == 0 || height == 0 {
        return;
    }
    let geo = Geometry::wide(width);
    app.spectrum.set_band_count(geo.bands);
    if app.visualizer == Visualizer::Defrag {
        super::defrag::draw(f, app, inner);
        return;
    }
    if app.visualizer == Visualizer::Matrix {
        app.rain.resize(width, height);
        f.render_widget(Paragraph::new(matrix_lines(app)), inner);
        return;
    }
    let with_axis = height >= 4;
    let rows = if with_axis { height - 1 } else { height };
    let mut lines = bar_lines(app, geo, rows, rows >= 3);
    if with_axis {
        lines.push(axis_line(app, geo, width));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// The full-width strip under the playlist and Now Playing panes.
pub fn draw_strip(f: &mut Frame<'_>, app: &mut App, area: Rect) {
    let title = match app.visualizer {
        Visualizer::Defrag => super::defrag::status(app),
        _ => " Spectrum ".to_string(),
    };
    let block = Block::bordered()
        .border_set(app.symbols.border)
        .border_style(app.theme.border)
        .style(app.theme.base)
        .title(Line::styled(title, app.theme.title))
        .title(Line::styled(hint(app), app.theme.dim).right_aligned());
    let inner = block.inner(area);
    f.render_widget(block, area);
    draw_bars(f, app, inner);
}

/// The fullscreen visualizer: the track in the title, bars filling the rest.
pub fn draw_fullscreen(f: &mut Frame<'_>, app: &mut App, area: Rect) {
    let now = app.now.as_ref().map(|now| {
        let meta = now
            .track
            .and_then(|u| app.root.track_by_uid(u))
            .and_then(|t| t.metadata.as_ref());
        let title = meta.and_then(|m| m.title.clone()).unwrap_or_else(|| {
            now.path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        match meta.and_then(|m| m.artist.clone()) {
            Some(artist) => format!(" {artist} - {title} "),
            None => format!(" {title} "),
        }
    });
    let key = app
        .keymap
        .hint(Context::Playlist, Action::SpectrumFullscreen);
    // In Matrix mode, show where in the file the falling bytes come from.
    let back = match (app.visualizer, app.rain.feed.offset()) {
        (Visualizer::Matrix, Some(offset)) => format!(
            " bytes 0x{offset:08X} of {} · {key}/Esc: back ",
            human_size(app.rain.feed.file_len())
        ),
        _ => format!(" {key}/Esc: back "),
    };
    let (title, back) = if app.visualizer == Visualizer::Defrag {
        // Defrag: the defragmenter status on the left, the track next to the key hint.
        let status = super::defrag::status(app);
        let room = (area.width as usize).saturating_sub(status.len() + back.len() + 6);
        let track = truncate(now.as_deref().unwrap_or(""), room, app.symbols.ellipsis);
        let right = if track.trim().is_empty() {
            back
        } else {
            format!("{track}·{back}")
        };
        (status, right)
    } else {
        let title_room = (area.width as usize).saturating_sub(back.len() + 4);
        let title = truncate(
            now.as_deref().unwrap_or(" Nothing playing "),
            title_room,
            app.symbols.ellipsis,
        );
        (title, back)
    };
    let block = Block::bordered()
        .border_set(app.symbols.border)
        .border_style(app.theme.border)
        .style(app.theme.base)
        .title(Line::styled(title, app.theme.title))
        .title(Line::styled(back, app.theme.dim).right_aligned());
    let inner = block.inner(area);
    f.render_widget(block, area);
    // A blank row above the bars keeps peak markers off the border.
    let bars = Rect {
        y: inner.y + 1,
        height: inner.height.saturating_sub(1),
        ..inner
    };
    draw_bars(f, app, bars);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_fills_the_width() {
        for width in [20usize, 26, 47, 80, 118, 200, 400] {
            let g = Geometry::wide(width);
            let used = g.bands * g.bar + (g.bands - 1) * g.gap;
            assert!(used <= width, "{width}: {g:?}");
            assert!(width - used <= 2 * g.pad + 1, "{width}: {g:?}");
            if width >= 47 {
                assert!(g.bar >= 2 && g.gap == 1, "{width}: {g:?}");
            }
        }
        assert_eq!(
            Geometry::new(26, 16),
            Geometry {
                bands: 16,
                bar: 1,
                gap: 0,
                pad: 5
            }
        );
        assert_eq!(Geometry::new(31, 16).gap, 1);
    }
}
