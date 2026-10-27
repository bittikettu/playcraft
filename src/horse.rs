//! "Look at my horse, my horse is amazing" (F5), after Weebl's cartoon song: the songs
//! ahead, as a ride. A white horse with a rainbow mane gallops fast along a country track,
//! under a rainbow. With beat matching on (`[ui] horse_beat_match`), it gallops to the
//! beat instead: one stride per beat, landing on it (the beat detector is HYPERSPACE's),
//! and slows to a walk without one. The songs coming up are signposts on the track,
//! placed by the time until they start, so the next one reaches the horse just as its
//! song begins. Then the horse turns into a plane, loops the loop over the meadow and
//! lands as a horse again.
//!
//! As with the jukebox, the layout is planned in terminal cells, so the UI can write text
//! on the plain parts: the banner, the speech bubble, the signs, the rail and the board.
//! The canvas always has two pixel rows per terminal row.

use std::f32::consts::{PI, TAU};
use std::time::Duration;

use crate::hyperspace::BeatDetector;
use crate::outputs::pixels::Canvas;
use crate::paint::{Area, Pen, Rgb, frame, hsv, lerp, mix, noise, phase};

/// Colours the UI writes text on.
pub const BANNER: Rgb = [0x3b, 0x84, 0xd4];
pub const BUBBLE: Rgb = [0xff, 0xfd, 0xf5];
pub const SIGN: Rgb = [0xf6, 0xe7, 0xb8];
pub const RAIL: Rgb = [0xf3, 0xee, 0xe0];
pub const BOARD: Rgb = [0x24, 0x3d, 0x27];
pub const BOARD_LIT: Rgb = [0xff, 0xd8, 0x74];
/// Text colours: dark brown, red and blue on the light parts, chalk on the board.
pub const INK: Rgb = [0x3b, 0x24, 0x14];
pub const RED: Rgb = [0xc0, 0x2c, 0x2c];
pub const BLUE: Rgb = [0x1d, 0x4f, 0x9a];
pub const CHALK: Rgb = [0xee, 0xf2, 0xe4];

const SKY_LOW: Rgb = [0xbd, 0xe5, 0xfb];
const SUN: Rgb = [0xff, 0xe0, 0x66];
const CLOUD: Rgb = [0xff, 0xff, 0xff];
const CLOUD_SHADE: Rgb = [0xdc, 0xe8, 0xf2];
const HILL: Rgb = [0x78, 0xb8, 0x66];
const MEADOW: Rgb = [0x62, 0xb5, 0x4b];
const MEADOW_LOW: Rgb = [0x3c, 0x8a, 0x31];
const GRASS: Rgb = [0x2f, 0x72, 0x27];
const TRACK: Rgb = [0xcf, 0xa4, 0x6b];
const TRACK_EDGE: Rgb = [0xa8, 0x7e, 0x4c];
const POST: Rgb = [0x7a, 0x52, 0x2c];
const FENCE: Rgb = [0xf7, 0xf3, 0xe8];
const COAT: Rgb = [0xfa, 0xf7, 0xf0];
const COAT_SHADE: Rgb = [0xd6, 0xd0, 0xc4];
const COAT_FAR: Rgb = [0xb9, 0xb2, 0xa5];
const HOOF: Rgb = [0x4b, 0x3c, 0x33];
const EYE: Rgb = [0x1c, 0x14, 0x10];
const MUZZLE: Rgb = [0xea, 0xc4, 0xbb];
const DUST: Rgb = [0xe6, 0xcf, 0xa2];
const OUTLINE: Rgb = [0x3b, 0x2c, 0x22];
/// The wooden frame's top, left and bottom-right edges.
const WOOD_FRAME: [Rgb; 3] = [[0xa0, 0x70, 0x3e], [0xb4, 0x80, 0x4a], [0x5a, 0x38, 0x1c]];

/// Seconds the plane trick takes.
pub const TRICK_SECS: f32 = 3.4;
// The steps of the trick, as shares of `TRICK_SECS`.
const POOF_OUT: (f32, f32) = (0.0, 0.1);
const FLIGHT: (f32, f32) = (0.08, 0.92);
const POOF_IN: (f32, f32) = (0.9, 1.0);
/// Strides per second: the steady gallop without beat matching (twelve frames a stride
/// at 30 frames a second, like the twelve-phase chart), a gallop before the tempo is
/// known, and a walk when no beat is heard.
const STEADY: f32 = 2.5;
const GALLOP: f32 = 1.9;
const WALK: f32 = 0.6;
/// The pace a tempo is folded into (strides per second): one stride per beat, or per two
/// beats in fast music.
const PACE: (f32, f32) = (1.0, 2.4);
/// Without a beat for this many beats (and at least `BEAT_LOST_MIN` seconds) the horse
/// slows to a walk.
const BEATS_LOST: f32 = 3.0;
const BEAT_LOST_MIN: f32 = 1.2;
/// The beat detector notices a kick about this long after it is heard: its FFT window
/// is 85 ms, so a kick shows fully half way in, and a frame passes before the check.
const DETECT_LAG: f32 = 0.045;
/// Seconds ahead at which a signpost stands half way from the horse to the right edge.
const HALF_WAY: f32 = 240.0;

/// Where the parts of the picture go: cells from the view's top left, and pixels (two per
/// row) for the horse.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub width: usize,
    /// The title across the top: one row of text.
    pub banner: Area,
    /// The sky, the meadow and the track.
    pub field: Area,
    /// Where the hooves touch the track (pixel row), the middle of the horse's body (pixel
    /// column), and the horse's size: about its body length, in pixels.
    pub ground: f32,
    pub horse_x: f32,
    pub size: f32,
    /// The row the signposts' boards are on.
    pub sign_row: usize,
    /// The horse's speech bubble: framed, with text inside (`Area::inside`).
    pub bubble: Option<Area>,
    /// The fence rail saying what is playing: one row of text.
    pub rail: Area,
    /// The board of songs ahead: framed, a header row, then a song per row.
    pub board: Area,
    pub columns: usize,
}

impl Plan {
    /// Plans a picture filling `cols` x `rows` cells, or `None` when it would not fit.
    pub fn new(cols: usize, rows: usize) -> Option<Plan> {
        if cols < 40 || rows < 12 {
            return None;
        }
        // Below the banner: the field, the rail and the board (its frame, header and at
        // least one song).
        let rest = rows - 2;
        let mut field_h = ((rest as f32 * 0.6).round() as usize).clamp(8, 32);
        while rest - field_h < 4 {
            field_h -= 1;
        }
        let board_h = rest - field_h;
        let field = Area::new(0, 1, cols, field_h);
        let height = field_h as f32 * 2.0;
        let ground = ((1 + field_h) * 2) as f32 - (height * 0.14).max(2.5);
        let size = (height * 0.5).min(cols as f32 * 0.2);
        let horse_x = (cols as f32 * 0.16).max(size * 0.8);
        let sign_row = (((ground - size * 0.8) / 2.0).floor() as usize).clamp(2, field_h - 1);

        // The bubble floats right of the head, above the signposts.
        let muzzle = horse_x + size * 0.95;
        let bx = (muzzle + 3.0).ceil() as usize;
        let by = if field_h >= 14 { 2 } else { 1 };
        let bh = (sign_row.saturating_sub(by)).min(if field_h >= 16 { 6 } else { 5 });
        let bw = cols.saturating_sub(bx + 3).min(54);
        let bubble = (bh >= 4 && bw >= 28).then(|| Area::new(bx, by, bw, bh));
        Some(Plan {
            width: cols,
            banner: Area::new(0, 0, cols, 1),
            field,
            ground,
            horse_x,
            size,
            sign_row,
            bubble,
            rail: Area::new(0, 1 + field_h, cols, 1),
            board: Area::new(0, 2 + field_h, cols, board_h),
            columns: if cols >= 150 { 2 } else { 1 },
        })
    }

