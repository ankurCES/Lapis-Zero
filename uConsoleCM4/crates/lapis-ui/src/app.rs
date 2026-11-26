//! The HUD frame: a top bar with the seven workspaces, a sidebar with the current workspace's
//! sections (and the deck's character), the content area, and a guide bar that always says
//! what each button does right now. Overlays: approvals, the command palette, the controls
//! help, toasts. Backends (KMS on the device, a window on a laptop) call [`DeckApp::frame`]
//! once per frame.
//!
//! Navigation: SELECT / START (or Alt+1..7, or a click) change workspace; in the sidebar ▲▼
//! choose a section and A or ▶ enter it; in the content B or ◀ come back. Y opens the command
//! palette, F1 or ? the help, SELECT+START is the kill switch.

use crate::character::{self, Mood};
use crate::client::{Client, Inbound, Tag};
use crate::input::{Pad, PadEvent};
use crate::model::{Image, Model};
use crate::theme::{self, AMBER, CRIMSON, CYAN, DIM, FAINT, LINE, RED, TEXT};
use crate::widgets;
use crate::{term, ws};
use egui::{
    Align2, Color32, Context, CornerRadius, FontId, Key, Pos2, Rect, RichText, Sense, Stroke,
    StrokeKind, TextureHandle, TextureOptions, Vec2,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const WORKSPACES: [&str; 7] = [
    "AGENT",
    "RF/SDR",
    "MESH/GPS",
    "NETOPS",
    "FLEET/KVM",
    "PTY/MUX",
    "SYSTEM",
];
/// Short tab names for the top bar (the full ones are in the sidebar and the palette).
pub const TABS: [&str; 7] = ["AGENT", "RF", "MESH", "NET", "FLEET", "PTY", "SYS"];
pub const ICONS: [&str; 7] = ["💬", "📡", "📟", "🌐", "🖥", "⌨", "⚙"];
pub const AGENT: usize = 0;
pub const RF: usize = 1;
pub const MESH: usize = 2;
pub const NET: usize = 3;
pub const FLEET: usize = 4;
pub const PTY: usize = 5;
pub const SYS: usize = 6;

/// One frame's navigation intent, from the D-pad/arrows, A/B/X and Enter/Esc.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Nav {
    pub up: bool,
    pub down: bool,
    pub left: bool,
    pub right: bool,
    /// A / Enter: select, activate, approve.
    pub a: bool,
    /// B / Esc: back, cancel, deny.
    pub b: bool,
    /// X: the context action.
    pub x: bool,
    /// A text field has the keyboard.
    pub typing: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Workspace(usize),
    /// Open a workspace's section with the content focused.
    Section(usize, usize),
    Kill,
    Pause,
    Resume,
    PairQr,
    PairBle,
    StopPairing,
    Tool(&'static str, Value),
    Tune(u64),
    Theme,
    NewShell,
    Serial(String, u32),
    Split,
    Help,
    Ask(String),
}

pub struct Palette {
    pub query: String,
    pub sel: usize,
    pub opened: bool,
}

pub enum Capture {
    Upload(String),
    File(std::path::PathBuf),
}

pub struct DeckApp {
    pub client: Client,
    pub m: Model,
    pub ws: usize,
    /// The sidebar's selected section, per workspace.
    pub section: [usize; 7],
    /// The content (not the sidebar) has the D-pad, per workspace.
    pub content: [bool; 7],
    pub sel: [[usize; 4]; 7],
    pub palette: Option<Palette>,
    pub help: bool,
    /// Written when the first-boot help is closed, so it shows once.
    pub help_marker: Option<std::path::PathBuf>,
    pub agent: ws::agent::State,
    pub rf: ws::rf::State,
    pub mesh: ws::mesh::State,
    pub net: ws::net::State,
    pub fleet: ws::fleet::State,
    pub pty: ws::pty::State,
    pub sys: ws::sys::State,
    pub terms: term::Terms,
    pub capture: Option<Capture>,
    pub screenshot_after: Option<(u32, std::path::PathBuf)>,
    pub frames: u32,
    pub paper: bool,
    /// The content used ◀ / B itself this frame (a field, a choice, tuning): do not go back.
    pub grab_left: bool,
    pub grab_b: bool,
    /// The content was entered this frame (focus its input).
    pub entered: bool,
    hints: Vec<(String, String)>,
    hints_next: Vec<(String, String)>,
    clicked_section: Option<usize>,
    textures: HashMap<String, (u64, TextureHandle)>,
    pad_down: HashMap<Pad, Instant>,
    hold_start: Option<(String, Instant)>,
    last_poll: Instant,
    slow_poll: Instant,
    spectrum_on: bool,
    tool_seq: u64,
    ptt: bool,
    pending_prompt: Option<String>,
    themed: Option<bool>,
    saw_pairing: bool,
    last_agent: String,
    happy_until: Option<Instant>,
    started: Instant,
}

impl DeckApp {
    pub fn new(client: Client) -> Self {
        let long_ago = Instant::now()
            .checked_sub(Duration::from_secs(60))
            .unwrap_or_else(Instant::now);
        DeckApp {
            client,
            m: Model::default(),
            ws: AGENT,
            section: [0; 7],
            content: [false; 7],
            sel: [[0; 4]; 7],
            palette: None,
            help: false,
            help_marker: None,
            agent: Default::default(),
            rf: Default::default(),
            mesh: Default::default(),
            net: Default::default(),
            fleet: Default::default(),
            pty: Default::default(),
            sys: Default::default(),
            terms: term::Terms::default(),
            capture: None,
            screenshot_after: None,
            frames: 0,
            paper: false,
            grab_left: false,
            grab_b: false,
            entered: false,
            hints: Vec::new(),
            hints_next: Vec::new(),
            clicked_section: None,
            textures: HashMap::new(),
            pad_down: HashMap::new(),
            hold_start: None,
            last_poll: long_ago,
            slow_poll: long_ago,
            spectrum_on: false,
            tool_seq: 0,
            ptt: false,
            pending_prompt: None,
            themed: None,
            saw_pairing: false,
            last_agent: String::new(),
            happy_until: None,
            started: Instant::now(),
        }
    }

    // ------------------------------------------------------------------ plumbing

    pub fn invoke(&mut self, tool: &'static str, args: Value) {
        self.tool_seq += 1;
        self.m.tool_busy.insert(tool.into(), true);
        let body = json!({"tool": tool, "args": args, "context": {"source": "ui"}});
        self.client.request(
            "POST",
            "/v1/deck/tools/invoke",
            Some(body),
            Tag::Tool(tool.into(), self.tool_seq),
        );
    }

    pub fn get(&self, path: &str, tag: Tag) {
        self.client.request("GET", path, None, tag);
    }

    pub fn send_prompt(&mut self, chat: &str, content: &str) {
        self.client.request(
            "POST",
            &format!("/v1/chats/{chat}/messages"),
            Some(json!({"content": content})),
            Tag::Ignore,
        );
    }

    /// The agent prompt from the palette goes to the deck's own chat (made on first use).
    pub fn ask_deck(&mut self, text: String) {
        let chat = self
            .m
            .chats
            .iter()
            .find(|c| c["host_id"] == "deck")
            .and_then(|c| c["id"].as_str())
            .map(str::to_owned);
        match chat {
            Some(id) => {
                self.send_prompt(&id, &text);
                self.agent.selected = Some(id.clone());
                self.get(&format!("/v1/chats/{id}?limit=50"), Tag::Chat(id));
            }
            None => {
                self.pending_prompt = Some(text);
                self.client.request(
                    "POST",
                    "/v1/chats",
                    Some(json!({"host_id": "deck", "title": "Deck operator"})),
                    Tag::NewChat,
                );
            }
        }
        self.ws = AGENT;
    }

    /// Adds a line to the guide bar for this frame.
    pub fn hint(&mut self, glyph: &str, label: impl Into<String>) {
        self.hints_next.push((glyph.to_owned(), label.into()));
    }

    pub fn texture(&mut self, ctx: &Context, key: &str, img: &Image) -> TextureHandle {
        if let Some((v, t)) = self.textures.get(key) {
            if *v == img.version {
                return t.clone();
            }
        }
        let color = egui::ColorImage::from_rgba_unmultiplied([img.width, img.height], &img.rgba);
        let tex = ctx.load_texture(key, color, TextureOptions::LINEAR);
        self.textures.insert(key.into(), (img.version, tex.clone()));
        tex
    }

    pub fn wants_capture(&self) -> bool {
        self.capture.is_some()
    }

    /// The backend hands over the last frame (RGBA, top row first).
    pub fn deliver_capture(&mut self, w: u32, h: u32, rgba: Vec<u8>) {
        let Some(cap) = self.capture.take() else {
            return;
        };
        let Some(img) = image::RgbaImage::from_raw(w, h, rgba) else {
            return;
        };
        let mut png = std::io::Cursor::new(Vec::new());
        if img.write_to(&mut png, image::ImageFormat::Png).is_err() {
            return;
        }
        match cap {
            Capture::Upload(id) => self
                .client
                .upload_png(&format!("/v1/internal/screen/{id}"), png.into_inner()),
            Capture::File(path) => {
                if let Err(e) = std::fs::write(&path, png.into_inner()) {
                    eprintln!("lapis-ui: cannot write {}: {e}", path.display());
                }
            }
        }
    }

    fn pump(&mut self, ctx: &Context) {
        for item in self.client.drain() {
            match item {
                Inbound::Frame(f) => self.m.apply_frame(f),
                Inbound::Http {
                    tag,
                    status,
                    body,
                    headers,
                } => self.m.apply_http(tag, status, &body, &headers),
                Inbound::Connected(up) => {
                    self.m.connected = up;
                    if up {
                        self.client
                            .send_ws(json!({"type": "subscribe_telemetry", "enabled": true}));
                        self.spectrum_on = false;
                        self.last_poll = Instant::now()
                            .checked_sub(Duration::from_secs(60))
                            .unwrap_or_else(Instant::now);
                    }
                }
            }
        }
        if let Some(id) = self.m.screen_requests.pop() {
            self.capture = Some(Capture::Upload(id));
        }
        if let Some(id) = self.m.created_chat.take() {
            self.agent.selected = Some(id.clone());
            if let Some(p) = self.pending_prompt.take() {
                self.send_prompt(&id, &p);
            }
            self.get(&format!("/v1/chats/{id}?limit=50"), Tag::Chat(id));
            if self.ws == AGENT {
                self.content[AGENT] = true;
                self.entered = true;
            }
        }
        let paper = self.m.state["ui"]["theme"] == "paper";
        if self.themed != Some(paper) {
            theme::apply(ctx, paper);
            self.themed = Some(paper);
            self.paper = paper;
        }
        let pairing = self.m.state["pairing"]["mode"]
            .as_str()
            .is_some_and(|m| m == "qr" || m == "pin");
        if pairing && !self.saw_pairing {
            // A fresh pairing window: show the PIN / QR.
            self.ws = SYS;
            self.section[SYS] = ws::sys::PAIRING;
        }
        self.saw_pairing = pairing;
        let agent = self.m.state["agent_state"]
            .as_str()
            .unwrap_or("IDLE")
            .to_owned();
        if agent == "IDLE" && matches!(self.last_agent.as_str(), "ACTING" | "THINKING") {
            self.happy_until = Some(Instant::now() + Duration::from_secs(4));
        }
        self.last_agent = agent;
        self.m
            .toasts
            .retain(|t| t.at.elapsed() < Duration::from_secs(6));
    }

    fn poll(&mut self) {
        let want = self.ws == RF;
        if want != self.spectrum_on && self.m.connected {
            self.client
                .send_ws(json!({"type": "subscribe_spectrum", "enabled": want}));
            self.spectrum_on = want;
        }
        let now = Instant::now();
        if now.duration_since(self.last_poll) < Duration::from_secs(3) {
            return;
        }
        self.last_poll = now;
        match self.ws {
            AGENT => {
                self.get("/v1/chats", Tag::Chats);
                if let Some(id) = self.agent.selected.clone() {
                    self.get(&format!("/v1/chats/{id}?limit=50"), Tag::Chat(id));
                }
            }
            MESH => {
                self.get("/v1/deck/mesh", Tag::Mesh);
                self.get("/v1/deck/gps", Tag::Gps);
            }
            FLEET => self.get("/v1/hosts", Tag::Hosts),
            SYS => {
                self.get("/v1/deck/status", Tag::DeckStatus);
                self.get("/v1/devices", Tag::Devices);
                self.get("/v1/deck/audit?limit=60", Tag::Audit);
                if self.m.config.is_null() {
                    self.get("/v1/config", Tag::Config);
                }
                self.get("/v1/secrets/status", Tag::Secrets);
            }
            _ => {}
        }
        if now.duration_since(self.slow_poll) > Duration::from_secs(15) {
            self.slow_poll = now;
            self.get("/v1/hosts", Tag::Hosts);
            self.get("/v1/chats", Tag::Chats);
        }
    }

    pub fn run(&mut self, a: Action) {
        match a {
            Action::Workspace(i) => self.ws = i.min(6),
            Action::Section(w, s) => {
                self.ws = w.min(6);
                self.section[self.ws] = s;
                self.content[self.ws] = true;
                self.entered = true;
                if w == AGENT && s == 0 {
                    self.agent.selected = None;
                }
            }
            Action::Kill => self.client.send_ws(json!({"type": "kill_all"})),
            Action::Pause => {
                self.client
                    .request("POST", "/v1/agent/pause", Some(json!({})), Tag::Ignore)
            }
            Action::Resume => {
                self.client
                    .request("POST", "/v1/agent/resume", Some(json!({})), Tag::Ignore)
            }
            Action::PairQr => self.client.request(
                "POST",
                "/v1/device/power",
                Some(json!({"action": "start_qr_pairing"})),
                Tag::Ignore,
            ),
            Action::PairBle => self.client.request(
                "POST",
                "/v1/device/power",
                Some(json!({"action": "start_ble_pairing"})),
                Tag::Ignore,
            ),
            Action::StopPairing => self.client.request(
                "POST",
                "/v1/device/power",
                Some(json!({"action": "stop_pairing"})),
                Tag::Ignore,
            ),
            Action::Tool(t, args) => self.invoke(t, args),
            Action::Tune(hz) => {
                self.client.request(
                    "POST",
                    "/v1/deck/sdr/tune",
                    Some(json!({"center_hz": hz})),
                    Tag::Ignore,
                );
                self.ws = RF;
            }
            Action::Theme => {
                let next = if self.paper { "dark" } else { "paper" };
                self.client.request(
                    "PATCH",
                    "/v1/device/ui",
                    Some(json!({"theme": next})),
                    Tag::Ignore,
                );
            }
            Action::NewShell => {
                self.terms.open_shell();
                self.ws = PTY;
                self.content[PTY] = true;
            }
            Action::Serial(dev, baud) => {
                self.terms.open_serial(&dev, baud);
                self.ws = PTY;
                self.content[PTY] = true;
            }
            Action::Split => {
                self.terms.split = !self.terms.split;
                self.ws = PTY;
            }
            Action::Help => self.help = true,
            Action::Ask(text) => self.ask_deck(text),
        }
    }

    pub fn palette_items(&self) -> Vec<(String, Action)> {
        let mut v: Vec<(String, Action)> = vec![
            ("Controls & navigation help".into(), Action::Help),
            (
                "New chat (deck or a Tailscale host)".into(),
                Action::Section(AGENT, 0),
            ),
            (
                "AI provider: model and API key".into(),
                Action::Section(SYS, ws::sys::AI),
            ),
            (
                "Add a Tailscale host to the fleet".into(),
                Action::Section(FLEET, 0),
            ),
        ];
        v.extend(
            WORKSPACES
                .iter()
                .enumerate()
                .map(|(i, w)| (format!("Go to [{}] {w}", i + 1), Action::Workspace(i))),
        );
        v.extend([
            ("KILL SWITCH: stop every agent".to_owned(), Action::Kill),
            ("Pause agent".into(), Action::Pause),
            ("Resume agent".into(), Action::Resume),
            ("Pair a phone (QR over Tailscale)".into(), Action::PairQr),
            ("Pair a phone (Bluetooth PIN)".into(), Action::PairBle),
            ("Stop pairing".into(), Action::StopPairing),
            (
                "Tune 100.1 MHz (broadcast FM)".into(),
                Action::Tune(100_100_000),
            ),
            (
                "Tune 162.400 MHz (NOAA weather)".into(),
                Action::Tune(162_400_000),
            ),
            (
                "Tune 433.92 MHz (ISM sensors)".into(),
                Action::Tune(433_920_000),
            ),
            ("Tune 1090 MHz (ADS-B)".into(), Action::Tune(1_090_000_000)),
            (
                "Decode ISM sensors (rtl_433)".into(),
                Action::Tool("sdr.decode_ism", json!({})),
            ),
            (
                "ADS-B aircraft table".into(),
                Action::Tool("sdr.adsb_snapshot", json!({})),
            ),
            (
                "Wi-Fi survey (passive)".into(),
                Action::Tool("net.wifi_scan_passive", json!({})),
            ),
            (
                "BLE survey".into(),
                Action::Tool("net.ble_scan_passive", json!({})),
            ),
            (
                "Tailscale peers".into(),
                Action::Tool("net.tailscale_peers", json!({})),
            ),
            ("GPS fix".into(), Action::Tool("gps.get_fix", json!({}))),
            ("New shell".into(), Action::NewShell),
            (
                "Serial console /dev/ttyUSB0 115200".into(),
                Action::Serial("/dev/ttyUSB0".into(), 115_200),
            ),
            ("Split terminals on / off".into(), Action::Split),
            ("Toggle theme (dark / paper)".into(), Action::Theme),
        ]);
        v
    }

    fn filtered(&self, q: &str) -> Vec<(String, Action)> {
        let ql = q.to_lowercase();
        let mut v: Vec<(String, Action)> = self
            .palette_items()
            .into_iter()
            .filter(|(l, _)| ql.is_empty() || l.to_lowercase().contains(&ql))
            .collect();
        if !q.trim().is_empty() {
            v.push((
                format!("Ask the deck agent: {}", q.trim()),
                Action::Ask(q.trim().to_owned()),
            ));
        }
        v
    }

    /// What the character shows.
    pub fn mood(&self) -> Mood {
        if !self.m.connected {
            return Mood::Offline;
        }
        if self.m.pending().is_some() {
            return Mood::Waiting;
        }
        if self.ptt {
            return Mood::Listening;
        }
        match self.m.state["agent_state"].as_str().unwrap_or("IDLE") {
            "THINKING" => Mood::Thinking,
            "ACTING" => Mood::Acting,
            "WAITING_APPROVAL" => Mood::Waiting,
            _ if self.happy_until.is_some_and(|t| Instant::now() < t) => Mood::Happy,
            _ => Mood::Idle,
        }
    }

    pub fn clock(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    fn close_help(&mut self) {
        self.help = false;
        if let Some(p) = self.help_marker.take() {
            let _ = std::fs::write(p, b"seen\n");
        }
    }

    // ------------------------------------------------------------------ input

    fn read_input(&mut self, ctx: &Context, pads: &[PadEvent]) -> Nav {
        let typing = ctx.wants_keyboard_input() || (self.ws == PTY && self.terms.has_focus());
        let mut nav = Nav {
            typing,
            ..Default::default()
        };
        let pressed = |p: Pad| pads.iter().any(|e| e.button == p && e.pressed);
        for e in pads {
            if e.pressed {
                self.pad_down.insert(e.button, Instant::now());
            } else {
                self.pad_down.remove(&e.button);
            }
        }
        // Select + Start together: the kill switch, from anywhere.
        if self.pad_down.contains_key(&Pad::Select)
            && self.pad_down.contains_key(&Pad::Start)
            && (pressed(Pad::Select) || pressed(Pad::Start))
        {
            self.run(Action::Kill);
            self.m.toast("warn", "KILL SWITCH");
            return nav;
        }
        let (alt_num, palette_key, ptt_down, ptt_up, help_key) = ctx.input(|i| {
            let nums = [
                Key::Num1,
                Key::Num2,
                Key::Num3,
                Key::Num4,
                Key::Num5,
                Key::Num6,
                Key::Num7,
            ];
            let alt = i
                .modifiers
                .alt
                .then(|| nums.iter().position(|k| i.key_pressed(*k)))
                .flatten();
            let question = !typing
                && i.events
                    .iter()
                    .any(|e| matches!(e, egui::Event::Text(t) if t == "?"));
            (
                alt,
                i.modifiers.ctrl && i.key_pressed(Key::Space),
                i.key_pressed(Key::F12),
                i.events.iter().any(|e| {
                    matches!(
                        e,
                        egui::Event::Key {
                            key: Key::F12,
                            pressed: false,
                            ..
                        }
                    )
                }),
                i.key_pressed(Key::F1) || question,
            )
        });
        if self.help {
            if help_key
                || pressed(Pad::B)
                || pressed(Pad::A)
                || ctx.input(|i| i.key_pressed(Key::Escape) || i.key_pressed(Key::Enter))
            {
                self.close_help();
            }
            return Nav {
                typing,
                ..Default::default()
            };
        }
        if help_key && self.palette.is_none() {
            self.help = true;
            return nav;
        }
        if let Some(n) = alt_num {
            self.ws = n;
        }
        if ptt_down || ptt_up {
            self.set_ptt(ptt_down);
        }
        if palette_key || (pressed(Pad::Y) && self.m.pending().is_none()) {
            self.palette = match self.palette.take() {
                Some(_) => None,
                None => Some(Palette {
                    query: String::new(),
                    sel: 0,
                    opened: true,
                }),
            };
        }
        if pressed(Pad::Select) && !self.pad_down.contains_key(&Pad::Start) {
            self.ws = (self.ws + 6) % 7;
        }
        if pressed(Pad::Start) && !self.pad_down.contains_key(&Pad::Select) {
            self.ws = (self.ws + 1) % 7;
        }
        nav.a = pressed(Pad::A);
        nav.b = pressed(Pad::B);
        nav.x = pressed(Pad::X);
        if !typing {
            ctx.input(|i| {
                nav.up = i.key_pressed(Key::ArrowUp);
                nav.down = i.key_pressed(Key::ArrowDown);
                nav.left = i.key_pressed(Key::ArrowLeft);
                nav.right = i.key_pressed(Key::ArrowRight);
                nav.a |= i.key_pressed(Key::Enter);
                nav.b |= i.key_pressed(Key::Escape);
            });
        } else {
            nav.b |= ctx.input(|i| i.key_pressed(Key::Escape)) && self.ws != PTY;
        }
        nav
    }

    pub fn set_ptt(&mut self, pressed: bool) {
        if self.ptt != pressed {
            self.ptt = pressed;
            self.client.request(
                "POST",
                "/v1/voice/ptt",
                Some(json!({"pressed": pressed})),
                Tag::Ignore,
            );
        }
    }

    /// A held on the pad or Enter held on the keyboard (hold-to-confirm rows).
    pub fn pad_or_enter_held(&self, ctx: &Context) -> bool {
        self.pad_down.contains_key(&Pad::A) || ctx.input(|i| i.key_down(Key::Enter))
    }

    fn pad_held(&self, p: Pad) -> Option<Duration> {
        self.pad_down.get(&p).map(Instant::elapsed)
    }

    // ------------------------------------------------------------------ frame

    pub fn frame(&mut self, ctx: &Context, pads: &[PadEvent]) {
        // egui reruns a frame when a layout needs a second pass (a new Grid): keyboard events
        // are consumed by the first pass, so the pad events must be too.
        let pads = if ctx.output(|o| o.num_completed_passes) == 0 {
            pads
        } else {
            &[]
        };
        self.frames += 1;
        self.pump(ctx);
        self.poll();
        let mut nav = self.read_input(ctx, pads);
        let modal = self.m.pending().is_some() || self.palette.is_some() || self.help;
        let ws = self.ws;
        let secs = ws::sections(self, ws);
        let n = secs.len();
        let mut moved = false;
        if let Some(i) = self.clicked_section.take() {
            moved = self.section[ws] != i;
            self.section[ws] = i;
            self.content[ws] = false;
        }
        if !modal && !nav.typing && !self.content[ws] {
            let before = self.section[ws];
            step(&mut self.section[ws], n, nav);
            moved |= before != self.section[ws];
            if nav.x {
                ws::sidebar_x(self, ws, self.section[ws]);
            }
            if (nav.right || nav.a) && n > 0 {
                self.content[ws] = true;
                self.entered = true;
            }
            nav = Nav {
                typing: nav.typing,
                ..Default::default()
            };
        }
        self.section[ws] = self.section[ws].min(n.saturating_sub(1));
        ws::sync(self, ws, moved);
        let secs = ws::sections(self, ws);
        let section = self.section[ws].min(secs.len().saturating_sub(1));
        let focused = self.content[ws] && !modal;
        let content_nav = if focused {
            nav
        } else {
            Nav {
                typing: nav.typing,
                ..Default::default()
            }
        };

        self.top_bar(ctx);
        self.bottom_bar(ctx);
        self.sidebar(ctx, &secs, section);
        self.grab_left = false;
        self.grab_b = false;
        self.hints_next.clear();
        let crumb = secs
            .get(section)
            .map(|s| s.label.clone())
            .unwrap_or_default();
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(if self.paper {
                        theme::PAPER_BG
                    } else {
                        theme::BG
                    })
                    .inner_margin(egui::Margin {
                        left: 10,
                        right: 10,
                        top: 6,
                        bottom: 6,
                    }),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(WORKSPACES[ws])
                            .font(theme::mono(14.0))
                            .color(FAINT),
                    );
                    ui.label(RichText::new("›").font(theme::mono(14.0)).color(FAINT));
                    ui.label(
                        RichText::new(&crumb)
                            .font(theme::mono(15.0))
                            .color(if focused { CYAN } else { TEXT }),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (text, color) = if focused {
                            ("◀ B  back to the list", DIM)
                        } else {
                            ("A / ▶  open this", CYAN)
                        };
                        ui.label(RichText::new(text).font(theme::mono(13.0)).color(color));
                    });
                });
                ui.add_space(4.0);
                ws::show(self, ui, ws, content_nav, section, focused);
            });
        self.entered = false;
        if focused && ((nav.b && !self.grab_b) || (nav.left && !self.grab_left && !nav.typing)) {
            self.content[ws] = false;
        }
        if self.m.pending().is_some() {
            self.approval(ctx, &mut nav);
        } else if self.help {
            self.help_ui(ctx);
        } else if self.palette.is_some() {
            self.palette_ui(ctx, nav);
        }
        self.toasts(ctx);
        self.hints = std::mem::take(&mut self.hints_next);
        if let Some((n, path)) = &self.screenshot_after {
            if self.frames >= *n && self.capture.is_none() {
                self.capture = Some(Capture::File(path.clone()));
                self.screenshot_after = None;
            }
        }
        // Live data arrives on other threads; keep the waterfall, holds and the character moving.
        let busy = self.ws == RF || self.hold_start.is_some() || self.ws == PTY;
        ctx.request_repaint_after(Duration::from_millis(if busy { 33 } else { 66 }));
    }

    fn top_bar(&mut self, ctx: &Context) {
        let s = self.m.state.clone();
        egui::TopBottomPanel::top("top")
            .exact_height(58.0)
            .frame(
                egui::Frame::new()
                    .fill(theme::PANEL)
                    .inner_margin(egui::Margin::symmetric(10, 7))
                    .stroke(Stroke::new(1.0_f32, LINE)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    let (logo, _) = ui.allocate_exact_size(Vec2::new(132.0, 40.0), Sense::hover());
                    let p = ui.painter();
                    p.add(egui::Shape::convex_polygon(
                        widgets::chamfer(logo, 9.0),
                        theme::RAISED,
                        Stroke::new(2.0_f32, CYAN),
                    ));
                    p.text(
                        logo.center(),
                        Align2::CENTER_CENTER,
                        "LAPIS·DECK",
                        theme::mono(16.0),
                        CYAN,
                    );
                    ui.add_space(8.0);
                    widgets::key_cap(ui, "SEL");
                    ui.label(RichText::new("◀").font(theme::mono(14.0)).color(FAINT));
                    for (i, name) in TABS.iter().enumerate() {
                        let badge = if i == AGENT {
                            self.m.approvals.len()
                        } else {
                            0
                        };
                        if widgets::tab(ui, ICONS[i], name, i + 1, i == self.ws, badge).clicked() {
                            self.ws = i;
                        }
                    }
                    ui.label(RichText::new("▶").font(theme::mono(14.0)).color(FAINT));
                    widgets::key_cap(ui, "START");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let offset = s["utc_offset_min"].as_i64().unwrap_or(0);
                        let secs = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs() as i64)
                            .unwrap_or(0)
                            + offset * 60;
                        ui.label(
                            RichText::new(format!(
                                "{:02}:{:02}",
                                (secs / 3600).rem_euclid(24),
                                (secs / 60).rem_euclid(60)
                            ))
                            .font(theme::mono(18.0))
                            .color(TEXT),
                        );
                        let pct = s["battery_pct"].as_u64();
                        let bc = match pct {
                            Some(p) if p < 15 => RED,
                            Some(p) if p < 30 => AMBER,
                            _ => CYAN,
                        };
                        let charge = if s["battery_charging"] == true {
                            "+"
                        } else {
                            ""
                        };
                        widgets::chip(
                            ui,
                            &format!(
                                "BAT {}{charge}",
                                pct.map(|p| format!("{p}%")).unwrap_or("--".into())
                            ),
                            bc,
                        );
                        let ts_up = s["tailscale_state"] == "connected";
                        widgets::chip(
                            ui,
                            if ts_up { "TS UP" } else { "TS --" },
                            if ts_up { CYAN } else { AMBER },
                        );
                        if !self.m.connected {
                            widgets::chip(ui, "OFFLINE", RED);
                        } else if s["deck"]["harness"] == "down" {
                            widgets::chip(ui, "NO AGENT", AMBER);
                        } else {
                            let agent = s["agent_state"].as_str().unwrap_or("IDLE");
                            let ac = match agent {
                                "IDLE" => DIM,
                                "WAITING_APPROVAL" => RED,
                                "PAUSED" => AMBER,
                                _ => CYAN,
                            };
                            widgets::chip(
                                ui,
                                if agent == "WAITING_APPROVAL" {
                                    "APPROVE?"
                                } else {
                                    agent
                                },
                                ac,
                            );
                        }
                        if self.ptt {
                            widgets::chip(
                                ui,
                                &format!("● REC {:.0}%", self.m.mic_level * 100.0),
                                RED,
                            );
                        }
                    });
                });
            });
    }

    fn bottom_bar(&self, ctx: &Context) {
        let ws = self.ws;
        let in_content = self.content[ws];
        egui::TopBottomPanel::bottom("hints")
            .exact_height(40.0)
            .frame(
                egui::Frame::new()
                    .fill(theme::PANEL)
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .stroke(Stroke::new(1.0_f32, LINE)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    widgets::chip(ui, if in_content { "CONTENT" } else { "SIDEBAR" }, CYAN);
                    ui.add_space(6.0);
                    if self.m.pending().is_some() {
                        widgets::hint(ui, "A", "approve");
                        widgets::hint(ui, "B", "deny");
                    } else if self.help {
                        widgets::hint(ui, "B", "close help");
                    } else if self.palette.is_some() {
                        widgets::hint(ui, "▲▼", "choose");
                        widgets::hint(ui, "A", "run");
                        widgets::hint(ui, "B", "close");
                        widgets::hint(ui, "⌨", "type to filter or ask");
                    } else if in_content {
                        for (g, l) in &self.hints {
                            widgets::hint(ui, g, l);
                        }
                        if !self.hints.iter().any(|(g, _)| g == "B") {
                            widgets::hint(ui, "B", "back");
                        }
                    } else {
                        widgets::hint(ui, "▲▼", "choose");
                        widgets::hint(ui, "A", "open");
                        if let Some(x) = sidebar_x_label(ws, self.section[ws], &self.terms) {
                            widgets::hint(ui, "X", x);
                        }
                        widgets::hint(ui, "SEL/START", "workspace");
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        widgets::hint(ui, "?", "help");
                        widgets::hint(ui, "Y", "commands");
                    });
                });
            });
    }

    fn sidebar(&mut self, ctx: &Context, secs: &[ws::Section], section: usize) {
        let side_focus = !self.content[self.ws]
            && self.palette.is_none()
            && !self.help
            && self.m.pending().is_none();
        let mood = self.mood();
        let t = self.clock();
        egui::SidePanel::left("sidebar")
            .exact_width(262.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(theme::PANEL)
                    .inner_margin(egui::Margin {
                        left: 8,
                        right: 8,
                        top: 10,
                        bottom: 8,
                    })
                    .stroke(Stroke::new(1.0_f32, LINE)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(WORKSPACES[self.ws])
                            .font(theme::mono(17.0))
                            .color(if side_focus { CYAN } else { DIM }),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("{}/{}", section + 1, secs.len()))
                                .font(theme::mono(13.0))
                                .color(FAINT),
                        );
                    });
                });
                ui.add_space(6.0);
                let card_h = 132.0;
                let list_h = (ui.available_height() - card_h - 10.0).max(80.0);
                egui::ScrollArea::vertical()
                    .id_salt(("side", self.ws))
                    .max_height(list_h)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (i, s) in secs.iter().enumerate() {
                            let r = widgets::nav_row(
                                ui,
                                s.icon,
                                &s.label,
                                &s.sub,
                                i == section,
                                side_focus,
                                s.badge.as_deref(),
                            );
                            if i == section && side_focus {
                                r.scroll_to_me(None);
                            }
                            if r.clicked() {
                                self.clicked_section = Some(i);
                            }
                            ui.add_space(2.0);
                        }
                    });
                ui.add_space(6.0);
                let (card, _) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), card_h), Sense::hover());
                let p = ui.painter();
                p.rect_filled(card, CornerRadius::same(theme::RADIUS), theme::RAISED);
                p.rect_stroke(
                    card,
                    CornerRadius::same(theme::RADIUS),
                    Stroke::new(1.0_f32, LINE),
                    StrokeKind::Inside,
                );
                character::draw(
                    p,
                    Rect::from_min_size(card.min + Vec2::new(2.0, 6.0), Vec2::splat(120.0)),
                    mood,
                    t,
                );
                let tx = card.min.x + 124.0;
                p.text(
                    Pos2::new(tx, card.min.y + 34.0),
                    Align2::LEFT_CENTER,
                    mood.label(),
                    theme::mono(16.0),
                    mood.eye(),
                );
                let detail = p.layout(
                    mood.detail().to_owned(),
                    theme::sans(13.0),
                    DIM,
                    card.max.x - tx - 6.0,
                );
                p.galley(Pos2::new(tx, card.min.y + 50.0), detail, DIM);
            });
    }

    fn approval(&mut self, ctx: &Context, nav: &mut Nav) {
        let Some(a) = self.m.pending().cloned() else {
            return;
        };
        let id = Model::str_of(&a, "request_id").to_owned();
        let tier = a["tier"]
            .as_u64()
            .unwrap_or(if a["risk"] == "high" { 2 } else { 1 });
        let hold = Duration::from_millis(
            a["hold_ms"]
                .as_u64()
                .filter(|h| *h > 0)
                .unwrap_or(if tier >= 2 { 1500 } else { 0 }),
        );
        let (key_y, key_n, key_s, y_down) = ctx.input(|i| {
            (
                i.key_pressed(Key::Y),
                i.key_pressed(Key::N),
                i.key_pressed(Key::S),
                i.key_down(Key::Y),
            )
        });
        let approve_down = y_down || self.pad_held(Pad::A).is_some();
        let mut decision: Option<&str> = None;
        if hold.is_zero() {
            if key_y || nav.a {
                decision = Some("approve_once");
            }
            if key_s || nav.x {
                decision = Some("approve_session");
            }
        } else if approve_down {
            let start = match &self.hold_start {
                Some((rid, t)) if *rid == id => *t,
                _ => {
                    let t = Instant::now();
                    self.hold_start = Some((id.clone(), t));
                    t
                }
            };
            if start.elapsed() >= hold {
                decision = Some("approve_once");
                self.hold_start = None;
            }
        } else {
            self.hold_start = None;
        }
        if key_n || nav.b {
            decision = Some("deny");
        }
        if let Some(d) = decision {
            self.client
                .send_ws(json!({"type": "approval_decision", "request_id": id, "decision": d}));
            self.m
                .approvals
                .retain(|x| x["request_id"] != a["request_id"]);
            self.hold_start = None;
            *nav = Nav::default();
            return;
        }
        let progress = self
            .hold_start
            .as_ref()
            .filter(|(r, _)| *r == id)
            .map(|(_, t)| (t.elapsed().as_secs_f32() / hold.as_secs_f32().max(0.01)).min(1.0));
        let shot = a["screenshot_id"].as_i64();
        if let Some(sid) = shot {
            if !self.m.screenshots.contains_key(&sid) && self.frames % 30 == 1 {
                self.get(&format!("/v1/screenshots/{sid}.jpg"), Tag::Screenshot(sid));
            }
        }
        let tex = match shot.and_then(|sid| self.m.screenshots.remove(&sid).map(|img| (sid, img))) {
            Some((sid, img)) => {
                let t = self.texture(ctx, &format!("shot{sid}"), &img);
                let w = img.width;
                self.m.screenshots.insert(sid, img);
                Some((t, w))
            }
            None => None,
        };
        let color = if tier >= 2 { RED } else { CYAN };
        let t = self.clock();
        ctx.layer_painter(egui::LayerId::new(
            egui::Order::Middle,
            egui::Id::new("dim"),
        ))
        .rect_filled(ctx.screen_rect(), 0.0, Color32::from_black_alpha(190));
        egui::Area::new(egui::Id::new("approval"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                let inner = egui::Frame::new()
                    .fill(theme::RAISED)
                    .stroke(Stroke::new(2.0_f32, color))
                    .inner_margin(16)
                    .corner_radius(theme::RADIUS)
                    .show(ui, |ui| {
                        ui.set_width(820.0);
                        ui.horizontal_top(|ui| {
                            let (face, _) =
                                ui.allocate_exact_size(Vec2::splat(150.0), Sense::hover());
                            character::draw(ui.painter(), face, Mood::Waiting, t);
                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    widgets::chip(ui, &format!("APPROVAL · TIER {tier}"), color);
                                    ui.label(
                                        RichText::new(format!(
                                            "{}  on  {}",
                                            Model::str_of(&a, "action"),
                                            Model::str_of(&a, "host")
                                        ))
                                        .font(theme::mono(16.0))
                                        .color(TEXT),
                                    );
                                });
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new(Model::str_of(&a, "summary"))
                                        .font(theme::sans(22.0))
                                        .color(TEXT),
                                );
                                for k in ["detail", "reason"] {
                                    let v = Model::str_of(&a, k);
                                    if !v.is_empty() {
                                        ui.label(
                                            RichText::new(v).font(theme::sans(16.0)).color(DIM),
                                        );
                                    }
                                }
                                ui.label(
                                    RichText::new(format!(
                                        "from {} · {}",
                                        Model::str_of(&a, "source"),
                                        id
                                    ))
                                    .font(theme::mono(13.0))
                                    .color(FAINT),
                                );
                            });
                        });
                        if let Some((tx, w)) = tex {
                            let size = tx.size_vec2();
                            let scale = (780.0 / size.x).min(260.0 / size.y).min(1.0);
                            let resp =
                                ui.add(egui::Image::new(&tx).fit_to_exact_size(size * scale));
                            if let (Some(x), Some(y)) =
                                (a["click_x"].as_f64(), a["click_y"].as_f64())
                            {
                                let f = resp.rect.width() as f64 / (w as f64).max(1.0);
                                let p = resp.rect.min + egui::vec2((x * f) as f32, (y * f) as f32);
                                ui.painter()
                                    .circle_stroke(p, 10.0, Stroke::new(2.0_f32, CRIMSON));
                            }
                        }
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if hold.is_zero() {
                                widgets::hint(ui, "A", "approve once   [Y]");
                                widgets::hint(ui, "X", "approve for this task   [S]");
                                widgets::hint(ui, "B", "deny   [N]");
                            } else {
                                widgets::hint(
                                    ui,
                                    "A",
                                    &format!(
                                        "HOLD {:.1} s to approve   [hold Y]",
                                        hold.as_secs_f32()
                                    ),
                                );
                                widgets::hint(ui, "B", "deny   [N]");
                            }
                        });
                        if !hold.is_zero() {
                            ui.add(
                                egui::ProgressBar::new(progress.unwrap_or(0.0))
                                    .fill(color)
                                    .desired_width(790.0)
                                    .desired_height(10.0),
                            );
                        }
                        let more = self.m.approvals.len().saturating_sub(1);
                        if more > 0 {
                            ui.label(
                                RichText::new(format!("+{more} more waiting"))
                                    .font(theme::sans(14.0))
                                    .color(DIM),
                            );
                        }
                    });
                widgets::glow(ui.painter(), inner.response.rect, color, 1.0);
            });
    }

    fn palette_ui(&mut self, ctx: &Context, nav: Nav) {
        let Some(mut p) = self.palette.take() else {
            return;
        };
        let items = self.filtered(&p.query);
        let mut chosen = None;
        let mut close = nav.b;
        if ctx.input(|i| i.key_pressed(Key::ArrowDown)) || nav.down {
            p.sel = (p.sel + 1).min(items.len().saturating_sub(1));
        }
        if ctx.input(|i| i.key_pressed(Key::ArrowUp)) || nav.up {
            p.sel = p.sel.saturating_sub(1);
        }
        ctx.layer_painter(egui::LayerId::new(
            egui::Order::Middle,
            egui::Id::new("dim"),
        ))
        .rect_filled(ctx.screen_rect(), 0.0, Color32::from_black_alpha(140));
        egui::Area::new(egui::Id::new("palette"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_TOP, egui::vec2(0.0, 70.0))
            .show(ctx, |ui| {
                let inner = egui::Frame::new()
                    .fill(theme::RAISED)
                    .stroke(Stroke::new(2.0_f32, CYAN))
                    .inner_margin(12)
                    .corner_radius(theme::RADIUS)
                    .show(ui, |ui| {
                        ui.set_width(680.0);
                        ui.label(
                            RichText::new("COMMANDS")
                                .font(theme::mono(15.0))
                                .color(CYAN),
                        );
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut p.query)
                                .hint_text(
                                    "type a command, a workspace, or a question for the agent…",
                                )
                                .desired_width(656.0)
                                .font(FontId::monospace(17.0)),
                        );
                        if p.opened {
                            r.request_focus();
                            p.opened = false;
                        }
                        if r.changed() {
                            p.sel = 0;
                        }
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        if ui.input(|i| i.key_pressed(Key::Escape)) {
                            close = true;
                        }
                        ui.add_space(4.0);
                        egui::ScrollArea::vertical()
                            .max_height(420.0)
                            .show(ui, |ui| {
                                for (i, (label, action)) in items.iter().enumerate() {
                                    let on = i == p.sel;
                                    let resp = widgets::list_row(ui, label, on, TEXT);
                                    if resp.clicked() || (on && (enter || nav.a)) {
                                        chosen = Some(action.clone());
                                    }
                                }
                            });
                    });
                widgets::glow(ui.painter(), inner.response.rect, CYAN, 1.0);
            });
        match chosen {
            Some(a) => self.run(a),
            None if !close => self.palette = Some(p),
            None => {}
        }
    }

    fn help_ui(&mut self, ctx: &Context) {
        let t = self.clock();
        ctx.layer_painter(egui::LayerId::new(
            egui::Order::Middle,
            egui::Id::new("dim"),
        ))
        .rect_filled(ctx.screen_rect(), 0.0, Color32::from_black_alpha(200));
        egui::Area::new(egui::Id::new("help")).order(egui::Order::Foreground).anchor(Align2::CENTER_CENTER, Vec2::ZERO).show(ctx, |ui| {
            let inner = egui::Frame::new().fill(theme::RAISED).stroke(Stroke::new(2.0_f32, CYAN)).inner_margin(18).corner_radius(theme::RADIUS).show(ui, |ui| {
                ui.set_width(1000.0);
                ui.horizontal(|ui| {
                    widgets::chip(ui, "HOW TO GET AROUND", CYAN);
                    ui.label(RichText::new("The screen has three parts. The guide bar at the bottom always shows what each button does.").font(theme::sans(16.0)).color(DIM));
                });
                ui.add_space(10.0);
                ui.horizontal_top(|ui| {
                    let (map, _) = ui.allocate_exact_size(Vec2::new(440.0, 300.0), Sense::hover());
                    help_map(ui.painter(), map);
                    ui.add_space(16.0);
                    ui.vertical(|ui| {
                        let rows: [(&str, &str, &str); 10] = [
                            ("SEL / START", "Alt+1..7", "previous / next workspace (top bar)"),
                            ("▲ ▼", "arrows", "choose a section or row"),
                            ("A  or  ▶", "Enter", "open the section, run, select"),
                            ("B  or  ◀", "Esc", "back to the sidebar, cancel, deny"),
                            ("◀ ▶", "arrows", "change a ◀ value ▶ in a form, tune RF"),
                            ("X", "", "the section's action (probe, refresh, new…)"),
                            ("Y", "Ctrl+Space", "commands: search everything, ask the agent"),
                            ("SEL + START", "", "KILL SWITCH: stop every agent now"),
                            ("", "F12 (hold)", "push to talk"),
                            ("", "F1  or  ?", "this help"),
                        ];
                        egui::Grid::new("keys").num_columns(3).spacing([14.0, 8.0]).show(ui, |ui| {
                            ui.label(RichText::new("GAMEPAD").font(theme::mono(13.0)).color(FAINT));
                            ui.label(RichText::new("KEYBOARD").font(theme::mono(13.0)).color(FAINT));
                            ui.label("");
                            ui.end_row();
                            for (pad, key, what) in rows {
                                ui.label(RichText::new(pad).font(theme::mono(16.0)).color(CYAN));
                                ui.label(RichText::new(key).font(theme::mono(15.0)).color(DIM));
                                ui.label(RichText::new(what).font(theme::sans(16.0)).color(TEXT));
                                ui.end_row();
                            }
                        });
                    });
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let (face, _) = ui.allocate_exact_size(Vec2::splat(64.0), Sense::hover());
                    character::draw(ui.painter(), face, Mood::Idle, t);
                    ui.label(RichText::new("Approvals pop up over everything: A approves, B denies, tier 2 needs A held. Press B, A or F1 to close this.").font(theme::sans(16.0)).color(DIM));
                });
            });
            widgets::glow(ui.painter(), inner.response.rect, CYAN, 1.0);
        });
    }

    fn toasts(&self, ctx: &Context) {
        if self.m.toasts.is_empty() {
            return;
        }
        egui::Area::new(egui::Id::new("toasts"))
            .order(egui::Order::Tooltip)
            .anchor(Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -50.0))
            .show(ctx, |ui| {
                for t in &self.m.toasts {
                    let c = match t.level.as_str() {
                        "error" => RED,
                        "warn" => AMBER,
                        _ => CYAN,
                    };
                    egui::Frame::new()
                        .fill(theme::RAISED)
                        .stroke(Stroke::new(2.0_f32, c))
                        .corner_radius(theme::RADIUS)
                        .inner_margin(10)
                        .show(ui, |ui| {
                            ui.set_max_width(460.0);
                            ui.label(RichText::new(&t.text).font(theme::sans(15.0)).color(TEXT));
                        });
                }
            });
    }
}

