//! lapis-ble-v1 provisioning session: handshake, PIN proof, lockout, commands. Ported from
//! Lapis-Zero's `ble_prov/session.py` (hardware-tested) so the same Android app pairs with it.
//! Paired-phone mode (protocol 4.2): a phone paired earlier proves its api_token instead of a
//! PIN (`"tok:" + sha256 hex` takes the PIN's place in the salt) and may only change Wi-Fi.
//! Transport-independent: the GATT server feeds bytes in and sends [`Outbound`] frames out;
//! the tests drive it with a fake phone.

use lapis_crypto::ble::{self, DeviceChannel, Ephemeral, Hello};
use lapis_crypto::framing::{self, Reassembler, FLAG_ENCRYPTED};
use regex::Regex;
use serde_json::{json, Map, Value};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex};

pub const MAX_FAILURES: u32 = 3;
pub const LOCKOUT_COOLDOWN: Duration = Duration::from_secs(5);
pub const TAILSCALE_TIMEOUT: Duration = Duration::from_secs(300);
pub const NOT_FOR_PAIRED: &str = "not allowed for a paired phone";
pub const HTTPS_NOT_ENABLED: &str = "Joined the tailnet, but HTTPS is not enabled for it. In the Tailscale admin console (DNS page) enable MagicDNS and HTTPS Certificates, then retry.";

pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What provisioning does to the device; faked in tests.
pub trait Backend: Send + Sync {
    fn register_device(&self, name: &str, model: &str) -> (String, String);
    fn wifi_scan(&self) -> BoxFut<'_, Result<Vec<Value>, String>>;
    fn wifi_connect(&self, ssid: String, psk: Option<String>) -> BoxFut<'_, Value>;
    fn tailscale_status(&self) -> BoxFut<'_, crate::net::NetStatus>;
    fn tailscale_up(&self, authkey: Option<String>);
    fn tailscale_up_error(&self) -> Option<String>;
    fn tailscale_serve(&self) -> BoxFut<'_, bool>;
    fn push_secrets(&self, values: Map<String, Value>) -> BoxFut<'_, Result<Vec<String>, String>>;
    /// `(device id, token hash hex)` of every phone paired before (not revoked).
    fn paired_secrets(&self) -> Vec<(String, String)>;
    /// Pairing view updates: waiting | connected | verified | wifi | tailscale | locked | done.
    fn status(&self, status: &str, pin: &str);
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outbound {
    Frame(Vec<u8>),
    Disconnect,
}

struct Inner {
    eph: Ephemeral,
    pin: String,
    /// False in the phone Wi-Fi window: no PIN is on screen, so none is accepted.
    allow_pin: bool,
    /// Key sets derived at the handshake; the first envelope that opens picks the mode.
    candidates: Vec<(Option<String>, ble::SessionKeys)>,
    /// The paired phone's device id once a token key set opened the first envelope.
    paired: Option<String>,
    chan: Option<DeviceChannel>,
    rx: Reassembler,
    tx_msg: u8,
    mtu: usize,
    verified: bool,
    failures: u32,
    locked_until: Option<Instant>,
    credentials: Option<(String, String)>,
    done: bool,
}

impl Inner {
    fn rotate(&mut self) {
        self.eph = Ephemeral::generate();
        self.pin = lapis_crypto::token::generate_pin();
        self.chan = None;
        self.candidates.clear();
        self.paired = None;
        self.verified = false;
        self.failures = 0;
        self.rx.reset();
    }

    /// Pairing-screen statuses belong to PIN sessions only.
    fn shows_status(&self) -> bool {
        self.allow_pin && self.paired.is_none()
    }

    fn locked(&self) -> bool {
        self.locked_until.is_some_and(|t| Instant::now() < t)
    }
}

pub struct Session {
    inner: Mutex<Inner>,
    backend: Arc<dyn Backend>,
    out: mpsc::UnboundedSender<Outbound>,
    tailscale: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    pub tailscale_timeout: Duration,
    pub poll_interval: Duration,
}

fn secret_name_re() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]{0,63}$").ok())
        .as_ref()
}

impl Session {
    pub fn new(backend: Arc<dyn Backend>, out: mpsc::UnboundedSender<Outbound>) -> Arc<Self> {
        Self::build(backend, out, true)
    }

