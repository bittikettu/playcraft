//! The jukebox view: the machine in half-block pixels (see `crate::jukebox`), with text on
//! its sign, the "Next up" glass, the "Now playing" plaque and the title strips.

use std::time::Duration;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use unicode_width::UnicodeWidthStr;

use super::{format_time, justify, truncate};
use crate::jukebox::{self, Disc, Plan, Scene};
use crate::model::PlaybackStatus;
use crate::outputs::card::CardColors;
use crate::outputs::pixels::Canvas;
use crate::paint::{Area, Rgb};
use crate::ui::app::App;
use crate::ui::jukebox::{LineupEnd, Song};
use crate::ui::keymap::{Action, Context};

/// Records shown in the rack.
const RACK: usize = 12;

pub(super) fn color(c: Rgb) -> Color {
    Color::Rgb(c[0], c[1], c[2])
}

pub(super) fn style(fg: Rgb, bg: Rgb) -> Style {
    Style::new().fg(color(fg)).bg(color(bg))
}

/// "in 04:12", or the given fallback when the start is unknown.
pub(super) fn starts_in(at: Option<Duration>, unknown: &str) -> String {
    at.map_or_else(|| unknown.to_string(), |d| format!("in {}", format_time(d)))
}

pub(super) fn title_of(song: &Song) -> String {
    let meta = song.track.metadata.as_ref();
    meta.and_then(|m| m.title.clone()).unwrap_or_else(|| {
        song.track
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    })
}

pub(super) fn artist_of(song: &Song) -> String {
    song.track
        .metadata
        .as_ref()
        .and_then(|m| m.artist.clone())
        .unwrap_or_default()
}

/// Writes `spans` from column `at.x` of row `at.y`, at most `at.w` columns.
pub(super) fn put(buf: &mut Buffer, origin: Rect, at: Area, row: usize, spans: Vec<Span<'_>>) {
    let line = Line::from(spans);
    buf.set_line(
        origin.x + at.x as u16,
        origin.y + (at.y + row) as u16,
        &line,
        at.w as u16,
    );
}

/// `left` and `right` in `width` columns: `left` is cut to make room.
pub(super) fn ends<'a>(
    left: Vec<(String, Style)>,
    right: Vec<(String, Style)>,
    width: usize,
    ell: &str,
) -> Vec<Span<'a>> {
    let right_w: usize = right.iter().map(|(s, _)| s.width()).sum();
    let mut room = width.saturating_sub(right_w + 1);
    let mut spans = Vec::new();
    let mut used = 0;
    let fill = right
        .first()
        .or(left.first())
        .map_or(Style::new(), |(_, s)| *s);
    for (text, style) in left {
        let piece = truncate(&text, room, ell);
        let cut = piece.width() < text.width();
        room = room.saturating_sub(piece.width());
        used += piece.width();
        spans.push(Span::styled(piece, style));
        if cut || room == 0 {
            break;
        }
    }
    let pad = width.saturating_sub(used + right_w);
    spans.push(Span::styled(" ".repeat(pad), fill));
    spans.extend(right.into_iter().map(|(t, s)| Span::styled(t, s)));
    spans
}

