//! The amazing-horse view (F5): the ride in half-block pixels (see `crate::horse`), with
//! text on the banner, the horse's speech bubble, the signposts, the rail and the board of
//! songs ahead.

use std::time::Duration;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use unicode_width::UnicodeWidthStr;

use super::jukebox::{artist_of, blit, dot, draw_list, ends, put, starts_in, style, title_of};
use super::{format_time, truncate};
use crate::horse::{self, BLUE, BOARD, BOARD_LIT, BUBBLE, CHALK, INK, Plan, RAIL, RED, SIGN};
use crate::model::PlaybackStatus;
use crate::outputs::pixels::Canvas;
use crate::paint::{Area, Rgb, hsv};
use crate::ui::app::App;
use crate::ui::jukebox::LineupEnd;
use crate::ui::keymap::{Action, Context};

/// Signposts on the track at most.
const SIGNS: usize = 12;
const TITLE: &str = "LOOK AT MY HORSE, MY HORSE IS AMAZING";
/// Text colours on the dark board.
const YELLOW: Rgb = [0xff, 0xd8, 0x74];
const SKY_BLUE: Rgb = [0x9f, 0xd8, 0xff];
const PINK: Rgb = [0xff, 0x9a, 0x8c];

fn bold(s: Style) -> Style {
    s.add_modifier(Modifier::BOLD)
}

pub fn draw(f: &mut Frame<'_>, app: &mut App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(plan) = Plan::new(area.width as usize, area.height as usize) else {
        return draw_list(f, app, area, " Look at my horse ");
    };
    app.lineup();
    let count = app.jukebox.lineup.songs.len();
    // The board scrolls a row at a time to keep the selection on it.
    let entries = plan.entries().max(1);
    let cursor = app.jukebox.cursor.min(count.saturating_sub(1));
    let mut scroll = app.jukebox.scroll.min(cursor);
    if cursor >= scroll + entries {
        scroll = cursor + 1 - entries;
    }
    app.jukebox.scroll = scroll;
    app.jukebox.page = entries;
    let shown = count.saturating_sub(scroll).min(entries);

    // How far along the track each sign stands: the time until its song, or while
    // stopped, the time from pressing play.
    let starts = app.lineup_start_times();
    let songs = &app.jukebox.lineup.songs;
    let ahead: Vec<Option<Duration>> = if app.now.is_some() {
        starts.clone()
    } else {
        let mut at = Some(Duration::ZERO);
        songs
            .iter()
            .map(|s| {
                let this = at;
                at = at.zip(s.track.duration()).map(|(a, d)| a + d);
                this
            })
            .collect()
    };
    let n = songs.len().min(SIGNS);
    let widths: Vec<usize> = songs[..n].iter().map(|s| s.code.width() + 2).collect();
    let posts = plan.signposts(&ahead[..n], &widths);
    let signs: Vec<(usize, usize, usize)> = posts
        .iter()
        .zip(&widths)
        .enumerate()
        .filter_map(|(i, (col, &w))| Some(((*col)?, w, i)))
        .collect();
    let boards: Vec<(usize, usize)> = signs.iter().map(|&(col, w, _)| (col, w)).collect();
    let scene = horse::Scene {
        signs: &boards,
        filled: shown,
        selected: (count > 0).then(|| cursor - scroll),
    };
    let mut canvas = Canvas::new(area.width as usize, area.height as usize * 2, horse::BANNER);
    app.horse.draw(&mut canvas, &plan, &scene);
    blit(f.buffer_mut(), area, &canvas, 2, app.symbols.unicode);

    let buf = f.buffer_mut();
    draw_banner(buf, app, area, plan.banner);
    for &(col, w, i) in &signs {
        let label = format!(" {} ", app.jukebox.lineup.songs[i].code);
        let at = Area::new(col, plan.sign_row, w, 1);
        put(
            buf,
            area,
            at,
            0,
            vec![Span::styled(label, bold(style(INK, SIGN)))],
        );
    }
    if let Some(bubble) = plan.bubble
        && !app.horse.is_flying()
    {
        draw_bubble(buf, app, area, bubble.inside(), &starts);
    }
    draw_rail(buf, app, area, plan.rail);
    draw_board(buf, app, area, &plan, &starts, scroll, shown);
}

