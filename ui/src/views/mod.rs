//! The six core views and the stateful `App` orchestrator (Phase 1).
//!
//! Phase 1 renders from synthetic state (IPC arrives in Phase 2); every view is
//! deterministic given `now` so `--dump-frame` snapshots are reproducible.

use std::time::{Duration, Instant};

use crate::gfx::canvas::{Canvas, ACCENT_AMBER, ACCENT_BLUE, INK, PAPER, WIDTH};
use crate::gfx::font::{draw_text, Font};
use crate::gfx::mascot::{MascotAnim, MascotState};
use crate::gfx::spring::Spring;

pub mod approval;
pub mod companion;
pub mod menu;
pub mod prompt;
pub mod setup;
pub mod ticker;

/// Status bar height in pixels (rows `0..17`); the 1px rule sits at [`RULE_Y`].
pub const STATUS_BAR_H: i32 = 18;
/// Row of the 1px rule separating the status bar from content.
pub const RULE_Y: i32 = 18;
/// Top row of view content (below the rule).
pub const CONTENT_Y: i32 = 19;
/// Height available to a view below the status bar.
pub const CONTENT_H: i32 = crate::gfx::canvas::HEIGHT - CONTENT_Y;

/// The six core views (SPEC 5.3), in cycle order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// Ambient agent face + desk clock (default home).
    Companion,
    /// Live activity / tool-call feed.
    Ticker,
    /// Quick action & system menu.
    Menu,
    /// Tool approval modal.
    Approval,
    /// Keyboard + push-to-talk prompt overlay.
    Prompt,
    /// First-boot / network & Tailscale onboarding.
    Setup,
}

impl View {
    /// All views in cycle order.
    pub const ALL: [View; 6] = [
        View::Companion,
        View::Ticker,
        View::Menu,
        View::Approval,
        View::Prompt,
        View::Setup,
    ];

    /// Parses a CLI view name (e.g. `"companion"`).
    pub fn from_name(name: &str) -> Option<View> {
        View::ALL.into_iter().find(|v| v.name() == name)
    }

    /// Lowercase CLI name of the view.
    pub fn name(self) -> &'static str {
        match self {
            View::Companion => "companion",
            View::Ticker => "ticker",
            View::Menu => "menu",
            View::Approval => "approval",
            View::Prompt => "prompt",
            View::Setup => "setup",
        }
    }

    /// The view `delta` steps away in [`View::ALL`], wrapping around.
    pub fn cycle(self, delta: i32) -> View {
        let n = View::ALL.len() as i32;
        let i = View::ALL.iter().position(|v| *v == self).unwrap_or(0) as i32;
        View::ALL[(i + delta).rem_euclid(n) as usize]
    }
}

/// Persistent status-bar values (SPEC 5.3 top bar).
pub struct Header {
    pub state: MascotState,
    pub host: &'static str,
    pub tailscale: bool,
    /// Minutes since midnight (clamped to one day).
    pub minutes: u16,
    pub battery: u8,
}

impl Default for Header {
    fn default() -> Self {
        Self {
            state: MascotState::Idle,
            host: "macbook-pro",
            tailscale: true,
            minutes: 14 * 60 + 32,
            battery: 84,
        }
    }
}

/// Orchestrates a whole Phase 1 session: springs, view slide, mascot, inputs.
pub struct App {
    view: View,
    /// Horizontal slide offset in pixels (0 at rest).
    slide: Spring,
    /// Captured previous frame + direction while a slide is running.
    prev: Canvas,
    slide_dir: i32,
    sliding: bool,
    /// Reusable scratch frame (persistent status bar + current view content).
    scratch: Canvas,
    /// Seconds since epoch-ish; drives deterministic animation + desk clock.
    now: f32,
    header: Header,
    mascot_anim: MascotAnim,
    ticker: ticker::TickerView,
    menu: menu::MenuView,
    prompt: prompt::PromptView,
    setup: setup::SetupView,
    approval: Option<approval::Approval>,
    last_input: Instant,
}

/// Fixed desk-clock pin for `--dump-frame` snapshots (14:32).
pub const DUMP_CLOCK: u16 = 14 * 60 + 32;

