//! Rendering. The layout follows spec §5: playlist (left), Now Playing (right),
//! progress bar and a contextual key/prompt bar.

mod defrag;
mod file_manager;
mod help;
mod hyperspace;
mod log;
mod now_playing;
mod playlist;
mod progress;
mod queue;
mod spectrum;
mod status_bar;
mod themes;

use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::Block;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::app::{App, Overlay, SpectrumLayout, View};

/// The Now Playing pane takes a third of the width within these limits; it is hidden on
/// terminals narrower than `SIDE_MIN_WIDTH`.
const SIDE_WIDTH: u16 = 28;
const SIDE_MAX_WIDTH: u16 = 48;
const SIDE_MIN_WIDTH: u16 = 80;
/// Main-area height needed before the spectrum gets its own full-width strip.
const STRIP_MIN_MAIN: u16 = 18;

pub fn draw(f: &mut Frame<'_>, app: &mut App) {
    let area = f.area();
    f.render_widget(Block::new().style(app.theme.base), area);
    // Set again below if Now Playing is on screen with scrolling text.
    app.marquee_scrolling = false;

    let compact = area.height < 14;
    let bar_height = if compact { 1 } else { 3 };
    let [main, progress_area, bar_area] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(bar_height),
        Constraint::Length(bar_height),
    ])
    .areas(area);

    match app.view {
        View::Playlist => {
            // The wide spectrum strip needs room for the panes above it; otherwise the
            // spectrum moves into Now Playing.
            let wide = app.spectrum_layout == SpectrumLayout::Wide && main.height >= STRIP_MIN_MAIN;
            let (panes, strip) = if wide {
                let height = (main.height / 3).clamp(6, 14);
                let [panes, strip] =
                    Layout::vertical([Constraint::Fill(1), Constraint::Length(height)]).areas(main);
                (panes, Some(strip))
            } else {
                (main, None)
            };
            let pane_spectrum = !wide && app.spectrum_layout != SpectrumLayout::Off;
            if panes.width >= SIDE_MIN_WIDTH {
                let [list, side] = Layout::horizontal([
                    Constraint::Fill(1),
                    Constraint::Length(side_width(panes.width)),
                ])
                .areas(panes);
                playlist::draw(f, app, list, true);
                if pane_spectrum {
                    app.spectrum.set_band_count(spectrum::PANE_BANDS);
                }
                app.marquee_scrolling = now_playing::draw(f, app, side, pane_spectrum);
            } else {
                playlist::draw(f, app, panes, true);
            }
            if let Some(strip) = strip {
                spectrum::draw_strip(f, app, strip);
            }
        }
        View::Visualizer => spectrum::draw_fullscreen(f, app, main),
        View::FileManager if main.width >= 60 => {
            let [files, list] =
                Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).areas(main);
            file_manager::draw(f, app, files);
            playlist::draw(f, app, list, false);
        }
        View::FileManager => file_manager::draw(f, app, main),
    }
    progress::draw(f, app, progress_area, compact);
    status_bar::draw(f, app, bar_area, compact);

    match app.overlay {
        Overlay::Help { scroll } => help::draw(f, app, centered(area, 80, 85), scroll),
        Overlay::Log { scroll } => log::draw(f, app, centered(area, 90, 85), scroll),
        Overlay::Themes { cursor, original } => themes::draw(f, app, area, cursor, original),
        Overlay::Queue { cursor } => queue::draw(f, app, centered(area, 70, 70), cursor),
        Overlay::None => {}
    }
}

fn side_width(total: u16) -> u16 {
    (total / 3).clamp(SIDE_WIDTH, SIDE_MAX_WIDTH)
}

/// Number of bands drawn inside the Now Playing pane (for tests).
#[cfg(test)]
pub(crate) fn spectrum_pane_bands() -> usize {
    spectrum::PANE_BANDS
}

/// A rectangle of `pct_w` x `pct_h` percent centred in `area`.
fn centered(area: Rect, pct_w: u16, pct_h: u16) -> Rect {
    let w = (area.width * pct_w / 100).max(20).min(area.width);
    let h = (area.height * pct_h / 100).max(5).min(area.height);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

/// `mm:ss`, or `h:mm:ss` from one hour up.
pub fn format_time(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

/// Truncates `s` to at most `width` terminal columns, ending with `ellipsis` when cut.
pub fn truncate(s: &str, width: usize, ellipsis: &str) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    let budget = width.saturating_sub(ellipsis.width());
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > budget {
            break;
        }
        used += w;
        out.push(c);
    }
    if width >= ellipsis.width() {
        out.push_str(ellipsis);
    }
    out
}

