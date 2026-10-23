//! HYPERSPACE: an audio-reactive demoscene visualizer rendered into an RGB framebuffer.
//!
//! The scene, from back to front:
//! - a warp starfield flying out of a vanishing point; speed follows loudness, every beat
//!   jolts it forward and each new track is a full hyperspace jump;
//! - a pulsing nebula glow behind the centre;
//! - a mirrored spectrum corona (bass at the bottom, treble at the top) that slowly turns;
//! - a circular oscilloscope of the real waveform;
//! - a rotating 3D point-cloud sphere whose surface bulges with the spectrum (bass at the
//!   equator, treble at the poles);
//! - beat shockwave rings and particle bursts;
//! - the track title in glowing pixel letters that glitch on the beat.
//!
//! Light is accumulated additively with persistence (motion trails), blurred into a bloom,
//! tone-mapped over the background and quantised. Beat detection compares the bass energy
//! with its recent average and estimates the tempo from the beat intervals.

use std::collections::VecDeque;
use std::f32::consts::{PI, TAU};

pub type Rgb = [f32; 3];

/// Neon colours the scene is painted with, plus the background.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub bg: Rgb,
    pub colors: [Rgb; 6],
}

impl Default for Palette {
    /// Dracula-style neon.
    fn default() -> Self {
        let c = |hex: u32| {
            [
                ((hex >> 16) & 0xff) as f32 / 255.0,
                ((hex >> 8) & 0xff) as f32 / 255.0,
                (hex & 0xff) as f32 / 255.0,
            ]
        };
        Palette {
            bg: c(0x0b0b14),
            colors: [
                c(0xbd93f9),
                c(0xff79c6),
                c(0x8be9fd),
                c(0x50fa7b),
                c(0xf1fa8c),
                c(0xff5555),
            ],
        }
    }
}

#[derive(Debug, Clone)]
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }
}

/// Finds kicks in the bass energy and estimates the tempo.
#[derive(Debug, Default)]
pub struct BeatDetector {
    history: VecDeque<(f32, f32)>,
    last_beat: f32,
    intervals: VecDeque<f32>,
    bpm: Option<f32>,
    /// 1.0 right after a beat, decaying towards 0.
    pub pulse: f32,
}

impl BeatDetector {
    const WINDOW_SECS: f32 = 1.5;
    const MIN_GAP_SECS: f32 = 0.26;

    /// Feeds the bass energy at time `t`. Returns true on a beat.
    pub fn update(&mut self, t: f32, dt: f32, energy: f32) -> bool {
        while self
            .history
            .front()
            .is_some_and(|&(when, _)| t - when > Self::WINDOW_SECS)
        {
            self.history.pop_front();
        }
        let n = self.history.len() as f32;
        let beat = if n >= 10.0 {
            let mean = self.history.iter().map(|&(_, e)| e).sum::<f32>() / n;
            let var = self
                .history
                .iter()
                .map(|&(_, e)| (e - mean) * (e - mean))
                .sum::<f32>()
                / n;
            energy > 0.25
                && energy - mean > (1.5 * var.sqrt()).max(0.05)
                && t - self.last_beat > Self::MIN_GAP_SECS
        } else {
            false
        };
        self.history.push_back((t, energy));
        self.pulse *= (-dt * 5.0).exp();
        if beat {
            let interval = t - self.last_beat;
            if (Self::MIN_GAP_SECS..2.0).contains(&interval) {
                self.intervals.push_back(interval);
                if self.intervals.len() > 16 {
                    self.intervals.pop_front();
                }
            }
            self.last_beat = t;
            self.pulse = 1.0;
            if self.intervals.len() >= 4 {
                let mut sorted: Vec<f32> = self.intervals.iter().copied().collect();
                sorted.sort_by(f32::total_cmp);
                let mut bpm = 60.0 / sorted[sorted.len() / 2];
                // Fold into the range where most music lives.
                while bpm < 70.0 {
                    bpm *= 2.0;
                }
                while bpm > 180.0 {
                    bpm /= 2.0;
                }
                self.bpm = Some(bpm);
            }
        }
        beat
    }

    pub fn bpm(&self) -> Option<f32> {
        self.bpm
    }

    pub fn reset_tempo(&mut self) {
        self.intervals.clear();
        self.bpm = None;
    }
}

/// A 3x5 pixel font for the track title.
const FONT: &[(char, [&str; 5])] = &[
    ('A', [".#.", "#.#", "###", "#.#", "#.#"]),
    ('B', ["##.", "#.#", "##.", "#.#", "##."]),
    ('C', [".##", "#..", "#..", "#..", ".##"]),
    ('D', ["##.", "#.#", "#.#", "#.#", "##."]),
    ('E', ["###", "#..", "##.", "#..", "###"]),
    ('F', ["###", "#..", "##.", "#..", "#.."]),
    ('G', [".##", "#..", "#.#", "#.#", ".##"]),
    ('H', ["#.#", "#.#", "###", "#.#", "#.#"]),
    ('I', ["###", ".#.", ".#.", ".#.", "###"]),
    ('J', ["..#", "..#", "..#", "#.#", ".#."]),
    ('K', ["#.#", "#.#", "##.", "#.#", "#.#"]),
    ('L', ["#..", "#..", "#..", "#..", "###"]),
    ('M', ["#.#", "###", "###", "#.#", "#.#"]),
    ('N', ["##.", "#.#", "#.#", "#.#", "#.#"]),
    ('O', [".#.", "#.#", "#.#", "#.#", ".#."]),
    ('P', ["##.", "#.#", "##.", "#..", "#.."]),
    ('Q', [".#.", "#.#", "#.#", "##.", ".##"]),
    ('R', ["##.", "#.#", "##.", "#.#", "#.#"]),
    ('S', [".##", "#..", ".#.", "..#", "##."]),
    ('T', ["###", ".#.", ".#.", ".#.", ".#."]),
    ('U', ["#.#", "#.#", "#.#", "#.#", "###"]),
    ('V', ["#.#", "#.#", "#.#", "#.#", ".#."]),
    ('W', ["#.#", "#.#", "###", "###", "#.#"]),
    ('X', ["#.#", "#.#", ".#.", "#.#", "#.#"]),
    ('Y', ["#.#", "#.#", ".#.", ".#.", ".#."]),
    ('Z', ["###", "..#", ".#.", "#..", "###"]),
    ('0', ["###", "#.#", "#.#", "#.#", "###"]),
    ('1', [".#.", "##.", ".#.", ".#.", "###"]),
    ('2', ["##.", "..#", ".#.", "#..", "###"]),
    ('3', ["##.", "..#", ".#.", "..#", "##."]),
    ('4', ["#.#", "#.#", "###", "..#", "..#"]),
    ('5', ["###", "#..", "##.", "..#", "##."]),
    ('6', [".##", "#..", "###", "#.#", "###"]),
    ('7', ["###", "..#", ".#.", ".#.", ".#."]),
    ('8', ["###", "#.#", "###", "#.#", "###"]),
    ('9', ["###", "#.#", "###", "..#", "##."]),
    ('-', ["...", "...", "###", "...", "..."]),
    ('.', ["...", "...", "...", "...", ".#."]),
    (',', ["...", "...", "...", ".#.", "#.."]),
    ('\'', [".#.", ".#.", "...", "...", "..."]),
    ('&', [".#.", "#.#", ".#.", "#.#", ".##"]),
    ('(', ["..#", ".#.", ".#.", ".#.", "..#"]),
    (')', ["#..", ".#.", ".#.", ".#.", "#.."]),
    ('!', [".#.", ".#.", ".#.", "...", ".#."]),
    ('?', ["##.", "..#", ".#.", "...", ".#."]),
    (':', ["...", ".#.", "...", ".#.", "..."]),
    ('/', ["..#", "..#", ".#.", "#..", "#.."]),
    (' ', ["...", "...", "...", "...", "..."]),
];

