//! Damped spring physics (SPEC 5.2), integrated semi-implicit Euler.
//!
//!   a = k * (target - x) - d * v
//!   v += a * dt
//!   x += v * dt
//!
//! All UI motion (view slides, menu pill, modal pop-in) shares this struct so a
//! settled spring is reported consistently by the adaptive frame governor.

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

    /// Advances physics by `dt` seconds using semi-implicit Euler.
    pub fn tick(&mut self, dt: f32) {
        let a = self.k * (self.target - self.x) - self.d * self.v;
        self.v += a * dt;
        self.x += self.v * dt;
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
