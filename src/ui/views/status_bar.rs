use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use super::truncate;
use crate::ui::app::{App, BannerLevel, View};
use crate::ui::keymap::Action;

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
    let width = inner.width as usize;
    let ell = app.symbols.ellipsis;

    if let Some(prompt) = &app.prompt {
        let hint = if prompt.is_confirm() { " [y/N]" } else { "_" };
        let text = format!("{}{}{hint}", prompt.label, prompt.input);
        // Keep the end of long input visible.
        let skip = text.chars().count().saturating_sub(width);
        let text: String = text.chars().skip(skip).collect();
        f.render_widget(Paragraph::new(Line::styled(text, theme.key)), inner);
        return;
    }

    if let Some(banner) = &app.banner {
        let style = match banner.level {
            BannerLevel::Info => theme.info,
            BannerLevel::Warning => theme.warning,
            BannerLevel::Error => theme.error,
        };
        f.render_widget(
            Paragraph::new(Line::styled(truncate(&banner.text, width, ell), style)),
            inner,
        );
        return;
    }

    let ctx = app.context();
    let hints: &[(Action, &str)] = match app.view {
        View::Playlist => &[
            (Action::PlayPause, "Play/Pause"),
            (Action::Stop, "Stop"),
            (Action::NextTrack, "Next"),
            (Action::PrevTrack, "Prev"),
            (Action::SeekForward, "Seek"),
            (Action::VolumeUp, "Vol"),
            (Action::Activate, "Play"),
            (Action::Help, "Help"),
            (Action::Quit, "Quit"),
        ],
        View::FileManager => &[
            (Action::Select, "Select"),
            (Action::InvertSelection, "Invert"),
            (Action::SelectGlob, "Pattern"),
            (Action::AddSelected, "Add"),
            (Action::Activate, "Open"),
            (Action::Back, "Up"),
            (Action::ToggleHidden, "Hidden"),
            (Action::Close, "Close"),
        ],
    };
    let mut spans = Vec::new();
    let mut used = 0;
    if app.is_scanning() {
        spans.push(Span::styled("[scanning] ", theme.warning));
        used += 11;
    }
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
