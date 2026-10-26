//! Drawing helpers for the pixel scenes of the jukebox and the amazing-horse views:
//! colours, rectangles in cells and in pixels, and a pen that fills shapes clipped to a
//! rectangle.

use crate::outputs::pixels::Canvas;

pub type Rgb = [u8; 3];

/// How far (0–1, eased in and out) `step` has got at time `t`.
pub fn phase(t: f32, step: (f32, f32)) -> f32 {
    let k = ((t - step.0) / (step.1 - step.0)).clamp(0.0, 1.0);
    k * k * (3.0 - 2.0 * k)
}

pub fn lerp(a: f32, b: f32, k: f32) -> f32 {
    a + (b - a) * k
}

pub fn mix(a: Rgb, b: Rgb, k: f32) -> Rgb {
    let k = k.clamp(0.0, 1.0);
    [0, 1, 2].map(|i| lerp(f32::from(a[i]), f32::from(b[i]), k).round() as u8)
}

pub fn shade(c: Rgb, k: f32) -> Rgb {
    c.map(|v| (f32::from(v) * k).round().clamp(0.0, 255.0) as u8)
}

/// A colour from hue, saturation and value (all 0–1).
pub fn hsv(h: f32, s: f32, v: f32) -> Rgb {
    let h = h.rem_euclid(1.0) * 6.0;
    let f = h.fract();
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    let (r, g, b) = match h as u32 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    [r, g, b].map(|c| (c * 255.0).round() as u8)
}

/// A number in 0..1 from `n` (the same `n`, the same number).
pub fn noise(n: u64) -> f32 {
    let mut x = n.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x ^= x >> 31;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 29;
    (x >> 40) as f32 / (1u64 << 24) as f32
}

/// A rectangle, in terminal cells or in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Area {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Area {
    pub fn new(x: usize, y: usize, w: usize, h: usize) -> Area {
        Area { x, y, w, h }
    }

    /// The pixels of a rectangle of cells.
    pub fn px(self, rows_per_cell: usize) -> Area {
        Area::new(
            self.x,
            self.y * rows_per_cell,
            self.w,
            self.h * rows_per_cell,
        )
    }

    /// The rows inside a frame drawn by `frame`, for text.
    pub fn inside(self) -> Area {
        Area::new(
            self.x + 1,
            self.y + 1,
            self.w.saturating_sub(2),
            self.h.saturating_sub(2),
        )
    }
}

/// Draws a frame on the edge of `cells` and returns the pixels inside it. With two pixel
/// rows per cell the frame takes only the outer half of the first and last rows, so the
/// rows in between (`Area::inside`) are free for text. `colors` are the top and left edge,
/// and the bottom and right edge.
pub fn frame(canvas: &mut Canvas, cells: Area, ppc: usize, colors: [Rgb; 3]) -> Area {
    let [top_color, left_color, low_color] = colors;
    let px = cells.px(ppc);
    let top = (px.y + ppc).saturating_sub(1);
    let bottom = (px.y + px.h).saturating_sub(ppc);
    let mut pen = Pen::new(canvas);
    pen.fill(Area::new(px.x, top, px.w, 1), top_color);
    pen.fill(Area::new(px.x, bottom, px.w, 1), low_color);
    pen.fill(Area::new(px.x, top, 1, bottom - top + 1), left_color);
    pen.fill(
        Area::new(px.x + px.w.saturating_sub(1), top, 1, bottom - top + 1),
        low_color,
    );
    Area::new(
        px.x + 1,
        top + 1,
        px.w.saturating_sub(2),
        bottom.saturating_sub(top + 1),
    )
}

/// Draws into a canvas, clipped to a rectangle.
pub struct Pen<'a> {
    canvas: &'a mut Canvas,
    pub clip: Area,
}

impl<'a> Pen<'a> {
    pub fn new(canvas: &'a mut Canvas) -> Self {
        let clip = Area::new(0, 0, canvas.width, canvas.height);
        Pen { canvas, clip }
    }

    fn inside(&self, x: i64, y: i64) -> bool {
        let c = self.clip;
        x >= c.x as i64 && y >= c.y as i64 && x < (c.x + c.w) as i64 && y < (c.y + c.h) as i64
    }

