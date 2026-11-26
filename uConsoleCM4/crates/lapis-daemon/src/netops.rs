//! NetOps: passive Wi-Fi and BLE surveys, TCP port probes, packet capture, DNS lookups.

use crate::net::split_terse;
use regex::Regex;
use serde_json::{json, Value};
use std::sync::OnceLock;
use std::time::Duration;

/// `nmcli -t -f SSID,BSSID,CHAN,FREQ,SIGNAL,SECURITY dev wifi list --rescan no`: what the radio
/// already heard; nothing is transmitted to get it.
pub const WIFI_ARGV: [&str; 10] = [
    "nmcli",
    "-t",
    "-f",
    "SSID,BSSID,CHAN,FREQ,SIGNAL,SECURITY",
    "dev",
    "wifi",
    "list",
    "--rescan",
    "no",
    "--",
];

pub fn parse_wifi_survey(text: &str) -> Vec<Value> {
    let mut out: Vec<Value> = text
        .lines()
        .filter_map(|l| {
            let f = split_terse(l);
            if f.len() < 6 {
                return None;
            }
            let signal: i64 = f[4].parse().ok()?;
            let freq: i64 = f[3].split_whitespace().next()?.parse().ok()?;
            Some(json!({"ssid": f[0], "bssid": f[1], "channel": f[2].parse::<i64>().ok(), "freq_mhz": freq,
                        "signal_pct": signal, "rssi_dbm": signal / 2 - 100, "security": if f[5].is_empty() { "--" } else { &f[5] },
                        "band": if freq >= 5000 { "5 GHz" } else { "2.4 GHz" }}))
        })
        .collect();
    out.sort_by_key(|v| -v["signal_pct"].as_i64().unwrap_or(0));
    out
}

/// bluetoothctl colours its output and wraps prompts in readline markers.
pub fn strip_ansi(s: &str) -> String {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    match RE.get_or_init(|| Regex::new(r"\x1b\[[0-9;?]*[A-Za-z]|[\x01\x02\r]").ok()) {
        Some(re) => re.replace_all(s, "").into_owned(),
        None => s.to_owned(),
    }
}

fn mac_re() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"Device ([0-9A-F]{2}(?::[0-9A-F]{2}){5})(?: (.*))?$").ok())
        .as_ref()
}

/// `bluetoothctl --timeout N scan on` output -> devices with their last RSSI.
pub fn parse_bluetoothctl(text: &str) -> Vec<Value> {
    let mut devs: Vec<(String, String, Option<i64>)> = Vec::new();
    let Some(re) = mac_re() else {
        return Vec::new();
    };
    for line in text.lines() {
        let clean = strip_ansi(line);
        let Some(c) = re.captures(&clean) else {
            continue;
        };
        let addr = c[1].to_owned();
        let rest = c
            .get(2)
            .map(|m| m.as_str().trim().to_owned())
            .unwrap_or_default();
        let idx = match devs.iter().position(|d| d.0 == addr) {
            Some(i) => i,
            None => {
                devs.push((addr.clone(), String::new(), None));
                devs.len() - 1
            }
        };
        if let Some(r) = rest.strip_prefix("RSSI:") {
            // "RSSI: -60" or "RSSI: 0xffffffc4 (-60)"
            let n = r
                .rsplit('(')
                .next()
                .unwrap_or(r)
                .trim_end_matches(')')
                .trim();
            devs[idx].2 = n.parse().ok();
        } else if clean.contains("[NEW]") && !rest.contains(':') {
            devs[idx].1 = rest;
        } else if let Some(name) = rest
            .strip_prefix("Name:")
            .or_else(|| rest.strip_prefix("Alias:"))
        {
            devs[idx].1 = name.trim().to_owned();
        }
    }
    let mut out: Vec<Value> = devs
        .into_iter()
        .map(|(a, n, r)| json!({"address": a, "name": if n.is_empty() || n.replace('-', ":") == a { Value::Null } else { json!(n) }, "rssi_dbm": r}))
        .collect();
    out.sort_by_key(|v| -v["rssi_dbm"].as_i64().unwrap_or(-200));
    out
}

fn host_re() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^[A-Za-z0-9]([A-Za-z0-9.-]{0,251}[A-Za-z0-9])?$|^[0-9a-fA-F:]{2,39}$").ok()
    })
    .as_ref()
}

pub fn valid_host(h: &str) -> bool {
    host_re().is_some_and(|re| re.is_match(h))
}

pub fn service_name(port: u16) -> &'static str {
    match port {
        22 => "ssh",
        53 => "dns",
        80 => "http",
        443 => "https",
        445 => "smb",
        1883 => "mqtt",
        3389 => "rdp",
        4403 => "meshtastic",
        5900 => "vnc",
        8080 => "http-alt",
        8421 => "lapis",
        _ => "",
    }
}

/// TCP connect probe of up to 64 ports, concurrently, 1.5 s each.
pub async fn port_probe(target: &str, ports: &[u16]) -> Result<Value, String> {
    if !valid_host(target) {
        return Err("target must be a host name or an IP address".into());
    }
    if ports.is_empty() || ports.len() > 64 {
        return Err("give 1 to 64 ports".into());
    }
    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((target, 0))
        .await
        .map_err(|e| format!("{target}: {e}"))?
        .collect();
    let ip = addrs.first().map(|a| a.ip()).ok_or("no address")?;
    let mut tasks = Vec::new();
    for &port in ports {
        tasks.push(tokio::spawn(async move {
            let t0 = std::time::Instant::now();
            let res = tokio::time::timeout(Duration::from_millis(1500), tokio::net::TcpStream::connect((ip, port))).await;
            let state = match res {
                Ok(Ok(_)) => "open",
                Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => "closed",
                _ => "filtered",
            };
            json!({"port": port, "state": state, "service": service_name(port), "ms": t0.elapsed().as_millis() as u64})
        }));
    }
    let mut results = Vec::new();
    for t in tasks {
        results.push(t.await.map_err(|e| e.to_string())?);
    }
    Ok(json!({"target": target, "ip": ip.to_string(), "ports": results}))
}

