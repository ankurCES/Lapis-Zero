//! The fan-out every WebSocket client reads from. Every frame is redacted here, once, before
//! it is serialised (AGENTS.md invariant 4): nothing reaches a socket any other way.

use serde_json::Value;
use std::sync::Arc;
use tokio::sync::broadcast;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    All,
    /// lapis-ui only (screen requests, UI settings): never sent to phones.
    Local,
    /// Telemetry ticks for clients that subscribed to the fast rate.
    TelemetryFast,
    /// SDR spectrum rows for clients that subscribed.
    Spectrum,
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub text: Arc<str>,
    pub audience: Audience,
    pub kind: Arc<str>,
}

#[derive(Clone)]
pub struct Hub {
    tx: broadcast::Sender<Frame>,
}

impl Default for Hub {
    fn default() -> Self {
        Hub {
            tx: broadcast::channel(512).0,
        }
    }
}

impl Hub {
    pub fn subscribe(&self) -> broadcast::Receiver<Frame> {
        self.tx.subscribe()
    }

    pub fn send(&self, value: Value) {
        self.send_to(value, Audience::All);
    }

    pub fn send_to(&self, mut value: Value, audience: Audience) {
        lapis_crypto::redact_json(&mut value);
        let kind: Arc<str> = value["type"].as_str().unwrap_or("").into();
        // No receivers is normal (nobody connected): nothing to do.
        let _ = self.tx.send(Frame {
            text: value.to_string().into(),
            audience,
            kind,
        });
    }

    pub fn toast(&self, level: &str, text: &str) {
        self.send(serde_json::json!({"type": "toast", "level": level, "text": text}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_redacted() {
        let hub = Hub::default();
        let mut rx = hub.subscribe();
        hub.send(serde_json::json!({"type": "toast", "text": "key sk-ant-api03-abcdefghijklmnopqr", "api_token": "abcdefghijkl"}));
        let f = rx.try_recv().unwrap();
        assert_eq!(&*f.kind, "toast");
        assert!(!f.text.contains("sk-ant") && !f.text.contains("abcdefghijkl"));
        assert!(f.text.contains("[REDACTED]"));
    }
}