    /// The phone Wi-Fi window: paired phones only, no PIN pairing.
    pub fn paired_only(
        backend: Arc<dyn Backend>,
        out: mpsc::UnboundedSender<Outbound>,
    ) -> Arc<Self> {
        Self::build(backend, out, false)
    }

    fn build(
        backend: Arc<dyn Backend>,
        out: mpsc::UnboundedSender<Outbound>,
        allow_pin: bool,
    ) -> Arc<Self> {
        let mut inner = Inner {
            eph: Ephemeral::generate(),
            pin: String::new(),
            allow_pin,
            candidates: Vec::new(),
            paired: None,
            chan: None,
            rx: Reassembler::default(),
            tx_msg: 0,
            mtu: framing::DEFAULT_ATT_MTU,
            verified: false,
            failures: 0,
            locked_until: None,
            credentials: None,
            done: false,
        };
        inner.rotate();
        Arc::new(Session {
            inner: Mutex::new(inner),
            backend,
            out,
            tailscale: std::sync::Mutex::new(None),
            tailscale_timeout: TAILSCALE_TIMEOUT,
            poll_interval: Duration::from_secs(2),
        })
    }

    pub async fn pin(&self) -> String {
        self.inner.lock().await.pin.clone()
    }

    pub async fn is_done(&self) -> bool {
        self.inner.lock().await.done
    }

    pub async fn is_verified(&self) -> bool {
        self.inner.lock().await.verified
    }

    pub async fn set_mtu(&self, mtu: usize) {
        if mtu >= framing::DEFAULT_ATT_MTU {
            self.inner.lock().await.mtu = mtu;
        }
    }

    /// ServerHello (`HANDSHAKE` read).
    pub async fn handshake_read(&self) -> Vec<u8> {
        self.inner.lock().await.eph.hello.encode().to_vec()
    }

    /// ClientHello (`HANDSHAKE` write): derive the keys bound to the PIN on screen and to
    /// each paired phone's token.
    pub async fn handshake_write(&self, data: &[u8]) -> Result<(), String> {
        let paired = self.backend.paired_secrets();
        let mut inner = self.inner.lock().await;
        if inner.locked() {
            return Err("locked out; retry shortly".into());
        }
        let client = Hello::decode(data).map_err(|e| e.to_string())?;
        let shared = inner
            .eph
            .shared(&client.public)
            .map_err(|e| e.to_string())?;
        let device = inner.eph.hello;
        let mut candidates = Vec::new();
        if inner.allow_pin {
            candidates.push((None, ble::derive(&shared, &client, &device, &inner.pin)));
        }
        for (id, hash) in paired {
            let keys = ble::derive(&shared, &client, &device, &format!("tok:{hash}"));
            candidates.push((Some(id), keys));
        }
        inner.candidates = candidates;
        inner.chan = None;
        inner.paired = None;
        inner.tx_msg = 0;
        inner.verified = false;
        inner.rx.reset();
        let (pin, show) = (inner.pin.clone(), inner.allow_pin);
        drop(inner);
        if show {
            self.backend.status("connected", &pin);
        }
        Ok(())
    }

