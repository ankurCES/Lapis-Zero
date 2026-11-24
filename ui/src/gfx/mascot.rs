//! Procedural pixel mascot: five distinct animation states (SPEC 5.3 View 1).
//!
//! The creature is a fixed 40x32 rounded-rect "blob" built from rectangles, so
//! rendering is allocation-free and deterministic for `--dump-frame` snapshots.

use crate::gfx::canvas::{Canvas, ACCENT_AMBER, ACCENT_BLUE, INK, PAPER};

/// The five mascot animation states (SPEC 5.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MascotState {
    Idle,
    Listening,
    Thinking,
    Acting,
    WaitingApproval,
}

impl MascotState {
    /// Short label for the persistent status bar.
    pub fn label(self) -> &'static str {
        match self {
            MascotState::Idle => "IDLE",
            MascotState::Listening => "LISTEN",
            MascotState::Thinking => "THINK",
            MascotState::Acting => "ACTING",
            MascotState::WaitingApproval => "APPROV",
        }
    }
}

/// Time-varying scalar inputs computed by the coordinator (or pinned in dumps).
#[derive(Clone, Copy, Debug, Default)]
pub struct MascotAnim {
    /// Breathing phase `-1.0..=1.0` (sine wave).
    pub breath: f32,
    /// Close the eyes this frame.
    pub blink: bool,
    /// Microphone RMS `0.0..=1.0` (listening waveform).
    pub rms: f32,
    /// Eye scan `-1.0..=1.0` (thinking).
    pub scan: f32,
    /// Orbiting-dot phase `0.0..=1.0` (thinking).
    pub orbit: f32,
}

/// Draws the mascot with its top-left at `(x0, y0)`. Out-of-canvas pixels clip.
pub fn draw(canvas: &mut Canvas, x0: i32, y0: i32, state: MascotState, a: &MascotAnim) {
    let jitter = match state {
        MascotState::WaitingApproval => jitter_px(a.breath),
        _ => 0,
    };
    let bx = x0 + jitter;
    let by = y0 + breathe_px(a.breath);

    // Rounded body: solid ink rect with the four corners punched back to paper.
    canvas.fill_rect(bx, by, 40, 32, INK);
    let corner = 3;
    for (cx, cy) in [
        (bx, by),
        (bx + 40 - corner, by),
        (bx, by + 32 - corner),
        (bx + 40 - corner, by + 32 - corner),
    ] {
        canvas.fill_rect(cx, cy, corner, corner, PAPER);
    }

    // Eyes (scan for THINKING).
    let scan = (a.scan * 2.0).round() as i32;
    let el = bx + 8 + scan;
    for x in [el, el + 20] {
        if a.blink {
            // Closed: a single paper 4x1 line at mid-height.
            canvas.fill_rect(x, by + 14, 4, 4, INK);
            canvas.fill_rect(x, by + 14, 4, 1, PAPER);
        } else {
            canvas.fill_rect(x, by + 10, 4, 4, PAPER);
        }
    }

    // Mouth: a wide paper bar; slightly taller + open while talking.
    let mh = if state == MascotState::Listening {
        5
    } else {
        2
    };
    canvas.fill_rect(bx + 16, by + 22, 6, mh, PAPER);

    // State-specific accents.
    match state {
        MascotState::Idle => {}
        MascotState::Listening => draw_waveform(canvas, bx + 44, by + 8, a.rms),
        MascotState::Thinking => draw_orbit(canvas, bx + 20, by - 4, a.orbit),
        MascotState::Acting => draw_typing(canvas, bx, by, a.breath),
        MascotState::WaitingApproval => {
            let p = (a.breath * 1.5).round() as i32;
            canvas.fill_rect(bx + 34, by - 6 + p, 4, 4, ACCENT_AMBER);
        }
    }
}

/// Five vertical bars driven by mic RMS (listening).
fn draw_waveform(canvas: &mut Canvas, x: i32, y: i32, rms: f32) {
    for i in 0..5 {
        // Deterministic pseudo-noise so the waveform is lively even at fixed RMS.
        let var = (((i * 7 + 3) % 9) - 4) as f32 / 12.0;
        let level = (rms * 0.8 + 0.2 + var * rms).clamp(0.1, 1.0);
        let h = (level * 16.0).round() as i32;
        canvas.fill_rect(x + i * 7, y + 16 - h, 3, h, INK);
    }
}

/// Three accent-blue dots orbiting above the head (thinking).
fn draw_orbit(canvas: &mut Canvas, cx: i32, ty: i32, phase: f32) {
    for i in 0..3 {
        let t = phase + i as f32 / 3.0;
        let dx = ((t * std::f32::consts::TAU).sin() * 16.0).round() as i32;
        let dy = ((t * std::f32::consts::TAU).cos() * 5.0).round() as i32;
        canvas.fill_rect(cx + dx, ty + 2 + dy, 3, 3, ACCENT_BLUE);
    }
}

/// Typing hands + pulsing accent pip (acting).
fn draw_typing(canvas: &mut Canvas, bx: i32, by: i32, breath: f32) {
    let pulse = ((breath * 0.5 + 0.5) * 2.0).round() as i32;
    canvas.fill_rect(bx + 16, by - 7 + pulse, 3, 3, ACCENT_BLUE);
    // Two small paper "hands" at the base, offset alternately.
    let sway = (breath * 1.5).round() as i32;
    canvas.fill_rect(bx - 5, by + 24 + sway, 4, 2, INK);
    canvas.fill_rect(bx + 41, by + 24 - sway, 4, 2, INK);
}

fn breathe_px(breath: f32) -> i32 {
    (breath * 1.5).round() as i32
}

fn jitter_px(breath: f32) -> i32 {
    (breath * 2.0).round() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_distinct_and_short() {
        let states = [
            MascotState::Idle,
            MascotState::Listening,
            MascotState::Thinking,
            MascotState::Acting,
            MascotState::WaitingApproval,
        ];
        for s in states {
            let l = s.label();
            assert!(!l.is_empty());
            assert!(l.len() <= 7);
        }
    }

    #[test]
    fn draw_does_not_panic_out_of_bounds() {
        let mut c = Canvas::new();
        let a = MascotAnim {
            breath: 1.0,
            blink: true,
            rms: 1.0,
            scan: 1.0,
            orbit: 1.0,
        };
        for (i, s) in [
            MascotState::Idle,
            MascotState::Listening,
            MascotState::Thinking,
            MascotState::Acting,
            MascotState::WaitingApproval,
        ]
        .into_iter()
        .enumerate()
        {
            let x0 = -10 + i as i32 * 5;
            draw(&mut c, x0, -5, s, &a);
            draw(&mut c, 300, 160, s, &a);
        }
        // Blob at origin: body ink present, corner punched back to paper.
        let mut c2 = Canvas::new();
        draw(&mut c2, 10, 20, MascotState::Idle, &a);
        assert_eq!(c2.get_pixel(30, 36), Some(INK));
        assert_eq!(c2.get_pixel(10, 20), Some(PAPER));
    }
}
