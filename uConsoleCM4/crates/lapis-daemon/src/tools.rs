//! Local cyberdeck tools (SPEC 5.2): the typed surface the agent, the MCP server and the HUD
//! share. Each tool has a base policy tier; `invoke` runs only after the Policy Gate allowed it.

use crate::app::App;
use crate::hw::{run_command, Rail};
use crate::policy::Tier;
use regex::Regex;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub tier: Tier,
    pub schema: fn() -> Value,
}

fn obj(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
}

fn none() -> Value {
    obj(json!({}), &[])
}

pub fn specs() -> Vec<ToolSpec> {
    let mut v = vec![
        ToolSpec { name: "hw.battery_status", description: "Battery percent, voltage, draw (W) and charging state from the AXP228, plus power rails.", tier: Tier::T0, schema: none },
        ToolSpec {
            name: "hw.gpio_rail",
            description: "Switch a power rail: gps, lora, sdr, usb_hub, bias_tee, lte, wifi, bluetooth.",
            tier: Tier::T2,
            schema: || obj(json!({"name": {"type": "string", "enum": Rail::ALL.map(Rail::name)}, "state": {"type": "boolean"}}), &["name", "state"]),
        },
        ToolSpec {
            name: "hw.serial_probe",
            description: "Listen on a serial port (/dev/ttyUSB*, ttyACM*, ttyS*, ttyAMA*) and return what arrives.",
            tier: Tier::T1,
            schema: || obj(json!({"device": {"type": "string"}, "baud": {"type": "integer"}, "duration_ms": {"type": "integer", "maximum": 5000}}), &["device"]),
        },
        ToolSpec {
            name: "hw.pty_exec",
            description: "Run a shell command on the uConsole (read-only commands run at once; others need approval).",
            tier: Tier::T1,
            schema: || obj(json!({"cmd": {"type": "string"}, "timeout_s": {"type": "integer", "maximum": 120}}), &["cmd"]),
        },
        ToolSpec { name: "net.tailscale_peers", description: "Tailscale peers with IP, OS and online state.", tier: Tier::T0, schema: none },
    ];
    v.extend(crate::deck::specs());
    v
}

pub fn tier_of(name: &str) -> Option<Tier> {
    specs().into_iter().find(|s| s.name == name).map(|s| s.tier)
}

/// Checks a call's arguments before it is put to the Policy Gate.
pub fn validate(name: &str, args: &Value) -> Result<(), String> {
    match name {
        "hw.gpio_rail" => {
            args["name"]
                .as_str()
                .and_then(Rail::parse)
                .ok_or("unknown rail")?;
            args["state"]
                .as_bool()
                .ok_or("state must be true or false")?;
        }
        "hw.serial_probe"
            if !serial_re()
                .is_some_and(|re| re.is_match(args["device"].as_str().unwrap_or(""))) =>
        {
            return Err(
                "device must be /dev/ttyUSB*, /dev/ttyACM*, /dev/ttyS* or /dev/ttyAMA*".into(),
            );
        }
        "hw.pty_exec" if args["cmd"].as_str().unwrap_or("").trim().is_empty() => {
            return Err("cmd is required".into())
        }
        _ => crate::deck::validate(name, args)?,
    }
    Ok(())
}

/// The text the Policy Gate inspects for a call (the command of `hw.pty_exec`).
pub fn inspected(name: &str, args: &Value) -> Option<String> {
    (name == "hw.pty_exec").then(|| args["cmd"].as_str().unwrap_or("").to_owned())
}

pub fn summary(name: &str, args: &Value) -> String {
    match name {
        "hw.pty_exec" => format!("$ {}", args["cmd"].as_str().unwrap_or("")),
        "hw.gpio_rail" => format!(
            "{} rail {}",
            args["name"].as_str().unwrap_or("?"),
            if args["state"].as_bool() == Some(true) {
                "ON"
            } else {
                "OFF"
            }
        ),
        _ => {
            let a = args.to_string();
            if a == "{}" || a == "null" {
                name.to_owned()
            } else {
                format!("{name} {}", a.chars().take(120).collect::<String>())
            }
        }
    }
}

fn serial_re() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^/dev/tty(USB|ACM|S|AMA)[0-9]{1,2}$").ok())
        .as_ref()
}