    /// Song rows on the board.
    pub fn entries(&self) -> usize {
        self.columns * self.board.h.saturating_sub(3)
    }

    /// The board's header row.
    pub fn board_header(&self) -> Area {
        let inside = self.board.inside();
        Area::new(inside.x + 1, inside.y, inside.w.saturating_sub(2), 1)
    }

    /// Song row `i` on the board: down the first column, then the next.
    pub fn entry(&self, i: usize) -> Area {
        let rows = self.board.h.saturating_sub(3).max(1);
        let inside = self.board.inside();
        let width = (inside.w.saturating_sub(2) + 2) / self.columns - 2;
        Area::new(
            inside.x + 1 + (i / rows) * (width + 2),
            inside.y + 1 + i % rows,
            width,
            1,
        )
    }

    /// Where the signposts of songs starting `ahead` from now stand: the first column of
    /// each sign (`widths` wide), or `None` when it does not fit. The next song's sign is
    /// at the horse's nose when it starts; later ones are further along the track.
    pub fn signposts(&self, ahead: &[Option<Duration>], widths: &[usize]) -> Vec<Option<usize>> {
        let nose = self.horse_x + self.size * 0.95;
        let end = self.width as f32 - 3.0;
        let mut free = 0;
        ahead
            .iter()
            .zip(widths)
            .map(|(at, &w)| {
                let secs = (*at)?.as_secs_f32();
                let x = nose + (end - nose) * secs / (secs + HALF_WAY);
                let col = (x - w as f32 / 2.0).round().max(1.0) as usize;
                // Signs keep a column of track between them and stay inside the picture.
                (col >= free && col + w < self.width).then(|| {
                    free = col + w + 2;
                    col
                })
            })
            .collect()
    }

    /// The middle of the horse's body, standing.
    fn home(&self) -> (f32, f32) {
        (self.horse_x, self.ground - 0.72 * self.size)
    }

    /// Where the plane is at `u` (0–1) through its flight: up from the horse, a loop in the
    /// sky, and back down.
    fn flight(&self, u: f32) -> (f32, f32) {
        let f = self.field.px(2);
        let (cx, cy) = (self.width as f32 * 0.56, f.y as f32 + f.h as f32 * 0.4);
        let (rx, ry) = (self.width as f32 * 0.22, f.h as f32 * 0.28);
        let a = PI / 2.0 - TAU * u;
        let (ex, ey) = (cx + rx * a.cos(), cy + ry * a.sin());
        // Starts and ends at the horse, blending into the loop in between.
        let home = self.home();
        let k = (PI * u).cos().powi(2);
        (ex + (home.0 - cx) * k, ey + (home.1 - (cy + ry)) * k)
    }
}

/// What the picture shows besides the ride itself.
#[derive(Debug, Clone, Copy)]
pub struct Scene<'a> {
    /// The signposts: first column and width of each sign (labels go on them).
    pub signs: &'a [(usize, usize)],
    /// Board rows with a song on them, and the selected one.
    pub filled: usize,
    pub selected: Option<usize>,
}

/// The moving parts: the gallop, the ground going by, and the plane trick.
#[derive(Debug, Default)]
pub struct Horse {
    /// Gallop to the beat of the music rather than at a steady pace.
    pub beat_match: bool,
    /// Seconds since the scene started (wraps after an hour).
    clock: f32,
    /// Gallop: where in its stride the horse is (in strides; it lands at 0) and how fast it
    /// goes (strides per second).
    stride: f32,
    speed: f32,
    /// How far the track has gone by, in strides.
    travelled: f32,
    /// The beat in the bass, timed by `time` (which does not wrap).
    beats: BeatDetector,
    time: f32,
    /// When the last beat came, how many there have been, and how much of a stride the
    /// horse still has to catch up to land on the beat.
    last_beat: Option<f32>,
    beat_count: u64,
    catch_up: f32,
    /// Seconds between the last two beats: the tempo until the detector has its own.
    gap: Option<f32>,
    /// 1 on a beat (or, without beat matching, on landing), fading fast: the horse kicks up
    /// dust and tosses its mane.
    kick: f32,
    song: Option<u64>,
    /// Progress (0–1) of the plane trick.
    trick: Option<f32>,
}

impl Horse {
    /// Starts with `song` playing, without a trick.
    pub fn reset(&mut self, song: Option<u64>) {
        self.song = song;
        self.trick = None;
    }

    /// Advances by `dt` seconds. A new `song` starts the plane trick. While `playing`, the
    /// horse gallops; with beat matching, to the beats in the `bass` loudness (0–1), and
    /// it walks when there are none.
    pub fn update(&mut self, dt: f32, song: Option<u64>, playing: bool, bass: f32) {
        let dt = dt.clamp(0.0, 0.25);
        self.clock = (self.clock + dt) % 3600.0;
        self.time += dt;
        if song != self.song {
            if song.is_some() {
                self.trick = Some(0.0);
            }
            self.song = song;
            // A new song has its own tempo.
            self.beats.reset_tempo();
        }
        if let Some(t) = self.trick.as_mut() {
            *t += dt / TRICK_SECS;
            if *t >= 1.0 {
                self.trick = None;
            }
        }
        self.kick *= (-dt * 7.0).exp();
        if playing && self.beat_match && self.beats.update(self.time, dt, bass) {
            if let Some(gap) = self.last_beat.map(|t| self.time - t)
                && (0.25..2.0).contains(&gap)
            {
                self.gap = Some(gap);
            }
            self.last_beat = Some(self.time);
            self.beat_count += 1;
            self.kick = 1.0;
            self.catch_up = -self.off_beat();
        }
        let target = match (playing, self.beat_match, self.hears_beat()) {
            (false, ..) => 0.0,
            (true, false, _) => STEADY,
            (true, true, false) => WALK,
            (true, true, true) => self.tempo().unwrap_or(GALLOP),
        };
        self.speed += (target - self.speed) * (1.0 - (-dt * 6.0).exp());
        if target == 0.0 && self.speed < 0.02 {
            self.speed = 0.0;
        }
        // Catch up with the beat quickly, rather than jump; never running backwards.
        let nudge = (self.catch_up * (1.0 - (-dt * 12.0).exp())).max(-0.7 * self.speed * dt);
        self.catch_up -= nudge;
        let before = self.stride;
        self.stride = (self.stride + self.speed * dt + nudge).rem_euclid(1.0);
        // Without beat matching, each landing kicks instead of each beat.
        if !self.beat_match && self.speed > 0.0 && self.stride < before {
            self.beat_count += 1;
            self.kick = 1.0;
        }
        // The track goes by as fast as the hooves on it push back (less at a walk).
        self.travelled = (self.travelled + self.speed * self.amp() * dt) % 10_000.0;
    }

    /// Seconds between beats, as far as is known.
    fn beat_secs(&self) -> Option<f32> {
        self.beats.bpm().map(|bpm| 60.0 / bpm).or(self.gap)
    }

