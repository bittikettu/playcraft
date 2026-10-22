use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::ui::app::App;

/// The message log, newest at the bottom; `scroll` counts lines back from the newest.
pub fn draw(f: &mut Frame<'_>, app: &App, area: Rect, scroll: u16) {
    let theme = &app.theme;
    let block = Block::bordered()
        .border_set(app.symbols.border)
        .border_style(theme.title)
        .style(theme.base)
        .title(
            Line::styled(
                " Message log — Up/Down to scroll, Esc to close ",
                theme.title,
            )
            .centered(),
        );
    let inner = block.inner(area);
    let entries = crate::log_ring::get()
        .map(|l| l.snapshot())
        .unwrap_or_default();
    let height = inner.height as usize;
    let end = entries.len().saturating_sub(scroll as usize);
    let start = end.saturating_sub(height);
    let lines: Vec<Line> = entries[start..end]
        .iter()
        .map(|e| {
            let style = match e.level {
                log::Level::Error => theme.error,
                log::Level::Warn => theme.warning,
                _ => theme.base,
            };
            Line::from(vec![
                Span::styled(format!("{:8.1}s ", e.elapsed_secs), theme.dim),
                Span::styled(format!("{:5} ", e.level), style),
                Span::styled(e.message.clone(), theme.base),
            ])
        })
        .collect();
    let lines = if lines.is_empty() {
        vec![Line::styled("No messages.", theme.dim)]
    } else {
        lines
    };
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(lines).block(block), area);
}
