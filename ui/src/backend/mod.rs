//! Display and input backend traits shared by the simulator, PNG dump and CM0 targets.

use crate::gfx::canvas::Canvas;

#[cfg(feature = "sim")]
pub mod sdl_sim;

/// CardputerZero semantic keys (SPEC 5.4). Backends map physical keys onto these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LapisKey {
    /// Move highlight / scroll up (`F` or `Up`).
    Up,
    /// Move highlight / scroll down (`X` or `Down`).
    Down,
    /// Generic left (unused by the CardputerZero map; views cycle via [`LapisKey::CycleL`]).
    Left,
    /// Generic right (unused by the CardputerZero map; views cycle via [`LapisKey::CycleR`]).
    Right,
    /// Select / confirm / expand / approve.
    Enter,
    /// Back / cancel / deny.
    Esc,
    /// Quick-cycle the active Tailscale target host.
    Tab,
    /// Cycle to the previous view (`Z` or `Left`).
    CycleL,
    /// Cycle to the next view (`C` or `Right`).
    CycleR,
    /// A printable character.
    Char(char),
}

/// Something that can show a finished frame.
pub trait DisplayBackend {
    /// Presents `canvas` as the next frame.
    fn push_frame(&mut self, canvas: &Canvas);
}

/// Something that yields semantic key presses without blocking.
pub trait InputBackend {
    /// Returns the next pending key, or `None` if nothing is queued.
    fn poll_event(&mut self) -> Option<LapisKey>;
}