/// The help's map of the screen: the three zones and the buttons between them.
fn help_map(p: &egui::Painter, r: Rect) {
    let top = Rect::from_min_size(r.min, Vec2::new(r.width(), 46.0));
    let side = Rect::from_min_max(
        Pos2::new(r.min.x, top.max.y + 8.0),
        Pos2::new(r.min.x + 140.0, r.max.y - 40.0),
    );
    let content = Rect::from_min_max(
        Pos2::new(side.max.x + 70.0, side.min.y),
        Pos2::new(r.max.x, side.max.y),
    );
    let guide = Rect::from_min_max(Pos2::new(r.min.x, r.max.y - 32.0), r.max);
    for (rect, title, sub) in [
        (top, "TOP BAR · workspaces", "SEL ◀   ▶ START"),
        (side, "SIDEBAR", "sections  ▲▼"),
        (content, "CONTENT", "rows ▲▼ · A run"),
        (guide, "GUIDE BAR · what each button does now", ""),
    ] {
        p.rect_filled(rect, CornerRadius::same(theme::RADIUS), theme::PANEL);
        p.rect_stroke(
            rect,
            CornerRadius::same(theme::RADIUS),
            Stroke::new(2.0_f32, CYAN.gamma_multiply(0.7)),
            StrokeKind::Inside,
        );
        p.text(
            rect.min + Vec2::new(10.0, 14.0),
            Align2::LEFT_CENTER,
            title,
            theme::mono(14.0),
            CYAN,
        );
        if !sub.is_empty() {
            p.text(
                rect.min + Vec2::new(10.0, 34.0),
                Align2::LEFT_CENTER,
                sub,
                theme::mono(13.0),
                DIM,
            );
        }
    }
    let y = side.center().y;
    let (x0, x1) = (side.max.x + 6.0, content.min.x - 6.0);
    let s = Stroke::new(2.0_f32, CYAN);
    p.arrow(Pos2::new(x0, y - 16.0), Vec2::new(x1 - x0, 0.0), s);
    p.text(
        Pos2::new((x0 + x1) / 2.0, y - 30.0),
        Align2::CENTER_CENTER,
        "A / ▶",
        theme::mono(13.0),
        CYAN,
    );
    p.arrow(
        Pos2::new(x1, y + 16.0),
        Vec2::new(x0 - x1, 0.0),
        Stroke::new(2.0_f32, RED),
    );
    p.text(
        Pos2::new((x0 + x1) / 2.0, y + 30.0),
        Align2::CENTER_CENTER,
        "B / ◀",
        theme::mono(13.0),
        RED,
    );
}