    fn hears_beat(&self) -> bool {
        let lost = self
            .beat_secs()
            .map_or(BEAT_LOST_MIN, |secs| (secs * BEATS_LOST).max(BEAT_LOST_MIN));
        self.last_beat.is_some_and(|t| self.time - t < lost)
    }

    /// The tempo the horse gallops to, in beats per minute (with beat matching, while
    /// there is a beat).
    pub fn bpm(&self) -> Option<f32> {
        self.beats
            .bpm()
            .filter(|_| self.beat_match && self.hears_beat())
    }

    /// Strides per second for the tempo heard: one per beat, folded into a horse's pace.
    /// From the second beat on, before the detector has worked out the tempo, it follows
    /// the gap between the last two beats.
    fn tempo(&self) -> Option<f32> {
        let mut pace = 1.0 / self.beat_secs()?;
        while pace > PACE.1 {
            pace /= 2.0;
        }
        while pace < PACE.0 {
            pace *= 2.0;
        }
        Some(pace)
    }

    /// How far (in strides, -0.5..0.5) the horse is from landing, which should happen on a
    /// beat. With two beats to a stride it may land on either. Beats are noticed a little
    /// after they are heard, so by then the horse should be just past landing.
    fn off_beat(&self) -> f32 {
        let per_stride = match (self.beat_secs(), self.tempo()) {
            (Some(secs), Some(pace)) => (1.0 / secs / pace).round().max(1.0),
            _ => 1.0,
        };
        let landed = self.stride - DETECT_LAG * self.tempo().unwrap_or(self.speed);
        ((landed * per_stride + 0.5).rem_euclid(1.0) - 0.5) / per_stride
    }

    pub fn is_flying(&self) -> bool {
        self.trick.is_some()
    }

    /// Whether the picture moves on its own: galloping or doing the trick.
    pub fn is_active(&self) -> bool {
        self.trick.is_some() || self.speed > 0.0
    }