/// `s` in `width` columns. Text that does not fit scrolls: `step` columns in, looping with
/// `gap` between its end and start. At step 0 it is the start, cut off with `ellipsis`.
pub fn marquee(s: &str, width: usize, step: u64, gap: &str, ellipsis: &str) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    if step == 0 {
        return truncate(s, width, ellipsis);
    }
    let lap = format!("{s}{gap}");
    let offset = (step % lap.width() as u64) as usize;
    let mut out = String::new();
    let (mut col, mut used) = (0, 0);
    // Two laps always cover `offset + width` columns, since `width` is less than a lap.
    for c in lap.chars().chain(lap.chars()) {
        let w = c.width().unwrap_or(0);
        let start = col;
        col += w;
        if col <= offset {
            continue;
        }
        if start < offset {
            // The right half of a wide character cut at the left edge.
            let visible = col - offset;
            out.extend(std::iter::repeat_n(' ', visible));
            used += visible;
            continue;
        }
        if used + w > width {
            break;
        }
        out.push(c);
        used += w;
    }
    // A wide character cut at the right edge leaves a column free.
    out.extend(std::iter::repeat_n(' ', width - used));
    out
}

/// `left` and `right` on one line of exactly `width` columns, truncating `left` as needed.
pub fn justify(left: &str, right: &str, width: usize, ellipsis: &str) -> String {
    let right_w = right.width();
    if right_w + 1 > width {
        return truncate(left, width, ellipsis);
    }
    let left = truncate(left, width - right_w - 1, ellipsis);
    let pad = width - right_w - left.width();
    format!("{left}{}{right}", " ".repeat(pad))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_time() {
        assert_eq!(format_time(Duration::from_secs(0)), "00:00");
        assert_eq!(format_time(Duration::from_secs(295)), "04:55");
        assert_eq!(format_time(Duration::from_secs(3725)), "1:02:05");
    }

    #[test]
    fn truncates_by_display_width() {
        assert_eq!(truncate("abcdef", 10, "…"), "abcdef");
        assert_eq!(truncate("abcdef", 4, "…"), "abc…");
        // Wide CJK characters take two columns each.
        assert_eq!(truncate("日本語の歌", 5, "…"), "日本…");
        assert_eq!(truncate("abc", 0, "…"), "");
    }

    #[test]
    fn marquee_scrolls_only_text_that_does_not_fit() {
        assert_eq!(marquee("short", 10, 7, " | ", "…"), "short");
        assert_eq!(marquee("abcdefgh", 5, 0, " | ", "…"), "abcd…");
        assert_eq!(marquee("abcdefgh", 5, 1, " | ", "…"), "bcdef");
        assert_eq!(marquee("abcdefgh", 5, 6, " | ", "…"), "gh | ");
        // It loops: the lap is 11 columns ("abcdefgh | ").
        assert_eq!(marquee("abcdefgh", 5, 9, " | ", "…"), "| abc");
        assert_eq!(marquee("abcdefgh", 5, 11, " | ", "…"), "abcde");
        assert_eq!(marquee("abcdefgh", 5, 12, " | ", "…"), "bcdef");
    }

    #[test]
    fn marquee_keeps_wide_characters_aligned() {
        // Each CJK character takes two columns.
        for step in 0..30 {
            let s = marquee("日本語の歌です", 5, step, " · ", "…");
            assert_eq!(s.width(), 5, "step {step}: {s:?}");
        }
        assert_eq!(marquee("日本語の歌です", 5, 1, " · ", "…"), " 本語");
    }

    #[test]
    fn justifies_to_exact_width() {
        let s = justify("Led Zeppelin - Black Dog", "[04:55]", 20, "…");
        assert_eq!(s.width(), 20);
        assert!(s.ends_with("[04:55]"));
        assert_eq!(justify("short", "[1]", 10, "…"), "short  [1]");
    }
}
