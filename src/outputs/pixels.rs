//! Drawing into RGB pixel buffers: rectangles and text in an 8x8 pixel font, and PNG files.
//! Used for the cover cards and the TV picture.

use font8x8::UnicodeFonts;

/// An RGB picture, three bytes per pixel, rows top to bottom.
pub struct Canvas {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

/// The 8x8 picture of `c` (rows top to bottom, the lowest bit leftmost). Characters outside
/// the font are drawn as "?".
pub fn glyph(c: char) -> [u8; 8] {
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
    pub fn new(width: usize, height: usize, background: [u8; 3]) -> Self {
        Canvas {
            width,
            height,
            pixels: background.repeat(width * height),
        }
    }

    pub fn fill(&mut self, color: [u8; 3]) {
        for p in self.pixels.as_chunks_mut::<3>().0 {
            *p = color;
        }
    }

    pub fn set(&mut self, x: usize, y: usize, color: [u8; 3]) {
        if x < self.width && y < self.height {
            let i = (y * self.width + x) * 3;
            self.pixels[i..i + 3].copy_from_slice(&color);
        }
    }

    pub fn get(&self, x: usize, y: usize) -> [u8; 3] {
        let i = (y.min(self.height - 1) * self.width + x.min(self.width - 1)) * 3;
        [self.pixels[i], self.pixels[i + 1], self.pixels[i + 2]]
    }

    pub fn rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: [u8; 3]) {
        let (x1, y1) = ((x + w).min(self.width), (y + h).min(self.height));
        for row in y.min(self.height)..y1 {
            for col in x.min(self.width)..x1 {
                let i = (row * self.width + col) * 3;
                self.pixels[i..i + 3].copy_from_slice(&color);
            }
        }
    }

    /// Mixes `color` over the area, `alpha` (0–1) of it.
    pub fn blend_rect(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        color: [u8; 3],
        alpha: f32,
    ) {
        let a = alpha.clamp(0.0, 1.0);
        let (x1, y1) = ((x + w).min(self.width), (y + h).min(self.height));
        for row in y.min(self.height)..y1 {
            for col in x.min(self.width)..x1 {
                let i = (row * self.width + col) * 3;
                for (old, new) in self.pixels[i..i + 3].iter_mut().zip(color) {
                    let o = f32::from(*old);
                    *old = (o + (f32::from(new) - o) * a).round() as u8;
                }
            }
        }
    }

    /// Draws `text` with its top left at (`x`, `y`), each font pixel `scale` pixels square.
    pub fn text(&mut self, text: &str, x: usize, y: usize, scale: usize, color: [u8; 3]) {
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

    /// The picture as a PNG file.
    pub fn png(&self) -> Vec<u8> {
        let mut raw = Vec::with_capacity((self.width * 3 + 1) * self.height);
        for row in self.pixels.chunks(self.width * 3) {
            // Filter type 0: the row as it is.
            raw.push(0);
            raw.extend_from_slice(row);
        }
        let mut header = Vec::with_capacity(13);
        header.extend_from_slice(&(self.width as u32).to_be_bytes());
        header.extend_from_slice(&(self.height as u32).to_be_bytes());
        // 8 bits per channel, RGB, deflate, standard filters, no interlace.
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
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32fast::hash(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// Splits `text` into lines of at most `width` characters, at spaces where it can.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
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
pub fn fit(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        text.to_string()
    } else {
        let mut cut: String = text.chars().take(width.saturating_sub(2)).collect();
        cut.push_str("..");
        cut
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_blends_and_clips() {
        let mut c = Canvas::new(10, 6, [0, 0, 0]);
        c.rect(8, 4, 5, 5, [255, 0, 0]);
        assert_eq!(c.get(9, 5), [255, 0, 0]);
        assert_eq!(c.get(7, 5), [0, 0, 0]);
        c.blend_rect(0, 0, 2, 2, [200, 100, 50], 0.5);
        assert_eq!(c.get(1, 1), [100, 50, 25]);
        c.text("Hi", 0, 0, 1, [1, 2, 3]);
        assert!(c.pixels.chunks(3).any(|p| p == [1, 2, 3]));
        assert_eq!(&c.png()[1..4], b"PNG");
    }

    #[test]
    fn wraps_and_fits() {
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
        assert_eq!(glyph('ä'), font8x8::LATIN_FONTS.get('ä').unwrap());
        assert_eq!(glyph('漢'), glyph('?'));
    }
}
