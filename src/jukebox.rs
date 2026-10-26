//! The jukebox view's machine, drawn in pixels: an arched wooden cabinet outlined in neon
//! tubes, a window onto the record mechanism, a "Next up" glass, the title-strip board and
//! a speaker grille.
//!
//! Upcoming songs stand in the rack as records, the next one in front. The playing record
//! spins on the turntable and the tonearm moves inwards as the song plays, like a real one.
//! When the song changes, the arm swings away, the record goes back into the rack, the next
//! one tips over onto the turntable and the arm swings back.
//!
//! The layout is planned in terminal cells, so the UI can write text on the plain-coloured
//! parts (the sign, the glass, the plaque and the cards).

use std::f32::consts::{FRAC_PI_2, PI, TAU};
use std::hash::{Hash, Hasher};
use std::path::Path;

use crate::outputs::pixels::Canvas;
use crate::paint::{Area, Pen, Rgb, frame, hsv, lerp, mix, noise, phase, shade};

/// Colours the UI writes text on.
pub const SIGN: Rgb = [0x16, 0x0b, 0x12];
pub const GLASS: Rgb = [0x14, 0x10, 0x1c];
pub const PLATE: Rgb = [0xc6, 0xcc, 0xd4];
pub const BOARD: Rgb = [0x26, 0x19, 0x12];
pub const CARD: Rgb = [0xf1, 0xe6, 0xcb];
pub const CARD_LIT: Rgb = [0xff, 0xd8, 0x74];
/// Text colours on the cards and the plaque.
pub const INK: Rgb = [0x2b, 0x1d, 0x14];
pub const RED: Rgb = [0xb3, 0x26, 0x1c];
pub const BLUE: Rgb = [0x1d, 0x3f, 0x8a];

const WOOD: Rgb = [0x72, 0x37, 0x17];
const CHROME_HI: Rgb = [0xee, 0xf1, 0xf5];
const CHROME: Rgb = [0xa8, 0xb0, 0xba];
const CHROME_LO: Rgb = [0x55, 0x5c, 0x66];
/// The chrome frames' top, left and bottom-right edges.
const CHROME_FRAME: [Rgb; 3] = [CHROME, CHROME_HI, CHROME_LO];
const VINYL: Rgb = [0x14, 0x14, 0x17];
const GROOVE: Rgb = [0x1d, 0x1d, 0x22];

/// Colours of the neon rainbow.
const NEON_STEPS: usize = 48;

/// Seconds a record change takes.
pub const CHANGE_SECS: f32 = 2.6;
/// 45 rpm, in turns per second.
const SPEED: f32 = 0.75;

// The steps of a record change, as shares of `CHANGE_SECS`.
const ARM_OUT: (f32, f32) = (0.0, 0.14);
const RETURN: (f32, f32) = (0.12, 0.42);
const DROP: (f32, f32) = (0.40, 0.74);
const SLIDE: (f32, f32) = (0.56, 0.80);
const ARM_IN: (f32, f32) = (0.82, 1.0);

/// Identifies a song's record.
pub fn path_key(path: &Path) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    h.finish()
}

/// A record: which song it is, and the colour of its label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Disc {
    pub key: u64,
    pub label: Rgb,
}

impl Disc {
    pub fn new(key: u64) -> Disc {
        let hue = noise(key);
        let sat = 0.55 + 0.35 * noise(key ^ 0x5bd1);
        Disc {
            key,
            label: hsv(hue, sat, 0.92),
        }
    }

    /// The record of the song in `path`: every song keeps its label colour.
    pub fn for_path(path: &Path) -> Disc {
        Disc::new(path_key(path))
    }
}

/// Where the parts of the machine go, in terminal cells from the view's top left.
///
/// The window, the glass and the board are framed in chrome: their first and last rows
/// hold the frame, so text goes on the rows in between (see `Area::inside`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// Pixel rows per terminal row: 2 with half blocks, 1 without.
    pub rows_per_cell: usize,
    pub cabinet: Area,
    /// Rows of the arched top.
    pub crown: usize,
    /// The lit sign in the arch: one row of text.
    pub sign: Option<Area>,
    /// The window onto the mechanism.
    pub window: Area,
    /// The "Next up" glass beside the window, when there is room for it.
    pub panel: Option<Area>,
    /// The "Now playing" plaque: one row of text.
    pub plaque: Area,
    /// The title-strip board: a header row, then rows of cards.
    pub board: Area,
    /// Cards side by side, their width, and how many rows of them fit.
    pub columns: usize,
    pub card_width: usize,
    pub card_rows: usize,
    pub grille: Option<Area>,
}