impl App {
    /// A fresh session with synthetic demo state.
    pub fn new() -> Self {
        Self {
            view: View::Companion,
            slide: Spring::slide(),
            prev: Canvas::new(),
            slide_dir: 1,
            sliding: false,
            scratch: Canvas::new(),
            now: 0.0,
            header: Header::default(),
            mascot_anim: MascotAnim {
                breath: 0.0,
                blink: false,
                rms: 0.0,
                scan: 0.0,
                orbit: 0.0,
            },
            ticker: ticker::TickerView::demo(),
            menu: menu::MenuView::demo(),
            prompt: prompt::PromptView::new(),
            setup: setup::SetupView::demo(),
            approval: None,
            last_input: Instant::now(),
        }
    }

    /// Jumps straight to `view` with no slide (used by `--dump-frame`).
    pub fn set_view(&mut self, view: View) {
        self.view = view;
        self.slide.snap_to(0.0);
        self.sliding = false;
    }

    /// Starts a horizontal spring slide to `self.view.cycle(delta)`.
    pub fn cycle(&mut self, delta: i32) {
        if self.sliding {
            self.slide.snap_to(0.0);
        }
        // Capture the current (pre-switch) frame as the outgoing one.
        let mut prev = Canvas::new();
        self.render_full(&mut prev);
        self.prev = prev;
        self.slide_dir = if delta >= 0 { 1 } else { -1 };
        self.view = self.view.cycle(delta);
        self.slide.snap_to(0.0);
        self.slide.set_position(-(self.slide_dir * WIDTH) as f32);
        self.sliding = true;
    }

    /// Momentarily selects a slide target and lets it animate.
    pub fn set_view_slide(&mut self, delta: i32) {
        self.cycle(delta);
    }

    /// Shows a synthetic high-risk approval modal (used by `--demo`/dumps).
    pub fn show_synthetic_approval(&mut self) {
        self.approval = Some(approval::Approval::synthetic());
        self.header.state = MascotState::WaitingApproval;
    }

    /// Snaps every spring to rest for a deterministic `--dump-frame` snapshot.
    pub fn settle_all(&mut self) {
        self.slide.snap_to(0.0);
        self.sliding = false;
        self.menu.pill.snap_to(self.menu.index as f32);
        self.ticker.settle();
        if let Some(a) = self.approval.as_mut() {
            a.settle();
        }
    }