    /// What the banner says during the plane trick.
    pub fn caption(&self) -> Option<&'static str> {
        let t = self.trick?;
        Some(if t < FLIGHT.0 + 0.04 {
            "*poof*"
        } else if t < POOF_IN.0 {
            "LOOK! MY HORSE IS A PLANE NOW!"
        } else {
            "...AND A HORSE AGAIN. AMAZING."
        })
    }

    /// The rainbow shifts a step at a time (so the banner does not redraw every frame).
    pub fn rainbow_shift(&self) -> f32 {
        (self.clock * 0.25 * 16.0).floor() / 16.0
    }

    /// How hard the horse gallops: 0 standing, 1 at full speed.
    fn amp(&self) -> f32 {
        (self.speed / GALLOP).min(1.0)
    }

    /// Draws the picture into `canvas`: one pixel per column, two per row.
    pub fn draw(&self, canvas: &mut Canvas, plan: &Plan, scene: &Scene<'_>) {
        Pen::new(canvas).fill(plan.banner.px(2), BANNER);
        self.draw_field(canvas, plan, scene);
        let mut pen = Pen::new(canvas);
        pen.fill(plan.rail.px(2), RAIL);
        // The board stands in the grass.
        pen.fill(plan.board.px(2), MEADOW_LOW);
        let inside = frame(canvas, plan.board, 2, WOOD_FRAME);
        let mut pen = Pen::new(canvas);
        pen.fill(inside, BOARD);
        if let Some(i) = scene.selected.filter(|&i| i < scene.filled) {
            let row = plan.entry(i);
            pen.fill(
                Area::new(row.x.saturating_sub(1), row.y, row.w + 2, 1).px(2),
                BOARD_LIT,
            );
        }
    }

    fn draw_field(&self, canvas: &mut Canvas, plan: &Plan, scene: &Scene<'_>) {
        self.draw_land(canvas, plan, scene);
        // The speech bubble, with its tail to the horse's mouth (not while it is a plane).
        if let Some(bubble) = plan.bubble
            && self.trick.is_none()
        {
            let inside = frame(canvas, bubble, 2, [OUTLINE; 3]);
            let mut pen = Pen::new(canvas);
            pen.fill(inside, BUBBLE);
            let px = bubble.px(2);
            let (left, low) = (px.x as f32, (px.y + px.h - 3) as f32);
            let mouth = (
                plan.horse_x + plan.size * 0.95,
                plan.ground - plan.size * 0.78,
            );
            let tip = (mouth.0.min(left - 2.0), mouth.1);
            pen.triangle((left + 1.0, low - 3.0), (left + 1.0, low), tip, BUBBLE);
            pen.line(left, low - 3.0, tip.0, tip.1, OUTLINE);
            pen.line(left, low + 0.5, tip.0, tip.1, OUTLINE);
        }
        let mut pen = Pen::new(canvas);
        pen.clip = plan.field.px(2);
        self.draw_rider(&mut pen, plan);
    }

    /// The sky, the meadow, the track and the signposts.
    fn draw_land(&self, canvas: &mut Canvas, plan: &Plan, scene: &Scene<'_>) {
        let f = plan.field.px(2);
        let mut pen = Pen::new(canvas);
        pen.clip = f;
        let (top, height) = (f.y as f32, f.h as f32);
        let horizon = (top + height * 0.45).round() as usize;
        let s = plan.size;
        // How far the ground has moved, in whole pixels: near things move with it, far
        // things slower.
        let moved = (self.travelled * s * GROUND) as usize;

        // Sky, and a rainbow over the meadow.
        for y in f.y..horizon {
            let k = (y - f.y) as f32 / (horizon - f.y).max(1) as f32;
            pen.fill(Area::new(f.x, y, f.w, 1), mix(BANNER, SKY_LOW, k));
        }
        let (rcx, rcy, radius) = (
            plan.width as f32 * 0.68,
            horizon as f32 + 2.0,
            height * 0.62,
        );
        let band = (radius * 0.07).max(1.0);
        for y in f.y..horizon {
            for x in f.x..f.x + f.w {
                let d = ((x as f32 + 0.5 - rcx).powi(2) + (y as f32 + 0.5 - rcy).powi(2)).sqrt();
                let b = (radius - d) / band;
                if (0.0..6.0).contains(&b) {
                    pen.blend(x as f32, y as f32, hsv(b.floor() / 7.0, 0.75, 1.0), 0.5);
                }
            }
        }
        let sun = (plan.width as f32 * 0.9, top + height * 0.16);
        let sun_r = (height * 0.08).max(2.0);
        pen.glow(sun.0, sun.1, sun_r * 2.6, sun_r * 2.6, SUN, 0.45);
        pen.ellipse(sun.0, sun.1, sun_r, sun_r, SUN);
        // Clouds drift by.
        let drift = (self.clock * 1.2) as usize + moved / 20;
        let span = plan.width + 40;
        for i in 0..4u64 {
            let x = ((noise(i * 7 + 1) * span as f32) as usize + span - drift % span) % span;
            let (x, y) = (
                x as f32 - 20.0,
                top + height * (0.06 + 0.16 * noise(i * 13 + 5)),
            );
            let w = s * (0.35 + 0.3 * noise(i * 3 + 2)) + 3.0;
            pen.ellipse(x, y + 1.0, w, w * 0.32, CLOUD_SHADE);
            pen.ellipse(x - w * 0.35, y, w * 0.45, w * 0.3, CLOUD);
            pen.ellipse(x + w * 0.25, y - w * 0.12, w * 0.5, w * 0.38, CLOUD);
        }

        // Rolling hills, then the meadow down to the bottom.
        for x in f.x..f.x + f.w {
            let wx = (x + moved / 12) as f32;
            let rise = ((wx * 0.045).sin() * 0.5 + 0.5) * height * 0.09
                + ((wx * 0.11 + 2.0).sin() * 0.5 + 0.5) * height * 0.04;
            let from = (horizon as f32 - rise).round() as usize;
            pen.fill(Area::new(x, from, 1, horizon - from), HILL);
        }
        let bottom = f.y + f.h;
        for y in horizon..bottom {
            let k = (y - horizon) as f32 / (bottom - horizon).max(1) as f32;
            pen.fill(Area::new(f.x, y, f.w, 1), mix(MEADOW, MEADOW_LOW, k));
        }
        // A white fence along the far side of the meadow.
        let ground = plan.ground;
        let fence = (horizon as f32 + (ground - horizon as f32) * 0.3).round() as usize;
        pen.fill(Area::new(f.x, fence, f.w, 1), FENCE);
        pen.fill(Area::new(f.x, fence + 2, f.w, 1), FENCE);
        for x in f.x..f.x + f.w {
            if (x + moved / 2).is_multiple_of(10) {
                pen.fill(Area::new(x, fence.saturating_sub(1), 1, 5), FENCE);
            }
        }
        // The track, with pebbles going by.
        let track_h = (height * 0.07).max(2.0);
        let (t0, t1) = (
            (ground - track_h).round() as usize,
            (ground + track_h * 0.7).round() as usize,
        );
        pen.fill(Area::new(f.x, t0, f.w, t1 - t0), TRACK);
        pen.fill(Area::new(f.x, t0, f.w, 1), TRACK_EDGE);
        pen.fill(Area::new(f.x, t1, f.w, 1), TRACK_EDGE);
        for x in f.x..f.x + f.w {
            let wx = (x + moved) as u64;
            if noise(wx.wrapping_mul(31)) > 0.86 {
                let y = t0 + 1 + (noise(wx) * (t1 - t0 - 1).max(1) as f32) as usize;
                pen.put(x as i64, y as i64, TRACK_EDGE);
            }
            // Grass tufts and flowers in front of the track.
            let n = noise(wx.wrapping_mul(97) + 3);
            let y = t1 + 2 + (noise(wx + 11) * (bottom.saturating_sub(t1 + 3)) as f32) as usize;
            if n > 0.9 {
                pen.put(x as i64, y as i64, GRASS);
                pen.put(x as i64 - 1, y as i64 - 1, GRASS);
                pen.put(x as i64 + 1, y as i64 - 1, GRASS);
            } else if n > 0.875 {
                pen.put(x as i64, y as i64, hsv(noise(wx + 5), 0.6, 1.0));
            }
        }

        // Signposts for the songs ahead: a post down to the track, a board for the label.
        for &(col, w) in scene.signs {
            let board = Area::new(col, plan.sign_row, w, 1).px(2);
            let mid = (col + w / 2) as f32;
            pen.fill_f(
                mid,
                (board.y + board.h) as f32,
                1.0,
                ground - (board.y + board.h) as f32,
                POST,
            );
            pen.fill(
                Area::new(col.saturating_sub(1), board.y - 1, w + 2, board.h + 2),
                POST,
            );
            pen.fill(board, SIGN);
        }
    }

    /// The horse, or the plane it turned into, and its sparkles.
    fn draw_rider(&self, pen: &mut Pen<'_>, plan: &Plan) {
        let s = plan.size;
        let home = plan.home();
        match self.trick {
            Some(t) => {
                // The horse shows until the first puff hides it, and after the last one.
                if t < POOF_OUT.0 + (POOF_OUT.1 - POOF_OUT.0) * 0.5 || t > POOF_IN.0 + 0.05 {
                    self.draw_horse(pen, plan);
                }
                let u = phase(t, FLIGHT);
                if u > 0.0 && u < 1.0 {
                    self.draw_plane(pen, plan, u);
                }
                // Puffs of smoke where the horse changes.
                for (step, k) in [(POOF_OUT, phase(t, POOF_OUT)), (POOF_IN, phase(t, POOF_IN))] {
                    if t >= step.0 && t <= step.1 {
                        let size = (k * PI).sin();
                        for i in 0..7 {
                            let a = i as f32 * TAU / 7.0 + k;
                            let d = s * 0.4 * (0.5 + k);
                            pen.ellipse(
                                home.0 + a.cos() * d,
                                home.1 + a.sin() * d * 0.7,
                                s * 0.26 * size + 0.5,
                                s * 0.22 * size + 0.5,
                                CLOUD,
                            );
                        }
                    }
                }
            }
            None => self.draw_horse(pen, plan),
        }
        // Sparkles on the beat: an amazing horse twinkles.
        if self.trick.is_none() && self.kick > 0.25 {
            for i in 0..5u64 {
                let n = self.beat_count * 5 + i;
                if noise(n * 3 + 1) < 0.3 {
                    continue;
                }
                let x = home.0 + (noise(n * 7) - 0.3) * s * 1.6;
                let y = home.1 - s * 0.2 + (noise(n * 11) - 0.6) * s * 1.1;
                let c = if i % 2 == 0 { [0xff; 3] } else { SUN };
                pen.put(x as i64, y as i64, c);
                for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                    pen.blend(x + dx as f32, y + dy as f32, c, 0.6);
                }
            }
        }
    }

    /// The horse, with dust kicked up behind it on the beat.
    fn draw_horse(&self, pen: &mut Pen<'_>, plan: &Plan) {
        let (amp, kick) = (self.amp(), self.kick);
        if kick > 0.05 && amp > 0.4 {
            let (s, (x, _)) = (plan.size, plan.home());
            let spread = 1.0 - kick;
            for i in 0..4 {
                let i = i as f32;
                let r = s * (0.06 + 0.12 * spread) + 0.5;
                let px = x - s * (0.45 + 0.7 * spread + 0.14 * i);
                let py = plan.ground - s * (0.04 + 0.12 * spread + 0.05 * i);
                pen.ellipse_with(px, py, r, r * 0.8, |_, _| Some((DUST, 0.75 * kick)));
            }
        }
        draw_horse(pen, plan, self.stride, amp, kick, self.clock);
    }

    fn draw_plane(&self, pen: &mut Pen<'_>, plan: &Plan, u: f32) {
        let s = plan.size;
        // A rainbow trail behind it.
        for k in (1..=16).rev() {
            let back = u - k as f32 * 0.012;
            if back <= 0.0 {
                continue;
            }
            let (x, y) = plan.flight(back);
            let r = s * 0.06 * (1.0 + k as f32 * 0.06) + 0.5;
            let c = hsv(k as f32 / 16.0, 0.7, 1.0);
            pen.ellipse_with(x, y, r, r, |_, _| Some((c, 0.8 * (1.0 - k as f32 / 17.0))));
        }
        let (x, y) = plan.flight(u);
        let ahead = plan.flight((u + 0.01).min(1.0));
        let behind = plan.flight((u - 0.01).max(0.0));
        let heading = (ahead.1 - behind.1).atan2(ahead.0 - behind.0);
        draw_plane(pen, x, y, heading, s, self.clock);
    }
}

