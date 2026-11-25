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

    /// Copies `src` into this canvas with its top-left at `(dx, dy)`, row by row.
    ///
    /// Used for horizontal view slides. Pixels whose source row intersects the
    /// canvas are copied; rows outside it are skipped. Never allocates.
    pub fn blit(&mut self, src: &Canvas, dx: i32, dy: i32) {
        let sx = dx.max(0);
        let ex = (dx + WIDTH).min(WIDTH);
        if sx >= ex {
            return;
        }
        let sw = (ex - sx) as usize * 3;
        let src_off = (sx - dx) as usize * 3;
        for y in 0..HEIGHT {
            let ty = dy + y;
            if !(0..HEIGHT).contains(&ty) {
                continue;
            }
            let src_row = (y * WIDTH * 3) as usize + src_off;
            let dst_row = (ty * WIDTH * 3) as usize + sx as usize * 3;
            self.buf[dst_row..dst_row + sw].copy_from_slice(&src.buf[src_row..src_row + sw]);
        }
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

    /// Swaps [`INK`] and [`PAPER`] in place (the dark theme, applied to a finished
    /// frame). Accent colours are untouched, and so is every pixel inside `keep`
    /// (`(x, y, w, h)`): a QR code must stay dark-on-light to scan. Never allocates.
    pub fn swap_ink_paper(&mut self, keep: Option<(i32, i32, i32, i32)>) {
        let (ink, paper) = (split(INK), split(PAPER));
        let keep = keep.and_then(|(x, y, w, h)| clip(x, y, w, h));
        for (i, px) in self.buf.chunks_exact_mut(3).enumerate() {
            let (x, y) = (i as i32 % WIDTH, i as i32 / WIDTH);
            if keep.is_some_and(|(x0, y0, x1, y1)| (x0..x1).contains(&x) && (y0..y1).contains(&y)) {
                continue;
            }
            if px == ink {
                px.copy_from_slice(&paper);
            } else if px == paper {
                px.copy_from_slice(&ink);
            }
        }
    }

    /// Encodes the frame as a PNG (`screen_snapshot`, SPEC 6.3).
    ///
    /// The UI only draws the four palette colours, so the usual result is a
    /// 2-bit indexed image of a few kB, which keeps the NDJSON line small. A
    /// frame holding any other colour is written as 8-bit RGB instead.
    pub fn encode_png(&self) -> Result<Vec<u8>, png::EncodingError> {
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, WIDTH as u32, HEIGHT as u32);
        let indexed = self.pack_2bit();
        if indexed.is_some() {
            enc.set_color(png::ColorType::Indexed);
            enc.set_depth(png::BitDepth::Two);
            enc.set_palette(
                PNG_PALETTE
                    .iter()
                    .flat_map(|c| split(*c))
                    .collect::<Vec<u8>>(),
            );
        } else {
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Eight);
        }
        let mut w = enc.write_header()?;
        w.write_image_data(indexed.as_deref().unwrap_or(&self.buf))?;
        w.finish()?;
        Ok(out)
    }

    /// Packs the frame at 2 bits per pixel (indices into [`PNG_PALETTE`], leftmost
    /// pixel in the high bits), or `None` if it holds a colour outside the palette.
    fn pack_2bit(&self) -> Option<Vec<u8>> {
        // Four pixels per byte and no row padding: the width is a multiple of 4.
        const _: () = assert!(WIDTH % 4 == 0);
        let mut out = vec![0u8; (WIDTH * HEIGHT / 4) as usize];
        for (i, px) in self.buf.chunks_exact(3).enumerate() {
            let idx = PNG_PALETTE.iter().position(|c| split(*c) == px)?;
            out[i / 4] |= (idx as u8) << (6 - 2 * (i % 4));
        }
        Some(out)
    }
}

/// Palette of an indexed [`Canvas::encode_png`] image.
const PNG_PALETTE: [u32; 4] = [PAPER, INK, ACCENT_BLUE, ACCENT_AMBER];

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
    fn blit_copies_and_clips() {
        let mut src = Canvas::new();
        for x in 0..WIDTH {
            src.set_pixel(x, 80, ACCENT_BLUE);
        }
        let mut dst = Canvas::new();
        dst.blit(&src, -10, 0);
        // Source row y=80 is fully accent; shifting left 10 copies src cols
        // 10..=319 into dst cols 0..=309, leaving dst 310..=319 paper.
        assert_eq!(dst.get_pixel(0, 80), Some(ACCENT_BLUE));
        assert_eq!(dst.get_pixel(309, 80), Some(ACCENT_BLUE));
        assert_eq!(dst.get_pixel(310, 80), Some(PAPER));
    }

    #[test]
    fn swap_ink_paper_spares_accents_and_the_keep_rect() {
        let mut c = Canvas::new();
        c.fill_rect(0, 0, 10, 10, INK);
        c.fill_rect(20, 0, 10, 10, ACCENT_BLUE);
        c.fill_rect(100, 100, 4, 4, INK); // inside the keep rect
        c.swap_ink_paper(Some((96, 96, 12, 12)));
        assert_eq!(c.get_pixel(0, 0), Some(PAPER));
        assert_eq!(c.get_pixel(15, 5), Some(INK));
        assert_eq!(c.get_pixel(20, 0), Some(ACCENT_BLUE));
        assert_eq!(c.get_pixel(100, 100), Some(INK));
        assert_eq!(c.get_pixel(96, 96), Some(PAPER));
        assert_eq!(c.get_pixel(107, 107), Some(PAPER));
        assert_eq!(c.get_pixel(108, 108), Some(INK));
        // Swapping twice without a keep rect is the identity.
        let before = c.pixels().to_vec();
        c.swap_ink_paper(None);
        c.swap_ink_paper(None);
        assert_eq!(c.pixels(), &before[..]);
    }

    /// Decodes a PNG to `(width, height, RGB888 bytes)`.
    fn decode_rgb(png_bytes: &[u8]) -> (u32, u32, Vec<u8>) {
        let mut dec = png::Decoder::new(png_bytes);
        dec.set_transformations(png::Transformations::EXPAND);
        let mut reader = dec.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!(info.color_type, png::ColorType::Rgb);
        buf.truncate(info.buffer_size());
        (info.width, info.height, buf)
    }

    #[test]
    fn png_round_trips_indexed_and_rgb() {
        let mut c = Canvas::new();
        c.fill_rect(3, 5, 40, 20, INK);
        c.fill_rect(317, 160, 3, 10, ACCENT_AMBER);
        c.set_pixel(1, 0, ACCENT_BLUE);
        let small = c.encode_png().unwrap();
        assert_eq!(decode_rgb(&small), (320, 170, c.pixels().to_vec()));

        // One off-palette pixel forces the RGB fallback, still pixel-exact.
        c.set_pixel(160, 85, 0x123456);
        let rgb = c.encode_png().unwrap();
        assert_eq!(decode_rgb(&rgb), (320, 170, c.pixels().to_vec()));
        assert!(small.len() < rgb.len());
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
