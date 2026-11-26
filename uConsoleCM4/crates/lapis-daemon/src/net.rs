//! Wi-Fi (NetworkManager) and Tailscale: status for the HUD and the app, and the control
//! provisioning needs. Ported from Lapis-Zero's `network_ctl.py`, including what the real
//! device taught it (clean nmcli profiles, auth keys never on a command line, serve verified
//! by reading it back).

use crate::hw::Runner;
use serde::Serialize;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct NetStatus {
    pub wifi_ssid: String,
    pub wifi_ip: String,
    pub wifi_rssi_dbm: Option<i32>,
    /// connected | needs_login | needs_approval | starting | stopped | offline
    pub tailscale_state: String,
    pub tailscale_ip: String,
    pub tailscale_fqdn: String,
    pub tailscale_auth_url: Option<String>,
    pub tailscale_peers: u32,
    /// `tailscale serve` proxies https://<fqdn>/ to the control plane.
    pub tailscale_served: bool,
}

pub fn tailscale_state(backend: &str) -> &'static str {
    match backend {
        "Running" => "connected",
        "NeedsLogin" => "needs_login",
        "NeedsMachineAuth" => "needs_approval",
        "Starting" => "starting",
        "Stopped" => "stopped",
        _ => "offline",
    }
}

/// `tailscale status --json` -> (state, ipv4, fqdn, auth url, online peers).
pub fn parse_tailscale_status(text: &str) -> (String, String, String, Option<String>, u32) {
    let data: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    let me = &data["Self"];
    let ip = me["TailscaleIPs"]
        .as_array()
        .and_then(|ips| {
            ips.iter()
                .filter_map(Value::as_str)
                .find(|ip| ip.contains('.'))
        })
        .unwrap_or("")
        .to_owned();
    let fqdn = me["DNSName"]
        .as_str()
        .unwrap_or("")
        .trim_end_matches('.')
        .to_owned();
    let auth = data["AuthURL"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let peers = data["Peer"]
        .as_object()
        .map(|p| {
            p.values()
                .filter(|v| v["Online"].as_bool() == Some(true))
                .count() as u32
        })
        .unwrap_or(0);
    let state = tailscale_state(data["BackendState"].as_str().unwrap_or("")).to_owned();
    (state, ip, fqdn, auth, peers)
}

/// Tailscale peers for the fleet roster and `net.tailscale_peers`.
pub fn parse_tailscale_peers(text: &str) -> Vec<Value> {
    let data: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    let mut peers: Vec<Value> = data["Peer"]
        .as_object()
        .map(|p| {
            p.values()
                .map(|v| {
                    json!({
                        "hostname": v["HostName"].as_str().unwrap_or(""),
                        "dns_name": v["DNSName"].as_str().unwrap_or("").trim_end_matches('.'),
                        "ip": v["TailscaleIPs"].as_array().and_then(|a| a.iter().filter_map(Value::as_str).find(|i| i.contains('.'))).unwrap_or(""),
                        "os": v["OS"].as_str().unwrap_or(""),
                        "online": v["Online"].as_bool().unwrap_or(false),
                        "exit_node": v["ExitNode"].as_bool().unwrap_or(false),
                        "last_seen": v["LastSeen"].as_str().unwrap_or(""),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    peers.sort_by(|a, b| a["hostname"].as_str().cmp(&b["hostname"].as_str()));
    peers
}

/// One `nmcli -t` line split on unescaped colons (SSIDs may contain `\:`).
pub fn split_terse(line: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut esc = false;
    for ch in line.chars() {
        if esc {
            if let Some(cur) = out.last_mut() {
                cur.push(ch);
            }
            esc = false;
        } else if ch == '\\' {
            esc = true;
        } else if ch == ':' {
            out.push(String::new());
        } else if let Some(cur) = out.last_mut() {
            cur.push(ch);
        }
    }
    out
}

/// `nmcli -t -f SSID,SIGNAL,SECURITY dev wifi list` -> strongest entry per SSID, as the
/// provisioning protocol carries it (`rssi` ~ quality/2 - 100 dBm).
pub fn parse_wifi_list(text: &str) -> Vec<Value> {
    let mut best: Vec<(String, i64, bool)> = Vec::new();
    for line in text.lines() {
        let parts = split_terse(line);
        if parts.len() < 3 || parts[0].is_empty() {
            continue;
        }
        let Ok(quality) = parts[1].parse::<i64>() else {
            continue;
        };
        let rssi = quality / 2 - 100;
        let secured = !matches!(parts[2].as_str(), "" | "--");
        match best.iter_mut().find(|b| b.0 == parts[0]) {
            Some(b) if b.1 < rssi => *b = (parts[0].clone(), rssi, secured),
            Some(_) => {}
            None => best.push((parts[0].clone(), rssi, secured)),
        }
    }
    best.sort_by_key(|b| -b.1);
    best.into_iter()
        .map(|(ssid, rssi, secured)| json!({"ssid": ssid, "rssi": rssi, "secured": secured}))
        .collect()
}

/// `nmcli -t -f GENERAL.CONNECTION,IP4.ADDRESS device show wlan0` -> (ssid, ip).
pub fn parse_nmcli_device(text: &str) -> (String, String) {
    let (mut ssid, mut ip) = (String::new(), String::new());
    for line in text.lines() {
        let (key, value) = line.split_once(':').unwrap_or((line, ""));
        if key == "GENERAL.CONNECTION" && !matches!(value, "" | "--") {
            ssid = value.to_owned();
        } else if key.starts_with("IP4.ADDRESS") && ip.is_empty() {
            ip = value.split('/').next().unwrap_or("").to_owned();
        }
    }
    (ssid, ip)
}

/// Signal level in dBm from /proc/net/wireless.
pub fn parse_wireless_rssi(text: &str, iface: &str) -> Option<i32> {
    text.lines().find_map(|line| {
        let (name, rest) = line.split_once(':')?;
        if name.trim() != iface {
            return None;
        }
        rest.split_whitespace()
            .nth(2)?
            .trim_end_matches('.')
            .parse::<f64>()
            .ok()
            .map(|v| v as i32)
    })
}

pub fn serve_configured(serve_status_json: &str, port: u16) -> bool {
    serve_status_json.contains(&format!("127.0.0.1:{port}"))
}

pub fn read_status(runner: &dyn Runner, iface: &str, root: &Path) -> NetStatus {
    let t = Duration::from_secs(5);
    let (_, dev) = runner.run(
        &[
            "nmcli",
            "-t",
            "-f",
            "GENERAL.CONNECTION,IP4.ADDRESS",
            "device",
            "show",
            iface,
        ],
        t,
    );
    let (wifi_ssid, wifi_ip) = parse_nmcli_device(&dev);
    let wireless = std::fs::read_to_string(root.join("proc/net/wireless")).unwrap_or_default();
    let (rc, ts) = runner.run(&["tailscale", "status", "--json"], t);
    let (state, ip, fqdn, auth, peers) = if rc == 127 {
        ("offline".into(), String::new(), String::new(), None, 0)
    } else {
        parse_tailscale_status(&ts)
    };
    let served = state == "connected"
        && serve_configured(
            &runner.run(&["tailscale", "serve", "status", "--json"], t).1,
            crate::config::PORT,
        );
    NetStatus {
        wifi_rssi_dbm: parse_wireless_rssi(&wireless, iface),
        wifi_ssid,
        wifi_ip,
        tailscale_state: state,
        tailscale_ip: ip,
        tailscale_fqdn: fqdn,
        tailscale_auth_url: auth,
        tailscale_peers: peers,
        tailscale_served: served,
    }
}

/// What BLE provisioning does to the network. Blocking: callers use `spawn_blocking`.
pub trait NetControl: Send + Sync {
    fn wifi_scan(&self) -> Result<Vec<Value>, String>;
    /// `{"state": "connected"|"failed", "local_ip": "..."}`
    fn wifi_connect(&self, ssid: &str, psk: Option<&str>) -> Value;
    fn tailscale_status(&self) -> NetStatus;
    /// Starts `tailscale up` in the background; it blocks until the node is authorised.
    fn tailscale_up(&self, authkey: Option<&str>);
    /// Why the last `tailscale up` gave up, once it has.
    fn tailscale_up_error(&self) -> Option<String>;
    /// Exposes the control plane over tailnet HTTPS; true only when read back as stored.
    fn tailscale_serve(&self) -> bool;
}

pub struct LinuxNet {
    runner: Arc<dyn Runner>,
    iface: String,
    up_result: Arc<Mutex<Option<(i32, String)>>>,
}

impl LinuxNet {
    pub fn new(runner: Arc<dyn Runner>, iface: &str) -> Self {
        LinuxNet {
            runner,
            iface: iface.to_owned(),
            up_result: Arc::new(Mutex::new(None)),
        }
    }

    fn wifi_ready(&self, tries: u32) -> bool {
        for attempt in 0..tries {
            let (_, out) = self.runner.run(
                &[
                    "nmcli",
                    "-g",
                    "GENERAL.STATE",
                    "device",
                    "show",
                    &self.iface,
                ],
                Duration::from_secs(5),
            );
            if out
                .split_whitespace()
                .next()
                .and_then(|c| c.parse::<u32>().ok())
                .is_some_and(|c| c >= 30)
            {
                return true;
            }
            if attempt + 1 < tries {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        false
    }
}

impl NetControl for LinuxNet {
    fn wifi_scan(&self) -> Result<Vec<Value>, String> {
        if !self.wifi_ready(6) {
            return Err("Wi-Fi is not available on the device (wlan0 missing or blocked)".into());
        }
        self.runner
            .run(&["nmcli", "dev", "wifi", "rescan"], Duration::from_secs(10));
        let (_, out) = self.runner.run(
            &[
                "nmcli",
                "-t",
                "-f",
                "SSID,SIGNAL,SECURITY",
                "dev",
                "wifi",
                "list",
            ],
            Duration::from_secs(20),
        );
        Ok(parse_wifi_list(&out).into_iter().take(20).collect())
    }

    fn wifi_connect(&self, ssid: &str, psk: Option<&str>) -> Value {
        self.wifi_ready(3);
        // nmcli reuses a saved profile but drops its key-mgmt when a password is passed:
        // always start from a clean profile (learned on Lapis-Zero hardware).
        self.runner.run(
            &["nmcli", "connection", "delete", "id", ssid],
            Duration::from_secs(10),
        );
        let mut argv = vec!["nmcli", "--wait", "30", "dev", "wifi", "connect", ssid];
        if let Some(psk) = psk {
            argv.extend(["password", psk]);
        }
        let (rc, _) = self.runner.run(&argv, Duration::from_secs(40));
        let mut ip = String::new();
        for _ in 0..10 {
            let (_, dev) = self.runner.run(
                &[
                    "nmcli",
                    "-t",
                    "-f",
                    "GENERAL.CONNECTION,IP4.ADDRESS",
                    "device",
                    "show",
                    &self.iface,
                ],
                Duration::from_secs(5),
            );
            ip = parse_nmcli_device(&dev).1;
            if rc != 0 || !ip.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        if rc != 0 || ip.is_empty() {
            tracing::warn!(rc, "wifi connect failed");
            return json!({"state": "failed", "local_ip": ""});
        }
        json!({"state": "connected", "local_ip": ip})
    }

    fn tailscale_status(&self) -> NetStatus {
        read_status(self.runner.as_ref(), &self.iface, Path::new("/"))
    }

    fn tailscale_up(&self, authkey: Option<&str>) {
        let mut argv: Vec<String> = ["tailscale", "up", "--ssh", "--timeout=300s"]
            .map(String::from)
            .to_vec();
        let mut keyfile = None;
        if let Some(key) = authkey {
            // Not on the command line (any process can read /proc/*/cmdline): tailscale
            // reads `file:` itself. 0600 on tmpfs.
            let path = std::path::PathBuf::from(format!("/run/lapis-tskey-{}", std::process::id()));
            if crate::config::atomic_write(&path, key.as_bytes(), 0o600).is_ok() {
                argv.push(format!("--auth-key=file:{}", path.display()));
                keyfile = Some(path);
            }
        }
        if let Ok(mut r) = self.up_result.lock() {
            *r = None;
        }
        let runner = self.runner.clone();
        let result = self.up_result.clone();
        std::thread::spawn(move || {
            let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
            let out = runner.run(&refs, Duration::from_secs(310));
            if let Some(p) = keyfile {
                let _ = std::fs::remove_file(p);
            }
            if let Ok(mut r) = result.lock() {
                *r = Some(out);
            }
        });
    }

    fn tailscale_up_error(&self) -> Option<String> {
        let r = self.up_result.lock().ok()?.clone()?;
        if r.0 == 0 {
            return None;
        }
        let reason =
            r.1.lines()
                .map(str::trim)
                .rfind(|l| !l.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("tailscale up failed (exit {})", r.0));
        Some(lapis_crypto::redact(&reason).chars().take(160).collect())
    }

    fn tailscale_serve(&self) -> bool {
        let t = Duration::from_secs(20);
        let stored = || {
            serve_configured(
                &self
                    .runner
                    .run(&["tailscale", "serve", "status", "--json"], t)
                    .1,
                crate::config::PORT,
            )
        };
        if stored() {
            return true;
        }
        let target = format!("http://127.0.0.1:{}", crate::config::PORT);
        self.runner
            .run(&["tailscale", "serve", "--bg", "--https=443", &target], t);
        stored()
    }
}

/// Deterministic provisioning network for tests and the host preview.
pub struct MockNet {
    pub status: Mutex<NetStatus>,
    pub up_calls: Mutex<Vec<Option<String>>>,
    pub serve_ok: bool,
}

impl Default for MockNet {
    fn default() -> Self {
        MockNet {
            status: Mutex::new(NetStatus {
                tailscale_state: "needs_login".into(),
                ..Default::default()
            }),
            up_calls: Mutex::new(Vec::new()),
            serve_ok: true,
        }
    }
}

impl NetControl for MockNet {
    fn wifi_scan(&self) -> Result<Vec<Value>, String> {
        Ok(vec![
            json!({"ssid": "lapis-lab", "rssi": -48, "secured": true}),
            json!({"ssid": "cafe", "rssi": -71, "secured": false}),
        ])
    }

    fn wifi_connect(&self, ssid: &str, psk: Option<&str>) -> Value {
        if ssid == "lapis-lab" && psk.is_none() {
            return json!({"state": "failed", "local_ip": ""});
        }
        if let Ok(mut s) = self.status.lock() {
            s.wifi_ssid = ssid.to_owned();
            s.wifi_ip = "192.168.1.42".into();
        }
        json!({"state": "connected", "local_ip": "192.168.1.42"})
    }

    fn tailscale_status(&self) -> NetStatus {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn tailscale_up(&self, authkey: Option<&str>) {
        if let Ok(mut c) = self.up_calls.lock() {
            c.push(authkey.map(str::to_owned));
        }
        if let Ok(mut s) = self.status.lock() {
            if authkey.is_some() {
                s.tailscale_state = "connected".into();
                s.tailscale_ip = "100.101.102.103".into();
                s.tailscale_fqdn = "lapis-deck.tail1234.ts.net".into();
            } else {
                s.tailscale_auth_url = Some("https://login.tailscale.com/a/mock".into());
            }
        }
    }

    fn tailscale_up_error(&self) -> Option<String> {
        None
    }

    fn tailscale_serve(&self) -> bool {
        self.serve_ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsers() {
        let ts = r#"{"BackendState":"Running","AuthURL":"","Self":{"DNSName":"deck.tail1.ts.net.","TailscaleIPs":["100.1.2.3","fd7a::1"]},
            "Peer":{"a":{"HostName":"mbp","DNSName":"mbp.tail1.ts.net.","TailscaleIPs":["100.9.9.9"],"OS":"macOS","Online":true},
                    "b":{"HostName":"arch","Online":false}}}"#;
        let (state, ip, fqdn, auth, peers) = parse_tailscale_status(ts);
        assert_eq!(
            (state.as_str(), ip.as_str(), fqdn.as_str(), auth, peers),
            ("connected", "100.1.2.3", "deck.tail1.ts.net", None, 1)
        );
        let roster = parse_tailscale_peers(ts);
        assert_eq!(roster.len(), 2);
        assert_eq!(roster[1]["hostname"], "mbp");
        assert_eq!(roster[1]["ip"], "100.9.9.9");
        assert_eq!(parse_tailscale_status("not json").0, "offline");

        assert_eq!(
            split_terse(r"my\:net:70:WPA2"),
            vec!["my:net", "70", "WPA2"]
        );
        let list = parse_wifi_list("home:80:WPA2\nhome:40:WPA2\nopen:30:--\n:90:WPA2\nbad:x:--\n");
        assert_eq!(
            list,
            vec![
                json!({"ssid":"home","rssi":-60,"secured":true}),
                json!({"ssid":"open","rssi":-85,"secured":false})
            ]
        );
        assert_eq!(
            parse_nmcli_device("GENERAL.CONNECTION:home\nIP4.ADDRESS[1]:192.168.1.9/24\n"),
            ("home".into(), "192.168.1.9".into())
        );
        assert_eq!(
            parse_nmcli_device("GENERAL.CONNECTION:--\n"),
            (String::new(), String::new())
        );
        let wireless = "Inter-| sta-|   Quality        |\n face | tus | link level noise\n wlan0: 0000   55.  -55.  -256        0\n";
        assert_eq!(parse_wireless_rssi(wireless, "wlan0"), Some(-55));
        assert!(serve_configured(
            r#"{"Web":{"x:443":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:8421"}}}}}"#,
            8421
        ));
        assert!(!serve_configured("{}", 8421));
    }
}
