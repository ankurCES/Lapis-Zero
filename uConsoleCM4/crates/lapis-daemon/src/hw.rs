//! Hardware behind traits: AXP228 battery, power rails (GPIO `pinctrl`, rfkill, bias tee, 4G),
//! backlight, system stats and power actions. `LinuxHardware` reads sysfs under a configurable
//! root and runs commands through a [`Runner`]; `MockHardware` is deterministic, so the whole
//! stack runs and is tested on a laptop.

use crate::config::RailsConfig;
use crate::net;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, thiserror::Error)]
pub enum HwError {
    #[error("{0} is not available on this device")]
    Unavailable(String),
    #[error("{0}")]
    Failed(String),
}

/// Runs a command: `(exit status, stdout+stderr)`. 127 = not installed, 124 = timed out.
pub trait Runner: Send + Sync {
    fn run(&self, argv: &[&str], timeout: Duration) -> (i32, String);
}

pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, argv: &[&str], timeout: Duration) -> (i32, String) {
        run_command(argv, timeout, None)
    }
}

/// Spawns `argv`, keeps at most 64 KiB of output, kills it after `timeout`.
pub fn run_command(argv: &[&str], timeout: Duration, uid_gid: Option<(u32, u32)>) -> (i32, String) {
    let Some((prog, args)) = argv.split_first() else {
        return (127, String::new());
    };
    let mut cmd = Command::new(prog);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Own process group, so a timeout kills what the command started too.
        cmd.process_group(0);
        if let Some((uid, gid)) = uid_gid {
            cmd.uid(uid).gid(gid);
        }
    }
    #[cfg(not(unix))]
    let _ = uid_gid;
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return (127, String::new()),
    };
    let (Some(mut out), Some(mut err)) = (child.stdout.take(), child.stderr.take()) else {
        return (127, String::new());
    };
    // Drain both pipes to EOF (so a chatty child never blocks on a full pipe), keep the head.
    fn drain(mut pipe: impl Read, keep: usize) -> Vec<u8> {
        let mut kept = Vec::new();
        let mut chunk = [0u8; 8192];
        while let Ok(n) = pipe.read(&mut chunk) {
            if n == 0 {
                break;
            }
            let room = keep.saturating_sub(kept.len());
            kept.extend_from_slice(&chunk[..n.min(room)]);
        }
        kept
    }
    let err_reader = std::thread::spawn(move || drain(&mut err, 16 * 1024));
    let reader = std::thread::spawn(move || {
        let mut buf = drain(&mut out, 64 * 1024);
        buf.extend(err_reader.join().unwrap_or_default());
        buf
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let group = format!("-{}", child.id());
                let _ = Command::new("kill").args(["-KILL", "--", &group]).status();
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let output = reader.join().unwrap_or_default();
    let text = String::from_utf8_lossy(&output).into_owned();
    match status {
        Some(s) => (s.code().unwrap_or(1), text),
        None => (124, text),
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Battery {
    pub percent: u8,
    pub voltage_v: f64,
    pub current_a: Option<f64>,
    pub power_w: Option<f64>,
    pub charging: bool,
    pub status: String,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct SystemStats {
    pub temp_c: Option<f64>,
    pub load1: Option<f64>,
    pub total_mb: f64,
    pub available_mb: f64,
    pub swap_total_mb: f64,
    pub swap_used_mb: f64,
    pub uptime_sec: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rail {
    Gps,
    Lora,
    Sdr,
    UsbHub,
    BiasTee,
    Lte,
    Wifi,
    Bluetooth,
}

impl Rail {
    pub const ALL: [Rail; 8] = [
        Rail::Gps,
        Rail::Lora,
        Rail::Sdr,
        Rail::UsbHub,
        Rail::BiasTee,
        Rail::Lte,
        Rail::Wifi,
        Rail::Bluetooth,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Rail::Gps => "gps",
            Rail::Lora => "lora",
            Rail::Sdr => "sdr",
            Rail::UsbHub => "usb_hub",
            Rail::BiasTee => "bias_tee",
            Rail::Lte => "lte",
            Rail::Wifi => "wifi",
            Rail::Bluetooth => "bluetooth",
        }
    }

    pub fn parse(name: &str) -> Option<Rail> {
        Rail::ALL.into_iter().find(|r| r.name() == name)
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RailState {
    pub name: &'static str,
    /// None when the state cannot be read back (bias tee, 4G).
    pub on: Option<bool>,
    pub switchable: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    Reboot,
    Shutdown,
    RestartDaemon,
}

pub trait Hardware: Send + Sync {
    fn is_mock(&self) -> bool;
    fn battery(&self) -> Option<Battery>;
    fn system(&self) -> SystemStats;
    fn rails(&self) -> Vec<RailState>;
    fn set_rail(&self, rail: Rail, on: bool) -> Result<(), HwError>;
    /// Screen brightness in percent.
    fn brightness(&self) -> Option<u8>;
    fn set_brightness(&self, percent: u8) -> Result<(), HwError>;
    fn set_mute(&self, mute: bool) -> Result<(), HwError>;
    fn set_timezone(&self, tz: &str) -> Result<(), HwError>;
    fn power(&self, action: PowerAction) -> Result<(), HwError>;
    /// `systemctl is-active` of the Lapis units.
    fn services(&self) -> BTreeMap<String, String>;
    /// Wi-Fi and Tailscale as seen right now (slow: the status loop caches it).
    fn network(&self) -> net::NetStatus;
}

pub const SERVICES: [&str; 4] = ["lapis-daemon", "lapis-harness", "lapis-ui", "tailscaled"];

// ----------------------------------------------------------------- Linux

pub struct LinuxHardware {
    root: PathBuf,
    runner: Arc<dyn Runner>,
    rails: RailsConfig,
    wifi_iface: String,
    /// Rails whose state cannot be read back remember what was last set.
    remembered: Mutex<BTreeMap<Rail, bool>>,
}

impl LinuxHardware {
    pub fn new(
        root: impl Into<PathBuf>,
        runner: Arc<dyn Runner>,
        rails: RailsConfig,
        wifi_iface: &str,
    ) -> Self {
        LinuxHardware {
            root: root.into(),
            runner,
            rails,
            wifi_iface: wifi_iface.to_owned(),
            remembered: Mutex::new(BTreeMap::new()),
        }
    }

    fn read(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.root.join(rel.trim_start_matches('/')))
            .ok()
            .map(|s| s.trim().to_owned())
    }

    fn num(&self, rel: &str) -> Option<f64> {
        self.read(rel)?.parse().ok()
    }

    fn backlight_dir(&self) -> Option<PathBuf> {
        let dir = self.root.join("sys/class/backlight");
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .collect();
        entries.sort();
        // The uConsole panel's OCP8178 is `backlight@0`; take it first when present.
        entries
            .iter()
            .find(|p| p.ends_with("backlight@0"))
            .cloned()
            .or_else(|| entries.into_iter().next())
    }

    fn gpio_pin(&self, rail: Rail) -> Option<u8> {
        if self.rails.board != "v2" {
            return None;
        }
        match rail {
            Rail::Gps => self.rails.gps_pin,
            Rail::Lora => self.rails.lora_pin,
            Rail::Sdr => self.rails.sdr_pin,
            Rail::UsbHub => self.rails.usb_hub_pin,
            _ => None,
        }
    }

    fn rfkill(&self, kind: &str) -> Option<bool> {
        let dir = self.root.join("sys/class/rfkill");
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let p = entry.path();
            let ty = std::fs::read_to_string(p.join("type")).unwrap_or_default();
            if ty.trim() == kind {
                let soft = std::fs::read_to_string(p.join("soft")).unwrap_or_default();
                let hard = std::fs::read_to_string(p.join("hard")).unwrap_or_default();
                return Some(soft.trim() == "0" && hard.trim() == "0");
            }
        }
        None
    }

    fn run_ok(&self, argv: &[&str]) -> Result<String, HwError> {
        let (rc, out) = self.runner.run(argv, Duration::from_secs(10));
        match rc {
            0 => Ok(out),
            127 => Err(HwError::Unavailable(
                argv.first().copied().unwrap_or("command").to_owned(),
            )),
            _ => Err(HwError::Failed(format!(
                "{} exited {rc}: {}",
                argv.join(" "),
                out.trim()
            ))),
        }
    }
}

/// `pinctrl get 27` -> `27: op -- pn | hi // GPIO27 = output`.
pub fn parse_pinctrl_level(out: &str) -> Option<bool> {
    let level = out.split('|').nth(1)?.split_whitespace().next()?;
    match level {
        "hi" => Some(true),
        "lo" => Some(false),
        _ => None,
    }
}

pub fn parse_meminfo(text: &str) -> (f64, f64, f64, f64) {
    let mut kb = BTreeMap::new();
    for line in text.lines() {
        if let Some((key, rest)) = line.split_once(':') {
            if let Some(v) = rest
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<f64>().ok())
            {
                kb.insert(key.trim().to_owned(), v);
            }
        }
    }
    let get = |k: &str| kb.get(k).copied().unwrap_or(0.0) / 1024.0;
    (
        get("MemTotal"),
        get("MemAvailable"),
        get("SwapTotal"),
        get("SwapTotal") - get("SwapFree"),
    )
}

impl Hardware for LinuxHardware {
    fn is_mock(&self) -> bool {
        false
    }

    fn battery(&self) -> Option<Battery> {
        const B: &str = "sys/class/power_supply/axp20x-battery";
        let percent = self.num(&format!("{B}/capacity"))?;
        let voltage_v = self.num(&format!("{B}/voltage_now")).unwrap_or(0.0) / 1e6;
        let current_a = self.num(&format!("{B}/current_now")).map(|ua| ua / 1e6);
        let status = self.read(&format!("{B}/status")).unwrap_or_default();
        let ac = self
            .read("sys/class/power_supply/axp22x-ac/online")
            .is_some_and(|v| v == "1");
        let power_w = self
            .num(&format!("{B}/power_now"))
            .map(|uw| uw / 1e6)
            .or_else(|| current_a.map(|a| (a * voltage_v).abs()));
        Some(Battery {
            percent: percent.clamp(0.0, 100.0) as u8,
            voltage_v,
            current_a,
            power_w,
            charging: status == "Charging" || (ac && status != "Discharging"),
            status,
        })
    }

    fn system(&self) -> SystemStats {
        let (total_mb, available_mb, swap_total_mb, swap_used_mb) =
            parse_meminfo(&self.read("proc/meminfo").unwrap_or_default());
        SystemStats {
            temp_c: self
                .num("sys/class/thermal/thermal_zone0/temp")
                .map(|m| m / 1000.0),
            load1: self
                .read("proc/loadavg")
                .and_then(|l| l.split_whitespace().next()?.parse().ok()),
            total_mb,
            available_mb,
            swap_total_mb,
            swap_used_mb,
            uptime_sec: self
                .read("proc/uptime")
                .and_then(|u| u.split_whitespace().next()?.parse::<f64>().ok())
                .unwrap_or(0.0) as u64,
        }
    }

    fn rails(&self) -> Vec<RailState> {
        let remembered = self
            .remembered
            .lock()
            .map(|m| m.clone())
            .unwrap_or_default();
        Rail::ALL
            .into_iter()
            .map(|rail| match rail {
                Rail::Wifi | Rail::Bluetooth => {
                    let kind = if rail == Rail::Wifi {
                        "wlan"
                    } else {
                        "bluetooth"
                    };
                    let on = self.rfkill(kind);
                    RailState {
                        name: rail.name(),
                        on,
                        switchable: on.is_some(),
                        detail: "rfkill".into(),
                    }
                }
                Rail::BiasTee | Rail::Lte => {
                    let cmd = if rail == Rail::BiasTee {
                        &self.rails.bias_tee_command
                    } else {
                        &self.rails.lte_command
                    };
                    RailState {
                        name: rail.name(),
                        on: remembered.get(&rail).copied(),
                        switchable: cmd.is_some(),
                        detail: cmd.clone().unwrap_or_else(|| "not configured".into()),
                    }
                }
                _ => match self.gpio_pin(rail) {
                    Some(pin) => {
                        let (rc, out) = self.runner.run(
                            &["pinctrl", "get", &pin.to_string()],
                            Duration::from_secs(3),
                        );
                        let on = if rc == 0 {
                            parse_pinctrl_level(&out)
                        } else {
                            None
                        };
                        RailState {
                            name: rail.name(),
                            on,
                            switchable: true,
                            detail: format!("GPIO{pin}"),
                        }
                    }
                    None => RailState {
                        name: rail.name(),
                        on: (self.rails.board == "v1").then_some(true),
                        switchable: false,
                        detail: format!("AIO board {}", self.rails.board),
                    },
                },
            })
            .collect()
    }

    fn set_rail(&self, rail: Rail, on: bool) -> Result<(), HwError> {
        match rail {
            Rail::Wifi | Rail::Bluetooth => {
                let kind = if rail == Rail::Wifi {
                    "wifi"
                } else {
                    "bluetooth"
                };
                self.run_ok(&["rfkill", if on { "unblock" } else { "block" }, kind])?;
            }
            Rail::BiasTee => {
                let cmd = self
                    .rails
                    .bias_tee_command
                    .as_deref()
                    .ok_or_else(|| HwError::Unavailable("bias tee".into()))?;
                self.run_ok(&[cmd, "-b", if on { "1" } else { "0" }])?;
            }
            Rail::Lte => {
                let cmd = self
                    .rails
                    .lte_command
                    .as_deref()
                    .ok_or_else(|| HwError::Unavailable("4G module".into()))?;
                self.run_ok(&[cmd, if on { "enable" } else { "disable" }])?;
            }
            _ => {
                let pin = self
                    .gpio_pin(rail)
                    .ok_or_else(|| HwError::Unavailable(rail.name().into()))?;
                self.run_ok(&[
                    "pinctrl",
                    "set",
                    &pin.to_string(),
                    "op",
                    if on { "dh" } else { "dl" },
                ])?;
            }
        }
        if let Ok(mut m) = self.remembered.lock() {
            m.insert(rail, on);
        }
        Ok(())
    }

    fn brightness(&self) -> Option<u8> {
        let dir = self.backlight_dir()?;
        let cur: f64 = std::fs::read_to_string(dir.join("brightness"))
            .ok()?
            .trim()
            .parse()
            .ok()?;
        let max: f64 = std::fs::read_to_string(dir.join("max_brightness"))
            .ok()?
            .trim()
            .parse()
            .ok()?;
        (max > 0.0).then(|| (cur * 100.0 / max).round().clamp(0.0, 100.0) as u8)
    }

    fn set_brightness(&self, percent: u8) -> Result<(), HwError> {
        let dir = self
            .backlight_dir()
            .ok_or_else(|| HwError::Unavailable("backlight".into()))?;
        let max: u32 = std::fs::read_to_string(dir.join("max_brightness"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .ok_or_else(|| HwError::Unavailable("backlight".into()))?;
        // Never write 0 from a slider: a black panel with no way back on is a brick.
        let level = ((u32::from(percent.min(100)) * max).div_ceil(100)).max(1);
        std::fs::write(dir.join("brightness"), level.to_string())
            .map_err(|e| HwError::Failed(e.to_string()))
    }

    fn set_mute(&self, mute: bool) -> Result<(), HwError> {
        self.run_ok(&[
            "amixer",
            "-q",
            "sset",
            "Master",
            if mute { "mute" } else { "unmute" },
        ])
        .map(|_| ())
    }

    fn set_timezone(&self, tz: &str) -> Result<(), HwError> {
        self.run_ok(&["timedatectl", "set-timezone", tz])
            .map(|_| ())
    }

    fn power(&self, action: PowerAction) -> Result<(), HwError> {
        let argv: &[&str] = match action {
            PowerAction::Reboot => &["systemctl", "reboot"],
            PowerAction::Shutdown => &["systemctl", "poweroff"],
            PowerAction::RestartDaemon => {
                &["systemctl", "restart", "lapis-harness", "lapis-daemon"]
            }
        };
        self.run_ok(argv).map(|_| ())
    }

    fn services(&self) -> BTreeMap<String, String> {
        let units: Vec<String> = SERVICES.iter().map(|s| format!("{s}.service")).collect();
        let mut argv = vec!["systemctl", "is-active"];
        argv.extend(units.iter().map(String::as_str));
        let (_, out) = self.runner.run(&argv, Duration::from_secs(5));
        let states: Vec<&str> = out.split_whitespace().collect();
        SERVICES
            .iter()
            .enumerate()
            .map(|(i, s)| {
                (
                    (*s).to_owned(),
                    states.get(i).copied().unwrap_or("unknown").to_owned(),
                )
            })
            .collect()
    }

    fn network(&self) -> net::NetStatus {
        net::read_status(self.runner.as_ref(), &self.wifi_iface, &self.root)
    }
}

// ----------------------------------------------------------------- mock

/// Deterministic stand-in for a uConsole: what the preview, the tests and CI see.
pub struct MockHardware {
    state: Mutex<MockState>,
    started: Instant,
}

struct MockState {
    rails: BTreeMap<Rail, bool>,
    brightness: u8,
    muted: bool,
    timezone: String,
    tailscale: String,
}

impl Default for MockHardware {
    fn default() -> Self {
        let rails = Rail::ALL
            .into_iter()
            .map(|r| (r, !matches!(r, Rail::BiasTee | Rail::Lte)))
            .collect();
        MockHardware {
            state: Mutex::new(MockState {
                rails,
                brightness: 80,
                muted: false,
                timezone: "UTC".into(),
                tailscale: "connected".into(),
            }),
            started: Instant::now(),
        }
    }
}

impl MockHardware {
    /// e.g. `needs_login`, to exercise the pairing paths.
    pub fn set_tailscale_state(&self, state: &str) {
        if let Ok(mut s) = self.state.lock() {
            s.tailscale = state.to_owned();
        }
    }

    pub fn timezone(&self) -> String {
        self.state
            .lock()
            .map(|s| s.timezone.clone())
            .unwrap_or_default()
    }

    pub fn muted(&self) -> bool {
        self.state.lock().map(|s| s.muted).unwrap_or(false)
    }
}

impl Hardware for MockHardware {
    fn is_mock(&self) -> bool {
        true
    }

    fn battery(&self) -> Option<Battery> {
        Some(Battery {
            percent: 78,
            voltage_v: 3.92,
            current_a: Some(-0.82),
            power_w: Some(3.21),
            charging: false,
            status: "Discharging".into(),
        })
    }

    fn system(&self) -> SystemStats {
        SystemStats {
            temp_c: Some(51.5),
            load1: Some(0.42),
            total_mb: 3796.0,
            available_mb: 2931.0,
            swap_total_mb: 1024.0,
            swap_used_mb: 0.0,
            uptime_sec: self.started.elapsed().as_secs(),
        }
    }

    fn rails(&self) -> Vec<RailState> {
        let rails = self
            .state
            .lock()
            .map(|s| s.rails.clone())
            .unwrap_or_default();
        Rail::ALL
            .into_iter()
            .map(|r| RailState {
                name: r.name(),
                on: rails.get(&r).copied(),
                switchable: true,
                detail: "mock".into(),
            })
            .collect()
    }

    fn set_rail(&self, rail: Rail, on: bool) -> Result<(), HwError> {
        let mut s = self
            .state
            .lock()
            .map_err(|_| HwError::Failed("mock state poisoned".into()))?;
        s.rails.insert(rail, on);
        Ok(())
    }

    fn brightness(&self) -> Option<u8> {
        self.state.lock().ok().map(|s| s.brightness)
    }

    fn set_brightness(&self, percent: u8) -> Result<(), HwError> {
        let mut s = self
            .state
            .lock()
            .map_err(|_| HwError::Failed("mock state poisoned".into()))?;
        s.brightness = percent.clamp(1, 100);
        Ok(())
    }

    fn set_mute(&self, mute: bool) -> Result<(), HwError> {
        let mut s = self
            .state
            .lock()
            .map_err(|_| HwError::Failed("mock state poisoned".into()))?;
        s.muted = mute;
        Ok(())
    }

    fn set_timezone(&self, tz: &str) -> Result<(), HwError> {
        let mut s = self
            .state
            .lock()
            .map_err(|_| HwError::Failed("mock state poisoned".into()))?;
        s.timezone = tz.to_owned();
        Ok(())
    }

    fn power(&self, action: PowerAction) -> Result<(), HwError> {
        tracing::info!(?action, "mock power action (nothing happens)");
        Ok(())
    }

    fn services(&self) -> BTreeMap<String, String> {
        SERVICES
            .iter()
            .map(|s| ((*s).to_owned(), "active".to_owned()))
            .collect()
    }

    fn network(&self) -> net::NetStatus {
        let ts = self
            .state
            .lock()
            .map(|s| s.tailscale.clone())
            .unwrap_or_default();
        let wifi_on = self
            .state
            .lock()
            .map(|s| s.rails.get(&Rail::Wifi) != Some(&false))
            .unwrap_or(true);
        let connected = ts == "connected" && wifi_on;
        net::NetStatus {
            wifi_ssid: if wifi_on {
                "lapis-lab".into()
            } else {
                String::new()
            },
            wifi_ip: if wifi_on {
                "192.168.1.42".into()
            } else {
                String::new()
            },
            wifi_rssi_dbm: wifi_on.then_some(-58),
            tailscale_state: if wifi_on { ts } else { "offline".into() },
            tailscale_ip: if connected {
                "100.101.102.103".into()
            } else {
                String::new()
            },
            tailscale_fqdn: if connected {
                "lapis-deck.tail1234.ts.net".into()
            } else {
                String::new()
            },
            tailscale_auth_url: None,
            tailscale_peers: if connected { 3 } else { 0 },
            tailscale_served: connected,
        }
    }
}

/// uid/gid of a local account, from /etc/passwd.
pub fn lookup_user(passwd: &Path, name: &str) -> Option<(u32, u32)> {
    let text = std::fs::read_to_string(passwd).ok()?;
    text.lines().find_map(|line| {
        let f: Vec<&str> = line.split(':').collect();
        (f.len() > 3 && f[0] == name).then(|| Some((f[2].parse().ok()?, f[3].parse().ok()?)))?
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake(Mutex<Vec<String>>);
    impl Runner for Fake {
        fn run(&self, argv: &[&str], _t: Duration) -> (i32, String) {
            self.0.lock().unwrap().push(argv.join(" "));
            match argv {
                ["pinctrl", "get", "27"] => (0, "27: op -- pn | hi // GPIO27 = output".into()),
                ["pinctrl", "get", _] => (0, "16: op -- pd | lo // GPIO16 = output".into()),
                ["systemctl", "is-active", ..] => (3, "active\nactive\ninactive\nactive\n".into()),
                _ => (0, String::new()),
            }
        }
    }

    fn sysfs() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let w = |rel: &str, v: &str| {
            let p = d.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, v).unwrap();
        };
        w("sys/class/power_supply/axp20x-battery/capacity", "64\n");
        w(
            "sys/class/power_supply/axp20x-battery/voltage_now",
            "3850000\n",
        );
        w(
            "sys/class/power_supply/axp20x-battery/current_now",
            "-650000\n",
        );
        w(
            "sys/class/power_supply/axp20x-battery/status",
            "Discharging\n",
        );
        w("sys/class/power_supply/axp22x-ac/online", "0\n");
        w("sys/class/backlight/backlight@0/brightness", "5\n");
        w("sys/class/backlight/backlight@0/max_brightness", "9\n");
        w("sys/class/thermal/thermal_zone0/temp", "48250\n");
        w("sys/class/rfkill/rfkill0/type", "wlan\n");
        w("sys/class/rfkill/rfkill0/soft", "0\n");
        w("sys/class/rfkill/rfkill0/hard", "0\n");
        w("sys/class/rfkill/rfkill1/type", "bluetooth\n");
        w("sys/class/rfkill/rfkill1/soft", "1\n");
        w("sys/class/rfkill/rfkill1/hard", "0\n");
        w("proc/loadavg", "0.51 0.40 0.30 1/200 999\n");
        w("proc/uptime", "1234.5 4000.0\n");
        w("proc/meminfo", "MemTotal: 3887104 kB\nMemAvailable: 3001344 kB\nSwapTotal: 1048576 kB\nSwapFree: 1048576 kB\n");
        d
    }

    #[test]
    fn linux_sysfs_and_rails() {
        let root = sysfs();
        let runner = Arc::new(Fake(Mutex::new(Vec::new())));
        let hw = LinuxHardware::new(root.path(), runner.clone(), RailsConfig::default(), "wlan0");
        let b = hw.battery().unwrap();
        assert_eq!(b.percent, 64);
        assert!((b.voltage_v - 3.85).abs() < 1e-9);
        assert!((b.power_w.unwrap() - 2.5025).abs() < 1e-6);
        assert!(!b.charging);
        let s = hw.system();
        assert_eq!(s.temp_c, Some(48.25));
        assert_eq!(s.load1, Some(0.51));
        assert_eq!(s.uptime_sec, 1234);
        assert_eq!(s.total_mb.round(), 3796.0);
        assert_eq!(hw.brightness(), Some(56));
        hw.set_brightness(0).unwrap();
        assert_eq!(
            std::fs::read_to_string(
                root.path()
                    .join("sys/class/backlight/backlight@0/brightness")
            )
            .unwrap(),
            "1"
        );
        hw.set_brightness(100).unwrap();
        assert_eq!(hw.brightness(), Some(100));

        let rails = hw.rails();
        let get = |n: &str| rails.iter().find(|r| r.name == n).unwrap().clone();
        assert_eq!(get("gps").on, Some(true));
        assert_eq!(get("lora").on, Some(false));
        assert_eq!(get("wifi").on, Some(true));
        assert_eq!(get("bluetooth").on, Some(false));
        assert_eq!(get("bias_tee").on, None);
        hw.set_rail(Rail::UsbHub, false).unwrap();
        hw.set_rail(Rail::BiasTee, true).unwrap();
        hw.set_rail(Rail::Bluetooth, true).unwrap();
        let calls = runner.0.lock().unwrap().clone();
        assert!(calls.contains(&"pinctrl set 23 op dl".to_string()));
        assert!(calls.contains(&"rtl_biast -b 1".to_string()));
        assert!(calls.contains(&"rfkill unblock bluetooth".to_string()));
        assert_eq!(
            hw.rails().iter().find(|r| r.name == "bias_tee").unwrap().on,
            Some(true)
        );
        assert_eq!(hw.services()["lapis-ui"], "inactive");

        let v1 = LinuxHardware::new(
            root.path(),
            runner,
            RailsConfig {
                board: "v1".into(),
                ..Default::default()
            },
            "wlan0",
        );
        assert!(matches!(
            v1.set_rail(Rail::Gps, false),
            Err(HwError::Unavailable(_))
        ));
    }

    #[test]
    fn helpers() {
        assert_eq!(
            parse_pinctrl_level("23: op -- pn | lo // GPIO23 = output"),
            Some(false)
        );
        assert_eq!(parse_pinctrl_level("garbage"), None);
        let (rc, out) = run_command(
            &["sh", "-c", "echo hi; sleep 5"],
            Duration::from_millis(300),
            None,
        );
        assert_eq!((rc, out.trim()), (124, "hi"));
        assert_eq!(
            run_command(&["/nonexistent/x"], Duration::from_secs(1), None).0,
            127
        );
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("passwd");
        std::fs::write(
            &p,
            "root:x:0:0::/root:/bin/sh\nlapis:x:1000:1000::/home/lapis:/bin/bash\n",
        )
        .unwrap();
        assert_eq!(lookup_user(&p, "lapis"), Some((1000, 1000)));
        assert_eq!(lookup_user(&p, "nobody"), None);
    }
}
