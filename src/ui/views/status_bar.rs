use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use super::truncate;
use crate::ui::app::{App, BannerLevel};
use crate::ui::keymap::Action;

pub fn draw(f: &mut Frame<'_>, app: &App, area: Rect, compact: bool) {
    let theme = &app.theme;
    let inner = if compact {
        area
    } else {
        let block = Block::bordered().border_style(theme.border).style(theme.base);
        let inner = block.inner(area);
        f.render_widget(block, area);
        inner
    };
    let width = inner.width as usize;

    if let Some(banner) = &app.banner {
        let style = match banner.level {
            BannerLevel::Info => theme.info,
            BannerLevel::Warning => theme.warning,
            BannerLevel::Error => theme.error,
        };
        let text = truncate(&banner.text, width, app.symbols.ellipsis);
        f.render_widget(Paragraph::new(Line::styled(text, style)), inner);
        return;
    }

    let ctx = app.context();
    let hints: &[(Action, &str)] = &[
        (Action::PlayPause, "Play/Pause"),
        (Action::Stop, "Stop"),
        (Action::NextTrack, "Next"),
        (Action::PrevTrack, "Prev"),
        (Action::SeekForward, "Seek"),
        (Action::VolumeUp, "Vol"),
        (Action::Activate, "Play"),
        (Action::Help, "Help"),
        (Action::Quit, "Quit"),
    ];
    let mut spans = Vec::new();
    let mut used = 0;
    for (action, label) in hints {
        let key = app.keymap.hint(ctx, *action);
        let piece = key.len() + label.len() + 3;
        if used + piece > width {
            break;
        }
        used += piece;
        spans.push(Span::styled(key, theme.key));
        spans.push(Span::styled(format!(":{label}  "), theme.dim));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), inner);
}