/// Cards are two rows of text over a coloured strip.
const CARD_ROWS: usize = 3;
const CARD_GAP: usize = 2;
const CARD_MIN_WIDTH: usize = 34;

impl Plan {
    /// Plans a machine filling `cols` x `rows` cells, or `None` when it would not fit.
    pub fn new(cols: usize, rows: usize, rows_per_cell: usize) -> Option<Plan> {
        if cols < 40 || rows < 12 {
            return None;
        }
        let cab_w = cols.min(140);
        let cabinet = Area::new((cols - cab_w) / 2, 0, cab_w, rows);
        let crown = (rows / 7).clamp(2, 6);
        let grille = match rows {
            30.. => 2,
            22.. => 1,
            _ => 0,
        };
        // Below the crown: the window, the plaque, the board and the grille. The board
        // gets about half, in whole rows of cards; it has a frame and a header besides.
        let rest = rows - crown - grille - 1;
        let mut card_rows = ((rest as f32 * 0.55 - 3.0) / 3.0).round().clamp(1.0, 8.0) as usize;
        while card_rows > 1 && rest < 3 + CARD_ROWS * card_rows + 5 {
            card_rows -= 1;
        }
        let board_h = (3 + CARD_ROWS * card_rows).min(rest - 4);
        let window_h = rest - board_h;
        let margin = if cab_w >= 70 { 5 } else { 4 };
        let (x, w) = (cabinet.x + margin, cab_w - 2 * margin);
        let (window, panel) = if w >= 76 {
            let panel_w = (w * 2 / 5).clamp(30, 52);
            let mech_w = w - panel_w - 2;
            (
                Area::new(x, crown, mech_w, window_h),
                Some(Area::new(x + mech_w + 2, crown, panel_w, window_h)),
            )
        } else {
            (Area::new(x, crown, w, window_h), None)
        };
        let sign = (crown >= 3).then(|| {
            let sw = w.min(34);
            Area::new(cabinet.x + (cab_w - sw) / 2, crown - 1, sw, 1)
        });
        let board = Area::new(x, crown + window_h + 1, w, board_h);
        let inner = w - 4;
        let columns = (inner / CARD_MIN_WIDTH).clamp(1, 4);
        let card_width = (inner - CARD_GAP * (columns - 1)) / columns;
        // On a short board the last row of cards goes without its strip.
        let room = board_h - 3;
        let card_rows = (room / CARD_ROWS + usize::from(room % CARD_ROWS == 2)).max(1);
        Some(Plan {
            rows_per_cell,
            cabinet,
            crown,
            sign,
            window,
            panel,
            plaque: Area::new(x, crown + window_h, w, 1),
            board,
            columns,
            card_width,
            card_rows,
            grille: (grille > 0).then(|| Area::new(x, rows - grille, w, grille)),
        })
    }

    /// The board's header row.
    pub fn board_header(&self) -> Area {
        let inside = self.board.inside();
        Area::new(inside.x + 1, inside.y, inside.w.saturating_sub(2), 1)
    }

    /// How many cards are on the board.
    pub fn cards(&self) -> usize {
        self.columns * self.card_rows
    }

    /// The two text rows of card `i` (left to right, then down).
    pub fn card(&self, i: usize) -> Area {
        let (row, col) = (i / self.columns, i % self.columns);
        Area::new(
            self.board.x + 2 + col * (self.card_width + CARD_GAP),
            self.board.y + 2 + row * CARD_ROWS,
            self.card_width,
            2,
        )
    }
}

/// What a frame shows besides the machine itself.
#[derive(Debug, Clone, Copy)]
pub struct Scene<'a> {
    /// The upcoming records, the next one first.
    pub rack: &'a [Disc],
    /// How far the song has played (0–1), when known: where the tonearm is.
    pub progress: Option<f32>,
    /// The wall behind the machine.
    pub wall: Rgb,
    /// Cards with a song on them (the rest are blank), and the selected one.
    pub filled: usize,
    pub selected: Option<usize>,
    /// The card of the next song, which is lit.
    pub next: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
struct Change {
    from: Option<Disc>,
    to: Option<Disc>,
    t: f32,
    end: f32,
}

/// The moving parts: the turntable, the record change and the lights.
#[derive(Debug, Default)]
pub struct Jukebox {
    /// Seconds since the scene started (lights and bubbles).
    clock: f32,
    /// Turntable angle in turns, and its speed in turns per second.
    angle: f32,
    speed: f32,
    /// Smoothed bass loudness (0–1): the neon pulses with it.
    glow: f32,
    platter: Option<Disc>,
    change: Option<Change>,
}

