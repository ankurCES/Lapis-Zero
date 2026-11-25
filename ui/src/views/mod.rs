//! The six core views, the pairing view, and the stateful `App` orchestrator.
//!
//! Views render from synthetic demo state until the daemon's first `state_sync`
//! (SPEC 6); every view is deterministic given `now` so `--dump-frame`
//! snapshots are reproducible. `App` is transport-agnostic: daemon frames come
//! in through [`App::on_daemon_message`]; UI->daemon frames are returned from
//! [`App::key`], and replies to daemon requests wait in [`App::take_outbox`].

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::gfx::canvas::{Canvas, ACCENT_AMBER, ACCENT_BLUE, INK, PAPER, WIDTH};
use crate::gfx::font::{draw_text, Font};
use crate::gfx::head::{self, Accent};
use crate::gfx::mascot::{self, Mascot, MascotState, ACTION_HOLD_SECS, BEAT_SECS};
use crate::gfx::spring::Spring;
use crate::ipc::{self, DaemonState};

pub mod approval;
pub mod companion;
pub mod menu;
pub mod pairing;
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

/// The six core views (SPEC 5.3) in cycle order, then the pairing view.
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
    /// Android pairing PIN / QR (SPEC 6.3). Shown and closed by the daemon's
    /// `pairing` message, so it is not in [`View::ALL`] and never cycled to.
    Pairing,
}

/// Every `--view` name: the six core views plus the two pairing demo screens.
pub const VIEW_NAMES: [&str; 8] = [
    "companion",
    "ticker",
    "menu",
    "approval",
    "prompt",
    "setup",
    "pairing_pin",
    "pairing_qr",
];

impl View {
    /// The six core views in `Z`/`C` cycle order.
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
            View::Pairing => "pairing",
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
    /// The mascot state on screen: [`App::state`] as of the last tick.
    pub state: MascotState,
    pub host: String,
    pub tailscale: bool,
    /// Minutes since midnight (clamped to one day).
    pub minutes: u16,
    /// Battery percentage; `None` (shown as `--%`) when the daemon has no gauge.
    pub battery: Option<u8>,
    /// Draws a charging bolt beside the percentage.
    pub charging: bool,
}

impl Default for Header {
    fn default() -> Self {
        Self {
            state: MascotState::Idle,
            host: "macbook-pro".to_string(),
            tailscale: true,
            minutes: 14 * 60 + 32,
            battery: Some(84),
            charging: false,
        }
    }
}

