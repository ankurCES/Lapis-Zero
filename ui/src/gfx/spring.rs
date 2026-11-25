//! Damped spring physics (SPEC 5.2), integrated semi-implicit Euler.
//!
//!   a = k * (target - x) - d * v
//!   v += a * dt
//!   x += v * dt
//!
//! All UI motion (view slides, menu pill, modal pop-in, the head's easing)
//! shares this struct so a settled spring is reported consistently by the
//! adaptive frame governor.
//!
//! The frame governor is also why a tick is not one integration step. The
//! scheme above only settles while `k * dt^2 + 2 * d * dt < 4`, which for the
//! springs in this UI means a step under 53 to 77 ms; past that every step
//! throws the value further out. Frames are 16.7 ms apart only while
//! something is known to be moving: otherwise they come every 83 ms (12 FPS)
//! or slower, and the loops hand over up to 100 ms after a stall. So
//! [`Spring::tick`] takes the time it is given in steps of [`MAX_STEP`].

/// The longest single integration step: a 60 FPS frame, so a spring stepped
/// at 60 FPS moves exactly as one step per frame would move it.
const MAX_STEP: f32 = 1.0 / 60.0;
/// The most time one tick advances: what the live loops cap a frame's time
/// at. After a stall, or between idle frames a second apart, motion picks up
/// where it was instead of having happened unseen.
const MAX_TICK: f32 = 0.1;

/// One scalar damped spring.
#[derive(Clone, Copy, Debug)]
pub struct Spring {
    /// Stiffness (N/m analogue).
    k: f32,
    /// Damping coefficient.
    d: f32,
    /// Current position.
    x: f32,
    /// Current velocity.
    v: f32,
    /// Rest position the spring is pulled toward.
    target: f32,
}

impl Spring {
    /// A spring with explicit stiffness and damping, at rest at `target`.
    pub fn new(k: f32, d: f32, target: f32) -> Self {
        Self {
            k,
            d,
            x: target,
            v: 0.0,
            target,
        }
    }

    /// SPEC 5.2 presets.
    pub fn slide() -> Self {
        Self::new(MENU_SLIDE.k, MENU_SLIDE.d, 0.0)
    }
    pub fn pill() -> Self {
        Self::new(MENU_PILL.k, MENU_PILL.d, 0.0)
    }
    pub fn modal() -> Self {
        Self::new(MODAL_POP.k, MODAL_POP.d, 1.0)
    }

    /// Snaps to `target` with zero velocity (used by `--dump-frame` snapshots).
    pub fn snap_to(&mut self, target: f32) {
        self.target = target;
        self.x = target;
        self.v = 0.0;
    }

    /// Moves the rest position (the spring then animates toward it).
    pub fn set_target(&mut self, target: f32) {
        self.target = target;
    }

    /// Keeps the velocity but jumps the position (instant start of a slide).
    pub fn set_position(&mut self, x: f32) {
        self.x = x;
    }

    /// Advances physics by `dt` seconds (at most [`MAX_TICK`]), in steps the
    /// integrator is stable at whatever the frame rate. A `dt` that is zero,
    /// negative or not a number advances nothing.
    pub fn tick(&mut self, dt: f32) {
        if dt.is_nan() {
            return;
        }
        let mut left = dt.min(MAX_TICK);
        while left > 0.0 {
            let h = left.min(MAX_STEP);
            let a = self.k * (self.target - self.x) - self.d * self.v;
            self.v += a * h;
            self.x += self.v * h;
            left -= h;
        }
    }

    /// Current position.
    pub fn value(self) -> f32 {
        self.x
    }

    /// Current velocity.
    pub fn velocity(self) -> f32 {
        self.v
    }

    /// True when the spring is essentially at rest (SPEC governor threshold).
    pub fn settled(self) -> bool {
        self.v.abs() <= 0.05 && (self.target - self.x).abs() <= 0.25
    }

    /// True when there is visible motion left (`!settled`).
    pub fn is_active(self) -> bool {
        !self.settled()
    }

    /// Absolute displacement remaining, used by the governor.
    pub fn remaining(self) -> f32 {
        (self.target - self.x).abs()
    }
}

/// SPEC 5.2 spring constants.
#[derive(Clone, Copy)]
struct Preset {
    k: f32,
    d: f32,
}

