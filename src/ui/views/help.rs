use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::ui::app::App;
use crate::ui::keymap::{Action, Context};

pub fn draw(f: &mut Frame<'_>, app: &App, area: Rect, scroll: u16) {
    let theme = &app.theme;
    let block = Block::bordered()
        .border_set(app.symbols.border)
        .border_style(theme.title)
        .style(theme.base)
        .title(Line::styled(" Help — Up/Down to scroll, Esc to close ", theme.title).centered());
    let mut lines = Vec::new();
    for (heading, ctx) in [
        ("Playback and playlist", Context::Playlist),
        ("File manager", Context::FileManager),
    ] {
        lines.push(Line::styled(heading, theme.title));
        for &action in Action::ALL {
            let keys = app.keymap.keys_for(ctx, action);
            if keys.is_empty() {
                continue;
            }
            // Only list file-manager entries that differ from the playlist bindings.
            if ctx == Context::FileManager && keys == app.keymap.keys_for(Context::Playlist, action)
            {
                continue;
            }
            let keys = keys
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(Line::from(vec![
                Span::styled(format!("  {keys:<22}"), theme.key),
                Span::styled(action.description(), theme.base),
            ]));
        }
        lines.push(Line::default());
    }
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(lines).block(block).scroll((scroll, 0)), area);
}