/// A step on the host, as the head follows it.
#[derive(Clone, Copy)]
struct Step {
    /// What the head acts out for it.
    state: MascotState,
    /// When it was first seen: the start of its [`ACTION_HOLD_SECS`] on screen.
    since: f64,
    /// Still executing, as far as the daemon has said.
    running: bool,
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
    /// Reusable frame the incoming view is drawn into during a slide. Only
    /// `None` while [`App::render`] has it out.
    scratch: Option<Canvas>,
    /// Monotonic seconds; drives every animation. `f64`: the device stays up
    /// for weeks, and an `f32` of seconds cannot tell frames apart after days.
    now: f64,
    header: Header,
    /// The 3D head and its easing between states.
    mascot: Mascot,
    /// The daemon's `agent_state` as a mascot state. The approval modal
    /// stands in for it locally until the next `state_sync`.
    agent: MascotState,
    /// Push-to-talk is held: the head listens whatever the daemon says (it
    /// has no state of its own for a capture in progress).
    ptt: bool,
    /// The latest step on the host: the one `state_sync.action` names while
    /// it runs, or, from a daemon that does not name it, the one the last
    /// `activity_event` reported.
    step: Option<Step>,
    /// What the pending approval is for: once granted, the step that follows
    /// is known before its own event arrives (older daemons only).
    approved: Option<MascotState>,
    /// The beat the running task would end on if it ended now, read from how
    /// its last step went. Only used with a daemon that sends no
    /// `task_finished` here (see [`App::daemon_narrates`]).
    ending: Option<MascotState>,
    /// A task ending (`Done` / `Error`) that has not played out yet.
    beat: Option<MascotState>,
    /// `--state` and `--demo`: show this state whatever the daemon says.
    pinned: Option<MascotState>,
    /// The backlight is off (`--fb` loop): nobody can see the head, so it
    /// stops asking for frames of its own.
    backlight_off: bool,
    ticker: ticker::TickerView,
    menu: menu::MenuView,
    prompt: prompt::PromptView,
    setup: setup::SetupView,
    approval: Option<approval::Approval>,
    pairing: Option<pairing::Pairing>,
    /// The view an overlay (approval modal, pairing) interrupted; restored when
    /// the last overlay closes.
    base: View,
    /// True while the overlay on screen sits on top of the other overlay.
    covered: bool,
    /// `ui_settings.theme == "dark"`: INK and PAPER swap when a frame is presented.
    dark: bool,
    /// Last `ui_settings.brightness` (percent) received, to spot a change.
    daemon_brightness: Option<u8>,
    /// A changed brightness the `--fb` loop has not collected yet.
    pending_brightness: Option<u8>,
    /// Replies to daemon requests, waiting for the frontend loop to send them.
    outbox: Vec<Value>,
    /// Copy of the most recently presented frame (`screen_request`).
    last_frame: Canvas,
    /// False until the first [`App::render`].
    has_frame: bool,
    last_input: Instant,
    /// Daemon-owned state mirrored from `state_sync` frames.
    daemon: DaemonState,
    /// False until the first `state_sync`; views show demo data meanwhile.
    daemon_synced: bool,
    /// Summary of the most recent `activity_event` (companion "last" line).
    last_tool: String,
    /// Live mic RMS from `mic_level`; overrides the synthetic waveform.
    mic_rms: Option<f32>,
    /// Appliance mode only: the daemon socket is down, so nothing shown is live.
    offline: bool,
    /// The onboarding view has been offered once (first sync without Tailscale).
    setup_surfaced: bool,
    /// Local-time offset from UTC in minutes (`state_sync.utc_offset_min`).
    utc_offset_min: i32,
    toast_text: String,
    /// `now` (monotonic seconds) at which the toast disappears.
    toast_until: f64,
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
            scratch: Some(Canvas::new()),
            now: 0.0,
            header: Header::default(),
            mascot: Mascot::new(),
            agent: MascotState::Idle,
            ptt: false,
            step: None,
            approved: None,
            ending: None,
            beat: None,
            pinned: None,
            backlight_off: false,
            ticker: ticker::TickerView::demo(),
            menu: menu::MenuView::demo(),
            prompt: prompt::PromptView::new(),
            setup: setup::SetupView::demo(),
            approval: None,
            pairing: None,
            base: View::Companion,
            covered: false,
            dark: false,
            daemon_brightness: None,
            pending_brightness: None,
            outbox: Vec::new(),
            last_frame: Canvas::new(),
            has_frame: false,
            last_input: Instant::now(),
            daemon: DaemonState::default(),
            daemon_synced: false,
            last_tool: String::new(),
            mic_rms: None,
            offline: false,
            setup_surfaced: false,
            utc_offset_min: 0,
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
        // Capture the current (pre-switch) frame as the outgoing one, in the
        // scratch frame; the old outgoing frame becomes the scratch.
        let mut prev = self.scratch.take().unwrap_or_default();
        prev.clear(PAPER);
        self.render_full(&mut prev);
        self.scratch = Some(std::mem::replace(&mut self.prev, prev));
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
        self.agent = MascotState::WaitingApproval;
    }

    /// Shows `state` whatever the daemon says (`--state`). With `snap` the
    /// head is put straight into it as if it began at time zero, which pins
    /// the frame `--dump-frame --time` renders; without, it eases over.
    pub fn pin_state(&mut self, state: MascotState, snap: bool) {
        self.pinned = Some(state);
        if snap {
            self.mascot.snap(state);
        }
    }

    /// The mascot state to show now: the daemon's `agent_state`, refined by
    /// the step on the host and by a task that just ended.
    pub fn state(&self) -> MascotState {
        if let Some(pinned) = self.pinned {
            return pinned;
        }
        if self.ptt {
            return MascotState::Listening;
        }
        // A step is on the face while it runs, and in any case for
        // ACTION_HOLD_SECS from when it was first seen: the daemon is back to
        // thinking the moment a quick one returns.
        let acting = self.agent == MascotState::Working;
        let held = |s: &Step| self.now - s.since < f64::from(ACTION_HOLD_SECS);
        let step = self.step.filter(|s| (s.running && acting) || held(s));
        MascotState::resolve(self.agent, step.map(|s| s.state), self.beat)
    }

    /// True for a daemon that says for itself which step is running
    /// (`state_sync.action`) and how a task ended (`task_finished`). An older
    /// one sends neither: its steps are known when their `activity_event`
    /// comes back, and its endings are read from how the last step went.
    fn daemon_narrates(&self) -> bool {
        self.daemon.action.is_some()
    }

    /// Follows a new `agent_state`, and with it `state_sync.action`.
    fn set_agent(&mut self, agent: MascotState) {
        if agent == MascotState::Working {
            let entering = self.agent != agent;
            match self.daemon.action.as_deref() {
                // The daemon names the step it is executing ("" for none, and
                // a kind this UI does not know is plain work).
                Some(kind) => {
                    let state = MascotState::from_activity(kind).unwrap_or(agent);
                    let named = !kind.is_empty();
                    match self.step.as_mut() {
                        // No step named: the last one has returned, and keeps
                        // what is left of its hold.
                        Some(step) if !entering && !named => step.running = false,
                        // The same step, still going: every change of the
                        // daemon's status repeats the frame.
                        Some(step) if !entering && step.running && step.state == state => {}
                        _ => {
                            self.step = Some(Step {
                                state,
                                since: self.now,
                                running: named,
                            });
                        }
                    }
                }
                // An older daemon: the step's event comes when it returns.
                // Until then only one that needed approval is known for what it is.
                None if entering => {
                    self.step = self.approved.take().map(|state| Step {
                        state,
                        since: self.now,
                        running: true,
                    });
                }
                None => {}
            }
        } else if agent != MascotState::WaitingApproval {
            self.approved = None;
        }
        if agent != MascotState::Idle {
            self.beat = None;
        }
        self.agent = agent;
    }

    /// Jumps to the `--view <name>` target (one of [`VIEW_NAMES`]); the two
    /// pairing names load deterministic demo data. Returns `false`, changing
    /// nothing, for an unknown name.
    pub fn open_named(&mut self, name: &str) -> bool {
        let demo = match name {
            "pairing_pin" => pairing::Pairing::demo_pin(),
            "pairing_qr" => pairing::Pairing::demo_qr(),
            _ => {
                let view = View::from_name(name);
                if let Some(v) = view {
                    self.set_view(v);
                }
                return view.is_some();
            }
        };
        self.show_pairing(demo);
        self.set_view(View::Pairing);
        true
    }

    /// Puts `p` on the pairing view. The first frame of a pairing always takes
    /// the screen; a later one (a status update) leaves a pending approval
    /// modal on top, since the modal returns here once it is decided.
    pub fn show_pairing(&mut self, p: pairing::Pairing) {
        let first = self.pairing.replace(p).is_none();
        if first || !(self.view == View::Approval && self.approval.is_some()) {
            self.show_overlay(View::Pairing);
        }
    }

    /// Closes the pairing view, returning to what it covered. No-op when it is not open.
    fn close_pairing(&mut self) {
        if self.pairing.take().is_some() || self.view == View::Pairing {
            self.hide_overlay(View::Pairing);
        }
    }

    /// Takes the approval modal down the way a decision does (mascot back to
    /// idle, view back to what the modal covered) and hands it back; telling
    /// the daemon is the caller's business.
    fn close_approval(&mut self) -> Option<approval::Approval> {
        let a = self.approval.take()?;
        if self.agent == MascotState::WaitingApproval {
            self.agent = MascotState::Idle;
        }
        self.hide_overlay(View::Approval);
        Some(a)
    }

    /// Puts an overlay (`Approval` or `Pairing`) on screen, remembering what it covers.
    fn show_overlay(&mut self, overlay: View) {
        if self.view == overlay {
            return;
        }
        match self.view {
            View::Approval | View::Pairing => self.covered = true,
            v => {
                self.base = v;
                self.covered = false;
            }
        }
        self.set_view(overlay);
    }

    /// Takes a closed overlay off screen: back to the overlay it covered if that
    /// is still open, else to the interrupted view. Call after clearing its state.
    fn hide_overlay(&mut self, overlay: View) {
        let covered = std::mem::take(&mut self.covered);
        if self.view != overlay {
            // It was the covered one, or the user had navigated away from it.
            return;
        }
        self.set_view(match overlay {
            View::Approval if covered && self.pairing.is_some() => View::Pairing,
            View::Pairing if covered && self.approval.is_some() => View::Approval,
            _ => self.base,
        });
    }

    /// The brightness percentage (`0..=100`) from `ui_settings`, once per change.
    /// The `--fb` loop turns it into a backlight level; local `fn+U`/`fn+I`
    /// steps stay in force until the daemon sends a different value.
    pub fn take_brightness(&mut self) -> Option<u8> {
        self.pending_brightness.take()
    }

    /// Drains the replies queued by [`App::on_daemon_message`] (`screen_snapshot`);
    /// the frontend loop sends them on the socket.
    pub fn take_outbox(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.outbox)
    }

    /// Snaps every spring to rest for a deterministic `--dump-frame` snapshot.
    pub fn settle_all(&mut self) {
        self.slide.snap_to(0.0);
        self.sliding = false;
        self.menu.pill.snap_to(self.menu.index as f32);
        self.ticker.settle();
        self.mascot.settle();
        if let Some(a) = self.approval.as_mut() {
            a.settle();
        }
        self.toast_text.clear();
        self.toast_until = 0.0;
    }

    /// Applies one daemon->UI NDJSON frame (SPEC 6.1, 6.3). Unknown types are
    /// ignored. Frames that need an answer (`screen_request`) queue it for
    /// [`App::take_outbox`].
    pub fn on_daemon_message(&mut self, v: Value) {
        let text = |key: &str| v[key].as_str().unwrap_or("").to_string();
        match v["type"].as_str() {
            Some("state_sync") => {
                let had_task = self.daemon.active_task.is_some();
                self.daemon.apply_state_sync(&v);
                if let Some(agent) = MascotState::from_agent(&self.daemon.agent_state) {
                    self.set_agent(agent);
                }
                match (had_task, self.daemon.active_task.is_some()) {
                    (false, true) => (self.ending, self.beat) = (Some(MascotState::Done), None),
                    // The task is over. A daemon that narrates says how in a
                    // `task_finished` of its own; for an older one, guess.
                    (true, false) if self.agent == MascotState::Idle && !self.daemon_narrates() => {
                        self.beat = self.ending;
                    }
                    _ => {}
                }
                let d = &self.daemon;
                self.header.host = d.active_host.clone();
                self.header.tailscale = d.tailscale_state == "connected";
                self.menu.set_hosts(d.hosts.clone(), &d.active_host);
                self.prompt.host = d.active_host.clone();
                self.setup.wifi_ssid = d.wifi_ssid.clone();
                self.setup.local_ip = d.local_ip.clone();
                self.setup.tailscale = d.tailscale_state.clone();
                self.setup.tailscale_auth = d.tailscale_auth_url.clone();
                self.setup.api_key_ok = d.api_key_ready;
                if let Some(offset) = v["utc_offset_min"].as_i64() {
                    self.utc_offset_min = offset.clamp(-14 * 60, 14 * 60) as i32;
                }
                // Battery (SPEC 6.3): a number, or null when there is no gauge.
                // Read leniently here so an odd value cannot void the whole sync.
                if let Some(pct) = v.get("battery_pct") {
                    self.header.battery = pct.as_f64().map(|p| p.clamp(0.0, 100.0).round() as u8);
                    self.setup.battery_pct = self.header.battery;
                    self.setup.battery_v.clear(); // the daemon reports no voltage
                }
                if let Some(charging) = v["battery_charging"].as_bool() {
                    self.header.charging = charging;
                }
                // `pairing: null` means no window is open: drop a view left over
                // from a daemon that restarted without sending `mode: "off"`.
                if v.get("pairing").is_some_and(Value::is_null) {
                    self.close_pairing();
                }
                // A submitted prompt stays on screen until the daemon answers.
                if self.prompt.dispatch {
                    self.prompt.clear();
                }
                // SPEC 5.3 View 6: a device that is not on the tailnet yet opens
                // on the network/onboarding view, once, instead of an idle clock.
                if !self.setup_surfaced {
                    self.setup_surfaced = true;
                    if self.daemon.tailscale_state != "connected" && self.view == View::Companion {
                        self.set_view(View::Setup);
                    }
                }
                self.daemon_synced = true;
            }
            Some("activity_event") => {
                let kind = v["kind"].as_str().unwrap_or("");
                match v["status"].as_str() {
                    Some("error") => self.ending = Some(MascotState::Error),
                    Some("denied") => self.ending = None,
                    _ => {
                        self.ending = Some(MascotState::Done);
                        // A daemon that narrates named this step while it ran;
                        // from an older one this report is the first word of it.
                        let state = MascotState::from_activity(kind);
                        if let Some(state) = state.filter(|_| !self.daemon_narrates()) {
                            self.step = Some(Step {
                                state,
                                since: self.now,
                                running: true,
                            });
                        }
                    }
                }
                let kind = match kind {
                    "type_text" => "TXT",
                    k if k.starts_with("vnc") => "VNC",
                    k if k.starts_with("ssh") => "SSH",
                    _ => "LLM",
                };
                let utc = v["ts"].as_f64().unwrap_or(0.0) as i64;
                let ts = fmt_hms(utc + i64::from(self.utc_offset_min) * 60);
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
                self.approved = MascotState::from_activity(v["action"].as_str().unwrap_or(""));
                self.agent = MascotState::WaitingApproval;
                self.show_overlay(View::Approval);
                self.wake(); // SPEC 5.2: an approval lights the screen instantly
            }
            Some("approval_resolved") => {
                // Decided elsewhere (phone, kill switch): drop the matching modal
                // as a local decision would, but send nothing back.
                let id = v["request_id"].as_str().unwrap_or("");
                let on_modal = |a: &approval::Approval| a.request_id == id;
                if !id.is_empty() && self.approval.as_ref().is_some_and(on_modal) {
                    self.close_approval();
                }
            }
            Some("pairing") => {
                if v["mode"].as_str() == Some("off") {
                    self.close_pairing();
                } else if let Some(p) = pairing::Pairing::from_message(&v, self.pairing.as_ref()) {
                    self.show_pairing(p);
                    self.wake(); // a PIN or QR nobody can see is no use
                }
            }
            Some("ui_settings") => {
                // Any subset of fields may be present. `mute_audio` is the
                // daemon's own business (the UI plays no sound).
                if let Some(theme) = v["theme"].as_str() {
                    self.dark = theme == "dark";
                }
                if let Some(pct) = v["brightness"].as_f64() {
                    let pct = pct.clamp(0.0, 100.0).round() as u8;
                    // Every `ui_settings` frame repeats the brightness; only a
                    // new value may override what the user set with fn+U / fn+I.
                    if self.daemon_brightness.replace(pct) != Some(pct) {
                        self.pending_brightness = Some(pct);
                    }
                }
                if v["wake"].as_bool() == Some(true) {
                    self.wake();
                }
            }
            Some("screen_request") => {
                if let Some(id) = v["id"].as_str() {
                    self.queue_screen_snapshot(id);
                }
            }
            Some("mic_level") => {
                self.mic_rms = Some(v["rms"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0) as f32);
            }
            Some("toast") => {
                // Trouble reported as a task ends: it did not end well. (The
                // kill switch's warning arrives just after its task is gone.)
                // Guesswork, so only for a daemon that sends no `task_finished`.
                match v["level"].as_str() {
                    _ if self.daemon_narrates() => {}
                    Some("error") => {
                        self.ending = Some(MascotState::Error);
                        self.beat = self.beat.and(self.ending);
                    }
                    Some("warn") if self.beat == Some(MascotState::Done) => self.beat = None,
                    _ => {}
                }
                self.toast(&text("text"));
            }
            // How the task ended, in the daemon's own words: done plays the
            // Done beat, failed and stopped the Error one, cancelled (and
            // anything unknown) goes straight to idle. It follows the idle
            // `state_sync`; a beat it sets early waits for that.
            Some("task_finished") => {
                if let Some(status) = ipc::task_finished_status(&v) {
                    self.beat = MascotState::from_finish(status);
                }
            }
            _ => {}
        }
    }

    /// Queues a `screen_snapshot` of the most recently presented frame.
    fn queue_screen_snapshot(&mut self, id: &str) {
        // Bounded: a daemon asking faster than the loop sends must not pile up PNGs.
        if self.outbox.len() >= 8 {
            return;
        }
        if !self.has_frame {
            self.render(&mut Canvas::new());
        }
        match self.last_frame.encode_png() {
            Ok(png) => {
                let b64 = approval::base64_encode(&png);
                self.outbox.push(ipc::msg_screen_snapshot(id, &b64));
            }
            Err(e) => eprintln!("lapis-ui: screen snapshot failed: {e}"),
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
        let mut a = self.close_approval()?;
        a.finish(decision);
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

    /// Feeds the desk clock from the system clock (live loops only; `--dump-frame`
    /// keeps its pinned 14:32 so snapshots stay reproducible). The daemon supplies
    /// the timezone offset, so this needs no timezone database in the UI.
    pub fn set_wall_clock(&mut self, unix_secs: i64) {
        let local = unix_secs + i64::from(self.utc_offset_min) * 60;
        self.header.minutes = (local.rem_euclid(86_400) / 60) as u16;
    }

    /// Tells the UI whether the daemon link is up (called every frame by `--fb`).
    ///
    /// On the device there is no demo data: while the daemon is unreachable the
    /// status bar says OFFLINE and everything daemon-derived is blanked, so a
    /// crashed daemon can never look like a healthy idle agent.
    pub fn set_offline(&mut self, offline: bool) {
        if offline && !self.offline {
            self.header.host.clear();
            self.header.tailscale = false;
            self.header.battery = None;
            self.header.charging = false;
            self.set_agent(MascotState::Idle);
            (self.step, self.beat) = (None, None);
            // Whatever comes back on the socket may be another daemon.
            self.daemon.action = None;
            self.daemon.active_task = None;
            self.daemon.next_cron.clear();
            self.last_tool.clear();
            // A modal nobody can answer must not stay on screen.
            self.approval = None;
            if self.view == View::Approval {
                self.set_view(View::Companion);
            }
        }
        self.offline = offline;
    }

    /// Milliseconds since the last key press or wake event (backlight idle policy).
    pub fn idle_ms(&self) -> u64 {
        self.last_input.elapsed().as_millis() as u64
    }

    /// Name of the view on screen, for the journal (the device has no console).
    pub fn view_name(&self) -> &'static str {
        self.view.name()
    }

    /// True while the pairing PIN or QR is showing: the idle policy must not
    /// dim or blank something the user has to read (a dark pairing screen
    /// looks like a dead device). Bounded by the daemon's pairing window.
    pub fn keeps_screen_on(&self) -> bool {
        self.view == View::Pairing
    }

    /// Tells the UI whether the idle policy has switched the backlight off.
    pub fn set_backlight_off(&mut self, off: bool) {
        self.backlight_off = off;
    }

    /// Resets the idle timer without routing a key (wakes a blanked screen).
    pub fn wake(&mut self) {
        self.last_input = Instant::now();
    }

    /// Routes one semantic key. Returns the UI->daemon frame it produced, if any
    /// (SPEC 6.2); the caller owns the transport.
    pub fn key(&mut self, key: crate::backend::LapisKey) -> Option<Value> {
        use crate::backend::LapisKey as K;
        self.last_input = Instant::now();
        // The pairing view is modal: ESC cancels the window, nothing else
        // navigates. The kill switch and push-to-talk edges stay global.
        if self.view == View::Pairing && !matches!(key, K::EscHold | K::PushToTalk(_)) {
            if key != K::Esc {
                return None;
            }
            self.close_pairing();
            return Some(ipc::msg_pairing_control("stop"));
        }
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
            K::PushToTalk(down) => {
                // Holding PTT flips the mascot to LISTEN and stops the synthetic wave;
                // the live `mic_level` stream takes over while the daemon captures.
                self.ptt = down;
                self.mic_rms = None;
                return Some(ipc::msg_ptt_audio(if down { "start" } else { "stop" }));
            }
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
                self.close_approval();
                self.set_agent(MascotState::Idle);
                (self.ending, self.beat) = (None, None); // a killed task is not a success
                self.toast("kill-switch");
                return Some(ipc::msg_agent_control("kill_all"));
            }
            K::Char('a' | 'A') if self.approval.is_some() && self.view == View::Approval => {
                return self.decide("approve_session");
            }
            K::Char(c) if self.view == View::Prompt => self.prompt.push(c),
            // Outside the Prompt view the letter keys are navigation (SPEC 5.4);
            // backends deliver plain chars so typing "fix" in a prompt still works.
            K::Char(c) => match c.to_ascii_lowercase() {
                'z' => self.cycle(-1),
                'c' => self.cycle(1),
                'f' => return self.key(K::Up),
                'x' => return self.key(K::Down),
                ' ' | '/' if self.view == View::Companion => self.set_view(View::Prompt),
                _ => {}
            },
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
            // The daemon answers with a `pairing` frame (QR on the tailnet,
            // BLE PIN otherwise), which is what opens the pairing view.
            6 => return Some(ipc::msg_pairing_control("start")),
            // ponytail: saved workflows / scheduled jobs have no IPC message in SPEC 6 yet.
            _ => {}
        }
        None
    }

    /// Advances all springs and animation inputs by `dt` seconds at time `now`.
    pub fn tick(&mut self, dt: f32, now: f64) {
        self.now = now;
        self.slide.tick(dt);
        self.menu.pill.tick(dt);
        self.menu.pill.set_target(self.menu.index as f32);
        if let Some(a) = self.approval.as_mut() {
            a.pop.tick(dt);
            a.pop.set_target(0.0); // slide up to rest
        }
        self.ticker.tick(dt);
        // A beat that has played out gives way to idle.
        let played = |b| self.mascot.state() == b && self.mascot.elapsed(now) >= BEAT_SECS;
        if self.beat.is_some_and(played) {
            self.beat = None;
        }
        self.header.state = self.state();
        // `dt` is the time that led up to this frame: it belongs to the easing
        // already under way. A change of state starts its own with the next
        // frame's, or a long idle wait would be spent on it in one jump.
        self.mascot.tick(dt);
        self.mascot.set_state(self.header.state, now);
        // Without a live `mic_level` the listening head gets a synthetic swell.
        let swell = 0.5 + 0.5 * head::sin((now * 3.0 % std::f64::consts::TAU) as f32);
        self.mascot.rms = self.mic_rms.unwrap_or(swell * swell);
    }

    /// Demo-mode: cycles mascot states and stages a synthetic approval modal.
    pub fn demo_tick(&mut self, now: f64) {
        let states = MascotState::ALL;
        self.pinned = Some(states[((now / 2.5) as usize) % states.len()]);
        let show_approval = ((now / 5.0) as usize) % 2 == 1 && self.approval.is_none();
        if show_approval {
            self.show_synthetic_approval();
        }
    }

    /// Adaptive frame interval (SPEC 5.2): 60 FPS animating, 12 agent-active, 1 idle
    /// (4 while the idle head is on a lit screen, for its sway).
    pub fn frame_interval(&self) -> Duration {
        let input_recent = self.last_input.elapsed() < Duration::from_secs(2);
        let animating = self.sliding
            || self.slide.is_active()
            || self.menu.pill.is_active()
            || self.approval.as_ref().is_some_and(|a| a.pop.is_active())
            || self.ticker.is_active();
        // The head is only on the companion view, and only worth extra
        // frames while the screen is lit.
        let head = self.view == View::Companion && !self.backlight_off;
        if animating || input_recent || (head && self.mascot.is_moving(self.now)) {
            return Duration::from_micros(16_667);
        }
        match self.header.state {
            // The idle head's sway needs 4 FPS; each blink and glance is
            // woken for and run through at the active rate.
            MascotState::Idle if head => Duration::from_secs_f64(
                mascot::idle_wait(self.now).clamp(1.0 / 12.0, mascot::IDLE_FRAME_SECS),
            ),
            MascotState::Idle => Duration::from_millis(1000),
            _ => Duration::from_micros(83_333),
        }
    }

    /// Renders the full 320x170 frame for the current view with any slide offset,
    /// applies the theme, and keeps a copy for `screen_request`.
    pub fn render(&mut self, canvas: &mut Canvas) {
        canvas.clear(PAPER);
        if self.sliding {
            // Taken out for the call and put back: nothing here allocates.
            let mut scratch = self.scratch.take().unwrap_or_default();
            scratch.clear(PAPER);
            self.render_full(&mut scratch);
            let off = self.slide.value().round() as i32;
            canvas.blit(&scratch, off, 0);
            canvas.blit(&self.prev, off + self.slide_dir * WIDTH, 0);
            self.scratch = Some(scratch);
            if self.slide.settled() {
                self.sliding = false;
            }
        } else {
            self.render_full(canvas);
        }
        if self.dark {
            canvas.swap_ink_paper(self.qr_rect());
        }
        self.last_frame.blit(canvas, 0, 0);
        self.has_frame = true;
    }

    /// The QR's paper box while the pairing view shows one: the dark theme must
    /// not invert it. (The view never slides, so the box is always at rest.)
    fn qr_rect(&self) -> Option<(i32, i32, i32, i32)> {
        let p = self
            .pairing
            .as_ref()
            .filter(|_| self.view == View::Pairing)?;
        p.qr.as_ref().map(pairing::Qr::box_rect)
    }

    /// Renders status bar + the current view's content into `canvas`.
    fn render_full(&self, canvas: &mut Canvas) {
        let toast = (self.now < self.toast_until).then_some(self.toast_text.as_str());
        draw_status_bar(canvas, &self.header, toast, self.offline);
        match self.view {
            View::Companion => companion::render(
                canvas,
                &self.header,
                &self.daemon,
                self.daemon_synced || self.offline,
                self.offline,
                &self.last_tool,
                &self.mascot,
                self.now,
                self.dark,
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
            View::Pairing => {
                if let Some(p) = self.pairing.as_ref() {
                    pairing::render(canvas, p);
                }
            }
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
fn draw_status_bar(canvas: &mut Canvas, h: &Header, toast: Option<&str>, offline: bool) {
    let ty = (STATUS_BAR_H - 8) / 2 + 1;
    // State pip: accent by state.
    let pip = match h.state.accent() {
        Accent::Amber => ACCENT_AMBER,
        Accent::Blue => ACCENT_BLUE,
        Accent::None => INK,
    };
    canvas.fill_rect(4, ty + 2, 3, 3, pip);

    // `STATE` (labels are at most six glyphs, clear of the host at x = 52).
    draw_text(canvas, 10, ty, h.state.label(), Font::F6x8, INK);

    // Right-aligned battery (with a bolt while charging) then clock.
    let right = WIDTH - 4;
    let mut pct_buf = [0u8; 4];
    let pct = fmt_battery(&mut pct_buf, h.battery);
    let mut pct_x = right - Font::F6x8.text_width(pct);
    draw_text(canvas, pct_x, ty, pct, Font::F6x8, INK);
    if h.charging {
        pct_x -= 7;
        for (row, bits) in CHARGE_BOLT.iter().enumerate() {
            for col in 0..5 {
                if bits & (0x10 >> col) != 0 {
                    canvas.set_pixel(pct_x + col, ty + row as i32, INK);
                }
            }
        }
    }

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
    } else if offline {
        draw_text(canvas, host_x, ty, "OFFLINE", Font::F6x8, ACCENT_AMBER);
    } else {
        // Tailscale flag sits just left of the clock; the host label gets the
        // room in between and is truncated to it, so the two can never touch.
        let ts_x = clock_x - 8 - 18;
        let mut host_buf = [0u8; 28];
        {
            let room = ((ts_x - 6 - host_x) / 6).clamp(0, 28) as usize;
            let host = fmt_into(&mut host_buf[..room], &["HOST:", &h.host]);
            draw_text(canvas, host_x, ty, host, Font::F6x8, INK);
        }
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

/// Formats seconds since the epoch as `HH:MM:SS` (callers add the local offset).
fn fmt_hms(ts: i64) -> String {
    let s = ts.rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// 5x7 charging bolt for the status bar, one row per byte (bit 4 = leftmost).
const CHARGE_BOLT: [u8; 7] = [
    0b00010, 0b00110, 0b01100, 0b11111, 0b00110, 0b01100, 0b01000,
];

/// Formats the battery as `NN%`, or `--%` when the daemon reports no gauge.
fn fmt_battery(buf: &mut [u8; 4], pct: Option<u8>) -> &str {
    match pct {
        Some(pct) => fmt_pct(buf, pct),
        None => "--%",
    }
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
        app.now = f64::from(DUMP_CLOCK) * 60.0;
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
        assert_eq!(app.state(), MascotState::Working);
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
        assert_eq!(app.state(), MascotState::WaitingApproval);
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
        assert_eq!(app.state(), MascotState::Idle);
        app.demo_tick(7.5);
        assert!(app.approval.is_some());
    }

    /// A `state_sync` with (or without) a running task.
    fn sync(agent_state: &str, task: bool) -> Value {
        let task = task
            .then(|| serde_json::json!({"id": "t_1", "title": "T", "step": 1, "max_steps": 25}));
        serde_json::json!({"type": "state_sync", "agent_state": agent_state,
            "active_task": task, "tailscale_state": "connected"})
    }

    fn event(kind: &str, status: &str) -> Value {
        serde_json::json!({"type": "activity_event", "ts": 0, "task_id": "t_1",
            "kind": kind, "summary": "s", "status": status})
    }

    fn toast(level: &str) -> Value {
        serde_json::json!({"type": "toast", "level": level, "text": "t"})
    }

    #[test]
    fn activity_refines_the_agent_state() {
        use MascotState::*;
        let mut app = App::new();
        app.tick(1.0 / 60.0, 10.0);
        app.on_daemon_message(sync("THINKING", true));
        // The task's opening `prompt` event and the model's thoughts are not actions.
        app.on_daemon_message(event("prompt", "ok"));
        app.on_daemon_message(event("thought", "ok"));
        assert_eq!(app.state(), Thinking);

        // A step is under way; what it is comes with its event.
        app.on_daemon_message(sync("ACTING", true));
        assert_eq!(app.state(), Working);
        app.on_daemon_message(event("screenshot", "ok"));
        assert_eq!(app.state(), Fetching);
        // The daemon is thinking again at once: the step stays on screen a moment.
        app.on_daemon_message(sync("THINKING", true));
        assert_eq!(app.state(), Fetching);
        app.tick(1.0 / 60.0, 10.0 + f64::from(ACTION_HOLD_SECS) + 0.05);
        assert_eq!(app.state(), Thinking);
        assert_eq!(app.header.state.label(), "THINK");

        // The next step does not inherit the last one's kind.
        app.on_daemon_message(sync("ACTING", true));
        assert_eq!(app.state(), Working);
        app.on_daemon_message(event("ssh_exec", "ok"));
        assert_eq!(app.state(), Command);
        app.on_daemon_message(event("some_new_tool", "ok"));
        assert_eq!(app.state(), Working, "unknown kinds are plain work");
        // A step that failed or was refused is not acted out.
        app.on_daemon_message(sync("THINKING", true));
        app.tick(1.0 / 60.0, 20.0);
        app.on_daemon_message(sync("ACTING", true));
        app.on_daemon_message(event("ssh_exec", "error"));
        app.on_daemon_message(event("screenshot", "denied"));
        assert_eq!(app.state(), Working);

        // Push-to-talk listens for as long as it is held, whatever is synced
        // meanwhile, and then hands back to the daemon's state.
        assert!(app
            .key(crate::backend::LapisKey::PushToTalk(true))
            .is_some());
        assert_eq!(app.state(), Listening);
        app.on_daemon_message(sync("THINKING", true));
        assert_eq!(app.state(), Listening);
        assert!(app
            .key(crate::backend::LapisKey::PushToTalk(false))
            .is_some());
        assert_eq!(app.state(), Thinking);

        app.on_daemon_message(sync("PAUSED", true));
        assert_eq!(app.state(), Paused);
        app.on_daemon_message(sync("NAPPING", true));
        assert_eq!(
            app.state(),
            Paused,
            "an unknown agent state changes nothing"
        );
        app.tick(1.0 / 60.0, 20.1);
        assert_eq!(app.header.state.label(), "PAUSED");
    }

    #[test]
    fn an_approved_step_is_known_before_it_reports() {
        use crate::backend::LapisKey as K;
        use MascotState::*;
        let request = |id: &str| {
            serde_json::json!({"type": "approval_request", "request_id": id, "host": "h",
                "action": "ssh_exec", "summary": "ls", "risk": "low",
                "preview_1bit_120x68_b64": null})
        };
        let mut app = App::new();
        app.on_daemon_message(sync("THINKING", true));
        app.on_daemon_message(request("r1"));
        assert_eq!(app.state(), WaitingApproval);
        assert!(app.key(K::Enter).is_some());
        app.on_daemon_message(sync("ACTING", true));
        assert_eq!(app.state(), Command, "the approved command is running");

        // Denied: the daemon goes back to thinking, and the step after that
        // is not mistaken for the refused one.
        app.on_daemon_message(sync("THINKING", true));
        app.on_daemon_message(request("r2"));
        assert!(app.key(K::Esc).is_some());
        app.on_daemon_message(sync("THINKING", true));
        app.on_daemon_message(sync("ACTING", true));
        assert_eq!(app.state(), Working);
    }

    #[test]
    fn a_finished_task_plays_one_beat() {
        use crate::backend::LapisKey as K;
        use MascotState::*;
        // Runs a task whose last step ended with `status`, then ends it.
        let run = |app: &mut App, status: &str| {
            app.on_daemon_message(sync("THINKING", true));
            app.on_daemon_message(sync("ACTING", true));
            app.on_daemon_message(event("vnc_click", status));
        };
        let idle = || sync("IDLE", false);

        // Success: a Done beat, then idle.
        let mut app = App::new();
        app.tick(1.0 / 60.0, 5.0);
        run(&mut app, "ok");
        app.on_daemon_message(idle());
        assert_eq!(app.state(), Done);
        app.tick(1.0 / 60.0, 5.1);
        assert_eq!(app.header.state.label(), "DONE");
        app.on_daemon_message(idle()); // a repeated sync does not replay it
        app.tick(1.0 / 60.0, 5.1 + f64::from(BEAT_SECS) - 0.1);
        assert_eq!(app.state(), Done);
        app.tick(1.0 / 60.0, 5.1 + f64::from(BEAT_SECS) + 0.1);
        assert_eq!(app.state(), Idle);
        app.on_daemon_message(idle());
        assert_eq!(app.state(), Idle);

        // The last step failed, or the daemon reported an error: an Error beat.
        run(&mut app, "error");
        app.on_daemon_message(idle());
        assert_eq!(app.state(), Error);
        run(&mut app, "ok");
        assert_eq!(app.state(), Working, "a new task ends the beat");
        app.on_daemon_message(toast("error"));
        app.on_daemon_message(idle());
        assert_eq!(app.state(), Error);

        // Refused, or killed from here: no celebration.
        run(&mut app, "denied");
        app.on_daemon_message(idle());
        assert_eq!(app.state(), Idle);
        run(&mut app, "ok");
        app.key(K::EscHold);
        app.on_daemon_message(idle());
        assert_eq!(app.state(), Idle);
        // Killed from the phone: the daemon's warning follows the idle sync.
        run(&mut app, "ok");
        app.on_daemon_message(idle());
        assert_eq!(app.state(), Done);
        app.on_daemon_message(toast("warn"));
        assert_eq!(app.state(), Idle);
        // A warning in the middle of a task does not spoil its ending.
        run(&mut app, "ok");
        app.on_daemon_message(toast("warn"));
        app.on_daemon_message(event("vnc_key", "ok"));
        app.on_daemon_message(idle());
        assert_eq!(app.state(), Done);

        // `task_finished`, should the daemon send it here, has the last word.
        let finished =
            |status: &str| serde_json::json!({"type": "task_finished", "status": status});
        for (status, state) in [
            ("failed", Error),
            ("stopped", Error),
            ("done", Done),
            ("cancelled", Idle),
        ] {
            run(&mut app, "ok");
            app.on_daemon_message(idle());
            app.on_daemon_message(finished(status));
            assert_eq!(app.state(), state, "{status}");
        }
    }

    /// A `state_sync` from a daemon that names the step it is executing.
    fn narrated(agent_state: &str, task: bool, action: &str) -> Value {
        let mut frame = sync(agent_state, task);
        frame["action"] = action.into();
        frame
    }

    fn finished(status: &str) -> Value {
        serde_json::json!({"type": "task_finished", "task_id": "t_1", "chat_id": null,
            "host": "macbook-pro", "status": status, "summary": "s"})
    }

    #[test]
    fn the_running_step_comes_from_state_sync_action() {
        use MascotState::*;
        let mut app = App::new();
        app.tick(1.0 / 60.0, 10.0);
        app.on_daemon_message(narrated("THINKING", true, ""));
        assert_eq!(app.state(), Thinking);

        // Known while it runs, with no event needed; unknown and empty are plain work.
        for (action, state) in [
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
            ("some_future_tool", Working),
            ("thought", Working),
            ("", Working),
        ] {
            app.on_daemon_message(narrated("ACTING", true, action));
            assert_eq!(app.state(), state, "{action:?}");
        }
        // `action` only refines ACTING: it does not act over any other state.
        for agent in ["THINKING", "PAUSED", "WAITING_APPROVAL", "IDLE"] {
            let mut app = App::new();
            app.on_daemon_message(narrated(agent, agent != "IDLE", "ssh_exec"));
            assert_eq!(Some(app.state()), MascotState::from_agent(agent), "{agent}");
        }

        // A long step is on the face for as long as it runs, however often
        // the daemon repeats the frame, and hands back to thinking at once.
        let at = |app: &mut App, t: f64| app.tick(1.0 / 60.0, 20.0 + t);
        at(&mut app, 0.0);
        app.on_daemon_message(narrated("THINKING", true, ""));
        app.on_daemon_message(narrated("ACTING", true, "ssh_exec"));
        for t in [0.5, 3.0, 30.0] {
            at(&mut app, t);
            app.on_daemon_message(narrated("ACTING", true, "ssh_exec"));
            assert_eq!(app.state(), Command, "{t} s into the command");
        }
        // Its event, when it returns, adds nothing: the daemon has said it all.
        app.on_daemon_message(event("screenshot", "ok"));
        assert_eq!(app.state(), Command);
        app.on_daemon_message(narrated("THINKING", true, ""));
        assert_eq!(app.state(), Thinking);
        assert_eq!(app.last_tool, "s", "the event still feeds the ticker");

        // A quick step has come and gone between two frames of this UI: it is
        // held for ACTION_HOLD_SECS from when it was first seen, no longer.
        at(&mut app, 40.0);
        app.on_daemon_message(narrated("ACTING", true, "screenshot"));
        app.on_daemon_message(event("screenshot", "ok"));
        app.on_daemon_message(narrated("THINKING", true, ""));
        assert_eq!(app.state(), Fetching);
        at(&mut app, 40.0 + f64::from(ACTION_HOLD_SECS) - 0.05);
        assert_eq!(app.state(), Fetching);
        at(&mut app, 40.0 + f64::from(ACTION_HOLD_SECS) + 0.05);
        assert_eq!(app.state(), Thinking);
        // A repeated frame does not start the hold again.
        at(&mut app, 50.0);
        app.on_daemon_message(narrated("ACTING", true, "ssh_exec"));
        at(&mut app, 51.0);
        app.on_daemon_message(narrated("ACTING", true, "ssh_exec"));
        at(&mut app, 51.1);
        app.on_daemon_message(narrated("THINKING", true, ""));
        assert_eq!(app.state(), Command, "1.1 s after it was first seen");
        at(&mut app, 51.3);
        assert_eq!(app.state(), Thinking);
        // The same kind twice in a row is two steps, each with its own hold.
        at(&mut app, 60.0);
        app.on_daemon_message(narrated("ACTING", true, "vnc_click"));
        app.on_daemon_message(narrated("THINKING", true, ""));
        at(&mut app, 61.0);
        app.on_daemon_message(narrated("ACTING", true, "vnc_click"));
        app.on_daemon_message(narrated("THINKING", true, ""));
        at(&mut app, 62.0);
        assert_eq!(app.state(), Working, "1 s after the second click");
        at(&mut app, 62.3);
        assert_eq!(app.state(), Thinking);

        // Still acting but no step named: the one that returned keeps what is
        // left of its hold, then it is plain work until the next is named.
        at(&mut app, 70.0);
        app.on_daemon_message(narrated("ACTING", true, "ssh_exec"));
        at(&mut app, 70.1);
        app.on_daemon_message(narrated("ACTING", true, ""));
        app.on_daemon_message(narrated("ACTING", true, ""));
        assert_eq!(app.state(), Command);
        at(&mut app, 71.3);
        assert_eq!(app.state(), Working);
        app.on_daemon_message(narrated("ACTING", true, "wait"));
        assert_eq!(app.state(), Fetching);
        app.on_daemon_message(narrated("ACTING", true, "type_text"));
        assert_eq!(app.state(), Working, "the next step replaces it at once");

        // What an approval was for is not needed: the daemon names what runs.
        app.on_daemon_message(narrated("THINKING", true, ""));
        app.on_daemon_message(approval_frame("r1"));
        assert_eq!(app.state(), WaitingApproval);
        assert!(app.key(crate::backend::LapisKey::Enter).is_some());
        app.on_daemon_message(narrated("ACTING", true, "ssh_exec"));
        assert_eq!(app.state(), Command, "not the approved vnc_key");

        // Push-to-talk and a pause still come first.
        assert!(app
            .key(crate::backend::LapisKey::PushToTalk(true))
            .is_some());
        assert_eq!(app.state(), Listening);
        assert!(app
            .key(crate::backend::LapisKey::PushToTalk(false))
            .is_some());
        app.on_daemon_message(narrated("PAUSED", true, "ssh_exec"));
        assert_eq!(app.state(), Paused);
    }

    #[test]
    fn a_daemon_without_action_is_followed_as_before() {
        use MascotState::*;
        // The same socket, a daemon from before `action`: the first frame
        // without the field switches back to reading steps from their events.
        let mut app = App::new();
        app.tick(1.0 / 60.0, 5.0);
        app.on_daemon_message(narrated("ACTING", true, "ssh_exec"));
        assert_eq!(app.state(), Command);
        app.on_daemon_message(sync("THINKING", true));
        app.tick(1.0 / 60.0, 8.0);
        app.on_daemon_message(sync("ACTING", true));
        assert_eq!(app.state(), Working, "nothing is known until the event");
        app.on_daemon_message(event("screenshot", "ok"));
        assert_eq!(app.state(), Fetching);
        app.on_daemon_message(sync("THINKING", true));
        assert_eq!(app.state(), Fetching, "held from the report");
        app.tick(1.0 / 60.0, 8.0 + f64::from(ACTION_HOLD_SECS) + 0.05);
        assert_eq!(app.state(), Thinking);
        // ... and its ending is guessed again.
        app.on_daemon_message(sync("IDLE", false));
        assert_eq!(app.state(), Done);

        // Losing the daemon forgets what kind it was.
        let mut app = App::new();
        app.on_daemon_message(narrated("ACTING", true, "ssh_exec"));
        app.set_offline(true);
        assert_eq!(app.state(), Idle);
        app.set_offline(false);
        app.on_daemon_message(sync("ACTING", true));
        assert_eq!(app.state(), Working);
        assert!(!app.daemon_narrates());
    }

    #[test]
    fn task_finished_decides_the_beat() {
        use MascotState::*;
        // A task from a daemon that narrates, as far as its last step.
        let task = |app: &mut App, last_status: &str| {
            app.on_daemon_message(narrated("THINKING", true, ""));
            app.on_daemon_message(narrated("ACTING", true, "vnc_click"));
            app.on_daemon_message(event("vnc_click", last_status));
        };
        let idle = || narrated("IDLE", false, "");
        let mut app = App::new();
        let mut now = 5.0;
        app.tick(1.0 / 60.0, now);

        for (status, state) in [
            ("done", Done),
            ("failed", Error),
            ("stopped", Error),
            ("cancelled", Idle),
            ("a_status_from_the_future", Idle),
        ] {
            task(&mut app, "ok");
            // The idle sync comes first and decides nothing by itself.
            app.on_daemon_message(idle());
            assert_eq!(app.state(), Idle, "{status}: no guess before the word");
            app.on_daemon_message(finished(status));
            assert_eq!(app.state(), state, "{status}");
            // The beat plays once, for BEAT_SECS, and repeats of the sync do not replay it.
            now += 0.1;
            app.tick(1.0 / 60.0, now);
            app.on_daemon_message(idle());
            assert_eq!(app.state(), state, "{status}");
            now += f64::from(BEAT_SECS) + 0.1;
            app.tick(1.0 / 60.0, now);
            assert_eq!(app.state(), Idle, "{status}: played out");
            app.on_daemon_message(idle());
            assert_eq!(app.state(), Idle);
        }

        // Nothing is read into how the last step went, or into toasts.
        task(&mut app, "error");
        app.on_daemon_message(toast("error"));
        app.on_daemon_message(idle());
        app.on_daemon_message(finished("done"));
        assert_eq!(app.state(), Done);
        app.on_daemon_message(toast("warn"));
        app.on_daemon_message(toast("error"));
        assert_eq!(app.state(), Done, "a toast does not rewrite the ending");
        task(&mut app, "ok");
        assert_eq!(app.state(), Working, "a new task ends the beat");
        app.on_daemon_message(idle());
        app.on_daemon_message(finished("failed"));
        assert_eq!(app.state(), Error);

        // Sent ahead of the idle sync, it waits for it.
        task(&mut app, "ok");
        app.on_daemon_message(finished("done"));
        assert_eq!(app.state(), Working);
        app.on_daemon_message(idle());
        assert_eq!(app.state(), Done);
        // A frame without a status says nothing.
        app.on_daemon_message(serde_json::json!({"type": "task_finished"}));
        assert_eq!(app.state(), Done);

        // The kill switch here: the daemon's `cancelled` agrees, no beat.
        task(&mut app, "ok");
        app.key(crate::backend::LapisKey::EscHold);
        app.on_daemon_message(idle());
        app.on_daemon_message(finished("cancelled"));
        app.on_daemon_message(toast("warn"));
        assert_eq!(app.state(), Idle);

        // The beat is drawn: Done on the status bar, and it eases back to idle.
        task(&mut app, "ok");
        app.on_daemon_message(idle());
        app.on_daemon_message(finished("stopped"));
        now += 10.0;
        app.tick(1.0 / 60.0, now);
        assert_eq!(app.header.state.label(), "ERROR");
        let (now, _) = run(&mut app, now, f64::from(BEAT_SECS) + 1.5);
        assert_eq!(app.header.state.label(), "IDLE");
        assert!(!app.mascot.is_moving(now));
    }

    /// Walks `app` from `from` for `secs` the way the live loops do: tick with
    /// the time since the last frame (clamped to 0.1 s), then wait out
    /// `frame_interval`. Returns the time reached and the frames drawn.
    fn run(app: &mut App, from: f64, secs: f64) -> (f64, usize) {
        let (mut now, mut prev, mut frames) = (from, from, 0);
        while now < from + secs {
            app.tick((now - prev).clamp(0.0, 0.1) as f32, now);
            prev = now;
            frames += 1;
            now += app.frame_interval().as_secs_f64();
        }
        (now, frames)
    }

    #[test]
    fn frame_governor_follows_the_head() {
        let fps60 = Duration::from_micros(16_667);
        let fps12 = Duration::from_micros(83_333);
        let mut app = App::new();
        fall_asleep(&mut app);
        // Idle on the companion view: 4 FPS for the sway, woken early for
        // blinks and glances.
        let mut now = 0.0;
        let (mut waits, mut short) = (0, 0);
        while now < 60.0 {
            app.tick(0.1, now);
            let wait = app.frame_interval();
            assert!(
                wait >= fps12 && wait <= Duration::from_millis(250),
                "{wait:?}"
            );
            short += usize::from(wait < Duration::from_millis(250));
            waits += 1;
            now += wait.as_secs_f64();
        }
        assert!(short > 0, "some blink was scheduled within a minute");
        assert!(waits < 60 * 5, "{waits} frames in an idle minute");
        // Other views have no head: a plain 1 FPS.
        app.set_view(View::Menu);
        for step in 0..600 {
            app.tick(0.1, 60.0 + f64::from(step) * 0.1);
            assert_eq!(app.frame_interval(), Duration::from_secs(1));
        }

        // A state change eases at full rate where the head is, then drops to
        // the active rate.
        app.set_view(View::Companion);
        app.on_daemon_message(sync("THINKING", true));
        app.tick(1.0 / 60.0, 200.0);
        assert_eq!(app.frame_interval(), fps60);
        for i in 1..=120 {
            app.tick(1.0 / 60.0, 200.0 + f64::from(i) / 60.0);
        }
        assert_eq!(app.frame_interval(), fps12);
        app.set_view(View::Ticker);
        app.on_daemon_message(sync("ACTING", true));
        app.tick(1.0 / 60.0, 203.0);
        assert_eq!(
            app.frame_interval(),
            fps12,
            "nothing eases off the companion view"
        );

        // With the backlight off the head asks for nothing: the plain tiers.
        app.set_view(View::Companion);
        app.set_backlight_off(true);
        app.on_daemon_message(sync("THINKING", true));
        app.tick(1.0 / 60.0, 204.0);
        assert_eq!(app.frame_interval(), fps12);
        app.on_daemon_message(sync("IDLE", false));
        app.tick(0.1, 205.0);
        assert_eq!(
            app.frame_interval(),
            fps12,
            "the Done beat, at the active rate"
        );
        for step in 0..300 {
            app.tick(0.1, 210.0 + f64::from(step) * 0.1);
            assert_eq!(app.frame_interval(), Duration::from_secs(1));
        }
        app.set_backlight_off(false);
        app.on_daemon_message(sync("THINKING", true));
        app.tick(1.0 / 60.0, 250.0);
        assert_eq!(app.frame_interval(), fps60);
    }

    #[test]
    fn the_governor_holds_its_rates_after_a_task() {
        // A spring that has settled must stay settled at the slow rates, or
        // every later second is a burst of 60 FPS frames.
        let mut app = App::new();
        fall_asleep(&mut app);
        let (now, _) = run(&mut app, 0.0, 5.0);
        // A task with a step in it: the step's event scrolls the ticker,
        // whose spring is stepped by the same slow frames.
        app.on_daemon_message(sync("THINKING", true));
        app.on_daemon_message(sync("ACTING", true));
        app.on_daemon_message(event("vnc_click", "ok"));
        app.on_daemon_message(sync("THINKING", true));
        // Three seconds for the step's hold and the easing, then ten of
        // settled thinking at 12 FPS.
        let (now, _) = run(&mut app, now, 3.0);
        assert_eq!(app.state(), MascotState::Thinking);
        let (now, frames) = run(&mut app, now, 10.0);
        assert!((119..=122).contains(&frames), "{frames} frames in 10 s");
        // Back to idle (through the Done beat): after it, an idle minute
        // costs what it did before the task.
        app.on_daemon_message(sync("IDLE", false));
        let (now, _) = run(&mut app, now, 4.0);
        assert_eq!(app.state(), MascotState::Idle);
        let (_, frames) = run(&mut app, now, 60.0);
        assert!(frames < 60 * 5, "{frames} frames in an idle minute");
        assert!(frames > 60 * 3, "{frames}: the sway is drawn at 4 FPS");
    }

    /// The pixels that are not paper in the head's zone of the companion view.
    fn head_pixels(app: &mut App) -> usize {
        head_pixels_into(app, &mut Canvas::new())
    }

    /// The same, leaving the rendered frame in `c`.
    fn head_pixels_into(app: &mut App, c: &mut Canvas) -> usize {
        app.render(c);
        let mut n = 0;
        for y in CONTENT_Y..124 {
            for x in 0..150 {
                n += usize::from(c.get_pixel(x, y) != Some(PAPER));
            }
        }
        n
    }

    #[test]
    fn the_head_survives_changes_nobody_watched() {
        // The head eases at 60 FPS only while it is on a lit screen. A change
        // taken on another view, or in the dark, is stepped at 12 FPS or
        // 1 FPS: it must still arrive, and the head must still be there.
        let unwatched: [fn(&mut App, bool); 2] = [
            |app, away| app.set_view(if away { View::Menu } else { View::Companion }),
            |app, away| app.set_backlight_off(away),
        ];
        for leave in unwatched {
            for agent in ["THINKING", "PAUSED", "IDLE"] {
                let mut app = App::new();
                fall_asleep(&mut app);
                app.on_daemon_message(sync("ACTING", true));
                let (now, _) = run(&mut app, 0.0, 3.0);
                leave(&mut app, true);
                app.on_daemon_message(sync(agent, agent != "IDLE"));
                let (now, _) = run(&mut app, now, 60.0);
                leave(&mut app, false);
                app.tick(0.1, now);
                assert!(!app.mascot.is_moving(now), "{agent}: still easing");
                assert_eq!(app.mascot.progress(), 1.0, "{agent}");
                assert!(app.frame_interval() > Duration::from_micros(16_667));

                // The same change watched at 60 FPS ends on the same frame.
                let mut seen = App::new();
                seen.on_daemon_message(sync(agent, agent != "IDLE"));
                for i in 0..=180 {
                    seen.tick(1.0 / 60.0, now - 3.0 + f64::from(i) / 60.0);
                }
                assert!(head_pixels(&mut seen) > 2000, "{agent}");
                assert_eq!(head_pixels(&mut app), head_pixels(&mut seen), "{agent}");
            }
        }
    }

    #[test]
    fn a_change_noticed_after_a_long_wait_still_eases() {
        // Coming from idle the frame that notices a change follows a wait of
        // up to a second. That time is not the new easing's: its first frame
        // shows the head where it was, and it never jumps past its target.
        for waited in [0.1, 1.0 / 12.0, 1.0 / 60.0] {
            let mut app = App::new();
            app.tick(waited, 10.0);
            app.on_daemon_message(sync("THINKING", true));
            app.tick(waited, 10.0 + f64::from(waited));
            assert_eq!(app.mascot.progress(), 0.0, "the frame of the change");
            let mut now = 10.0 + f64::from(waited);
            now += 1.0 / 60.0;
            app.tick(1.0 / 60.0, now);
            let first = app.mascot.progress();
            assert!(first > 0.0 && first < 0.1, "first eased frame at {first}");
            let mut peak = first;
            for _ in 0..120 {
                now += 1.0 / 60.0;
                app.tick(1.0 / 60.0, now);
                peak = peak.max(app.mascot.progress());
            }
            assert!(peak > 1.0 && peak < 1.1, "peak {peak}");
            assert_eq!(app.mascot.progress(), 1.0);
        }
    }

    #[test]
    fn a_ragged_loop_never_loses_the_head() {
        // The device's loop is not a metronome. It waits out the governor's
        // interval unless a key or a daemon frame cuts the wait to a
        // millisecond, now and then it stalls for seconds, the owner leaves
        // the companion view, the backlight goes off, and the daemon's
        // frames land wherever they land. Whatever the mix, every frame
        // drawn has a whole head in it and an untouched status bar.
        let mut seed = 0x2545_f491u32;
        let mut dice = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            f64::from(seed) / f64::from(u32::MAX)
        };
        let frames = [
            narrated("THINKING", true, ""),
            narrated("ACTING", true, "ssh_exec"),
            narrated("ACTING", true, "screenshot"),
            narrated("ACTING", true, "vnc_click"),
            narrated("ACTING", true, ""),
            narrated("PAUSED", true, ""),
            narrated("WAITING_APPROVAL", true, ""),
            narrated("IDLE", false, ""),
            finished("done"),
            finished("failed"),
            sync("ACTING", true),
            event("ssh_exec", "ok"),
            sync("THINKING", true),
            sync("IDLE", false),
        ];
        // The status bar right of the state label, over the head's column.
        let strip = |c: &Canvas| -> Vec<Option<u32>> {
            (0..=RULE_Y)
                .flat_map(|y| (52..150).map(move |x| (x, y)))
                .map(|(x, y)| c.get_pixel(x, y))
                .collect()
        };
        let mut app = App::new();
        fall_asleep(&mut app);
        app.on_daemon_message(narrated("IDLE", false, ""));
        let mut canvas = Canvas::new();
        app.render(&mut canvas);
        let bar = strip(&canvas);

        let (mut now, mut prev, mut shown) = (0.0f64, 0.0f64, app.state());
        let (mut changes, mut unwatched, mut dark) = (0, 0, false);
        for n in 0..700 {
            now += match dice() {
                early if early < 0.25 => early * 0.02,
                stall if stall > 0.97 => 2.5,
                _ => app.frame_interval().as_secs_f64(),
            };
            if dice() < 0.15 {
                let pick = (dice() * frames.len() as f64) as usize % frames.len();
                app.on_daemon_message(frames[pick].clone());
            }
            if dice() < 0.03 {
                let away = app.view == View::Companion;
                app.set_view(if away { View::Menu } else { View::Companion });
            }
            if dice() < 0.03 {
                dark = !dark;
                app.set_backlight_off(dark);
            }
            let watched = app.view == View::Companion && !dark;

            // As the live loops tick: the time since the last frame, capped.
            app.tick((now - prev).clamp(0.0, 0.1) as f32, now);
            prev = now;
            let w = app.mascot.progress();
            assert!((0.0..1.06).contains(&w), "frame {n}: blend at {w}");
            if app.header.state != shown {
                shown = app.header.state;
                // However long the wait before it, the frame that notices a
                // change still shows the head where it was.
                assert_eq!(w, 0.0, "frame {n}: {shown:?} began part-way");
                changes += 1;
                unwatched += usize::from(!watched);
            }
            if app.view == View::Companion {
                let pixels = head_pixels_into(&mut app, &mut canvas);
                assert!(pixels > 1400, "frame {n}: {pixels} pixels of head");
                assert!(strip(&canvas) == bar, "frame {n}: the status bar");
            }
        }
        assert!(changes > 40 && unwatched > 5, "{changes} / {unwatched}");

        // Left alone where it can be seen, it comes to rest and stays there.
        app.set_view(View::Companion);
        app.set_backlight_off(false);
        let (now, _) = run(&mut app, now, 5.0);
        assert!(!app.mascot.is_moving(now));
        assert_eq!(app.mascot.progress(), 1.0);
        assert!(app.frame_interval() > Duration::from_micros(16_667));
        assert!(head_pixels(&mut app) > 2000);
    }

    #[test]
    fn a_month_of_uptime_still_animates() {
        // The clock is f64 end to end: after thirty days a 60 FPS frame is
        // still a different frame, and an action is still held for 1.2 s.
        let month = 30.0 * 86_400.0;
        let mut app = App::new();
        app.pin_state(MascotState::Working, true);
        app.tick(1.0 / 60.0, month);
        let mut a = Canvas::new();
        app.render(&mut a);
        app.tick(1.0 / 60.0, month + 0.1);
        let mut b = Canvas::new();
        app.render(&mut b);
        assert_ne!(a.pixels(), b.pixels());

        let mut app = App::new();
        app.tick(1.0 / 60.0, month);
        app.on_daemon_message(sync("ACTING", true));
        app.on_daemon_message(event("screenshot", "ok"));
        app.on_daemon_message(sync("THINKING", true));
        app.tick(1.0 / 60.0, month + 1.0);
        assert_eq!(app.state(), MascotState::Fetching);
        app.tick(1.0 / 60.0, month + 1.25);
        assert_eq!(app.state(), MascotState::Thinking);
        app.on_daemon_message(toast("info"));
        assert!(app.now < app.toast_until && app.toast_until - app.now < 4.01);
    }

    #[test]
    fn pinned_states_and_the_demo_cover_every_state() {
        let mut app = App::new();
        for (i, state) in MascotState::ALL.into_iter().enumerate() {
            app.demo_tick(i as f64 * 2.5 + 0.1);
            assert_eq!(app.state(), state);
            app.pin_state(state, true);
            app.on_daemon_message(sync("THINKING", true));
            assert_eq!(app.state(), state, "a pinned state ignores the daemon");
            app.tick(1.0 / 60.0, 0.5);
            let (mut a, mut b) = (Canvas::new(), Canvas::new());
            app.render(&mut a);
            app.render(&mut b);
            assert_eq!(a.pixels(), b.pixels(), "{state:?} renders the same twice");
        }
    }

    /// The demo `pairing` frames of SPEC 6.3.
    fn pin_frame(status: &str) -> Value {
        serde_json::json!({"type": "pairing", "mode": "pin", "pin": "482917",
            "device_name": "LapisZero-4F2A", "status": status, "expires_in": 300})
    }

    fn qr_frame() -> Value {
        serde_json::json!({"type": "pairing", "mode": "qr", "payload": pairing::DEMO_PAYLOAD,
            "host": "lapis-zero.tailnet.ts.net", "pin": "482917", "expires_in": 180})
    }

    fn approval_frame(id: &str) -> Value {
        serde_json::json!({"type": "approval_request", "request_id": id, "host": "arch-dev",
            "action": "vnc_key", "summary": "press ctrl+c", "risk": "low",
            "preview_1bit_120x68_b64": null})
    }

    /// Pushes the idle timer back so a wake is observable.
    fn fall_asleep(app: &mut App) {
        app.last_input = Instant::now()
            .checked_sub(Duration::from_secs(120))
            .expect("monotonic clock older than two minutes");
        assert!(app.idle_ms() >= 90_000);
    }

    #[test]
    fn pairing_frames_open_the_view_and_off_restores_the_previous_one() {
        use serde_json::json;
        let mut app = App::new();
        app.set_view(View::Menu);
        fall_asleep(&mut app);

        assert!(!app.keeps_screen_on());

        // PIN mode takes the screen and lights it.
        app.on_daemon_message(pin_frame("waiting"));
        assert_eq!(app.view, View::Pairing);
        assert!(app.idle_ms() < 1000, "a new PIN wakes the screen");
        // ... and keeps it lit however long the user takes to find their phone.
        fall_asleep(&mut app);
        assert!(app.keeps_screen_on());
        app.wake();
        let p = app.pairing.as_ref().unwrap();
        assert_eq!((p.mode, p.pin.as_str()), (pairing::Mode::Pin, "482917"));
        assert_eq!(
            (p.name.as_str(), p.status.as_str()),
            ("LapisZero-4F2A", "waiting")
        );
        assert!(app.qr_rect().is_none());
        app.render(&mut Canvas::new());

        // A status update refreshes in place; the view to return to is unchanged.
        app.on_daemon_message(pin_frame("verified"));
        assert_eq!(app.view, View::Pairing);
        assert_eq!(app.pairing.as_ref().unwrap().status, "verified");

        // QR mode replaces it, with the matrix built on arrival.
        app.on_daemon_message(qr_frame());
        let p = app.pairing.as_ref().unwrap();
        assert_eq!(p.mode, pairing::Mode::Qr);
        assert_eq!(p.name, "lapis-zero.tailnet.ts.net");
        assert_eq!(p.qr.as_ref().unwrap().modules(), 41);
        assert_eq!(app.qr_rect(), p.qr.as_ref().map(pairing::Qr::box_rect));
        app.render(&mut Canvas::new());

        // `off` goes back to the view that was showing before, exactly once.
        app.on_daemon_message(json!({"type": "pairing", "mode": "off"}));
        assert_eq!(app.view, View::Menu);
        assert!(app.pairing.is_none());
        app.set_view(View::Ticker);
        app.on_daemon_message(json!({"type": "pairing", "mode": "off"}));
        assert_eq!(app.view, View::Ticker, "a stray off changes nothing");

        // Unknown modes are ignored; `state_sync` with `pairing: null` also
        // closes a view whose daemon went away without saying `off`.
        app.on_daemon_message(json!({"type": "pairing", "mode": "starting"}));
        assert_eq!(app.view, View::Ticker);
        app.on_daemon_message(qr_frame());
        assert_eq!(app.view, View::Pairing);
        app.on_daemon_message(json!({"type": "state_sync", "pairing": {"mode": "qr"}}));
        assert_eq!(app.view, View::Pairing);
        app.on_daemon_message(json!({"type": "state_sync", "pairing": null}));
        assert_eq!(app.view, View::Ticker);
    }

    #[test]
    fn pairing_view_is_modal_and_esc_cancels_it() {
        use crate::backend::LapisKey as K;
        use serde_json::json;
        let mut app = App::new();
        app.set_view(View::Ticker);
        app.on_daemon_message(qr_frame());

        // Not part of the Z/C cycle, in either direction, from any view.
        for v in View::ALL {
            assert_ne!(v.cycle(1), View::Pairing);
            assert_ne!(v.cycle(-1), View::Pairing);
        }
        assert_eq!(View::from_name("pairing"), None);
        // Navigation and text keys do nothing while it is up.
        let menu_row = app.menu.index;
        for key in [
            K::CycleL,
            K::CycleR,
            K::Up,
            K::Down,
            K::Enter,
            K::Tab,
            K::Char('c'),
            K::Char(' '),
        ] {
            assert_eq!(app.key(key), None);
            assert_eq!(app.view, View::Pairing);
        }
        assert_eq!(app.menu.index, menu_row);
        // Push-to-talk edges still reach the daemon (a release must not be lost).
        assert_eq!(
            app.key(K::PushToTalk(false)),
            Some(json!({"type": "ptt_audio", "state": "stop"}))
        );

        // ESC stops the window and leaves for the previous view.
        assert_eq!(
            app.key(K::Esc),
            Some(json!({"type": "pairing_control", "command": "stop"}))
        );
        assert_eq!(app.view, View::Ticker);
        assert!(app.pairing.is_none());
        // The daemon's answering `off` is then a no-op.
        app.on_daemon_message(json!({"type": "pairing", "mode": "off"}));
        assert_eq!(app.view, View::Ticker);

        // The kill switch stays global and does not close the pairing window.
        app.on_daemon_message(pin_frame("waiting"));
        assert_eq!(
            app.key(K::EscHold),
            Some(json!({"type": "agent_control", "command": "kill_all"}))
        );
        assert_eq!(app.view, View::Pairing);
    }

    #[test]
    fn menu_row_starts_pairing() {
        use crate::backend::LapisKey as K;
        use serde_json::json;
        let mut app = App::new();
        app.set_view(View::Menu);
        app.key(K::Up); // wraps from the first row to the last
        assert_eq!(app.menu.index, 6);
        assert_eq!(
            app.key(K::Enter),
            Some(json!({"type": "pairing_control", "command": "start"}))
        );
        // The view only opens once the daemon answers with a `pairing` frame.
        assert_eq!(app.view, View::Menu);
        app.on_daemon_message(qr_frame());
        assert_eq!(app.view, View::Pairing);
        app.key(K::Esc);
        assert_eq!(app.view, View::Menu);
        // The row above still opens the network view.
        app.key(K::Up);
        app.key(K::Enter);
        assert_eq!(app.view, View::Setup);
    }

    #[test]
    fn approval_resolved_dismisses_the_matching_modal_silently() {
        use crate::backend::LapisKey as K;
        use serde_json::json;
        let resolved = |id: &str| {
            json!({"type": "approval_resolved", "request_id": id,
            "decision": "approve_once", "by": "android:Pixel"})
        };
        let mut app = App::new();
        app.set_view(View::Ticker);
        app.on_daemon_message(approval_frame("req_99"));
        assert_eq!(app.view, View::Approval);
        assert_eq!(
            app.approval.as_ref().unwrap().kind,
            approval::ActionKind::VncKey
        );

        // Someone else's request, or no id at all: the modal stays.
        app.on_daemon_message(resolved("req_98"));
        app.on_daemon_message(json!({"type": "approval_resolved"}));
        assert!(app.approval.is_some());
        assert_eq!(app.state(), MascotState::WaitingApproval);

        // The match: same end state as a local decision, nothing sent.
        app.on_daemon_message(resolved("req_99"));
        assert!(app.approval.is_none());
        assert_eq!(app.state(), MascotState::Idle);
        assert_eq!(app.view, View::Ticker);
        assert!(app.take_outbox().is_empty());
        assert_eq!(app.key(K::Enter), None, "no modal left to approve");

        // A local decision ends in the same place (and does tell the daemon).
        app.on_daemon_message(approval_frame("req_100"));
        assert_eq!(
            app.key(K::Esc),
            Some(json!({"type": "approval_decision", "request_id": "req_100", "decision": "deny"}))
        );
        assert_eq!((app.view, app.state()), (View::Ticker, MascotState::Idle));
        // Its echo from the daemon finds nothing to dismiss.
        app.on_daemon_message(resolved("req_100"));
        assert_eq!(app.view, View::Ticker);

        // A modal the user navigated away from is cleared without moving them.
        app.on_daemon_message(approval_frame("req_101"));
        app.key(K::CycleR);
        let elsewhere = app.view;
        assert_ne!(elsewhere, View::Approval);
        app.on_daemon_message(resolved("req_101"));
        assert!(app.approval.is_none());
        assert_eq!(app.view, elsewhere);

        // The synthetic demo modal (empty id) is never matched.
        app.show_synthetic_approval();
        app.on_daemon_message(resolved(""));
        assert!(app.approval.is_some());
    }

    #[test]
    fn approval_and_pairing_overlays_nest_and_unwind() {
        use crate::backend::LapisKey as K;
        use serde_json::json;
        let off = || json!({"type": "pairing", "mode": "off"});
        let mut app = App::new();
        app.set_view(View::Ticker);

        // Modal over the pairing view: deciding returns to pairing, off to the ticker.
        app.on_daemon_message(pin_frame("waiting"));
        app.on_daemon_message(approval_frame("a"));
        assert_eq!(app.view, View::Approval);
        // A pairing status update must not pull the screen from the modal.
        app.on_daemon_message(pin_frame("connected"));
        assert_eq!(app.view, View::Approval);
        assert!(app.key(K::Enter).is_some());
        assert_eq!(app.view, View::Pairing);
        assert_eq!(app.pairing.as_ref().unwrap().status, "connected");
        app.on_daemon_message(off());
        assert_eq!(app.view, View::Ticker);

        // Pairing closes underneath the modal: deciding returns to the ticker.
        app.on_daemon_message(pin_frame("waiting"));
        app.on_daemon_message(approval_frame("b"));
        app.on_daemon_message(off());
        assert_eq!(app.view, View::Approval);
        assert!(app.key(K::Enter).is_some());
        assert_eq!(app.view, View::Ticker);

        // Pairing over a pending modal: off returns to the modal, which then
        // returns to the ticker. A second request re-stacks on top.
        app.on_daemon_message(approval_frame("c"));
        app.on_daemon_message(qr_frame());
        assert_eq!(app.view, View::Pairing);
        app.on_daemon_message(approval_frame("d"));
        assert_eq!(app.view, View::Approval);
        assert!(app.key(K::Enter).is_some());
        assert_eq!(app.view, View::Pairing);
        app.on_daemon_message(off());
        assert_eq!(app.view, View::Ticker);

        // Pairing over a modal that is resolved elsewhere meanwhile.
        app.on_daemon_message(approval_frame("e"));
        app.on_daemon_message(qr_frame());
        app.on_daemon_message(json!({"type": "approval_resolved", "request_id": "e"}));
        assert_eq!(app.view, View::Pairing);
        app.on_daemon_message(off());
        assert_eq!(app.view, View::Ticker);

        // A modal the user cycled away from no longer holds the pairing view
        // down: the next status update brings it back over where they went.
        app.on_daemon_message(pin_frame("waiting"));
        app.on_daemon_message(approval_frame("f"));
        app.key(K::CycleR);
        assert_eq!(app.view, View::Prompt);
        app.on_daemon_message(pin_frame("wifi"));
        assert_eq!(app.view, View::Pairing);
        app.on_daemon_message(off());
        assert_eq!(app.view, View::Prompt);
    }

    #[test]
    fn ui_settings_apply_theme_brightness_and_wake() {
        use serde_json::json;
        let mut app = App::new();
        let mut light = Canvas::new();
        app.render(&mut light);
        assert_eq!(light.get_pixel(0, RULE_Y), Some(INK));
        assert_eq!(light.get_pixel(319, 169), Some(PAPER));
        assert_eq!(app.take_brightness(), None);

        // The full frame the daemon sends.
        fall_asleep(&mut app);
        app.on_daemon_message(json!({"type": "ui_settings", "theme": "dark",
            "brightness": 80, "mute_audio": false, "wake": true}));
        assert!(app.idle_ms() < 1000, "wake: true resets the idle timer");
        assert_eq!(app.take_brightness(), Some(80));
        assert_eq!(app.take_brightness(), None, "handed over once");

        // Dark: every INK and PAPER pixel swapped, nothing else touched...
        let mut dark = Canvas::new();
        app.render(&mut dark);
        assert_eq!(dark.get_pixel(0, RULE_Y), Some(PAPER));
        assert_eq!(dark.get_pixel(319, 169), Some(INK));
        let mut unswapped = Canvas::new();
        unswapped.blit(&dark, 0, 0);
        unswapped.swap_ink_paper(None);
        // ... except the head, which keeps its own light: a negative of it
        // would be lit from below. Its box is identical in both themes.
        let (hx, hy, hr) = companion::HEAD;
        let (half_w, half_h) = ((1.3 * hr) as i32, hr as i32 + 3);
        let in_head = |x: i32, y: i32| (x - hx).abs() <= half_w && (y - hy).abs() <= half_h;
        for y in 0..crate::gfx::canvas::HEIGHT {
            for x in 0..WIDTH {
                let (l, d) = (light.get_pixel(x, y), dark.get_pixel(x, y));
                if !in_head(x, y) {
                    assert_eq!(unswapped.get_pixel(x, y), l, "pixel {x},{y}");
                } else if l != Some(PAPER) {
                    // Inside the box only the background and the contour swap.
                    assert!(
                        l == d || (l, d) == (Some(INK), Some(PAPER)),
                        "pixel {x},{y}"
                    );
                }
            }
        }
        let (lit_x, lit_y) = (hx - hr as i32 / 2, hy - hr as i32 * 2 / 3);
        assert_eq!(light.get_pixel(lit_x, lit_y), Some(PAPER), "lit shell");
        assert_eq!(
            dark.get_pixel(lit_x, lit_y),
            Some(PAPER),
            "lit shell, dark theme"
        );

        // Any subset of fields: each one leaves the others alone.
        fall_asleep(&mut app);
        app.on_daemon_message(json!({"type": "ui_settings", "wake": false}));
        app.on_daemon_message(json!({"type": "ui_settings", "mute_audio": true}));
        assert!(app.dark && app.idle_ms() >= 90_000);
        assert_eq!(app.take_brightness(), None);
        // A repeated value does not undo a local fn+U / fn+I step; a new one applies.
        app.on_daemon_message(json!({"type": "ui_settings", "brightness": 80}));
        assert_eq!(app.take_brightness(), None);
        app.on_daemon_message(json!({"type": "ui_settings", "brightness": 250}));
        assert_eq!(app.take_brightness(), Some(100), "clamped to a percentage");
        app.on_daemon_message(json!({"type": "ui_settings", "brightness": 0}));
        assert_eq!(app.take_brightness(), Some(0));
        assert!(app.dark, "brightness-only frames keep the theme");
        app.on_daemon_message(json!({"type": "ui_settings", "theme": "paper"}));
        assert!(!app.dark);
        let mut back = Canvas::new();
        app.render(&mut back);
        assert_eq!(back.pixels(), light.pixels());
    }

    #[test]
    fn dark_theme_leaves_the_qr_dark_on_paper() {
        let mut app = App::new();
        app.on_daemon_message(qr_frame());
        let mut light = Canvas::new();
        app.render(&mut light);
        app.on_daemon_message(serde_json::json!({"type": "ui_settings", "theme": "dark"}));
        let mut dark = Canvas::new();
        app.render(&mut dark);

        let (bx, by, bw, bh) = app.qr_rect().expect("QR on screen");
        let (mut ink, mut paper) = (0, 0);
        for y in 0..crate::gfx::canvas::HEIGHT {
            for x in 0..WIDTH {
                let (l, d) = (light.get_pixel(x, y), dark.get_pixel(x, y));
                if (bx..bx + bw).contains(&x) && (by..by + bh).contains(&y) {
                    assert_eq!(l, d, "QR box pixel {x},{y} must not invert");
                    match d {
                        Some(INK) => ink += 1,
                        Some(PAPER) => paper += 1,
                        other => panic!("unexpected colour {other:?} in the QR box"),
                    }
                } else if l == Some(INK) {
                    assert_eq!(d, Some(PAPER));
                } else if l == Some(PAPER) {
                    assert_eq!(d, Some(INK));
                } else {
                    assert_eq!(l, d, "accents are not themed");
                }
            }
        }
        assert!(ink > 3000 && paper > ink, "dark modules on a paper box");
        // The quiet zone is paper even though the surrounding frame is now ink.
        assert_eq!(dark.get_pixel(bx, by), Some(PAPER));
        assert_eq!(dark.get_pixel(bx + bw, by), Some(INK));

        // The PIN screen has no such exemption.
        app.on_daemon_message(pin_frame("waiting"));
        assert!(app.qr_rect().is_none());
    }

    /// Decodes a PNG to `(width, height, RGB888 bytes)`.
    fn decode_png(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
        let mut dec = png::Decoder::new(bytes);
        dec.set_transformations(png::Transformations::EXPAND);
        let mut reader = dec.read_info().expect("PNG header");
        let mut buf = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).expect("PNG image data");
        buf.truncate(info.buffer_size());
        (info.width, info.height, buf)
    }

    #[test]
    fn screen_request_replies_with_a_png_of_the_last_frame() {
        use serde_json::json;
        let mut app = App::new();
        app.set_view(View::Menu);
        app.on_daemon_message(json!({"type": "ui_settings", "theme": "dark"}));
        let mut shown = Canvas::new();
        app.render(&mut shown);
        // The reply is the frame as presented, even if the state moved on since.
        app.set_view(View::Ticker);

        app.on_daemon_message(json!({"type": "screen_request", "id": "a1b2c3d4"}));
        app.on_daemon_message(json!({"type": "screen_request"})); // no id: no reply
        let replies = app.take_outbox();
        assert_eq!(replies.len(), 1);
        assert!(app.take_outbox().is_empty(), "drained");
        let reply = &replies[0];
        assert_eq!(reply["type"], "screen_snapshot");
        assert_eq!(reply["id"], "a1b2c3d4");
        assert_eq!(reply.as_object().unwrap().len(), 3);

        let b64 = reply["png_b64"].as_str().unwrap();
        let png_bytes = approval::b64_decode(b64.as_bytes()).unwrap();
        assert_eq!(&png_bytes[..8], b"\x89PNG\r\n\x1a\n");
        let (w, h, rgb) = decode_png(&png_bytes);
        assert_eq!((w, h), (320, 170));
        assert_eq!(rgb, shown.pixels());
        // Small enough for one NDJSON line well under any socket buffer worry.
        assert!(b64.len() < 16 * 1024, "{} bytes of base64", b64.len());

        // Asked before anything was presented: render a frame rather than send a blank.
        let mut fresh = App::new();
        fresh.on_daemon_message(json!({"type": "screen_request", "id": "first"}));
        let reply = fresh.take_outbox().remove(0);
        let png_bytes =
            approval::b64_decode(reply["png_b64"].as_str().unwrap().as_bytes()).unwrap();
        let (w, h, rgb) = decode_png(&png_bytes);
        assert_eq!((w, h), (320, 170));
        assert!(rgb.chunks_exact(3).any(|px| px == [0x0B, 0x0B, 0x0D]));

        // A burst of requests with nobody draining stays bounded.
        for i in 0..50 {
            fresh.on_daemon_message(json!({"type": "screen_request", "id": format!("r{i}")}));
        }
        assert_eq!(fresh.take_outbox().len(), 8);
    }

    #[test]
    fn battery_comes_from_state_sync() {
        use serde_json::json;
        /// Ink pixels in the status bar's battery/clock corner.
        fn corner(app: &mut App) -> Vec<(i32, i32)> {
            let mut c = Canvas::new();
            app.render(&mut c);
            let mut on = Vec::new();
            for y in 0..RULE_Y {
                for x in 240..WIDTH {
                    if c.get_pixel(x, y) == Some(INK) {
                        on.push((x, y));
                    }
                }
            }
            on
        }
        let mut app = App::new();
        assert_eq!((app.header.battery, app.header.charging), (Some(84), false));
        let demo = corner(&mut app);

        app.on_daemon_message(
            json!({"type": "state_sync", "battery_pct": 37, "battery_charging": true}),
        );
        assert_eq!((app.header.battery, app.header.charging), (Some(37), true));
        assert_eq!(app.setup.battery_pct, Some(37));
        let charging = corner(&mut app);
        app.on_daemon_message(json!({"type": "state_sync", "battery_charging": false}));
        assert_eq!(
            (app.header.battery, app.header.charging),
            (Some(37), false),
            "missing key keeps the value"
        );
        let discharging = corner(&mut app);
        assert_ne!(demo, discharging, "84% and 37% draw differently");
        assert!(charging.len() > discharging.len(), "the bolt adds ink");

        // No fuel gauge: `--%`, never a stale or made-up number.
        app.on_daemon_message(json!({"type": "state_sync", "battery_pct": null}));
        assert_eq!(app.header.battery, None);
        assert_eq!(app.setup.battery_pct, None);
        let mut b = [0u8; 4];
        assert_eq!(fmt_battery(&mut b, None), "--%");
        assert_eq!(fmt_battery(&mut b, Some(100)), "100%");
        assert_ne!(corner(&mut app), discharging);
        for v in View::ALL {
            app.set_view(v);
            app.render(&mut Canvas::new());
        }

        // Out-of-range and fractional values are clamped, not rejected.
        app.on_daemon_message(json!({"type": "state_sync", "battery_pct": 250.4}));
        assert_eq!(app.header.battery, Some(100));
        app.on_daemon_message(json!({"type": "state_sync", "battery_pct": 61.6}));
        assert_eq!(app.header.battery, Some(62));
    }

    #[test]
    fn view_names_open_every_cli_view() {
        for name in VIEW_NAMES {
            let mut app = App::new();
            assert!(app.open_named(name), "{name}");
            let mut c = Canvas::new();
            app.render(&mut c);
            assert_eq!(c.get_pixel(0, RULE_Y), Some(INK));
            match name {
                "pairing_pin" => {
                    assert_eq!(app.view, View::Pairing);
                    let p = app.pairing.as_ref().unwrap();
                    assert_eq!(
                        (p.mode, p.pin.as_str()),
                        (pairing::Mode::Pin, pairing::DEMO_PIN)
                    );
                    assert_eq!(p.name, pairing::DEMO_NAME);
                }
                "pairing_qr" => {
                    assert_eq!(app.view, View::Pairing);
                    let p = app.pairing.as_ref().unwrap();
                    assert_eq!(p.mode, pairing::Mode::Qr);
                    assert!(p.qr.is_some());
                }
                _ => assert_eq!(app.view.name(), name),
            }
            // Demo frames are deterministic: a second render is identical.
            let mut again = Canvas::new();
            app.render(&mut again);
            assert_eq!(c.pixels(), again.pixels(), "{name}");
        }
        let mut app = App::new();
        assert!(!app.open_named("pairing"));
        assert!(!app.open_named("nope"));
        assert_eq!(app.view, View::Companion);
        // The bare view without data renders just the status bar.
        app.set_view(View::Pairing);
        app.render(&mut Canvas::new());
        assert_eq!(
            app.key(crate::backend::LapisKey::Esc).unwrap()["command"],
            "stop"
        );
        assert_eq!(app.view, View::Companion);
    }

    #[test]
    fn letter_keys_navigate_outside_prompt_but_type_inside_it() {
        use crate::backend::LapisKey as K;
        let mut app = App::new();
        app.set_view(View::Companion);
        app.key(K::Char('c'));
        assert_eq!(app.view, View::Ticker);
        app.key(K::Char('z'));
        assert_eq!(app.view, View::Companion);
        // SPACE (or `/`) on the home view opens the prompt (SPEC 5.3)...
        app.key(K::Char(' '));
        assert_eq!(app.view, View::Prompt);
        // ...where the same letters are text, not navigation.
        for ch in "fix zc".chars() {
            app.key(K::Char(ch));
        }
        assert_eq!(app.view, View::Prompt);
        assert_eq!(app.prompt.buffer, b"fix zc");
    }

    #[test]
    fn offline_blanks_daemon_state_instead_of_showing_demo_data() {
        let mut app = App::new();
        app.on_daemon_message(serde_json::json!({
            "type": "approval_request", "request_id": "req_1", "host": "mac",
            "action": "ssh_exec", "summary": "ls", "risk": "low",
            "preview_1bit_120x68_b64": null
        }));
        assert_eq!(app.view, View::Approval);
        app.set_offline(true);
        assert!(app.offline && app.approval.is_none());
        assert_eq!(app.view, View::Companion);
        assert!(app.header.host.is_empty() && !app.header.tailscale);
        assert_eq!(app.header.battery, None);

        // The status bar says so, in amber, where the host label normally is.
        let mut canvas = Canvas::new();
        app.render(&mut canvas);
        let amber = (0..WIDTH)
            .any(|x| (0..STATUS_BAR_H).any(|y| canvas.get_pixel(x, y) == Some(ACCENT_AMBER)));
        assert!(amber);

        // Reconnect: the next state_sync restores real data.
        app.set_offline(false);
        app.on_daemon_message(serde_json::json!({
            "type": "state_sync", "agent_state": "IDLE", "active_host": "studio",
            "hosts": ["studio"], "tailscale_state": "connected", "battery_pct": 61
        }));
        assert_eq!(app.header.host, "studio");
        assert_eq!(app.header.battery, Some(61));
    }

    #[test]
    fn first_sync_without_tailscale_opens_setup_once() {
        let offline_net = serde_json::json!({
            "type": "state_sync", "agent_state": "IDLE", "active_host": "",
            "hosts": [], "tailscale_state": "needs_login", "wifi_ssid": ""
        });
        let mut app = App::new();
        app.on_daemon_message(offline_net.clone());
        assert_eq!(app.view, View::Setup);
        // Only offered once: leaving it is respected on later syncs.
        app.key(crate::backend::LapisKey::Esc);
        assert_eq!(app.view, View::Companion);
        app.on_daemon_message(offline_net);
        assert_eq!(app.view, View::Companion);

        // A device that is already online starts on the companion view.
        let mut app = App::new();
        app.on_daemon_message(serde_json::json!({
            "type": "state_sync", "agent_state": "IDLE", "tailscale_state": "connected"
        }));
        assert_eq!(app.view, View::Companion);
    }

    #[test]
    fn desk_clock_follows_the_system_clock_and_daemon_timezone() {
        let mut app = App::new();
        assert_eq!(app.header.minutes, 14 * 60 + 32); // pinned until a live loop feeds it
        app.set_wall_clock(1_795_438_264); // 12:51:04 UTC
        assert_eq!(app.header.minutes, 12 * 60 + 51);
        app.on_daemon_message(serde_json::json!({
            "type": "state_sync", "agent_state": "IDLE", "tailscale_state": "connected",
            "utc_offset_min": -480
        }));
        app.set_wall_clock(1_795_438_264);
        assert_eq!(app.header.minutes, 4 * 60 + 51); // 04:51 at UTC-8
                                                     // Offsets that cross midnight wrap instead of going negative.
        app.on_daemon_message(serde_json::json!({
            "type": "state_sync", "agent_state": "IDLE", "utc_offset_min": 780
        }));
        app.set_wall_clock(1_795_438_264);
        assert_eq!(app.header.minutes, 60 + 51); // 01:51 next day at UTC+13
    }
}