/// The title in rainbow letters, or what is happening during the plane trick.
fn draw_banner(buf: &mut Buffer, app: &App, origin: Rect, at: Area) {
    let words = match app.horse.caption() {
        Some(caption) => caption,
        None if at.w >= TITLE.len() + 6 => TITLE,
        None => "MY HORSE IS AMAZING",
    };
    let text = if app.symbols.unicode {
        format!("♞ {words} ♞")
    } else {
        words.to_string()
    };
    let pad = at.w.saturating_sub(text.width()) / 2;
    let shift = app.horse.rainbow_shift();
    let spans = text
        .chars()
        .enumerate()
        .map(|(i, c)| {
            let fg = hsv(i as f32 / 24.0 - shift, 0.3, 1.0);
            Span::styled(c.to_string(), bold(style(fg, horse::BANNER)))
        })
        .collect();
    put(
        buf,
        origin,
        Area::new(at.x + pad, at.y, at.w - pad, 1),
        0,
        spans,
    );
}

/// What the horse says: the next song.
fn draw_bubble(buf: &mut Buffer, app: &App, origin: Rect, at: Area, starts: &[Option<Duration>]) {
    let (w, ell) = (at.w.saturating_sub(2), app.symbols.ellipsis);
    let at = Area::new(at.x + 1, at.y, w, at.h);
    let ink = style(INK, BUBBLE);
    let lineup = &app.jukebox.lineup;
    let key = |action| app.keymap.hint(Context::Playlist, action);
    let mut lines: Vec<Vec<Span>> = Vec::new();
    match lineup.songs.first() {
        Some(next) => {
            let repeats = lineup.end == LineupEnd::Repeats && next.queued.is_none();
            let when = if app.now.is_none() {
                format!("{} to ride", key(Action::PlayPause))
            } else if repeats {
                "again next".to_string()
            } else {
                starts_in(starts.first().copied().flatten(), "after this one")
            };
            lines.push(ends(
                vec![
                    ("Next up: ".into(), ink),
                    (next.code.clone(), bold(style(RED, BUBBLE))),
                ],
                vec![(when, bold(style(BLUE, BUBBLE)))],
                w,
                ell,
            ));
            lines.push(vec![Span::styled(
                truncate(&title_of(next), w, ell),
                bold(ink),
            )]);
            let mut by = artist_of(next);
            if let Some(d) = next.track.duration() {
                if !by.is_empty() {
                    by.push_str(dot(app));
                }
                by.push_str(&format_time(d));
            }
            lines.push(vec![Span::styled(truncate(&by, w, ell), ink)]);
            let why = match next.queued {
                Some(n) => format!("You queued it (#{n} in the queue)"),
                None if repeats => "Repeat track is on".to_string(),
                None => format!("Next in play order ({})", app.play_mode.label()),
            };
            lines.push(vec![Span::styled(
                truncate(&why, w, ell),
                style(BLUE, BUBBLE),
            )]);
        }
        None => {
            for text in [
                "My horse is amazing.".to_string(),
                "But no songs are coming up.".to_string(),
                format!("Queue some with {} in the playlist.", key(Action::Enqueue)),
            ] {
                lines.push(vec![Span::styled(truncate(&text, w, ell), ink)]);
            }
        }
    }
    for (row, spans) in lines.into_iter().take(at.h).enumerate() {
        put(buf, origin, at, row, spans);
    }
}

