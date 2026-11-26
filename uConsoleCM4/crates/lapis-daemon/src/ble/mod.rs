//! BLE + QR first-boot provisioning (SPEC 6.2), `lapis-ble-v1`, identical to Lapis-Zero.

#[cfg(all(target_os = "linux", feature = "bluez"))]
pub mod bluez;
pub mod session;

use crate::auth::Auth;
use crate::net::{NetControl, NetStatus};
use crate::proxy::HarnessClient;
use serde_json::{json, Map, Value};
use session::{Backend, BoxFut};
use std::sync::Arc;

pub const SERVICE_UUID: &str = "7c4f0001-9a3b-4c6e-8e21-51a0c0de0001";
pub const HANDSHAKE_UUID: &str = "7c4f0001-9a3b-4c6e-8e21-51a0c0de0002";
pub const CMD_TX_UUID: &str = "7c4f0001-9a3b-4c6e-8e21-51a0c0de0003";
pub const EVT_RX_UUID: &str = "7c4f0001-9a3b-4c6e-8e21-51a0c0de0004";
/// The app finds devices by service UUID; the name only labels them in its list.
pub const NAME_PREFIX: &str = "LapisDeck-";
/// Advertising window, and how long a verified phone then has to finish.
pub const WINDOW_S: u64 = 300;
pub const FINISH_S: u64 = 600;

pub type StatusFn = Arc<dyn Fn(&str, &str) + Send + Sync>;

/// The real device behind a provisioning session.
pub struct DeviceBackend {
    pub auth: Arc<Auth>,
    pub net: Arc<dyn NetControl>,
    pub harness: HarnessClient,
    pub on_status: StatusFn,
}

impl Backend for DeviceBackend {
    fn register_device(&self, name: &str, model: &str) -> (String, String) {
        let (id, token) = self.auth.register(name, model);
        lapis_crypto::register_secret(&token);
        (id, token)
    }

    fn wifi_scan(&self) -> BoxFut<'_, Result<Vec<Value>, String>> {
        let net = self.net.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || net.wifi_scan())
                .await
                .map_err(|e| e.to_string())?
        })
    }

    fn wifi_connect(&self, ssid: String, psk: Option<String>) -> BoxFut<'_, Value> {
        let net = self.net.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || net.wifi_connect(&ssid, psk.as_deref()))
                .await
                .unwrap_or_else(|_| json!({"state": "failed", "local_ip": ""}))
        })
    }

    fn tailscale_status(&self) -> BoxFut<'_, NetStatus> {
        let net = self.net.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || net.tailscale_status())
                .await
                .unwrap_or_default()
        })
    }

    fn tailscale_up(&self, authkey: Option<String>) {
        if let Some(k) = &authkey {
            lapis_crypto::register_secret(k);
        }
        let net = self.net.clone();
        tokio::task::spawn_blocking(move || net.tailscale_up(authkey.as_deref()));
    }

    fn tailscale_up_error(&self) -> Option<String> {
        self.net.tailscale_up_error()
    }

    fn tailscale_serve(&self) -> BoxFut<'_, bool> {
        let net = self.net.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || net.tailscale_serve())
                .await
                .unwrap_or(false)
        })
    }

    fn push_secrets(&self, values: Map<String, Value>) -> BoxFut<'_, Result<Vec<String>, String>> {
        Box::pin(async move {
            let keys: Vec<String> = values.keys().cloned().collect();
            // The harness owns secrets.env (it runs the LLM providers that read it).
            let (status, body) = self
                .harness
                .json(
                    "PATCH",
                    "/v1/secrets",
                    Some(json!({"set": values, "unset": []})),
                    None,
                )
                .await?;
            if status.is_success() {
                Ok(keys)
            } else {
                Err(body["error"]["message"]
                    .as_str()
                    .unwrap_or("could not save secrets")
                    .to_owned())
            }
        })
    }

    fn status(&self, status: &str, pin: &str) {
        (self.on_status)(status, pin);
    }
}