pub fn draw(f: &mut Frame<'_>, app: &mut App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let half_blocks = app.symbols.unicode;
    let ppc = if half_blocks { 2 } else { 1 };
    let Some(plan) = Plan::new(area.width as usize, area.height as usize, ppc) else {
        return draw_list(f, app, area, " Jukebox ");
    };
    app.lineup();
    let count = app.jukebox.lineup.songs.len();
    // The board scrolls a row of cards at a time to keep the selection on it.
    let cards = plan.cards();
    let columns = plan.columns;
    let cursor = app.jukebox.cursor.min(count.saturating_sub(1));
    let mut scroll = app.jukebox.scroll.min(cursor - cursor % columns);
    if cursor >= scroll + cards {
        scroll = (cursor / columns + 1) * columns - cards;
    }
    app.jukebox.scroll = scroll;
    app.jukebox.page = cards;

    let colors = app.theme.card_colors();
    let shared = app.audio.shared();
    let length = app
        .now
        .as_ref()
        .and_then(|n| n.duration)
        .unwrap_or(shared.duration());
    let progress = (app.now.is_some() && !length.is_zero())
        .then(|| (shared.position().as_secs_f64() / length.as_secs_f64()) as f32);
    let rack: Vec<Disc> = app
        .jukebox
        .lineup
        .songs
        .iter()
        .take(RACK)
        .map(|s| s.disc)
        .collect();
    let shown = count.saturating_sub(scroll).min(cards);
    let scene = Scene {
        rack: &rack,
        progress,
        wall: colors.background,
        filled: shown,
        selected: (count > 0).then(|| cursor - scroll),
        next: (scroll == 0 && count > 0).then_some(0),
    };
    let mut canvas = Canvas::new(
        area.width as usize,
        area.height as usize * ppc,
        colors.background,
    );
    app.jukebox.scene.draw(&mut canvas, &plan, &scene);
    blit(f.buffer_mut(), area, &canvas, ppc, half_blocks);

    let starts = app.lineup_start_times();
    let buf = f.buffer_mut();
    if let Some(sign) = plan.sign {
        let text = if app.symbols.unicode {
            "♪ MP3BLASTER JUKEBOX ♪"
        } else {
            "* MP3BLASTER JUKEBOX *"
        };
        let pad = sign.w.saturating_sub(text.width()) / 2;
        let bold = style(colors.title, jukebox::SIGN).add_modifier(Modifier::BOLD);
        put(
            buf,
            area,
            Area::new(sign.x + pad, sign.y, sign.w - pad, 1),
            0,
            vec![Span::styled(text, bold)],
        );
    }
    if let Some(panel) = plan.panel {
        draw_next_up(buf, app, area, panel.inside(), &colors, &starts);
    }
    draw_plaque(buf, app, area, plan.plaque);
    draw_board(buf, app, area, &plan, &starts, scroll, shown);
}

/// Separates details in a line.
pub(super) fn dot(app: &App) -> &'static str {
    if app.symbols.unicode { " · " } else { " - " }
}

/// Copies the canvas, which has `ppc` pixel rows per terminal row, into the terminal: two
/// pixels per cell with the upper half block (foreground on top, background below), or
/// one colour per cell without `half_blocks`.
pub(super) fn blit(buf: &mut Buffer, area: Rect, canvas: &Canvas, ppc: usize, half_blocks: bool) {
    for cy in 0..area.height {
        for cx in 0..area.width {
            let (x, y) = (cx as usize, cy as usize * ppc);
            let mut top = canvas.get(x, y);
            let mut bottom = if ppc > 1 { canvas.get(x, y + 1) } else { top };
            if !half_blocks {
                top = crate::paint::mix(top, bottom, 0.5);
                bottom = top;
            }
            let cell = &mut buf[(area.x + cx, area.y + cy)];
            if top == bottom {
                cell.set_char(' ').set_fg(color(top)).set_bg(color(top));
            } else {
                cell.set_char('▀').set_fg(color(top)).set_bg(color(bottom));
            }
        }
    }
}

