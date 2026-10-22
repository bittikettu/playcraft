//! "Matrix" digital rain made of the playing file's own bytes.
//!
//! `ByteFeed` reads the audio file around the current playback position and hands out its
//! bytes as hex digits; `MatrixRain` drops streams of those digits down the screen, one
//! stream per column, driven by the spectrum: louder bands start drops more often and make
//! them faster and longer.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const HEX: &[u8; 16] = b"0123456789ABCDEF";
const BLOCK: usize = 4096;
/// Jump to the playback position when the feed has drifted further than this.
const MAX_DRIFT: u64 = 256 * 1024;

/// Small, fast, seedable generator (xorshift64*); visuals need no more.
#[derive(Debug, Clone)]
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [0, 1).
    fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// Reads the playing file near the playback position and yields its bytes as hex digits.
pub struct ByteFeed {
    path: Option<PathBuf>,
    file: Option<File>,
    len: u64,
    buf: Vec<u8>,
    idx: usize,
    high_nibble: bool,
    /// File offset of `buf[0]`.
    buf_offset: u64,
    /// Offset that follows playback (fraction of the file played × length).
    target: u64,
    rng: Rng,
}

impl ByteFeed {
    fn new(seed: u64) -> Self {
        ByteFeed {
            path: None,
            file: None,
            len: 0,
            buf: Vec::with_capacity(BLOCK),
            idx: 0,
            high_nibble: true,
            buf_offset: 0,
            target: 0,
            rng: Rng(seed | 1),
        }
    }

    /// Follows the playing file. `fraction` is how much of it has been played (0.0–1.0).
    pub fn set_source(&mut self, path: Option<&Path>, fraction: f64) {
        if path != self.path.as_deref() {
            self.path = path.map(Path::to_path_buf);
            self.file = path.and_then(|p| File::open(p).ok());
            self.len = self
                .file
                .as_ref()
                .and_then(|f| f.metadata().ok())
                .map_or(0, |m| m.len());
            self.buf.clear();
            self.idx = 0;
            self.high_nibble = true;
        }
        self.target = (fraction.clamp(0.0, 1.0) * self.len as f64) as u64;
    }

    fn refill(&mut self) {
        let Some(file) = self.file.as_mut() else {
            self.buf.clear();
            return;
        };
        let next = self.buf_offset + self.buf.len() as u64;
        let start =
            if self.buf.is_empty() || next >= self.len || next.abs_diff(self.target) > MAX_DRIFT {
                self.target.min(self.len.saturating_sub(1))
            } else {
                next
            };
        self.buf.resize(BLOCK, 0);
        let n = file
            .seek(SeekFrom::Start(start))
            .and_then(|_| file.read(&mut self.buf))
            .unwrap_or(0);
        self.buf.truncate(n);
        self.buf_offset = start;
        self.idx = 0;
        self.high_nibble = true;
    }

    /// The next hex digit of the file (random digits when nothing is playing).
    pub fn next_digit(&mut self) -> u8 {
        if self.idx >= self.buf.len() {
            self.refill();
        }
        let Some(&byte) = self.buf.get(self.idx) else {
            return HEX[(self.rng.next_u64() % 16) as usize];
        };
        let nibble = if self.high_nibble {
            byte >> 4
        } else {
            self.idx += 1;
            byte & 0x0F
        };
        self.high_nibble = !self.high_nibble;
        HEX[nibble as usize]
    }

    /// File offset of the byte being shown now, if a file is open.
    pub fn offset(&self) -> Option<u64> {
        self.file
            .as_ref()
            .map(|_| self.buf_offset + self.idx as u64)
    }

    pub fn file_len(&self) -> u64 {
        self.len
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Drop {
    active: bool,
    /// Row of the head (fractional; negative while still above the screen).
    head: f32,
    /// Rows per second.
    speed: f32,
    /// Trail length in rows.
    length: f32,
    last_row: isize,
}

pub struct MatrixRain {
    width: usize,
    height: usize,
    /// Hex digits per cell, column-major so each column reads like a vertical hex dump.
    glyphs: Vec<u8>,
    drops: Vec<Drop>,
    rng: Rng,
    pub feed: ByteFeed,
}

impl Default for MatrixRain {
    fn default() -> Self {
        Self::new(0x9E37_79B9_7F4A_7C15)
    }
}

/// How a lit cell of the rain should look.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Lit {
    /// The leading character of a drop.
    Head,
    /// Part of the trail; 0.0 right behind the head, 1.0 at the end.
    Trail(f32),
}

impl MatrixRain {
    pub fn new(seed: u64) -> Self {
        MatrixRain {
            width: 0,
            height: 0,
            glyphs: Vec::new(),
            drops: Vec::new(),
            rng: Rng(seed | 1),
            feed: ByteFeed::new(seed.rotate_left(17)),
        }
    }

    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// Sets the drawing area. A new size refills the screen with file data.
    pub fn resize(&mut self, width: usize, height: usize) {
        if (width, height) == (self.width, self.height) {
            return;
        }
        self.width = width;
        self.height = height;
        self.drops = vec![Drop::default(); width];
        self.glyphs = (0..width * height)
            .map(|_| self.feed.next_digit())
            .collect();
    }

    /// The hex digit at a cell.
    pub fn glyph(&self, x: usize, y: usize) -> char {
        self.glyphs
            .get(x * self.height + y)
            .map_or(' ', |&g| g as char)
    }

