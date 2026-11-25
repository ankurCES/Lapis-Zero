//! Non-blocking Unix Domain Socket NDJSON IPC client and daemon state (SPEC 6).
//!
//! `lapis-ui` never blocks its render loop on socket I/O. Connection attempts are
//! rate-limited with exponential backoff (`200ms..=2000ms`), reads drain whatever
//! bytes are currently buffered in the kernel, writes queue what the kernel will
//! not take yet, and disconnects reset cleanly.

use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Default IPC Unix domain socket path for the current OS target.
#[cfg(target_os = "linux")]
pub const DEFAULT_SOCKET_PATH: &str = "/run/lapis/lapisd.sock";

/// Default IPC Unix domain socket path on non-Linux dev hosts (e.g. macOS).
#[cfg(not(target_os = "linux"))]
pub const DEFAULT_SOCKET_PATH: &str = "/tmp/lapis/lapisd.sock";

/// Initial reconnect backoff (200 ms, SPEC 6).
const MIN_BACKOFF_SECS: f32 = 0.2;
/// Maximum reconnect backoff (2000 ms, SPEC 6).
const MAX_BACKOFF_SECS: f32 = 2.0;
/// Hard ceiling on buffered unread line bytes to protect the 512 MB RAM budget.
const MAX_BUF_BYTES: usize = 1024 * 1024;

/// Active task metadata inside `state_sync` (SPEC 6.1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveTask {
    pub id: String,
    pub title: String,
    pub step: i64,
    pub max_steps: i64,
}

/// Snapshot of daemon-owned state synced via `state_sync` NDJSON frames (SPEC 6.1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonState {
    pub agent_state: String,
    pub active_host: String,
    pub hosts: Vec<String>,
    pub active_task: Option<ActiveTask>,
    pub next_cron: String,
    pub wifi_ssid: String,
    pub local_ip: String,
    pub tailscale_state: String,
    pub tailscale_ip: String,
    pub tailscale_auth_url: Option<String>,
    pub api_key_ready: bool,
    /// `state_sync.action`: the activity kind of the step being executed
    /// right now (`screenshot`, `vnc_click`, `ssh_exec`, ...), `Some("")`
    /// when no step is, and `None` when the daemon does not say (one from
    /// before the field existed). Unlike the fields above this is never
    /// carried over from an earlier frame: see [`DaemonState::apply_state_sync`].
    #[serde(skip)]
    pub action: Option<String>,
}

impl Default for DaemonState {
    fn default() -> Self {
        Self {
            agent_state: "IDLE".to_string(),
            active_host: "macbook-pro".to_string(),
            hosts: vec!["macbook-pro".to_string(), "arch-dev".to_string()],
            active_task: None,
            next_cron: String::new(),
            wifi_ssid: String::new(),
            local_ip: String::new(),
            tailscale_state: "connected".to_string(),
            tailscale_ip: "100.91.12.4".to_string(),
            tailscale_auth_url: None,
            api_key_ready: true,
            action: None,
        }
    }
}