fn current_uid() -> Option<u32> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find(|l| l.starts_with("Uid:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

pub async fn invoke(app: &Arc<App>, name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "hw.battery_status" => {
            let hw = app.hw.clone();
            let (battery, rails) = tokio::task::spawn_blocking(move || (hw.battery(), hw.rails()))
                .await
                .map_err(|e| e.to_string())?;
            Ok(json!({"battery": battery, "rails": rails}))
        }
        "hw.gpio_rail" => {
            let rail = args["name"]
                .as_str()
                .and_then(Rail::parse)
                .ok_or("unknown rail")?;
            let on = args["state"]
                .as_bool()
                .ok_or("state must be true or false")?;
            app.set_rail(rail, on, "agent").await?;
            Ok(json!({"rail": rail.name(), "on": on}))
        }
        "hw.serial_probe" => {
            let device = args["device"].as_str().unwrap_or("").to_owned();
            if !serial_re().is_some_and(|re| re.is_match(&device)) {
                return Err(
                    "device must be /dev/ttyUSB*, /dev/ttyACM*, /dev/ttyS* or /dev/ttyAMA*".into(),
                );
            }
            let baud = args["baud"].as_u64().unwrap_or(115_200) as u32;
            if ![
                1200, 2400, 4800, 9600, 19_200, 38_400, 57_600, 115_200, 230_400, 460_800, 921_600,
            ]
            .contains(&baud)
            {
                return Err("unsupported baud rate".into());
            }
            let ms = args["duration_ms"].as_u64().unwrap_or(1500).min(5000);
            tokio::task::spawn_blocking(move || serial_probe(&device, baud, ms))
                .await
                .map_err(|e| e.to_string())?
        }
        "hw.pty_exec" => {
            let cmd = args["cmd"].as_str().unwrap_or("").to_owned();
            if cmd.trim().is_empty() {
                return Err("cmd is required".into());
            }
            let max = app.cfg.pty.max_timeout_s;
            let timeout =
                Duration::from_secs(args["timeout_s"].as_u64().unwrap_or(30).clamp(1, max));
            // As root, commands run as the unprivileged account, never as root.
            let as_user = if current_uid() == Some(0) {
                // Never root: the configured account, else `nobody`.
                let passwd = std::path::Path::new("/etc/passwd");
                Some(
                    crate::hw::lookup_user(passwd, &app.cfg.pty.user)
                        .or_else(|| crate::hw::lookup_user(passwd, "nobody"))
                        .unwrap_or((65_534, 65_534)),
                )
            } else {
                None
            };
            let (rc, out) = tokio::task::spawn_blocking(move || {
                run_command(&["sh", "-c", &cmd], timeout, as_user)
            })
            .await
            .map_err(|e| e.to_string())?;
            Ok(
                json!({"exit_status": rc, "timed_out": rc == 124, "output": lapis_crypto::redact(&out)}),
            )
        }
        "net.tailscale_peers" => {
            if app.mock {
                return Ok(json!({"peers": crate::deck::mock_peers()}));
            }
            let (rc, out) = tokio::task::spawn_blocking(|| {
                run_command(
                    &["tailscale", "status", "--json"],
                    Duration::from_secs(5),
                    None,
                )
            })
            .await
            .map_err(|e| e.to_string())?;
            if rc != 0 {
                return Err("tailscale is not running".into());
            }
            Ok(json!({"peers": crate::net::parse_tailscale_peers(&out)}))
        }
        _ => crate::deck::invoke(app, name, args).await,
    }
}

fn serial_probe(device: &str, baud: u32, ms: u64) -> Result<Value, String> {
    use std::io::Read;
    let mut port = serialport::new(device, baud)
        .timeout(Duration::from_millis(100))
        .open()
        .map_err(|e| format!("{device}: {e}"))?;
    let deadline = std::time::Instant::now() + Duration::from_millis(ms);
    let mut data = Vec::new();
    let mut buf = [0u8; 512];
    while std::time::Instant::now() < deadline && data.len() < 8192 {
        match port.read(&mut buf) {
            Ok(n) => data.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    let text: String = String::from_utf8_lossy(&data)
        .chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\r' {
                '.'
            } else {
                c
            }
        })
        .collect();
    let hex: String = data.iter().take(256).map(|b| format!("{b:02x}")).collect();
    Ok(
        json!({"device": device, "baud": baud, "bytes": data.len(), "text": lapis_crypto::redact(&text), "hex": hex}),
    )
}