impl Jukebox {
    /// Puts `platter` on the turntable at once, with no record change.
    pub fn reset(&mut self, platter: Option<Disc>) {
        self.platter = platter;
        self.change = None;
    }

    /// Advances by `dt` seconds. A record other than the one on the turntable starts a
    /// record change; `None` takes the record off. `bass` (0–1) makes the neon pulse.
    pub fn update(&mut self, dt: f32, platter: Option<Disc>, playing: bool, bass: f32) {
        let dt = dt.clamp(0.0, 0.25);
        self.clock = (self.clock + dt) % 3600.0;
        if platter != self.platter {
            let from = self.platter;
            self.change = Some(Change {
                from,
                to: platter,
                // Nothing to take off: start with the arm already away.
                t: if from.is_some() { 0.0 } else { RETURN.1 - 0.04 },
                // Nothing to put on: done once the record is back in the rack.
                end: if platter.is_some() { 1.0 } else { RETURN.1 },
            });
            self.platter = platter;
        }
        if let Some(change) = self.change.as_mut() {
            change.t += dt / CHANGE_SECS;
            if change.t >= change.end {
                self.change = None;
            }
        }
        let on_record = self.arm() >= 1.0;
        let target = if playing && on_record { SPEED } else { 0.0 };
        self.speed += (target - self.speed) * (1.0 - (-dt * 3.0).exp());
        if target == 0.0 && self.speed < 0.01 {
            self.speed = 0.0;
        }
        self.angle = (self.angle + self.speed * dt).fract();
        self.glow += (bass.clamp(0.0, 1.0) - self.glow) * (1.0 - (-dt * 12.0).exp());
    }

    pub fn is_changing(&self) -> bool {
        self.change.is_some()
    }

    /// Whether the picture moves on its own: a record change or a spinning record.
    pub fn is_active(&self) -> bool {
        self.change.is_some() || self.speed > 0.0
    }

    /// The record on the turntable (or arriving there).
    pub fn platter(&self) -> Option<Disc> {
        self.platter
    }

    /// Where the tonearm is: 0 parked, 1 on the record.
    fn arm(&self) -> f32 {
        match self.change {
            Some(c) if c.t < ARM_IN.0 => {
                if c.from.is_some() {
                    1.0 - phase(c.t, ARM_OUT)
                } else {
                    0.0
                }
            }
            Some(c) => phase(c.t, ARM_IN),
            None => f32::from(u8::from(self.platter.is_some())),
        }
    }

    /// The neon colours around the outline (see `neon`). The lights change in steps:
    /// colours that drift a little every frame would make the terminal redraw them all.
    fn neon_colors(&self) -> [Rgb; NEON_STEPS] {
        let pulse = 0.72 + 0.28 * self.pulse();
        std::array::from_fn(|i| hsv(i as f32 / NEON_STEPS as f32, 0.82, pulse))
    }

    /// The colour from `neon_colors` at `along` (0–1 around the arch, beyond it down the
    /// sides). The rainbow moves round a step at a time.
    fn neon(colors: &[Rgb; NEON_STEPS], shift: f32, along: f32) -> Rgb {
        let hue = (along * 0.85 - shift).rem_euclid(1.0);
        colors[((hue * NEON_STEPS as f32) as usize).min(NEON_STEPS - 1)]
    }

    fn neon_shift(&self) -> f32 {
        (self.clock * 0.08 * NEON_STEPS as f32).floor() / NEON_STEPS as f32
    }

    /// The bass loudness in a few steps.
    fn pulse(&self) -> f32 {
        (self.glow * 5.0).round() / 5.0
    }

    /// Draws the machine into `canvas`, which has one pixel per column and
    /// `plan.rows_per_cell` per row.
    pub fn draw(&self, canvas: &mut Canvas, plan: &Plan, scene: &Scene<'_>) {
        let ppc = plan.rows_per_cell.max(1);
        self.draw_cabinet(canvas, plan, scene.wall);
        if let Some(sign) = plan.sign {
            let px = sign.px(ppc);
            let mut pen = Pen::new(canvas);
            pen.fill(px, SIGN);
            // Thin chrome rims above and below the sign.
            if ppc > 1 {
                pen.fill(Area::new(px.x, px.y.saturating_sub(1), px.w, 1), CHROME);
                pen.fill(Area::new(px.x, px.y + px.h, px.w, 1), CHROME_LO);
            }
        }
        let inside = frame(canvas, plan.window, ppc, CHROME_FRAME);
        self.draw_mechanism(canvas, inside, ppc, scene);
        if let Some(panel) = plan.panel {
            let inside = frame(canvas, panel, ppc, CHROME_FRAME);
            Pen::new(canvas).fill(inside, GLASS);
        }
        Pen::new(canvas).fill(plan.plaque.px(ppc), PLATE);
        let inside = frame(canvas, plan.board, ppc, CHROME_FRAME);
        self.draw_board(canvas, plan, inside, scene);
        if let Some(grille) = plan.grille {
            self.draw_grille(canvas, grille.px(ppc));
        }
    }