/// The glyph rows for `c`, folding accents (Ä→A, Ö→O, …) and unknown characters to '?'.
fn glyph(c: char) -> &'static [&'static str; 5] {
    let upper = c.to_uppercase().next().unwrap_or(c);
    let folded = match upper {
        'Ä' | 'Å' | 'Á' | 'À' | 'Â' | 'Ã' => 'A',
        'Ö' | 'Ó' | 'Ò' | 'Ô' | 'Õ' | 'Ø' => 'O',
        'Ü' | 'Ú' | 'Ù' | 'Û' => 'U',
        'É' | 'È' | 'Ê' | 'Ë' => 'E',
        'Í' | 'Ì' | 'Î' | 'Ï' => 'I',
        'Ñ' => 'N',
        'Ç' => 'C',
        'ß' => 'S',
        '_' => ' ',
        other => other,
    };
    FONT.iter()
        .find(|(ch, _)| *ch == folded)
        .or_else(|| FONT.iter().find(|(ch, _)| *ch == '?'))
        .map(|(_, rows)| rows)
        .expect("font has '?'")
}

#[derive(Debug, Clone, Copy)]
struct Star {
    x: f32,
    y: f32,
    z: f32,
    prev: Option<(f32, f32)>,
    tint: usize,
}

#[derive(Debug, Clone, Copy)]
struct Ring {
    radius: f32,
    speed: f32,
    life: f32,
    color: Rgb,
}

#[derive(Debug, Clone, Copy)]
struct Particle {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    life: f32,
    max_life: f32,
    color: Rgb,
}

pub struct Hyperspace {
    w: usize,
    h: usize,
    light: Vec<Rgb>,
    glow: Vec<Rgb>,
    scratch: Vec<Rgb>,
    pixels: Vec<[u8; 3]>,
    stars: Vec<Star>,
    sphere: Vec<[f32; 3]>,
    rings: Vec<Ring>,
    particles: Vec<Particle>,
    rng: Rng,
    t: f32,
    dt: f32,
    yaw: f32,
    pitch: f32,
    energy: f32,
    bass: f32,
    speed: f32,
    warp: f32,
    flash: f32,
    shake: (f32, f32),
    glitch: f32,
    hue: usize,
    bands: Vec<f32>,
    wave: Vec<f32>,
    title: String,
    title_age: f32,
    track: Option<u64>,
    pub beat: BeatDetector,
    pub palette: Palette,
    pub tuning: Tuning,
    /// Plot whole pixels this frame (see `Lod::crisp`).
    crisp: bool,
    /// Light that lasts only one frame (static elements when they do not persist).
    frame: Vec<Rgb>,
    /// `light + frame`, the input of the bloom.
    sum: Vec<Rgb>,
    /// Where `add` currently draws: the per-frame buffer instead of the persistent one.
    to_frame: bool,
    /// Title pixels of this frame (x, y, colour, alpha), stamped over the final image.
    title_px: Vec<(usize, usize, Rgb, f32)>,
}

/// Below this size (the smaller side, in pixels) the scene is drawn in its crisp form.
const CRISP_BELOW: f32 = 56.0;

/// Level of detail for the current size: small screens get fewer, crisper elements instead
/// of a glowing blur, and wide strips spread the scene sideways.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lod {
    /// Plot whole pixels instead of soft sub-pixel splats.
    pub crisp: bool,
    /// Bloom blur radius in pixels (0 = no glow).
    pub bloom_radius: usize,
    pub nebula: bool,
    /// The circular oscilloscope (too small to read on tiny screens).
    pub scope: bool,
    /// Draw every n-th point of the sphere, so points never pile up into a blob.
    pub sphere_step: usize,
    /// Pixel title scale (0 = not shown).
    pub title_scale: usize,
    /// Horizontal stretch of the corona, rings and sparks in wide strips (1.0 = round).
    pub stretch: f32,
    /// Trail fade speed multiplier (shorter trails when crisp).
    pub fade: f32,
    /// Whether the corona, oscilloscope and sphere leave trails too. On small and medium
    /// screens they are drawn fresh each frame, so they stay sharp instead of piling up.
    pub persist_static: bool,
}

/// Knobs that trade looks for terminal output.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tuning {
    pub nebula: bool,
    pub bloom: bool,
    /// Colour channels are rounded down to multiples of `1 << quant_shift`.
    pub quant_shift: u8,
    /// How fast trails fade (per second).
    pub fade_rate: f32,
    /// Light below this is drawn as plain background.
    pub floor: f32,
    /// A shown pixel is only updated when a colour channel moves by more than this (or the
    /// pixel returns to the background). Suppresses tiny changes the terminal would
    /// otherwise have to redraw every frame.
    pub hysteresis: u8,
    /// Star count relative to the screen area (1.0 = one star per 45 pixels).
    pub stars: f32,
    /// Scales how many particles each beat throws out.
    pub particles: f32,
}

