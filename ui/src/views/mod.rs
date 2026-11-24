//! The six core views and the stateful `App` orchestrator.
//!
//! Views render from synthetic demo state until the daemon's first `state_sync`
//! (SPEC 6); every view is deterministic given `now` so `--dump-frame`
//! snapshots are reproducible. `App` is transport-agnostic: daemon frames come
//! in through [`App::on_daemon_message`] and UI->daemon frames are returned
//! from [`App::key`].

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::gfx::canvas::{Canvas, ACCENT_AMBER, ACCENT_BLUE, INK, PAPER, WIDTH};
use crate::gfx::font::{draw_text, Font};
use crate::gfx::mascot::{MascotAnim, MascotState};
use crate::gfx::spring::Spring;
use crate::ipc::{self, DaemonState};

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
    pub host: String,
    pub tailscale: bool,
    /// Minutes since midnight (clamped to one day).
    pub minutes: u16,
    pub battery: u8,
}

impl Default for Header {
    fn default() -> Self {
        Self {
            state: MascotState::Idle,
            host: "macbook-pro".to_string(),
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
    /// Daemon-owned state mirrored from `state_sync` frames.
    daemon: DaemonState,
    /// False until the first `state_sync`; views show demo data meanwhile.
    daemon_synced: bool,
    /// Summary of the most recent `activity_event` (companion "last" line).
    last_tool: String,
    /// Live mic RMS from `mic_level`; overrides the synthetic waveform.
    mic_rms: Option<f32>,
    toast_text: String,
    /// `now` (monotonic seconds) at which the toast disappears.
    toast_until: f32,
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
            daemon: DaemonState::default(),
            daemon_synced: false,
            last_tool: String::new(),
            mic_rms: None,
            toast_text: String::new(),
            toast_until: 0.0,
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
        self.toast_text.clear();
        self.toast_until = 0.0;
    }

    /// Applies one daemon->UI NDJSON frame (SPEC 6.1). Unknown types are ignored.
    pub fn on_daemon_message(&mut self, v: Value) {
        let text = |key: &str| v[key].as_str().unwrap_or("").to_string();
        match v["type"].as_str() {
            Some("state_sync") => {
                self.daemon.apply_state_sync(&v);
                let d = &self.daemon;
                self.header.host = d.active_host.clone();
                self.header.tailscale = d.tailscale_state == "connected";
                if let Some(state) = mascot_state(&d.agent_state) {
                    self.header.state = state;
                }
                self.menu.set_hosts(d.hosts.clone(), &d.active_host);
                self.prompt.host = d.active_host.clone();
                self.setup.wifi_ssid = d.wifi_ssid.clone();
                self.setup.local_ip = d.local_ip.clone();
                self.setup.tailscale = d.tailscale_state.clone();
                self.setup.tailscale_auth = d.tailscale_auth_url.clone();
                self.setup.api_key_ok = d.api_key_ready;
                // A submitted prompt stays on screen until the daemon answers.
                if self.prompt.dispatch {
                    self.prompt.clear();
                }
                self.daemon_synced = true;
            }
            Some("activity_event") => {
                let kind = match v["kind"].as_str().unwrap_or("") {
                    "type_text" => "TXT",
                    k if k.starts_with("vnc") => "VNC",
                    k if k.starts_with("ssh") => "SSH",
                    _ => "LLM",
                };
                let ts = fmt_hms(v["ts"].as_f64().unwrap_or(0.0) as i64);
                self.last_tool = text("summary");
                self.ticker
                    .push(ts, kind.to_string(), self.last_tool.clone());
            }
            Some("approval_request") => {
                self.approval = Some(approval::Approval::from_request(
                    text("request_id"),
                    approval::ActionKind::from_action(v["action"].as_str().unwrap_or("")),
                    text("host"),
                    text("summary"),
                    text("risk"),
                    text("preview_1bit_120x68_b64"),
                ));
                self.header.state = MascotState::WaitingApproval;
                self.set_view(View::Approval);
            }
            Some("mic_level") => {
                self.mic_rms = Some(v["rms"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0) as f32);
            }
            Some("toast") => self.toast(&text("text")),
            _ => {}
        }
    }

    /// Shows `text` in the status bar for four seconds.
    fn toast(&mut self, text: &str) {
        self.toast_text = text.to_string();
        self.toast_until = self.now + 4.0;
    }

    /// Dismisses the approval modal, returning its `approval_decision` frame
    /// when it came from the daemon (synthetic demo modals have no request id).
    fn decide(&mut self, decision: &str) -> Option<Value> {
        let mut a = self.approval.take()?;
        a.finish(decision);
        if self.header.state == MascotState::WaitingApproval {
            self.header.state = MascotState::Idle;
        }
        if self.view == View::Approval {
            self.set_view(View::Companion);
        }
        (!a.request_id.is_empty()).then(|| ipc::msg_approval_decision(&a.request_id, decision))
    }

    /// Cycles the target host; returns `select_host` if it actually changed.
    fn cycle_host(&mut self) -> Option<Value> {
        self.menu.cycle_host();
        let host = self.menu.host();
        if host == self.header.host {
            return None;
        }
        self.header.host = host.to_string();
        self.prompt.host = host.to_string();
        Some(ipc::msg_select_host(host))
    }

    /// Routes one semantic key. Returns the UI->daemon frame it produced, if any
    /// (SPEC 6.2); the caller owns the transport.
    pub fn key(&mut self, key: crate::backend::LapisKey) -> Option<Value> {
        use crate::backend::LapisKey as K;
        self.last_input = Instant::now();
        match key {
            K::CycleL => self.cycle(-1),
            K::CycleR => self.cycle(1),
            K::Up | K::Down => {
                let delta = if key == K::Up { -1 } else { 1 };
                if self.view == View::Setup {
                    let n = setup::ACTIONS.len() as i32;
                    self.setup.action = (self.setup.action as i32 + delta).rem_euclid(n) as usize;
                } else {
                    self.menu.move_sel(delta);
                }
            }
            K::Tab => return self.cycle_host(),
            K::Enter => {
                // Decisions only count while the modal is on screen.
                if self.view == View::Approval && self.approval.is_some() {
                    return self.decide("approve_once");
                }
                match self.view {
                    View::Prompt if !self.prompt.buffer.is_empty() => {
                        self.prompt.dispatch = true;
                        let prompt = String::from_utf8_lossy(&self.prompt.buffer);
                        return Some(ipc::msg_submit_task(&self.prompt.host, &prompt));
                    }
                    View::Menu => return self.menu_select(),
                    View::Setup => {
                        let (_, command) = setup::ACTIONS[self.setup.action];
                        return Some(ipc::msg_system_control(command));
                    }
                    _ => {}
                }
            }
            K::Esc => {
                if self.view == View::Approval && self.approval.is_some() {
                    return self.decide("deny");
                }
                if self.view == View::Prompt {
                    self.prompt.clear();
                }
                self.set_view(View::Companion);
            }
            K::EscHold => {
                // Global kill-switch: the modal is dropped without a decision,
                // `kill_all` cancels whatever was waiting on it.
                self.approval = None;
                self.header.state = MascotState::Idle;
                if self.view == View::Approval {
                    self.set_view(View::Companion);
                }
                self.toast("kill-switch");
                return Some(ipc::msg_agent_control("kill_all"));
            }
            K::Char('a' | 'A') if self.approval.is_some() && self.view == View::Approval => {
                return self.decide("approve_session");
            }
            K::Char(c) => {
                if self.view == View::Prompt {
                    self.prompt.push(c);
                }
            }
            // Local hardware-only controls: the `--fb` loop steps the ST7789
            // backlight itself (sysfs); nothing travels over IPC.
            K::BacklightDown | K::BacklightUp => return None,
            K::Left | K::Right => {}
        }
        None
    }

    /// Activates the highlighted menu row (order matches `menu::ROWS`).
    fn menu_select(&mut self) -> Option<Value> {
        match self.menu.index {
            0 => self.set_view(View::Prompt),
            1 => return self.cycle_host(),
            2 => {
                let paused = self.daemon.agent_state == "PAUSED";
                let command = if paused { "resume" } else { "pause" };
                return Some(ipc::msg_agent_control(command));
            }
            5 => self.set_view(View::Setup),
            // ponytail: saved workflows / scheduled jobs have no IPC message in SPEC 6 yet.
            _ => {}
        }
        None
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
        let mut m = self.mascot_anim_for(now);
        if let Some(rms) = self.mic_rms {
            m.rms = rms;
        }
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
        let toast = (self.now < self.toast_until).then_some(self.toast_text.as_str());
        draw_status_bar(canvas, &self.header, toast);
        match self.view {
            View::Companion => companion::render(
                canvas,
                &self.header,
                &self.daemon,
                self.daemon_synced,
                &self.last_tool,
                &self.mascot_anim,
                self.now,
            ),
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

/// Draws `[● STATE] [HOST: x] [TS] [HH:MM] [NN%]` and the rule. An active
/// `toast` takes over the host/Tailscale segment (the bar has no spare room).
///
/// All text is built in fixed stack buffers so the hot render path never allocates.
fn draw_status_bar(canvas: &mut Canvas, h: &Header, toast: Option<&str>) {
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

    let host_x = 52;
    if let Some(toast) = toast {
        let mut toast_buf = [0u8; 32];
        let room = ((clock_x - 8 - host_x) / 6).clamp(0, 32) as usize;
        let t = fmt_into(&mut toast_buf[..room], &[toast]);
        draw_text(canvas, host_x, ty, t, Font::F6x8, ACCENT_AMBER);
    } else {
        // Host label built into a stack buffer.
        let mut host_buf = [0u8; 24];
        {
            let host = fmt_into(&mut host_buf, &["HOST:", &h.host]);
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
    }

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

/// Concatenates `parts` into `buf`, truncating (on a char boundary) when full:
/// daemon-supplied strings have no length guarantee.
fn fmt_into<'a>(buf: &'a mut [u8], parts: &[&str]) -> &'a str {
    let mut len = 0;
    for p in parts {
        let mut n = p.len().min(buf.len() - len);
        while !p.is_char_boundary(n) {
            n -= 1;
        }
        buf[len..len + n].copy_from_slice(&p.as_bytes()[..n]);
        len += n;
    }
    std::str::from_utf8(&buf[..len]).unwrap()
}

/// Formats unix seconds as UTC `HH:MM:SS`.
// ponytail: UTC, the daemon sends no timezone; add an offset to state_sync if local time matters.
fn fmt_hms(ts: i64) -> String {
    let s = ts.rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// Maps a `state_sync.agent_state` onto a mascot state (`None` if unknown, e.g. `PAUSED`).
fn mascot_state(agent_state: &str) -> Option<MascotState> {
    Some(match agent_state {
        "IDLE" => MascotState::Idle,
        "LISTENING" => MascotState::Listening,
        "THINKING" => MascotState::Thinking,
        "ACTING" => MascotState::Acting,
        "WAITING_APPROVAL" => MascotState::WaitingApproval,
        _ => return None,
    })
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
    fn daemon_messages_drive_views_and_keys_emit_frames() {
        use crate::backend::LapisKey as K;
        use serde_json::json;
        let mut app = App::new();
        // Over-long daemon strings must truncate, not panic the status bar.
        let long_host = "a-very-long-tailscale-hostname-é.example.ts.net";
        app.on_daemon_message(json!({
            "type": "state_sync", "agent_state": "ACTING", "active_host": long_host,
            "hosts": ["macbook-pro", long_host], "active_task": null,
            "tailscale_state": "needs_login",
            "tailscale_auth_url": format!("https://login.tailscale.com/a/{}", "x".repeat(80)),
        }));
        assert_eq!(app.header.host, long_host);
        assert_eq!(app.header.state, MascotState::Acting);
        assert!(!app.header.tailscale);
        assert_eq!(app.menu.host(), long_host);
        app.on_daemon_message(json!({"type": "toast", "level": "info", "text": "t".repeat(90)}));
        for v in View::ALL {
            app.set_view(v);
            app.render(&mut Canvas::new());
        }

        for i in 0..50 {
            app.on_daemon_message(json!({
                "type": "activity_event", "ts": 1795438264 + i, "kind": "vnc_click",
                "summary": "click(840, 412) left",
            }));
        }
        assert_eq!(app.ticker.len(), 40);
        let (ts, kind, _) = app.ticker.last_entry().unwrap();
        assert_eq!((ts.len(), kind), (8, "VNC"));
        assert_eq!(fmt_hms(1795438264), "12:51:04");

        app.on_daemon_message(json!({
            "type": "approval_request", "request_id": "req_7", "host": "arch-dev",
            "action": "ssh_exec", "summary": "rm -rf build", "risk": "high",
            "preview_1bit_120x68_b64": null,
        }));
        assert_eq!(app.view, View::Approval);
        assert_eq!(app.header.state, MascotState::WaitingApproval);
        app.render(&mut Canvas::new());
        assert_eq!(
            app.key(K::Char('a')),
            Some(
                json!({"type": "approval_decision", "request_id": "req_7", "decision": "approve_session"})
            )
        );
        assert!(app.approval.is_none());

        // Synthetic (demo) modals never reach the daemon.
        app.show_synthetic_approval();
        app.set_view(View::Approval);
        assert_eq!(app.key(K::Enter), None);
        assert!(app.approval.is_none());

        app.set_view(View::Prompt);
        assert_eq!(app.key(K::Enter), None, "empty prompt sends nothing");
        app.key(K::Char('h'));
        app.key(K::Char('i'));
        assert_eq!(
            app.key(K::Enter),
            Some(json!({"type": "submit_task", "host": long_host, "prompt": "hi"}))
        );
        assert!(app.prompt.dispatch && !app.prompt.buffer.is_empty());
        app.on_daemon_message(json!({"type": "state_sync"}));
        assert!(app.prompt.buffer.is_empty());

        assert_eq!(
            app.key(K::Tab),
            Some(json!({"type": "select_host", "host": "macbook-pro"}))
        );
        app.set_view(View::Setup);
        app.key(K::Down);
        assert_eq!(
            app.key(K::Enter),
            Some(json!({"type": "system_control", "command": "reboot"}))
        );
        assert_eq!(
            app.key(K::EscHold),
            Some(json!({"type": "agent_control", "command": "kill_all"}))
        );
        assert_eq!(app.toast_text, "kill-switch");
        app.settle_all();
        assert!(app.toast_text.is_empty());
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
