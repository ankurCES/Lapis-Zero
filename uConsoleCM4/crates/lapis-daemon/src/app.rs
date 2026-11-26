//! Daemon state: the merged device state, pairing, telemetry, UI settings and the loops
//! that keep them current.

use crate::audit::Audit;
use crate::auth::{now, Auth};
use crate::ble::{self, session::Session};
use crate::config::{atomic_write, DeckConfig, Paths, VERSION};
use crate::events::{Audience, Hub};
use crate::hw::{Hardware, Rail};
use crate::net::{NetControl, NetStatus};
use crate::policy::Gate;
use crate::proxy::HarnessClient;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

pub const QR_TTL_S: u64 = 180;

/// Opens the phone Wi-Fi window once per offline episode (protocol 4.2).
#[derive(Default)]
pub struct OfflineWatch {
    since: Option<Instant>,
    fired: bool,
}

impl OfflineWatch {
    pub const AFTER: Duration = Duration::from_secs(30);

    /// True exactly once per episode: 30 s after Wi-Fi lost its connection.
    pub fn tick(&mut self, online: bool, now: Instant) -> bool {
        if online {
            *self = Self::default();
            return false;
        }
        let since = *self.since.get_or_insert(now);
        if !self.fired && now.duration_since(since) >= Self::AFTER {
            self.fired = true;
            return true;
        }
        false
    }
}
const SKIP_TO_PHONES: [&str; 3] = ["mic_level", "screen_request", "ui_settings"];

pub struct App {
    pub cfg: DeckConfig,
    pub paths: Paths,
    pub mock: bool,
    pub hub: Hub,
    pub auth: Arc<Auth>,
    pub audit: Arc<Audit>,
    pub gate: Arc<Gate>,
    pub hw: Arc<dyn Hardware>,
    pub net_ctl: Arc<dyn NetControl>,
    pub harness: HarnessClient,
    pub hostname: String,
    /// `--dev-advertise HOST:PORT`: QR codes for a development daemon (plain HTTP).
    pub dev_advertise: Option<String>,
    pub started: Instant,
    net: RwLock<NetStatus>,
    harness_state: Mutex<Option<Value>>,
    harness_up: AtomicBool,
    pairing: Mutex<Option<Value>>,
    pairing_epoch: AtomicU64,
    ble_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// The phone Wi-Fi window (paired phones only): its task and when it closes.
    phone_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    phone_until: Mutex<i64>,
    offline: Mutex<OfflineWatch>,
    ui: Mutex<Value>,
    screens: Mutex<HashMap<String, oneshot::Sender<Vec<u8>>>>,
    pub deck: crate::deck::DeckState,
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "lapis-deck".into())
}

pub struct Parts {
    pub cfg: DeckConfig,
    pub paths: Paths,
    pub mock: bool,
    pub hw: Arc<dyn Hardware>,
    pub net_ctl: Arc<dyn NetControl>,
    pub dev_advertise: Option<String>,
}

impl App {
    pub fn new(p: Parts) -> anyhow::Result<Arc<Self>> {
        std::fs::create_dir_all(&p.paths.state_dir)?;
        let hub = Hub::default();
        let auth = Arc::new(Auth::open(&p.paths.devices(), &p.paths.local_token())?);
        let audit = Arc::new(Audit::new(p.paths.audit()));
        let gate = Arc::new(Gate::new(
            hub.clone(),
            audit.clone(),
            Duration::from_secs(p.cfg.policy.approval_timeout_s),
            p.cfg.policy.extra_denylist.clone(),
        ));
        let ui = std::fs::read(p.paths.ui_settings())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_else(
                || json!({"theme": "dark", "brightness": 100, "mute_audio": false, "timezone": ""}),
            );
        let deck = crate::deck::DeckState::new(&p.cfg, p.mock);
        Ok(Arc::new(App {
            harness: HarnessClient::new(p.paths.harness_socket.clone()),
            hostname: hostname(),
            started: Instant::now(),
            net: RwLock::new(NetStatus::default()),
            harness_state: Mutex::new(None),
            harness_up: AtomicBool::new(false),
            pairing: Mutex::new(None),
            pairing_epoch: AtomicU64::new(0),
            ble_task: Mutex::new(None),
            phone_task: Mutex::new(None),
            phone_until: Mutex::new(0),
            offline: Mutex::new(OfflineWatch::default()),
            ui: Mutex::new(ui),
            screens: Mutex::new(HashMap::new()),
            cfg: p.cfg,
            paths: p.paths,
            mock: p.mock,
            hub,
            auth,
            audit,
            gate,
            hw: p.hw,
            net_ctl: p.net_ctl,
            dev_advertise: p.dev_advertise,
            deck,
        }))
    }