/// The rail: what the horse is riding to now.
fn draw_rail(buf: &mut Buffer, app: &App, origin: Rect, at: Area) {
    let ink = style(INK, RAIL);
    let icon = if app.symbols.unicode { " ♞ " } else { " " };
    let label = match app.status() {
        PlaybackStatus::Playing => "NOW RIDING",
        PlaybackStatus::Paused => "STANDING STILL",
        PlaybackStatus::Stopped => "IN THE STABLE",
    };
    let mut left = vec![(format!("{icon}{label}  "), bold(ink))];
    let mut right = Vec::new();
    if let Some(now) = &app.now {
        if let Some(code) = &app.jukebox.lineup.playing_code {
            left.push((format!("{code}  "), bold(style(RED, RAIL))));
        }
        left.push((app.now_playing_title().unwrap_or_default(), ink));
        let shared = app.audio.shared();
        let length = now.duration.unwrap_or(shared.duration());
        let mut time = format_time(shared.position());
        if !length.is_zero() {
            time = format!("{time} / {}", format_time(length));
        }
        // The tempo the horse gallops to.
        if let Some(bpm) = app.horse.bpm() {
            right.push((format!("{bpm:.0} BPM  "), style(RED, RAIL)));
        }
        right.push((format!("{time} "), bold(ink)));
    } else {
        let ride = format!(
            "Nobody is riding. Press {} to ride.",
            app.keymap.hint(Context::Playlist, Action::PlayPause)
        );
        left.push((ride, ink));
    }
    put(
        buf,
        origin,
        at,
        0,
        ends(left, right, at.w, app.symbols.ellipsis),
    );
}