fn draw_next_up(
    buf: &mut Buffer,
    app: &App,
    origin: Rect,
    at: Area,
    colors: &CardColors,
    starts: &[Option<Duration>],
) {
    let bg = jukebox::GLASS;
    let ell = app.symbols.ellipsis;
    let (w, h) = (at.w.saturating_sub(2), at.h);
    let at = Area::new(at.x + 1, at.y, w, h);
    let bold = |c| style(c, bg).add_modifier(Modifier::BOLD);
    let lineup = &app.jukebox.lineup;
    let mut lines: Vec<Vec<Span>> = Vec::new();
    let stopped = app.now.is_none();
    match lineup.songs.first() {
        Some(next) => {
            let when = if stopped {
                format!(
                    "{} plays it",
                    app.keymap.hint(Context::Playlist, Action::PlayPause)
                )
            } else if lineup.end == LineupEnd::Repeats && next.queued.is_none() {
                "again next".to_string()
            } else {
                starts_in(starts.first().copied().flatten(), "after this one")
            };
            lines.push(ends(
                vec![
                    ("NEXT UP  ".into(), bold(colors.accent)),
                    (next.code.clone(), bold(colors.hot)),
                ],
                vec![(when, bold(colors.hot))],
                w,
                ell,
            ));
            lines.push(Vec::new());
            lines.push(vec![Span::styled(
                truncate(&title_of(next), w, ell),
                bold(colors.title),
            )]);
            let artist = artist_of(next);
            if !artist.is_empty() {
                lines.push(vec![Span::styled(
                    truncate(&artist, w, ell),
                    style(colors.text, bg),
                )]);
            }
            let meta = next.track.metadata.as_ref();
            let album = meta.and_then(|m| match (&m.album, m.year) {
                (Some(a), Some(y)) => Some(format!("{a} ({y})")),
                (Some(a), None) => Some(a.clone()),
                (None, Some(y)) => Some(y.to_string()),
                (None, None) => None,
            });
            if let Some(album) = album {
                lines.push(vec![Span::styled(
                    truncate(&album, w, ell),
                    style(colors.dim, bg),
                )]);
            }
            let mut details = Vec::new();
            if let Some(d) = next.track.duration() {
                details.push(format_time(d));
            }
            if let Some(m) = meta {
                details.extend(m.genre.clone());
                if m.bitrate_kbps > 0 {
                    details.push(format!("{} kbps", m.bitrate_kbps));
                }
            }
            if let Some(ext) = next.track.path.extension() {
                details.push(ext.to_string_lossy().to_uppercase());
            }
            let details = details.join(dot(app));
            lines.push(vec![Span::styled(
                truncate(&details, w, ell),
                style(colors.dim, bg),
            )]);
            lines.push(Vec::new());
            let why = match next.queued {
                Some(n) => format!("Queued by you (#{n} in the queue)"),
                None if lineup.end == LineupEnd::Repeats => "Repeat track is on".to_string(),
                None => format!("Next in play order ({})", app.play_mode.label()),
            };
            lines.push(vec![Span::styled(
                truncate(&why, w, ell),
                style(colors.low, bg),
            )]);
            if let Some(then) = lineup.songs.get(1) {
                let when = starts_in(starts.get(1).copied().flatten(), "");
                lines.push(ends(
                    vec![
                        ("Then ".into(), style(colors.dim, bg)),
                        (format!("{} ", then.code), bold(colors.hot)),
                        (title_of(then), style(colors.text, bg)),
                    ],
                    vec![(when, style(colors.dim, bg))],
                    w,
                    ell,
                ));
            }
        }
        None => {
            lines.push(vec![Span::styled("NEXT UP", bold(colors.accent))]);
            lines.push(Vec::new());
            let hint = format!(
                "Nothing. Press {} on songs in the playlist to queue them.",
                app.keymap.hint(Context::Playlist, Action::Enqueue)
            );
            for line in crate::outputs::pixels::wrap(&hint, w) {
                lines.push(vec![Span::styled(line, style(colors.dim, bg))]);
            }
        }
    }
    // Drop the blank lines first when the glass is short.
    while lines.len() > h {
        match lines.iter().rposition(Vec::is_empty) {
            Some(i) => {
                lines.remove(i);
            }
            None => lines.truncate(h),
        }
    }
    for (row, spans) in lines.into_iter().enumerate() {
        put(buf, origin, at, row, spans);
    }
}