    pub fn net(&self) -> NetStatus {
        self.net.read().map(|n| n.clone()).unwrap_or_default()
    }

    pub fn harness_up(&self) -> bool {
        self.harness_up.load(Ordering::Relaxed)
    }

    pub fn ui_settings(&self) -> Value {
        self.ui.lock().map(|u| u.clone()).unwrap_or(Value::Null)
    }

    // ------------------------------------------------------------------ state

    /// The harness's agent state with the device's own fields laid over it.
    pub fn state(&self) -> Value {
        let harness = self.harness_state.lock().ok().and_then(|s| s.clone());
        let up = self.harness_up() && harness.is_some();
        let mut s = harness.filter(|_| up).unwrap_or_else(|| {
            json!({"agent_state": "IDLE", "action": "", "active_host": "", "hosts": [], "active_task": null,
                   "active_chat_id": null, "next_cron": "", "api_key_ready": false})
        });
        let net = self.net();
        let battery = self.hw.battery();
        let pending = self.gate.pending();
        if !pending.is_empty() && matches!(s["agent_state"].as_str(), Some("THINKING" | "ACTING")) {
            s["agent_state"] = json!("WAITING_APPROVAL");
        }
        let fields = json!({
            "type": "state_sync",
            "wifi_ssid": net.wifi_ssid,
            "local_ip": net.wifi_ip,
            "tailscale_state": net.tailscale_state,
            "tailscale_ip": net.tailscale_ip,
            "tailscale_fqdn": net.tailscale_fqdn,
            "tailscale_auth_url": net.tailscale_auth_url,
            "battery_pct": battery.as_ref().map(|b| b.percent),
            "battery_charging": battery.as_ref().is_some_and(|b| b.charging),
            "pending_approvals": pending,
            "pairing": self.pairing.lock().ok().and_then(|p| p.clone()),
            "ui": self.ui_settings(),
            "device": {"hostname": self.hostname, "version": VERSION, "mock": self.mock, "kind": "lapis-deck"},
            "deck": {
                "harness": if up { "up" } else { "down" },
                "battery_w": battery.as_ref().and_then(|b| b.power_w),
                "rails": self.hw.rails(),
                "summary": self.deck.summary(),
            },
        });
        if let (Some(obj), Some(f)) = (s.as_object_mut(), fields.as_object()) {
            for (k, v) in f {
                obj.insert(k.clone(), v.clone());
            }
        }
        s
    }

    pub fn broadcast_state(&self) {
        self.hub.send(self.state());
    }

    /// One frame from the harness's event stream.
    pub fn on_harness_frame(&self, frame: Value) {
        let kind = frame["type"].as_str().unwrap_or("").to_owned();
        match kind.as_str() {
            "state_sync" => {
                if let Ok(mut s) = self.harness_state.lock() {
                    *s = Some(frame);
                }
                self.broadcast_state();
            }
            // The daemon produces its own ticks from the real hardware.
            "telemetry_tick" => {}
            k if SKIP_TO_PHONES.contains(&k) => self.hub.send_to(frame, Audience::Local),
            _ => self.hub.send(frame),
        }
    }