impl Tuning {
    /// Quality presets. Measured at 200x55 cells with busy music, terminal output per frame:
    /// `high` ~150 KB, `balanced` ~80 KB, `fast` ~38 KB (untuned, with ratatui's own backend:
    /// ~285 KB). See `hyperspace_terminal_load` in `ui/mod.rs`.
    pub const PRESETS: &[&str] = &["high", "balanced", "fast"];

    pub fn preset(name: &str) -> Option<Tuning> {
        let base = Tuning {
            nebula: true,
            bloom: true,
            quant_shift: 3,
            fade_rate: 9.0,
            floor: 0.03,
            hysteresis: 32,
            stars: 1.0,
            particles: 1.0,
        };
        match name {
            "high" => Some(Tuning {
                quant_shift: 2,
                hysteresis: 16,
                ..base
            }),
            "balanced" => Some(base),
            "fast" => Some(Tuning {
                quant_shift: 4,
                hysteresis: 48,
                bloom: false,
                stars: 0.5,
                particles: 0.6,
                ..base
            }),
            _ => None,
        }
    }
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning::preset("balanced").expect("balanced preset exists")
    }
}

impl Default for Hyperspace {
    fn default() -> Self {
        Self::new(0x5EED_CAFE)
    }
}

/// Points spread evenly over a unit sphere (Fibonacci lattice).
fn fibonacci_sphere(n: usize) -> Vec<[f32; 3]> {
    let golden = PI * (3.0 - 5f32.sqrt());
    (0..n)
        .map(|i| {
            let y = 1.0 - 2.0 * (i as f32 + 0.5) / n as f32;
            let r = (1.0 - y * y).sqrt();
            let theta = golden * i as f32;
            [r * theta.cos(), y, r * theta.sin()]
        })
        .collect()
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

fn mean(v: &[f32]) -> f32 {
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<f32>() / v.len() as f32
    }
}

impl Hyperspace {
    pub fn new(seed: u64) -> Self {
        Hyperspace {
            w: 0,
            h: 0,
            light: Vec::new(),
            glow: Vec::new(),
            scratch: Vec::new(),
            pixels: Vec::new(),
            stars: Vec::new(),
            sphere: fibonacci_sphere(700),
            rings: Vec::new(),
            particles: Vec::new(),
            rng: Rng(seed | 1),
            t: 0.0,
            dt: 1.0 / 30.0,
            yaw: 0.0,
            pitch: 0.4,
            energy: 0.0,
            bass: 0.0,
            speed: 0.0,
            warp: 0.0,
            flash: 0.0,
            shake: (0.0, 0.0),
            glitch: 0.0,
            hue: 0,
            bands: Vec::new(),
            wave: Vec::new(),
            title: String::new(),
            title_age: f32::MAX,
            track: None,
            beat: BeatDetector::default(),
            palette: Palette::default(),
            tuning: Tuning::default(),
            crisp: false,
            frame: Vec::new(),
            sum: Vec::new(),
            to_frame: false,
            title_px: Vec::new(),
        }
    }

