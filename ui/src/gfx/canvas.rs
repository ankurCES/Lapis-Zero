//! 320x170 RGB888 framebuffer with clipped primitives and 4x4 Bayer dithering.

/// Canvas width in pixels (ST7789v3 landscape).
pub const WIDTH: i32 = 320;
/// Canvas height in pixels (ST7789v3 landscape).
pub const HEIGHT: i32 = 170;

/// Deep matte black, `#0B0B0D`.
pub const INK: u32 = 0x0B0B0D;
/// Warm e-paper off-white, `#F2F2EC`.
pub const PAPER: u32 = 0xF2F2EC;
/// Active agent execution / tool call pip, `#2D6BFF`.
pub const ACCENT_BLUE: u32 = 0x2D6BFF;
/// Approval required / warning / offline, `#FF6B2B`.
pub const ACCENT_AMBER: u32 = 0xFF6B2B;

/// 4x4 Bayer ordered-dither index matrix, indexed `[y & 3][x & 3]`.
const BAYER4: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// Bayer threshold at `(x, y)`, scaled x17 into `0..=255`.
pub fn bayer_threshold(x: i32, y: i32) -> u8 {
    BAYER4[(y & 3) as usize][(x & 3) as usize] * 17
}

/// A 320x170 RGB888 framebuffer. Colors are `0xRRGGBB`.
///
/// Every drawing call clips silently to the canvas and never allocates.
pub struct Canvas {
    buf: Vec<u8>,
}

impl Default for Canvas {
    fn default() -> Self {
        Self::new()
    }
}

impl Canvas {
    /// Creates a canvas filled with [`PAPER`].
    pub fn new() -> Self {
        let mut c = Self {
            buf: vec![0; (WIDTH * HEIGHT * 3) as usize],
        };
        c.clear(PAPER);
        c
    }

    /// Raw RGB888 bytes, row-major, `WIDTH * 3` bytes per row.
    pub fn pixels(&self) -> &[u8] {
        &self.buf
    }

    /// Returns the color at `(x, y)`, or `None` if out of bounds.
    pub fn get_pixel(&self, x: i32, y: i32) -> Option<u32> {
        if !(0..WIDTH).contains(&x) || !(0..HEIGHT).contains(&y) {
            return None;
        }
        let i = ((y * WIDTH + x) * 3) as usize;
        let p = &self.buf[i..i + 3];
        Some((p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32)
    }

    /// Fills the whole canvas with `color`.
    pub fn clear(&mut self, color: u32) {
        let rgb = split(color);
        for px in self.buf.chunks_exact_mut(3) {
            px.copy_from_slice(&rgb);
        }
    }

    /// Sets one pixel; out-of-bounds coordinates are ignored.
    pub fn set_pixel(&mut self, x: i32, y: i32, color: u32) {
        if !(0..WIDTH).contains(&x) || !(0..HEIGHT).contains(&y) {
            return;
        }
        let i = ((y * WIDTH + x) * 3) as usize;
        self.buf[i..i + 3].copy_from_slice(&split(color));
    }

    /// Fills the rectangle `(x, y, w, h)`, clipped to the canvas.
    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: u32) {
        let Some((x0, y0, x1, y1)) = clip(x, y, w, h) else {
            return;
        };
        let rgb = split(color);
        for yy in y0..y1 {
            let row = ((yy * WIDTH + x0) * 3) as usize;
            let end = ((yy * WIDTH + x1) * 3) as usize;
            for px in self.buf[row..end].chunks_exact_mut(3) {
                px.copy_from_slice(&rgb);
            }
        }
    }

