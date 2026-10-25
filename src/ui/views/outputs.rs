use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::{justify, truncate};
use crate::ui::app::App;
use crate::ui::keymap::{Action, Context};

/// The output picker: this computer, then the Sonos speakers and Cast devices found.
pub fn draw(f: &mut Frame<'_>, app: &App, area: Rect, cursor: usize) {
    let theme = &app.theme;
    let ell = app.symbols.ellipsis;
    let key = |action| app.keymap.hint(Context::Global, action);
    let hint = format!(
        " {} play here · {} close ",
        key(Action::Activate),
        key(Action::Close)
    );
    let devices = &app.devices;
    let active = app.cast.as_ref().map(|c| (c.device.id(), c.visuals()));
    // Name, what it is, and whether it plays now.
    let mut names = vec![("This computer".to_string(), "", active.is_none())];
    names.extend(app.output_targets().iter().map(|(d, visuals)| {
        let name = if *visuals {
            format!("{} + visuals", d.name())
        } else {
            d.name()
        };
        (name, d.kind(), active == Some((d.id(), *visuals)))
    }));

    let mut notes: Vec<(String, ratatui::style::Style)> = Vec::new();
    if devices.searching.is_some() {
        notes.push((format!("Searching for speakers and TVs{ell}"), theme.dim));
    } else if let Some(e) = &devices.error {
        notes.push((format!("Search: {e}"), theme.warning));
    } else if devices.searched && devices.list.is_empty() {
        notes.push(("No Sonos speakers or Cast devices found.".into(), theme.dim));
        notes.push((
            "Sonos elsewhere: list it under [outputs] sonos_hosts in config.toml.".into(),
            theme.dim,
        ));
    }

    let widest = names
        .iter()
        .map(|(n, _, _)| n.width() + 16)
        .chain(notes.iter().map(|(n, _)| n.width() + 2))
        .chain([hint.width() + 2, 34])
        .max()
        .unwrap_or(34);
    let width = (widest as u16 + 2).min(area.width);
    let height = (names.len() + notes.len() + 4) as u16;
    let rect = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height.min(area.height),
    );
    let block = Block::bordered()
        .border_set(app.symbols.border)
        .border_style(theme.title)
        .style(theme.base)
        .title(Line::styled(" Play on ", theme.title).centered())
        .title_bottom(Line::styled(hint, theme.dim).centered());
    let inner = block.inner(rect);
    f.render_widget(Clear, rect);
    f.render_widget(block, rect);
    let row_width = (inner.width as usize).saturating_sub(2);

    let cursor = cursor.min(names.len() - 1);
    let mut lines = vec![Line::default()];
    for (i, (name, kind, playing)) in names.iter().enumerate() {
        let marker = if i == cursor { app.symbols.cursor } else { " " };
        let tag = if *playing { "[playing]" } else { kind };
        let style = if i == cursor {
            theme.base.patch(theme.cursor)
        } else {
            theme.base
        };
        let body = justify(name, tag, row_width.saturating_sub(1), ell);
        lines.push(Line::styled(format!("{marker} {body}"), style));
    }
    if !notes.is_empty() {
        lines.push(Line::default());
    }
    for (note, style) in notes {
        lines.push(Line::styled(
            format!(" {}", truncate(&note, row_width, ell)),
            style,
        ));
    }
    f.render_widget(Paragraph::new(lines), inner);
}
