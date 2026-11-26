//! The HUD's connection to lapis-daemon: one WebSocket (state and live events, the same
//! stream the phone gets) and REST calls on worker threads. Nothing blocks the frame loop.

use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::Read;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What a REST answer belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tag {
    Chats,
    Chat(String),
    NewChat,
    Hosts,
    Agents(String),
    Probe(String),
    Tool(String, u64),
    DeckStatus,
    Mesh,
    Gps,
    Sdr,
    Devices,
    Audit,
    HostScreen(String),
    Screenshot(i64),
    Config,
    ConfigSaved,
    Secrets,
    HostsSaved,
    Ignore,
}

#[derive(Debug)]
pub enum Inbound {
    Frame(Value),
    Http {
        tag: Tag,
        status: u16,
        body: Vec<u8>,
        headers: Vec<(String, String)>,
    },
    Connected(bool),
}

pub type Wake = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone)]
pub struct Client {
    base: Arc<String>,
    token: Arc<String>,
    inbox: Arc<Mutex<VecDeque<Inbound>>>,
    ws_out: Option<mpsc::Sender<String>>,
    wake: Wake,
    /// Test mode: everything sent is recorded here instead of leaving the process.
    pub record: Option<Arc<Mutex<Vec<Value>>>>,
}

impl Client {
    pub fn start(base: &str, token: String, wake: Wake) -> Self {
        let (tx, rx) = mpsc::channel::<String>();
        let c = Client {
            base: Arc::new(base.trim_end_matches('/').to_owned()),
            token: Arc::new(token),
            inbox: Arc::new(Mutex::new(VecDeque::new())),
            ws_out: Some(tx),
            wake,
            record: None,
        };
        let me = c.clone();
        std::thread::Builder::new()
            .name("lapis-ws".into())
            .spawn(move || me.ws_loop(rx))
            .ok();
        c
    }

    /// A client that talks to nobody and records what it would have sent (tests, screenshots).
    pub fn recorder() -> Self {
        Client {
            base: Arc::new("http://127.0.0.1:0".into()),
            token: Arc::new(String::new()),
            inbox: Arc::new(Mutex::new(VecDeque::new())),
            ws_out: None,
            wake: Arc::new(|| {}),
            record: Some(Arc::new(Mutex::new(Vec::new()))),
        }
    }

    fn push(&self, item: Inbound) {
        if let Ok(mut q) = self.inbox.lock() {
            q.push_back(item);
            while q.len() > 2048 {
                q.pop_front();
            }
        }
        (self.wake)();
    }

    /// Lets tests and the offline preview inject frames as if the daemon sent them.
    pub fn inject(&self, item: Inbound) {
        self.push(item);
    }

    pub fn drain(&self) -> Vec<Inbound> {
        self.inbox
            .lock()
            .map(|mut q| q.drain(..).collect())
            .unwrap_or_default()
    }

    pub fn send_ws(&self, v: Value) {
        if let Some(r) = &self.record {
            if let Ok(mut r) = r.lock() {
                r.push(json!({"ws": v}));
            }
            return;
        }
        if let Some(tx) = &self.ws_out {
            let _ = tx.send(v.to_string());
        }
    }

    pub fn request(&self, method: &str, path: &str, body: Option<Value>, tag: Tag) {
        if let Some(r) = &self.record {
            if let Ok(mut r) = r.lock() {
                r.push(json!({"method": method, "path": path, "body": body}));
            }
            return;
        }
        let (me, method, path) = (self.clone(), method.to_owned(), path.to_owned());
        std::thread::spawn(move || {
            let (status, body, headers) = me.http(
                &method,
                &path,
                body.map(|b| b.to_string().into_bytes()),
                "application/json",
            );
            me.push(Inbound::Http {
                tag,
                status,
                body,
                headers,
            });
        });
    }

    pub fn upload_png(&self, path: &str, png: Vec<u8>) {
        if self.record.is_some() {
            return self.request("POST", path, None, Tag::Ignore);
        }
        let (me, path) = (self.clone(), path.to_owned());
        std::thread::spawn(move || {
            me.http("POST", &path, Some(png), "image/png");
        });
    }

    fn http(
        &self,
        method: &str,
        path: &str,
        body: Option<Vec<u8>>,
        content_type: &str,
    ) -> (u16, Vec<u8>, Vec<(String, String)>) {
        let url = format!("{}{path}", self.base);
        // Tool calls wait for the owner's approval: give them the gate's full timeout.
        let req = ureq::request(method, &url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .timeout(Duration::from_secs(660));
        let resp = match body {
            Some(b) => req.set("Content-Type", content_type).send_bytes(&b),
            None => req.call(),
        };
        match resp {
            Ok(r) | Err(ureq::Error::Status(_, r)) => {
                let status = r.status();
                let headers = r
                    .headers_names()
                    .into_iter()
                    .filter(|n| n.starts_with("x-lapis") || n == "content-type")
                    .filter_map(|n| r.header(&n).map(|v| (n.clone(), v.to_owned())))
                    .collect();
                let mut buf = Vec::new();
                let _ = r.into_reader().take(32 * 1024 * 1024).read_to_end(&mut buf);
                (status, buf, headers)
            }
            Err(e) => (0, e.to_string().into_bytes(), Vec::new()),
        }
    }

    fn ws_loop(&self, rx: mpsc::Receiver<String>) {
        use tungstenite::client::IntoClientRequest;
        use tungstenite::stream::MaybeTlsStream;
        use tungstenite::Message;
        let url = format!("{}/v1/ws", self.base.replacen("http", "ws", 1));
        let mut backoff = Duration::from_millis(300);
        loop {
            let mut req = match url.as_str().into_client_request() {
                Ok(r) => r,
                Err(_) => return,
            };
            if let Ok(v) = format!("Bearer {}", self.token).parse() {
                req.headers_mut().insert("Authorization", v);
            }
            if let Ok((mut ws, _)) = tungstenite::connect(req) {
                {
                    backoff = Duration::from_millis(300);
                    if let MaybeTlsStream::Plain(s) = ws.get_mut() {
                        let _ = s.set_read_timeout(Some(Duration::from_millis(30)));
                    }
                    self.push(Inbound::Connected(true));
                    'conn: loop {
                        loop {
                            match ws.read() {
                                Ok(Message::Text(t)) => {
                                    if let Ok(v) = serde_json::from_str::<Value>(t.as_str()) {
                                        self.push(Inbound::Frame(v));
                                    }
                                }
                                Ok(Message::Close(_)) => break 'conn,
                                Ok(_) => {}
                                Err(tungstenite::Error::Io(e))
                                    if matches!(
                                        e.kind(),
                                        std::io::ErrorKind::WouldBlock
                                            | std::io::ErrorKind::TimedOut
                                    ) =>
                                {
                                    break
                                }
                                Err(_) => break 'conn,
                            }
                        }
                        while let Ok(out) = rx.try_recv() {
                            if ws.send(Message::Text(out.into())).is_err() {
                                break 'conn;
                            }
                        }
                    }
                    self.push(Inbound::Connected(false));
                }
            }
            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(Duration::from_secs(5));
        }
    }
}
