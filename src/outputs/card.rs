//! Cover cards: a picture of the song in the player's colour theme, which TVs (and the Sonos
//! app) show as its cover art. It looks like the player: a "Now Playing" frame, the title,
//! artist and album in a pixel font, and spectrum bars in the theme's gradient, shaped
//! differently for every song.

use font8x8::UnicodeFonts;

/// Width and height of a card, in pixels.
pub const SIZE: usize = 720;
const MARGIN: usize = 36;
const FRAME: usize = 6;
/// Where text and bars start, and how wide they may be.
const LEFT: usize = 78;
const INNER: usize = SIZE - 2 * LEFT;

/// The colours of a card, from the theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CardColors {
    pub background: [u8; 3],
    pub frame: [u8; 3],
    pub title: [u8; 3],
    pub text: [u8; 3],
    pub dim: [u8; 3],
    /// The spectrum gradient: low part of the bars, middle, and tops.
    pub low: [u8; 3],
    pub mid: [u8; 3],
    pub hot: [u8; 3],
}

/// What a card shows.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Card {
    pub title: String,
    pub artist: Option<String>,
    /// "Album (year)".
    pub album: Option<String>,
    pub colors: CardColors,
    /// Shapes the bars, so every song gets its own.
    pub seed: u64,
}

struct Canvas {
    pixels: Vec<u8>,
}

/// The 8x8 picture of `c` (rows top to bottom, the lowest bit leftmost).
fn glyph(c: char) -> [u8; 8] {
    font8x8::BASIC_FONTS
        .get(c)
        .or_else(|| font8x8::LATIN_FONTS.get(c))
        .or_else(|| font8x8::GREEK_FONTS.get(c))
        .or_else(|| font8x8::BOX_FONTS.get(c))
        .or_else(|| font8x8::BLOCK_FONTS.get(c))
        .or_else(|| font8x8::HIRAGANA_FONTS.get(c))
        .or_else(|| font8x8::BASIC_FONTS.get('?'))
        .unwrap_or([0; 8])
}

impl Canvas {
    fn new(background: [u8; 3]) -> Self {
        Canvas {
            pixels: background.repeat(SIZE * SIZE),
        }
    }

    fn rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: [u8; 3]) {
        for row in y.min(SIZE)..(y + h).min(SIZE) {
            for col in x.min(SIZE)..(x + w).min(SIZE) {
                let i = (row * SIZE + col) * 3;
                self.pixels[i..i + 3].copy_from_slice(&color);
            }
        }
    }

    /// Draws `text` with its top left at (`x`, `y`), each font pixel `scale` pixels square.
    fn text(&mut self, text: &str, x: usize, y: usize, scale: usize, color: [u8; 3]) {
        for (n, c) in text.chars().enumerate() {
            let left = x + n * 8 * scale;
            for (row, bits) in glyph(c).iter().enumerate() {
                for col in 0..8 {
                    if bits & (1 << col) != 0 {
                        self.rect(left + col * scale, y + row * scale, scale, scale, color);
                    }
                }
            }
        }
    }
}

/// Splits `text` into lines of at most `width` characters, at spaces where it can.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let mut word: Vec<char> = word.chars().collect();
        // Words longer than a line are cut.
        while word.len() > width {
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            lines.push(word.drain(..width).collect());
        }
        let word: String = word.into_iter().collect();
        let len = line.chars().count();
        if len > 0 && len + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// `text` cut to `width` characters, ending in ".." when cut.
fn fit(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        text.to_string()
    } else {
        let mut cut: String = text.chars().take(width.saturating_sub(2)).collect();
        cut.push_str("..");
        cut
    }
}

/// A pseudo-random number in 0..1 from `seed` and `n` (the same inputs, the same number).
fn noise(seed: u64, n: u64) -> f32 {
    let mut x = seed ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x ^= x >> 33;
    x = x.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    x ^= x >> 33;
    (x >> 40) as f32 / (1u64 << 24) as f32
}