pub fn valid_iface(i: &str) -> bool {
    !i.is_empty()
        && i.len() <= 15
        && i.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
}

pub fn valid_bpf(f: &str) -> bool {
    f.len() <= 200
        && f.chars()
            .all(|c| c.is_ascii_alphanumeric() || " .:/()!<>=&|-[]".contains(c))
}

/// `tcpdump` argv: no shell, the filter passed as words.
pub fn tcpdump_argv(iface: &str, bpf: &str, count: u32) -> Vec<String> {
    let mut v: Vec<String> = [
        "tcpdump",
        "-i",
        iface,
        "-nn",
        "-l",
        "-q",
        "-tttt",
        "-c",
        &count.to_string(),
    ]
    .map(String::from)
    .to_vec();
    v.extend(bpf.split_whitespace().map(String::from));
    v
}

pub async fn dns_lookup(name: &str) -> Result<Value, String> {
    if !valid_host(name) {
        return Err("not a host name".into());
    }
    let t0 = std::time::Instant::now();
    let addrs: Vec<String> = tokio::net::lookup_host((name, 0))
        .await
        .map_err(|e| format!("{name}: {e}"))?
        .map(|a| a.ip().to_string())
        .collect();
    Ok(json!({"name": name, "addresses": addrs, "ms": t0.elapsed().as_millis() as u64}))
}

pub const MOCK_WIFI: &str = "lapis-lab:AA\\:BB\\:CC\\:00\\:11\\:22:36:5180 MHz:82:WPA2\nlapis-lab:AA\\:BB\\:CC\\:00\\:11\\:23:6:2437 MHz:74:WPA2\ncafe-guest:10\\:20\\:30\\:40\\:50\\:60:11:2462 MHz:41:\nIoT\\:mesh:DE\\:AD\\:BE\\:EF\\:00\\:01:1:2412 MHz:28:WPA3\n";
pub const MOCK_BLE: &str = "Discovery started\n[CHG] Controller DC:A6:32:00:00:01 Discovering: yes\n[NEW] Device F4:12:FA:01:02:03 Pixel 9 Pro\n[NEW] Device C8:2B:96:10:20:30 C8-2B-96-10-20-30\n[CHG] Device F4:12:FA:01:02:03 RSSI: -58\n[NEW] Device E2:11:22:33:44:55 Meshtastic_c0ff\n[CHG] Device E2:11:22:33:44:55 RSSI: 0xffffffa6 (-90)\n[CHG] Device C8:2B:96:10:20:30 RSSI: -77\n";
pub const MOCK_CAPTURE: &str = "2026-11-25 18:00:00.000001 IP 192.168.1.42.51514 > 192.168.1.1.53: UDP, length 37\n2026-11-25 18:00:00.012230 IP 192.168.1.1.53 > 192.168.1.42.51514: UDP, length 53\n2026-11-25 18:00:00.031000 IP 192.168.1.42.40122 > 100.88.1.10.22: tcp 0\n";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsers_and_validation() {
        let w = parse_wifi_survey(MOCK_WIFI);
        assert_eq!(w.len(), 4);
        assert_eq!(
            (w[0]["bssid"].as_str(), w[0]["band"].as_str()),
            (Some("AA:BB:CC:00:11:22"), Some("5 GHz"))
        );
        assert_eq!(w[3]["ssid"], "IoT:mesh");
        assert_eq!(w[2]["security"], "--");
        let b = parse_bluetoothctl(MOCK_BLE);
        assert_eq!(b.len(), 3);
        assert_eq!(
            (b[0]["name"].as_str(), b[0]["rssi_dbm"].as_i64()),
            (Some("Pixel 9 Pro"), Some(-58))
        );
        assert_eq!(b[1]["name"], Value::Null);
        assert_eq!(b[2]["rssi_dbm"], -90);
        assert!(
            valid_host("mbp.tail1234.ts.net") && valid_host("100.88.1.10") && valid_host("fd7a::1")
        );
        assert!(!valid_host("a;rm -rf") && !valid_host(""));
        assert!(valid_iface("wlan0") && !valid_iface("wlan0;x"));
        assert!(valid_bpf("tcp port 22 and not host 1.2.3.4") && !valid_bpf("port 22; reboot"));
        assert_eq!(
            tcpdump_argv("wlan0", "port 53", 10).join(" "),
            "tcpdump -i wlan0 -nn -l -q -tttt -c 10 port 53"
        );
    }

    #[tokio::test]
    async fn probes_loopback() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let open = l.local_addr().unwrap().port();
        let r = port_probe("127.0.0.1", &[open, 1]).await.unwrap();
        assert_eq!(r["ports"][0]["state"], "open");
        assert_eq!(r["ports"][1]["state"], "closed");
        assert!(port_probe("bad host", &[1]).await.is_err());
        assert!(dns_lookup("localhost").await.unwrap()["addresses"]
            .as_array()
            .is_some());
    }
}
