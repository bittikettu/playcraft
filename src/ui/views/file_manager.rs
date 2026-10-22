use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph};

use super::truncate;
use crate::library::fs_browser::EntryKind;
use crate::ui::app::App;
use crate::ui::file_manager::FmMode;

pub fn draw(f: &mut Frame<'_>, app: &mut App, area: Rect) {
    let theme = &app.theme;
    let fm = &mut app.fm;
    let title = match fm.mode {
        FmMode::AddFiles => " Add files ",
        FmMode::PickPlaylist => " Load playlist ",
    };
    let block = Block::bordered()
        .border_set(app.symbols.border)
        .border_style(theme.title)
        .style(theme.base)
        .title(Line::styled(title, theme.title))
        .title_bottom(Line::styled(
            if fm.selected.is_empty() {
                String::new()
            } else {
                format!(" {} selected ", fm.selected.len())
            },
            theme.selected,
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 {
        return;
    }
    let width = inner.width as usize;
    let ell = app.symbols.ellipsis;

    let header = fm.error.clone().unwrap_or_else(|| fm.location.display());
    let header_style = if fm.error.is_some() {
        theme.warning
    } else {
        theme.title
    };
    f.render_widget(
        Paragraph::new(Line::styled(truncate(&header, width, ell), header_style)),
        Rect { height: 1, ..inner },
    );

    let list = Rect {
        y: inner.y + 1,
        height: inner.height.saturating_sub(1),
        ..inner
    };
    let height = list.height as usize;
    fm.height = height;
    if fm.entries.is_empty() {
        f.render_widget(
            Paragraph::new(Line::styled("  (no audio files here)", theme.dim)),
            list,
        );
        return;
    }
    fm.cursor = fm.cursor.min(fm.entries.len() - 1);
    if fm.cursor < fm.scroll {
        fm.scroll = fm.cursor;
    } else if height > 0 && fm.cursor >= fm.scroll + height {
        fm.scroll = fm.cursor + 1 - height;
    }

    let mut lines = Vec::with_capacity(height);
    for (i, entry) in fm.entries.iter().enumerate().skip(fm.scroll).take(height) {
        let selected = fm.selected.contains(&entry.path);
        let marker = match (i == fm.cursor, selected) {
            (true, true) => ">*",
            (true, false) => "> ",
            (false, true) => " *",
            (false, false) => "  ",
        };
        let (tag, mut style) = match entry.kind {
            EntryKind::Parent => ("     ", theme.dim),
            EntryKind::Root => ("[DRV]", theme.group),
            EntryKind::Dir => ("[DIR]", theme.group),
            EntryKind::Playlist => ("[PLS]", theme.playing),
            EntryKind::Audio => ("     ", theme.base),
        };
        if selected {
            style = theme.selected;
        }
        if i == fm.cursor {
            style = style.patch(theme.cursor);
        }
        let text = format!("{marker}{tag} {}", entry.name);
        let text = truncate(&text, width, ell);
        let pad = width.saturating_sub(unicode_width::UnicodeWidthStr::width(text.as_str()));
        lines.push(Line::styled(format!("{text}{}", " ".repeat(pad)), style));
    }
    f.render_widget(Paragraph::new(lines), list);
}
