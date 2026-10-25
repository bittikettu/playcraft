//! The TV picture: the music's visuals drawn for a 720p screen, with a "Now Playing" strip
//! along the bottom. It is drawn on the TV stream's own thread from the same audio that goes
//! into the stream, so picture and sound match.

use std::time::Duration;

use crate::hyperspace::{Hyperspace, Palette, Tuning};
use crate::outputs::card::CardColors;
use crate::outputs::pixels::{Canvas, fit};
use crate::spectrum::SpectrumAnalyzer;

pub const WIDTH: usize = 1280;
pub const HEIGHT: usize = 720;
/// HYPERSPACE is drawn at a fraction of the picture size and scaled up (crisp pixels).
const HYPERSPACE_SCALE: usize = 2;
const HYPERSPACE_BANDS: usize = 48;
const BAR_BANDS: usize = 64;
/// Height of the "Now Playing" strip.
const STRIP: usize = 150;
const SIDE: usize = 48;

/// What fills the picture above the strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TvVisual {
    Hyperspace,
    Bars,
}

/// What the picture shows besides the music; written by the UI thread.
#[derive(Debug, Clone)]
pub struct TvInfo {
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub position: Duration,
    pub duration: Option<Duration>,
    pub playing: bool,
    pub colors: CardColors,
    pub palette: Palette,
    pub visual: TvVisual,
    /// Tells songs apart (HYPERSPACE greets each new one).
    pub track: Option<u64>,
}

impl Default for TvInfo {
    fn default() -> Self {
        TvInfo {
            title: "mp3blaster-rs".into(),
            artist: None,
            album: None,
            position: Duration::ZERO,
            duration: None,
            playing: false,
            colors: CardColors {
                background: [0x28, 0x2a, 0x36],
                frame: [0x62, 0x72, 0xa4],
                title: [0xbd, 0x93, 0xf9],
                text: [0xf8, 0xf8, 0xf2],
                dim: [0x62, 0x72, 0xa4],
                low: [0xbd, 0x93, 0xf9],
                mid: [0xff, 0x79, 0xc6],
                hot: [0xff, 0x55, 0x55],
                accent: [0xf1, 0xfa, 0x8c],
            },
            palette: Palette::default(),
            visual: TvVisual::Hyperspace,
            track: None,
        }
    }
}

