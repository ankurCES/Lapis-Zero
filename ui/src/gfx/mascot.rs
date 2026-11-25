//! The agent's face (SPEC 5.3 View 1): ten states, each a pose of the 3D head
//! in [`super::head`] plus a small prop, all computed from the state and the
//! clock. Nothing here allocates, and a given state and time always draw the
//! same pixels, so `--dump-frame --state .. --time ..` snapshots are stable.
//!
//! The clock is seconds as `f64`: this runs for weeks on end, and an `f32` of
//! seconds has lost 60 FPS resolution after three days. Only what has been
//! reduced first (a phase, the seconds since something) is narrowed to `f32`.

use std::f32::consts::{PI, TAU};

use super::canvas::{Canvas, ACCENT_AMBER, ACCENT_BLUE, INK, PAPER};
use super::font::{draw_text, Font};
use super::head::{self, cos, dither, sin, Accent, Eyes, Pose};
use super::spring::Spring;

/// What the head is acting out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MascotState {
    /// Nothing to do: breathing, blinking, an occasional glance.
    Idle,
    /// Push-to-talk held: leaning in, sound arcs closing in on the ears.
    Listening,
    /// Waiting for the model: eyes up, a moon orbiting the head.
    Thinking,
    /// Acting on the host (clicks, typing, keys): nodding over a keyboard,
    /// two hands striking it in turn.
    Working,
    /// Taking a screenshot or waiting on the host: looking out, swallowing packets.
    Fetching,
    /// A command running on the host: a prompt for eyes, output scrolling by.
    Command,
    /// Needs the owner: amber, hopping, a question in a bubble.
    WaitingApproval,
    /// Held by the owner: drooped and asleep under a pause mark.
    Paused,
    /// A task just succeeded (a beat of [`BEAT_SECS`], then idle).
    Done,
    /// A task just failed (a beat of [`BEAT_SECS`], then idle).
    Error,
}

/// How long the `Done` and `Error` beats last before the head goes idle.
pub const BEAT_SECS: f32 = 1.6;
/// The least time a step on the host stays on the face, however quickly it
/// returned: counted from when it was first seen.
pub const ACTION_HOLD_SECS: f32 = 1.2;

impl MascotState {
    /// Every state, in `--demo` order.
    pub const ALL: [MascotState; 10] = [
        MascotState::Idle,
        MascotState::Listening,
        MascotState::Thinking,
        MascotState::Working,
        MascotState::Fetching,
        MascotState::Command,
        MascotState::WaitingApproval,
        MascotState::Paused,
        MascotState::Done,
        MascotState::Error,
    ];

    /// Short label for the persistent status bar (at most six characters).
    pub fn label(self) -> &'static str {
        match self {
            MascotState::Idle => "IDLE",
            MascotState::Listening => "LISTEN",
            MascotState::Thinking => "THINK",
            MascotState::Working => "WORK",
            MascotState::Fetching => "FETCH",
            MascotState::Command => "RUN",
            MascotState::WaitingApproval => "APPROV",
            MascotState::Paused => "PAUSED",
            MascotState::Done => "DONE",
            MascotState::Error => "ERROR",
        }
    }

    /// Lowercase CLI name (`--state`).
    pub fn name(self) -> &'static str {
        match self {
            MascotState::Idle => "idle",
            MascotState::Listening => "listening",
            MascotState::Thinking => "thinking",
            MascotState::Working => "working",
            MascotState::Fetching => "fetching",
            MascotState::Command => "command",
            MascotState::WaitingApproval => "approval",
            MascotState::Paused => "paused",
            MascotState::Done => "done",
            MascotState::Error => "error",
        }
    }

    /// Parses a CLI state name.
    pub fn from_name(name: &str) -> Option<MascotState> {
        MascotState::ALL.into_iter().find(|s| s.name() == name)
    }

    /// The state for a `state_sync.agent_state`, or `None` for one this UI does not know.
    pub fn from_agent(agent_state: &str) -> Option<MascotState> {
        Some(match agent_state {
            "IDLE" => MascotState::Idle,
            "LISTENING" => MascotState::Listening,
            "THINKING" => MascotState::Thinking,
            "ACTING" => MascotState::Working,
            "WAITING_APPROVAL" => MascotState::WaitingApproval,
            "PAUSED" => MascotState::Paused,
            _ => return None,
        })
    }

    /// What the head acts out for an `activity_event.kind` (or an approval's
    /// `action`). `None` for the kinds that are not something done on the host.
    pub fn from_activity(kind: &str) -> Option<MascotState> {
        Some(match kind {
            "thought" | "prompt" => return None,
            "screenshot" | "wait" => MascotState::Fetching,
            "ssh_exec" | "agent_run" => MascotState::Command,
            // vnc_click, vnc_move, vnc_drag, vnc_scroll, type_text, vnc_key,
            // and any kind added later.
            _ => MascotState::Working,
        })
    }

    /// The beat for a finished task's status; `None` (straight to idle) for a
    /// cancelled one.
    pub fn from_finish(status: &str) -> Option<MascotState> {
        match status {
            "done" => Some(MascotState::Done),
            "failed" | "stopped" => Some(MascotState::Error),
            _ => None,
        }
    }

    /// The state to show: `base` is the daemon's `agent_state`, `step` the
    /// step on the host that is on show (one still running, or one first seen
    /// less than [`ACTION_HOLD_SECS`] ago), `beat` a task ending that has not
    /// played out yet.
    pub fn resolve(
        base: MascotState,
        step: Option<MascotState>,
        beat: Option<MascotState>,
    ) -> MascotState {
        match base {
            // Acting shows the step; so does thinking while a step that has
            // just returned still has some of its hold left.
            MascotState::Working | MascotState::Thinking => step.unwrap_or(base),
            MascotState::Idle => beat.unwrap_or(base),
            _ => base,
        }
    }

    /// The accent this state lights the eyes and ear pods with.
    pub fn accent(self) -> Accent {
        match self {
            MascotState::Idle | MascotState::Listening | MascotState::Paused => Accent::None,
            MascotState::WaitingApproval | MascotState::Error => Accent::Amber,
            _ => Accent::Blue,
        }
    }

    /// True for the two one-shot beats.
    pub fn is_beat(self) -> bool {
        matches!(self, MascotState::Done | MascotState::Error)
    }

    fn eyes(self, now: f64) -> Eyes {
        match self {
            MascotState::Done => Eyes::Happy,
            MascotState::Error => Eyes::Cross,
            MascotState::Paused => Eyes::Asleep,
            MascotState::Command => Eyes::Prompt {
                cursor: phase(now, 1.0) < 0.55,
            },
            _ => Eyes::Open,
        }
    }
}