    /// Routes one semantic key. Returns `true` if it was consumed.
    pub fn key(&mut self, key: crate::backend::LapisKey) -> bool {
        use crate::backend::LapisKey as K;
        self.last_input = Instant::now();
        match key {
            K::CycleL => {
                self.cycle(-1);
                true
            }
            K::CycleR => {
                self.cycle(1);
                true
            }
            K::Up => {
                self.menu.move_sel(-1);
                true
            }
            K::Down => {
                self.menu.move_sel(1);
                true
            }
            K::Tab => {
                self.menu.cycle_host();
                self.header.host = self.menu.host();
                true
            }
            K::Enter => {
                if let Some(a) = self.approval.as_mut() {
                    a.finish("approve_once");
                } else if self.view == View::Prompt {
                    self.prompt.dispatch = true;
                } else if self.view == View::Menu {
                    self.menu_select();
                }
                true
            }
            K::Esc => {
                if let Some(_a) = self.approval.take() {
                    // Deny: modal dismissed.
                } else if self.view == View::Prompt {
                    self.prompt.clear();
                    self.set_view(View::Companion);
                } else {
                    self.set_view(View::Companion);
                }
                true
            }
            K::Char(c) => {
                if self.view == View::Prompt {
                    self.prompt.push(c);
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    fn menu_select(&mut self) {
        // "New Task / Prompt" is the first row.
        if self.menu.index == 0 {
            self.set_view(View::Prompt);
        }
    }

    /// Advances all springs and animation inputs by `dt` seconds at time `now`.
    pub fn tick(&mut self, dt: f32, now: f32) {
        self.now = now;
        self.slide.tick(dt);
        self.menu.pill.tick(dt);
        self.menu.pill.set_target(self.menu.index as f32);
        if let Some(a) = self.approval.as_mut() {
            a.pop.tick(dt);
            a.pop.set_target(0.0); // slide up to rest
        }
        self.ticker.tick(dt);
        let m = self.mascot_anim_for(now);
        self.mascot_anim = m;
    }

    /// Deterministic animation inputs for `now`.
    fn mascot_anim_for(&self, now: f32) -> MascotAnim {
        let period = |freq: f32| (now * freq * std::f32::consts::TAU).sin();
        MascotAnim {
            breath: period(0.4),
            blink: (now * 0.3).sin() > 0.94,
            rms: ((now * 3.0).sin() * 0.5 + 0.5).powi(2),
            scan: period(0.15),
            orbit: (now * 0.25).fract(),
        }
    }

    /// Demo-mode: cycles mascot states and stages a synthetic approval modal.
    pub fn demo_tick(&mut self, now: f32) {
        let states = [
            MascotState::Idle,
            MascotState::Listening,
            MascotState::Thinking,
            MascotState::Acting,
            MascotState::WaitingApproval,
        ];
        self.header.state = states[((now / 2.5) as usize) % states.len()];
        let show_approval = ((now / 5.0) as usize) % 2 == 1 && self.approval.is_none();
        if show_approval {
            self.show_synthetic_approval();
        }
    }

    /// Adaptive frame interval (SPEC 5.2): 60 FPS animating, 12 agent-active, 1 idle.
    pub fn frame_interval(&self) -> Duration {
        let input_recent = self.last_input.elapsed() < Duration::from_secs(2);
        let animating = self.sliding
            || self.slide.is_active()
            || self.menu.pill.is_active()
            || self.approval.as_ref().is_some_and(|a| a.pop.is_active())
            || self.ticker.is_active();
        if animating || input_recent {
            return Duration::from_micros(16_667);
        }
        match self.header.state {
            MascotState::Idle => Duration::from_millis(1000),
            _ => Duration::from_micros(83_333),
        }
    }

    /// Renders the full 320x170 frame for the current view with any slide offset.
    pub fn render(&mut self, canvas: &mut Canvas) {
        canvas.clear(PAPER);
        let mut scratch = Canvas::new();
        self.render_full(&mut scratch);
        self.scratch = scratch;
        if self.sliding {
            let off = self.slide.value().round() as i32;
            canvas.blit(&self.scratch, off, 0);
            canvas.blit(&self.prev, off + self.slide_dir * WIDTH, 0);
            if self.slide.settled() {
                self.sliding = false;
            }
        } else {
            canvas.blit(&self.scratch, 0, 0);
        }
    }

    /// Renders status bar + the current view's content into `canvas`.
    fn render_full(&self, canvas: &mut Canvas) {
        draw_status_bar(canvas, &self.header);
        match self.view {
            View::Companion => companion::render(canvas, &self.header, &self.mascot_anim, self.now),
            View::Ticker => ticker::render(canvas, &self.ticker),
            View::Menu => menu::render(canvas, &self.menu),
            View::Approval => approval::render(
                canvas,
                self.approval
                    .as_ref()
                    .unwrap_or(&approval::Approval::empty()),
            ),
            View::Prompt => prompt::render(canvas, &self.prompt, self.now),
            View::Setup => setup::render(canvas, &self.setup),
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

/// Draws `[● STATE] [HOST: x] [TS] [HH:MM] [NN%]` and the rule.
///
/// All text is built in fixed stack buffers so the hot render path never allocates.
fn draw_status_bar(canvas: &mut Canvas, h: &Header) {
    let ty = (STATUS_BAR_H - 8) / 2 + 1;
    // State pip: accent by state.
    let pip = match h.state {
        MascotState::WaitingApproval => ACCENT_AMBER,
        MascotState::Acting | MascotState::Thinking => ACCENT_BLUE,
        _ => INK,
    };
    canvas.fill_rect(4, ty + 2, 3, 3, pip);

    // `[STATE]` (labels up to 7 chars + brackets fit in 60px).
    draw_text(canvas, 10, ty, h.state.label(), Font::F6x8, INK);

    // Host label built into a stack buffer.
    let host_x = 52;
    let mut host_buf = [0u8; 24];
    {
        let host = fmt_into(&mut host_buf, &["HOST:", h.host]);
        draw_text(canvas, host_x, ty, host, Font::F6x8, INK);
    }

    // Tailscale flag.
    let ts_x = host_x + 96;
    draw_text(
        canvas,
        ts_x,
        ty,
        if h.tailscale { "TS" } else { "--" },
        Font::F6x8,
        INK,
    );
    canvas.fill_rect(
        ts_x + 14,
        ty + 2,
        4,
        4,
        if h.tailscale { INK } else { ACCENT_AMBER },
    );

    // Right-aligned battery then clock.
    let right = WIDTH - 4;
    let mut pct_buf = [0u8; 4];
    let pct = fmt_pct(&mut pct_buf, h.battery);
    let pct_x = right - Font::F6x8.text_width(pct);
    draw_text(canvas, pct_x, ty, pct, Font::F6x8, INK);

    let mut clock_buf = [0u8; 5];
    let clock = fmt_clock(h.minutes, &mut clock_buf);
    let clock_x = pct_x - 12 - Font::F6x8.text_width(clock);
    draw_text(canvas, clock_x, ty, clock, Font::F6x8, INK);

    canvas.hline(0, RULE_Y, WIDTH, INK);
}

/// Formats `HH:MM` into a stack buffer and returns it.
fn fmt_clock(minutes: u16, buf: &mut [u8; 5]) -> &str {
    let m = minutes % (24 * 60);
    let (hh, mm) = (m / 60, m % 60);
    buf[0] = b'0' + (hh / 10) as u8;
    buf[1] = b'0' + (hh % 10) as u8;
    buf[2] = b':';
    buf[3] = b'0' + (mm / 10) as u8;
    buf[4] = b'0' + (mm % 10) as u8;
    std::str::from_utf8(&buf[..]).unwrap()
}

fn fmt_into<'a>(buf: &'a mut [u8], parts: &[&str]) -> &'a str {
    let mut len = 0;
    for p in parts {
        let b = p.as_bytes();
        buf[len..len + b.len()].copy_from_slice(b);
        len += b.len();
    }
    std::str::from_utf8(&buf[..len]).unwrap()
}

/// Formats `NN%` (0..=100) into a stack buffer and returns it. Allocation-free.
fn fmt_pct(buf: &mut [u8; 4], pct: u8) -> &str {
    let v = pct.clamp(0, 100);
    let mut len = 0;
    if v >= 100 {
        buf[len] = b'1';
        len += 1;
        buf[len] = b'0';
        len += 1;
        buf[len] = b'0';
        len += 1;
    } else if v >= 10 {
        buf[len] = b'0' + v / 10;
        len += 1;
        buf[len] = b'0' + v % 10;
        len += 1;
    } else {
        buf[len] = b'0' + v;
        len += 1;
    }
    buf[len] = b'%';
    len += 1;
    std::str::from_utf8(&buf[..len]).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_formats() {
        let mut b = [0u8; 5];
        assert_eq!(fmt_clock(0, &mut b), "00:00");
        assert_eq!(fmt_clock(14 * 60 + 32, &mut b), "14:32");
        assert_eq!(fmt_clock(23 * 60 + 59, &mut b), "23:59");
        assert_eq!(fmt_clock(24 * 60 + 5, &mut b), "00:05");
    }

    #[test]
    fn names_round_trip() {
        for v in View::ALL {
            assert_eq!(View::from_name(v.name()), Some(v));
        }
        assert_eq!(View::from_name("nope"), None);
        assert_eq!(View::Companion.cycle(-1), View::Setup);
        assert_eq!(View::Setup.cycle(1), View::Companion);
    }

    #[test]
    fn render_all_views_does_not_panic() {
        let mut app = App::new();
        app.now = DUMP_CLOCK as f32 * 60.0;
        for v in View::ALL {
            app.set_view(v);
            let mut c = Canvas::new();
            app.render(&mut c);
            assert_eq!(c.get_pixel(0, RULE_Y), Some(INK));
        }
    }

    #[test]
    fn spring_slide_transitions() {
        let mut app = App::new();
        app.set_view(View::Companion);
        assert!(!app.sliding);
        app.cycle(1);
        assert_eq!(app.view, View::Ticker);
        assert!(app.sliding);
        for _ in 0..200 {
            app.tick(1.0 / 60.0, 0.5);
            let mut c = Canvas::new();
            app.render(&mut c);
        }
        assert!(!app.sliding, "slide should settle");
    }

    #[test]
    fn demo_cycles_states_and_approval() {
        let mut app = App::new();
        app.demo_tick(0.0);
        assert_eq!(app.header.state, MascotState::Idle);
        app.demo_tick(7.5);
        assert!(app.approval.is_some());
    }
}