    /// One GATT write on `CMD_TX`.
    pub async fn on_frame(self: &Arc<Self>, frame: &[u8]) {
        let command = {
            let mut inner = self.inner.lock().await;
            if inner.locked() {
                return;
            }
            let complete = match inner.rx.feed(frame, Instant::now()) {
                Ok(c) => c,
                Err(_) => {
                    let _ = self
                        .out
                        .send(Outbound::Frame(framing::reset_frame(0).to_vec()));
                    return;
                }
            };
            let Some((flags, payload)) = complete else {
                return;
            };
            if flags & FLAG_ENCRYPTED == 0 || (inner.chan.is_none() && inner.candidates.is_empty())
            {
                let _ = self
                    .out
                    .send(Outbound::Frame(framing::reset_frame(0).to_vec()));
                return;
            }
            if inner.chan.is_none() {
                // First envelope: whichever key set opens it decides PIN vs paired phone.
                let mut chosen = None;
                for (id, keys) in &inner.candidates {
                    let mut chan = DeviceChannel::new(keys.clone());
                    if let Ok(pt) = chan.open(&payload) {
                        chosen = Some((id.clone(), chan, pt));
                        break;
                    }
                }
                let Some((id, chan, pt)) = chosen else {
                    drop(inner);
                    return self.failure().await; // wrong PIN or unknown phone
                };
                let back_to_waiting = id.is_some() && inner.allow_pin;
                inner.paired = id;
                inner.chan = Some(chan);
                inner.candidates.clear();
                if back_to_waiting {
                    // Not the new phone the PIN screen waits for: show it as it was.
                    self.backend.status("waiting", &inner.pin);
                }
                match serde_json::from_slice::<Value>(&pt) {
                    Ok(Value::Object(cmd)) => cmd,
                    _ => {
                        drop(inner);
                        return self.failure().await;
                    }
                }
            } else {
                let opened = inner.chan.as_mut().map(|c| c.open(&payload));
                match opened {
                    Some(Ok(pt)) => match serde_json::from_slice::<Value>(&pt) {
                        Ok(Value::Object(cmd)) => cmd,
                        _ => {
                            drop(inner);
                            return self.failure().await;
                        }
                    },
                    _ => {
                        drop(inner);
                        return self.failure().await;
                    }
                }
            }
        };
        self.dispatch(command).await;
    }

    async fn failure(&self) {
        let mut inner = self.inner.lock().await;
        if inner.verified {
            return; // tampering after verification is dropped, never counted against the PIN
        }
        inner.failures += 1;
        if inner.failures < MAX_FAILURES {
            return;
        }
        inner.locked_until = Some(Instant::now() + LOCKOUT_COOLDOWN);
        inner.rotate();
        let (pin, show) = (inner.pin.clone(), inner.allow_pin);
        drop(inner);
        if show {
            self.backend.status("locked", &pin);
        }
        let _ = self.out.send(Outbound::Disconnect);
    }

    async fn send(&self, event: Value) {
        let mut inner = self.inner.lock().await;
        let mtu = inner.mtu;
        let msg_id = inner.tx_msg;
        let Some(chan) = inner.chan.as_mut() else {
            return;
        };
        let Ok(envelope) = chan.seal(event.to_string().as_bytes()) else {
            return;
        };
        match framing::split(
            msg_id,
            FLAG_ENCRYPTED,
            &envelope,
            framing::max_chunk_data(mtu),
        ) {
            Ok(frames) => {
                inner.tx_msg = inner.tx_msg.wrapping_add(1);
                for f in frames {
                    let _ = self.out.send(Outbound::Frame(f));
                }
            }
            Err(e) => tracing::warn!(error = %e, "provisioning event too large to frame"),
        }
    }

    async fn error(&self, cmd: &str, message: &str) {
        self.send(json!({"evt": "error", "cmd": cmd, "message": message}))
            .await;
    }

