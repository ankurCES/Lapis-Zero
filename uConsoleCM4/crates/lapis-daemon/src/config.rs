//! `/etc/lapis/deck.toml`: hardware wiring and policy knobs. Every field has a default, so a
//! missing file is a working configuration.

use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const PORT: u16 = 8421;
/// The only address the control plane ever binds (AGENTS.md invariant 2).
pub const BIND: [u8; 4] = [127, 0, 0, 1];
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DeckConfig {
    pub rails: RailsConfig,
    pub policy: PolicyConfig,
    pub pty: PtyConfig,
    pub wifi_iface: String,
    pub gpsd: String,
    pub meshtasticd: String,
    /// LoRa region the daemon sets on meshtasticd (`US`, `EU_868`, ...: Meshtastic's RegionCode
    /// names). Empty: leave the radio's own setting alone.
    pub mesh_region: String,
    pub sdr: SdrConfig,
}

impl Default for DeckConfig {
    fn default() -> Self {
        DeckConfig {
            rails: RailsConfig::default(),
            policy: PolicyConfig::default(),
            pty: PtyConfig::default(),
            wifi_iface: "wlan0".into(),
            gpsd: "127.0.0.1:2947".into(),
            meshtasticd: "127.0.0.1:4403".into(),
            mesh_region: String::new(),
            sdr: SdrConfig::default(),
        }
    }
}

/// GPIO numbers of the switchable power rails. Defaults are the HackerGadgets AIO V2 board
/// (aiov2_ctl): GPS 27, LoRa 16, SDR 7, internal USB hub 23, all active high. On a V1 board
/// the rails are hard-wired on: set `board = "v1"` and the GPIO rails are reported as fixed.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RailsConfig {
    /// `v2` (switchable rails), `v1` (always on) or `none` (no AIO board).
    pub board: String,
    pub gps_pin: Option<u8>,
    pub lora_pin: Option<u8>,
    pub sdr_pin: Option<u8>,
    pub usb_hub_pin: Option<u8>,
    /// Command that powers the ClockworkPi 4G module (`<cmd> enable|disable`).
    pub lte_command: Option<String>,
    /// `rtl_biast` toggles the RTL-SDR Blog bias tee.
    pub bias_tee_command: Option<String>,
}

impl Default for RailsConfig {
    fn default() -> Self {
        RailsConfig {
            board: "v2".into(),
            gps_pin: Some(27),
            lora_pin: Some(16),
            sdr_pin: Some(7),
            usb_hub_pin: Some(23),
            lte_command: Some("uconsole-4g-cm4".into()),
            bias_tee_command: Some("rtl_biast".into()),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PolicyConfig {
    /// A pending approval nobody answers is denied after this long.
    pub approval_timeout_s: u64,
    /// Extra command fragments that are refused outright, on top of the built-in list.
    pub extra_denylist: Vec<String>,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        PolicyConfig {
            approval_timeout_s: 600,
            extra_denylist: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PtyConfig {
    /// Account `hw.pty_exec` runs commands as when the daemon is root.
    pub user: String,
    pub max_timeout_s: u64,
}

impl Default for PtyConfig {
    fn default() -> Self {
        PtyConfig {
            user: "lapis".into(),
            max_timeout_s: 120,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SdrConfig {
    /// `auto`, `synthetic`, `rtl_sdr` or `soapy`.
    pub source: String,
    pub sample_rate: u32,
    pub fft_size: usize,
    pub gain_db: f32,
    pub center_hz: u64,
}

impl Default for SdrConfig {
    fn default() -> Self {
        SdrConfig {
            source: "auto".into(),
            sample_rate: 2_048_000,
            fft_size: 1024,
            gain_db: 30.0,
            center_hz: 100_000_000,
        }
    }
}

impl DeckConfig {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(toml::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
}

/// Where the daemon keeps its files.
#[derive(Debug, Clone)]
pub struct Paths {
    pub state_dir: PathBuf,
    pub harness_socket: PathBuf,
}

impl Paths {
    pub fn devices(&self) -> PathBuf {
        self.state_dir.join("devices.json")
    }
    pub fn local_token(&self) -> PathBuf {
        self.state_dir.join("local.token")
    }
    pub fn audit(&self) -> PathBuf {
        self.state_dir.join("audit.jsonl")
    }
    pub fn ui_settings(&self) -> PathBuf {
        self.state_dir.join("ui.json")
    }
}

/// Atomic write (temp file + rename) with the given mode.
pub fn atomic_write(path: &Path, data: &[u8], mode: u32) -> std::io::Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    opts.mode(mode);
    let mut f = opts.open(&tmp)?;
    f.write_all(data)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_overrides() {
        let c: DeckConfig =
            toml::from_str("[rails]\ngps_pin = 5\n[policy]\napproval_timeout_s = 30").unwrap();
        assert_eq!(c.rails.gps_pin, Some(5));
        assert_eq!(c.rails.usb_hub_pin, Some(23));
        assert_eq!(c.policy.approval_timeout_s, 30);
        assert_eq!(c.wifi_iface, "wlan0");
        let missing = DeckConfig::load(Path::new("/nonexistent/deck.toml")).unwrap();
        assert_eq!(missing.rails.lora_pin, Some(16));
        // The file the image ships parses, and its mesh_region default leaves the radio alone.
        let shipped: DeckConfig = toml::from_str(include_str!(
            "../../../os-image/stage-lapis/files/etc/deck.toml"
        ))
        .unwrap();
        assert_eq!(shipped.mesh_region, "");
    }
}