impl Card {
    fn draw(&self) -> Canvas {
        let c = &self.colors;
        let mut canvas = Canvas::new(c.background);
        // The frame, with its title in the top edge, like the player's panes.
        let span = SIZE - 2 * MARGIN;
        canvas.rect(MARGIN, MARGIN, span, FRAME, c.frame);
        canvas.rect(MARGIN, SIZE - MARGIN - FRAME, span, FRAME, c.frame);
        canvas.rect(MARGIN, MARGIN, FRAME, span, c.frame);
        canvas.rect(SIZE - MARGIN - FRAME, MARGIN, FRAME, span, c.frame);
        let label = " Now Playing ";
        let label_width = label.len() * 24;
        let label_x = (SIZE - label_width) / 2;
        canvas.rect(label_x, MARGIN - 9, label_width, 24, c.background);
        canvas.text(label, label_x, MARGIN - 9, 3, c.title);

        // The title as large as fits in three lines.
        let mut y = 108;
        let (scale, lines) = [6, 5, 4]
            .into_iter()
            .map(|s| (s, wrap(&self.title, INNER / (8 * s))))
            .find(|(_, lines)| lines.len() <= 3)
            .unwrap_or_else(|| {
                let mut lines = wrap(&self.title, INNER / 24);
                lines.truncate(4);
                (3, lines)
            });
        for line in &lines {
            canvas.text(line, LEFT, y, scale, c.title);
            y += 10 * scale;
        }
        y += 14;
        if let Some(artist) = &self.artist {
            canvas.text(&fit(artist, INNER / 32), LEFT, y, 4, c.text);
            y += 48;
        }
        if let Some(album) = &self.album {
            // Long album names get smaller letters and a second line.
            if album.chars().count() <= INNER / 24 {
                canvas.text(album, LEFT, y, 3, c.dim);
            } else {
                let width = INNER / 16;
                let mut lines = wrap(album, width);
                if lines.len() > 2 {
                    let rest = lines[1..].join(" ");
                    lines.truncate(1);
                    lines.push(fit(&rest, width));
                }
                for line in &lines {
                    canvas.text(line, LEFT, y, 2, c.dim);
                    y += 22;
                }
            }
        }

        // Spectrum bars of stacked blocks, louder in the bass, with peak markers.
        let bars = 24;
        let pitch = INNER / bars;
        let block = 12;
        let floor = SIZE - MARGIN - FRAME - 64;
        let most = 14;
        for i in 0..bars {
            let slope = 1.0 - 0.55 * i as f32 / bars as f32;
            let level = slope * (0.35 + 0.65 * noise(self.seed, i as u64));
            let blocks = ((level * most as f32).round() as usize).clamp(1, most);
            let x = LEFT + i * pitch;
            for b in 0..blocks {
                let height = b as f32 / most as f32;
                let color = if height < 0.4 {
                    c.low
                } else if height < 0.75 {
                    c.mid
                } else {
                    c.hot
                };
                canvas.rect(x, floor - (b + 1) * block, pitch - 6, block - 3, color);
            }
            let peak = (blocks + 1 + (noise(self.seed, 100 + i as u64) * 2.0) as usize).min(most);
            if peak > blocks {
                canvas.rect(x, floor - (peak + 1) * block, pitch - 6, 4, c.hot);
            }
        }
        canvas.text("mp3blaster-rs", LEFT, floor + 22, 2, c.dim);
        canvas
    }

    /// The card as a PNG file.
    pub fn png(&self) -> Vec<u8> {
        encode_png(&self.draw().pixels)
    }
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32fast::hash(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// A PNG of `SIZE` x `SIZE` RGB pixels.
fn encode_png(rgb: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity((SIZE * 3 + 1) * SIZE);
    for row in rgb.chunks(SIZE * 3) {
        // Filter type 0: the row as it is.
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&(SIZE as u32).to_be_bytes());
    header.extend_from_slice(&(SIZE as u32).to_be_bytes());
    // 8 bits per channel, RGB, deflate, no filter method extras, no interlace.
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &header);
    chunk(
        &mut out,
        b"IDAT",
        &miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6),
    );
    chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const COLORS: CardColors = CardColors {
        background: [0x28, 0x2a, 0x36],
        frame: [0x62, 0x72, 0xa4],
        title: [0xbd, 0x93, 0xf9],
        text: [0xf8, 0xf8, 0xf2],
        dim: [0x60, 0x60, 0x60],
        low: [0x50, 0xfa, 0x7b],
        mid: [0xff, 0x79, 0xc6],
        hot: [0xff, 0x55, 0x55],
    };

    fn card(title: &str) -> Card {
        Card {
            title: title.into(),
            artist: Some("Miles Davis".into()),
            album: Some("Kind of Blue (1959)".into()),
            colors: COLORS,
            seed: 7,
        }
    }