/// The guide-bar label for X in the sidebar, if X does something there.
fn sidebar_x_label(ws: usize, section: usize, terms: &term::Terms) -> Option<&'static str> {
    match ws {
        AGENT => Some("new chat"),
        FLEET if section > 0 => Some("probe host"),
        PTY if section < terms.list.len() => Some("close terminal"),
        NET => Some("refresh / run"),
        _ => None,
    }
}

/// Selection helper: moves `sel` within `len` on up/down and keeps it in range.
pub fn step(sel: &mut usize, len: usize, nav: Nav) {
    if len == 0 {
        *sel = 0;
        return;
    }
    if nav.down {
        *sel = (*sel + 1).min(len - 1);
    }
    if nav.up {
        *sel = sel.saturating_sub(1);
    }
    *sel = (*sel).min(len - 1);
}

/// A titled pane (the styleguide panel: chamfered title tab, cyan border when focused).
pub fn pane<R>(
    ui: &mut egui::Ui,
    title: &str,
    focused: bool,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    widgets::panel(ui, title, focused, add)
}

pub fn kv(ui: &mut egui::Ui, k: &str, v: impl Into<String>) {
    widgets::field(ui, k, v);
}

pub fn fmt_hz(hz: f64) -> String {
    if hz >= 1e9 {
        format!("{:.4} GHz", hz / 1e9)
    } else if hz >= 1e6 {
        format!("{:.4} MHz", hz / 1e6)
    } else {
        format!("{:.1} kHz", hz / 1e3)
    }
}

pub fn painter_text(p: &egui::Painter, pos: egui::Pos2, text: &str, color: Color32, size: f32) {
    p.text(pos, Align2::LEFT_TOP, text, FontId::monospace(size), color);
}