    fn draw_cabinet(&self, canvas: &mut Canvas, plan: &Plan, wall: Rgb) {
        let ppc = plan.rows_per_cell.max(1);
        let cab = plan.cabinet.px(ppc);
        let hw = cab.w as f32 / 2.0;
        let arch = (plan.crown * ppc).max(1) as f32;
        let cx = cab.x as f32 + hw;
        let base = cab.y as f32 + arch;
        let window_top = (plan.window.y * ppc) as f32;
        let (left, right) = (cab.x as f32, (cab.x + cab.w) as f32);
        let height = canvas.height.max(1) as f32;
        let pulse = 0.6 + 0.4 * self.pulse();
        let (neon, shift) = (self.neon_colors(), self.neon_shift());
        for y in 0..canvas.height {
            for x in 0..canvas.width {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let u = (px - cx) / hw;
                // Distance in pixels inside the outline (negative outside), and how far
                // around the outline this is (0–1 over the arch, beyond it down the sides).
                let (d, along) = if py < base {
                    let v = (py - base) / arch;
                    let rho = (u * u + v * v).sqrt().max(1e-3);
                    let grad = ((u / hw).powi(2) + (v / arch).powi(2)).sqrt() / rho;
                    ((1.0 - rho) / grad.max(1e-4), (u + 1.0) / 2.0)
                } else {
                    let down = (py - base) / height * 0.6;
                    let (l, r) = (px - left, right - px);
                    if l < r { (l, -down) } else { (r, 1.0 + down) }
                };
                let color = if d < -12.0 {
                    wall
                } else if d < 0.0 {
                    // The wall, lit by the neon.
                    let halo = 0.5 * pulse * (d / 3.0).exp();
                    mix(wall, Self::neon(&neon, shift, along), halo)
                } else if d < 1.0 {
                    CHROME_LO
                } else if d < 2.0 {
                    mix(CHROME, CHROME_HI, 1.0 - u.abs())
                } else if d < 4.0 {
                    let mut c = Self::neon(&neon, shift, along);
                    // A bright core, and bubbles rising in the side tubes.
                    if (2.7..3.3).contains(&d) {
                        c = mix(c, [255; 3], 0.3);
                    }
                    let side = if u < 0.0 { 0.0 } else { 3.1 };
                    if py >= base && (py * 0.45 + self.clock * 4.0 + side).rem_euclid(5.0) < 0.7 {
                        c = mix(c, [255; 3], 0.6);
                    }
                    c
                } else if d < 5.0 {
                    CHROME
                } else if py < window_top {
                    // The crown: an art-deco sunburst rising from the window.
                    let a = (py - window_top).atan2(px - cx);
                    let ray = ((a + PI) / (PI / 18.0)) as i32 % 2 == 0;
                    let tone = if ray {
                        [0xa3, 0x68, 0x1c]
                    } else {
                        [0x7c, 0x47, 0x14]
                    };
                    shade(tone, 0.8 + 0.2 * (1.0 - u.abs()))
                } else {
                    // Wood with a vertical grain, rounder in the middle.
                    let grain = 0.9 + 0.1 * noise(x as u64 / 2);
                    shade(WOOD, (0.72 + 0.28 * (u * FRAC_PI_2).cos()) * grain)
                };
                canvas.set(x, y, color);
            }
        }
    }

