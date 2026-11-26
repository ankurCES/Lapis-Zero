//! The harness (Python agent stack) behind a Unix socket. Requests the daemon does not serve
//! itself are forwarded there after authentication, with the caller's identity in headers;
//! the harness's event stream is read back and re-broadcast (redacted) to every client.

use crate::auth::{Caller, CallerKind};
use axum::body::Body;
use axum::http::{header, HeaderValue, Method, Request, Response, StatusCode};
use futures_util::StreamExt;
use http_body_util::BodyExt;
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub const HEADER_ID: &str = "x-lapis-device-id";
pub const HEADER_NAME: &str = "x-lapis-device-name";
pub const HEADER_KIND: &str = "x-lapis-device-kind";

#[derive(Clone)]
pub struct HarnessClient {
    socket: Arc<PathBuf>,
}

fn ascii_header(s: &str) -> HeaderValue {
    let clean: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '?'
            }
        })
        .take(64)
        .collect();
    HeaderValue::from_str(&clean).unwrap_or_else(|_| HeaderValue::from_static("unknown"))
}

pub fn unavailable() -> Response<Body> {
    let body = json!({"error": {"code": "harness_unavailable", "message": "The agent harness is not running"}});
    let mut r = Response::new(Body::from(body.to_string()));
    *r.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    r
}

impl HarnessClient {
    pub fn new(socket: PathBuf) -> Self {
        HarnessClient {
            socket: Arc::new(socket),
        }
    }

    async fn send(&self, req: Request<Body>) -> Result<Response<hyper::body::Incoming>, String> {
        let stream = tokio::net::UnixStream::connect(&*self.socket)
            .await
            .map_err(|e| e.to_string())?;
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|e| e.to_string())?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        sender.send_request(req).await.map_err(|e| e.to_string())
    }

    fn identify(req: &mut Request<Body>, caller: Option<&Caller>) {
        let h = req.headers_mut();
        for name in [HEADER_ID, HEADER_NAME, HEADER_KIND] {
            h.remove(name);
        }
        h.remove(header::AUTHORIZATION);
        h.insert(header::HOST, HeaderValue::from_static("lapis-harness"));
        let (id, name, kind) = match caller {
            Some(c) => (
                c.id.as_str(),
                c.name.as_str(),
                if c.kind == CallerKind::Phone {
                    "phone"
                } else {
                    "local"
                },
            ),
            None => ("lapis-daemon", "daemon", "local"),
        };
        h.insert(HEADER_ID, ascii_header(id));
        h.insert(HEADER_NAME, ascii_header(name));
        h.insert(
            HEADER_KIND,
            HeaderValue::from_static(if kind == "phone" { "phone" } else { "local" }),
        );
    }

    /// Forwards an authenticated request as it is (streaming both ways).
    pub async fn forward(&self, mut req: Request<Body>, caller: &Caller) -> Response<Body> {
        Self::identify(&mut req, Some(caller));
        let path = req
            .uri()
            .path_and_query()
            .map(|p| p.as_str().to_owned())
            .unwrap_or_else(|| "/".into());
        match path.parse() {
            Ok(uri) => *req.uri_mut() = uri,
            Err(_) => return unavailable(),
        }
        match self.send(req).await {
            Ok(resp) => resp.map(Body::new),
            Err(e) => {
                tracing::debug!(error = %e, "harness unreachable");
                unavailable()
            }
        }
    }

    /// A JSON call of the daemon's own: `(status, body)`.
    pub async fn json(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        caller: Option<&Caller>,
    ) -> Result<(StatusCode, Value), String> {
        let method = Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
        let mut req = Request::builder()
            .method(method)
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
            .map_err(|e| e.to_string())?;
        Self::identify(&mut req, caller);
        let resp = tokio::time::timeout(Duration::from_secs(60), self.send(req))
            .await
            .map_err(|_| "harness timed out".to_string())??;
        let status = resp.status();
        let bytes = resp
            .into_body()
            .collect()
            .await
            .map_err(|e| e.to_string())?
            .to_bytes();
        Ok((
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        ))
    }

    /// Reads the harness's `/v1/ws` until it closes; `on_frame` gets every JSON frame.
    pub async fn events(&self, mut on_frame: impl FnMut(Value)) -> Result<(), String> {
        let stream = tokio::net::UnixStream::connect(&*self.socket)
            .await
            .map_err(|e| e.to_string())?;
        let request = tokio_tungstenite::tungstenite::http::Request::builder()
            .uri("ws://lapis-harness/v1/ws")
            .header("Host", "lapis-harness")
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header(
                "Sec-WebSocket-Key",
                tokio_tungstenite::tungstenite::handshake::client::generate_key(),
            )
            .header(HEADER_ID, "lapis-daemon")
            .header(HEADER_NAME, "daemon")
            .header(HEADER_KIND, "local")
            .body(())
            .map_err(|e| e.to_string())?;
        let (mut ws, _) = tokio_tungstenite::client_async(request, stream)
            .await
            .map_err(|e| e.to_string())?;
        while let Some(msg) = ws.next().await {
            match msg.map_err(|e| e.to_string())? {
                tokio_tungstenite::tungstenite::Message::Text(t) => {
                    if let Ok(v) = serde_json::from_str::<Value>(t.as_str()) {
                        on_frame(v);
                    }
                }
                tokio_tungstenite::tungstenite::Message::Close(_) => break,
                _ => {}
            }
        }
        Ok(())
    }
}