/// Position in a cycle of `period` seconds, `0.0..=1.0`.
fn phase(t: f64, period: f64) -> f32 {
    let x = t / period;
    (x - x.floor()) as f32
}

/// A sine of `period` seconds.
fn wave(t: f64, period: f64) -> f32 {
    sin(TAU * phase(t, period))
}

/// Smoothstep on `0..1`.
fn smooth(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// A repeatable pseudo-random `0.0..1.0` for slot `n` (blink and glance timing).
fn hash01(n: u32) -> f32 {
    let mut x = n.wrapping_mul(0x9E37_79B9) ^ 0x85EB_CA6B;
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    (x >> 8) as f32 / 16_777_216.0
}

/// Blinks: one per slot, starting at a hashed moment inside it.
const BLINK_SLOT: f64 = 5.6;
const BLINK_SECS: f64 = 0.24;
/// Idle glances: one per slot, to a hashed side.
const GLANCE_SLOT: f64 = 13.0;
const GLANCE_SECS: f64 = 2.2;
const GLANCE_TURN: f64 = 0.35;
/// The idle head sways slowly in depth between those: a frame this often
/// (4 FPS) is enough to draw it.
pub const IDLE_FRAME_SECS: f64 = 0.25;

/// Start of the blink in the slot `t` falls in, shifted by `ahead` slots.
fn blink_start(t: f64, ahead: f64) -> f64 {
    let slot = (t / BLINK_SLOT).floor() + ahead;
    (slot + 0.1 + 0.8 * f64::from(hash01(slot as u32))) * BLINK_SLOT
}

/// Start of the glance in the slot `t` falls in, shifted by `ahead` slots, and its side.
fn glance_start(t: f64, ahead: f64) -> (f64, f32) {
    let slot = (t / GLANCE_SLOT).floor() + ahead;
    let h = f64::from(hash01(slot as u32 ^ 0x5bd1));
    let side = if hash01(slot as u32 ^ 0x1234) < 0.5 {
        -1.0
    } else {
        1.0
    };
    (
        slot * GLANCE_SLOT + 2.0 + h * (GLANCE_SLOT - GLANCE_SECS - 2.5),
        side,
    )
}

/// Eye opening from the blink schedule: 1 open, 0 shut.
fn blink(t: f64) -> f32 {
    let x = ((t - blink_start(t, 0.0)) / BLINK_SECS) as f32;
    if !(0.0..1.0).contains(&x) {
        1.0
    } else if x < 0.3 {
        1.0 - x / 0.3
    } else if x < 0.5 {
        0.0
    } else {
        (x - 0.5) / 0.5
    }
}

/// The idle glance, `-1.0..=1.0`: eased out to one side, held, eased back.
fn glance(t: f64) -> f32 {
    let (start, side) = glance_start(t, 0.0);
    let x = t - start;
    side * smooth((x / GLANCE_TURN) as f32) * smooth(((GLANCE_SECS - x) / GLANCE_TURN) as f32)
}

/// Seconds until an idle head's next blink or glance turn: 0 while one is
/// moving. Lets the idle governor wake exactly for them instead of sampling
/// past a 240 ms blink at [`IDLE_FRAME_SECS`].
pub fn idle_wait(t: f64) -> f64 {
    let mut wait = f64::MAX;
    for ahead in [0.0, 1.0] {
        let blink = blink_start(t, ahead);
        // A glance is two turns with a still hold between them.
        let (out, _) = glance_start(t, ahead);
        let back = out + GLANCE_SECS - GLANCE_TURN;
        for (start, len) in [(blink, BLINK_SECS), (out, GLANCE_TURN), (back, GLANCE_TURN)] {
            if (start..start + len).contains(&t) {
                return 0.0;
            }
            if start > t {
                wait = wait.min(start - t);
            }
        }
    }
    wait
}

/// Height of a hop that starts at `x = 0` and lands at `x = 1` (0 outside).
fn hop(x: f32) -> f32 {
    if (0.0..1.0).contains(&x) {
        sin(PI * x)
    } else {
        0.0
    }
}

/// Where `state` wants the head at time `t`, `tau` seconds into the state.
fn target(state: MascotState, t: f64, tau: f32, rms: f32) -> Pose {
    let glow = if state.accent() == Accent::None {
        0.0
    } else {
        1.0
    };
    let p = Pose {
        open: blink(t),
        glow,
        ..Pose::REST
    };
    match state {
        MascotState::Idle => {
            // Breathing, and a slow sway in depth so the face is never held
            // dead-on for long: turned a little, the visor shows its curve.
            let (breath, g) = (wave(t, 4.0), glance(t));
            Pose {
                yaw: 0.32 * g + 0.07 * wave(t, 9.0),
                pitch: 0.05 + 0.02 * breath,
                roll: 0.025 * wave(t, 7.0),
                lift: 0.03 * breath,
                look_x: 0.09 * g,
                ..p
            }
        }
        MascotState::Listening => Pose {
            yaw: -0.10,
            pitch: -0.10,
            roll: 0.15 + 0.03 * wave(t, 3.2),
            lift: 0.03 + 0.06 * rms,
            wide: 1.06 + 0.24 * rms,
            ..p
        },
        MascotState::Thinking => Pose {
            yaw: 0.24 * wave(t, 4.8),
            pitch: -0.22,
            roll: 0.07 * wave(t - 1.2, 4.8),
            lift: 0.03 * wave(t, 3.0),
            look_x: 0.10 * wave(t, 2.4),
            look_y: -0.11,
            ..p
        },
        MascotState::Working => {
            // One key every half second: a nod that lands on the beat and a
            // turn toward the hand that struck it.
            let b = phase(t, BEAT_STEP);
            let nod = (1.0 - b) * (1.0 - b);
            Pose {
                yaw: -0.10 * wave(t, 2.0 * BEAT_STEP),
                pitch: 0.20 + 0.14 * nod,
                lift: -0.02 - 0.07 * nod,
                look_x: -0.05 * wave(t, 2.0 * BEAT_STEP),
                look_y: 0.10,
                open: 0.66 * p.open,
                ..p
            }
        }
        MascotState::Fetching => {
            // A gulp for each packet: the eyes widen while it shrinks into
            // the head and ease back once it has gone.
            let q = phase(t, PACKET_SECS);
            let gulp = if q > 0.7 {
                smooth((q - 0.7) / 0.3)
            } else {
                (1.0 - q / 0.35).max(0.0)
            };
            Pose {
                yaw: 0.52 + 0.04 * wave(t, 1.8),
                pitch: -0.06,
                roll: 0.03,
                lift: 0.02 * wave(t, 2.2),
                look_x: 0.10,
                look_y: -0.02,
                wide: 1.0 + 0.18 * gulp,
                ..p
            }
        }
        MascotState::Command => {
            // Reading the output: a slow scan toward it and back, and a
            // small nod as every third line comes in.
            let nod = (1.0 - phase(t, 3.0 * LINE_SECS) / 0.3).max(0.0);
            Pose {
                yaw: 0.16 + 0.10 * wave(t, 3.6),
                pitch: 0.05 + 0.04 * nod * nod,
                lift: 0.015 * wave(t, 2.4) - 0.02 * nod * nod,
                open: 1.0,
                ..p
            }
        }
        MascotState::WaitingApproval => {
            // Two hops, then a wait, every 1.6 s.
            let q = phase(t, 1.6) * 1.6;
            Pose {
                pitch: -0.08,
                roll: 0.12,
                lift: 0.02 + 0.14 * hop(q / 0.28) + 0.09 * hop((q - 0.28) / 0.28),
                wide: 1.18,
                ..p
            }
        }
        MascotState::Paused => {
            // Asleep: sunk and tilted, rising and falling on a slow breath.
            // (The sleeping arcs are the eye shape, drawn at full height.)
            let breath = wave(t, 6.0);
            Pose {
                yaw: -0.14,
                pitch: 0.26 + 0.03 * breath,
                roll: -0.10,
                lift: -0.06 + 0.02 * breath,
                open: 1.0,
                ..p
            }
        }
        MascotState::Done => {
            let h = hop(tau / 0.42) + 0.3 * hop((tau - 0.42) / 0.24);
            Pose {
                pitch: -0.14 * h,
                roll: 0.06,
                lift: 0.30 * h,
                wide: 1.1,
                open: 1.0,
                ..p
            }
        }
        MascotState::Error => {
            // A "no" that dies away over 0.9 s.
            let fade = (1.0 - tau / 0.9).max(0.0);
            Pose {
                yaw: 0.46 * fade * fade * sin(TAU * tau / 0.30),
                pitch: 0.12,
                lift: -0.04,
                wide: 1.05,
                open: 1.0,
                ..p
            }
        }
    }
}

/// The head plus the easing between its states.
pub struct Mascot {
    state: MascotState,
    /// The state being eased away from (its prop is still fading out).
    prev: MascotState,
    /// How much of `prev`'s prop was showing when it was left, `0.0..=1.0`.
    prev_strength: f32,
    /// When `state` was entered: the clock of the `Done` / `Error` beats.
    since: f64,
    /// The pose the head was in at that moment.
    from: Pose,
    /// 0 -> 100 as the pose and props cross over from `from` to `state`.
    blend: Spring,
    /// Microphone level `0.0..=1.0` (listening).
    pub rms: f32,
}

impl Default for Mascot {
    fn default() -> Self {
        Self::new()
    }
}

impl Mascot {
    /// An idle head at rest.
    pub fn new() -> Self {
        Self {
            state: MascotState::Idle,
            prev: MascotState::Idle,
            prev_strength: 1.0,
            since: 0.0,
            from: Pose::REST,
            // Slightly underdamped: arrives in about 0.4 s with a small overshoot.
            blend: Spring::new(180.0, 19.0, 100.0),
            rms: 0.0,
        }
    }

    /// The state being shown (or eased into).
    pub fn state(&self) -> MascotState {
        self.state
    }

    /// Seconds since the current state was entered.
    pub fn elapsed(&self, now: f64) -> f32 {
        (now - self.since) as f32
    }

    /// Starts easing into `state`; a no-op when it is already the state.
    pub fn set_state(&mut self, state: MascotState, now: f64) {
        if state == self.state {
            return;
        }
        self.from = self.pose(now);
        // The prop being left fades from however far it had grown, not from full.
        self.prev_strength = self.weight();
        self.prev = self.state;
        self.state = state;
        self.since = now;
        // From rest: whatever speed the last easing had belongs to it.
        self.blend.snap_to(100.0);
        self.blend.set_position(0.0);
    }

    /// Jumps into `state` as if it began at time zero (`--dump-frame`).
    pub fn snap(&mut self, state: MascotState) {
        self.state = state;
        self.prev = state;
        self.prev_strength = 1.0;
        self.since = 0.0;
        self.settle();
    }

    /// Finishes any easing at once.
    pub fn settle(&mut self) {
        self.blend.snap_to(100.0);
    }

    /// Advances the easing by `dt` seconds, whatever the frame rate: frames
    /// come 16.7 ms apart while the head is watched and up to a second apart
    /// while it is not, and the spring takes either in steps it is stable at.
    pub fn tick(&mut self, dt: f32) {
        self.blend.tick(dt);
        // At rest means exactly at rest: the governor then leaves the head
        // to its slow frame rate for good, and nothing that is not a number
        // can ever reach the renderer.
        if !self.blend.is_active() || !self.blend.value().is_finite() {
            self.blend.snap_to(100.0);
        }
    }

    /// True while the head needs full-rate frames: easing between states, or
    /// in the moving part of a beat.
    pub fn is_moving(&self, now: f64) -> bool {
        self.blend.is_active() || (self.state.is_beat() && self.elapsed(now) < 1.0)
    }

    /// How far the easing into the current state has come: 0 at the change,
    /// 1 once settled, a little past it while the head overshoots.
    pub fn progress(&self) -> f32 {
        self.blend.value() / 100.0
    }

    /// The same, held to `0.0..=1.0`: the strength of the state's prop.
    fn weight(&self) -> f32 {
        self.progress().clamp(0.0, 1.0)
    }

    fn pose(&self, now: f64) -> Pose {
        let to = target(self.state, now, self.elapsed(now), self.rms);
        // A little overshoot is the head settling; more would throw it about.
        let mix = self.progress().clamp(-0.25, 1.25);
        let mut p = self.from.mix(&to, mix);
        // A change of eye shape happens behind a blink.
        if self.eyes_differ(now) {
            p.open *= (2.0 * self.weight() - 1.0).abs();
        }
        p.open = p.open.clamp(0.0, 1.3);
        p.wide = p.wide.clamp(0.5, 1.5);
        p.glow = p.glow.clamp(0.0, 1.0);
        p
    }

    fn eyes_differ(&self, now: f64) -> bool {
        std::mem::discriminant(&self.prev.eyes(now))
            != std::mem::discriminant(&self.state.eyes(now))
    }

    /// Draws the head with its centre at rest on `(cx, cy)` and radius `r`
    /// pixels. Props reach about `1.9 r` to the right, `1.8 r` to the left,
    /// `1.3 r` up and `1.4 r` down. `dark` is the theme (see [`head::draw`]).
    pub fn draw(&self, canvas: &mut Canvas, cx: i32, cy: i32, r: f32, now: f64, dark: bool) {
        let w = self.weight();
        let pose = self.pose(now);
        let eyes = if self.eyes_differ(now) && w < 0.5 {
            self.prev.eyes(now)
        } else {
            self.state.eyes(now)
        };
        // A state without an accent lets the previous one's fade out.
        let accent = match self.state.accent() {
            Accent::None => self.prev.accent(),
            a => a,
        };
        let stage = Stage {
            cx,
            cy,
            r,
            now,
            dark,
            pose,
            rms: self.rms,
        };
        // The shadow stays on the ground: it shrinks and thins as the head
        // rises, and gives way to the keyboard that working puts there.
        let rise = pose.lift.max(-0.1);
        let fading = (self.prev != self.state && w < 1.0).then_some(self.prev);
        let leaving = self.prev_strength * (1.0 - w);
        let keyboard = match (self.state, fading) {
            (MascotState::Working, _) => w,
            (_, Some(MascotState::Working)) => leaving,
            _ => 0.0,
        };
        head::shadow(
            canvas,
            cx + (0.05 * r) as i32,
            cy + (1.2 * r) as i32,
            r * (0.80 - 0.9 * rise),
            r * 0.15,
            (0.55 - 0.9 * rise) * (1.0 - keyboard),
        );
        for front in [false, true] {
            if let Some(prev) = fading {
                stage.prop(canvas, prev, BEAT_SECS, leaving, front);
            }
            stage.prop(canvas, self.state, self.elapsed(now), w, front);
            if !front {
                head::draw(canvas, cx, cy, r, &pose, eyes, accent, dark);
            }
        }
    }
}

/// A packet leaves the edge every this many seconds (fetching).
const PACKET_SECS: f64 = 0.6;
/// A hand strikes every this many seconds (working).
const BEAT_STEP: f64 = 0.5;
/// Where a waiting hand floats, in radii below the head's centre (over the
/// chin), and how far a strike takes it down: onto the keyboard.
const HAND_Y: f32 = 0.76;
const HAND_TRAVEL: f32 = 0.24;
/// The keyboard under the hands: its top edge in radii below the head's
/// centre, its half-width, and the width of one key.
const KEYS_Y: f32 = 1.14;
const KEYS_HALF: f32 = 0.80;
const KEY_PITCH: f32 = 0.105;
/// A line of output arrives every this many seconds (command).
const LINE_SECS: f64 = 0.36;

/// Where and when the props are drawn.
struct Stage {
    cx: i32,
    cy: i32,
    r: f32,
    now: f64,
    dark: bool,
    pose: Pose,
    rms: f32,
}

impl Stage {
    /// A point `(x, y)` head radii from the head's rest centre, in pixels.
    fn at(&self, x: f32, y: f32) -> (f32, f32) {
        (self.cx as f32 + x * self.r, self.cy as f32 + y * self.r)
    }

    /// Draws `state`'s prop at strength `w` (`0..1`, it grows in and shrinks
    /// out), either the part behind the head or the part in front of it.
    fn prop(&self, c: &mut Canvas, state: MascotState, tau: f32, w: f32, front: bool) {
        if w <= 0.02 {
            return;
        }
        let (r, t) = (self.r, self.now);
        match state {
            MascotState::Listening if front => {
                // Sound arriving: arcs close in on both ear pods and fade as
                // they reach them. Louder brings more of them, and bolder.
                let s = phase(t, 0.6);
                let loud = self.rms * 3.2;
                for side in [-1.0f32, 1.0] {
                    let pod = [side * 1.0, 0.06, -0.04];
                    let (px, py, _) = head::project(self.cx, self.cy, r, &self.pose, pod);
                    for k in 0..3 {
                        // Arc spacings out from the pod: each slides one inward per cycle.
                        let pos = (k + 1) as f32 - s;
                        let cover = ((loud + 1.0 - pos) / 0.4).min(pos / 0.5).min(1.0);
                        if cover <= 0.0 {
                            break;
                        }
                        let radius = r * (0.34 + 0.155 * pos);
                        let thick = if pos < loud { 2 } else { 1 };
                        arc(c, px, py, radius, side, 0.62 * w, thick, cover);
                    }
                }
            }
            MascotState::Thinking => {
                // A moon on a tilted orbit: big across the brow in front, then
                // small and high over the crown behind, like a moon setting.
                // Three smaller ones trail it.
                let mut moon = (0.0, 0.0, 0.0);
                for (k, scale) in [1.0f32, 0.5, 0.35, 0.2].into_iter().enumerate() {
                    let a = TAU * phase(t, 2.4) - 0.30 * k as f32;
                    let z = sin(a);
                    let (x, y) = self.at(1.40 * cos(a), -0.84 + 0.14 * z - 0.12 * cos(a));
                    let size = r * (0.16 + 0.05 * z) * scale * w;
                    if k == 0 {
                        moon = (x, y, size);
                    }
                    // Where the orbit doubles back the tail bunches up on
                    // the moon: it goes behind it, not across its face.
                    let (dx, dy, clear) = (x - moon.0, y - moon.1, moon.2 + size);
                    let hidden = k > 0 && dx * dx + dy * dy < clear * clear;
                    if (z >= 0.0) == front && !hidden {
                        head::ball(c, x, y, size, ACCENT_BLUE, self.dark);
                    }
                }
            }
            MascotState::Working if front => {
                // A keyboard on the ground and two hands over it, striking
                // in turn: the key that was hit lights up, and each strike
                // throws off a pair of ticks.
                let left = (t / BEAT_STEP).floor() as i64 % 2 == 0;
                let b = phase(t, BEAT_STEP);
                let key = ((KEY_PITCH * r).round() as i32).max(3);
                // It grows from the middle, a key at a time; a stub of one
                // or two keys is not drawn.
                let keys = (2.0 * KEYS_HALF * r * w) as i32 / key;
                let keys = if keys < 3 { 0 } else { keys };
                let (kx, ky) = self.at(0.0, KEYS_Y);
                let (kx, ky) = (kx as i32 - (keys * key + 1) / 2, ky as i32);
                if keys > 0 {
                    c.fill_rect(kx, ky, keys * key + 1, 2 * key + 1, INK);
                }
                // Two rows of key caps a pixel apart, the lower row set half
                // a key along (its last cap cut off at the case).
                for i in 0..keys {
                    let x = kx + 1 + i * key;
                    c.fill_rect(x, ky + 1, key - 1, key - 1, PAPER);
                    let x = x + key / 2;
                    let cap = (key - 1).min(kx + keys * key - x);
                    c.fill_rect(x, ky + 1 + key, cap, key - 1, PAPER);
                }
                for side in [-1.0f32, 1.0] {
                    let striking = (side < 0.0) == left;
                    let down = if striking { (1.0 - b) * (1.0 - b) } else { 0.0 };
                    let (x, y) = self.at(side * 0.46, HAND_Y + HAND_TRAVEL * down);
                    let hand = r * 0.17 * w;
                    let i = (x as i32 - kx).div_euclid(key);
                    if striking && b < 0.4 && (0..keys).contains(&i) {
                        c.fill_rect(kx + 1 + i * key, ky + 1, key - 1, key - 1, ACCENT_BLUE);
                    }
                    head::ball(c, x, y, hand, ACCENT_BLUE, self.dark);
                    // 100 ms: long enough for a 12 FPS frame to land in it.
                    if striking && b < 0.2 && w > 0.5 {
                        for out in [-1.0f32, 1.0] {
                            let (dx, dy) = (0.82 * out, -0.57);
                            let (near, far) = (hand + 2.0, hand + 2.0 + 2.0 * w);
                            line(
                                c,
                                x + near * dx,
                                y + near * dy,
                                x + far * dx,
                                y + far * dy,
                                ACCENT_BLUE,
                            );
                        }
                    }
                }
            }
            MascotState::Fetching if front => {
                // Packets fly in from the right, growing; each shrinks away
                // just short of the head, which swallows it (see the gulp).
                for k in 0..3 {
                    let p = phase(t + k as f64 * PACKET_SECS, 3.0 * PACKET_SECS);
                    let e = p * p;
                    let (x, y) = self.at(1.80 - 0.56 * e, -0.34 + 0.28 * e);
                    let gone = ((1.0 - p) / 0.15).min(1.0);
                    let size = r * (0.045 + 0.085 * e).min(0.10) * gone * w;
                    square(c, x, y, size, ACCENT_BLUE);
                }
            }
            MascotState::Command if front => {
                // Output lines scrolling up beside the head; the newest is typed in blue.
                let (n, f) = ((t / LINE_SECS).floor() as u64, phase(t, LINE_SECS));
                for row in 0..5 {
                    let (x, y) = self.at(1.44, -0.52 + 0.25 * (row as f32 - f));
                    let len = r * (0.16 + 0.28 * hash01((n as u32).wrapping_add(row))) * w;
                    let (x, y) = (x as i32, y as i32);
                    match row {
                        // The top line dissolves as it leaves.
                        0 => {
                            for dx in 0..len as i32 {
                                if dither(x + dx, y, 1.0 - f) {
                                    c.fill_rect(x + dx, y, 1, 2, INK);
                                }
                            }
                        }
                        4 => c.fill_rect(x, y, (len * f) as i32, 2, ACCENT_BLUE),
                        _ => c.fill_rect(x, y, len as i32, 2, INK),
                    }
                }
            }
            MascotState::WaitingApproval if front => {
                // An amber speech bubble with a question mark, bobbing.
                let (x, y) = self.at(1.42, -0.80 + 0.04 * wave(t, 0.8));
                let (hw, hh) = ((r * 0.36 * w) as i32, (r * 0.30 * w) as i32);
                let (x, y) = (x as i32, y as i32);
                c.fill_rect(x - hw + 2, y - hh, 2 * hw - 4, 2 * hh, ACCENT_AMBER);
                c.fill_rect(x - hw, y - hh + 2, 2 * hw, 2 * hh - 4, ACCENT_AMBER);
                c.fill_rect(x - hw + 1, y - hh + 1, 2 * hw - 2, 2 * hh - 2, ACCENT_AMBER);
                // The tail points down-left at the head.
                for i in 0..(hh / 2 + 1) {
                    c.fill_rect(
                        x - hw + 3 - i / 2,
                        y + hh + i,
                        hh / 2 + 1 - i,
                        1,
                        ACCENT_AMBER,
                    );
                }
                if w > 0.8 {
                    // Dark on amber in both themes: the dark theme's swap
                    // turns paper into ink.
                    let mark = if self.dark { PAPER } else { INK };
                    draw_text(c, x - 4, y - 7, "?", Font::F8x14, mark);
                }
            }
            MascotState::Paused if front => {
                // A pause mark that never leaves: solid, then hollow for a
                // moment, the way a paused tape deck blinks.
                let (x, y) = self.at(1.18, -0.98);
                let (bw, bh) = ((r * 0.10) as i32 + 1, (r * 0.34 * w) as i32);
                let hollow = phase(t, 1.6) >= 0.7;
                for dx in [0, 2 * bw] {
                    let (bx, by) = (x as i32 + dx, y as i32 - bh / 2);
                    if hollow {
                        c.draw_rect(bx, by, bw, bh, INK);
                    } else {
                        c.fill_rect(bx, by, bw, bh, INK);
                    }
                }
            }
            MascotState::Done if front => {
                // Six rays bursting outward for 0.7 s. None points straight
                // up, where the hop would carry it into the status bar.
                let e = (tau / 0.7).min(1.0);
                let out = 1.0 - (1.0 - e) * (1.0 - e);
                for turn in [0.03f32, 0.97, 1.14, 1.31, 1.69, 1.86] {
                    if e >= 1.0 {
                        break;
                    }
                    let a = PI * turn;
                    let (r1, r2) = (1.24 + 0.30 * out, 1.24 + 0.30 * out + 0.24 * (1.0 - e) * w);
                    let (x1, y1) = self.at(r1 * cos(a), r1 * sin(a) * 0.9);
                    let (x2, y2) = self.at(r2 * cos(a), r2 * sin(a) * 0.9);
                    line(c, x1, y1, x2, y2, ACCENT_BLUE);
                }
            }
            MascotState::Error if front => {
                // An amber `!` that shakes with the head.
                let fade = (1.0 - tau / 0.9).max(0.0);
                let (x, y) = self.at(1.22 + 0.06 * fade * sin(TAU * tau / 0.15), -0.96);
                let (bw, bh) = ((r * 0.11) as i32 + 1, (r * 0.36 * w) as i32);
                let (x, y) = (x as i32, y as i32);
                c.fill_rect(x, y - bh / 2, bw, bh, ACCENT_AMBER);
                c.fill_rect(x, y + bh / 2 + 2, bw, bw, ACCENT_AMBER);
            }
            _ => {}
        }
    }
}

/// A filled square of half-size `half` pixels centred on `(x, y)`.
fn square(c: &mut Canvas, x: f32, y: f32, half: f32, color: u32) {
    let s = (2.0 * half).round() as i32;
    if s > 0 {
        c.fill_rect(
            (x - half).round() as i32,
            (y - half).round() as i32,
            s,
            s,
            color,
        );
    }
}

/// A 2px line from `(x1, y1)` to `(x2, y2)`.
fn line(c: &mut Canvas, x1: f32, y1: f32, x2: f32, y2: f32, color: u32) {
    let n = (x2 - x1).abs().max((y2 - y1).abs()).ceil().max(1.0);
    for i in 0..=n as i32 {
        let f = i as f32 / n;
        let (x, y) = (x1 + (x2 - x1) * f, y1 + (y2 - y1) * f);
        c.fill_rect(x.round() as i32, y.round() as i32, 2, 2, color);
    }
}

/// An ink arc of `radius` pixels around `(cx, cy)`, opening toward `side`
/// (-1 left, +1 right) and spanning `half` radians either way, dithered to
/// `cover` (`0.0..=1.0`).
#[allow(clippy::too_many_arguments)]
fn arc(
    c: &mut Canvas,
    cx: f32,
    cy: f32,
    radius: f32,
    side: f32,
    half: f32,
    thick: i32,
    cover: f32,
) {
    let n = (radius * half * 2.0).ceil().max(1.0);
    for i in 0..=n as i32 {
        let a = half * (2.0 * i as f32 / n - 1.0);
        let (x, y) = (cx + side * radius * cos(a), cy + radius * sin(a));
        let (x, y) = (x.round() as i32, y.round() as i32);
        if dither(x, y, cover) {
            c.fill_rect(x, y, thick, thick, INK);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::canvas::{PAPER, WIDTH};

    #[test]
    fn labels_and_names_are_distinct_and_short() {
        for (i, s) in MascotState::ALL.into_iter().enumerate() {
            assert!(!s.label().is_empty() && s.label().len() <= 6, "{s:?}");
            assert_eq!(MascotState::from_name(s.name()), Some(s));
            for other in &MascotState::ALL[..i] {
                assert_ne!(s.label(), other.label());
            }
        }
        assert_eq!(MascotState::from_name("nope"), None);
    }

    #[test]
    fn daemon_signals_map_onto_states() {
        use MascotState::*;
        for (agent, state) in [
            ("IDLE", Idle),
            ("LISTENING", Listening),
            ("THINKING", Thinking),
            ("ACTING", Working),
            ("WAITING_APPROVAL", WaitingApproval),
            ("PAUSED", Paused),
        ] {
            assert_eq!(MascotState::from_agent(agent), Some(state), "{agent}");
        }
        assert_eq!(MascotState::from_agent("SLEEPING"), None);

        for (kind, state) in [
            ("screenshot", Fetching),
            ("wait", Fetching),
            ("ssh_exec", Command),
            ("agent_run", Command),
            ("vnc_click", Working),
            ("vnc_move", Working),
            ("vnc_drag", Working),
            ("vnc_scroll", Working),
            ("type_text", Working),
            ("vnc_key", Working),
            ("nudge", Working),
            ("some_future_tool", Working),
        ] {
            assert_eq!(MascotState::from_activity(kind), Some(state), "{kind}");
        }
        assert_eq!(MascotState::from_activity("thought"), None);
        assert_eq!(MascotState::from_activity("prompt"), None);

        assert_eq!(MascotState::from_finish("done"), Some(Done));
        assert_eq!(MascotState::from_finish("failed"), Some(Error));
        assert_eq!(MascotState::from_finish("stopped"), Some(Error));
        assert_eq!(MascotState::from_finish("cancelled"), None);
    }

    #[test]
    fn resolve_refines_the_agent_state() {
        use MascotState::*;
        let r = MascotState::resolve;
        // Acting shows the step on show, plain work when there is none.
        assert_eq!(r(Working, None, None), Working);
        assert_eq!(r(Working, Some(Command), None), Command);
        assert_eq!(r(Working, Some(Fetching), None), Fetching);
        assert_eq!(r(Working, Some(Working), None), Working);
        // Thinking keeps a held step on screen, then thinks.
        assert_eq!(r(Thinking, Some(Fetching), None), Fetching);
        assert_eq!(r(Thinking, Some(Working), None), Working);
        assert_eq!(r(Thinking, None, None), Thinking);
        // A beat only plays over idle, and a step never does.
        assert_eq!(r(Idle, None, Some(Done)), Done);
        assert_eq!(r(Idle, Some(Command), Some(Error)), Error);
        assert_eq!(r(Idle, Some(Command), None), Idle);
        assert_eq!(r(Thinking, None, Some(Done)), Thinking);
        assert_eq!(r(Working, None, Some(Error)), Working);
        // Approval, pause and listening are never overridden.
        for base in [WaitingApproval, Paused, Listening] {
            assert_eq!(r(base, Some(Command), Some(Done)), base);
        }
    }

    /// Paper as the canvas stores it.
    const PAPER_RGB: [u8; 3] = [(PAPER >> 16) as u8, (PAPER >> 8) as u8, PAPER as u8];

    /// FNV-1a over the frame, to compare renders cheaply.
    fn digest(c: &Canvas) -> u64 {
        c.pixels().iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
            (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
        })
    }

    fn frame(state: MascotState, now: f64, dark: bool) -> Canvas {
        let mut m = Mascot::new();
        m.snap(state);
        m.rms = 0.5;
        let mut c = Canvas::new();
        m.draw(&mut c, 75, 66, 35.0, now, dark);
        c
    }

    #[test]
    fn frames_are_deterministic_and_states_differ() {
        let mut seen = Vec::new();
        for state in MascotState::ALL {
            let a = digest(&frame(state, 0.2, false));
            assert_eq!(a, digest(&frame(state, 0.2, false)), "{state:?}");
            assert!(!seen.contains(&a), "{state:?} draws like another state");
            seen.push(a);
            // Time moves every state except the paused one's slow breath.
            if state != MascotState::Paused {
                assert_ne!(a, digest(&frame(state, 1.0, false)), "{state:?}");
            }
        }
    }

    /// The pixels themselves, pinned: every state at two moments in both
    /// themes. A change here means the head looks different (look at fresh
    /// dumps, then update the number) or that this target evaluates the
    /// float maths differently, which it should not.
    #[test]
    fn rendered_frames_are_pinned() {
        let mut all = 0xcbf2_9ce4_8422_2325u64;
        for state in MascotState::ALL {
            for now in [0.2, 1.0] {
                for dark in [false, true] {
                    let frame = digest(&frame(state, now, dark));
                    all = (all ^ frame).wrapping_mul(0x0100_0000_01b3);
                }
            }
        }
        assert_eq!(all, 0x3bb5_8923_5e75_5f01, "digest {all:#018x}");
    }

    #[test]
    fn only_palette_colours_are_drawn_in_both_themes() {
        let palette = [INK, PAPER, ACCENT_BLUE, ACCENT_AMBER];
        for dark in [false, true] {
            for state in MascotState::ALL {
                for step in 0..8 {
                    let c = frame(state, f64::from(step) * 0.21, dark);
                    for px in c.pixels().chunks_exact(3) {
                        let rgb = u32::from(px[0]) << 16 | u32::from(px[1]) << 8 | u32::from(px[2]);
                        assert!(palette.contains(&rgb), "{state:?} drew {rgb:06x}");
                    }
                }
            }
        }
        // Light theme: paper shell lit from the upper left, ink glass.
        let c = frame(MascotState::Paused, 1.0, false);
        assert_eq!(c.get_pixel(60, 42), Some(PAPER));
        assert_eq!(c.get_pixel(75, 75), Some(INK));
        // The dark theme draws the same head pre-swapped, so the frame-wide
        // ink/paper swap leaves it lit from the same side.
        let d = frame(MascotState::Paused, 1.0, true);
        assert_eq!(d.get_pixel(60, 42), Some(INK));
        assert_eq!(d.get_pixel(75, 75), Some(PAPER));
    }

    #[test]
    fn drawing_clips_at_every_edge() {
        for state in MascotState::ALL {
            let mut m = Mascot::new();
            m.snap(state);
            let mut c = Canvas::new();
            for (x, y) in [(-20, -20), (330, 180), (0, 170), (320, 0)] {
                m.draw(&mut c, x, y, 35.0, 0.4, false);
            }
            m.draw(&mut c, 160, 85, 4.0, 0.4, false);
            m.draw(&mut c, 160, 85, 90.0, 0.4, true);
        }
    }

    #[test]
    fn a_state_change_eases_and_then_settles() {
        let mut m = Mascot::new();
        m.set_state(MascotState::Fetching, 10.0);
        assert!(m.is_moving(10.0));
        let start = m.pose(10.0);
        assert!(start.yaw.abs() < 0.35, "starts from the idle pose");
        let mut now = 10.0;
        for _ in 0..90 {
            now += 1.0 / 60.0;
            m.tick(1.0 / 60.0);
            let mut c = Canvas::new();
            m.draw(&mut c, 75, 66, 35.0, now, false);
        }
        assert!(!m.is_moving(now), "settled after 1.5 s");
        assert_eq!(m.progress(), 1.0, "at rest means exactly at rest");
        assert!(m.pose(now).yaw > 0.45, "turned to look out");
        // A beat keeps full-rate frames for its first second only.
        m.set_state(MascotState::Done, now);
        m.settle();
        assert!(m.is_moving(now + 0.5) && !m.is_moving(now + 1.2));
        assert!(m.elapsed(now + f64::from(BEAT_SECS)) >= BEAT_SECS);
    }

    #[test]
    fn the_easing_is_stable_at_every_frame_rate() {
        // The UI steps this at 60 FPS only while the head is watched: on
        // another view, or with the backlight off, frames are 83 ms to a
        // (clamped) 100 ms apart, more than the spring's integrator can take
        // in one step.
        for dt in [1.0 / 60.0, 0.0853, 0.1] {
            let mut m = Mascot::new();
            m.set_state(MascotState::Thinking, 3.0);
            let mut peak = 0.0f32;
            for _ in 0..(60.0 / dt) as usize {
                m.tick(dt);
                assert!(m.progress().is_finite(), "dt {dt}");
                peak = peak.max(m.progress());
            }
            assert!(peak < 1.1, "dt {dt}: overshoots to {peak}");
            assert_eq!(m.progress(), 1.0, "dt {dt}: not settled");
            assert!(!m.is_moving(63.0), "dt {dt}");
            // ... and it stays at rest however slowly it is stepped afterwards.
            for _ in 0..600 {
                m.tick(0.1);
                assert!(!m.is_moving(63.0), "dt {dt}: stirred up again");
            }
        }
        // A step of no time at all, or of less, changes nothing.
        let mut m = Mascot::new();
        m.set_state(MascotState::Working, 0.0);
        for dt in [0.0, -1.0] {
            m.tick(dt);
            assert_eq!(m.progress(), 0.0);
        }
    }

    #[test]
    fn the_easing_takes_whatever_steps_the_loop_hands_it() {
        // The live loops pass on however long the last frame took: a key or a
        // daemon frame wakes them after a millisecond, an idle screen waits a
        // second, a busy SD card longer. Walk one head through two minutes of
        // state changes with such steps, the degenerate ones included, and
        // look at every frame.
        let steps = [
            1.0 / 60.0,
            0.001,
            0.0853,
            0.0,
            0.1,
            1.0,
            0.25,
            0.004,
            f32::INFINITY,
            -0.5,
            1.0 / 12.0,
            f32::NAN,
        ];
        let mut m = Mascot::new();
        let mut c = Canvas::new();
        let (mut now, mut quiet, mut last) = (0.0f64, 0.0f32, 1.0f32);
        let mut changes = 0;
        for n in 0u32..2400 {
            // Irregular: the table above, stretched by a hash so that no two
            // rounds are alike.
            let dt = steps[n as usize % steps.len()] * (0.5 + hash01(n));
            if dt.is_finite() && dt > 0.0 {
                now += f64::from(dt);
            }
            m.tick(dt);
            // (No time, negative time and nonsense advance nothing.)
            quiet += if dt.is_nan() { 0.0 } else { dt.clamp(0.0, 0.1) };

            let w = m.progress();
            assert!((0.0..1.06).contains(&w), "frame {n}: blend at {w}");
            assert!(w > last - 0.04, "frame {n}: fell back from {last} to {w}");
            last = w;
            // However it was stepped, 1.2 s of easing is enough to arrive, and
            // arriving means exactly at rest.
            if quiet >= 1.2 {
                assert_eq!(w, 1.0, "frame {n}: {quiet} s after a change");
                assert!(m.state().is_beat() || !m.is_moving(now), "frame {n}");
            }

            // A change comes at a moment of its own choosing, often in the
            // middle of the last one's easing.
            if hash01(n ^ 0x51ed) < 0.02 || (quiet > 1.5 && hash01(n) < 0.2) {
                let next = MascotState::ALL[(hash01(n ^ 0xa3) * 10.0) as usize % 10];
                if next != m.state() {
                    let before = m.pose(now);
                    m.set_state(next, now);
                    // The frame of the change shows the head where it was.
                    assert_eq!(m.pose(now), before, "frame {n}: jumped on the change");
                    assert_eq!(m.progress(), 0.0);
                    (quiet, last, changes) = (0.0, 0.0, changes + 1);
                }
            }

            // The head is on screen, whole, and nowhere it should not be.
            let pose = m.pose(now);
            for v in [pose.yaw, pose.pitch, pose.roll, pose.lift] {
                assert!(v.abs() < 0.8, "frame {n}: pose {pose:?}");
            }
            // (Drawn every third frame, and every frame of a change's first
            // quarter second: an unoptimised build takes its time over it.)
            if n % 3 != 0 && quiet > 0.25 {
                continue;
            }
            c.fill_rect(0, 0, 150, 125, PAPER);
            m.draw(&mut c, 75, 66, 35.0, now, false);
            // A head that has left takes all but its shadow with it: some
            // 250 pixels, against 1800 and more.
            let row = |y: usize| &c.pixels()[y * WIDTH as usize * 3..][..150 * 3];
            let drawn = |y: usize| row(y).chunks_exact(3).filter(|px| *px != PAPER_RGB).count();
            let head: usize = (20..120).map(drawn).sum();
            assert!(head > 1200, "frame {n}: {head} pixels left of the head");
            let above: usize = (0..14).map(drawn).sum();
            assert_eq!(above, 0, "frame {n}: drawn over the status bar");
        }
        assert!(changes > 40, "{changes} changes");
    }

    #[test]
    fn a_wild_blend_cannot_throw_the_head() {
        let mut m = Mascot::new();
        m.snap(MascotState::Idle);
        m.set_state(MascotState::Fetching, 1.0);
        m.blend.set_position(125.0);
        let limit = m.pose(1.5);
        for wild in [500.0, 1e9] {
            m.blend.set_position(wild);
            assert_eq!(m.pose(1.5), limit);
        }
        m.blend.set_position(-1e9);
        assert!(m.pose(1.5).yaw.abs() < 0.3);
        // The next change starts from rest whatever speed the last one had.
        m.blend.set_position(50.0);
        m.tick(1.0 / 60.0);
        assert!(m.blend.velocity() > 1.0);
        m.set_state(MascotState::Working, 2.0);
        assert_eq!((m.blend.value(), m.blend.velocity()), (0.0, 0.0));
    }

    #[test]
    fn an_interrupted_prop_fades_from_where_it_was() {
        // Thinking, then a step 150 ms later: the moon was about a third
        // grown, and must shrink from there instead of popping to full size.
        let mut m = Mascot::new();
        m.set_state(MascotState::Thinking, 1.0);
        for _ in 0..9 {
            m.tick(1.0 / 60.0);
        }
        let grown = m.weight();
        assert!((0.1..0.8).contains(&grown), "{grown}");
        m.set_state(MascotState::Fetching, 1.15);
        assert_eq!(m.prev_strength, grown);
        // A settled state leaves at full strength; a pinned one is whole.
        for _ in 0..120 {
            m.tick(1.0 / 60.0);
        }
        m.set_state(MascotState::Working, 4.0);
        assert_eq!(m.prev_strength, 1.0);
        m.snap(MascotState::Paused);
        assert_eq!(m.prev_strength, 1.0);
    }

    #[test]
    fn the_clock_keeps_its_resolution_for_weeks() {
        // Thirty days in, a 60 FPS step must still move the animation: the
        // phases are taken from the `f64` clock before anything is narrowed.
        let month = 30.0 * 86_400.0;
        let step = 1.0 / 60.0;
        assert!((phase(month + step, 0.5) - phase(month, 0.5) - 1.0 / 30.0).abs() < 1e-4);
        let mut m = Mascot::new();
        m.set_state(MascotState::Done, month);
        assert!((m.elapsed(month + step) - 1.0 / 60.0).abs() < 1e-6);
        m.snap(MascotState::Working);
        let (mut a, mut b) = (Canvas::new(), Canvas::new());
        m.draw(&mut a, 75, 66, 35.0, month, false);
        m.draw(&mut b, 75, 66, 35.0, month + 0.1, false);
        assert_ne!(digest(&a), digest(&b));
        // The idle schedule still wakes for a blink at the right moment.
        let blink_at = blink_start(month, 1.0);
        assert!(idle_wait(blink_at - 0.05) < 0.051);
        assert_eq!(idle_wait(blink_at + 0.01), 0.0);
    }

    #[test]
    fn idle_events_are_scheduled_and_never_missed() {
        // Walking the idle timeline with the governor's own waits must land
        // inside every blink: eyes are seen shut at least once per slot.
        let (mut t, mut shut, mut frames) = (0.0f64, 0, 0);
        while t < 10.0 * BLINK_SLOT {
            if blink(t) < 0.5 {
                shut += 1;
            }
            frames += 1;
            t += idle_wait(t).clamp(1.0 / 12.0, IDLE_FRAME_SECS);
        }
        assert!(shut >= 10, "{shut} shut frames in 10 slots");
        // ... while staying close to the 4 FPS the idle sway is drawn at.
        assert!(frames < 56 * 5, "{frames} frames in 56 s");
        // Glances go both ways and come back to centre.
        let sides: Vec<f32> = (0..12)
            .map(|s| glance_start(f64::from(s) * GLANCE_SLOT, 0.0).1)
            .collect();
        assert!(sides.contains(&-1.0) && sides.contains(&1.0));
        let (start, _) = glance_start(0.0, 0.0);
        assert_eq!(glance(start - 0.01), 0.0);
        assert!(glance(start + GLANCE_SECS / 2.0).abs() > 0.99);
    }
}