    fn draw_mechanism(&self, canvas: &mut Canvas, inside: Area, ppc: usize, scene: &Scene<'_>) {
        let mut pen = Pen::new(canvas);
        pen.clip = inside;
        // Pixels are square with half blocks; without them, twice as tall as wide.
        let sq = ppc as f32 / 2.0;
        let (x0, y0) = (inside.x as f32, inside.y as f32);
        let (w, h) = (inside.w as f32, inside.h as f32);
        let units = h / sq;
        // A warm-lit cabinet interior.
        for y in 0..inside.h {
            let k = y as f32 / h.max(1.0);
            let row = mix([0x1d, 0x10, 0x12], [0x33, 0x1f, 0x14], k);
            pen.fill(Area::new(inside.x, inside.y + y, inside.w, 1), row);
        }

        // The turntable, in front on the right. Seen from higher up when there is room.
        let radius = (w * 0.21).min(units * 1.3).max(2.0);
        let (pcx, pcy) = (x0 + w * 0.70, y0 + units * 0.60 * sq);
        let tilt = (units * 0.28 / radius).clamp(0.3, 0.55) * sq;
        pen.glow(
            pcx,
            pcy,
            radius * 1.8,
            radius * 1.2 * sq,
            [0xff, 0xb0, 0x50],
            0.18,
        );
        // The rack, standing behind it on the left: records seen nearly edge on.
        let rack_r = (units * 0.34).max(2.0);
        let floor = y0 + units * 0.84 * sq;
        let slot_y = floor - rack_r * sq;
        let thin = 0.42;
        let spacing = rack_r * thin * 1.6 + 1.0;
        let rack_right = pcx - radius * 1.15 - 2.0;
        let slot_x = |k: f32| rack_right - rack_r * thin - k * spacing;
        let slots = (((rack_right - x0) / spacing) as usize).clamp(2, 12);

        let change = self.change;
        let t = change.map_or(1.0, |c| c.t);
        let arriving = change.and_then(|c| c.to);
        let drop = arriving.map_or(1.0, |_| phase(t, DROP));
        // The new song's record leaves the front slot; the others move up behind it.
        let shift = if arriving.is_some() {
            1.0 - phase(t, SLIDE)
        } else {
            0.0
        };
        let upright = |k: f32| (slot_x(k), slot_y, rack_r * thin, rack_r * sq);
        for i in (0..scene.rack.len().min(slots + 1)).rev() {
            let (cx, cy, rx, ry) = upright(i as f32 + shift);
            record(&mut pen, cx, cy, rx, ry, scene.rack[i].label, 0.0, 1.0);
        }
        // The rail holding them.
        pen.fill_f(x0, floor - sq, rack_right - x0 + 1.0, sq.max(1.0), CHROME);
        pen.fill_f(x0, floor, rack_right - x0 + 1.0, sq.max(1.0), CHROME_LO);

        // Platter: its side, the metal top, the rubber mat.
        let (prx, pry) = (radius * 1.04, radius * tilt * 1.04);
        let side = (1.2 * sq).max(1.0);
        pen.ellipse(pcx, pcy + side, prx, pry, [0x36, 0x39, 0x40]);
        pen.ellipse_with(pcx, pcy, prx, pry, |r, _| {
            Some((
                if r > 0.93 {
                    [0x8a, 0x90, 0x9a]
                } else {
                    [0x22, 0x23, 0x27]
                },
                1.0,
            ))
        });
        let on_platter = match change {
            Some(c) if c.t < RETURN.0 => c.from,
            Some(c) if drop >= 1.0 => c.to,
            Some(_) => None,
            None => self.platter,
        };
        match on_platter {
            Some(disc) => record(
                &mut pen,
                pcx,
                pcy,
                radius,
                radius * tilt,
                disc.label,
                self.angle,
                1.0,
            ),
            None => pen.ellipse(pcx, pcy, radius * 0.12, radius * tilt * 0.12, CHROME_HI),
        }

        // The tonearm, from its pivot behind the platter on the right.
        let pivot = (pcx + radius * 1.12, pcy - radius * tilt * 0.9);
        let k = self.arm();
        let groove = 0.9 - 0.45 * scene.progress.unwrap_or(0.0).clamp(0.0, 1.0);
        let play = (
            pcx + groove * radius * 0.35f32.cos(),
            pcy + groove * radius * tilt * 0.35f32.sin(),
        );
        let park = (pcx + radius * 1.06, pcy + radius * tilt * 0.9);
        let lift = if k > 0.0 && k < 1.0 { sq.max(1.0) } else { 0.0 };
        let tip = (lerp(park.0, play.0, k), lerp(park.1, play.1, k) - lift);
        pen.line(pivot.0, pivot.1 + sq, tip.0, tip.1 + sq, CHROME_LO);
        pen.line(pivot.0, pivot.1, tip.0, tip.1, CHROME_HI);
        pen.fill_f(
            tip.0 - 1.0,
            tip.1 - sq * 0.5,
            2.0,
            sq.max(1.0),
            [0xf6, 0xf6, 0xf6],
        );
        let back = {
            let (dx, dy) = (pivot.0 - tip.0, pivot.1 - tip.1);
            let len = (dx * dx + dy * dy).sqrt().max(1.0);
            (pivot.0 + dx / len * 2.5, pivot.1 + dy / len * 2.5)
        };
        pen.ellipse(back.0, back.1, 1.6, 1.6 * sq, CHROME_LO);
        pen.ellipse_with(pivot.0, pivot.1, 2.0, 2.0 * sq, |r, _| {
            Some((if r < 0.5 { CHROME_HI } else { CHROME }, 1.0))
        });

        // Records on the move: the old one back into the rack, the new one from the front
        // of the rack onto the platter.
        if let Some(c) = change {
            if let Some(disc) = c.from {
                let k = phase(c.t, RETURN);
                if k > 0.0 && k < 1.0 {
                    let (bx, by, brx, bry) = upright(slots as f32);
                    let arc = (k * PI).sin() * rack_r * sq * 1.2;
                    let fade = 1.0 - phase(k, (0.7, 1.0));
                    record(
                        &mut pen,
                        lerp(pcx, bx, k),
                        lerp(pcy, by, k) - arc,
                        lerp(radius, brx, k),
                        lerp(radius * tilt, bry, k),
                        disc.label,
                        self.angle,
                        fade,
                    );
                }
            }
            if let Some(disc) = c.to
                && drop <= 0.0
            {
                let (cx, cy, rx, ry) = upright(0.0);
                record(&mut pen, cx, cy, rx, ry, disc.label, 0.0, 1.0);
            }
            if let Some(disc) = c.to
                && drop > 0.0
                && drop < 1.0
            {
                let (ux, uy, urx, ury) = upright(0.0);
                let arc = (drop * PI).sin() * rack_r * sq;
                record(
                    &mut pen,
                    lerp(ux, pcx, drop),
                    lerp(uy, pcy, drop) - arc,
                    lerp(urx, radius, drop),
                    lerp(ury, radius * tilt, drop),
                    disc.label,
                    0.0,
                    1.0,
                );
            }
        }
        // Light falling through the glass.
        for i in 0..inside.w {
            let x = inside.x + i;
            let y = inside.y + (i * 3 / 5) % inside.h.max(1);
            pen.blend(x as f32, y as f32, [255; 3], 0.04);
        }
    }

