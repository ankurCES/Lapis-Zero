//! Crisp bitmap text rendering in three sizes derived from the 5x7 glcdfont.

use super::canvas::{Canvas, HEIGHT, WIDTH};
use super::font_data::{FONT_16X28, FONT_6X8, FONT_8X14};

/// Bitmap font sizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Font {
    /// 5x7 glyphs in a 6x8 cell: status bar, timestamps, ticker metadata.
    F6x8,
    /// 8x14 glyphs (1px letter spacing): menu items, titles, modal text.
    F8x14,
    /// 16x28 glyphs (2px letter spacing): ambient desk clock.
    F16x28,
}

impl Font {
    /// Advance cell `(width, height)` in pixels; width includes letter spacing.
    pub fn cell_size(self) -> (i32, i32) {
        match self {
            Font::F6x8 => (6, 8),
            Font::F8x14 => (9, 14),
            Font::F16x28 => (18, 28),
        }
    }

    /// Raw glyph bytes for `ch` (non-Latin-1 chars render as `?`).
    ///
    /// Layout per font: `F6x8` is 5 column bytes (bit 0 = top row); `F8x14` is
    /// 14 row bytes (bit 7 = leftmost); `F16x28` is 28 rows of 2 big-endian bytes.
    pub fn glyph_cols(self, ch: char) -> &'static [u8] {
        let i = if (ch as u32) < 256 {
            ch as usize
        } else {
            b'?' as usize
        };
        match self {
            Font::F6x8 => &FONT_6X8[i],
            Font::F8x14 => &FONT_8X14[i],
            Font::F16x28 => &FONT_16X28[i],
        }
    }

    /// Pixel width of `text` without trailing letter spacing.
    pub fn text_width(self, text: &str) -> i32 {
        let (w, _) = self.cell_size();
        let n = text.chars().count() as i32;
        if n == 0 {
            0
        } else {
            n * w - (w - self.glyph_width())
        }
    }

    /// Inked glyph width (cell width minus letter spacing).
    fn glyph_width(self) -> i32 {
        match self {
            Font::F6x8 => 5,
            Font::F8x14 => 8,
            Font::F16x28 => 16,
        }
    }
}

