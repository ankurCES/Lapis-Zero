//! Append-only JSONL audit log of every gated decision and owner action, redacted.

use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

pub struct Audit {
    path: PathBuf,
    lock: Mutex<()>,
}

/// Rotates at this size (one previous file kept), so the SD card never fills.
const MAX_BYTES: u64 = 4 * 1024 * 1024;

impl Audit {
    pub fn new(path: PathBuf) -> Self {
        Audit {
            path,
            lock: Mutex::new(()),
        }
    }

    pub fn record(&self, event: &str, mut fields: Value) {
        lapis_crypto::redact_json(&mut fields);
        let line = json!({"ts": crate::auth::now(), "event": event, "data": fields}).to_string();
        let Ok(_guard) = self.lock.lock() else { return };
        if std::fs::metadata(&self.path)
            .map(|m| m.len() > MAX_BYTES)
            .unwrap_or(false)
        {
            let _ = std::fs::rename(&self.path, self.path.with_extension("jsonl.1"));
        }
        let res = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .and_then(|mut f| writeln!(f, "{line}"));
        if let Err(e) = res {
            tracing::error!(error = %e, "audit log write failed");
        }
    }

    /// The newest `limit` entries, newest first.
    pub fn tail(&self, limit: usize) -> Vec<Value> {
        let text = std::fs::read_to_string(&self.path).unwrap_or_default();
        text.lines()
            .rev()
            .take(limit)
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_and_tail() {
        let d = tempfile::tempdir().unwrap();
        let a = Audit::new(d.path().join("audit.jsonl"));
        a.record("rail", json!({"rail": "gps", "on": false}));
        a.record(
            "approval",
            json!({"summary": "export TS_AUTHKEY=tskey-auth-abcdef123456"}),
        );
        let t = a.tail(10);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0]["event"], "approval");
        assert!(!t[0].to_string().contains("tskey-auth"));
        assert_eq!(t[1]["data"]["rail"], "gps");
    }
}