/// The board of songs ahead, with when each starts.
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
    let chalk = style(CHALK, BOARD);
    let header = plan.board_header();
    let total: Option<Duration> = lineup.songs.iter().map(|s| s.track.duration()).sum();
    let mut left = format!("THE ROAD AHEAD  {count}");
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
            LineupEnd::Stops => "then the road ends",
            LineupEnd::Reshuffles => "then a new shuffle",
            LineupEnd::Repeats => "then the same song again",
            LineupEnd::More => "and on it goes",
        }
        .to_string()
    };
    let spans = ends(
        vec![(left, bold(chalk))],
        vec![(right, style(SKY_BLUE, BOARD))],
        header.w,
        ell,
    );
    put(buf, origin, header, 0, spans);
    if count == 0 {
        let hint = format!(
            "No songs ahead. Queue some with {} in the playlist.",
            app.keymap.hint(Context::Playlist, Action::Enqueue)
        );
        let at = plan.entry(0);
        put(
            buf,
            origin,
            at,
            0,
            vec![Span::styled(truncate(&hint, at.w, ell), chalk)],
        );
        return;
    }

    let stopped = app.now.is_none();
    let selected = app.jukebox.cursor.min(count - 1);
    for slot in 0..shown {
        let i = scroll + slot;
        let song = &lineup.songs[i];
        let lit = i == selected;
        let bg = if lit { BOARD_LIT } else { BOARD };
        let pick = |dark: Rgb, light: Rgb| style(if lit { dark } else { light }, bg);
        let text = pick(INK, CHALK);
        let mut left = vec![
            (format!("{:<4}", song.code), bold(pick(RED, YELLOW))),
            (title_of(song), bold(text)),
        ];
        let artist = artist_of(song);
        if !artist.is_empty() {
            left.push((format!("{}{artist}", dot(app)), text));
        }
        let mut right = Vec::new();
        if let Some(n) = song.queued {
            right.push((format!("Q{n}  "), bold(pick(RED, PINK))));
        }
        if let Some(d) = song.track.duration() {
            right.push((format!("{}  ", format_time(d)), text));
        }
        if !stopped {
            let repeats = i == 0 && lineup.end == LineupEnd::Repeats && song.queued.is_none();
            let when = if repeats {
                "again".to_string()
            } else {
                starts_in(starts.get(i).copied().flatten(), "later")
            };
            right.push((when, bold(pick(BLUE, SKY_BLUE))));
        }
        put(
            buf,
            origin,
            plan.entry(slot),
            0,
            ends(left, right, plan.entry(slot).w, ell),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{AudioEvent, StreamInfo};
    use crate::model::{Group, Track, TrackMetadata};
    use crate::ui::app::View;
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
                duration: Duration::from_secs(secs),
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

    fn playing(n: usize) -> App {
        let mut app = test_app(tracks());
        let uid = app.root.tracks[n].uid;
        app.play(uid, true);
        start(&mut app, n);
        app
    }

    #[test]
    fn shows_the_songs_ahead_as_a_ride() {
        let mut app = playing(0);
        app.audio.shared().set_position(Duration::from_secs(95));
        app.on_key(crate::ui::keymap::KeyBinding::parse("F5").unwrap());
        assert_eq!(app.view, View::Horse);
        let screen = render(&mut app, 120, 34);
        println!("{screen}");
        assert!(screen.contains("LOOK AT MY HORSE, MY HORSE IS AMAZING"));
        assert!(
            screen.contains("Next up: A2"),
            "the horse says what is next"
        );
        assert!(screen.contains("Highway Star"));
        assert!(
            screen.contains(" A2 ") && screen.contains(" A3 "),
            "signposts"
        );
        assert!(screen.contains("A1  Led Zeppelin - Black Dog"), "the rail");
        assert!(screen.contains("01:35 / 04:55"));
        assert!(screen.contains("THE ROAD AHEAD  3 songs · 16:39"));
        assert!(screen.contains("then the road ends"));
        // 200 s left of Black Dog, then Highway Star's 368 s, then Money's 383 s.
        assert!(screen.contains("in 03:20") && screen.contains("in 09:28"));
        assert!(screen.contains("in 15:51"));
        assert!(screen.contains("F5:Back"));
        assert!(screen.contains('▀'), "the ride is drawn in half blocks");
    }

    #[test]
    fn f4_and_f5_switch_between_the_jukebox_and_the_horse() {
        let mut app = playing(0);
        app.on_action(Action::FileManager);
        app.on_action(Action::Horse);
        assert_eq!(app.view, View::Horse);
        app.on_action(Action::Jukebox);
        assert_eq!(app.view, View::Jukebox);
        app.on_action(Action::CursorDown);
        app.on_action(Action::Horse);
        assert_eq!(app.jukebox.cursor, 1, "the selection is shared");
        // Esc goes back to where the first of them was opened.
        app.on_action(Action::Close);
        assert_eq!(app.view, View::FileManager);
    }

    #[test]
    fn a_new_song_turns_the_horse_into_a_plane() {
        let mut app = playing(0);
        app.on_action(Action::Horse);
        app.update_horse(Duration::from_millis(16));
        assert!(!app.horse.is_flying(), "opening shows the ride at once");
        start(&mut app, 1);
        for _ in 0..30 {
            app.update_horse(Duration::from_millis(33));
        }
        assert!(app.horse.is_flying());
        assert!(app.is_animating());
        assert_eq!(app.effective_fps(), 30);
        let screen = render(&mut app, 120, 34);
        println!("{screen}");
        assert!(screen.contains("MY HORSE IS A PLANE NOW"));
        assert!(!screen.contains("Next up:"), "no speech bubble mid-air");
    }

    #[test]
    fn ascii_and_small_terminals() {
        let mut app = playing(0);
        app.queue = vec![app.root.tracks[3].uid];
        app.on_action(Action::Horse);
        app.symbols = crate::ui::theme::Symbols::ASCII;
        let screen = render(&mut app, 100, 34);
        println!("{screen}");
        assert!(screen.is_ascii(), "ascii charset must not draw unicode");
        assert!(screen.contains("MY HORSE IS AMAZING"));
        assert!(screen.contains("You queued it (#1 in the queue)"));
        assert!(screen.contains("Q1"));

        let screen = render(&mut app, 60, 14);
        println!("{screen}");
        assert!(screen.contains("Look at my horse"));
        assert!(screen.contains("A4  Queen - Under Pressure"));
    }

    #[test]
    fn the_config_turns_beat_matching_on() {
        let mut app = test_app(tracks());
        assert!(!app.horse.beat_match, "off by default");
        let (cfg, warnings) = crate::config::parse("[ui]\nhorse_beat_match = true\n");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(app.apply_config(&cfg).is_empty());
        assert!(app.horse.beat_match);
    }

    #[test]
    fn stopped_and_empty() {
        let mut app = test_app(tracks());
        app.on_action(Action::Horse);
        let screen = render(&mut app, 120, 34);
        println!("{screen}");
        assert!(screen.contains("Nobody is riding."));
        assert!(screen.contains("Space to ride"));
        assert!(
            screen.contains(" A1 "),
            "the first song's sign waits at the horse"
        );

        let mut app = test_app(Group::new(0, "Root"));
        app.on_action(Action::Horse);
        let screen = render(&mut app, 120, 34);
        assert!(screen.contains("But no songs are coming up."));
        assert!(screen.contains("No songs ahead."));
    }
}
