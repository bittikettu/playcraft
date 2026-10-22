use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use super::{format_time, justify};
use crate::model::TrackRef;
use crate::ui::app::{App, Row};
use crate::ui::keymap::{Action, Context};

/// Draws the current group. `focused` is false when it is the read-only side pane of the
/// file manager.
pub fn draw(f: &mut Frame<'_>, app: &mut App, area: Rect, focused: bool) {
    let theme = &app.theme;
    let km = &app.keymap;
    let hints = [
        (Action::FileManager, "Files"),
        (Action::NewGroup, "Add Group"),
        (Action::LoadPlaylist, "Load"),
        (Action::Help, "Help"),
    ];
    let mut title = vec![Span::styled(" ", theme.border)];
    if focused {
        for (action, label) in hints {
            title.push(Span::styled(format!("[{}]", km.hint(Context::Playlist, action)), theme.key));
            title.push(Span::styled(format!(" {label}  "), theme.title));
        }
    } else {
        title.push(Span::styled("Playlist ", theme.title));
    }
    let block = Block::bordered().border_style(theme.border).style(theme.base).title(Line::from(title));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let group = app.current_group();
    let crumb = app.breadcrumb().join(app.symbols.breadcrumb);
    let header = format!("Playlist: {crumb} ({} tracks)", group.total_tracks());
    let width = inner.width as usize;
    f.render_widget(
        Paragraph::new(Line::styled(super::truncate(&header, width, app.symbols.ellipsis), theme.title)),
        Rect { height: 1, ..inner },
    );

    let list = Rect { y: inner.y + 1, height: inner.height.saturating_sub(1), ..inner };
    let height = list.height as usize;
    if focused {
        app.list_height = height;
    }
    let rows = app.rows();
    if rows.is_empty() {
        let hint = format!(
            "  Empty. Press {} to add files.",
            app.keymap.hint(Context::Playlist, Action::FileManager)
        );
        f.render_widget(Paragraph::new(Line::styled(hint, theme.dim)), list);
        return;
    }
    app.cursor = app.cursor.min(rows.len() - 1);
    if app.cursor < app.scroll {
        app.scroll = app.cursor;
    } else if height > 0 && app.cursor >= app.scroll + height {
        app.scroll = app.cursor + 1 - height;
    }

    let group = app.current_group();
    let digits = rows.len().to_string().len().max(2);
    let mut lines = Vec::with_capacity(height);
    for (i, row) in rows.iter().enumerate().skip(app.scroll).take(height) {
        let is_cursor = focused && i == app.cursor;
        let marker = if is_cursor { app.symbols.cursor } else { " " };
        let number = format!("{marker}{:0digits$}. ", i + 1);
        let (text, right, mut style) = match *row {
            Row::Group(g) => {
                let sub = &group.subgroups[g];
                let shuffle = if sub.shuffle { " ~shuffle" } else { "" };
                (format!("[GRP] {}{shuffle}", sub.name), format!("({})", sub.total_tracks()), theme.group)
            }
            Row::Track(t) => {
                let track = &group.tracks[t];
                let right = if track.missing {
                    "[MISSING]".to_string()
                } else if track.broken {
                    "[BAD]".to_string()
                } else {
                    track.duration().map(|d| format!("[{}]", format_time(d))).unwrap_or_default()
                };
                let here = TrackRef::new(app.cwd.clone(), t);
                let style = if !track.is_playable() {
                    theme.missing
                } else if app.playing.as_ref() == Some(&here) {
                    theme.playing
                } else {
                    theme.base
                };
                (track.display_name(), right, style)
            }
        };
        if is_cursor {
            style = style.patch(theme.cursor);
        }
        let body = justify(&text, &right, width.saturating_sub(number.len()), app.symbols.ellipsis);
        lines.push(Line::styled(format!("{number}{body}"), style));
    }
    f.render_widget(Paragraph::new(lines), list);
}
