//! The idle screensaver: the agent's head, large and full screen, playing its
//! idle animation (breathing, blinks, the odd glance) while it drifts slowly
//! round the panel with the time under it.
//!
//! [`App`](super::App) decides when it is up (`App::tick`) and what the key
//! that ends it does (nothing else, `App::key`); this module only draws it.
//! It is a function of the clock, like every view, so `--dump-frame
//! --screensaver --time ..` frames are reproducible.

use std::f32::consts::TAU;
use std::time::Duration;

use crate::gfx::canvas::{Canvas, HEIGHT, INK, WIDTH};
use crate::gfx::font::{draw_text, Font};
use crate::gfx::head::sin;
use crate::gfx::mascot::Mascot;

/// Quiet time before the screensaver starts: no key, and nothing on screen
/// for the user. Inside the backlight's 30 s dim, so it plays lit for ten
/// seconds and dimmed for a minute before the screen goes off at 90 s.
pub const SCREENSAVER_AFTER: Duration = Duration::from_secs(20);
/// Its frame rate: enough for a pixel of drift a frame and a visible blink.
pub const SCREENSAVER_FPS: u64 = 12;

/// Head radius in pixels (the companion view's is 38).
pub const RADIUS: f32 = 48.0;
/// Top of the time, below the head's centre: clear of its shadow.
const CLOCK_DY: i32 = (1.35 * RADIUS) as i32 + 5;
/// How far the head, its ear pods, its shadow and the time reach from the
/// head's centre, with a two-pixel margin: the box the centre may drift in.
const REACH_X: i32 = (1.3 * RADIUS) as i32 + 2;
const REACH_UP: i32 = RADIUS as i32 + 2;
const REACH_DOWN: i32 = CLOCK_DY + 14 + 2;
/// The drift is a sine on each axis; the periods never line up, so the path
/// takes minutes to repeat. About a pixel a frame across, a third of one down.
const DRIFT_X_SECS: f64 = 47.0;
const DRIFT_Y_SECS: f64 = 29.0;

/// Where the head's centre is at `now`.
pub fn centre(now: f64) -> (i32, i32) {
    let swing = |lo: i32, hi: i32, period: f64| {
        let wave = sin(TAU * (now / period).fract() as f32);
        ((lo + hi) as f32 / 2.0 + (hi - lo) as f32 / 2.0 * wave).round() as i32
    };
    (
        swing(REACH_X, WIDTH - REACH_X, DRIFT_X_SECS),
        swing(REACH_UP, HEIGHT - REACH_DOWN, DRIFT_Y_SECS),
    )
}

/// Draws the screensaver on a cleared canvas: `mascot` (idle, whenever this
/// is up) at [`centre`], and the time `minutes` past midnight under it.
pub fn render(canvas: &mut Canvas, mascot: &Mascot, minutes: u16, now: f64, dark: bool) {
    let (x, y) = centre(now);
    mascot.draw(canvas, x, y, RADIUS, now, dark);
    let mut buf = [0u8; 5];
    let clock = super::fmt_clock(minutes, &mut buf);
    let w = Font::F8x14.text_width(clock);
    draw_text(canvas, x - w / 2, y + CLOCK_DY, clock, Font::F8x14, INK);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::canvas::PAPER;

    #[test]
    fn the_drift_keeps_the_head_and_the_time_on_screen() {
        // Twenty minutes of drift, and the moment nearest each corner of it.
        let (mut xs, mut ys) = ((i32::MAX, i32::MIN), (i32::MAX, i32::MIN));
        let mut corners = [(i32::MIN, 0.0); 4];
        for i in 0..24_000 {
            let now = f64::from(i) * 0.05;
            let (x, y) = centre(now);
            (xs.0, xs.1) = (xs.0.min(x), xs.1.max(x));
            (ys.0, ys.1) = (ys.0.min(y), ys.1.max(y));
            for (k, best) in corners.iter_mut().enumerate() {
                let (sx, sy) = (if k & 1 == 0 { 1 } else { -1 }, if k < 2 { 1 } else { -1 });
                if sx * x + sy * y > best.0 {
                    *best = (sx * x + sy * y, now);
                }
            }
        }
        assert!(xs.0 >= REACH_X && xs.1 <= WIDTH - REACH_X, "{xs:?}");
        assert!(ys.0 >= REACH_UP && ys.1 <= HEIGHT - REACH_DOWN, "{ys:?}");
        // ... and uses the room it has.
        assert!(xs.1 - xs.0 > 150 && ys.1 - ys.0 > 25, "{xs:?} {ys:?}");

        // Nothing is cut off in those corners: the border stays paper.
        let mut m = Mascot::new();
        m.snap(crate::gfx::mascot::MascotState::Idle);
        for now in corners.map(|(_, now)| now) {
            let mut c = Canvas::new();
            render(&mut c, &m, 14 * 60 + 32, now, false);
            for x in 0..WIDTH {
                assert_eq!(c.get_pixel(x, 0), Some(PAPER), "{now}: top, x {x}");
                assert_eq!(c.get_pixel(x, HEIGHT - 1), Some(PAPER), "{now}: bottom");
            }
            for y in 0..HEIGHT {
                assert_eq!(c.get_pixel(0, y), Some(PAPER), "{now}: left, y {y}");
                assert_eq!(c.get_pixel(WIDTH - 1, y), Some(PAPER), "{now}: right");
            }
        }
    }
}