    async fn dispatch(self: &Arc<Self>, command: Map<String, Value>) {
        let cmd = command
            .get("cmd")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let (paired, show) = {
            let inner = self.inner.lock().await;
            (inner.paired.is_some(), inner.shows_status())
        };
        match (cmd.as_str(), paired) {
            ("pin_verify", false) => return self.pin_verify(&command).await,
            ("token_verify", true) => return self.token_verify(&command).await,
            ("pin_verify" | "token_verify", _) => return self.failure().await,
            _ => {}
        }
        if !self.is_verified().await {
            return self.failure().await;
        }
        if paired
            && !matches!(
                cmd.as_str(),
                "wifi_scan" | "wifi_connect" | "finish_provisioning"
            )
        {
            return self.error(&cmd, NOT_FOR_PAIRED).await;
        }
        let pin = self.pin().await;
        let status = |s: &str| {
            if show {
                self.backend.status(s, &pin);
            }
        };
        match cmd.as_str() {
            "wifi_scan" => match self.backend.wifi_scan().await {
                Ok(networks) => {
                    self.send(json!({"evt": "wifi_list", "networks": networks}))
                        .await
                }
                Err(e) => self.error(&cmd, &e).await,
            },
            "wifi_connect" => {
                let ssid = command
                    .get("ssid")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                if ssid.is_empty() {
                    return self.error("wifi_connect", "ssid is required").await;
                }
                let psk = command
                    .get("psk")
                    .and_then(Value::as_str)
                    .filter(|p| !p.is_empty())
                    .map(str::to_owned);
                status("wifi");
                self.send(json!({"evt": "wifi_status", "state": "connecting", "local_ip": ""}))
                    .await;
                let mut result = self.backend.wifi_connect(ssid, psk).await;
                if let Some(obj) = result.as_object_mut() {
                    obj.insert("evt".into(), json!("wifi_status"));
                }
                self.send(result).await;
            }
            "tailscale_up" => {
                let key = command
                    .get("authkey")
                    .and_then(Value::as_str)
                    .filter(|k| !k.is_empty())
                    .map(str::to_owned);
                status("tailscale");
                let me = self.clone();
                let task = tokio::spawn(async move { me.watch_tailscale(key).await });
                if let Ok(mut slot) = self.tailscale.lock() {
                    if let Some(old) = slot.replace(task) {
                        old.abort();
                    }
                }
            }
            "push_secrets" => self.push_secrets(&command).await,
            "finish_provisioning" => {
                self.send(json!({"evt": "bye"})).await;
                let mut inner = self.inner.lock().await;
                if paired && inner.allow_pin {
                    // A PIN window stays open for the new phone it was opened for.
                    inner.chan = None;
                    inner.paired = None;
                    inner.verified = false;
                } else {
                    inner.done = true;
                    drop(inner);
                    status("done");
                }
            }
            _ => self.error(&cmd, "unknown command").await,
        }
    }

    /// A paired phone proves its token (the keys are already bound to it); no new token.
    async fn token_verify(&self, command: &Map<String, Value>) {
        let proof = command
            .get("proof")
            .and_then(Value::as_str)
            .and_then(lapis_crypto_hex)
            .unwrap_or_default();
        let answer = {
            let mut inner = self.inner.lock().await;
            let keys = inner.chan.as_ref().map(|c| c.keys().clone());
            match (keys, inner.paired.clone()) {
                (Some(k), Some(id)) if k.verify_client_proof(&proof) => {
                    inner.verified = true;
                    inner.failures = 0;
                    Some((hex_encode(&k.server_proof()), id))
                }
                _ => None,
            }
        };
        match answer {
            Some((proof, id)) => {
                tracing::info!(device_id = %id, "BLE: paired phone verified (Wi-Fi only)");
                self.send(json!({"evt": "token_verified", "proof": proof, "device_id": id}))
                    .await
            }
            None => self.failure().await,
        }
    }

    async fn pin_verify(&self, command: &Map<String, Value>) {
        let proof = command
            .get("proof")
            .and_then(Value::as_str)
            .and_then(lapis_crypto_hex)
            .unwrap_or_default();
        let (ok, already) = {
            let inner = self.inner.lock().await;
            let ok = inner
                .chan
                .as_ref()
                .is_some_and(|c| c.keys().verify_client_proof(&proof));
            (ok, inner.credentials.clone().filter(|_| inner.verified))
        };
        if !ok {
            return self.failure().await;
        }
        let (device_id, token) = match already {
            Some(c) => c,
            None => {
                let name = command
                    .get("device_name")
                    .and_then(Value::as_str)
                    .unwrap_or("Android");
                let model = command
                    .get("device_model")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let creds = self.backend.register_device(name, model);
                let mut inner = self.inner.lock().await;
                inner.verified = true;
                inner.failures = 0;
                inner.credentials = Some(creds.clone());
                creds
            }
        };
        let (server_proof, pin) = {
            let inner = self.inner.lock().await;
            (
                inner
                    .chan
                    .as_ref()
                    .map(|c| c.keys().server_proof())
                    .unwrap_or([0; 32]),
                inner.pin.clone(),
            )
        };
        self.send(json!({"evt": "pin_verified", "proof": hex_encode(&server_proof), "device_id": device_id, "api_token": token}))
            .await;
        self.backend.status("verified", &pin);
    }