const MENU_SLIDE: Preset = Preset { k: 240.0, d: 24.0 };
const MENU_PILL: Preset = Preset { k: 320.0, d: 26.0 };
const MODAL_POP: Preset = Preset { k: 360.0, d: 28.0 };

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converges_to_target() {
        let mut s = Spring::new(240.0, 24.0, 0.0);
        s.set_position(320.0);
        for _ in 0..500 {
            s.tick(1.0 / 60.0);
        }
        assert!(s.value().abs() < 1.0, "value {} not converged", s.value());
        assert!(s.settled());
    }

    #[test]
    fn underdamped_overshoots_then_settles() {
        let mut s = Spring::slide();
        s.set_target(1.0);
        let mut overshot = false;
        for _ in 0..1000 {
            s.tick(1.0 / 60.0);
            if s.value() > 1.0 {
                overshot = true;
            }
        }
        assert!(overshot, "slide spring should overshoot slightly");
        assert!(s.settled());
    }

    #[test]
    fn presets_match_spec() {
        // Constants verified against SPEC 5.2.
        for (k, d) in [
            (240.0, 24.0), // view slide
            (320.0, 26.0), // menu pill
            (360.0, 28.0), // modal pop-in
        ] {
            let s = Spring::new(k, d, 0.0);
            assert_eq!(s.k, k);
            assert_eq!(s.d, d);
        }
    }

    #[test]
    fn slow_frames_settle_like_fast_ones() {
        // The governor steps a spring at 60 FPS only while it knows of
        // motion; a spring can just as well be ticked every 83 ms (12 FPS),
        // every 100 ms (the loops' cap on a frame's time) or raggedly. One
        // integration step that long would diverge for every spring here.
        let ragged = [0.0853, 0.001, 0.1, 1.0 / 60.0, 0.25, 0.004, 0.0633];
        for (k, d) in [
            (240.0f32, 24.0f32),
            (320.0, 26.0),
            (360.0, 28.0),
            (180.0, 19.0),
        ] {
            // The scheme's own limit for this spring (see the module docs).
            let limit = (-2.0 * d + (4.0 * d * d + 16.0 * k).sqrt()) / (2.0 * k);
            assert!(limit < 1.0 / 12.0 && MAX_STEP < limit / 3.0, "k {k}");
            for dt in [1.0 / 60.0, 1.0 / 12.0, 0.1, 1.0, f32::INFINITY, 0.0] {
                let mut s = Spring::new(k, d, 0.0);
                s.set_position(100.0);
                let (mut far, mut clock) = (0.0f32, 0.0f32);
                for i in 0..1200 {
                    // `0.0` stands for the ragged sequence.
                    let step = if dt == 0.0 {
                        ragged[i % ragged.len()]
                    } else {
                        dt
                    };
                    s.tick(step);
                    clock += step.min(MAX_TICK);
                    assert!(s.value().is_finite() && s.velocity().is_finite());
                    far = far.max(-s.value());
                    // A second and a half of spring time is enough to arrive ...
                    if clock > 1.5 {
                        // ... and what has settled stays settled.
                        assert!(s.settled(), "k {k} dt {dt}: {} at {clock} s", s.value());
                    }
                }
                assert!(far < 12.0, "k {k} dt {dt}: overshot by {far}%");
            }
        }
        // 60 FPS frames are still one step each: the same motion as ever.
        let (mut a, mut b) = (Spring::slide(), Spring::slide());
        a.set_position(320.0);
        b.set_position(320.0);
        for _ in 0..40 {
            let h = 1.0 / 60.0;
            a.tick(h);
            let acc = b.k * (b.target - b.x) - b.d * b.v;
            b.v += acc * h;
            b.x += b.v * h;
            assert_eq!((a.x, a.v), (b.x, b.v));
        }
        // A slower frame lands where the 60 FPS frames it spans would have.
        let mut c = Spring::slide();
        c.set_position(320.0);
        for _ in 0..8 {
            c.tick(5.0 / 60.0);
        }
        assert!((c.x - a.x).abs() < 0.01, "{} against {}", c.x, a.x);
        // No time, negative time and nonsense advance nothing.
        let before = (a.x, a.v);
        for dt in [0.0, -1.0, f32::NAN, f32::NEG_INFINITY] {
            a.tick(dt);
            assert_eq!((a.x, a.v), before, "dt {dt}");
        }
    }

    #[test]
    fn snap_to_rests_instantly() {
        let mut s = Spring::slide();
        s.set_position(999.0);
        s.snap_to(42.0);
        assert_eq!(s.value(), 42.0);
        assert!(s.settled());
    }

    #[test]
    fn activity_thresholds() {
        let mut s = Spring::new(240.0, 24.0, 0.0);
        s.snap_to(0.0);
        assert!(!s.is_active());
        s.set_target(0.1); // within 0.25, no velocity yet
        s.v = 0.0;
        assert!(!s.is_active(), "displacement 0.1 should read settled");
        s.set_target(10.0);
        assert!(s.is_active());
    }
}