    fn draw_board(&self, canvas: &mut Canvas, plan: &Plan, inside: Area, scene: &Scene<'_>) {
        let ppc = plan.rows_per_cell.max(1);
        let mut pen = Pen::new(canvas);
        pen.fill(inside, BOARD);
        let neon = self.neon_colors();
        // The next song's strip blinks.
        let blink_on = (self.clock * 1.5).fract() < 0.6;
        for i in 0..plan.cards().min(scene.filled) {
            let card = plan.card(i).px(ppc);
            let lit = scene.selected == Some(i);
            pen.fill(card, if lit { CARD_LIT } else { CARD });
            // The coloured strip under the card (only half blocks can draw it thin).
            if ppc > 1 && card.y + card.h < inside.y + inside.h {
                let strip = if scene.next == Some(i) {
                    let c = Self::neon(&neon, self.neon_shift(), 0.1);
                    if blink_on { c } else { shade(c, 0.45) }
                } else {
                    RED
                };
                pen.fill(Area::new(card.x, card.y + card.h, card.w, 1), strip);
            }
        }
    }

    fn draw_grille(&self, canvas: &mut Canvas, area: Area) {
        let mut pen = Pen::new(canvas);
        let (neon, shift) = (self.neon_colors(), self.neon_shift());
        for i in 0..area.w {
            let x = area.x + i;
            let along = i as f32 / area.w.max(1) as f32;
            let light = Self::neon(&neon, shift, along);
            let fabric = mix([0x1c, 0x12, 0x0f], light, 0.15 + 0.35 * self.pulse());
            let color = if i % 4 == 0 { CHROME } else { fabric };
            pen.fill(Area::new(x, area.y, 1, area.h), color);
        }
        pen.fill(Area::new(area.x, area.y, area.w, 1), CHROME_LO);
    }
}