/// One leg of the gallop: where it hangs from the body, and when its hoof is down.
struct Leg {
    /// Along the body from its middle, in sizes (forward is positive).
    along: f32,
    front: bool,
    /// On the near side: drawn in front of the body.
    near: bool,
    /// When in the stride its hoof strikes the ground, and for how much of the stride it
    /// stays down.
    strike: f32,
    stance: f32,
    /// How far forward of its shoulder or hip (in sizes) the hoof lands.
    reach: f32,
}

/// How fast a hoof on the ground moves back past the body, in sizes per stride. The track
/// goes by as fast, so hooves on the ground stay put.
const GROUND: f32 = 1.6;

/// A transverse gallop, timed after the classic twelve-phase chart (Muybridge's horse in
/// motion): the first hind hoof lands on the beat (1), the second follows (3), the front
/// legs land one after the other (4 and 7) while the hind legs trail behind, the last
/// front hoof pushes off (10), and all four legs fold under the body in mid-air (11-12).
const LEGS: [Leg; 4] = [
    Leg {
        along: -0.38,
        front: false,
        near: false,
        strike: 0.0,
        stance: 0.28,
        reach: 0.1,
    },
    Leg {
        along: 0.3,
        front: true,
        near: false,
        strike: 0.25,
        stance: 0.37,
        reach: 0.25,
    },
    Leg {
        along: -0.32,
        front: false,
        near: true,
        strike: 0.12,
        stance: 0.26,
        reach: 0.08,
    },
    Leg {
        along: 0.36,
        front: true,
        near: true,
        strike: 0.5,
        stance: 0.28,
        reach: 0.22,
    },
];

/// A smooth curve through `keys` (time 0-1, x, y), at time `t`.
fn curve(keys: &[(f32, f32, f32)], t: f32) -> (f32, f32) {
    let i = keys
        .windows(2)
        .position(|w| t <= w[1].0)
        .unwrap_or(keys.len() - 2);
    let (a, b) = (keys[i], keys[i + 1]);
    // Catmull-Rom tangents (one-sided at the ends), for uneven key times.
    let slope = |p: (f32, f32, f32), q: (f32, f32, f32)| {
        let dt = (q.0 - p.0).max(1e-4);
        ((q.1 - p.1) / dt, (q.2 - p.2) / dt)
    };
    let ta = if i > 0 {
        slope(keys[i - 1], b)
    } else {
        slope(a, b)
    };
    let tb = keys.get(i + 2).map_or(slope(a, b), |&c| slope(a, c));
    let h = (b.0 - a.0).max(1e-4);
    let u = ((t - a.0) / h).clamp(0.0, 1.0);
    let (u2, u3) = (u * u, u * u * u);
    let (h00, h10, h01, h11) = (
        2.0 * u3 - 3.0 * u2 + 1.0,
        u3 - 2.0 * u2 + u,
        -2.0 * u3 + 3.0 * u2,
        u3 - u2,
    );
    (
        h00 * a.1 + h10 * h * ta.0 + h01 * b.1 + h11 * h * tb.0,
        h00 * a.2 + h10 * h * ta.1 + h01 * b.2 + h11 * h * tb.1,
    )
}

/// Where a leg's hoof is at `stride` (0-1): how far forward of its shoulder or hip and how
/// high off the ground, in sizes. While down it moves back as the body goes over it. Then
/// a front leg curls up under the chest and reaches far forward to land; a hind leg trails
/// up behind, folds and swings forward under the belly.
fn hoof(leg: &Leg, stride: f32) -> (f32, f32) {
    let since = (stride - leg.strike).rem_euclid(1.0);
    let sweep = GROUND * leg.stance;
    let off = leg.reach - sweep;
    if since < leg.stance {
        return (leg.reach - sweep * since / leg.stance, 0.0);
    }
    let k = (since - leg.stance) / (1.0 - leg.stance);
    let keys: [(f32, f32, f32); 5] = if leg.front {
        [
            (0.0, off, 0.0),
            (0.3, -0.08, 0.36),
            (0.55, 0.12, 0.3),
            (0.8, leg.reach + 0.2, 0.18),
            (1.0, leg.reach, 0.0),
        ]
    } else {
        [
            (0.0, off, 0.0),
            (0.28, off - 0.06, 0.22),
            (0.62, -0.12, 0.28),
            (0.86, leg.reach + 0.06, 0.12),
            (1.0, leg.reach, 0.0),
        ]
    };
    let (x, up) = curve(&keys, k);
    (x, up.max(0.0))
}

/// The knee of a leg from `top` to `hoof`, `upper` and `lower` long, bending forward (front
/// legs) or back (hind legs, at the hock); and the hoof, pulled in if out of reach.
fn knee(
    top: (f32, f32),
    hoof: (f32, f32),
    upper: f32,
    lower: f32,
    forward: bool,
) -> [(f32, f32); 2] {
    let (dx, dy) = (hoof.0 - top.0, hoof.1 - top.1);
    let reach = (dx * dx + dy * dy).sqrt().max(1e-4);
    let d = reach.clamp((upper - lower).abs() + 0.01, (upper + lower) * 0.999);
    let theta = dy.atan2(dx);
    let bend = ((upper * upper + d * d - lower * lower) / (2.0 * upper * d))
        .clamp(-1.0, 1.0)
        .acos();
    let a = if forward { theta - bend } else { theta + bend };
    [
        (top.0 + a.cos() * upper, top.1 + a.sin() * upper),
        (top.0 + dx / reach * d, top.1 + dy / reach * d),
    ]
}