    pub fn put(&mut self, x: i64, y: i64, color: Rgb) {
        if self.inside(x, y) {
            self.canvas.set(x as usize, y as usize, color);
        }
    }

    pub fn blend(&mut self, x: f32, y: f32, color: Rgb, alpha: f32) {
        let (x, y) = (x.floor() as i64, y.floor() as i64);
        if self.inside(x, y) {
            let under = self.canvas.get(x as usize, y as usize);
            self.canvas
                .set(x as usize, y as usize, mix(under, color, alpha));
        }
    }

    pub fn fill(&mut self, area: Area, color: Rgb) {
        for y in area.y..area.y + area.h {
            for x in area.x..area.x + area.w {
                self.put(x as i64, y as i64, color);
            }
        }
    }

    pub fn fill_f(&mut self, x: f32, y: f32, w: f32, h: f32, color: Rgb) {
        let (x0, y0) = (x.round() as i64, y.round() as i64);
        let (x1, y1) = (
            (x + w).round() as i64,
            (y + h).round().max(y0 as f32 + 1.0) as i64,
        );
        for y in y0..y1 {
            for x in x0..x1 {
                self.put(x, y, color);
            }
        }
    }

    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, color: Rgb) {
        let steps = ((x1 - x0).abs().max((y1 - y0).abs()) * 2.0).ceil().max(1.0) as usize;
        for i in 0..=steps {
            let k = i as f32 / steps as f32;
            self.put(
                lerp(x0, x1, k).floor() as i64,
                lerp(y0, y1, k).floor() as i64,
                color,
            );
        }
    }

    /// A thick stroke from (`x0`, `y0`) to (`x1`, `y1`), `r0` wide at the start (as a
    /// radius) narrowing or widening to `r1` at the end, with round ends.
    #[allow(clippy::too_many_arguments)]
    pub fn stroke(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, r0: f32, r1: f32, color: Rgb) {
        let len = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
        let steps = (len * 2.0).ceil().max(1.0) as usize;
        for i in 0..=steps {
            let k = i as f32 / steps as f32;
            let r = lerp(r0, r1, k).max(0.5);
            self.ellipse(lerp(x0, x1, k), lerp(y0, y1, k), r, r, color);
        }
    }

    /// Fills an ellipse; `paint` gets each pixel's distance from the centre (0–1 at the
    /// edge) and its angle, and returns its colour and opacity (or `None` to leave it).
    pub fn ellipse_with(
        &mut self,
        cx: f32,
        cy: f32,
        rx: f32,
        ry: f32,
        mut paint: impl FnMut(f32, f32) -> Option<(Rgb, f32)>,
    ) {
        if rx <= 0.0 || ry <= 0.0 {
            return;
        }
        let (rx, ry) = (rx.max(0.5), ry.max(0.5));
        for y in (cy - ry).floor() as i64..=(cy + ry).ceil() as i64 {
            for x in (cx - rx).floor() as i64..=(cx + rx).ceil() as i64 {
                let dx = (x as f32 + 0.5 - cx) / rx;
                let dy = (y as f32 + 0.5 - cy) / ry;
                let r = (dx * dx + dy * dy).sqrt();
                match paint(r, dy.atan2(dx)) {
                    _ if r > 1.0 => {}
                    Some((color, a)) if a >= 1.0 => self.put(x, y, color),
                    Some((color, a)) => self.blend(x as f32, y as f32, color, a),
                    None => {}
                }
            }
        }
    }

    pub fn ellipse(&mut self, cx: f32, cy: f32, rx: f32, ry: f32, color: Rgb) {
        self.ellipse_with(cx, cy, rx, ry, |_, _| Some((color, 1.0)));
    }

    /// An ellipse turned by `angle` (radians, clockwise on screen); `paint` gets each
    /// pixel's place in the ellipse's own axes (-1..1 along and across) and returns its
    /// colour, or `None` to leave it.
    #[allow(clippy::too_many_arguments)]
    pub fn ellipse_turned(
        &mut self,
        cx: f32,
        cy: f32,
        rx: f32,
        ry: f32,
        angle: f32,
        mut paint: impl FnMut(f32, f32) -> Option<Rgb>,
    ) {
        let (rx, ry) = (rx.max(0.5), ry.max(0.5));
        let reach = rx.max(ry);
        let (sin, cos) = angle.sin_cos();
        for y in (cy - reach).floor() as i64..=(cy + reach).ceil() as i64 {
            for x in (cx - reach).floor() as i64..=(cx + reach).ceil() as i64 {
                let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                let (u, v) = ((dx * cos + dy * sin) / rx, (-dx * sin + dy * cos) / ry);
                if u * u + v * v <= 1.0
                    && let Some(color) = paint(u, v)
                {
                    self.put(x, y, color);
                }
            }
        }
    }

    /// A filled triangle.
    pub fn triangle(&mut self, a: (f32, f32), b: (f32, f32), c: (f32, f32), color: Rgb) {
        let (x0, x1) = (a.0.min(b.0).min(c.0).floor(), a.0.max(b.0).max(c.0).ceil());
        let (y0, y1) = (a.1.min(b.1).min(c.1).floor(), a.1.max(b.1).max(c.1).ceil());
        let side = |p: (f32, f32), q: (f32, f32), x: f32, y: f32| {
            (q.0 - p.0) * (y - p.1) - (q.1 - p.1) * (x - p.0)
        };
        for y in y0 as i64..=y1 as i64 {
            for x in x0 as i64..=x1 as i64 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let (d0, d1, d2) = (side(a, b, px, py), side(b, c, px, py), side(c, a, px, py));
                let neg = d0 < 0.0 || d1 < 0.0 || d2 < 0.0;
                let pos = d0 > 0.0 || d1 > 0.0 || d2 > 0.0;
                if !(neg && pos) {
                    self.put(x, y, color);
                }
            }
        }
    }

    /// A soft light: `color` mixed in, strongest in the middle.
    pub fn glow(&mut self, cx: f32, cy: f32, rx: f32, ry: f32, color: Rgb, alpha: f32) {
        self.ellipse_with(cx, cy, rx, ry, |r, _| {
            Some((color, alpha * (1.0 - r).powi(2)))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_stay_inside_the_clip() {
        let mut canvas = Canvas::new(20, 20, [0; 3]);
        let mut pen = Pen::new(&mut canvas);
        pen.clip = Area::new(5, 5, 10, 10);
        pen.ellipse(5.0, 5.0, 8.0, 8.0, [255, 0, 0]);
        pen.stroke(0.0, 10.0, 19.0, 10.0, 2.0, 1.0, [0, 255, 0]);
        pen.triangle((5.0, 5.0), (14.0, 5.0), (5.0, 14.0), [0, 0, 255]);
        assert_eq!(canvas.get(4, 10), [0; 3]);
        assert_eq!(canvas.get(6, 6), [0, 0, 255]);
        assert_eq!(canvas.get(13, 10), [0, 255, 0]);
        assert_eq!(canvas.get(16, 10), [0; 3]);
    }

    #[test]
    fn a_turned_ellipse_lies_along_its_angle() {
        let mut canvas = Canvas::new(21, 21, [0; 3]);
        let mut pen = Pen::new(&mut canvas);
        // Long and thin, turned a quarter: it stands upright.
        pen.ellipse_turned(10.5, 10.5, 9.0, 2.0, std::f32::consts::FRAC_PI_2, |_, _| {
            Some([9; 3])
        });
        assert_eq!(canvas.get(10, 3), [9; 3]);
        assert_eq!(canvas.get(3, 10), [0; 3]);
    }

    #[test]
    fn colours() {
        assert_eq!(hsv(0.0, 1.0, 1.0), [255, 0, 0]);
        assert_eq!(hsv(1.0 / 3.0, 1.0, 1.0), [0, 255, 0]);
        assert_eq!(mix([0; 3], [200; 3], 0.5), [100; 3]);
        assert_eq!(shade([100, 50, 10], 0.5), [50, 25, 5]);
        assert_eq!(phase(0.5, (0.0, 1.0)), 0.5);
        assert_eq!(phase(2.0, (0.0, 1.0)), 1.0);
    }
}