/// Draws `text` with its top-left at `(x, y)`; pixels off-canvas are clipped silently.
pub fn draw_text(canvas: &mut Canvas, x: i32, y: i32, text: &str, font: Font, color: u32) {
    let (cw, ch_h) = font.cell_size();
    if y <= -ch_h || y >= HEIGHT {
        return;
    }
    let mut gx = x;
    for ch in text.chars() {
        if gx >= WIDTH {
            break;
        }
        if gx <= -cw {
            gx = gx.saturating_add(cw);
            continue;
        }
        let g = font.glyph_cols(ch);
        match font {
            Font::F6x8 => {
                for (col, bits) in g.iter().enumerate() {
                    for row in 0..8 {
                        if bits >> row & 1 != 0 {
                            canvas.set_pixel(gx + col as i32, y + row, color);
                        }
                    }
                }
            }
            Font::F8x14 => {
                for (row, bits) in g.iter().enumerate() {
                    for col in 0..8 {
                        if bits & (0x80 >> col) != 0 {
                            canvas.set_pixel(gx + col, y + row as i32, color);
                        }
                    }
                }
            }
            Font::F16x28 => {
                for (row, pair) in g.chunks_exact(2).enumerate() {
                    let bits = u16::from_be_bytes([pair[0], pair[1]]);
                    for col in 0..16 {
                        if bits & (0x8000 >> col) != 0 {
                            canvas.set_pixel(gx + col, y + row as i32, color);
                        }
                    }
                }
            }
        }
        gx = gx.saturating_add(cw);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::canvas::{INK, PAPER};

    fn inked(c: &Canvas) -> usize {
        c.pixels()
            .chunks_exact(3)
            .filter(|p| p != &[0xF2, 0xF2, 0xEC])
            .count()
    }

    #[test]
    fn glyph_sizes() {
        for i in 0..=255u8 {
            let ch = i as char;
            assert_eq!(Font::F6x8.glyph_cols(ch).len(), 5);
            assert_eq!(Font::F8x14.glyph_cols(ch).len(), 14);
            assert_eq!(Font::F16x28.glyph_cols(ch).len(), 56);
        }
    }

    #[test]
    fn derived_fonts_scale_ink() {
        // Generator contract (ui/tools/gen_fonts.py): 8 cols from COL_MAP
        // [0,1,1,2,2,3,3,4]; 14 rows from ROW_MAP [0,1,1,2,2,3,3,4,4,5,5,6,6,7];
        // 16x28 doubles each row and column of 8x14. bit 0 = top row.
        const ROW_MAP: [usize; 14] = [0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7];
        for i in 0..=255u8 {
            let ch = i as char;
            let ones = |g: &[u8]| g.iter().map(|b| b.count_ones()).sum::<u32>();
            let cols = Font::F6x8.glyph_cols(ch);
            // Weight model verified against the generator: per source row r
            // (weighted by its ROW_MAP occurrence count) and per source column
            // sc (weighted by its COL_MAP run length: 1,2,2,2,1).
            let occ = |r: usize| ROW_MAP.iter().filter(|&&x| x == r).count() as u32;
            let run = [1u32, 2, 2, 2, 1];
            let want_8x14: u32 = (0..8)
                .map(|r| {
                    (0..5)
                        .map(|sc| if (cols[sc] >> r) & 1 != 0 { run[sc] } else { 0 })
                        .sum::<u32>()
                        * occ(r)
                })
                .sum();
            assert_eq!(ones(Font::F8x14.glyph_cols(ch)), want_8x14, "glyph {i}");
            assert_eq!(
                ones(Font::F16x28.glyph_cols(ch)),
                want_8x14 * 4,
                "glyph {i}"
            );
        }
    }

    #[test]
    fn descenders_survive_derivation() {
        // glcdfont draws descenders in the 8th row: p g y q j must keep ink
        // in the bottom third of 8x14 (rows 11..=13) and of 16x28.
        for ch in ['p', 'g', 'y', 'q', 'j'] {
            let rows = Font::F8x14.glyph_cols(ch);
            let lower: u32 = rows[11..14].iter().map(|b| b.count_ones()).sum();
            assert!(lower > 0, "8x14 '{ch}' lost its descender");
            let rows = Font::F16x28.glyph_cols(ch);
            let lower: u32 = rows[22..28].iter().map(|b| b.count_ones()).sum();
            assert!(lower > 0, "16x28 '{ch}' lost its descender");
        }
    }

    #[test]
    fn draw_text_inks_pixels() {
        let mut c = Canvas::new();
        assert_eq!(c.get_pixel(0, 0), Some(PAPER));
        draw_text(&mut c, 10, 10, "Ag~", Font::F6x8, INK);
        assert!(inked(&c) >= 10);
    }

    #[test]
    fn draw_text_clips_at_edges() {
        let mut c = Canvas::new();
        for font in [Font::F6x8, Font::F8x14, Font::F16x28] {
            draw_text(&mut c, -3, -3, "Ag~", font, INK);
            draw_text(&mut c, 317, 167, "Ag~", font, INK);
            draw_text(&mut c, i32::MIN, i32::MAX, "Ag~", font, INK);
            draw_text(&mut c, i32::MAX - 1, 0, "Ag~", font, INK);
        }
        assert!(inked(&c) > 0);
    }

    #[test]
    fn text_width() {
        assert_eq!(Font::F6x8.text_width(""), 0);
        assert_eq!(Font::F6x8.text_width("ab"), 11);
        assert_eq!(Font::F8x14.text_width("ab"), 17);
    }
}
