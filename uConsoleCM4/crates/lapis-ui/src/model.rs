//! What the HUD knows, updated from WebSocket frames and REST answers.

use crate::client::Tag;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::time::Instant;

pub const WF_W: usize = 512;
pub const WF_H: usize = 256;

pub struct Toast {
    pub level: String,
    pub text: String,
    pub at: Instant,
}

#[derive(Default)]
pub struct Spectrum {
    /// Newest first, `WF_W` levels each.
    pub rows: VecDeque<Vec<u8>>,
    pub center_hz: u64,
    pub sample_rate: u32,
    pub gain_db: f64,
    pub floor_db: f64,
    pub ceil_db: f64,
    pub synthetic: bool,
    pub source: String,
    pub peaks: Vec<Value>,
    pub dirty: bool,
}

pub struct Image {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
    /// The remote screen's size (KVM coordinates), from `X-Lapis-Width/Height`.
    pub remote: Option<(u32, u32)>,
    pub version: u64,
}

#[derive(Default)]
pub struct Model {
    pub connected: bool,
    pub state: Value,
    pub approvals: Vec<Value>,
    pub chats: Vec<Value>,
    pub messages: BTreeMap<String, Vec<Value>>,
    pub hosts: Value,
    pub agents: BTreeMap<String, Vec<Value>>,
    pub probes: BTreeMap<String, Value>,
    pub activity: VecDeque<Value>,
    pub telemetry: Value,
    pub spectrum: Spectrum,
    pub decodes: VecDeque<Value>,
    pub mesh: Value,
    pub gps: Value,
    pub deck_status: Value,
    pub devices: Value,
    pub audit: Value,
    /// lapis.toml as GET /v1/config returns it (secret NAMES only, never values).
    pub config: Value,
    /// GET /v1/secrets/status: presence and masked tails.
    pub secrets: Value,
    /// `GET /v1/wifi`, the last scan, and what the Wi-Fi pane is waiting for ("" = nothing).
    pub wifi: Value,
    pub wifi_networks: Vec<Value>,
    pub wifi_busy: String,
    pub tool_results: BTreeMap<String, Value>,
    pub tool_busy: BTreeMap<String, bool>,
    pub toasts: VecDeque<Toast>,
    pub screenshots: BTreeMap<i64, Image>,
    pub host_screens: BTreeMap<String, Image>,
    pub mic_level: f64,
    pub screen_requests: Vec<String>,
    pub ui_wake: bool,
    pub created_chat: Option<String>,
    image_version: u64,
}

impl Model {
    pub fn toast(&mut self, level: &str, text: impl Into<String>) {
        self.toasts.push_back(Toast {
            level: level.into(),
            text: text.into(),
            at: Instant::now(),
        });
        while self.toasts.len() > 5 {
            self.toasts.pop_front();
        }
    }

