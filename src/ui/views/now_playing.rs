use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use unicode_width::UnicodeWidthStr;

use super::{marquee, spectrum};
use crate::ui::app::App;

/// Draws track info and status; with_spectrum adds small spectrum bars in the free space.
/// Values too long for the pane scroll. Returns whether any did.
pub fn draw(f: &mut Frame<'_>, app: &App, area: Rect, with_spectrum: bool) -> bool {
    let theme = &app.theme;
    let block = Block::bordered()
        .border_set(app.symbols.border)
        .border_style(theme.border)
        .style(theme.base)
        .title(Line::styled(" Now Playing ", theme.title).centered());
    let inner = block.inner(area);
    f.render_widget(block, area);
    let width = inner.width as usize;
    let ell = app.symbols.ellipsis;

    let step = app.marquee_step();
    let mut scrolling = false;
    let mut field = |label: &str, value: String| -> Line<'static> {
        let room = width.saturating_sub(label.len());
        scrolling |= value.width() > room;
        let value = marquee(&value, room, step, app.symbols.marquee_gap, ell);
        Line::from(vec![
            Span::styled(label.to_string(), theme.dim),
            Span::styled(value, theme.base),
        ])
    };

    let track = app
        .now
        .as_ref()
        .and_then(|n| n.track)
        .and_then(|u| app.root.track_by_uid(u));
    let meta = track.and_then(|t| t.metadata.as_ref());
    let mut lines = Vec::new();
    match &app.now {
        Some(now) => {
            let title = meta
                .and_then(|m| m.title.clone())
                .or_else(|| {
                    now.path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                })
                .unwrap_or_default();
            lines.push(field("Title:  ", title));
            lines.push(field(
                "Artist: ",
                meta.and_then(|m| m.artist.clone()).unwrap_or_default(),
            ));
            let album = match meta {
                Some(m) => match (&m.album, m.year) {
                    (Some(a), Some(y)) => format!("{a} ({y})"),
                    (Some(a), None) => a.clone(),
                    (None, Some(y)) => y.to_string(),
                    (None, None) => String::new(),
                },
                None => String::new(),
            };
            lines.push(field("Album:  ", album));
            let khz = f64::from(now.info.sample_rate) / 1000.0;
            let mut format = format!("{} {khz}k", now.info.codec.to_uppercase());
            if let Some(kbps) = meta.map(|m| m.bitrate_kbps).filter(|&b| b > 0) {
                format = format!("{kbps}kbps {khz}k");
            }
            lines.push(field("Format: ", format));
        }
        None => {
            for label in ["Title:  ", "Artist: ", "Album:  ", "Format: "] {
                lines.push(field(label, String::new()));
            }
        }
    }
    if let Some(cast) = &app.cast {
        let mut on = cast.device.name();
        if cast.state() == Some(crate::outputs::TransportState::Transitioning) {
            on.push_str(" (buffering)");
        }
        lines.push(field("On:     ", on));
    }
    // What plays next: the gapless successor, or the head of the queue while stopped.
    let next = app
        .pending_next
        .or_else(|| app.queue.first().copied())
        .and_then(|u| app.root.track_by_uid(u));
    if let Some(next) = next {
        lines.push(field("Next:   ", next.display_name()));
    }

    // Bottom-anchored status lines.
    let status = app.status();
    let status_style = match status {
        crate::model::PlaybackStatus::Playing => theme.playing,
        crate::model::PlaybackStatus::Paused => theme.warning,
        crate::model::PlaybackStatus::Stopped => theme.dim,
    };
    let shuffle = if app.current_group().shuffle {
        "ON"
    } else {
        "OFF"
    };
    let volume = app.audio.shared().volume();
    let bar_width = width.saturating_sub(12).clamp(4, 10);
    let filled = (volume * bar_width as f32).round() as usize;
    let bar = format!(
        "[{}{}]",
        app.symbols.volume_on.repeat(filled),
        app.symbols.volume_off.repeat(bar_width - filled)
    );
    let footer = vec![
        Line::from(vec![
            Span::styled("Status: ", theme.dim),
            Span::styled(format!("[{}]", status.label()), status_style),
        ]),
        field("Mode:   ", app.play_mode.label().to_string()),
        field(
            "Shuf: ",
            format!("{shuffle:<4} Rep: {}", app.repeat.label()),
        ),
        Line::from(vec![
            Span::styled("Vol: ", theme.dim),
            Span::styled(bar, theme.progress_done),
            Span::styled(format!(" {:.0}%", volume * 100.0), theme.base),
        ]),
    ];

    let height = inner.height as usize;
    let top = lines.len().min(height.saturating_sub(footer.len()));
    lines.truncate(top);
    let mut gap = height.saturating_sub(top + footer.len());
    if with_spectrum && gap >= 2 {
        let rows = (gap - 1).min(6);
        lines.push(Line::default());
        let geo = spectrum::Geometry::new(width, spectrum::PANE_BANDS);
        lines.extend(spectrum::bar_lines(app, geo, rows, false));
        gap -= rows + 1;
    }
    lines.extend(std::iter::repeat_n(Line::default(), gap));
    lines.extend(footer.into_iter().take(height.saturating_sub(top)));
    f.render_widget(Paragraph::new(lines), inner);
    scrolling
}
