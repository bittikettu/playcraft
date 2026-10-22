use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use super::format_time;
use crate::ui::app::App;

pub fn draw(f: &mut Frame<'_>, app: &App, area: Rect, compact: bool) {
    let theme = &app.theme;
    let inner = if compact {
        area
    } else {
        let block = Block::bordered()
            .border_set(app.symbols.border)
            .border_style(theme.border)
            .style(theme.base);
        let inner = block.inner(area);
        f.render_widget(block, area);
        inner
    };
    let shared = app.audio.shared();
    let position = shared.position();
    let duration = app
        .now
        .as_ref()
        .and_then(|n| n.duration)
        .unwrap_or(shared.duration());
    let time = format!(" {} / {}", format_time(position), format_time(duration));

    let width = inner.width as usize;
    let bar_width = width.saturating_sub(time.len() + 2);
    let ratio = if duration.is_zero() || app.now.is_none() {
        0.0
    } else {
        (position.as_secs_f64() / duration.as_secs_f64()).clamp(0.0, 1.0)
    };
    let done = (ratio * bar_width as f64).round() as usize;
    let s = &app.symbols;
    let (done_str, head) = if done == 0 {
        (String::new(), String::new())
    } else if done >= bar_width {
        (s.progress_done.repeat(bar_width), String::new())
    } else {
        (
            s.progress_done.repeat(done - 1),
            s.progress_head.to_string(),
        )
    };
    let todo = bar_width - done.min(bar_width);
    let line = Line::from(vec![
        Span::styled("[", theme.border),
        Span::styled(done_str, theme.progress_done),
        Span::styled(head, theme.progress_done),
        Span::styled(s.progress_todo.repeat(todo), theme.progress_todo),
        Span::styled("]", theme.border),
        Span::styled(time, theme.title),
    ]);
    f.render_widget(Paragraph::new(line), inner);
}