    pub fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }

    /// Sets the framebuffer size in pixels.
    pub fn resize(&mut self, w: usize, h: usize) {
        if (w, h) == (self.w, self.h) {
            return;
        }
        self.w = w;
        self.h = h;
        let n = w * h;
        self.light = vec![[0.0; 3]; n];
        self.glow = vec![[0.0; 3]; n];
        self.scratch = vec![[0.0; 3]; n];
        self.frame = vec![[0.0; 3]; n];
        self.sum = vec![[0.0; 3]; n];
        self.pixels = vec![[0; 3]; n];
        let count = ((n as f32 / 45.0 * self.tuning.stars) as usize).clamp(30, 900);
        self.stars = (0..count).map(|_| self.new_star(true)).collect();
    }

    fn new_star(&mut self, anywhere: bool) -> Star {
        Star {
            x: self.rng.range(-1.6, 1.6),
            y: self.rng.range(-1.0, 1.0),
            z: if anywhere {
                self.rng.range(0.05, 1.0)
            } else {
                1.0
            },
            prev: None,
            tint: (self.rng.next_u64() % 6) as usize,
        }
    }

    /// How much detail fits at the current size.
    pub fn lod(&self) -> Lod {
        let s = self.w.min(self.h) as f32;
        let crisp = s < CRISP_BELOW;
        let bloom_radius = if self.tuning.bloom && s >= 48.0 {
            ((s / 40.0) as usize).clamp(1, 6)
        } else {
            0
        };
        // The sphere's disc is about 0.18 s in radius: about one point per two pixels of it.
        let r = 0.18 * s;
        let wanted = ((PI * r * r * 0.5) as usize).clamp(48, self.sphere.len().max(48));
        let sphere_step = self.sphere.len().div_ceil(wanted).max(1);
        let title_width = self.title.chars().count() * 4;
        let title_scale = if self.h < 24 {
            0
        } else if self.h >= 56 && title_width * 2 + 4 <= self.w {
            2
        } else {
            1
        };
        Lod {
            crisp,
            bloom_radius,
            nebula: self.tuning.nebula && s >= 48.0,
            scope: s >= 40.0,
            sphere_step,
            title_scale,
            // Only real strips: stretching a fullscreen scene costs terminal output for no gain.
            stretch: ((self.w as f32 / self.h.max(1) as f32) / 2.4).clamp(1.0, 2.5),
            fade: if crisp { 1.6 } else { 1.0 },
            persist_static: s >= 90.0,
        }
    }

    pub fn bpm(&self) -> Option<f32> {
        self.beat.bpm()
    }

    /// Whether anything is still moving (so the screen keeps redrawing).
    pub fn is_active(&self) -> bool {
        self.energy > 0.01
            || self.speed > 0.03
            || self.warp > 0.01
            || !self.rings.is_empty()
            || !self.particles.is_empty()
            || self.title_age < 6.0
    }

    /// Advances the scene. `bands` are spectrum levels (low to high), `wave` recent samples,
    /// `track` identifies the playing track and its title.
    pub fn update(
        &mut self,
        dt: f32,
        bands: &[f32],
        wave: &[f32],
        track: Option<(u64, &str)>,
        playing: bool,
    ) {
        let dt = dt.clamp(0.0, 0.1);
        self.dt = dt;
        self.t += dt;
        self.bands.clear();
        self.bands.extend_from_slice(bands);
        self.wave.clear();
        self.wave.extend_from_slice(wave);

        let n = bands.len();
        let bass_end = (n / 6).max(1).min(n);
        let bass = mean(&bands[..bass_end]);
        let energy = mean(bands);
        let smooth = 1.0 - (-dt * 8.0).exp();
        self.energy += (energy - self.energy) * smooth;
        self.bass += (bass - self.bass) * smooth;

        // A new track is a hyperspace jump with its title flying in.
        let key = track.map(|(k, _)| k);
        if key != self.track {
            self.track = key;
            if let Some((_, title)) = track {
                self.title = title.to_string();
                self.title_age = 0.0;
                self.warp += 3.0;
                self.flash = 1.0;
                self.beat.reset_tempo();
                self.spawn_ring(1.6);
                self.burst(160, 1.4);
            }
        }

        let beat = playing && self.beat.update(self.t, dt, bass);
        if beat {
            self.warp += 0.4 + self.bass;
            self.flash = 1.0;
            self.hue = (self.hue + 1) % 6;
            self.glitch = 0.25;
            self.spawn_ring(1.0);
            let count = ((30.0 + 90.0 * self.energy) * self.tuning.particles) as usize;
            self.burst(count, 0.6 + self.energy);
            let kick = 0.01 + 0.03 * self.bass;
            self.shake = (self.rng.range(-kick, kick), self.rng.range(-kick, kick));
        }
        self.warp *= (-dt * 2.2).exp();
        self.flash *= (-dt * 6.0).exp();
        self.shake.0 *= (-dt * 12.0).exp();
        self.shake.1 *= (-dt * 12.0).exp();
        self.glitch -= dt;
        self.title_age += dt;

        let target_speed = if playing {
            0.06 + 0.55 * self.energy
        } else {
            0.0
        };
        self.speed += (target_speed - self.speed) * (1.0 - (-dt * 2.0).exp());
        self.yaw += dt * (0.2 + 1.4 * self.energy);
        self.pitch += dt * (0.11 + 0.5 * self.energy);

        let speed = self.speed + self.warp;
        for i in 0..self.stars.len() {
            let mut s = self.stars[i];
            s.z -= speed * dt;
            if s.z < 0.03 {
                s = self.new_star(false);
            }
            self.stars[i] = s;
        }
        for p in &mut self.particles {
            p.x += p.vx * dt;
            p.y += p.vy * dt;
            let drag = (-dt * 1.8).exp();
            p.vx *= drag;
            p.vy *= drag;
            p.life -= dt;
        }
        self.particles.retain(|p| p.life > 0.0);
        for r in &mut self.rings {
            r.radius += r.speed * dt;
            r.life -= dt * 0.9;
        }
        self.rings.retain(|r| r.life > 0.0);
    }

    fn color(&self, i: usize) -> Rgb {
        self.palette.colors[(self.hue + i) % 6]
    }

    fn spawn_ring(&mut self, strength: f32) {
        let color = self.color(1);
        self.rings.push(Ring {
            radius: 0.18,
            speed: 0.55 * strength,
            life: strength.min(1.0),
            color,
        });
        if self.rings.len() > 12 {
            self.rings.remove(0);
        }
    }

    fn burst(&mut self, count: usize, power: f32) {
        for _ in 0..count {
            let angle = self.rng.range(0.0, TAU);
            let speed = power * self.rng.range(0.15, 0.6);
            let pick = (self.rng.next_u64() % 6) as usize;
            let color = self.color(pick);
            let life = self.rng.range(0.6, 1.6);
            self.particles.push(Particle {
                x: 0.2 * angle.cos(),
                y: 0.2 * angle.sin(),
                vx: speed * angle.cos(),
                vy: speed * angle.sin(),
                life,
                max_life: life,
                color,
            });
        }
        let excess = self.particles.len().saturating_sub(1500);
        self.particles.drain(..excess);
    }

    // ---- drawing --------------------------------------------------------------------------

    /// Adds light at a position: a soft sub-pixel (bilinear) splat, or a whole pixel on
    /// small screens where soft splats just look blurry.
    fn add(&mut self, x: f32, y: f32, c: Rgb, a: f32) {
        if a.is_nan() || a <= 0.0 || !x.is_finite() || !y.is_finite() {
            return;
        }
        if self.crisp {
            let (px, py) = (x.round(), y.round());
            if px >= 0.0 && py >= 0.0 && (px as usize) < self.w && (py as usize) < self.h {
                let buffer = if self.to_frame {
                    &mut self.frame
                } else {
                    &mut self.light
                };
                let cell = &mut buffer[py as usize * self.w + px as usize];
                cell[0] += c[0] * a;
                cell[1] += c[1] * a;
                cell[2] += c[2] * a;
            }
            return;
        }
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as isize, y0 as isize);
        for (dx, dy, wgt) in [
            (0, 0, (1.0 - fx) * (1.0 - fy)),
            (1, 0, fx * (1.0 - fy)),
            (0, 1, (1.0 - fx) * fy),
            (1, 1, fx * fy),
        ] {
            let (px, py) = (x0 + dx, y0 + dy);
            if px < 0 || py < 0 || px >= self.w as isize || py >= self.h as isize {
                continue;
            }
            let buffer = if self.to_frame {
                &mut self.frame
            } else {
                &mut self.light
            };
            let cell = &mut buffer[py as usize * self.w + px as usize];
            let k = a * wgt;
            cell[0] += c[0] * k;
            cell[1] += c[1] * k;
            cell[2] += c[2] * k;
        }
    }

    fn line(&mut self, from: (f32, f32), to: (f32, f32), colors: (Rgb, Rgb), a: f32) {
        let ((x0, y0), (x1, y1), (c0, c1)) = (from, to, colors);
        let steps = (x1 - x0)
            .abs()
            .max((y1 - y0).abs())
            .ceil()
            .clamp(1.0, 4000.0) as usize;
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            self.add(x0 + (x1 - x0) * t, y0 + (y1 - y0) * t, mix(c0, c1, t), a);
        }
    }

    fn ellipse(&mut self, cx: f32, cy: f32, rx: f32, ry: f32, c: Rgb, a: f32) {
        let steps = ((TAU * rx.max(ry)).ceil() as usize).clamp(8, 4000);
        for i in 0..steps {
            let th = i as f32 / steps as f32 * TAU;
            self.add(cx + rx * th.cos(), cy + ry * th.sin(), c, a);
        }
    }

    /// Renders the scene into the pixel buffer.
    pub fn render(&mut self) {
        let (w, h) = (self.w, self.h);
        if w == 0 || h == 0 {
            return;
        }
        let lod = self.lod();
        self.crisp = lod.crisp;
        // Persistence: the previous frame fades, leaving motion trails.
        let fade = (-self.dt * self.tuning.fade_rate * lod.fade).exp();
        for c in &mut self.light {
            for v in c.iter_mut() {
                *v *= fade;
                if *v < 0.01 {
                    *v = 0.0;
                }
            }
        }
        let s = w.min(h) as f32;
        let cx = w as f32 / 2.0 + self.shake.0 * s;
        let cy = h as f32 / 2.0 + self.shake.1 * s;
        // Wide areas stretch the rings, corona and sparks sideways; the sphere stays round.
        let sx = s * lod.stretch;
        let to_px = |u: f32, v: f32| (cx + u * sx, cy + v * s);

        if lod.nebula {
            self.draw_nebula(cx, cy, s, lod.stretch);
        }
        self.draw_stars(cx, cy, s);
        self.frame.iter_mut().for_each(|c| *c = [0.0; 3]);
        self.to_frame = !lod.persist_static;
        // Without trails these are not piled up over several frames, so they get a little more
        // light (but not so much that the colours wash out to white).
        let gain = if lod.persist_static { 1.0 } else { 1.4 };
        self.draw_corona(&to_px, s * (1.0 + lod.stretch) / 2.0, gain);
        if lod.scope {
            self.draw_scope(&to_px, gain);
        }
        self.draw_sphere(cx, cy, s, lod.sphere_step, gain);
        self.to_frame = false;
        for i in 0..self.rings.len() {
            let r = self.rings[i];
            let a = 0.9 * r.life * r.life;
            self.ellipse(cx, cy, r.radius * sx, r.radius * s, r.color, a);
        }
        for i in 0..self.particles.len() {
            let p = self.particles[i];
            let a = p.life / p.max_life;
            let (x, y) = to_px(p.x, p.y);
            let (tx, ty) = to_px(p.x - p.vx * 0.04, p.y - p.vy * 0.04);
            self.line((tx, ty), (x, y), (p.color, p.color), 0.5 * a);
        }
        self.draw_title(s, lod.title_scale);
        self.bloom_and_tonemap(lod.bloom_radius);
        self.stamp_title();
    }

    fn draw_nebula(&mut self, cx: f32, cy: f32, s: f32, stretch: f32) {
        let radius = 0.55 * s;
        let rx = radius * stretch;
        let strength = 0.012 + 0.05 * self.bass + 0.03 * self.flash;
        // Coarse steps keep most pixels unchanged between frames (cheaper to draw).
        let strength = (strength * 64.0).round() / 64.0;
        let color = self.color(0);
        let (x0, x1) = (
            (cx - rx).max(0.0) as usize,
            ((cx + rx) as usize).min(self.w),
        );
        let (y0, y1) = (
            (cy - radius).max(0.0) as usize,
            ((cy + radius) as usize).min(self.h),
        );
        for y in y0..y1 {
            for x in x0..x1 {
                let d =
                    (((x as f32 - cx) / rx).powi(2) + ((y as f32 - cy) / radius).powi(2)).sqrt();
                if d < 1.0 {
                    let k = strength * (1.0 - d) * (1.0 - d);
                    let cell = &mut self.light[y * self.w + x];
                    cell[0] += color[0] * k;
                    cell[1] += color[1] * k;
                    cell[2] += color[2] * k;
                }
            }
        }
    }

    fn draw_stars(&mut self, cx: f32, cy: f32, s: f32) {
        let warp = self.warp;
        for i in 0..self.stars.len() {
            let star = self.stars[i];
            let fov = 0.5 * s;
            let (x, y) = (cx + star.x / star.z * fov, cy + star.y / star.z * fov);
            let near = (1.0 - star.z).clamp(0.0, 1.0);
            let a = near * near * (0.8 + warp);
            let tint = mix([1.0, 1.0, 1.0], self.palette.colors[star.tint], 0.35);
            match star.prev {
                Some((px, py)) if (px - x).abs() + (py - y).abs() < s => {
                    self.line((px, py), (x, y), (tint, tint), a * 0.7)
                }
                _ => self.add(x, y, tint, a),
            }
            self.stars[i].prev = Some((x, y));
        }
    }

    /// `scale` is the ring's average radius unit in pixels (for spacing the spokes).
    fn draw_corona(&mut self, to_px: &impl Fn(f32, f32) -> (f32, f32), scale: f32, gain: f32) {
        let n = self.bands.len();
        if n == 0 {
            return;
        }
        let spin = self.t * 0.08;
        let pulse = self.beat.pulse;
        // At most one spoke per ~3.5 pixels of the inner circle, so spokes stay separate rays
        // instead of merging into a solid band; neighbouring bands are merged by their peak.
        let spokes = ((PI * 0.29 * scale / 3.5) as usize).clamp(8, n);
        for j in 0..spokes {
            let (lo, hi) = (
                j * n / spokes,
                ((j + 1) * n / spokes).max(j * n / spokes + 1),
            );
            let level = self.bands[lo..hi.min(n)]
                .iter()
                .copied()
                .fold(0.0, f32::max);
            if level <= 0.01 {
                continue;
            }
            let offset = (j as f32 + 0.5) / spokes as f32 * PI;
            let (inner, outer) = (0.29, 0.29 + level * 0.2 + pulse * 0.02);
            let (c0, c1) = (self.color(2), self.color(1));
            for angle in [PI / 2.0 + offset + spin, PI / 2.0 - offset + spin] {
                let (dx, dy) = (angle.cos(), angle.sin());
                let (x0, y0) = to_px(dx * inner, dy * inner);
                let (x1, y1) = to_px(dx * outer, dy * outer);
                self.line((x0, y0), (x1, y1), (c0, c1), (0.25 + 0.5 * level) * gain);
            }
        }
    }

    fn draw_scope(&mut self, to_px: &impl Fn(f32, f32) -> (f32, f32), gain: f32) {
        let n = self.wave.len();
        if n < 2 {
            return;
        }
        let color = self.color(3);
        let a = (0.35 + 0.6 * self.energy) * gain;
        let point = |i: usize, wave: &[f32]| {
            let th = i as f32 / n as f32 * TAU - PI / 2.0;
            let r = 0.245 + wave[i % n].clamp(-1.0, 1.0) * 0.07;
            to_px(r * th.cos(), r * th.sin())
        };
        let mut prev = point(0, &self.wave);
        for i in 1..=n {
            let next = point(i, &self.wave);
            self.line(prev, next, (color, color), a * 0.6);
            prev = next;
        }
    }

    fn draw_sphere(&mut self, cx: f32, cy: f32, s: f32, step: usize, gain: f32) {
        let n = self.bands.len();
        let (sy, cyaw) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        let pulse = self.beat.pulse;
        for i in (0..self.sphere.len()).step_by(step.max(1)) {
            let p = self.sphere[i];
            // Bass bulges the equator, treble the poles.
            let lat = p[1].abs();
            let level = if n == 0 {
                0.0
            } else {
                self.bands[((lat * (n - 1) as f32) as usize).min(n - 1)]
            };
            let r = 0.15 * (1.0 + 0.6 * level + 0.18 * pulse);
            // Rotate around Y (yaw) then X (pitch).
            let (x, z) = (p[0] * cyaw - p[2] * sy, p[0] * sy + p[2] * cyaw);
            let (y, z) = (p[1] * cp - z * sp, p[1] * sp + z * cp);
            let depth = 3.0 + z * r * 3.0;
            let (px, py) = (cx + x * r * 3.0 / depth * s, cy + y * r * 3.0 / depth * s);
            let front = 1.0 - (z + 1.0) / 2.0;
            let color = mix(self.color(0), self.color(2), lat);
            self.add(px, py, color, (0.15 + 0.85 * front) * (0.5 + level) * gain);
        }
    }

    fn draw_title(&mut self, s: f32, scale: usize) {
        let age = self.title_age;
        self.title_px.clear();
        if age > 6.0 || self.title.is_empty() || scale == 0 {
            return;
        }
        let alpha = if age < 0.6 {
            age / 0.6
        } else if age < 4.5 {
            1.0
        } else {
            1.0 - (age - 4.5) / 1.5
        };
        let text: Vec<char> = self.title.chars().collect();
        let max_chars = (self.w.saturating_sub(4)) / (4 * scale);
        let text = &text[..text.len().min(max_chars)];
        let width = text.len() * 4 * scale;
        let x0 = (self.w.saturating_sub(width)) / 2;
        // Slides up into place while fading in.
        let rise = (1.0 - (age / 0.6).min(1.0)) * 0.1 * s;
        let y0 = ((self.h as f32 * 0.82 + rise) as usize).min(self.h.saturating_sub(5 * scale + 1));
        self.title_px.clear();
        for (i, &ch) in text.iter().enumerate() {
            let color = self.color(i);
            let rows = glyph(ch);
            for (gy, row) in rows.iter().enumerate() {
                let jitter = if self.glitch > 0.0 {
                    self.rng.range(-2.0, 2.0) as isize
                } else {
                    0
                };
                for (gx, cell) in row.chars().enumerate() {
                    if cell != '#' {
                        continue;
                    }
                    for sy in 0..scale {
                        for sx in 0..scale {
                            let x = (x0 + i * 4 * scale + gx * scale + sx) as isize + jitter;
                            let y = y0 + gy * scale + sy;
                            self.add(x as f32, y as f32, color, 1.1 * alpha);
                            if x >= 0 && (x as usize) < self.w && y < self.h {
                                self.title_px.push((x as usize, y, color, alpha));
                            }
                        }
                    }
                }
            }
        }
    }

    fn bloom_and_tonemap(&mut self, radius: usize) {
        let (w, h) = (self.w, self.h);
        for ((sum, light), frame) in self.sum.iter_mut().zip(&self.light).zip(&self.frame) {
            *sum = [
                light[0] + frame[0],
                light[1] + frame[1],
                light[2] + frame[2],
            ];
        }
        // Separable box blur of the light into `glow`.
        if radius > 0 {
            box_blur(&self.sum, &mut self.scratch, w, h, radius, true);
            box_blur(&self.scratch, &mut self.glow, w, h, radius, false);
        } else {
            self.glow.iter_mut().for_each(|g| *g = [0.0; 3]);
        }
        let strength = 0.9 + 0.8 * self.flash;
        let bg = self.palette.bg;
        let mask = !((1u8 << self.tuning.quant_shift) - 1);
        let quantise = |v: f32| ((v.clamp(0.0, 1.0) * 255.0) as u8) & mask;
        let background = bg.map(quantise);
        for i in 0..w * h {
            let total: f32 = (0..3)
                .map(|ch| self.sum[i][ch] + self.glow[i][ch] * strength)
                .sum();
            // Near-black is exactly the background, so empty space never needs redrawing.
            if total < self.tuning.floor {
                self.pixels[i] = background;
                continue;
            }
            let mut out = [0u8; 3];
            for ch in 0..3 {
                let x = self.sum[i][ch] + self.glow[i][ch] * strength;
                let v = bg[ch] + (1.0 - bg[ch]) * (1.0 - (-x * 1.3).exp());
                // Quantise so slowly fading pixels change less often.
                out[ch] = quantise(v);
            }
            let shown = self.pixels[i];
            let moved = (0..3).any(|ch| out[ch].abs_diff(shown[ch]) > self.tuning.hysteresis);
            if moved {
                self.pixels[i] = out;
            }
        }
    }

    /// Draws the title over the finished image with a dark outline, so it stays readable over
    /// the busy middle of the scene (its light has already gone into the glow).
    fn stamp_title(&mut self) {
        if self.title_px.is_empty() {
            return;
        }
        let (w, h) = (self.w as isize, self.h as isize);
        let bg = self.palette.bg;
        let mask = !((1u8 << self.tuning.quant_shift) - 1);
        let quantise = |v: f32| ((v.clamp(0.0, 1.0) * 255.0) as u8) & mask;
        let blend = |shown: [u8; 3], to: Rgb, a: f32| {
            [0, 1, 2].map(|ch| quantise(shown[ch] as f32 / 255.0 * (1.0 - a) + to[ch] * a))
        };
        for &(x, y, _, alpha) in &self.title_px {
            for (dx, dy) in [
                (-1, -1),
                (0, -1),
                (1, -1),
                (-1, 0),
                (1, 0),
                (-1, 1),
                (0, 1),
                (1, 1),
            ] {
                let (nx, ny) = (x as isize + dx, y as isize + dy);
                if nx >= 0 && ny >= 0 && nx < w && ny < h {
                    let i = ny as usize * self.w + nx as usize;
                    self.pixels[i] = blend(self.pixels[i], bg, 0.85 * alpha);
                }
            }
        }
        for &(x, y, color, alpha) in &self.title_px {
            let i = y * self.w + x;
            // A hot core: the neon colour pushed a little towards white.
            self.pixels[i] = blend(self.pixels[i], mix(color, [1.0; 3], 0.2), alpha);
        }
    }

    /// Final colour of pixel (x, y).
    pub fn pixel(&self, x: usize, y: usize) -> [u8; 3] {
        self.pixels.get(y * self.w + x).copied().unwrap_or([0; 3])
    }
}