/// The horse, galloping (`amp` 1) or standing (0), facing right. `kick` (1 on a beat)
/// tosses its head and mane.
fn draw_horse(pen: &mut Pen<'_>, plan: &Plan, stride: f32, amp: f32, kick: f32, clock: f32) {
    let s = plan.size;
    let (cx, ground) = (plan.horse_x, plan.ground);
    // Highest in mid-air, coming down onto the hind legs and lowest over the front legs;
    // the forehand up while the hind legs push, down as the front legs land.
    let rise = 0.5 - 0.5 * (TAU * (stride - 0.45)).cos();
    let pitch = (0.08 * (TAU * (stride - 0.15)).cos() + 0.03 * kick) * amp;
    let height = lerp(0.62, 0.56, amp) + 0.05 * rise * amp;
    let cy = ground - (0.1 + height) * s;
    let (sin, cos) = pitch.sin_cos();
    // A point `along` the body and `up` from its middle, in sizes, rocked by the pitch.
    let at = |along: f32, up: f32| {
        (
            cx + (along * cos - up * sin) * s,
            cy - (along * sin + up * cos) * s,
        )
    };
    let flick = 0.5 + amp + 1.5 * kick;

    let leg = |pen: &mut Pen<'_>, leg: &Leg| {
        let top = at(leg.along, -0.1);
        let (upper, lower) = if leg.front {
            (0.32, 0.35)
        } else {
            (0.34, 0.34)
        };
        let (dx, up) = hoof(leg, stride);
        let stand = if leg.front { 0.03 } else { -0.03 };
        let target = (top.0 + lerp(stand, dx, amp) * s, ground - up * amp * s);
        let [knee, hoof] = knee(top, target, upper * s, lower * s, leg.front);
        let color = if leg.near { COAT } else { COAT_FAR };
        let thick = if leg.front { 0.085 } else { 0.11 };
        pen.stroke(top.0, top.1, knee.0, knee.1, thick * s, 0.05 * s, color);
        pen.stroke(knee.0, knee.1, hoof.0, hoof.1, 0.048 * s, 0.04 * s, color);
        pen.ellipse(
            hoof.0,
            hoof.1 - 0.02 * s,
            0.06 * s + 0.3,
            0.035 * s + 0.3,
            HOOF,
        );
    };
    for l in LEGS.iter().filter(|l| !l.near) {
        leg(pen, l);
    }
    // The tail streams back, a rainbow.
    let mut last = at(-0.5, 0.12);
    for k in 1..=10 {
        let kf = k as f32;
        let wave = (clock * 7.0 + kf * 0.6).sin() * 0.012 * s * kf * flick;
        let next = (
            last.0 - 0.07 * s,
            last.1 + (0.035 - 0.03 * amp) * s + wave * 0.3,
        );
        let r = 0.075 * s * (1.0 - kf / 14.0);
        let c = hsv(kf / 10.0, 0.75, 1.0);
        pen.stroke(last.0, last.1 + wave, next.0, next.1 + wave, r, r, c);
        last = next;
    }
    // Body: belly in shade, rounder at the chest and rump.
    let body = |_: f32, v: f32| Some(mix(COAT, COAT_SHADE, ((v - 0.1) * 1.4).clamp(0.0, 1.0)));
    let rump = at(-0.33, 0.04);
    pen.ellipse_turned(rump.0, rump.1, 0.22 * s, 0.23 * s, -pitch, body);
    let chest = at(0.3, 0.02);
    pen.ellipse_turned(chest.0, chest.1, 0.2 * s, 0.22 * s, -pitch, body);
    pen.ellipse_turned(cx, cy, 0.5 * s, 0.2 * s, -pitch, body);
    // The neck reaches forward at a gallop and rises when standing; the head nods down on
    // the beat.
    let base = at(0.32, 0.06);
    let neck = lerp(0.95, 0.62, amp) + 0.6 * pitch - 0.1 * kick * amp;
    let poll = (
        base.0 + neck.cos() * 0.42 * s,
        base.1 - neck.sin() * 0.42 * s,
    );
    pen.stroke(base.0, base.1, poll.0, poll.1, 0.18 * s, 0.1 * s, COAT);
    let tilt = lerp(1.05, 0.75, amp) - 0.4 * pitch + 0.15 * kick * amp;
    let (ts, tc) = tilt.sin_cos();
    let head = (poll.0 + tc * 0.17 * s, poll.1 + ts * 0.17 * s);
    pen.ellipse_turned(head.0, head.1, 0.2 * s, 0.085 * s, tilt, |u, _| {
        Some(if u > 0.55 { MUZZLE } else { COAT })
    });
    // Ears laid back at a gallop.
    let ear = lerp(0.0, 0.07, amp) * s;
    pen.triangle(
        (poll.0 - 0.03 * s, poll.1 - 0.02 * s),
        (poll.0 + 0.04 * s, poll.1 - 0.04 * s),
        (poll.0 - 0.02 * s - ear, poll.1 - 0.16 * s + ear * 0.5),
        COAT,
    );
    let eye = (
        head.0 - tc * 0.05 * s + ts * 0.03 * s,
        head.1 - ts * 0.05 * s - tc * 0.03 * s,
    );
    pen.put(eye.0 as i64, eye.1 as i64, EYE);
    // The mane streams back from the crest of the neck.
    let crest = (-neck.sin(), -neck.cos());
    for k in 0..7 {
        let kf = k as f32 / 6.0;
        let r = lerp(0.1, 0.18, kf) * 0.85 * s;
        let (bx, by) = (
            lerp(poll.0, base.0, kf) + crest.0 * r,
            lerp(poll.1, base.1, kf) + crest.1 * r,
        );
        let wave = (clock * 9.0 + k as f32).sin() * 0.04 * s * flick;
        let c = hsv(kf * 0.85, 0.75, 1.0);
        pen.stroke(
            bx,
            by,
            bx - 0.24 * s,
            by - 0.03 * s + wave,
            0.05 * s,
            0.025 * s,
            c,
        );
    }
    for l in LEGS.iter().filter(|l| l.near) {
        leg(pen, l);
    }
}