fn draw_plaque(buf: &mut Buffer, app: &App, origin: Rect, at: Area) {
    let (ink, bg) = (jukebox::INK, jukebox::PLATE);
    let bold = style(ink, bg).add_modifier(Modifier::BOLD);
    let red = style(jukebox::RED, bg).add_modifier(Modifier::BOLD);
    let uni = app.symbols.unicode;
    let (icon, label) = match app.status() {
        PlaybackStatus::Playing => (if uni { "▶" } else { ">" }, "NOW PLAYING"),
        PlaybackStatus::Paused => (if uni { "❚❚" } else { "||" }, "PAUSED"),
        PlaybackStatus::Stopped => (if uni { "■" } else { "[]" }, "STOPPED"),
    };
    let mut left = vec![(format!(" {icon} {label}  "), bold)];
    let mut right = Vec::new();
    if let Some(now) = &app.now {
        if let Some(code) = &app.jukebox.lineup.playing_code {
            left.push((format!("{code}  "), red));
        }
        left.push((app.now_playing_title().unwrap_or_default(), style(ink, bg)));
        let shared = app.audio.shared();
        let length = now.duration.unwrap_or(shared.duration());
        let mut time = format_time(shared.position());
        if !length.is_zero() {
            time = format!("{time} / {}", format_time(length));
        }
        right.push((format!("{time} "), bold));
    } else {
        left.push(("The turntable is empty".into(), style(ink, bg)));
    }
    let spans = ends(left, right, at.w, app.symbols.ellipsis);
    put(buf, origin, at, 0, spans);
}

fn draw_board(
    buf: &mut Buffer,
    app: &App,
    origin: Rect,
    plan: &Plan,
    starts: &[Option<Duration>],
    scroll: usize,
    shown: usize,
) {
    let ell = app.symbols.ellipsis;
    let lineup = &app.jukebox.lineup;
    let count = lineup.songs.len();
    let board = jukebox::BOARD;
    let cream = style(jukebox::CARD, board);
    let header = plan.board_header();
    let total: Option<Duration> = lineup.songs.iter().map(|s| s.track.duration()).sum();
    let mut left = format!("COMING UP  {count}");
    if lineup.end == LineupEnd::More {
        left.push('+');
    }
    left.push_str(if count == 1 { " song" } else { " songs" });
    if let Some(total) = total.filter(|_| lineup.end != LineupEnd::More && count > 0) {
        left.push_str(&format!("{}{}", dot(app), format_time(total)));
    }
    let right = if count > shown {
        format!("{}-{} of {count}", scroll + 1, scroll + shown)
    } else {
        match lineup.end {
            LineupEnd::Stops => "then the music stops",
            LineupEnd::Reshuffles => "then a new shuffle",
            LineupEnd::Repeats => "then it plays again",
            LineupEnd::More => "and more",
        }
        .to_string()
    };
    let spans = ends(
        vec![(left, cream.add_modifier(Modifier::BOLD))],
        vec![(right, style([0xb8, 0xa8, 0x8c], board))],
        header.w,
        ell,
    );
    put(buf, origin, header, 0, spans);
    if count == 0 {
        let hint = format!(
            "No songs coming up. Queue some with {} in the playlist.",
            app.keymap.hint(Context::Playlist, Action::Enqueue)
        );
        let at = plan.card(0);
        put(
            buf,
            origin,
            Area::new(at.x, at.y, header.w, 1),
            0,
            vec![Span::styled(truncate(&hint, header.w, ell), cream)],
        );
        return;
    }

    let stopped = app.now.is_none();
    for slot in 0..shown {
        let i = scroll + slot;
        let song = &lineup.songs[i];
        let at = plan.card(slot);
        let bg = if i == app.jukebox.cursor.min(count - 1) {
            jukebox::CARD_LIT
        } else {
            jukebox::CARD
        };
        let ink = style(jukebox::INK, bg);
        let red = style(jukebox::RED, bg).add_modifier(Modifier::BOLD);
        let blue = style(jukebox::BLUE, bg).add_modifier(Modifier::BOLD);
        let length = song.track.duration().map(format_time).unwrap_or_default();
        let when = if stopped {
            length.clone()
        } else if i == 0 && lineup.end == LineupEnd::Repeats && song.queued.is_none() {
            "again".to_string()
        } else {
            starts_in(starts.get(i).copied().flatten(), "later")
        };
        let mut when_spans = Vec::new();
        if i == 0 {
            when_spans.push(("NEXT ".to_string(), red));
        }
        when_spans.push((format!("{when} "), blue));
        let top = ends(
            vec![
                (format!(" {:<4}", song.code), red),
                (
                    title_of(song).to_uppercase(),
                    ink.add_modifier(Modifier::BOLD),
                ),
            ],
            when_spans,
            at.w,
            ell,
        );
        put(buf, origin, at, 0, top);
        let mut tail = Vec::new();
        if let Some(n) = song.queued {
            tail.push((format!("Q{n} "), red));
        }
        if !stopped && !length.is_empty() {
            tail.push((format!("{length} "), ink));
        }
        let bottom = ends(
            vec![("      ".into(), ink), (artist_of(song), ink)],
            tail,
            at.w,
            ell,
        );
        put(buf, origin, at, 1, bottom);
    }
}

