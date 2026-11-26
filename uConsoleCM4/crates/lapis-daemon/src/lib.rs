//! Lapis-Deck control plane (`lapis-daemon`).

pub mod api;
pub mod app;
pub mod audit;
pub mod auth;
pub mod ble;
pub mod config;
pub mod deck;
pub mod events;
pub mod gps;
pub mod hw;
pub mod mesh;
pub mod net;
pub mod netops;
pub mod policy;
pub mod proxy;
pub mod tools;
pub mod ws;

use std::net::SocketAddr;
use std::sync::Arc;

/// Binds the control plane: always 127.0.0.1 (port 0 in tests), never anything wider.
pub async fn bind(port: u16) -> std::io::Result<tokio::net::TcpListener> {
    tokio::net::TcpListener::bind(SocketAddr::from((config::BIND, port))).await
}

/// Starts the background loops and serves until the listener fails.
pub async fn serve(app: Arc<app::App>, listener: tokio::net::TcpListener) -> std::io::Result<()> {
    tokio::spawn(app.clone().status_loop());
    tokio::spawn(app.clone().harness_loop());
    app.deck.start(app.clone());
    axum::serve(listener, api::router(app)).await
}

/// Log lines go through the same redaction as every event (AGENTS.md invariant 4).
pub struct RedactingStderr;

impl std::io::Write for RedactingStderr {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let line = lapis_crypto::redact(&String::from_utf8_lossy(buf));
        std::io::stderr().write_all(line.as_bytes())?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stderr().flush()
    }
}

pub fn init_logging() {
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(|| RedactingStderr)
        .with_ansi(false)
        .try_init();
    std::panic::set_hook(Box::new(|info| {
        eprintln!("panic: {}", lapis_crypto::redact(&info.to_string()));
    }));
}