    async fn watch_tailscale(&self, authkey: Option<String>) {
        let deadline = Instant::now() + self.tailscale_timeout;
        let mut sent_url: Option<String> = None;
        // One status read first: a node already joined only needs serve + "ready"; `up` with
        // a key would restart the backend and re-submit a key that may be consumed.
        let mut status = self.backend.tailscale_status().await;
        if status.tailscale_state != "connected" {
            self.backend.tailscale_up(authkey.clone());
        }
        while Instant::now() < deadline {
            if status.tailscale_state == "connected" {
                if !self.backend.tailscale_serve().await {
                    return self.error("tailscale_up", HTTPS_NOT_ENABLED).await;
                }
                return self
                    .send(json!({"evt": "ready", "tailscale_fqdn": status.tailscale_fqdn, "tailscale_ip": status.tailscale_ip}))
                    .await;
            }
            if let (Some(url), None) = (&status.tailscale_auth_url, &authkey) {
                if sent_url.as_ref() != Some(url) {
                    sent_url = Some(url.clone());
                    self.send(json!({"evt": "tailscale_auth", "url": url}))
                        .await;
                }
            } else if let Some(why) = self.backend.tailscale_up_error() {
                return self.error("tailscale_up", &why).await;
            }
            tokio::time::sleep(self.poll_interval).await;
            status = self.backend.tailscale_status().await;
        }
        self.error("tailscale_up", "timed out waiting for Tailscale to connect")
            .await;
    }

    async fn push_secrets(&self, command: &Map<String, Value>) {
        let Some(values) = command
            .get("secrets")
            .and_then(Value::as_object)
            .filter(|v| !v.is_empty())
        else {
            return self
                .error("push_secrets", "secrets must be a non-empty object")
                .await;
        };
        for (name, value) in values {
            let ok = value
                .as_str()
                .is_some_and(|v| !v.is_empty() && !v.contains(['\n', '\r']));
            if !secret_name_re().is_some_and(|re| re.is_match(name)) || !ok {
                return self
                    .error("push_secrets", &format!("invalid secret {name:?}"))
                    .await;
            }
        }
        for v in values.values().filter_map(Value::as_str) {
            lapis_crypto::register_secret(v);
        }
        match self.backend.push_secrets(values.clone()).await {
            Ok(keys) => {
                self.send(json!({"evt": "secrets_saved", "keys": keys}))
                    .await
            }
            Err(e) => self.error("push_secrets", &e).await,
        }
    }

    pub fn close(&self) {
        if let Ok(mut slot) = self.tailscale.lock() {
            if let Some(t) = slot.take() {
                t.abort();
            }
        }
    }
}

fn lapis_crypto_hex(h: &str) -> Option<Vec<u8>> {
    if !h.len().is_multiple_of(2) || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..h.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&h[i..i + 2], 16).ok())
        .collect()
}

fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::NetStatus;
    use lapis_crypto::ble::ClientChannel;
    use std::sync::Mutex as StdMutex;

    #[derive(Default)]
    struct FakeBackend {
        statuses: StdMutex<Vec<String>>,
        ts: StdMutex<NetStatus>,
        secrets: StdMutex<Vec<String>>,
        paired: StdMutex<Vec<(String, String)>>,
    }

    const PHONE_TOKEN: &str = "phone-token-0123456789";

    fn with_paired_phone() -> Arc<FakeBackend> {
        let b = FakeBackend::default();
        b.paired.lock().unwrap().push((
            "dev_07".into(),
            lapis_crypto::token::hash_token(PHONE_TOKEN),
        ));
        Arc::new(b)
    }

    impl Backend for FakeBackend {
        fn register_device(&self, _name: &str, _model: &str) -> (String, String) {
            ("dev_01".into(), "tok-abcdefghijklmnop".into())
        }
        fn wifi_scan(&self) -> BoxFut<'_, Result<Vec<Value>, String>> {
            Box::pin(async { Ok(vec![json!({"ssid": "home", "rssi": -50, "secured": true})]) })
        }
        fn wifi_connect(&self, ssid: String, _psk: Option<String>) -> BoxFut<'_, Value> {
            Box::pin(async move {
                json!({"state": if ssid == "home" { "connected" } else { "failed" }, "local_ip": "192.168.1.9"})
            })
        }
        fn tailscale_status(&self) -> BoxFut<'_, NetStatus> {
            Box::pin(async { self.ts.lock().unwrap().clone() })
        }
        fn tailscale_up(&self, _authkey: Option<String>) {
            let mut ts = self.ts.lock().unwrap();
            ts.tailscale_auth_url = Some("https://login.tailscale.com/a/x".into());
        }
        fn tailscale_up_error(&self) -> Option<String> {
            None
        }
        fn tailscale_serve(&self) -> BoxFut<'_, bool> {
            Box::pin(async { true })
        }
        fn push_secrets(
            &self,
            values: Map<String, Value>,
        ) -> BoxFut<'_, Result<Vec<String>, String>> {
            Box::pin(async move {
                let keys: Vec<String> = values.keys().cloned().collect();
                self.secrets.lock().unwrap().extend(keys.clone());
                Ok(keys)
            })
        }
        fn paired_secrets(&self) -> Vec<(String, String)> {
            self.paired.lock().unwrap().clone()
        }
        fn status(&self, status: &str, _pin: &str) {
            self.statuses.lock().unwrap().push(status.into());
        }
    }

    fn token_secret(token: &str) -> String {
        format!("tok:{}", lapis_crypto::token::hash_token(token))
    }

    /// A paired phone: proves its token, changes Wi-Fi, and nothing else.
    #[tokio::test]
    async fn paired_phone_changes_wifi_only() {
        let backend = with_paired_phone();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let session = Session::new(backend.clone(), tx); // a PIN window accepts paired phones too
        let mut phone = connect(&session, &token_secret(PHONE_TOKEN)).await;
        let keys = phone_keys(&session, &phone, &token_secret(PHONE_TOKEN)).await;
        phone
            .send(
                &session,
                json!({"cmd": "token_verify", "proof": hex_encode(&keys.client_proof())}),
            )
            .await;
        let v = phone.recv(&mut rx).unwrap();
        assert_eq!(
            (v["evt"].as_str(), v["device_id"].as_str()),
            (Some("token_verified"), Some("dev_07"))
        );
        assert!(
            v.get("api_token").is_none(),
            "no new token for a paired phone"
        );
        let server = lapis_crypto_hex(v["proof"].as_str().unwrap()).unwrap();
        assert!(keys.verify_server_proof(&server));
        phone.send(&session, json!({"cmd": "wifi_scan"})).await;
        assert_eq!(phone.recv(&mut rx).unwrap()["evt"], "wifi_list");
        phone
            .send(
                &session,
                json!({"cmd": "wifi_connect", "ssid": "home", "psk": "pw"}),
            )
            .await;
        assert_eq!(phone.recv(&mut rx).unwrap()["state"], "connecting");
        assert_eq!(phone.recv(&mut rx).unwrap()["state"], "connected");
        for cmd in [
            json!({"cmd": "tailscale_up", "authkey": null}),
            json!({"cmd": "push_secrets", "secrets": {"ANTHROPIC_API_KEY": "sk-ant-x"}}),
        ] {
            phone.send(&session, cmd).await;
            assert_eq!(phone.recv(&mut rx).unwrap()["message"], NOT_FOR_PAIRED);
        }
        assert!(backend.secrets.lock().unwrap().is_empty());
        phone
            .send(&session, json!({"cmd": "finish_provisioning"}))
            .await;
        assert_eq!(phone.recv(&mut rx).unwrap()["evt"], "bye");
        assert!(
            !session.is_done().await && !session.is_verified().await,
            "the PIN window stays open for the new phone"
        );
        // The PIN screen went back to waiting: no verified / wifi / done.
        assert_eq!(*backend.statuses.lock().unwrap(), ["connected", "waiting"]);
    }

    /// The phone Wi-Fi window shows no PIN, so a PIN never works there; nor does a wrong token,
    /// a revoked one (absent from paired_secrets), or pin_verify on token keys.
    #[tokio::test]
    async fn paired_only_window_refuses_pin_and_unknown_phones() {
        let backend = with_paired_phone();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let session = Session::paired_only(backend.clone(), tx);
        let pin = session.pin().await;
        let mut phone = connect(&session, &pin).await;
        let proof = hex_encode(&phone.chan_keys_client_proof(&session).await);
        phone
            .send(&session, json!({"cmd": "pin_verify", "proof": proof}))
            .await;
        assert!(phone.recv(&mut rx).is_none(), "PIN keys are not offered");

        let secret = token_secret(PHONE_TOKEN);
        let mut phone = connect(&session, &secret).await;
        let keys = phone_keys(&session, &phone, &secret).await;
        phone
            .send(
                &session,
                json!({"cmd": "pin_verify", "proof": hex_encode(&keys.client_proof())}),
            )
            .await;
        assert!(phone.recv(&mut rx).is_none(), "pin_verify on token keys");

        let mut stranger = connect(&session, &token_secret("not-paired")).await;
        stranger
            .send(&session, json!({"cmd": "token_verify", "proof": "00"}))
            .await;
        let mut disconnected = false;
        while let Ok(o) = rx.try_recv() {
            disconnected |= o == Outbound::Disconnect;
        }
        assert!(disconnected, "three failures lock out as with a wrong PIN");
        assert!(
            backend.statuses.lock().unwrap().is_empty(),
            "no pairing screen"
        );
    }

    struct Phone {
        eph: Ephemeral,
        chan: ClientChannel,
        rx: Reassembler,
        msg: u8,
    }

    async fn connect(session: &Arc<Session>, pin: &str) -> Phone {
        let eph = Ephemeral::generate();
        session.handshake_write(&eph.hello.encode()).await.unwrap();
        let server = Hello::decode(&session.handshake_read().await).unwrap();
        let shared = eph.shared(&server.public).unwrap();
        let keys = ble::derive(&shared, &eph.hello, &server, pin);
        Phone {
            eph,
            chan: ClientChannel::new(keys),
            rx: Reassembler::default(),
            msg: 0,
        }
    }

    impl Phone {
        async fn send(&mut self, session: &Arc<Session>, cmd: Value) {
            let env = self.chan.seal(cmd.to_string().as_bytes()).unwrap();
            for f in framing::split(self.msg, FLAG_ENCRYPTED, &env, 16).unwrap() {
                session.on_frame(&f).await;
            }
            self.msg = self.msg.wrapping_add(1);
        }

        fn recv(&mut self, out: &mut mpsc::UnboundedReceiver<Outbound>) -> Option<Value> {
            while let Ok(o) = out.try_recv() {
                if let Outbound::Frame(f) = o {
                    if let Ok(Some((_, payload))) = self.rx.feed(&f, Instant::now()) {
                        return serde_json::from_slice(&self.chan.open(&payload).unwrap()).ok();
                    }
                }
            }
            None
        }
    }

    #[tokio::test]
    async fn full_provisioning_flow() {
        let backend = with_paired_phone(); // PIN pairing still works with phones paired before
        let (tx, mut rx) = mpsc::unbounded_channel();
        let session = Session::new(backend.clone(), tx);
        let pin = session.pin().await;
        let mut phone = connect(&session, &pin).await;
        let proof = hex_encode(&phone.chan_keys_client_proof(&session).await);
        phone
            .send(
                &session,
                json!({"cmd": "pin_verify", "proof": proof, "device_name": "Pixel"}),
            )
            .await;
        let v = phone.recv(&mut rx).unwrap();
        assert_eq!(v["evt"], "pin_verified");
        assert_eq!(v["api_token"], "tok-abcdefghijklmnop");
        phone.send(&session, json!({"cmd": "wifi_scan"})).await;
        assert_eq!(phone.recv(&mut rx).unwrap()["networks"][0]["ssid"], "home");
        phone
            .send(
                &session,
                json!({"cmd": "wifi_connect", "ssid": "home", "psk": "pw"}),
            )
            .await;
        assert_eq!(phone.recv(&mut rx).unwrap()["state"], "connecting");
        assert_eq!(phone.recv(&mut rx).unwrap()["state"], "connected");
        phone
            .send(
                &session,
                json!({"cmd": "push_secrets", "secrets": {"ANTHROPIC_API_KEY": "sk-ant-x"}}),
            )
            .await;
        assert_eq!(phone.recv(&mut rx).unwrap()["keys"][0], "ANTHROPIC_API_KEY");
        phone
            .send(
                &session,
                json!({"cmd": "push_secrets", "secrets": {"BAD NAME": "x"}}),
            )
            .await;
        assert_eq!(phone.recv(&mut rx).unwrap()["evt"], "error");
        phone.send(&session, json!({"cmd": "nope"})).await;
        assert_eq!(phone.recv(&mut rx).unwrap()["message"], "unknown command");
        phone
            .send(&session, json!({"cmd": "finish_provisioning"}))
            .await;
        assert_eq!(phone.recv(&mut rx).unwrap()["evt"], "bye");
        assert!(session.is_done().await);
        assert_eq!(
            *backend.statuses.lock().unwrap(),
            ["connected", "verified", "wifi", "done"]
        );
        let _ = phone.eph.hello;
    }

    #[tokio::test]
    async fn tailscale_login_link_then_ready() {
        let backend = Arc::new(FakeBackend::default());
        backend.ts.lock().unwrap().tailscale_state = "needs_login".into();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut s = Session::new(backend.clone(), tx);
        Arc::get_mut(&mut s).unwrap().poll_interval = Duration::from_millis(10);
        let pin = s.pin().await;
        let mut phone = connect(&s, &pin).await;
        let proof = hex_encode(&phone.chan_keys_client_proof(&s).await);
        phone
            .send(&s, json!({"cmd": "pin_verify", "proof": proof}))
            .await;
        phone.recv(&mut rx).unwrap();
        phone
            .send(&s, json!({"cmd": "tailscale_up", "authkey": null}))
            .await;
        let mut got = None;
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            if let Some(v) = phone.recv(&mut rx) {
                got = Some(v);
                break;
            }
        }
        assert_eq!(got.unwrap()["evt"], "tailscale_auth");
        {
            let mut ts = backend.ts.lock().unwrap();
            ts.tailscale_state = "connected".into();
            ts.tailscale_fqdn = "deck.tail1.ts.net".into();
        }
        let mut ready = None;
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            if let Some(v) = phone.recv(&mut rx) {
                ready = Some(v);
                break;
            }
        }
        let ready = ready.unwrap();
        assert_eq!(
            (ready["evt"].as_str(), ready["tailscale_fqdn"].as_str()),
            (Some("ready"), Some("deck.tail1.ts.net"))
        );
        s.close();
    }

    #[tokio::test]
    async fn wrong_pin_three_times_rotates_and_disconnects() {
        let backend = Arc::new(FakeBackend::default());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let session = Session::new(backend.clone(), tx);
        let pin = session.pin().await;
        let wrong = if pin == "000000" { "111111" } else { "000000" };
        let mut phone = connect(&session, wrong).await;
        for _ in 0..3 {
            // Same seq each time: a GCM failure does not consume it (protocol 4.1).
            let keys = phone_keys(&session, &phone, wrong).await;
            phone.chan = ClientChannel::new(keys);
            phone
                .send(&session, json!({"cmd": "pin_verify", "proof": "00"}))
                .await;
        }
        let mut disconnected = false;
        while let Ok(o) = rx.try_recv() {
            disconnected |= o == Outbound::Disconnect;
        }
        assert!(disconnected);
        assert_ne!(session.pin().await, pin, "PIN rotated");
        assert!(
            session
                .handshake_write(&phone.eph.hello.encode())
                .await
                .is_err(),
            "cooldown"
        );
        assert!(backend
            .statuses
            .lock()
            .unwrap()
            .contains(&"locked".to_string()));
    }

    async fn phone_keys(session: &Arc<Session>, phone: &Phone, pin: &str) -> ble::SessionKeys {
        let server = Hello::decode(&session.handshake_read().await).unwrap();
        let shared = phone.eph.shared(&server.public).unwrap();
        ble::derive(&shared, &phone.eph.hello, &server, pin)
    }

    impl Phone {
        async fn chan_keys_client_proof(&self, session: &Arc<Session>) -> [u8; 32] {
            let pin = session.pin().await;
            phone_keys(session, self, &pin).await.client_proof()
        }
    }

    #[test]
    fn hex_helpers() {
        assert_eq!(lapis_crypto_hex("0aff"), Some(vec![0x0a, 0xff]));
        assert_eq!(lapis_crypto_hex("0g"), None);
        assert_eq!(hex_encode(&[1, 171]), "01ab");
    }
}
