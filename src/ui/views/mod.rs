//! Rendering. The layout follows spec §5: playlist (left), Now Playing (right),
//! progress bar and a contextual key/prompt bar.

mod help;
mod now_playing;
mod playlist;
mod progress;
mod status_bar;

use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::Block;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::app::{App, Overlay};

/// Width of the Now Playing pane; it is hidden on terminals narrower than `SIDE_MIN_WIDTH`.
const SIDE_WIDTH: u16 = 28;
const SIDE_MIN_WIDTH: u16 = 80;

pub fn draw(f: &mut Frame<'_>, app: &mut App) {
    let area = f.area();
    f.render_widget(Block::new().style(app.theme.base), area);

    let compact = area.height < 14;
    let bar_height = if compact { 1 } else { 3 };
    let [main, progress_area, bar_area] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(bar_height),
        Constraint::Length(bar_height),
    ])
    .areas(area);

    if main.width >= SIDE_MIN_WIDTH {
        let [list, side] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(SIDE_WIDTH)]).areas(main);
        playlist::draw(f, app, list);
        now_playing::draw(f, app, side);
    } else {
        playlist::draw(f, app, main);
    }
    progress::draw(f, app, progress_area, compact);
    status_bar::draw(f, app, bar_area, compact);

    if let Overlay::Help { scroll } = app.overlay {
        help::draw(f, app, centered(area, 80, 85), scroll);
    }
}

/// A rectangle of `pct_w` x `pct_h` percent centred in `area`.
fn centered(area: Rect, pct_w: u16, pct_h: u16) -> Rect {
    let w = (area.width * pct_w / 100).max(20).min(area.width);
    let h = (area.height * pct_h / 100).max(5).min(area.height);
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

/// `mm:ss`, or `h:mm:ss` from one hour up.
pub fn format_time(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{:02}:{:02}", s / 60, s % 60) }
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
    fn justifies_to_exact_width() {
        let s = justify("Led Zeppelin - Black Dog", "[04:55]", 20, "…");
        assert_eq!(s.width(), 20);
        assert!(s.ends_with("[04:55]"));
        assert_eq!(justify("short", "[1]", 10, "…"), "short  [1]");
    }
}