    pub fn str_of<'a>(v: &'a Value, k: &str) -> &'a str {
        v[k].as_str().unwrap_or("")
    }

    pub fn pending(&self) -> Option<&Value> {
        self.approvals.first()
    }

    pub fn host_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.hosts["hosts"]
            .as_object()
            .map(|h| h.keys().cloned().collect())
            .unwrap_or_default();
        if !ids.iter().any(|h| h == "deck") {
            ids.insert(0, "deck".into());
        }
        ids
    }

    fn upsert_chat(&mut self, chat: &Value) {
        let id = Self::str_of(chat, "id").to_owned();
        if id.is_empty() {
            return;
        }
        match self.chats.iter_mut().find(|c| c["id"] == chat["id"]) {
            Some(c) => {
                if let (Some(dst), Some(src)) = (c.as_object_mut(), chat.as_object()) {
                    for (k, v) in src {
                        dst.insert(k.clone(), v.clone());
                    }
                }
            }
            None => self.chats.push(chat.clone()),
        }
        self.chats.sort_by(|a, b| {
            b["updated_at"]
                .as_i64()
                .unwrap_or(0)
                .cmp(&a["updated_at"].as_i64().unwrap_or(0))
        });
    }

    pub fn apply_frame(&mut self, f: Value) {
        match f["type"].as_str().unwrap_or("") {
            "state_sync" => {
                if let Some(list) = f["pending_approvals"].as_array() {
                    self.approvals = list.clone();
                }
                self.state = f;
            }
            "approval_request" => {
                if !self
                    .approvals
                    .iter()
                    .any(|a| a["request_id"] == f["request_id"])
                {
                    self.approvals.push(f);
                }
            }
            "approval_resolved" => {
                self.approvals
                    .retain(|a| a["request_id"] != f["request_id"]);
                let d = Self::str_of(&f, "decision").to_owned();
                self.toast(
                    if d.starts_with("approve") {
                        "info"
                    } else {
                        "warn"
                    },
                    format!(
                        "{} · {} by {}",
                        Self::str_of(&f, "request_id"),
                        d,
                        Self::str_of(&f, "by")
                    ),
                );
            }
            "chat_message_created" => {
                let chat = Self::str_of(&f, "chat_id").to_owned();
                let list = self.messages.entry(chat).or_default();
                if !list.iter().any(|m| m["id"] == f["message"]["id"]) {
                    list.push(f["message"].clone());
                }
            }
            "chat_updated" => self.upsert_chat(&f["chat"].clone()),
            "chat_deleted" => {
                let id = f["chat_id"].clone();
                self.chats.retain(|c| c["id"] != id);
            }
            "activity_event" => {
                self.activity.push_front(f);
                self.activity.truncate(60);
            }
            "task_finished" => {
                let text = format!(
                    "{} on {}: {}",
                    Self::str_of(&f, "status"),
                    Self::str_of(&f, "host"),
                    Self::str_of(&f, "summary")
                );
                self.toast("info", text);
            }
            "telemetry_tick" => self.telemetry = f["telemetry"].clone(),
            "toast" => {
                let (l, t) = (
                    Self::str_of(&f, "level").to_owned(),
                    Self::str_of(&f, "text").to_owned(),
                );
                self.toast(&l, t);
            }
            "pairing" => {
                if let Some(s) = self.state.as_object_mut() {
                    s.insert(
                        "pairing".into(),
                        if f["mode"] == "off" { Value::Null } else { f },
                    );
                }
            }
            "sdr_spectrum" => self.apply_spectrum(&f),
            "sdr_decode" => {
                self.decodes.push_front(f);
                self.decodes.truncate(100);
            }
            "mesh_message" => {
                if let Some(list) = self.mesh.get_mut("messages").and_then(Value::as_array_mut) {
                    if !list.iter().any(|m| m["id"] == f["message"]["id"]) {
                        list.push(f["message"].clone());
                    }
                }
                let m = &f["message"];
                if m["outgoing"] != true {
                    self.toast(
                        "info",
                        format!(
                            "MESH {}: {}",
                            Self::str_of(m, "from"),
                            Self::str_of(m, "text")
                        ),
                    );
                }
            }
            "mic_level" => {
                self.mic_level = f["level"].as_f64().or(f["rms"].as_f64()).unwrap_or(0.0)
            }
            "screen_request" => self
                .screen_requests
                .push(Self::str_of(&f, "request_id").to_owned()),
            "ui_settings" => {
                if let Some(s) = self.state.as_object_mut() {
                    s.insert("ui".into(), f["ui"].clone());
                }
                self.ui_wake |= f["wake"] == true;
            }
            _ => {}
        }
    }

    fn apply_spectrum(&mut self, f: &Value) {
        let Some(bins) = f["bins"].as_str().and_then(|b| B64.decode(b).ok()) else {
            return;
        };
        let row: Vec<u8> = if bins.len() == WF_W {
            bins
        } else {
            (0..WF_W)
                .map(|i| bins.get(i * bins.len() / WF_W).copied().unwrap_or(0))
                .collect()
        };
        let s = &mut self.spectrum;
        let center = f["center_hz"].as_u64().unwrap_or(0);
        if center != s.center_hz {
            s.rows.clear();
        }
        s.center_hz = center;
        s.sample_rate = f["sample_rate"].as_u64().unwrap_or(0) as u32;
        s.gain_db = f["gain_db"].as_f64().unwrap_or(0.0);
        s.floor_db = f["floor_db"].as_f64().unwrap_or(-100.0);
        s.ceil_db = f["ceil_db"].as_f64().unwrap_or(0.0);
        s.synthetic = f["synthetic"] == true;
        s.source = Self::str_of(f, "source").to_owned();
        s.peaks = f["peaks"].as_array().cloned().unwrap_or_default();
        s.rows.push_front(row);
        s.rows.truncate(WF_H);
        s.dirty = true;
    }

    fn decode_image(&mut self, body: &[u8], headers: &[(String, String)]) -> Option<Image> {
        let img = image::load_from_memory(body).ok()?.to_rgba8();
        let h = |k: &str| {
            headers
                .iter()
                .find(|(n, _)| n == k)
                .and_then(|(_, v)| v.parse::<u32>().ok())
        };
        self.image_version += 1;
        Some(Image {
            width: img.width() as usize,
            height: img.height() as usize,
            rgba: img.into_raw(),
            remote: h("x-lapis-width").zip(h("x-lapis-height")),
            version: self.image_version,
        })
    }

    pub fn apply_http(&mut self, tag: Tag, status: u16, body: &[u8], headers: &[(String, String)]) {
        if let Tag::Screenshot(id) = tag {
            if let Some(img) = self.decode_image(body, headers) {
                self.screenshots.insert(id, img);
                while self.screenshots.len() > 6 {
                    self.screenshots.pop_first();
                }
            }
            return;
        }
        if let Tag::HostScreen(host) = &tag {
            if status == 200 {
                if let Some(img) = self.decode_image(body, headers) {
                    self.host_screens.insert(host.clone(), img);
                }
            }
            return;
        }
        let v: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
        if status == 0 || status >= 400 {
            let msg = v["error"]["message"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| String::from_utf8_lossy(body).chars().take(160).collect());
            if let Tag::Tool(name, _) = &tag {
                self.tool_busy.insert(name.clone(), false);
                self.tool_results
                    .insert(name.clone(), json!({"ok": false, "error": msg}));
            }
            if matches!(tag, Tag::WifiScan | Tag::WifiConnect(_)) {
                self.wifi_busy.clear();
            }
            // The top bar already says when the harness is down; polling must not spam it.
            let harness_down = v["error"]["code"] == "harness_unavailable";
            if !matches!(tag, Tag::Ignore) && !harness_down {
                self.toast("error", format!("{} ({status})", msg));
            }
            return;
        }
        match tag {
            Tag::Chats => {
                self.chats = v["chats"].as_array().cloned().unwrap_or_default();
                self.chats.sort_by(|a, b| {
                    b["updated_at"]
                        .as_i64()
                        .unwrap_or(0)
                        .cmp(&a["updated_at"].as_i64().unwrap_or(0))
                });
            }
            Tag::Chat(id) => {
                if let Some(c) = v.get("chat") {
                    self.upsert_chat(&c.clone());
                }
                self.messages
                    .insert(id, v["messages"].as_array().cloned().unwrap_or_default());
            }
            Tag::NewChat => {
                self.upsert_chat(&v);
                self.created_chat = v["id"].as_str().map(str::to_owned);
            }
            Tag::Hosts => self.hosts = v,
            Tag::Agents(h) => {
                self.agents
                    .insert(h, v["agents"].as_array().cloned().unwrap_or_default());
            }
            Tag::Probe(h) => {
                if let Some(a) = v["agents"].as_array() {
                    self.agents.insert(h.clone(), a.clone());
                }
                self.probes.insert(h, v);
            }
            Tag::Tool(name, _) => {
                self.tool_busy.insert(name.clone(), false);
                if v["ok"] != true {
                    let e = Self::str_of(&v, "error").to_owned();
                    self.toast(
                        if v["decision"] == "deny" || v["decision"] == "timeout" {
                            "warn"
                        } else {
                            "error"
                        },
                        format!("{name}: {e}"),
                    );
                }
                self.tool_results.insert(name, v);
            }
            Tag::DeckStatus => self.deck_status = v,
            Tag::Mesh => self.mesh = v,
            Tag::Gps => self.gps = v,
            Tag::Sdr => {}
            Tag::Devices => self.devices = v,
            Tag::Audit => self.audit = v,
            Tag::Config => self.config = v["config"].clone(),
            Tag::ConfigSaved => {
                self.config = v["config"].clone();
                self.toast("info", "AI provider saved");
            }
            Tag::Secrets => self.secrets = v["secrets"].clone(),
            Tag::HostsSaved => {
                self.hosts = v;
                self.toast("info", "Fleet hosts saved");
            }
            Tag::Wifi => self.wifi = v,
            Tag::WifiScan => {
                self.wifi_busy.clear();
                self.wifi_networks = v["networks"].as_array().cloned().unwrap_or_default();
            }
            Tag::WifiConnect(ssid) => {
                self.wifi_busy.clear();
                if v["state"] == "connected" {
                    let ip = Self::str_of(&v, "local_ip").to_owned();
                    self.toast("info", format!("Joined {ssid} ({ip})"));
                } else {
                    self.toast(
                        "error",
                        format!("Could not join {ssid}: wrong password or out of range"),
                    );
                }
            }
            Tag::WifiForget(name) => {
                self.wifi = v;
                self.toast("info", format!("Forgot {name}"));
            }
            Tag::WifiBle => {
                self.wifi = v;
                self.toast(
                    "info",
                    "Paired phones can set Wi-Fi over Bluetooth for 5 minutes",
                );
            }
            Tag::Screenshot(_) | Tag::HostScreen(_) | Tag::Ignore => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_update_the_model() {
        let mut m = Model::default();
        m.apply_frame(json!({"type": "state_sync", "agent_state": "IDLE", "pending_approvals": [{"request_id": "req_1", "tier": 1}]}));
        assert_eq!(m.pending().unwrap()["request_id"], "req_1");
        m.apply_frame(json!({"type": "approval_request", "request_id": "req_2", "tier": 2}));
        m.apply_frame(json!({"type": "approval_resolved", "request_id": "req_1", "decision": "approve_once", "by": "device"}));
        assert_eq!(m.approvals.len(), 1);
        assert_eq!(m.pending().unwrap()["tier"], 2);
        m.apply_frame(
            json!({"type": "chat_updated", "chat": {"id": "c1", "title": "t", "updated_at": 5}}),
        );
        m.apply_frame(json!({"type": "chat_message_created", "chat_id": "c1", "message": {"id": 1, "role": "user", "content": "hi"}}));
        m.apply_frame(json!({"type": "chat_message_created", "chat_id": "c1", "message": {"id": 1, "role": "user", "content": "hi"}}));
        assert_eq!(m.messages["c1"].len(), 1);
        let bins = B64.encode(vec![128u8; 512]);
        m.apply_frame(json!({"type": "sdr_spectrum", "center_hz": 100_000_000, "sample_rate": 2_048_000, "bins": bins, "floor_db": -90.0, "ceil_db": -20.0}));
        assert_eq!(m.spectrum.rows.len(), 1);
        assert_eq!(m.spectrum.rows[0].len(), WF_W);
        m.apply_frame(json!({"type": "pairing", "mode": "pin", "pin": "123456"}));
        assert_eq!(m.state["pairing"]["pin"], "123456");
        m.apply_frame(json!({"type": "pairing", "mode": "off"}));
        assert!(m.state["pairing"].is_null());
        m.apply_http(
            Tag::Tool("gps.get_fix".into(), 1),
            200,
            br#"{"ok":true,"result":{"fix":{"fix":"3D"}}}"#,
            &[],
        );
        assert_eq!(m.tool_results["gps.get_fix"]["result"]["fix"]["fix"], "3D");
        m.apply_http(
            Tag::Chats,
            503,
            br#"{"error":{"code":"harness_unavailable","message":"down"}}"#,
            &[],
        );
        // The top bar shows a down harness; polling must not toast it every 3 s.
        assert!(!m.toasts.iter().any(|t| t.text.contains("down")));
        m.apply_http(
            Tag::Hosts,
            422,
            br#"{"error":{"code":"invalid","message":"bad host"}}"#,
            &[],
        );
        assert!(m.toasts.iter().any(|t| t.text.contains("bad host")));
        assert_eq!(m.host_ids()[0], "deck");
    }
}
