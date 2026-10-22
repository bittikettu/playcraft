use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::ui::app::App;
use crate::ui::theme::{THEMES, Theme, supports_truecolor};

const HINT: &str = " Up/Down preview · Enter keep · Esc cancel ";

/// The theme list. Each row is drawn in its own theme's colours, with colour swatches.
pub fn draw(f: &mut Frame<'_>, app: &App, area: Rect, cursor: usize, original: &str) {
    let truecolor = supports_truecolor();
    let label_width = THEMES.iter().map(|t| t.label.width()).max().unwrap_or(10) + 2;
    let swatch = app.symbols.levels[7].repeat(2);
    let width = (label_width + 26).max(HINT.width() + 2) as u16 + 2;
    let height = THEMES.len() as u16 + 4;
    let rect = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width.min(area.width),
        height.min(area.height),
    );

    let theme = &app.theme;
    let block = Block::bordered()
        .border_set(app.symbols.border)
        .border_style(theme.title)
        .style(theme.base)
        .title(Line::styled(" Colour theme ", theme.title).centered())
        .title_bottom(Line::styled(HINT, theme.dim).centered());
    let inner_width = rect.width.saturating_sub(2) as usize;

    let mut lines = vec![Line::default()];
    for (i, info) in THEMES.iter().enumerate() {
        let (row_theme, _) = Theme::by_name(info.name, true);
        let marker = if i == cursor { app.symbols.cursor } else { " " };
        let label_style = if i == cursor {
            row_theme.cursor
        } else {
            row_theme.base
        };
        let mut spans = vec![
            Span::styled(format!(" {marker} "), row_theme.title),
            Span::styled(format!("{:<label_width$}", info.label), label_style),
        ];
        for style in [
            row_theme.title,
            row_theme.key,
            row_theme.playing,
            row_theme.group,
            row_theme.missing,
        ] {
            spans.push(Span::styled(swatch.clone(), style));
            spans.push(Span::styled(" ", row_theme.base));
        }
        let note = if info.truecolor && !truecolor {
            " needs truecolor"
        } else if info.name == original {
            " current"
        } else {
            ""
        };
        spans.push(Span::styled(note, row_theme.dim));
        let used: usize = spans.iter().map(|s| s.content.width()).sum();
        spans.push(Span::styled(
            " ".repeat(inner_width.saturating_sub(used)),
            row_theme.base,
        ));
        lines.push(Line::from(spans));
    }
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(block), rect);
}