    /// Draws a 1px rectangle outline `(x, y, w, h)`.
    pub fn draw_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: u32) {
        if w <= 0 || h <= 0 {
            return;
        }
        self.hline(x, y, w, color);
        self.hline(x, y + h - 1, w, color);
        self.vline(x, y, h, color);
        self.vline(x + w - 1, y, h, color);
    }

    /// Draws a horizontal line of `w` pixels starting at `(x, y)`.
    pub fn hline(&mut self, x: i32, y: i32, w: i32, color: u32) {
        self.fill_rect(x, y, w, 1, color);
    }

    /// Draws a vertical line of `h` pixels starting at `(x, y)`.
    pub fn vline(&mut self, x: i32, y: i32, h: i32, color: u32) {
        self.fill_rect(x, y, 1, h, color);
    }

    /// Fills `(x, y, w, h)` with a 4x4 Bayer ordered dither between `ink` and `paper`.
    ///
    /// `level` is ink coverage: `0` is solid paper, `255` solid ink, `128` a 50% checker.
    /// The pattern is anchored to absolute canvas coordinates so adjacent fills tile.
    #[allow(clippy::too_many_arguments)] // signature fixed by the Phase 0 spec
    pub fn fill_rect_dithered(
        &mut self,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        level: u8,
        ink: u32,
        paper: u32,
    ) {
        let Some((x0, y0, x1, y1)) = clip(x, y, w, h) else {
            return;
        };
        let (ink, paper) = (split(ink), split(paper));
        for yy in y0..y1 {
            for xx in x0..x1 {
                let on = level == 255 || level > bayer_threshold(xx, yy);
                let i = ((yy * WIDTH + xx) * 3) as usize;
                self.buf[i..i + 3].copy_from_slice(if on { &ink } else { &paper });
            }
        }
    }
}

/// Splits `0xRRGGBB` into `[r, g, b]`.
fn split(color: u32) -> [u8; 3] {
    [(color >> 16) as u8, (color >> 8) as u8, color as u8]
}

/// Clips a rect to the canvas, returning half-open `(x0, y0, x1, y1)` or `None` if empty.
fn clip(x: i32, y: i32, w: i32, h: i32) -> Option<(i32, i32, i32, i32)> {
    let x0 = x.max(0);
    let y0 = y.max(0);
    let x1 = x.saturating_add(w).min(WIDTH);
    let y1 = y.saturating_add(h).min(HEIGHT);
    (x0 < x1 && y0 < y1).then_some((x0, y0, x1, y1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_pixel_bounds() {
        let mut c = Canvas::new();
        c.set_pixel(0, 0, INK);
        c.set_pixel(319, 169, INK);
        assert_eq!(c.get_pixel(0, 0), Some(INK));
        assert_eq!(c.get_pixel(319, 169), Some(INK));
        let before = c.pixels().to_vec();
        c.set_pixel(320, 170, INK);
        c.set_pixel(320, 0, INK);
        c.set_pixel(0, 170, INK);
        c.set_pixel(-1, -1, INK);
        assert_eq!(c.pixels(), &before[..]);
        assert_eq!(c.get_pixel(320, 170), None);
    }

    #[test]
    fn bayer_threshold_matches_matrix() {
        assert_eq!(bayer_threshold(0, 0), 0);
        // 319 & 3 = 3, 169 & 3 = 1 -> BAYER4[1][3] = 6.
        assert_eq!(bayer_threshold(319, 169), 6 * 17);
    }

    #[test]
    fn dither_half_has_ink_and_paper() {
        let mut c = Canvas::new();
        c.fill_rect_dithered(10, 10, 8, 8, 128, INK, PAPER);
        let (mut ink, mut paper) = (0, 0);
        for y in 10..18 {
            for x in 10..18 {
                match c.get_pixel(x, y) {
                    Some(INK) => ink += 1,
                    Some(PAPER) => paper += 1,
                    other => panic!("unexpected {other:?}"),
                }
            }
        }
        assert_eq!((ink, paper), (32, 32));
    }

    #[test]
    fn dither_extremes_are_solid() {
        let mut c = Canvas::new();
        c.fill_rect_dithered(0, 0, 4, 4, 255, INK, PAPER);
        c.fill_rect_dithered(4, 0, 4, 4, 0, INK, PAPER);
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(c.get_pixel(x, y), Some(INK));
                assert_eq!(c.get_pixel(x + 4, y), Some(PAPER));
            }
        }
    }

    #[test]
    fn rects_clip_without_panic() {
        let mut c = Canvas::new();
        c.fill_rect(-10, -10, 1000, 1000, INK);
        c.draw_rect(300, 160, 50, 50, ACCENT_BLUE);
        c.fill_rect_dithered(-5, 165, 20, 20, 128, INK, PAPER);
        c.fill_rect(i32::MAX, i32::MAX, i32::MAX, i32::MAX, INK);
        assert_eq!(c.get_pixel(300, 160), Some(ACCENT_BLUE));
        assert_eq!(c.get_pixel(319, 169), Some(INK));
    }
}