/// A propeller plane with a horse's head and a rainbow stripe, flying along `heading`.
fn draw_plane(pen: &mut Pen<'_>, x: f32, y: f32, heading: f32, s: f32, clock: f32) {
    let (sin, cos) = heading.sin_cos();
    // A point `fwd` along the plane and `up` above it, in sizes.
    let at = |fwd: f32, up: f32| {
        (
            x + (cos * fwd + sin * up) * s,
            y + (sin * fwd - cos * up) * s,
        )
    };
    let wing = at(-0.02, 0.02);
    pen.ellipse_turned(wing.0, wing.1, 0.2 * s, 0.07 * s, heading - 0.3, |_, _| {
        Some(COAT_FAR)
    });
    pen.triangle(at(-0.52, 0.0), at(-0.34, 0.04), at(-0.52, 0.32), COAT_SHADE);
    pen.ellipse_turned(x, y, 0.52 * s, 0.15 * s, heading, |_, v| {
        Some(if v > 0.35 { COAT_SHADE } else { COAT })
    });
    for k in 0..12 {
        let p = at(lerp(-0.42, 0.36, k as f32 / 11.0), -0.01);
        pen.ellipse(
            p.0,
            p.1,
            0.035 * s + 0.4,
            0.035 * s + 0.4,
            hsv(k as f32 / 12.0, 0.8, 1.0),
        );
    }
    let window = at(0.16, 0.08);
    pen.ellipse(
        window.0,
        window.1,
        0.06 * s + 0.3,
        0.04 * s + 0.3,
        [0x8e, 0xc8, 0xf0],
    );
    let wing = at(0.04, -0.06);
    pen.ellipse_turned(
        wing.0,
        wing.1,
        0.22 * s,
        0.075 * s,
        heading + 0.25,
        |_, v| Some(if v > 0.4 { COAT_SHADE } else { COAT }),
    );
    // The horse's head is the nose.
    let head = at(0.56, 0.1);
    pen.ellipse_turned(head.0, head.1, 0.18 * s, 0.09 * s, heading + 0.5, |u, _| {
        Some(if u > 0.55 { MUZZLE } else { COAT })
    });
    pen.triangle(at(0.44, 0.16), at(0.5, 0.17), at(0.45, 0.3), COAT);
    let eye = at(0.52, 0.14);
    pen.put(eye.0 as i64, eye.1 as i64, EYE);
    // The propeller, spinning.
    let hub = at(0.74, 0.02);
    let blade = if (clock * 24.0) as i64 % 2 == 0 {
        0.2
    } else {
        0.08
    };
    let (a, b) = (at(0.74, 0.02 + blade), at(0.74, 0.02 - blade));
    pen.line(a.0, a.1, b.0, b.1, [0x55, 0x55, 0x5c]);
    pen.ellipse(hub.0, hub.1, 0.6, 0.6, RED);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(horse: &Horse, plan: &Plan, signs: &[(usize, usize)]) -> Canvas {
        let mut canvas = Canvas::new(plan.width, (plan.board.y + plan.board.h) * 2, [0; 3]);
        let scene = Scene {
            signs,
            filled: 3,
            selected: Some(1),
        };
        horse.draw(&mut canvas, plan, &scene);
        canvas
    }

    fn count(canvas: &Canvas, area: Area, color: Rgb) -> usize {
        (area.y..area.y + area.h)
            .flat_map(|y| (area.x..area.x + area.w).map(move |x| (x, y)))
            .filter(|&(x, y)| canvas.get(x, y) == color)
            .count()
    }

    #[test]
    fn plan_fits_the_parts_in_order() {
        let plan = Plan::new(120, 28).unwrap();
        assert_eq!(plan.field.y, 1);
        assert_eq!(plan.rail.y, plan.field.y + plan.field.h);
        assert_eq!(plan.board.y, plan.rail.y + 1);
        assert_eq!(plan.board.y + plan.board.h, 28);
        assert!(plan.entries() >= 5);
        let bubble = plan.bubble.expect("room for a speech bubble");
        assert!(
            bubble.y + bubble.h <= plan.sign_row,
            "the bubble is above the signs"
        );
        assert!(
            bubble.x as f32 > plan.horse_x + plan.size,
            "and right of the horse"
        );
        assert!(plan.ground < (plan.rail.y * 2) as f32);
        // Entries go down the board, then across on wide terminals.
        assert_eq!(plan.entry(1).y, plan.entry(0).y + 1);
        let wide = Plan::new(200, 50).unwrap();
        assert_eq!(wide.columns, 2);
        let rows = wide.board.h - 3;
        assert_eq!(wide.entry(rows).y, wide.entry(0).y);
        assert!(wide.entry(rows).x > wide.entry(0).x + wide.entry(0).w);
        assert!(Plan::new(39, 30).is_none() && Plan::new(100, 11).is_none());
    }

    #[test]
    fn signposts_stand_by_the_time_until_their_song() {
        let plan = Plan::new(120, 28).unwrap();
        let secs = |s| Some(Duration::from_secs(s));
        let posts = plan.signposts(&[secs(0), secs(200), secs(205), secs(900), None], &[4; 5]);
        let nose = plan.horse_x + plan.size * 0.95;
        let first = posts[0].unwrap();
        assert!(
            (first as f32 + 2.0 - nose).abs() <= 1.0,
            "the next song is at the nose"
        );
        assert!(posts[1].unwrap() > first + 20);
        assert_eq!(posts[2], None, "too close to the one before");
        assert!(posts[3].unwrap() > posts[1].unwrap());
        assert!(posts[3].unwrap() + 4 < 120);
        assert_eq!(posts[4], None, "unknown start");
    }

    /// Bass loudness at `t` with a kick every `period` seconds, decaying over ~0.1 s, on
    /// top of a steady bed (as in HYPERSPACE's beat test).
    fn kick(t: f32, period: f32) -> f32 {
        0.5 + 0.45 * (-(t % period) * 30.0).exp()
    }

    /// Plays `secs` of music with a kick every `period` seconds (or none), at 60 fps.
    /// Returns how far off the beat the horse was at each kick after the first 4 s.
    fn ride(horse: &mut Horse, from: f32, secs: f32, period: Option<f32>) -> Vec<f32> {
        horse.beat_match = true;
        let dt = 1.0 / 60.0;
        let mut offs = Vec::new();
        for frame in 0..(secs / dt) as usize {
            let t = from + frame as f32 * dt;
            let bass = period.map_or(0.6, |p| kick(t, p));
            let before = horse.beat_count;
            horse.update(dt, Some(1), true, bass);
            if horse.beat_count > before && t - from > 4.0 {
                // The phase the beat found, before catching up.
                offs.push(-horse.catch_up);
            }
        }
        offs
    }

    #[test]
    fn by_default_it_gallops_fast_and_ignores_the_beat() {
        let mut horse = Horse::default();
        assert!(!horse.beat_match);
        horse.reset(Some(1));
        let dt = 1.0 / 60.0;
        for frame in 0..240 {
            let t = frame as f32 * dt;
            horse.update(dt, Some(1), true, kick(t, 0.5));
        }
        assert!((horse.speed - STEADY).abs() < 0.01, "{}", horse.speed);
        assert_eq!(horse.bpm(), None, "no tempo shown");
        assert!(horse.amp() >= 1.0);
        // It kicks up dust on every landing: 2.5 strides a second.
        let before = horse.beat_count;
        for _ in 0..120 {
            horse.update(dt, Some(1), true, 0.6);
        }
        assert_eq!(horse.beat_count - before, 5);
        // Paused, it stands.
        for _ in 0..120 {
            horse.update(dt, Some(1), false, 0.6);
        }
        assert!(!horse.is_active());
    }

    #[test]
    fn the_gallop_locks_onto_the_beat() {
        let mut horse = Horse::default();
        horse.reset(Some(1));
        let offs = ride(&mut horse, 0.0, 10.0, Some(0.5));
        assert!(offs.len() >= 10, "{} beats", offs.len());
        let bpm = horse.bpm().expect("tempo");
        assert!((bpm - 120.0).abs() < 3.0, "{bpm} BPM");
        assert!(
            (horse.speed - 2.0).abs() < 0.1,
            "one stride per beat: {}",
            horse.speed
        );
        // Once locked, every beat finds the horse landing.
        let late: Vec<f32> = offs[offs.len() - 6..].to_vec();
        assert!(late.iter().all(|o| o.abs() < 0.06), "{late:?}");
        assert!(horse.amp() > 0.9);
    }

    #[test]
    fn the_second_beat_sets_the_pace_and_every_beat_kicks() {
        let mut horse = Horse::default();
        horse.reset(Some(1));
        // A slow 90 BPM song: three beats in, the detector has no tempo yet.
        ride(&mut horse, 0.0, 1.7, Some(60.0 / 90.0));
        assert_eq!(horse.beats.bpm(), None);
        let pace = horse.tempo().expect("pace from the gap between beats");
        assert!((pace - 1.5).abs() < 0.05, "{pace} strides/s");
        assert!((horse.speed - pace).abs() < 0.2, "already galloping at it");
        // A beat kicks: dust, a mane flick and sparkles.
        let dt = 1.0 / 60.0;
        let mut t = 1.7;
        while horse.kick < 0.9 {
            horse.update(dt, Some(1), true, kick(t, 60.0 / 90.0));
            t += dt;
        }
        for _ in 0..12 {
            horse.update(dt, Some(1), true, 0.5);
        }
        assert!(horse.kick < 0.3, "the kick fades within a beat");
    }

    #[test]
    fn the_hooves_go_down_in_the_order_of_the_gallop_chart() {
        // LEGS: first hind, first front, second hind, second front.
        let down = |p: f32| LEGS.each_ref().map(|l| hoof(l, p).1 == 0.0);
        assert_eq!(
            down(0.02),
            [true, false, false, false],
            "1: first hind lands"
        );
        assert_eq!(down(0.2), [true, false, true, false], "3: both hind legs");
        assert_eq!(down(0.3), [false, true, true, false], "4-5: hind and front");
        assert_eq!(
            down(0.55),
            [false, true, false, true],
            "7-8: both front legs"
        );
        assert_eq!(
            down(0.7),
            [false, false, false, true],
            "9-10: last push off"
        );
        assert_eq!(down(0.85), [false; 4], "11: in mid-air");
        // In mid-air the legs are folded up under the body.
        for l in &LEGS {
            let (dx, up) = hoof(l, 0.88);
            assert!(up > 0.05 && dx.abs() < 0.3, "{dx} {up}");
        }
        // A front leg reaches far forward before it lands (5-6).
        let (dx, up) = hoof(&LEGS[3], 0.4);
        assert!(dx > LEGS[3].reach && up > 0.1, "{dx} {up}");
        // A hoof on the ground moves back as fast as the track goes by, and the path is
        // continuous where it lands and takes off.
        for l in &LEGS {
            let (a, _) = hoof(l, l.strike + 0.02);
            let (b, _) = hoof(l, l.strike + 0.12);
            assert!(((a - b) / 0.1 - GROUND).abs() < 1e-3);
            for p in [l.strike, l.strike + l.stance] {
                let (x0, y0) = hoof(l, p - 1e-3);
                let (x1, y1) = hoof(l, p + 1e-3);
                assert!(
                    (x0 - x1).abs() < 0.01 && (y0 - y1).abs() < 0.01,
                    "jump at {p}"
                );
            }
        }
    }

    #[test]
    fn hooves_on_the_ground_stay_within_reach() {
        let plan = Plan::new(120, 28).unwrap();
        let s = plan.size;
        for i in 0..100 {
            let stride = i as f32 / 100.0;
            let rise = 0.5 - 0.5 * (TAU * (stride - 0.45)).cos();
            let pitch = 0.08 * (TAU * (stride - 0.15)).cos();
            let cy = plan.ground - (0.1 + 0.56 + 0.05 * rise) * s;
            for l in LEGS.iter().filter(|l| hoof(l, stride).1 == 0.0) {
                let (dx, _) = hoof(l, stride);
                let top_y = cy - (l.along * pitch.sin() - 0.1 * pitch.cos()) * s;
                let top_x = plan.horse_x + (l.along * pitch.cos() + 0.1 * pitch.sin()) * s;
                let length = if l.front { 0.67 } else { 0.68 } * s;
                let d = ((top_x + dx * s - top_x).powi(2) + (plan.ground - top_y).powi(2)).sqrt();
                assert!(d <= length, "stride {stride}: hoof {d} of {length} away");
            }
        }
    }

    #[test]
    fn fast_music_is_a_stride_every_two_beats() {
        let mut horse = Horse::default();
        horse.reset(Some(1));
        // 160 BPM.
        let offs = ride(&mut horse, 0.0, 10.0, Some(0.375));
        let pace = horse.tempo().unwrap();
        assert!((pace - 160.0 / 120.0).abs() < 0.05, "{pace} strides/s");
        let late: Vec<f32> = offs[offs.len() - 6..].to_vec();
        assert!(late.iter().all(|o| o.abs() < 0.06), "{late:?}");
    }

    #[test]
    fn without_a_beat_the_horse_slows_to_a_walk() {
        let mut horse = Horse::default();
        horse.reset(Some(1));
        ride(&mut horse, 0.0, 6.0, Some(0.5));
        assert!(horse.speed > 1.8);
        // The beat stops: three beats later it slows down, and soon walks.
        ride(&mut horse, 6.0, 1.3, None);
        assert!(horse.speed < 1.5, "{}", horse.speed);
        ride(&mut horse, 7.3, 1.2, None);
        assert!((horse.speed - WALK).abs() < 0.05, "{}", horse.speed);
        assert_eq!(horse.bpm(), None);
        assert!(horse.amp() < 0.4);
        // Music that never had a beat: a walk from the start.
        let mut horse = Horse::default();
        horse.reset(Some(1));
        ride(&mut horse, 0.0, 4.0, None);
        assert!((horse.speed - WALK).abs() < 0.05);
    }

    #[test]
    fn the_horse_moves_while_playing_and_a_new_song_is_a_plane_trick() {
        let mut horse = Horse::default();
        horse.reset(Some(1));
        for _ in 0..60 {
            horse.update(0.05, Some(1), true, 0.0);
        }
        assert!(horse.is_active() && !horse.is_flying());
        assert_eq!(horse.caption(), None);

        horse.update(0.05, Some(2), true, 0.0);
        assert!(horse.is_flying());
        let mut captions = Vec::new();
        let mut elapsed = 0.0;
        while horse.is_flying() {
            if let Some(c) = horse.caption()
                && captions.last() != Some(&c)
            {
                captions.push(c);
            }
            horse.update(0.05, Some(2), true, 0.0);
            elapsed += 0.05;
        }
        assert!((elapsed - TRICK_SECS).abs() < 0.2, "{elapsed}");
        assert_eq!(captions.len(), 3, "{captions:?}");
        assert!(captions[1].contains("PLANE"));

        // Stopping: no trick; the horse slows down and stands.
        horse.update(0.05, None, false, 0.0);
        assert!(!horse.is_flying());
        for _ in 0..100 {
            horse.update(0.05, None, false, 0.0);
        }
        assert!(!horse.is_active());
    }

    #[test]
    fn draws_the_horse_the_signs_and_the_text_areas() {
        let plan = Plan::new(120, 28).unwrap();
        let mut horse = Horse::default();
        horse.reset(Some(1));
        let canvas = render(&horse, &plan, &[(70, 4)]);
        let field = plan.field.px(2);
        let around_horse = Area::new(
            (plan.horse_x - plan.size) as usize,
            field.y,
            (plan.size * 2.2) as usize,
            field.h,
        );
        assert!(count(&canvas, around_horse, COAT) > 30, "a white horse");
        assert!(count(&canvas, field, EYE) >= 1);
        // Text areas are plain colours.
        let sign = Area::new(70, plan.sign_row, 4, 1).px(2);
        assert_eq!(count(&canvas, sign, SIGN), 8);
        let rail = plan.rail.px(2);
        assert_eq!(count(&canvas, rail, RAIL), rail.w * 2);
        let bubble = plan.bubble.unwrap().inside().px(2);
        assert_eq!(count(&canvas, bubble, BUBBLE), bubble.w * bubble.h);
        assert_eq!(
            count(&canvas, plan.board_header().px(2), BOARD),
            plan.board_header().w * 2
        );
        assert_eq!(
            count(&canvas, plan.entry(1).px(2), BOARD_LIT),
            plan.entry(1).w * 2
        );
        assert_eq!(count(&canvas, plan.banner.px(2), BANNER), 240);
    }

    #[test]
    fn the_plane_flies_a_loop_in_the_sky_and_lands_on_the_horse() {
        let plan = Plan::new(120, 28).unwrap();
        let home = plan.home();
        let (start, end) = (plan.flight(0.0), plan.flight(1.0));
        assert!((start.0 - home.0).abs() < 0.01 && (start.1 - home.1).abs() < 0.01);
        assert!((end.0 - home.0).abs() < 0.01 && (end.1 - home.1).abs() < 0.01);
        let top = plan.flight(0.5);
        assert!(top.1 > (plan.field.y * 2) as f32 && top.1 < home.1 - plan.size * 0.5);

        // Mid-flight there is a plane in the sky and no horse on the track.
        let mut horse = Horse::default();
        horse.reset(Some(1));
        horse.update(0.0, Some(2), true, 0.0);
        while horse.trick.is_some_and(|t| t < 0.5) {
            horse.update(0.05, Some(2), true, 0.0);
        }
        let canvas = render(&horse, &plan, &[]);
        let f = plan.field.px(2);
        let sky = Area::new(0, f.y, plan.width, (f.h as f32 * 0.45) as usize);
        assert!(count(&canvas, sky, COAT) > 10, "the plane");
        let track = Area::new(
            0,
            (plan.ground - plan.size) as usize,
            (plan.horse_x * 1.5) as usize,
            plan.size as usize,
        );
        assert_eq!(count(&canvas, track, COAT), 0, "the horse is away");
    }
}
