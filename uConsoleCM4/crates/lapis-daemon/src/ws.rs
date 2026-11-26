//! `/v1/ws`: a full `state_sync` first, then the live (redacted) event stream; commands from
//! phones (Lapis-Zero vocabulary) and from lapis-ui.

use crate::app::App;
use crate::auth::{Caller, CallerKind};
use crate::events::Audience;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use axum::Extension;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;

pub async fn handler(
    State(app): State<Arc<App>>,
    Extension(caller): Extension<Caller>,
    ws: WebSocketUpgrade,
) -> Response {
    ws.max_message_size(256 * 1024)
        .on_upgrade(move |socket| client(app, caller, socket))
}

fn redacted(mut v: Value) -> Message {
    lapis_crypto::redact_json(&mut v);
    Message::Text(v.to_string().into())
}

async fn client(app: Arc<App>, caller: Caller, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let mut rx = app.hub.subscribe();
    if sink.send(redacted(app.state())).await.is_err() {
        return;
    }
    let local = caller.kind == CallerKind::Local;
    let (mut fast, mut spectrum) = (false, false);
    let mut heartbeat = tokio::time::interval(Duration::from_secs(20));
    loop {
        tokio::select! {
            frame = rx.recv() => match frame {
                Ok(f) => {
                    let wanted = match f.audience {
                        Audience::All => true,
                        Audience::Local => local,
                        Audience::TelemetryFast => fast,
                        Audience::Spectrum => spectrum,
                    };
                    if wanted && sink.send(Message::Text(f.text.to_string().into())).await.is_err() {
                        break;
                    }
                }
                // A stalled client must not grow memory on the device: drop it, it reconnects.
                Err(RecvError::Lagged(_)) | Err(RecvError::Closed) => break,
            },
            msg = stream.next() => match msg {
                Some(Ok(Message::Text(t))) => {
                    let reply = match serde_json::from_str::<Value>(t.as_str()) {
                        Ok(cmd) => command(&app, &caller, &cmd, &mut fast, &mut spectrum).await,
                        Err(_) => Some(json!({"type": "toast", "level": "error", "text": "Unrecognized command"})),
                    };
                    if let Some(r) = reply {
                        if sink.send(redacted(r)).await.is_err() {
                            break;
                        }
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
            _ = heartbeat.tick() => {
                if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
        }
    }
}

async fn command(
    app: &Arc<App>,
    caller: &Caller,
    cmd: &Value,
    fast: &mut bool,
    spectrum: &mut bool,
) -> Option<Value> {
    let by = caller.by();
    let s = |k: &str| cmd[k].as_str().unwrap_or("").to_owned();
    match cmd["type"].as_str().unwrap_or("") {
        "ping" => Some(json!({"type": "pong", "ts": crate::auth::now()})),
        "subscribe_telemetry" => {
            *fast = cmd["enabled"].as_bool().unwrap_or(false);
            None
        }
        "subscribe_spectrum" => {
            *spectrum = cmd["enabled"].as_bool().unwrap_or(false);
            None
        }
        "send_prompt" => {
            let (chat, content) = (s("chat_id"), s("content"));
            if chat.is_empty()
                || content.is_empty()
                || !chat
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                return Some(
                    json!({"type": "toast", "level": "error", "text": "send_prompt needs chat_id and content"}),
                );
            }
            match app
                .harness
                .json(
                    "POST",
                    &format!("/v1/chats/{chat}/messages"),
                    Some(json!({"content": content})),
                    Some(caller),
                )
                .await
            {
                Ok((st, _)) if st.is_success() => None,
                Ok((_, body)) => Some(
                    json!({"type": "toast", "level": "error", "text": body["error"]["message"].as_str().unwrap_or("Could not send")}),
                ),
                Err(_) => Some(
                    json!({"type": "toast", "level": "error", "text": "The agent harness is not running"}),
                ),
            }
        }
        "approval_decision" => {
            let ok = app
                .gate
                .decide(&s("request_id"), &s("decision"), &by, &s("reason"));
            (!ok).then(|| json!({"type": "toast", "level": "warn", "text": "No pending approval with that id"}))
        }
        "kill_all" => {
            app.kill_all(&by).await;
            None
        }
        _ => Some(json!({"type": "toast", "level": "error", "text": "Unrecognized command"})),
    }
}