    pub async fn harness_loop(self: Arc<Self>) {
        let mut backoff = Duration::from_millis(500);
        loop {
            let me = self.clone();
            let connected = Instant::now();
            let result = self
                .harness
                .events(move |frame| {
                    if !me.harness_up.swap(true, Ordering::Relaxed) {
                        tracing::info!("harness connected");
                    }
                    me.on_harness_frame(frame);
                })
                .await;
            if self.harness_up.swap(false, Ordering::Relaxed) {
                tracing::warn!(?result, "harness event stream ended");
                self.broadcast_state();
            }
            if connected.elapsed() > Duration::from_secs(10) {
                backoff = Duration::from_millis(500);
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(10));
        }
    }

    // ------------------------------------------------------------------ telemetry

    pub fn telemetry(&self) -> Value {
        let net = self.net();
        let sys = self.hw.system();
        let rss = |pid: &str| -> f64 {
            std::fs::read_to_string(format!("/proc/{pid}/status"))
                .ok()
                .and_then(|s| {
                    s.lines()
                        .find(|l| l.starts_with("VmRSS:"))
                        .and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok())
                })
                .map(|kb| (kb / 1024.0 * 10.0).round() / 10.0)
                .unwrap_or(0.0)
        };
        json!({
            "battery": self.hw.battery().map(|b| json!({"percent": b.percent, "voltage_v": b.voltage_v, "charging": b.charging,
                                                        "current_a": b.current_a, "power_w": b.power_w, "status": b.status})),
            "cpu": {"temp_c": sys.temp_c, "load1": sys.load1},
            "memory": {"total_mb": sys.total_mb, "available_mb": sys.available_mb, "zram_total_mb": sys.swap_total_mb,
                       "zram_used_mb": sys.swap_used_mb, "daemon_rss_mb": rss("self"), "ui_rss_mb": 0.0},
            "wifi": {"ssid": net.wifi_ssid, "ip": net.wifi_ip, "rssi_dbm": net.wifi_rssi_dbm},
            "tailscale": {"state": net.tailscale_state, "ip": net.tailscale_ip, "fqdn": net.tailscale_fqdn, "peers": net.tailscale_peers},
            "uptime_sec": sys.uptime_sec,
            "services": self.hw.services(),
            "rails": self.hw.rails(),
        })
    }

    /// Network status every 5 s; telemetry ticks every 5 s (subscribers) / 30 s (everyone).
    pub async fn status_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        let mut n: u64 = 0;
        loop {
            tick.tick().await;
            let hw = self.hw.clone();
            if let Ok(status) = tokio::task::spawn_blocking(move || hw.network()).await {
                let changed = self.net.read().map(|n| *n != status).unwrap_or(true);
                if let Ok(mut net) = self.net.write() {
                    *net = status;
                }
                if changed {
                    self.broadcast_state();
                }
                let online = !self.net().wifi_ssid.is_empty();
                let fire = self
                    .offline
                    .lock()
                    .map(|mut w| w.tick(online, Instant::now()))
                    .unwrap_or(false);
                if fire && !self.mock && self.auth.paired_count() > 0 {
                    tracing::info!(
                        "Wi-Fi offline for 30 s: paired phones may set it over Bluetooth"
                    );
                    if let Err(e) = self.phone_window_start().await {
                        tracing::warn!(error = %e, "phone Wi-Fi window");
                    }
                }
            }
            let me = self.clone();
            if let Ok(t) = tokio::task::spawn_blocking(move || me.telemetry()).await {
                let audience = if n % 6 == 5 {
                    Audience::All
                } else {
                    Audience::TelemetryFast
                };
                self.hub
                    .send_to(json!({"type": "telemetry_tick", "telemetry": t}), audience);
            }
            n += 1;
        }
    }

    pub async fn refresh_net(&self) {
        let hw = self.hw.clone();
        if let Ok(status) = tokio::task::spawn_blocking(move || hw.network()).await {
            if let Ok(mut net) = self.net.write() {
                *net = status;
            }
        }
    }

    // ------------------------------------------------------------------ owner actions

    pub async fn set_rail(&self, rail: Rail, on: bool, by: &str) -> Result<(), String> {
        let hw = self.hw.clone();
        let res = tokio::task::spawn_blocking(move || hw.set_rail(rail, on))
            .await
            .map_err(|e| e.to_string())?;
        self.audit.record(
            "rail",
            json!({"rail": rail.name(), "on": on, "by": by, "ok": res.is_ok()}),
        );
        res.map_err(|e| e.to_string())?;
        self.broadcast_state();
        Ok(())
    }

    pub async fn kill_all(&self, by: &str) {
        let n = self.gate.deny_all(by);
        tracing::warn!(by, denied = n, "kill switch");
        let _ = self
            .harness
            .json("POST", "/v1/agent/kill-all", Some(json!({})), None)
            .await;
        self.deck.stop_all();
        self.broadcast_state();
    }

    pub fn set_ui(&self, patch: &Value) -> Result<Value, String> {
        if let Some(t) = patch.get("theme").filter(|v| !v.is_null()) {
            if !matches!(t.as_str(), Some("dark" | "paper")) {
                return Err("theme must be dark or paper".into());
            }
        }
        let brightness = match patch.get("brightness").filter(|v| !v.is_null()) {
            Some(b) => Some(
                b.as_u64()
                    .filter(|b| *b <= 100)
                    .ok_or("brightness must be an integer from 0 to 100")? as u8,
            ),
            None => None,
        };
        let tz = match patch.get("timezone").filter(|v| !v.is_null()) {
            Some(t) => {
                let t = t.as_str().unwrap_or("");
                let ok = !t.is_empty()
                    && t.len() < 64
                    && t.split('/').count() <= 3
                    && t.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "/_+-".contains(c));
                if !ok {
                    return Err("timezone must be an IANA name such as America/Los_Angeles".into());
                }
                Some(t.to_owned())
            }
            None => None,
        };
        if let Some(b) = brightness {
            self.hw.set_brightness(b).map_err(|e| e.to_string())?;
        }
        if let Some(m) = patch.get("mute_audio").and_then(Value::as_bool) {
            self.hw.set_mute(m).map_err(|e| e.to_string())?;
        }
        if let Some(t) = &tz {
            self.hw
                .set_timezone(t)
                .map_err(|e| format!("unknown timezone: {t} ({e})"))?;
        }
        let ui = {
            let mut ui = self.ui.lock().map_err(|_| "ui settings unavailable")?;
            for key in ["theme", "mute_audio"] {
                if let Some(v) = patch.get(key).filter(|v| !v.is_null()) {
                    ui[key] = v.clone();
                }
            }
            if let Some(b) = brightness {
                ui["brightness"] = json!(b);
            }
            if let Some(t) = tz {
                ui["timezone"] = json!(t);
            }
            ui.clone()
        };
        if let Err(e) = atomic_write(&self.paths.ui_settings(), ui.to_string().as_bytes(), 0o644) {
            tracing::warn!(error = %e, "cannot save UI settings");
        }
        let wake = patch.get("wake").and_then(Value::as_bool).unwrap_or(false);
        self.hub.send_to(
            json!({"type": "ui_settings", "ui": ui, "wake": wake}),
            Audience::Local,
        );
        self.broadcast_state();
        Ok(ui)
    }

    /// PNG of the HUD, asked of lapis-ui over its socket.
    pub async fn screen_png(&self) -> Option<Vec<u8>> {
        let id = lapis_crypto::token::mint_token();
        let (tx, rx) = oneshot::channel();
        self.screens.lock().ok()?.insert(id.clone(), tx);
        self.hub.send_to(
            json!({"type": "screen_request", "request_id": id}),
            Audience::Local,
        );
        let got = tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .ok()
            .and_then(Result::ok);
        if let Ok(mut s) = self.screens.lock() {
            s.remove(&id);
        }
        got
    }

    pub fn on_screen(&self, id: &str, png: Vec<u8>) -> bool {
        self.screens
            .lock()
            .ok()
            .and_then(|mut s| s.remove(id))
            .is_some_and(|tx| tx.send(png).is_ok())
    }

    // ------------------------------------------------------------------ pairing

    fn set_pairing(self: &Arc<Self>, view: Option<Value>, ttl: Option<Duration>) {
        let epoch = self.pairing_epoch.fetch_add(1, Ordering::Relaxed) + 1;
        if let Ok(mut p) = self.pairing.lock() {
            *p = view.clone();
        }
        self.hub
            .send(view.unwrap_or_else(|| json!({"type": "pairing", "mode": "off"})));
        self.broadcast_state();
        if let Some(ttl) = ttl {
            let me = self.clone();
            tokio::spawn(async move {
                tokio::time::sleep(ttl).await;
                if me.pairing_epoch.load(Ordering::Relaxed) == epoch {
                    me.pairing_stop().await;
                }
            });
        }
    }

    pub fn pairing(&self) -> Option<Value> {
        self.pairing.lock().ok().and_then(|p| p.clone())
    }

    /// QR when the tailnet is up and served (instant), otherwise the BLE PIN window.
    pub async fn pairing_start(self: &Arc<Self>, mode: &str) -> Result<Value, String> {
        self.refresh_net().await;
        let net = self.net();
        let fqdn = self
            .dev_advertise
            .clone()
            .unwrap_or(net.tailscale_fqdn.clone());
        let online = net.tailscale_state == "connected" && !fqdn.is_empty();
        let served = net.tailscale_served || self.dev_advertise.is_some();
        let mode = match mode {
            "auto" if online && served => "qr",
            "auto" => "ble",
            m => m,
        };
        match mode {
            "qr" => {
                if !online {
                    return Err("Tailscale is not connected; pair over Bluetooth instead".into());
                }
                if !served {
                    return Err(
                        "Tailscale HTTPS is not active yet; pair over Bluetooth instead".into(),
                    );
                }
                let nonce = self.auth.new_nonce();
                let pin = lapis_crypto::token::generate_pin();
                let mut payload = format!("lapis://pair?host={fqdn}&nonce={nonce}&pin={pin}");
                if self.dev_advertise.is_some() {
                    payload.push_str("&tls=0");
                }
                self.set_pairing(
                    Some(json!({"type": "pairing", "mode": "qr", "payload": payload, "host": fqdn, "pin": pin,
                                "expires_in": QR_TTL_S, "expires_at": now() + QR_TTL_S as i64})),
                    Some(Duration::from_secs(QR_TTL_S)),
                );
            }
            "ble" => self.start_ble().await,
            other => return Err(format!("unknown pairing mode {other:?}")),
        }
        Ok(self
            .pairing()
            .unwrap_or_else(|| json!({"type": "pairing", "mode": "starting"})))
    }

    fn pin_view(pin: &str, name: &str, status: &str) -> Value {
        json!({"type": "pairing", "mode": "pin", "pin": pin, "device_name": name, "status": status,
               "expires_in": ble::WINDOW_S, "expires_at": now() + ble::WINDOW_S as i64})
    }

    fn ble_backend(&self, on_status: ble::StatusFn) -> Arc<ble::DeviceBackend> {
        Arc::new(ble::DeviceBackend {
            auth: self.auth.clone(),
            audit: self.audit.clone(),
            net: self.net_ctl.clone(),
            harness: self.harness.clone(),
            on_status,
        })
    }

    // ------------------------------------------------------------------ Wi-Fi

    /// When the phone Wi-Fi window closes (epoch s), 0 when it is not open.
    pub fn phone_window(&self) -> i64 {
        self.phone_until
            .lock()
            .map(|u| *u)
            .ok()
            .filter(|u| *u > now())
            .unwrap_or(0)
    }

    fn set_phone_until(&self, until: i64) {
        if let Ok(mut u) = self.phone_until.lock() {
            *u = until;
        }
    }

    fn stop_phone_window(&self) {
        if let Ok(mut t) = self.phone_task.lock() {
            if let Some(task) = t.take() {
                task.abort();
            }
        }
        self.set_phone_until(0);
    }

    /// Lets paired phones set Wi-Fi over Bluetooth for 5 minutes, with no PIN on screen.
    /// An open PIN window already accepts them.
    pub async fn phone_window_start(self: &Arc<Self>) -> Result<(), String> {
        if self.auth.paired_count() == 0 {
            return Err("No phone is paired with this device yet".into());
        }
        if self.pairing().is_some_and(|p| p["mode"] == "pin") || self.phone_window() > 0 {
            return Ok(());
        }
        let until = now() + ble::WINDOW_S as i64;
        self.set_phone_until(until);
        if self.mock {
            let me = self.clone();
            let task = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(ble::WINDOW_S)).await;
                me.set_phone_until(0);
            });
            if let Ok(mut t) = self.phone_task.lock() {
                *t = Some(task);
            }
            return Ok(());
        }
        self.run_phone_window()
    }

    #[cfg(all(target_os = "linux", feature = "bluez"))]
    fn run_phone_window(self: &Arc<Self>) -> Result<(), String> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let session = Session::paired_only(self.ble_backend(Arc::new(|_: &str, _: &str| {})), tx);
        let me = self.clone();
        let task = tokio::spawn(async move {
            let res = ble::bluez::run_window(
                session,
                rx,
                Duration::from_secs(ble::WINDOW_S),
                Duration::from_secs(ble::FINISH_S),
                |_| {},
            )
            .await;
            if let Err(e) = res {
                tracing::warn!(error = %e, "phone Wi-Fi window failed");
            }
            me.set_phone_until(0);
            me.refresh_net().await;
            me.broadcast_state();
        });
        if let Ok(mut t) = self.phone_task.lock() {
            if let Some(old) = t.replace(task) {
                old.abort();
            }
        }
        Ok(())
    }

    #[cfg(not(all(target_os = "linux", feature = "bluez")))]
    fn run_phone_window(self: &Arc<Self>) -> Result<(), String> {
        self.set_phone_until(0);
        Err("Bluetooth unavailable: this build has no BlueZ support (feature `bluez`)".into())
    }

    /// `GET /v1/wifi`: the current network, saved profiles and the phone window.
    pub async fn wifi_view(&self) -> Value {
        let ctl = self.net_ctl.clone();
        let saved = tokio::task::spawn_blocking(move || ctl.wifi_saved())
            .await
            .unwrap_or_default();
        // The mock's hardware never changes network; its NetControl does.
        let net = if self.mock {
            self.net_ctl.tailscale_status()
        } else {
            self.net()
        };
        // A PIN window on screen takes paired phones too.
        let pin_until = self
            .pairing()
            .filter(|p| p["mode"] == "pin")
            .and_then(|p| p["expires_at"].as_i64())
            .unwrap_or(0);
        let until = self.phone_window().max(pin_until);
        json!({"ssid": net.wifi_ssid, "ip": net.wifi_ip, "rssi": net.wifi_rssi_dbm, "saved": saved,
               "ble": {"open": until > 0, "expires_at": until}})
    }

    /// Joins a network (the owner's action: audited, the password never recorded).
    pub async fn wifi_connect(&self, ssid: String, psk: Option<String>, by: &str) -> Value {
        if let Some(p) = &psk {
            lapis_crypto::register_secret(p);
        }
        let ctl = self.net_ctl.clone();
        let name = ssid.clone();
        let result = tokio::task::spawn_blocking(move || ctl.wifi_connect(&name, psk.as_deref()))
            .await
            .unwrap_or_else(|_| json!({"state": "failed", "local_ip": ""}));
        self.audit.record(
            "wifi_connect",
            json!({"ssid": ssid, "state": result["state"], "by": by}),
        );
        self.refresh_net().await;
        self.broadcast_state();
        result
    }

    pub async fn wifi_forget(&self, name: String, by: &str) -> bool {
        let ctl = self.net_ctl.clone();
        let n = name.clone();
        let ok = tokio::task::spawn_blocking(move || ctl.wifi_forget(&n))
            .await
            .unwrap_or(false);
        if ok {
            self.audit
                .record("wifi_forget", json!({"name": name, "by": by}));
            self.refresh_net().await;
            self.broadcast_state();
        }
        ok
    }

    async fn start_ble(self: &Arc<Self>) {
        if let Ok(mut t) = self.ble_task.lock() {
            if let Some(old) = t.take() {
                old.abort();
            }
        }
        self.stop_phone_window(); // one GATT server at a time; the PIN window takes paired phones too
        let weak = Arc::downgrade(self);
        let name = Arc::new(Mutex::new(format!("{}MOCK", ble::NAME_PREFIX)));
        let name_cb = name.clone();
        let backend = self.ble_backend(Arc::new(move |status, pin| {
            if let Some(app) = weak.upgrade() {
                let device = name_cb.lock().map(|n| n.clone()).unwrap_or_default();
                let ttl = (status == "done").then_some(Duration::from_secs(3));
                tracing::info!(status, "pairing");
                app.set_pairing(Some(Self::pin_view(pin, &device, status)), ttl);
            }
        }));
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let session = Session::new(backend, tx);
        let pin = session.pin().await;
        let window = Duration::from_secs(ble::WINDOW_S);
        if self.mock {
            // No radio on a laptop: show the PIN view so the HUD and the app can be exercised.
            drop(rx);
            let device = name.lock().map(|n| n.clone()).unwrap_or_default();
            self.set_pairing(Some(Self::pin_view(&pin, &device, "waiting")), Some(window));
            return;
        }
        self.run_ble(session, rx, name, window, pin).await;
    }

    #[cfg(all(target_os = "linux", feature = "bluez"))]
    async fn run_ble(
        self: &Arc<Self>,
        session: Arc<Session>,
        rx: tokio::sync::mpsc::UnboundedReceiver<ble::session::Outbound>,
        name: Arc<Mutex<String>>,
        window: Duration,
        pin: String,
    ) {
        self.set_pairing(Some(Self::pin_view(&pin, "", "starting")), None);
        let me = self.clone();
        let named = name.clone();
        let task = tokio::spawn(async move {
            let res = ble::bluez::run_window(
                session,
                rx,
                window,
                Duration::from_secs(ble::FINISH_S),
                move |n| {
                    if let Ok(mut slot) = named.lock() {
                        *slot = n.to_owned();
                    }
                },
            )
            .await;
            match res {
                Ok(()) => me.pairing_stop().await,
                Err(e) => {
                    tracing::error!(error = %e, "BLE provisioning failed");
                    me.set_pairing(
                        Some(Self::pin_view(
                            "",
                            "",
                            &format!("Bluetooth unavailable: {e}"),
                        )),
                        Some(Duration::from_secs(60)),
                    );
                }
            }
        });
        // The view with the advertised name follows once BlueZ has registered it.
        let me = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            if me.pairing().is_some_and(|p| p["status"] == "starting") {
                let n = name.lock().map(|n| n.clone()).unwrap_or_default();
                me.set_pairing(Some(Self::pin_view(&pin, &n, "waiting")), None);
            }
        });
        if let Ok(mut t) = self.ble_task.lock() {
            *t = Some(task);
        }
    }

    #[cfg(not(all(target_os = "linux", feature = "bluez")))]
    async fn run_ble(
        self: &Arc<Self>,
        _session: Arc<Session>,
        _rx: tokio::sync::mpsc::UnboundedReceiver<ble::session::Outbound>,
        _name: Arc<Mutex<String>>,
        _window: Duration,
        _pin: String,
    ) {
        self.set_pairing(
            Some(Self::pin_view(
                "",
                "",
                "Bluetooth unavailable: this build has no BlueZ support (feature `bluez`)",
            )),
            Some(Duration::from_secs(60)),
        );
    }

    pub async fn pairing_stop(self: &Arc<Self>) {
        self.auth.cancel_nonces();
        if let Ok(mut t) = self.ble_task.lock() {
            if let Some(task) = t.take() {
                task.abort();
            }
        }
        self.set_pairing(None, None);
    }

    pub async fn pair_exchange(
        self: &Arc<Self>,
        nonce: &str,
        name: &str,
        model: &str,
    ) -> Option<(String, String)> {
        if !self.auth.consume_nonce(nonce) {
            return None;
        }
        let (id, token) = self.auth.register(name, model);
        lapis_crypto::register_secret(&token);
        self.audit.record(
            "paired",
            json!({"device_id": id, "name": name, "via": "qr"}),
        );
        self.pairing_stop().await;
        Some((id, token))
    }
}