    /// The pixels of a PNG made by `encode_png`.
    fn decode(png: &[u8]) -> Vec<u8> {
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(
            u32::from_be_bytes(png[16..20].try_into().unwrap()),
            SIZE as u32
        );
        let idat_len = u32::from_be_bytes(png[33..37].try_into().unwrap()) as usize;
        assert_eq!(&png[37..41], b"IDAT");
        let crc = crc32fast::hash(&png[37..41 + idat_len]);
        let stored = u32::from_be_bytes(png[41 + idat_len..45 + idat_len].try_into().unwrap());
        assert_eq!(crc, stored);
        let raw = miniz_oxide::inflate::decompress_to_vec_zlib(&png[41..41 + idat_len]).unwrap();
        assert_eq!(raw.len(), (SIZE * 3 + 1) * SIZE);
        raw.chunks(SIZE * 3 + 1)
            .flat_map(|row| row[1..].to_vec())
            .collect()
    }

    fn pixel(pixels: &[u8], x: usize, y: usize) -> [u8; 3] {
        let i = (y * SIZE + x) * 3;
        [pixels[i], pixels[i + 1], pixels[i + 2]]
    }

    #[test]
    fn draws_the_song_in_the_theme() {
        let png = card("So What").png();
        assert!(png.len() < 60_000, "{} bytes", png.len());
        let pixels = decode(&png);
        assert_eq!(pixel(&pixels, 0, 0), COLORS.background);
        assert_eq!(pixel(&pixels, MARGIN + 2, SIZE / 2), COLORS.frame);
        let count = |color: [u8; 3]| pixels.chunks(3).filter(|p| **p == color).count();
        for (what, color) in [
            ("title", COLORS.title),
            ("artist", COLORS.text),
            ("album", COLORS.dim),
            ("low bars", COLORS.low),
            ("bar tops", COLORS.hot),
        ] {
            assert!(count(color) > 500, "{what}");
        }
    }

    #[test]
    fn every_song_gets_its_own_bars() {
        let a = card("So What");
        let mut b = a.clone();
        b.seed = 8;
        assert_eq!(
            a.png(),
            card("So What").png(),
            "the same song, the same card"
        );
        assert_ne!(a.png(), b.png());
    }

    /// Writes sample cards in a few themes to `$CARD_OUT`, to look at:
    /// `CARD_OUT=<folder> cargo test --lib card_samples -- --ignored`
    #[test]
    #[ignore]
    fn card_samples() {
        use crate::ui::theme::Theme;
        let Ok(dir) = std::env::var("CARD_OUT") else {
            return;
        };
        for theme in [
            Theme::dracula(),
            Theme::nord(),
            Theme::gruvbox(),
            Theme::classic_blue(),
            Theme::monochrome(),
        ] {
            let mut c = card("The Great Gig in the Sky");
            c.artist = Some("Pink Floyd".into());
            c.album = Some("The Dark Side of the Moon (1973)".into());
            c.colors = theme.card_colors();
            let path = std::path::Path::new(&dir).join(format!("{}.png", theme.name));
            std::fs::write(path, c.png()).unwrap();
        }
        let mut c = card("Ellan Jytäboksi");
        c.artist = Some("Ääniä & Öitä".into());
        c.album = None;
        c.colors = Theme::tokyo_night().card_colors();
        std::fs::write(std::path::Path::new(&dir).join("finnish.png"), c.png()).unwrap();
    }

    #[test]
    fn long_titles_wrap_and_shrink() {
        assert_eq!(wrap("So What", 12), ["So What"]);
        assert_eq!(
            wrap("The Great Gig in the Sky", 12),
            ["The Great", "Gig in the", "Sky"]
        );
        assert_eq!(
            wrap("Supercalifragilistic", 8),
            ["Supercal", "ifragili", "stic"]
        );
        assert_eq!(fit("Kind of Blue", 6), "Kind..");
        // Even an absurd title stays inside the card.
        let long = "word ".repeat(80);
        let pixels = decode(&card(&long).png());
        assert_eq!(pixel(&pixels, SIZE - 1, SIZE - 1), COLORS.background);
        // Letters outside the font show as "?" instead of nothing.
        assert_eq!(glyph('ä'), font8x8::LATIN_FONTS.get('ä').unwrap());
        assert_eq!(glyph('漢'), glyph('?'));
    }
}
