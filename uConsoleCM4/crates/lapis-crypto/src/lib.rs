//! Lapis-Deck crypto: the `lapis-ble-v1` provisioning handshake shared with the Android
//! companion, bearer tokens, and the one redaction path every log line and event takes.

pub mod ble;
pub mod framing;
pub mod redact;
pub mod token;

pub use redact::{redact, redact_json, register_secret};