/// Horizontal (or vertical) box blur of `src` into `dst`.
fn box_blur(src: &[Rgb], dst: &mut [Rgb], w: usize, h: usize, r: usize, horizontal: bool) {
    let (outer, inner) = if horizontal { (h, w) } else { (w, h) };
    let idx = |o: usize, i: usize| if horizontal { o * w + i } else { i * w + o };
    let norm = 1.0 / (2 * r + 1) as f32;
    for o in 0..outer {
        let mut sum = [0.0f32; 3];
        for i in 0..=r.min(inner - 1) {
            let v = src[idx(o, i)];
            sum[0] += v[0];
            sum[1] += v[1];
            sum[2] += v[2];
        }
        for i in 0..inner {
            dst[idx(o, i)] = [sum[0] * norm, sum[1] * norm, sum[2] * norm];
            if i + r + 1 < inner {
                let v = src[idx(o, i + r + 1)];
                sum[0] += v[0];
                sum[1] += v[1];
                sum[2] += v[2];
            }
            if i >= r {
                let v = src[idx(o, i - r)];
                sum[0] -= v[0];
                sum[1] -= v[1];
                sum[2] -= v[2];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit_pixels(hs: &Hyperspace) -> usize {
        let bg = hs.pixel(0, 0);
        let (w, h) = hs.size();
        (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .filter(|&(x, y)| hs.pixel(x, y) != bg)
            .count()
    }

    #[test]
    fn detects_a_steady_120_bpm_kick() {
        let mut beat = BeatDetector::default();
        let dt = 1.0 / 60.0;
        let mut beats = 0;
        for frame in 0..(60 * 8) {
            let t = frame as f32 * dt;
            // A kick every 0.5 s that decays over ~0.1 s, on top of a steady bed.
            let since = t % 0.5;
            let energy = 0.5 + 0.45 * (-since * 30.0).exp();
            if beat.update(t, dt, energy) {
                beats += 1;
            }
        }
        assert!((14..=17).contains(&beats), "{beats} beats in 8 s");
        let bpm = beat.bpm().expect("tempo");
        assert!((bpm - 120.0).abs() < 3.0, "estimated {bpm} BPM");
    }

    #[test]
    fn steady_sound_has_no_beats() {
        let mut beat = BeatDetector::default();
        let beats = (0..600)
            .filter(|&f| beat.update(f as f32 / 60.0, 1.0 / 60.0, 0.7))
            .count();
        assert_eq!(beats, 0);
    }

    /// Writes frames as PPM images (one pixel per framebuffer pixel) to `HYPERSPACE_PPM_DIR`,
    /// with a music-like spectrum: louder bass, a kick every 0.5 s.
    /// `HYPERSPACE_PPM_DIR=dir cargo test --release --lib hyperspace_ppm_dump -- --ignored`
    #[test]
    #[ignore]
    fn hyperspace_ppm_dump() {
        let Ok(dir) = std::env::var("HYPERSPACE_PPM_DIR") else {
            return;
        };
        for (w, h) in [(118, 18), (78, 32), (118, 64), (200, 110)] {
            let mut hs = Hyperspace::new(11);
            hs.resize(w, h);
            let mut rng = Rng(7);
            for frame in 0..150 {
                let t = frame as f32 / 30.0;
                let kick = (-(t % 0.5) * 12.0).exp();
                let bands: Vec<f32> = (0..48)
                    .map(|b| {
                        let tilt = 0.75 - 0.5 * b as f32 / 47.0;
                        (tilt + 0.25 * kick * (1.0 - b as f32 / 47.0) + rng.range(-0.12, 0.12))
                            .clamp(0.0, 1.0)
                    })
                    .collect();
                let wave: Vec<f32> = (0..256)
                    .map(|i| (i as f32 * 0.21 + t * 9.0).sin() * 0.5 + rng.range(-0.1, 0.1))
                    .collect();
                hs.update(1.0 / 30.0, &bands, &wave, Some((3, "Black Dog")), true);
                hs.render();
            }
            let mut out = format!(
                "P6
{w} {h}
255
"
            )
            .into_bytes();
            for y in 0..h {
                for x in 0..w {
                    out.extend_from_slice(&hs.pixel(x, y));
                }
            }
            std::fs::write(format!("{dir}/{w}x{h}.ppm"), out).unwrap();
        }
    }

    /// Share of the screen blown out to near-white after a few seconds of busy music.
    fn washout(w: usize, h: usize) -> f32 {
        let mut hs = Hyperspace::new(11);
        hs.resize(w, h);
        let wave: Vec<f32> = (0..256).map(|i| (i as f32 * 0.21).sin() * 0.6).collect();
        for frame in 0..120 {
            let level = if frame % 15 == 0 { 0.95 } else { 0.6 };
            hs.update(
                1.0 / 30.0,
                &[level; 48],
                &wave,
                Some((3, "Black Dog")),
                true,
            );
            hs.render();
        }
        let white = hs
            .pixels
            .iter()
            .filter(|p| p.iter().all(|&c| c >= 200))
            .count();
        white as f32 / (w * h) as f32
    }

    #[test]
    fn small_screens_are_not_washed_out() {
        // Before the level of detail, these were 15-30 % white blobs.
        for (w, h) in [(118, 18), (78, 32), (118, 64)] {
            let white = washout(w, h);
            assert!(
                white < 0.05,
                "{w}x{h}: {:.1} % of the screen is white",
                white * 100.0
            );
        }
    }

    #[test]
    fn detail_follows_the_screen_size() {
        let lod = |w, h| {
            let mut hs = Hyperspace::new(1);
            hs.resize(w, h);
            hs.title = "Black Dog".into();
            hs.lod()
        };
        // The wide strip under the playlist: crisp, no glow, no title, spread sideways.
        let strip = lod(118, 18);
        assert!(strip.crisp && !strip.nebula && !strip.scope && !strip.persist_static);
        assert_eq!((strip.bloom_radius, strip.title_scale), (0, 0));
        assert_eq!(strip.stretch, 2.5);
        // A small window.
        let small = lod(78, 32);
        assert!(small.crisp && small.title_scale == 1 && small.bloom_radius == 0);
        // A big fullscreen terminal gets everything.
        let big = lod(200, 110);
        assert!(!big.crisp && big.nebula && big.scope && big.persist_static);
        assert!(big.bloom_radius >= 2 && big.title_scale == 2);
        assert_eq!(big.stretch, 1.0);
        // Fewer sphere points on smaller screens, so they never merge into a blob.
        assert!(strip.sphere_step > small.sphere_step && small.sphere_step > big.sphere_step);
    }

    #[test]
    fn the_title_is_outlined_over_the_scene() {
        let mut hs = Hyperspace::new(4);
        hs.resize(78, 32);
        for _ in 0..30 {
            hs.update(1.0 / 30.0, &[0.8; 48], &[], Some((1, "IIII")), true);
            hs.render();
        }
        assert!(!hs.title_px.is_empty());
        let bg = hs.palette.bg.map(|c| (c * 255.0) as u8);
        let lit: std::collections::HashSet<_> = hs.title_px.iter().map(|p| (p.0, p.1)).collect();
        // Every title pixel is bright, and the pixels around the letters are dark.
        for &(x, y) in &lit {
            assert!(
                hs.pixel(x, y).iter().any(|&c| c > 128),
                "title pixel {x},{y} is dark"
            );
            for (nx, ny) in [(x - 1, y), (x + 1, y)] {
                if !lit.contains(&(nx, ny)) {
                    let p = hs.pixel(nx, ny);
                    let near = (0..3).all(|c| p[c].abs_diff(bg[c]) <= 48);
                    assert!(near, "outline pixel {nx},{ny} is {p:?}");
                }
            }
        }
    }

    #[test]
    fn music_lights_up_the_scene() {
        let mut hs = Hyperspace::new(1);
        hs.resize(120, 60);
        let wave: Vec<f32> = (0..256).map(|i| (i as f32 * 0.2).sin() * 0.5).collect();
        for frame in 0..60 {
            let level = if frame % 15 == 0 { 0.95 } else { 0.6 };
            hs.update(
                1.0 / 30.0,
                &[level; 48],
                &wave,
                Some((7, "Pink Floyd - Money")),
                true,
            );
            hs.render();
        }
        let lit = lit_pixels(&hs);
        assert!(lit > 120 * 60 / 5, "only {lit} lit pixels");
        assert!(hs.is_active());
        assert!(hs.pixels.iter().all(|p| p.iter().all(|&c| c % 4 == 0)));
    }

    #[test]
    fn a_new_track_jumps_to_hyperspace_and_shows_its_title() {
        let mut hs = Hyperspace::new(2);
        hs.resize(160, 60);
        hs.update(1.0 / 30.0, &[0.5; 48], &[], Some((1, "Track One")), true);
        assert_eq!(hs.title, "Track One");
        assert!(hs.warp > 2.0, "no hyperspace jump");
        assert!(!hs.rings.is_empty() && !hs.particles.is_empty());
        for _ in 0..30 {
            hs.update(1.0 / 30.0, &[0.5; 48], &[], Some((1, "Track One")), true);
        }
        // The title is drawn in the lower part of the screen.
        hs.render();
        let bg = hs.pixel(0, 0);
        let title_rows = (40..60)
            .filter(|&y| (0..160).any(|x| hs.pixel(x, y) != bg))
            .count();
        assert!(title_rows > 3);
    }

    #[test]
    fn silence_settles_down() {
        let mut hs = Hyperspace::new(3);
        hs.resize(80, 40);
        hs.update(1.0 / 30.0, &[0.9; 32], &[], Some((5, "x")), true);
        for _ in 0..600 {
            hs.update(1.0 / 30.0, &[0.0; 32], &[], Some((5, "x")), false);
            hs.render();
        }
        assert!(!hs.is_active());
    }

    #[test]
    fn tiny_and_degenerate_sizes_do_not_panic() {
        for (w, h) in [(0, 0), (1, 1), (2, 1), (3, 2), (1, 50), (50, 1)] {
            let mut hs = Hyperspace::new(4);
            hs.resize(w, h);
            hs.update(
                0.05,
                &[1.0; 8],
                &[0.5, -0.5],
                Some((1, "ÄÖ Ünicode ✓")),
                true,
            );
            hs.render();
            hs.update(0.0, &[], &[], None, false);
            hs.render();
        }
    }

    #[test]
    fn font_covers_titles() {
        for (ch, rows) in FONT {
            assert!(rows.iter().all(|r| r.len() == 3), "{ch}");
        }
        assert_eq!(glyph('ä'), glyph('A'));
        assert_eq!(glyph('ö'), glyph('O'));
        assert_eq!(glyph('★'), glyph('?'));
    }
}