    /// Whether (and how) a cell is lit by a falling drop.
    pub fn lit(&self, x: usize, y: usize) -> Option<Lit> {
        let d = self.drops.get(x).filter(|d| d.active)?;
        let dist = d.head - y as f32;
        if !(0.0..d.length).contains(&dist) {
            return None;
        }
        if dist < 1.0 {
            Some(Lit::Head)
        } else {
            Some(Lit::Trail(dist / d.length))
        }
    }

    pub fn is_active(&self) -> bool {
        self.drops.iter().any(|d| d.active)
    }

    /// Advances the rain by `dt` seconds. `bands` are spectrum levels (0.0–1.0), spread
    /// across the columns from left (bass) to right (treble).
    pub fn update(&mut self, dt: f32, bands: &[f32]) {
        let (w, h) = (self.width, self.height);
        if w == 0 || h == 0 {
            return;
        }
        for x in 0..w {
            let level = if bands.is_empty() {
                0.0
            } else {
                bands[x * bands.len() / w]
            };
            let mut d = self.drops[x];
            if !d.active {
                // Louder bands start drops much more often.
                let rate = level * level * 3.0;
                if level > 0.05 && self.rng.unit() < rate * dt {
                    d = Drop {
                        active: true,
                        head: -self.rng.unit() * 3.0,
                        speed: h as f32 * (0.35 + 1.4 * level) * (0.8 + 0.4 * self.rng.unit()),
                        length: 3.0 + level * h as f32 * 0.7,
                        last_row: -1,
                    };
                    d.last_row = d.head.floor() as isize;
                }
            } else {
                d.head += d.speed * dt;
                // The head "prints" the next bytes of the file as it moves down.
                let row = d.head.floor() as isize;
                for r in (d.last_row + 1)..=row {
                    if (0..h as isize).contains(&r) {
                        self.glyphs[x * h + r as usize] = self.feed.next_digit();
                    }
                }
                d.last_row = row;
                if d.head - d.length > h as f32 {
                    d.active = false;
                }
            }
            self.drops[x] = d;
        }
        // A little flicker inside the trails, like the film.
        let flickers = ((w * h) as f32 * dt * 0.5).ceil() as usize;
        for _ in 0..flickers {
            let x = (self.rng.next_u64() % w as u64) as usize;
            let y = (self.rng.next_u64() % h as u64) as usize;
            if self.lit(x, y).is_some() {
                self.glyphs[x * h + y] = self.feed.next_digit();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    #[test]
    fn feed_yields_the_files_bytes_as_hex() {
        let path = fixture("tagged.mp3");
        let bytes = std::fs::read(&path).unwrap();
        let mut feed = ByteFeed::new(1);
        feed.set_source(Some(&path), 0.0);
        let digits: String = (0..16).map(|_| feed.next_digit() as char).collect();
        let expected: String = bytes[..8].iter().map(|b| format!("{b:02X}")).collect();
        assert_eq!(digits, expected);
        assert!(
            digits.starts_with("494433"),
            "ID3 tag header expected: {digits}"
        );
        assert_eq!(feed.offset(), Some(8));
    }

    #[test]
    fn feed_follows_the_playback_position() {
        let path = fixture("sine_44k_stereo.wav");
        let bytes = std::fs::read(&path).unwrap();
        let mut feed = ByteFeed::new(1);
        feed.set_source(Some(&path), 0.5);
        let at = bytes.len() / 2;
        let digits: String = (0..8).map(|_| feed.next_digit() as char).collect();
        let expected: String = bytes[at..at + 4]
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect();
        assert_eq!(digits, expected);
    }

    #[test]
    fn feed_without_a_file_still_produces_hex() {
        let mut feed = ByteFeed::new(7);
        feed.set_source(None, 0.0);
        assert!((0..100).all(|_| feed.next_digit().is_ascii_hexdigit()));
        assert_eq!(feed.offset(), None);
    }

    #[test]
    fn loud_bands_rain_and_silence_drains() {
        let mut rain = MatrixRain::new(42);
        rain.resize(40, 20);
        for _ in 0..120 {
            rain.update(1.0 / 60.0, &[0.9; 16]);
        }
        assert!(rain.is_active());
        let lit = (0..40)
            .flat_map(|x| (0..20).map(move |y| (x, y)))
            .filter(|&(x, y)| rain.lit(x, y).is_some())
            .count();
        assert!(lit > 40, "only {lit} lit cells");
        for _ in 0..600 {
            rain.update(1.0 / 60.0, &[0.0; 16]);
        }
        assert!(!rain.is_active());
    }

    #[test]
    fn quiet_columns_stay_dark() {
        let mut rain = MatrixRain::new(3);
        rain.resize(32, 10);
        // Bass loud, treble silent.
        let mut bands = [0.0; 16];
        bands[..8].fill(1.0);
        for _ in 0..240 {
            rain.update(1.0 / 60.0, &bands);
        }
        let lit_right = (16..32)
            .flat_map(|x| (0..10).map(move |y| (x, y)))
            .filter(|&(x, y)| rain.lit(x, y).is_some())
            .count();
        assert_eq!(lit_right, 0);
        assert!((0..16).any(|x| (0..10).any(|y| rain.lit(x, y).is_some())));
    }

    #[test]
    fn every_glyph_is_a_hex_digit() {
        let mut rain = MatrixRain::new(5);
        rain.feed
            .set_source(Some(&fixture("sine_44k_stereo.flac")), 0.3);
        rain.resize(25, 12);
        for _ in 0..60 {
            rain.update(1.0 / 60.0, &[0.7; 16]);
        }
        assert!((0..25).all(|x| (0..12).all(|y| rain.glyph(x, y).is_ascii_hexdigit())));
    }
}