/// For terminals too small for the pictures: the lineup as a list under `title`.
pub(super) fn draw_list(f: &mut Frame<'_>, app: &mut App, area: Rect, title: &str) {
    app.lineup();
    let theme = &app.theme;
    let block = Block::bordered()
        .border_set(app.symbols.border)
        .border_style(theme.border)
        .style(theme.base)
        .title(Line::styled(title.to_string(), theme.title));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let starts = app.lineup_start_times();
    let width = inner.width as usize;
    let lineup = &app.jukebox.lineup;
    let cursor = app.jukebox.cursor;
    let first = cursor.saturating_sub((inner.height as usize).saturating_sub(1));
    let lines: Vec<Line> = lineup
        .songs
        .iter()
        .enumerate()
        .skip(first)
        .take(inner.height as usize)
        .map(|(i, song)| {
            let text = format!("{:<4}{}", song.code, song.track.display_name());
            let when = starts_in(starts.get(i).copied().flatten(), "");
            let style = if i == cursor {
                theme.base.patch(theme.cursor)
            } else {
                theme.base
            };
            Line::styled(justify(&text, &when, width, app.symbols.ellipsis), style)
        })
        .collect();
    if lines.is_empty() {
        f.render_widget(
            Paragraph::new(Line::styled("No songs coming up.", theme.dim)),
            inner,
        );
    } else {
        f.render_widget(Paragraph::new(lines), inner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{AudioEvent, StreamInfo};
    use crate::model::{Group, Track, TrackMetadata};
    use crate::ui::tests::{render, test_app};

    fn tracks() -> Group {
        let mut root = Group::new(0, "Root");
        for (i, (artist, title, secs)) in [
            ("Led Zeppelin", "Black Dog", 295),
            ("Deep Purple", "Highway Star", 368),
            ("Pink Floyd", "Money", 383),
            ("Queen", "Under Pressure", 248),
        ]
        .into_iter()
        .enumerate()
        {
            let mut t = Track::new(format!("{i:02} {title}.flac"));
            t.metadata = Some(TrackMetadata {
                title: Some(title.into()),
                artist: Some(artist.into()),
                album: Some("Hits".into()),
                year: Some(1972),
                duration: Duration::from_secs(secs),
                bitrate_kbps: 900,
                ..Default::default()
            });
            root.tracks.push(t);
        }
        root
    }

    fn start(app: &mut App, i: usize) {
        let path = app.root.tracks[i].path.clone();
        let duration = app.root.tracks[i].duration();
        app.on_audio_event(AudioEvent::TrackStarted {
            path,
            duration,
            info: StreamInfo {
                codec: "flac".into(),
                sample_rate: 44_100,
                channels: 2,
            },
        });
    }

    #[test]
    fn shows_the_next_song_and_when_each_starts() {
        let mut app = test_app(tracks());
        let uid = app.root.tracks[0].uid;
        app.play(uid, true);
        start(&mut app, 0);
        app.audio.shared().set_position(Duration::from_secs(95));
        app.on_action(Action::Jukebox);
        let screen = render(&mut app, 120, 34);
        println!("{screen}");
        assert!(screen.contains("MP3BLASTER JUKEBOX"));
        assert!(
            screen.contains("NEXT UP  A2"),
            "the glass names the next song"
        );
        assert!(screen.contains("Highway Star"));
        // 295 - 95 s left of Black Dog, then Highway Star's 368 s.
        assert!(screen.contains("in 03:20"));
        assert!(screen.contains("in 09:28"));
        assert!(screen.contains("in 15:51"));
        assert!(screen.contains("HIGHWAY STAR"), "title strips in capitals");
        assert!(
            screen.contains("A1  Led Zeppelin - Black Dog"),
            "the plaque"
        );
        assert!(screen.contains("01:35 / 04:55"));
        assert!(screen.contains("COMING UP  3 songs · 16:39"));
        assert!(screen.contains("then the music stops"));
        assert!(screen.contains("F4:Back"));
        assert!(screen.contains('▀'), "the machine is drawn in half blocks");
    }

    #[test]
    fn queued_songs_say_so_and_ascii_terminals_get_plain_characters() {
        let mut app = test_app(tracks());
        let (first, last) = (app.root.tracks[0].uid, app.root.tracks[3].uid);
        app.play(first, true);
        start(&mut app, 0);
        app.queue = vec![last];
        app.on_action(Action::Jukebox);
        app.symbols = crate::ui::theme::Symbols::ASCII;
        let screen = render(&mut app, 100, 30);
        println!("{screen}");
        assert!(screen.is_ascii(), "ascii charset must not draw unicode");
        assert!(screen.contains("Queued by you (#1 in the queue)"));
        assert!(screen.contains("Q1"));
        assert!(screen.contains("NEXT UP  A4"));
    }

    #[test]
    fn small_terminals_get_a_list_and_empty_lineups_a_hint() {
        let mut app = test_app(tracks());
        app.on_action(Action::Jukebox);
        let screen = render(&mut app, 60, 14);
        println!("{screen}");
        assert!(screen.contains("Jukebox"));
        assert!(screen.contains("A1  Led Zeppelin - Black Dog"));

        let mut app = test_app(Group::new(0, "Root"));
        app.on_action(Action::Jukebox);
        let screen = render(&mut app, 120, 34);
        println!("{screen}");
        assert!(screen.contains("No songs coming up"));
        assert!(screen.contains("The turntable is empty"));
    }

    #[test]
    fn the_board_scrolls_to_keep_the_selection_on_it() {
        let mut root = Group::new(0, "Root");
        root.tracks = (0..40)
            .map(|i| Track::new(format!("song {i:02}.mp3")))
            .collect();
        let mut app = test_app(root);
        app.on_action(Action::Jukebox);
        render(&mut app, 120, 34);
        let page = app.jukebox.page;
        assert!(page >= 6, "{page} cards");
        app.on_action(Action::End);
        let screen = render(&mut app, 120, 34);
        println!("{screen}");
        assert!(screen.contains("SONG 39"));
        assert!(!screen.contains("SONG 00"));
        assert!(screen.contains("of 40"));
    }

    /// Drawing time and terminal load while a record plays: the lights change in steps,
    /// so few cells change from one frame to the next.
    /// `cargo test --release --lib jukebox_frame_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn jukebox_frame_cost() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut root = Group::new(0, "Root");
        root.tracks = (0..200)
            .map(|i| Track::new(format!("song {i:03}.mp3")))
            .collect();
        let mut app = test_app(root);
        let uid = app.root.tracks[0].uid;
        app.play(uid, true);
        app.on_action(Action::Jukebox);
        let shared = app.audio.shared().clone();
        for (w, h) in [(120u16, 34u16), (200, 55)] {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            let mut previous = terminal.backend().buffer().clone();
            let (mut changed, frames) = (0, 120);
            let started = std::time::Instant::now();
            for _ in 0..frames {
                app.update_jukebox(Duration::from_millis(33));
                shared.set_position(shared.position() + Duration::from_millis(33));
                terminal
                    .draw(|f| crate::ui::views::draw(f, &mut app))
                    .unwrap();
                let screen = terminal.backend().buffer().clone();
                changed += screen
                    .content
                    .iter()
                    .zip(&previous.content)
                    .filter(|(a, b)| a != b)
                    .count();
                previous = screen;
            }
            println!(
                "{w}x{h}: {:?} per frame, {} cells changed per frame",
                started.elapsed() / frames,
                changed / frames as usize
            );
        }
    }
}