/// A record seen at an angle: grooves, a sheen, and a label turned by `turn` (turns).
/// `alpha` below 1 fades it into what is behind.
#[allow(clippy::too_many_arguments)]
fn record(
    pen: &mut Pen<'_>,
    cx: f32,
    cy: f32,
    rx: f32,
    ry: f32,
    label: Rgb,
    turn: f32,
    alpha: f32,
) {
    if alpha <= 0.0 {
        return;
    }
    let spin = turn * TAU;
    let dark = shade(label, 0.72);
    pen.ellipse_with(cx, cy, rx, ry, |r, a| {
        let color = if r > 0.95 {
            [0x2c, 0x2c, 0x33]
        } else if r > 0.36 {
            let groove = if (r * 11.0).fract() < 0.5 {
                VINYL
            } else {
                GROOVE
            };
            // Light from the top left catches the grooves.
            let sheen = (2.0 * (a + 2.3)).cos().max(0.0).powi(6) * 0.22 * r;
            mix(groove, [0xd8, 0xdc, 0xe8], sheen)
        } else if r > 0.33 {
            dark
        } else if r > 0.09 {
            // Half the label is lighter, so the turning shows; plus a white mark.
            let rel = (a - spin).rem_euclid(TAU);
            if (0.2..0.3).contains(&r) && rel < 0.6 {
                [0xfa, 0xf4, 0xe4]
            } else if rel < PI {
                label
            } else {
                dark
            }
        } else {
            CHROME_HI
        };
        Some((color, alpha))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disc(key: u64) -> Disc {
        Disc::new(key)
    }

    fn scene(rack: &[Disc]) -> Scene<'_> {
        Scene {
            rack,
            progress: Some(0.3),
            wall: [0x20, 0x20, 0x28],
            filled: rack.len(),
            selected: Some(0),
            next: Some(0),
        }
    }

    fn render(jb: &Jukebox, plan: &Plan, rack: &[Disc]) -> Canvas {
        let mut canvas = Canvas::new(120, 28 * plan.rows_per_cell, [0; 3]);
        jb.draw(&mut canvas, plan, &scene(rack));
        canvas
    }

    fn has(canvas: &Canvas, area: Area, color: Rgb) -> bool {
        (area.y..area.y + area.h)
            .any(|y| (area.x..area.x + area.w).any(|x| canvas.get(x, y) == color))
    }

    /// Either half of the record's label is in `area`.
    fn has_label(canvas: &Canvas, area: Area, d: Disc) -> bool {
        has(canvas, area, d.label) || has(canvas, area, shade(d.label, 0.72))
    }

    fn window_px(plan: &Plan) -> Area {
        plan.window.px(plan.rows_per_cell)
    }

    #[test]
    fn plan_fits_the_parts_in_order() {
        let plan = Plan::new(120, 28, 2).unwrap();
        assert!(plan.sign.is_some());
        let panel = plan.panel.expect("room for the Next up glass");
        assert_eq!(panel.y, plan.window.y);
        assert!(plan.window.x + plan.window.w < panel.x);
        assert_eq!(plan.plaque.y, plan.window.y + plan.window.h);
        assert_eq!(plan.board.y, plan.plaque.y + 1);
        let grille = plan.grille.unwrap();
        assert_eq!(plan.board.y + plan.board.h, grille.y);
        assert_eq!(grille.y + grille.h, 28);
        assert_eq!(plan.columns, 3);
        // Cards stay inside the board and do not overlap.
        let last = plan.card(plan.cards() - 1);
        assert!(last.y + last.h < plan.board.y + plan.board.h);
        assert!(plan.card(0).x + plan.card(0).w < plan.card(1).x);
        assert_eq!(plan.board_header().y, plan.board.y + 1);

        // Narrow: no glass; tiny: no machine at all.
        let narrow = Plan::new(70, 20, 2).unwrap();
        assert!(narrow.panel.is_none());
        assert_eq!(narrow.columns, 1);
        assert!(Plan::new(39, 30, 2).is_none());
        assert!(Plan::new(100, 11, 2).is_none());
        // Very wide terminals get a machine of sensible width in the middle.
        let wide = Plan::new(240, 50, 2).unwrap();
        assert_eq!(wide.cabinet.w, 140);
        assert_eq!(wide.cabinet.x, 50);
    }

    #[test]
    fn a_new_song_starts_a_record_change_that_finishes() {
        let mut jb = Jukebox::default();
        jb.reset(Some(disc(1)));
        assert!(!jb.is_changing());
        jb.update(0.1, Some(disc(1)), true, 0.0);
        assert!(!jb.is_changing(), "the same record keeps playing");

        jb.update(0.1, Some(disc(2)), true, 0.0);
        assert!(jb.is_changing());
        assert_eq!(jb.platter(), Some(disc(2)));
        assert!(jb.arm() < 1.0, "the arm swings away");
        let mut elapsed = 0.1;
        while jb.is_changing() {
            jb.update(0.05, Some(disc(2)), true, 0.0);
            elapsed += 0.05;
            assert!(elapsed < CHANGE_SECS + 0.2, "never finished");
        }
        assert!(elapsed > CHANGE_SECS - 0.2);
        assert_eq!(jb.arm(), 1.0);

        // Stopping takes the record back, which is quicker.
        jb.update(0.05, None, false, 0.0);
        let mut elapsed = 0.0;
        while jb.is_changing() {
            jb.update(0.05, None, false, 0.0);
            elapsed += 0.05;
        }
        assert!(elapsed < CHANGE_SECS * 0.5, "{elapsed}");
        assert_eq!(jb.arm(), 0.0);
    }

    #[test]
    fn the_record_spins_only_while_playing_and_on_the_arm() {
        let mut jb = Jukebox::default();
        jb.reset(Some(disc(1)));
        for _ in 0..40 {
            jb.update(0.05, Some(disc(1)), true, 0.0);
        }
        assert!(jb.is_active());
        assert!((jb.speed - SPEED).abs() < 0.05, "spun up to 45 rpm");
        // Paused: it slows down and stops.
        for _ in 0..80 {
            jb.update(0.05, Some(disc(1)), false, 0.0);
        }
        assert_eq!(jb.speed, 0.0);
        assert!(!jb.is_active());
        // During a change it does not spin until the arm is back.
        jb.update(0.05, Some(disc(2)), true, 0.0);
        for _ in 0..10 {
            jb.update(0.05, Some(disc(2)), true, 0.0);
        }
        assert_eq!(jb.speed, 0.0);
    }

    #[test]
    fn records_show_their_labels_on_the_turntable_and_in_the_rack() {
        let plan = Plan::new(120, 28, 2).unwrap();
        let rack = [disc(10), disc(11), disc(12)];
        let mut jb = Jukebox::default();
        jb.reset(Some(disc(9)));
        let canvas = render(&jb, &plan, &rack);
        let window = window_px(&plan);
        for &d in rack.iter().chain([disc(9)].iter()) {
            assert!(has_label(&canvas, window, d), "label {:?} missing", d.label);
        }
        // Text areas are plain colours.
        let plaque = plan.plaque.px(2);
        assert!((plaque.x..plaque.x + plaque.w).all(|x| canvas.get(x, plaque.y) == PLATE));
        let card = plan.card(1).px(2);
        assert_eq!(canvas.get(card.x + 3, card.y + 3), CARD);
        assert_eq!(canvas.get(plan.card(0).x + 3, plan.card(0).y * 2), CARD_LIT);
        let panel = plan.panel.unwrap().inside().px(2);
        assert_eq!(canvas.get(panel.x, panel.y), GLASS);

        // Once stopped and the change is over, the turntable is empty.
        jb.update(0.05, None, false, 0.0);
        while jb.is_changing() {
            jb.update(0.05, None, false, 0.0);
        }
        let canvas = render(&jb, &plan, &rack);
        assert!(!has_label(&canvas, window, disc(9)));
    }

    #[test]
    fn a_record_in_flight_leaves_the_rack_and_lands_on_the_turntable() {
        let plan = Plan::new(120, 28, 2).unwrap();
        let mut jb = Jukebox::default();
        jb.reset(Some(disc(1)));
        let next = disc(2);
        jb.update(0.0, Some(next), true, 0.0);
        let window = window_px(&plan);
        // Through the whole change the new record is somewhere in the window.
        let mut seen = 0;
        while jb.is_changing() {
            let canvas = render(&jb, &plan, &[disc(3)]);
            assert!(
                has_label(&canvas, window, next),
                "lost the record at {:?}",
                jb.change
            );
            seen += 1;
            jb.update(0.1, Some(next), true, 0.0);
        }
        assert!(seen > 20);
    }

    #[test]
    fn ascii_pixels_are_whole_rows() {
        let plan = Plan::new(100, 24, 1).unwrap();
        let rack = [disc(5)];
        let mut jb = Jukebox::default();
        jb.reset(Some(disc(4)));
        let mut canvas = Canvas::new(100, 24, [0; 3]);
        jb.draw(&mut canvas, &plan, &scene(&rack));
        assert!(has_label(&canvas, plan.window, disc(4)));
        assert_eq!(canvas.get(plan.plaque.x, plan.plaque.y), PLATE);
    }
}
