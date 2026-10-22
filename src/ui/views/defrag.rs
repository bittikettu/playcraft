//! The disk-defragmenter visualizer: a grid of clusters that is fully defragmented when
//! the song ends, with the classic legend underneath.

use std::hash::{Hash, Hasher};
use std::path::{Component, Path};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::defrag::ClusterKind;
use crate::ui::app::App;

/// Each cluster takes two columns: the block and a gap, like the old grid of squares.
const CELL_WIDTH: usize = 2;

/// "Drive D:" for a Windows path, "Drive /" elsewhere.
pub fn drive_label(path: Option<&Path>) -> String {
    match path.and_then(|p| p.components().next()) {
        Some(Component::Prefix(prefix)) => {
            let s = prefix.as_os_str().to_string_lossy().into_owned();
            format!(
                "Drive {}",
                s.trim_start_matches(r"\\?\").trim_end_matches('\\')
            )
        }
        _ => "Drive /".into(),
    }
}

/// Title text: "Defragmenting Drive D:  43% complete", or the completion message.
pub fn status(app: &App) -> String {
    let drive = drive_label(app.now.as_ref().map(|n| n.path.as_path()));
    if app.now.is_none() {
        format!(" Disk Defragmenter · {drive} · waiting for a track ")
    } else if app.defrag.is_complete() {
        format!(" Defragmentation of {drive} is complete. ")
    } else {
        format!(
            " Defragmenting {drive}  {}% complete ",
            app.defrag.percent()
        )
    }
}

fn style_of(app: &App, kind: ClusterKind) -> Style {
    let t = &app.theme;
    match kind {
        ClusterKind::Free => t.dim,
        ClusterKind::Fragmented => t.missing,
        ClusterKind::Unoptimized => t.group,
        ClusterKind::Optimized => t.title,
        ClusterKind::Reading => t.playing.add_modifier(Modifier::BOLD),
        ClusterKind::Writing => t.warning.add_modifier(Modifier::BOLD),
    }
}

fn glyph(app: &App, kind: ClusterKind) -> &'static str {
    let ascii = app.symbols.ellipsis.is_ascii();
    match (ascii, kind) {
        (true, ClusterKind::Free) => ".",
        (true, _) => "#",
        // A lower three-quarter block leaves a thin gap between rows: square-ish clusters.
        (false, _) => "▆",
    }
}

/// Seeds the disk layout from the track, so every song has its own disk.
fn seed(app: &App) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    app.now.as_ref().map(|n| &n.path).hash(&mut h);
    h.finish()
}

/// Updates the simulation to the playback position and draws the grid into `area`.
pub fn draw(f: &mut Frame<'_>, app: &mut App, area: Rect) {
    let width = area.width as usize;
    let height = area.height as usize;
    if width < CELL_WIDTH || height == 0 {
        return;
    }
    let with_legend = height >= 4;
    let rows = if with_legend { height - 2 } else { height };
    let cols = (width + 1) / CELL_WIDTH;

    let shared = app.audio.shared();
    let duration = app
        .now
        .as_ref()
        .and_then(|n| n.duration)
        .unwrap_or(shared.duration())
        .as_secs_f64();
    let progress = if app.now.is_some() && duration > 0.0 {
        shared.position().as_secs_f64() / duration
    } else {
        0.0
    };
    // Louder music: the read head grabs more clusters at once.
    let bands = app.spectrum.bands();
    let loudness = bands.iter().sum::<f32>() / bands.len().max(1) as f32;
    let burst = 1 + (loudness * 10.0) as usize;
    let seed = seed(app);
    app.defrag.configure(cols * rows, seed);
    app.defrag.set_progress(progress, burst);

    let pad = " ".repeat(width.saturating_sub(cols * CELL_WIDTH - 1) / 2);
    let mut lines: Vec<Line<'static>> = (0..rows)
        .map(|r| {
            let mut spans = vec![Span::styled(pad.clone(), app.theme.base)];
            for c in 0..cols {
                let kind = app.defrag.kind(r * cols + c);
                spans.push(Span::styled(glyph(app, kind), style_of(app, kind)));
                if c + 1 < cols {
                    spans.push(Span::styled(" ", app.theme.base));
                }
            }
            Line::from(spans)
        })
        .collect();
    if with_legend {
        lines.push(Line::default());
        let mut legend = vec![Span::styled(pad.clone(), app.theme.base)];
        for (kind, label) in [
            (ClusterKind::Fragmented, "Fragmented"),
            (ClusterKind::Unoptimized, "Not optimized"),
            (ClusterKind::Optimized, "Optimized"),
            (ClusterKind::Reading, "Reading"),
            (ClusterKind::Writing, "Writing"),
            (ClusterKind::Free, "Free space"),
        ] {
            legend.push(Span::styled(glyph(app, kind), style_of(app, kind)));
            legend.push(Span::styled(format!(" {label}   "), app.theme.dim));
        }
        lines.push(Line::from(legend));
    }
    f.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_labels() {
        if cfg!(windows) {
            assert_eq!(drive_label(Some(Path::new(r"D:\Music\a.mp3"))), "Drive D:");
            assert_eq!(
                drive_label(Some(Path::new(r"\\nas\music\a.mp3"))),
                r"Drive \\nas\music"
            );
        }
        assert_eq!(drive_label(Some(Path::new("music/a.mp3"))), "Drive /");
        assert_eq!(drive_label(None), "Drive /");
    }
}