impl DaemonState {
    /// Applies a `state_sync` JSON payload onto `self`.
    ///
    /// Present keys overwrite, missing keys keep their current value, and a
    /// malformed payload (wrong field type) leaves `self` untouched.
    ///
    /// `action` is the exception: each frame speaks for itself. A frame
    /// without it (or with something that is not a string there) comes from a
    /// daemon that does not report the running step, and says so by leaving
    /// `action` at `None`, whatever an earlier frame carried. It is read
    /// leniently so that an odd value cannot void the rest of the sync.
    pub fn apply_state_sync(&mut self, value: &Value) {
        let (Ok(Value::Object(mut cur)), Some(obj)) =
            (serde_json::to_value(&*self), value.as_object())
        else {
            return;
        };
        for (k, v) in obj {
            cur.insert(k.clone(), v.clone());
        }
        if let Ok(next) = serde_json::from_value(Value::Object(cur)) {
            *self = next;
            self.action = obj
                .get("action")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
    }
}

/// The `status` of a `task_finished` frame (`done`, `failed`, `stopped` or
/// `cancelled`; passed on as sent, so the caller decides what an unknown one
/// means). `None` for any other frame, and for one without a string status.
pub fn task_finished_status(frame: &Value) -> Option<&str> {
    if frame["type"].as_str() != Some("task_finished") {
        return None;
    }
    frame["status"].as_str()
}

/// Resolves the socket path: an explicit `--socket` value or the OS default.
pub fn selected_socket_path(opt: Option<&str>) -> String {
    opt.unwrap_or(DEFAULT_SOCKET_PATH).to_string()
}

/// Builds `{"type":"request_sync"}` (SPEC 6.2).
pub fn msg_request_sync() -> Value {
    json!({ "type": "request_sync" })
}

/// Builds `{"type":"submit_task","host":...,"prompt":...}` (SPEC 6.2).
pub fn msg_submit_task(host: &str, prompt: &str) -> Value {
    json!({
        "type": "submit_task",
        "host": host,
        "prompt": prompt,
    })
}

/// Builds `{"type":"approval_decision","request_id":...,"decision":...}` (SPEC 6.2).
///
/// Allowed `decision` values: `"approve_once"`, `"approve_session"`, `"deny"`.
pub fn msg_approval_decision(request_id: &str, decision: &str) -> Value {
    json!({
        "type": "approval_decision",
        "request_id": request_id,
        "decision": decision,
    })
}

/// Builds `{"type":"select_host","host":...}` (SPEC 6.2).
pub fn msg_select_host(host: &str) -> Value {
    json!({
        "type": "select_host",
        "host": host,
    })
}

/// Builds `{"type":"agent_control","command":...}` (SPEC 6.2).
///
/// Allowed `command` values: `"kill_all"`, `"pause"`, `"resume"`.
pub fn msg_agent_control(command: &str) -> Value {
    json!({
        "type": "agent_control",
        "command": command,
    })
}

/// Builds `{"type":"system_control","command":...}` (SPEC 6.2).
///
/// Allowed `command` values: `"retry_tailscale"`, `"reboot"`, `"shutdown"`.
pub fn msg_system_control(command: &str) -> Value {
    json!({
        "type": "system_control",
        "command": command,
    })
}

/// Builds `{"type":"ptt_audio","state":...}` (SPEC 6.2).
///
/// Allowed `state` values: `"start"`, `"stop"`.
pub fn msg_ptt_audio(state: &str) -> Value {
    json!({
        "type": "ptt_audio",
        "state": state,
    })
}

/// Builds `{"type":"pairing_control","command":...}` (SPEC 6.3).
///
/// Allowed `command` values: `"start"`, `"stop"`.
pub fn msg_pairing_control(command: &str) -> Value {
    json!({
        "type": "pairing_control",
        "command": command,
    })
}

/// Builds `{"type":"screen_snapshot","id":...,"png_b64":...}` (SPEC 6.3), the
/// reply to a daemon `screen_request` with the same `id`.
pub fn msg_screen_snapshot(id: &str, png_b64: &str) -> Value {
    json!({
        "type": "screen_snapshot",
        "id": id,
        "png_b64": png_b64,
    })
}

/// Non-blocking Unix socket client speaking NDJSON with exponential reconnect backoff.
pub struct IpcClient {
    path: PathBuf,
    stream: Option<UnixStream>,
    buf: Vec<u8>,
    /// Outgoing bytes the kernel has not accepted yet (a `screen_snapshot` line
    /// can exceed the socket buffer, 8 kB on macOS); flushed by `send` and `poll`.
    out: Vec<u8>,
    backoff: f32,
    last_attempt: Option<f32>,
}

impl IpcClient {
    /// Creates a disconnected client targeting `path` (does not connect yet).
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            stream: None,
            buf: Vec::new(),
            out: Vec::new(),
            backoff: MIN_BACKOFF_SECS,
            last_attempt: None,
        }
    }

    /// Returns `true` if a socket connection is currently open.
    /// The socket's file descriptor while connected (for `poll(2)` in the fb loop).
    #[cfg(unix)]
    pub fn raw_fd(&self) -> Option<std::os::fd::RawFd> {
        use std::os::fd::AsRawFd;
        self.stream.as_ref().map(|s| s.as_raw_fd())
    }

    pub fn is_connected(&self) -> bool {
        self.stream.is_some()
    }

    /// Returns the configured Unix domain socket path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Ensures the client is connected when the reconnect backoff timer allows.
    ///
    /// - If already connected, returns `Ok(())`.
    /// - If disconnected and the backoff window has not elapsed, returns `Ok(())`.
    /// - Otherwise attempts `UnixStream::connect`:
    ///   - On success, sets non-blocking mode, resets backoff, and sends
    ///     `{"type":"request_sync"}`. Returns `Err` only if that initial send fails.
    ///   - On connect failure, schedules the next attempt with doubled backoff
    ///     capped at 2000ms and returns `Ok(())`.
    pub fn connect_ready(&mut self, now: f32) -> Result<(), String> {
        if self.stream.is_some() {
            return Ok(());
        }
        if let Some(last) = self.last_attempt {
            if (now - last) < self.backoff {
                return Ok(());
            }
        }

        match UnixStream::connect(&self.path) {
            Ok(stream) => {
                if let Err(e) = stream.set_nonblocking(true) {
                    self.record_failed_attempt(now);
                    return Err(format!("failed to set nonblocking: {e}"));
                }
                self.stream = Some(stream);
                self.buf.clear();
                self.out.clear();
                self.backoff = MIN_BACKOFF_SECS;
                self.last_attempt = None;
                if let Err(e) = self.send(&msg_request_sync()) {
                    // `send` already dropped the stream; arm backoff so we do not busy-loop.
                    self.backoff = MIN_BACKOFF_SECS;
                    self.last_attempt = Some(now);
                    return Err(e);
                }
                Ok(())
            }
            Err(_) => {
                self.record_failed_attempt(now);
                Ok(())
            }
        }
    }

    /// Non-blocking drain of all complete NDJSON lines currently available.
    pub fn poll<F>(&mut self, now: f32, mut on_msg: F)
    where
        F: FnMut(Value),
    {
        // Push out whatever an earlier `send` could not hand to the kernel.
        if self.flush().is_err() {
            self.backoff = MIN_BACKOFF_SECS;
            self.last_attempt = Some(now);
            return;
        }
        let mut disconnected = false;
        if let Some(stream) = self.stream.as_mut() {
            let mut tmp = [0u8; 4096];
            loop {
                match stream.read(&mut tmp) {
                    Ok(0) => {
                        disconnected = true;
                        break;
                    }
                    Ok(n) => {
                        if self.buf.len() + n > MAX_BUF_BYTES {
                            // Protect RAM budget on malformed runaway stream.
                            disconnected = true;
                            break;
                        }
                        self.buf.extend_from_slice(&tmp[..n]);
                    }
                    Err(ref e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(_) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        } else {
            return;
        }

        // Dispatch every complete newline-delimited JSON frame.
        let mut start = 0;
        while let Some(rel) = self.buf[start..].iter().position(|&b| b == b'\n') {
            let end = start + rel;
            let mut line = &self.buf[start..end];
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            if !line.is_empty() {
                if let Ok(val) = serde_json::from_slice::<Value>(line) {
                    on_msg(val);
                }
            }
            start = end + 1;
        }
        if start > 0 {
            self.buf.drain(0..start);
        }

        if disconnected {
            self.drop_stream();
            self.backoff = MIN_BACKOFF_SECS;
            self.last_attempt = Some(now);
        }
    }

    /// Serializes `value` as a single NDJSON line and writes it to the socket.
    ///
    /// Never blocks: bytes the socket does not take right now stay queued and go
    /// out on later `send`/`poll` calls, in order. Returns `Ok(())` when
    /// disconnected (best-effort UI transport). On write failure, or if the
    /// daemon stops reading and the queue outgrows its cap, drops the stream.
    pub fn send(&mut self, value: &Value) -> Result<(), String> {
        if self.stream.is_none() {
            return Ok(());
        }
        let mut line = serde_json::to_vec(value)
            .map_err(|e| format!("failed to serialize ipc message: {e}"))?;
        line.push(b'\n');
        if self.out.len() + line.len() > MAX_BUF_BYTES {
            self.drop_stream();
            return Err("ipc write failed: daemon is not reading".to_string());
        }
        self.out.extend_from_slice(&line);
        self.flush()
    }

    /// Writes as much of the outgoing queue as the socket accepts without blocking.
    fn flush(&mut self) -> Result<(), String> {
        let Some(stream) = self.stream.as_mut() else {
            return Ok(());
        };
        let mut sent = 0;
        let result = loop {
            if sent == self.out.len() {
                break Ok(());
            }
            match stream.write(&self.out[sent..]) {
                Ok(0) => break Err("ipc write failed: socket closed".to_string()),
                Ok(n) => sent += n,
                Err(ref e) if e.kind() == ErrorKind::WouldBlock => break Ok(()),
                Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => break Err(format!("ipc write failed: {e}")),
            }
        };
        self.out.drain(..sent);
        if result.is_err() {
            self.drop_stream();
        }
        result
    }

    /// Forgets the connection and everything buffered for it.
    fn drop_stream(&mut self) {
        self.stream = None;
        self.buf.clear();
        self.out.clear();
    }

    fn record_failed_attempt(&mut self, now: f32) {
        if self.last_attempt.is_some() {
            self.backoff = (self.backoff * 2.0).min(MAX_BACKOFF_SECS);
        }
        self.last_attempt = Some(now);
    }

    /// Connection flag for unit tests.
    #[cfg(test)]
    pub fn test_connected(&self) -> bool {
        self.stream.is_some()
    }

    /// Exposes `(connected, backoff, last_attempt)` for deterministic unit tests.
    #[cfg(test)]
    pub fn test_state(&self) -> (bool, f32, Option<f32>) {
        (self.stream.is_some(), self.backoff, self.last_attempt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixListener;

    #[test]
    fn daemon_state_apply_state_sync_and_nulls() {
        let mut ds = DaemonState::default();
        assert_eq!(ds.agent_state, "IDLE");
        assert_eq!(ds.active_host, "macbook-pro");
        assert_eq!(ds.hosts, vec!["macbook-pro", "arch-dev"]);
        assert_eq!(ds.tailscale_state, "connected");
        assert_eq!(ds.tailscale_ip, "100.91.12.4");
        assert!(ds.api_key_ready);

        // Full payload with active task and auth URL.
        let full = json!({
            "type": "state_sync",
            "agent_state": "ACTING",
            "active_host": "arch-dev",
            "hosts": ["macbook-pro", "arch-dev", "lab-box"],
            "active_task": {
                "id": "t_01",
                "title": "Check CI logs and restart worker",
                "step": 4,
                "max_steps": 25
            },
            "next_cron": "08:00 Daily Digest",
            "wifi_ssid": "HomeWiFi",
            "local_ip": "192.168.1.42",
            "tailscale_state": "needs_login",
            "tailscale_ip": "",
            "tailscale_auth_url": "https://login.tailscale.com/a/abc123",
            "api_key_ready": false
        });
        ds.apply_state_sync(&full);
        assert_eq!(ds.agent_state, "ACTING");
        assert_eq!(ds.active_host, "arch-dev");
        assert_eq!(ds.hosts.len(), 3);
        let task = ds.active_task.as_ref().expect("active_task present");
        assert_eq!(task.id, "t_01");
        assert_eq!(task.step, 4);
        assert_eq!(task.max_steps, 25);
        assert_eq!(ds.next_cron, "08:00 Daily Digest");
        assert_eq!(ds.wifi_ssid, "HomeWiFi");
        assert_eq!(ds.local_ip, "192.168.1.42");
        assert_eq!(ds.tailscale_state, "needs_login");
        assert_eq!(
            ds.tailscale_auth_url.as_deref(),
            Some("https://login.tailscale.com/a/abc123")
        );
        assert!(!ds.api_key_ready);

        // Null active_task and null tailscale_auth_url clear both Option fields.
        let nulls = json!({
            "type": "state_sync",
            "agent_state": "IDLE",
            "active_host": "macbook-pro",
            "hosts": ["macbook-pro"],
            "active_task": null,
            "next_cron": "",
            "wifi_ssid": "HomeWiFi",
            "local_ip": "192.168.1.42",
            "tailscale_state": "connected",
            "tailscale_ip": "100.91.12.4",
            "tailscale_auth_url": null,
            "api_key_ready": true
        });
        ds.apply_state_sync(&nulls);
        assert!(ds.active_task.is_none());
        assert!(ds.tailscale_auth_url.is_none());
        assert_eq!(ds.agent_state, "IDLE");
        assert!(ds.api_key_ready);

        // Missing keys keep their value; a malformed payload changes nothing.
        ds.apply_state_sync(&json!({"type": "state_sync", "agent_state": "THINKING"}));
        assert_eq!(ds.agent_state, "THINKING");
        assert_eq!(ds.wifi_ssid, "HomeWiFi");
        ds.apply_state_sync(&json!({"agent_state": "ACTING", "hosts": "oops"}));
        assert_eq!(ds.agent_state, "THINKING");
        assert_eq!(ds.hosts, vec!["macbook-pro"]);
    }

    #[test]
    fn state_sync_action_is_read_frame_by_frame() {
        let sync = |extra: Value| {
            let mut frame = json!({"type": "state_sync", "agent_state": "ACTING"});
            for (k, v) in extra.as_object().unwrap() {
                frame[k] = v.clone();
            }
            frame
        };
        let mut ds = DaemonState::default();
        assert_eq!(ds.action, None, "nothing is known before the first sync");

        // Every kind the daemon names, and the empty string for "no step".
        for kind in [
            "screenshot",
            "vnc_click",
            "vnc_move",
            "vnc_drag",
            "vnc_scroll",
            "type_text",
            "vnc_key",
            "ssh_exec",
            "agent_run",
            "wait",
            "a_kind_from_the_future",
            "",
        ] {
            ds.apply_state_sync(&sync(json!({"action": kind})));
            assert_eq!(ds.action.as_deref(), Some(kind));
        }

        // A frame without the field is a daemon that does not report steps:
        // nothing is carried over from the frame before, while the fields
        // that are merged keep their values.
        ds.apply_state_sync(&sync(
            json!({"action": "ssh_exec", "wifi_ssid": "HomeWiFi"}),
        ));
        ds.apply_state_sync(&json!({"type": "state_sync", "agent_state": "THINKING"}));
        assert_eq!(ds.action, None);
        assert_eq!(
            (ds.agent_state.as_str(), ds.wifi_ssid.as_str()),
            ("THINKING", "HomeWiFi")
        );

        // Not a string: read as "not reported", and the rest of the frame still counts.
        for odd in [
            json!(null),
            json!(7),
            json!(["ssh_exec"]),
            json!({"kind": "wait"}),
        ] {
            ds.apply_state_sync(&sync(json!({"action": "wait"})));
            ds.apply_state_sync(&sync(json!({"action": odd, "next_cron": "09:00 Sweep"})));
            assert_eq!(ds.action, None, "{odd}");
            assert_eq!(ds.next_cron, "09:00 Sweep");
            ds.next_cron.clear();
        }

        // A frame that is malformed elsewhere changes nothing, `action` included.
        ds.apply_state_sync(&sync(json!({"action": "vnc_key"})));
        ds.apply_state_sync(&sync(json!({"action": "wait", "hosts": "oops"})));
        assert_eq!(ds.action.as_deref(), Some("vnc_key"));
        ds.apply_state_sync(&json!("not an object"));
        assert_eq!(ds.action.as_deref(), Some("vnc_key"));

        // The field is this side's reading of a frame, not part of the state
        // that is merged: it never leaks into the next merge.
        let echoed = serde_json::to_value(&ds).unwrap();
        assert!(echoed.get("action").is_none());
        // The full frame of SPEC 6.1 with the field added.
        ds.apply_state_sync(&json!({
            "type": "state_sync", "agent_state": "ACTING", "action": "screenshot",
            "active_host": "arch-dev", "hosts": ["arch-dev"],
            "active_task": {"id": "t_01", "title": "T", "step": 4, "max_steps": 25},
            "next_cron": "", "wifi_ssid": "HomeWiFi", "local_ip": "192.168.1.42",
            "tailscale_state": "connected", "tailscale_ip": "100.91.12.4",
            "tailscale_auth_url": null, "api_key_ready": true,
        }));
        assert_eq!(ds.action.as_deref(), Some("screenshot"));
        assert_eq!(ds.active_task.as_ref().map(|t| t.step), Some(4));
    }

    #[test]
    fn task_finished_status_is_read_only_from_its_own_frame() {
        for status in ["done", "failed", "stopped", "cancelled", "something_new"] {
            let frame = json!({
                "type": "task_finished", "task_id": "t_01", "chat_id": null,
                "host": "macbook-pro", "status": status, "summary": "Done.",
            });
            assert_eq!(task_finished_status(&frame), Some(status));
        }
        // No status, or one that is not a string: nothing to go on.
        assert_eq!(
            task_finished_status(&json!({"type": "task_finished"})),
            None
        );
        for odd in [json!(null), json!(1), json!(["done"])] {
            let frame = json!({"type": "task_finished", "status": odd});
            assert_eq!(task_finished_status(&frame), None);
        }
        // Other frames have a `status` of their own that means something else.
        let event = json!({"type": "activity_event", "kind": "vnc_click", "status": "done"});
        assert_eq!(task_finished_status(&event), None);
        assert_eq!(task_finished_status(&json!({"status": "done"})), None);
        assert_eq!(task_finished_status(&json!("task_finished")), None);
    }

    #[test]
    fn outbound_messages_serialize_cleanly() {
        let submit = msg_submit_task("macbook-pro", "Open terminal and check disk usage");
        let parsed: Value = serde_json::from_str(&submit.to_string()).unwrap();
        assert_eq!(parsed["type"], "submit_task");
        assert_eq!(parsed["host"], "macbook-pro");
        assert_eq!(parsed["prompt"], "Open terminal and check disk usage");

        let decision = msg_approval_decision("req_99", "approve_once");
        let parsed: Value = serde_json::from_str(&decision.to_string()).unwrap();
        assert_eq!(parsed["type"], "approval_decision");
        assert_eq!(parsed["request_id"], "req_99");
        assert_eq!(parsed["decision"], "approve_once");

        let kill = msg_agent_control("kill_all");
        let parsed: Value = serde_json::from_str(&kill.to_string()).unwrap();
        assert_eq!(parsed["type"], "agent_control");
        assert_eq!(parsed["command"], "kill_all");

        let host = msg_select_host("arch-dev");
        assert_eq!(host["type"], "select_host");
        assert_eq!(host["host"], "arch-dev");

        let sys = msg_system_control("retry_tailscale");
        assert_eq!(sys["type"], "system_control");
        assert_eq!(sys["command"], "retry_tailscale");

        let ptt = msg_ptt_audio("start");
        assert_eq!(ptt["type"], "ptt_audio");
        assert_eq!(ptt["state"], "start");

        assert_eq!(
            msg_pairing_control("start"),
            json!({"type": "pairing_control", "command": "start"})
        );
        assert_eq!(
            msg_screen_snapshot("a1b2c3d4", "iVBORw0KGgo="),
            json!({"type": "screen_snapshot", "id": "a1b2c3d4", "png_b64": "iVBORw0KGgo="})
        );
    }

    #[test]
    fn reconnect_backoff_state_machine() {
        let mut client = IpcClient::new("/tmp/lapis-nonexistent-test-socket-0000.sock");
        assert_eq!(client.test_state(), (false, 0.2, None));
        assert!(!client.test_connected());
        assert_eq!(selected_socket_path(None), DEFAULT_SOCKET_PATH);
        assert_eq!(selected_socket_path(Some("/tmp/x.sock")), "/tmp/x.sock");

        // First attempt at t=0.0 fails -> arms 0.2s wait.
        assert!(client.connect_ready(0.0).is_ok());
        assert_eq!(client.test_state(), (false, 0.2, Some(0.0)));

        // Within backoff window -> no retry attempted, state unchanged.
        assert!(client.connect_ready(0.1).is_ok());
        assert_eq!(client.test_state(), (false, 0.2, Some(0.0)));

        // At t=0.2 -> retries, fails, doubles backoff to 0.4s.
        assert!(client.connect_ready(0.2).is_ok());
        let (conn, backoff, last) = client.test_state();
        assert!(!conn);
        assert!((backoff - 0.4).abs() < 1e-6);
        assert_eq!(last, Some(0.2));

        // Within 0.4s window -> skipped.
        assert!(client.connect_ready(0.55).is_ok());
        assert_eq!(client.test_state().2, Some(0.2));

        // Escalate through 0.8 -> 1.6 -> 2.0 -> 2.0 (cap).
        assert!(client.connect_ready(0.6).is_ok());
        assert!((client.test_state().1 - 0.8).abs() < 1e-6);

        assert!(client.connect_ready(1.5).is_ok());
        assert!((client.test_state().1 - 1.6).abs() < 1e-6);

        assert!(client.connect_ready(3.2).is_ok());
        assert!((client.test_state().1 - 2.0).abs() < 1e-6);

        assert!(client.connect_ready(5.5).is_ok());
        assert!((client.test_state().1 - 2.0).abs() < 1e-6);
    }

    #[test]
    fn live_socket_round_trip_and_disconnect() {
        let sock_path =
            std::env::temp_dir().join(format!("lapis_ui_ipc_test_{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock_path);
        let listener = UnixListener::bind(&sock_path).expect("bind unix socket");

        let mut client = IpcClient::new(&sock_path);
        client.connect_ready(1.0).expect("connect_ready ok");
        assert!(client.is_connected());

        let (mut server, _) = listener.accept().expect("accept client");
        let mut reader = BufReader::new(server.try_clone().expect("clone stream"));

        // Client automatically sent request_sync on connect.
        let mut first_line = String::new();
        reader
            .read_line(&mut first_line)
            .expect("read request_sync");
        let req: Value = serde_json::from_str(first_line.trim()).unwrap();
        assert_eq!(req["type"], "request_sync");

        // Server writes a frame split across two writes to verify line buffering.
        server
            .write_all(b"{\"type\":\"toast\",\"level\":\"info\",")
            .unwrap();
        let mut received = Vec::new();
        client.poll(1.1, |v| received.push(v));
        assert!(received.is_empty(), "partial line must stay buffered");

        server.write_all(b"\"text\":\"hello\"}\n").unwrap();
        client.poll(1.2, |v| received.push(v));
        assert_eq!(received.len(), 1);
        assert_eq!(received[0]["text"], "hello");

        // Client sends submit_task.
        client
            .send(&msg_submit_task("macbook-pro", "ping"))
            .expect("send ok");
        let mut second_line = String::new();
        reader
            .read_line(&mut second_line)
            .expect("read submit_task");
        let msg: Value = serde_json::from_str(second_line.trim()).unwrap();
        assert_eq!(msg["type"], "submit_task");
        assert_eq!(msg["prompt"], "ping");

        // Closing server triggers clean EOF disconnect with backoff armed.
        drop(reader);
        drop(server);
        client.poll(2.0, |_| {});
        assert!(!client.is_connected());
        assert_eq!(client.test_state(), (false, 0.2, Some(2.0)));

        drop(listener);
        let _ = std::fs::remove_file(&sock_path);
    }

    #[test]
    fn oversized_frame_is_queued_not_dropped() {
        let sock_path =
            std::env::temp_dir().join(format!("lapis_ui_ipc_big_{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock_path);
        let listener = UnixListener::bind(&sock_path).expect("bind unix socket");

        let mut client = IpcClient::new(&sock_path);
        client.connect_ready(1.0).expect("connect_ready ok");
        let (mut server, _) = listener.accept().expect("accept client");
        server.set_nonblocking(true).unwrap();

        // Larger than any default socket buffer, and nobody is reading yet: the
        // old `write_all` hit WouldBlock here and tore the connection down.
        let big = msg_screen_snapshot("big", &"A".repeat(600_000));
        client.send(&big).expect("queued, not failed");
        assert!(client.is_connected());
        assert!(!client.out.is_empty(), "the tail must wait in the queue");
        // A frame sent behind it keeps its place in line.
        client.send(&msg_pairing_control("stop")).expect("queued");

        let mut got = Vec::new();
        let mut chunk = [0u8; 65536];
        for _ in 0..20_000 {
            client.poll(1.5, |_| {});
            match server.read(&mut chunk) {
                Ok(n) => got.extend_from_slice(&chunk[..n]),
                Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_micros(200));
                }
                Err(e) => panic!("server read failed: {e}"),
            }
            if got.iter().filter(|&&b| b == b'\n').count() == 3 {
                break;
            }
        }
        assert!(client.is_connected());
        assert!(client.out.is_empty());
        let lines: Vec<Value> = got
            .split(|&b| b == b'\n')
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_slice(l).expect("whole NDJSON line"))
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["type"], "request_sync");
        assert_eq!(lines[1], big);
        assert_eq!(lines[2], msg_pairing_control("stop"));

        // A daemon that never reads cannot grow the queue without bound.
        let mut overflowed = false;
        for _ in 0..8 {
            if client.send(&big).is_err() {
                overflowed = true;
                break;
            }
        }
        assert!(overflowed && !client.is_connected());

        drop(server);
        drop(listener);
        let _ = std::fs::remove_file(&sock_path);
    }
}
