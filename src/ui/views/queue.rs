use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, Paragraph};

use super::{format_time, justify};
use crate::ui::app::App;
use crate::ui::keymap::{Action, Context};

/// The play queue list.
pub fn draw(f: &mut Frame<'_>, app: &App, area: Rect, cursor: usize) {
    let theme = &app.theme;
    let key = |action| app.keymap.hint(Context::Playlist, action);
    let hint = format!(
        " {} play now · {}/{} move · {} remove · {} clear · {} close ",
        key(Action::Activate),
        key(Action::MoveUp),
        key(Action::MoveDown),
        key(Action::Delete),
        key(Action::ClearQueue),
        key(Action::Close),
    );
    let block = Block::bordered()
        .border_set(app.symbols.border)
        .border_style(theme.title)
        .style(theme.base)
        .title(Line::styled(format!(" Play queue ({}) ", app.queue.len()), theme.title).centered())
        .title_bottom(Line::styled(hint, theme.dim).centered());
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);
    let width = inner.width as usize;
    let height = inner.height as usize;
    if height == 0 {
        return;
    }

    let mut lines = Vec::new();
    if app.queue.is_empty() {
        lines.push(Line::styled(
            format!(
                "  The queue is empty. Press {} on a track or group to queue it.",
                key(Action::Enqueue)
            ),
            theme.dim,
        ));
    } else {
        // One line is kept for the note under the list.
        let rows = height.saturating_sub(1).max(1);
        let cursor = cursor.min(app.queue.len() - 1);
        let first = cursor.saturating_sub(rows - 1);
        for (i, &uid) in app.queue.iter().enumerate().skip(first).take(rows) {
            let Some(track) = app.root.track_by_uid(uid) else {
                continue;
            };
            let marker = if i == cursor { app.symbols.cursor } else { " " };
            let number = format!("{marker}{:>2}. ", i + 1);
            let right = track
                .duration()
                .map(|d| format!("[{}]", format_time(d)))
                .unwrap_or_default();
            let body = justify(
                &track.display_name(),
                &right,
                width.saturating_sub(number.len()),
                app.symbols.ellipsis,
            );
            let style = if i == cursor {
                theme.base.patch(theme.cursor)
            } else {
                theme.base
            };
            lines.push(Line::styled(format!("{number}{body}"), style));
        }
    }
    lines.push(Line::styled(
        "  Afterwards playback continues in normal order.",
        theme.dim,
    ));
    f.render_widget(Paragraph::new(lines), inner);
}