/// `mm:ss`, or `h:mm:ss` from an hour up.
fn clock(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

pub struct Scene {
    canvas: Canvas,
    spectrum: SpectrumAnalyzer,
    hyperspace: Hyperspace,
    /// Pac-Man's chomps.
    chomp: f32,
}

impl Default for Scene {
    fn default() -> Self {
        Self::new()
    }
}

impl Scene {
    pub fn new() -> Self {
        let mut hyperspace = Hyperspace::new(0x7e1e);
        // Full colour detail: the terminal presets round colours and skip small changes to
        // spare the terminal, which only costs a video picture.
        if let Some(balanced) = Tuning::preset("balanced") {
            hyperspace.tuning = Tuning {
                quant_shift: 0,
                hysteresis: 0,
                ..balanced
            };
        }
        hyperspace.resize(WIDTH / HYPERSPACE_SCALE, HEIGHT / HYPERSPACE_SCALE);
        Scene {
            canvas: Canvas::new(WIDTH, HEIGHT, [0; 3]),
            spectrum: SpectrumAnalyzer::new(),
            hyperspace,
            chomp: 0.0,
        }
    }

    /// Draws the next frame, `dt` seconds after the last one. `samples` is the mono audio
    /// (48 kHz) that plays during it. Returns the picture as RGB bytes.
    pub fn frame(&mut self, dt: f32, samples: &[f32], info: &TvInfo) -> &[u8] {
        let bands = match info.visual {
            TvVisual::Hyperspace => HYPERSPACE_BANDS,
            TvVisual::Bars => BAR_BANDS,
        };
        self.spectrum.set_band_count(bands);
        self.spectrum.push_samples(samples);
        self.spectrum.update(Duration::from_secs_f32(dt));
        if info.playing {
            let loud = self.spectrum.bands().iter().sum::<f32>() / bands as f32;
            self.chomp += dt * (2.5 + 5.5 * loud.min(1.0));
        }
        match info.visual {
            TvVisual::Hyperspace => self.draw_hyperspace(dt, info),
            TvVisual::Bars => self.draw_bars(info),
        }
        self.draw_strip(info);
        &self.canvas.pixels
    }

    fn draw_hyperspace(&mut self, dt: f32, info: &TvInfo) {
        let wave = self.spectrum.waveform(256);
        let hs = &mut self.hyperspace;
        hs.palette = info.palette;
        hs.update(
            dt,
            self.spectrum.bands(),
            &wave,
            info.track.map(|t| (t, info.title.as_str())),
            info.playing,
        );
        hs.render();
        // Each scene pixel becomes a square of the picture.
        let s = HYPERSPACE_SCALE;
        let row_bytes = WIDTH * 3;
        for y in 0..HEIGHT / s {
            let top = y * s * row_bytes;
            let line = &mut self.canvas.pixels[top..top + row_bytes];
            for (block, p) in line.chunks_exact_mut(3 * s).zip(hs.row(y)) {
                for px in block.as_chunks_mut::<3>().0 {
                    *px = *p;
                }
            }
            let (done, rest) = self.canvas.pixels.split_at_mut(top + row_bytes);
            for dy in 1..s {
                let at = (dy - 1) * row_bytes;
                rest[at..at + row_bytes].copy_from_slice(&done[top..top + row_bytes]);
            }
        }
        self.canvas
            .text("mp3blaster-rs", 24, 20, 2, info.colors.dim);
    }

    /// Spectrum bars of stacked blocks in the theme's gradient, with falling peaks.
    fn draw_bars(&mut self, info: &TvInfo) {
        let c = &info.colors;
        self.canvas.fill(c.background);
        let pitch = (WIDTH - 2 * SIDE) / BAR_BANDS;
        let left = (WIDTH - pitch * BAR_BANDS) / 2;
        let block = 14;
        let floor = HEIGHT - STRIP - 24;
        let most = (floor - 70) / block;
        for (k, (&level, &peak)) in self
            .spectrum
            .bands()
            .iter()
            .zip(self.spectrum.peaks())
            .enumerate()
        {
            let x = left + k * pitch;
            let blocks = (level * most as f32).round() as usize;
            for b in 0..blocks.min(most) {
                let height = b as f32 / most as f32;
                let color = if height < 0.4 {
                    c.low
                } else if height < 0.75 {
                    c.mid
                } else {
                    c.hot
                };
                self.canvas
                    .rect(x, floor - (b + 1) * block, pitch - 4, block - 3, color);
            }
            let top = (peak * most as f32).round() as usize;
            if top > blocks {
                self.canvas.rect(
                    x,
                    floor - (top + 1) * block + block - 6,
                    pitch - 4,
                    4,
                    c.hot,
                );
            }
        }
        self.canvas.text("mp3blaster-rs", 24, 20, 2, c.dim);
    }

    /// The strip along the bottom: title, artist and album, the time, and Pac-Man eating
    /// his way through the song.
    fn draw_strip(&mut self, info: &TvInfo) {
        let c = &info.colors;
        let top = HEIGHT - STRIP;
        self.canvas
            .blend_rect(0, top, WIDTH, STRIP, c.background, 0.85);
        self.canvas.rect(0, top, WIDTH, 3, c.frame);

        let time = match info.duration {
            Some(d) => format!("{} / {}", clock(info.position), clock(d)),
            None => clock(info.position),
        };
        let time_width = time.chars().count() * 24;
        self.canvas
            .text(&time, WIDTH - SIDE - time_width, top + 22, 3, c.text);
        let room = (WIDTH - 2 * SIDE - time_width - 24) / 32;
        self.canvas
            .text(&fit(&info.title, room), SIDE, top + 18, 4, c.title);
        let byline = match (&info.artist, &info.album) {
            (Some(a), Some(b)) => format!("{a} · {b}"),
            (Some(a), None) => a.clone(),
            (None, Some(b)) => b.clone(),
            (None, None) => String::new(),
        };
        let room = (WIDTH - 2 * SIDE) / 16;
        self.canvas
            .text(&fit(&byline, room), SIDE, top + 62, 2, c.text);

        // Pellets to the end of the song, Pac-Man at the position, a ghost behind him.
        let y = HEIGHT - 34;
        let (x0, x1) = (SIDE + 16, WIDTH - SIDE - 16);
        let ratio = match info.duration {
            Some(d) if !d.is_zero() => {
                (info.position.as_secs_f64() / d.as_secs_f64()).clamp(0.0, 1.0)
            }
            _ => 0.0,
        };
        let pac_x = x0 + ((x1 - x0) as f64 * ratio) as usize;
        let mut x = x0 + 24;
        while x < x1 - 20 {
            if x > pac_x + 20 {
                self.canvas.rect(x - 3, y - 3, 6, 6, c.dim);
            }
            x += 24;
        }
        self.canvas.rect(x1 - 8, y - 8, 16, 16, c.dim);
        let open = info.playing && self.chomp.fract() < 0.5;
        self.pac_man(pac_x, y, 15, open, c.accent);
        if pac_x >= x0 + 60 {
            let wobble = if (self.chomp * 0.5).fract() < 0.5 {
                0
            } else {
                6
            };
            self.ghost(pac_x - 56 - wobble, y, c.hot);
        }
    }

    /// A Pac-Man facing right, centred at (`cx`, `cy`).
    fn pac_man(&mut self, cx: usize, cy: usize, r: usize, open: bool, color: [u8; 3]) {
        let r = r as isize;
        for dy in -r..=r {
            for dx in -r..=r {
                if dx * dx + dy * dy > r * r {
                    continue;
                }
                // The mouth: a wedge opening to the right.
                if open && dx > 0 && dy.abs() * 10 < dx * 8 {
                    continue;
                }
                let (x, y) = (cx as isize + dx, cy as isize + dy);
                if x >= 0 && y >= 0 {
                    self.canvas.set(x as usize, y as usize, color);
                }
            }
        }
    }

    /// A ghost: a dome with a wavy hem, and eyes.
    fn ghost(&mut self, cx: usize, cy: usize, color: [u8; 3]) {
        let r = 14isize;
        for dy in -r..=r {
            for dx in -r..=r {
                let inside = if dy <= 0 {
                    dx * dx + dy * dy <= r * r
                } else {
                    // The hem: three points along the bottom.
                    dx.abs() <= r && (dy < r - 4 || (dx + r) % 10 < 5)
                };
                if inside {
                    self.canvas.set(
                        (cx as isize + dx) as usize,
                        (cy as isize + dy) as usize,
                        color,
                    );
                }
            }
        }
        for ex in [cx - 6, cx + 4] {
            self.canvas.rect(ex - 2, cy - 6, 5, 6, [0xff, 0xff, 0xff]);
            self.canvas.rect(ex, cy - 4, 3, 3, [0x20, 0x20, 0x80]);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A second of a 110 Hz tone with a bass kick, as mono 48 kHz samples per frame.
    pub(crate) fn tone(frame: usize) -> Vec<f32> {
        (0..1600)
            .map(|i| {
                let t = (frame * 1600 + i) as f32 / 48_000.0;
                let kick = (-(t % 0.5) * 10.0).exp();
                (t * 110.0 * std::f32::consts::TAU).sin() * (0.3 + 0.5 * kick)
            })
            .collect()
    }

    fn info(visual: TvVisual) -> TvInfo {
        TvInfo {
            title: "So What".into(),
            artist: Some("Miles Davis".into()),
            album: Some("Kind of Blue (1959)".into()),
            position: Duration::from_secs(150),
            duration: Some(Duration::from_secs(562)),
            playing: true,
            visual,
            track: Some(1),
            ..TvInfo::default()
        }
    }

    fn count(frame: &[u8], color: [u8; 3]) -> usize {
        frame.chunks(3).filter(|p| *p == color).count()
    }

    #[test]
    fn draws_the_strip_over_the_visuals() {
        for visual in [TvVisual::Hyperspace, TvVisual::Bars] {
            let mut scene = Scene::new();
            let info = info(visual);
            for n in 0..20 {
                scene.frame(1.0 / 30.0, &tone(n), &info);
            }
            let frame = scene.frame(1.0 / 30.0, &tone(20), &info).to_vec();
            assert_eq!(frame.len(), WIDTH * HEIGHT * 3);
            let c = info.colors;
            assert!(count(&frame, c.title) > 1000, "{visual:?}: title");
            assert!(count(&frame, c.accent) > 300, "{visual:?}: Pac-Man");
            assert!(count(&frame, c.hot) > 300, "{visual:?}: the ghost");
            // The frame line above the strip.
            let strip_top = (HEIGHT - STRIP) * WIDTH * 3;
            assert_eq!(&frame[strip_top..strip_top + 3], &c.frame);
        }
    }

    #[test]
    fn bars_rise_with_the_music_and_hyperspace_fills_the_screen() {
        let mut scene = Scene::new();
        let quiet = info(TvVisual::Bars);
        let silent = scene.frame(1.0 / 30.0, &[0.0; 1600], &quiet).to_vec();
        let low = quiet.colors.low;
        let before = count(&silent, low);
        for n in 0..10 {
            scene.frame(1.0 / 30.0, &tone(n), &quiet);
        }
        let after = count(scene.frame(1.0 / 30.0, &tone(10), &quiet), low);
        assert!(after > before + 1000, "{before} -> {after}");

        // HYPERSPACE: not just the background above the strip, and every scene pixel is
        // a 2x2 square.
        let mut scene = Scene::new();
        let hs = info(TvVisual::Hyperspace);
        let mut frame = Vec::new();
        for n in 0..30 {
            frame = scene.frame(1.0 / 30.0, &tone(n), &hs).to_vec();
        }
        let at = |x: usize, y: usize| {
            let i = (y * WIDTH + x) * 3;
            [frame[i], frame[i + 1], frame[i + 2]]
        };
        let distinct: std::collections::HashSet<[u8; 3]> =
            (0..HEIGHT - STRIP).step_by(7).map(|y| at(640, y)).collect();
        assert!(distinct.len() > 5, "{distinct:?}");
        for (x, y) in [(300, 200), (902, 404), (40, 60)] {
            assert_eq!(at(x, y), at(x + 1, y + 1));
        }
    }

    /// How long a frame takes to draw: `cargo test --release --lib tv_frame_time --
    /// --ignored --nocapture`. The stream needs well under 33 ms.
    #[test]
    #[ignore]
    fn tv_frame_time() {
        for visual in [TvVisual::Hyperspace, TvVisual::Bars] {
            let mut scene = Scene::new();
            let info = info(visual);
            for n in 0..30 {
                scene.frame(1.0 / 30.0, &tone(n), &info);
            }
            let started = std::time::Instant::now();
            for n in 30..150 {
                scene.frame(1.0 / 30.0, &tone(n), &info);
            }
            let ms = started.elapsed().as_secs_f64() * 1000.0 / 120.0;
            println!("{visual:?}: {ms:.1} ms per frame");
        }
    }

    #[test]
    fn clock_formats() {
        assert_eq!(clock(Duration::from_secs(75)), "01:15");
        assert_eq!(clock(Duration::from_secs(3725)), "1:02:05");
    }
}
