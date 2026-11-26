//! Paired phones (bearer tokens, SHA-256 at rest), the device-local token for lapis-ui and
//! the harness, and single-use QR pairing nonces with a rolling lockout.

use crate::config::atomic_write;
use lapis_crypto::token;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const NONCE_TTL: Duration = Duration::from_secs(180);
pub const MAX_EXCHANGE_FAILURES: usize = 5;
const SEEN_WRITE_INTERVAL: i64 = 60;

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallerKind {
    /// lapis-ui, lapis-harness, the MCP server: holders of the local token file.
    Local,
    Phone,
}

#[derive(Debug, Clone)]
pub struct Caller {
    pub id: String,
    pub name: String,
    pub kind: CallerKind,
}

impl Caller {
    /// Who decided, as approvals and the audit log record it.
    pub fn by(&self) -> String {
        match self.kind {
            CallerKind::Local => "device".into(),
            CallerKind::Phone => format!("android:{}", self.name),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Paired {
    id: String,
    name: String,
    model: String,
    token_hash: String,
    created_at: i64,
    last_seen_at: Option<i64>,
    revoked: bool,
}

pub struct Auth {
    path: PathBuf,
    local_hash: String,
    devices: Mutex<Vec<Paired>>,
    nonces: Mutex<(HashMap<String, Instant>, Vec<Instant>)>,
}

impl Auth {
    /// Loads paired devices and creates the local token file (0640) when it is missing.
    pub fn open(devices: &Path, local_token: &Path) -> anyhow::Result<Self> {
        let list: Vec<Paired> = match std::fs::read(devices) {
            Ok(b) => serde_json::from_slice(&b)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        let local = match std::fs::read_to_string(local_token) {
            Ok(t) if t.trim().len() >= 32 => t.trim().to_owned(),
            _ => {
                let t = token::mint_token();
                atomic_write(local_token, t.as_bytes(), 0o640)?;
                t
            }
        };
        lapis_crypto::register_secret(&local);
        Ok(Auth {
            path: devices.to_owned(),
            local_hash: token::hash_token(&local),
            devices: Mutex::new(list),
            nonces: Mutex::new((HashMap::new(), Vec::new())),
        })
    }

    fn save(&self, list: &[Paired]) {
        match serde_json::to_vec_pretty(list) {
            Ok(b) => {
                if let Err(e) = atomic_write(&self.path, &b, 0o600) {
                    tracing::error!(error = %e, "cannot save paired devices");
                }
            }
            Err(e) => tracing::error!(error = %e, "cannot encode paired devices"),
        }
    }

    pub fn verify(&self, bearer: &str) -> Option<Caller> {
        if bearer.is_empty() {
            return None;
        }
        let hash = token::hash_token(bearer);
        if token::ct_eq(&hash, &self.local_hash) {
            return Some(Caller {
                id: "local".into(),
                name: "device".into(),
                kind: CallerKind::Local,
            });
        }
        let mut list = self.devices.lock().ok()?;
        let d = list
            .iter_mut()
            .find(|d| !d.revoked && token::ct_eq(&d.token_hash, &hash))?;
        let caller = Caller {
            id: d.id.clone(),
            name: d.name.clone(),
            kind: CallerKind::Phone,
        };
        let t = now();
        if t - d.last_seen_at.unwrap_or(0) > SEEN_WRITE_INTERVAL {
            // Spare the SD card: at most one write a minute per phone.
            d.last_seen_at = Some(t);
            let snapshot = list.clone();
            drop(list);
            self.save(&snapshot);
        }
        Some(caller)
    }

    /// A new paired phone: `(device_id, token)`. The token exists only in the return value.
    pub fn register(&self, name: &str, model: &str) -> (String, String) {
        let raw = token::mint_token();
        let id = format!("dev_{}", &token::hash_token(&token::mint_token())[..8]);
        let clip = |s: &str| s.chars().take(64).collect::<String>();
        let entry = Paired {
            id: id.clone(),
            name: if name.trim().is_empty() {
                "Android".into()
            } else {
                clip(name)
            },
            model: clip(model),
            token_hash: token::hash_token(&raw),
            created_at: now(),
            last_seen_at: None,
            revoked: false,
        };
        if let Ok(mut list) = self.devices.lock() {
            list.push(entry);
            let snapshot = list.clone();
            drop(list);
            self.save(&snapshot);
        }
        (id, raw)
    }

    /// Development only (`--mock-hw --dev-pair-token`): a fixed token for `adb reverse`.
    pub fn register_fixed(&self, id: &str, name: &str, raw: &str) {
        if let Ok(mut list) = self.devices.lock() {
            list.retain(|d| d.id != id);
            list.push(Paired {
                id: id.into(),
                name: name.into(),
                model: "dev".into(),
                token_hash: token::hash_token(raw),
                created_at: now(),
                last_seen_at: None,
                revoked: false,
            });
            let snapshot = list.clone();
            drop(list);
            self.save(&snapshot);
        }
    }

    pub fn devices(&self, current: &str) -> Value {
        let list = self.devices.lock().map(|l| l.clone()).unwrap_or_default();
        json!({"devices": list.iter().map(|d| json!({
            "id": d.id, "name": d.name, "model": d.model, "created_at": d.created_at,
            "last_seen_at": d.last_seen_at, "revoked": d.revoked, "current": d.id == current,
        })).collect::<Vec<_>>()})
    }

    pub fn paired_count(&self) -> usize {
        self.devices
            .lock()
            .map(|l| l.iter().filter(|d| !d.revoked).count())
            .unwrap_or(0)
    }

    pub fn revoke(&self, id: &str) -> bool {
        let Ok(mut list) = self.devices.lock() else {
            return false;
        };
        let Some(d) = list.iter_mut().find(|d| d.id == id) else {
            return false;
        };
        d.revoked = true;
        let snapshot = list.clone();
        drop(list);
        self.save(&snapshot);
        true
    }

    // -- QR pairing nonces

    fn prune(n: &mut (HashMap<String, Instant>, Vec<Instant>)) {
        let t = Instant::now();
        n.0.retain(|_, exp| *exp > t);
        n.1.retain(|f| t.duration_since(*f) < Duration::from_secs(60));
    }

    pub fn new_nonce(&self) -> String {
        let nonce = token::mint_token();
        if let Ok(mut n) = self.nonces.lock() {
            Self::prune(&mut n);
            n.0.insert(nonce.clone(), Instant::now() + NONCE_TTL);
        }
        nonce
    }

    pub fn cancel_nonces(&self) {
        if let Ok(mut n) = self.nonces.lock() {
            n.0.clear();
        }
    }

    pub fn locked_out(&self) -> bool {
        self.nonces
            .lock()
            .map(|mut n| {
                Self::prune(&mut n);
                n.1.len() >= MAX_EXCHANGE_FAILURES
            })
            .unwrap_or(true)
    }

    /// True exactly once per live nonce; a miss counts toward the lockout.
    pub fn consume_nonce(&self, nonce: &str) -> bool {
        let Ok(mut n) = self.nonces.lock() else {
            return false;
        };
        Self::prune(&mut n);
        let hit = n.0.keys().find(|k| token::ct_eq(k, nonce)).cloned();
        match hit {
            Some(k) => {
                n.0.remove(&k);
                true
            }
            None => {
                n.1.push(Instant::now());
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_nonces_and_revocation() {
        let d = tempfile::tempdir().unwrap();
        let (dev, local) = (d.path().join("devices.json"), d.path().join("local.token"));
        let auth = Auth::open(&dev, &local).unwrap();
        let local_token = std::fs::read_to_string(&local).unwrap();
        assert_eq!(auth.verify(&local_token).unwrap().kind, CallerKind::Local);
        assert!(auth.verify("").is_none());
        assert!(auth.verify("nope").is_none());

        let (id, tok) = auth.register("Pixel", "Pixel 9 Pro");
        assert!(id.starts_with("dev_"));
        let c = auth.verify(&tok).unwrap();
        assert_eq!(
            (c.id.as_str(), c.by()),
            (id.as_str(), "android:Pixel".to_string())
        );
        // Only the hash is on disk.
        let stored = std::fs::read_to_string(&dev).unwrap();
        assert!(!stored.contains(&tok) && stored.contains(&lapis_crypto::token::hash_token(&tok)));
        // Survives a restart, and the local token is reused.
        let again = Auth::open(&dev, &local).unwrap();
        assert!(again.verify(&tok).is_some());
        assert!(again.verify(&local_token).is_some());
        assert_eq!(again.devices(&id)["devices"][0]["current"], true);
        assert!(again.revoke(&id));
        assert!(again.verify(&tok).is_none());
        assert_eq!(again.paired_count(), 0);

        let n = auth.new_nonce();
        assert!(auth.consume_nonce(&n));
        assert!(!auth.consume_nonce(&n), "single use");
        for _ in 0..4 {
            auth.consume_nonce("bad");
        }
        assert!(auth.locked_out());
    }
}
