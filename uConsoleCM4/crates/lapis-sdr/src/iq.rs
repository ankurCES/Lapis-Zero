//! IQ sources: the RTL-SDR (`rtl_sdr`) and SoapySDR (`rx_sdr`) command-line receivers, and a
//! deterministic synthetic band plan for CI, the host preview and a deck with no dongle.

use rustfft::num_complex::Complex32;
use std::f64::consts::TAU;
use std::io::Read;
use std::process::{Child, ChildStdout, Command, Stdio};

#[derive(Debug, thiserror::Error)]
pub enum SdrError {
    #[error("SDR disconnected: {0}")]
    Disconnected(String),
    #[error("{0} is not installed")]
    Missing(String),
    #[error("{0}")]
    Invalid(String),
}

#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub struct SourceInfo {
    pub kind: String,
    pub center_hz: u64,
    pub sample_rate: u32,
    pub gain_db: f32,
    pub synthetic: bool,
}

pub trait IqSource: Send {
    /// Fills `buf` completely or fails (a short read means the device went away).
    fn read(&mut self, buf: &mut [Complex32]) -> Result<(), SdrError>;
    fn tune(&mut self, center_hz: u64) -> Result<(), SdrError>;
    fn set_gain(&mut self, gain_db: f32) -> Result<(), SdrError>;
    fn info(&self) -> SourceInfo;
    /// Stops using the hardware (another program needs the dongle); `read` reopens it.
    fn release(&mut self) {}
}

// ----------------------------------------------------------------- synthetic

#[derive(Debug, Clone, Copy)]
pub enum Kind {
    /// Frequency-modulated audio tone: (deviation Hz, tone Hz).
    Fm(f64, f64),
    /// Amplitude-modulated tone: (depth 0..1, tone Hz).
    Am(f64, f64),
    /// Unmodulated carrier (also an SSB tone: a USB tone at f is a carrier at f).
    Carrier,
    /// On-off keyed bursts: (on samples per period at 2.048 MS/s, period).
    Ook(u64, u64),
}

#[derive(Debug, Clone, Copy)]
pub struct Signal {
    pub freq_hz: f64,
    pub amp: f64,
    pub kind: Kind,
}

/// A small band plan the waterfall can find: broadcast FM, airband, NOAA weather, APRS,
/// 433 MHz ISM sensors, a 915 MHz carrier, ADS-B and an HF USB tone.
pub fn band_plan() -> Vec<Signal> {
    vec![
        Signal {
            freq_hz: 88.5e6,
            amp: 0.25,
            kind: Kind::Fm(75e3, 1000.0),
        },
        Signal {
            freq_hz: 94.7e6,
            amp: 0.30,
            kind: Kind::Fm(75e3, 800.0),
        },
        Signal {
            freq_hz: 100.1e6,
            amp: 0.35,
            kind: Kind::Fm(75e3, 1000.0),
        },
        Signal {
            freq_hz: 100.7e6,
            amp: 0.12,
            kind: Kind::Fm(75e3, 600.0),
        },
        Signal {
            freq_hz: 104.3e6,
            amp: 0.20,
            kind: Kind::Fm(75e3, 1200.0),
        },
        Signal {
            freq_hz: 121.5e6,
            amp: 0.15,
            kind: Kind::Am(0.8, 1000.0),
        },
        Signal {
            freq_hz: 144.39e6,
            amp: 0.20,
            kind: Kind::Fm(3.5e3, 1200.0),
        },
        Signal {
            freq_hz: 162.4e6,
            amp: 0.18,
            kind: Kind::Fm(5e3, 1050.0),
        },
        Signal {
            freq_hz: 162.55e6,
            amp: 0.10,
            kind: Kind::Fm(5e3, 1050.0),
        },
        Signal {
            freq_hz: 433.92e6,
            amp: 0.30,
            kind: Kind::Ook(4_096, 8_192),
        },
        Signal {
            freq_hz: 915.0e6,
            amp: 0.12,
            kind: Kind::Carrier,
        },
        Signal {
            freq_hz: 1090.0e6,
            amp: 0.25,
            kind: Kind::Ook(256, 20_480),
        },
        Signal {
            freq_hz: 7.1015e6,
            amp: 0.20,
            kind: Kind::Carrier,
        },
    ]
}

pub struct Synthetic {
    center: u64,
    rate: u32,
    gain: f32,
    signals: Vec<Signal>,
    phases: Vec<f64>,
    t: u64,
    rng: u64,
}

impl Synthetic {
    pub fn new(center_hz: u64, rate: u32, gain_db: f32) -> Self {
        Self::with_signals(center_hz, rate, gain_db, band_plan())
    }

    pub fn with_signals(center_hz: u64, rate: u32, gain_db: f32, signals: Vec<Signal>) -> Self {
        let n = signals.len();
        Synthetic {
            center: center_hz,
            rate: rate.max(1),
            gain: gain_db,
            signals,
            phases: vec![0.0; n],
            t: 0,
            rng: 0x9E37_79B9_7F4A_7C15,
        }
    }

    fn uniform(&mut self) -> f64 {
        // xorshift64*: deterministic, so every CI run sees the same waterfall.
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        ((self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }

    fn gauss(&mut self) -> (f64, f64) {
        let (u1, u2) = (self.uniform(), self.uniform());
        let r = (-2.0 * u1.ln()).sqrt();
        (r * (TAU * u2).cos(), r * (TAU * u2).sin())
    }
}

impl IqSource for Synthetic {
    fn read(&mut self, buf: &mut [Complex32]) -> Result<(), SdrError> {
        let rate = f64::from(self.rate);
        let g = 10f64.powf((f64::from(self.gain) - 30.0) / 20.0);
        let half = rate / 2.0;
        let active: Vec<usize> = (0..self.signals.len())
            .filter(|&i| (self.signals[i].freq_hz - self.center as f64).abs() < half)
            .collect();
        for s in buf.iter_mut() {
            let (nr, ni) = self.gauss();
            let (mut re, mut im) = (nr * 0.004, ni * 0.004);
            let t = self.t as f64 / rate;
            for &i in &active {
                let sig = self.signals[i];
                let offset = sig.freq_hz - self.center as f64;
                let (amp, inst_freq) = match sig.kind {
                    Kind::Fm(dev, tone) => (sig.amp, offset + dev * (TAU * tone * t).sin()),
                    Kind::Am(depth, tone) => (
                        sig.amp * (1.0 + depth * (TAU * tone * t).sin()) / (1.0 + depth),
                        offset,
                    ),
                    Kind::Carrier => (sig.amp, offset),
                    Kind::Ook(on, period) => {
                        let scaled = self.t * 2_048_000 / u64::from(self.rate.max(1));
                        (
                            if scaled % period.max(1) < on {
                                sig.amp
                            } else {
                                0.0
                            },
                            offset,
                        )
                    }
                };
                self.phases[i] = (self.phases[i] + TAU * inst_freq / rate) % TAU;
                re += amp * self.phases[i].cos();
                im += amp * self.phases[i].sin();
            }
            self.t += 1;
            *s = Complex32::new(
                (re * g).clamp(-1.0, 1.0) as f32,
                (im * g).clamp(-1.0, 1.0) as f32,
            );
        }
        Ok(())
    }

    fn tune(&mut self, center_hz: u64) -> Result<(), SdrError> {
        self.center = center_hz;
        Ok(())
    }

    fn set_gain(&mut self, gain_db: f32) -> Result<(), SdrError> {
        self.gain = gain_db.clamp(0.0, 50.0);
        Ok(())
    }

    fn info(&self) -> SourceInfo {
        SourceInfo {
            kind: "synthetic".into(),
            center_hz: self.center,
            sample_rate: self.rate,
            gain_db: self.gain,
            synthetic: true,
        }
    }
}

// ----------------------------------------------------------------- command-line receivers

#[derive(Debug, Clone, PartialEq)]
pub enum CliKind {
    /// `rtl_sdr` (librtlsdr): unsigned 8-bit interleaved IQ on stdout.
    RtlSdr,
    /// `rx_sdr` (rx_tools, SoapySDR): CF32 on stdout; `args` is the Soapy device string.
    Soapy(String),
}

pub struct CliSource {
    kind: CliKind,
    center: u64,
    rate: u32,
    gain: f32,
    child: Option<(Child, ChildStdout)>,
    raw: Vec<u8>,
}

impl CliSource {
    pub fn new(kind: CliKind, center_hz: u64, rate: u32, gain_db: f32) -> Self {
        CliSource {
            kind,
            center: center_hz,
            rate,
            gain: gain_db,
            child: None,
            raw: Vec::new(),
        }
    }

    pub fn argv(&self) -> Vec<String> {
        let (f, s, g) = (
            self.center.to_string(),
            self.rate.to_string(),
            format!("{:.1}", self.gain),
        );
        match &self.kind {
            CliKind::RtlSdr => vec![
                "rtl_sdr".into(),
                "-f".into(),
                f,
                "-s".into(),
                s,
                "-g".into(),
                g,
                "-".into(),
            ],
            CliKind::Soapy(args) => {
                let mut v = vec!["rx_sdr".to_owned()];
                if !args.is_empty() {
                    v.extend(["-d".into(), args.clone()]);
                }
                v.extend([
                    "-f".into(),
                    f,
                    "-s".into(),
                    s,
                    "-g".into(),
                    g,
                    "-F".into(),
                    "CF32".into(),
                    "-".into(),
                ]);
                v
            }
        }
    }

    fn spawn(&mut self) -> Result<(), SdrError> {
        let argv = self.argv();
        let mut child = Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| SdrError::Missing(argv[0].clone()))?;
        let out = child
            .stdout
            .take()
            .ok_or_else(|| SdrError::Disconnected("no stdout".into()))?;
        self.child = Some((child, out));
        Ok(())
    }

    fn stop(&mut self) {
        if let Some((mut c, _)) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Drop for CliSource {
    fn drop(&mut self) {
        self.stop();
    }
}

/// `rtl_sdr` bytes -> samples: `(b - 127.5) / 127.5`.
pub fn convert_u8(raw: &[u8], out: &mut [Complex32]) {
    for (s, pair) in out.iter_mut().zip(raw.as_chunks::<2>().0) {
        *s = Complex32::new(
            (f32::from(pair[0]) - 127.5) / 127.5,
            (f32::from(pair[1]) - 127.5) / 127.5,
        );
    }
}

/// CF32 little-endian -> samples.
pub fn convert_cf32(raw: &[u8], out: &mut [Complex32]) {
    for (s, c) in out.iter_mut().zip(raw.as_chunks::<8>().0) {
        *s = Complex32::new(
            f32::from_le_bytes([c[0], c[1], c[2], c[3]]),
            f32::from_le_bytes([c[4], c[5], c[6], c[7]]),
        );
    }
}

impl IqSource for CliSource {
    fn read(&mut self, buf: &mut [Complex32]) -> Result<(), SdrError> {
        if self.child.is_none() {
            self.spawn()?;
        }
        let width = if self.kind == CliKind::RtlSdr { 2 } else { 8 };
        self.raw.resize(buf.len() * width, 0);
        let Some((child, out)) = self.child.as_mut() else {
            return Err(SdrError::Disconnected("not running".into()));
        };
        if let Err(e) = out.read_exact(&mut self.raw) {
            let status = child.try_wait().ok().flatten();
            self.stop();
            return Err(SdrError::Disconnected(format!("{e} (exit {status:?})")));
        }
        if width == 2 {
            convert_u8(&self.raw, buf);
        } else {
            convert_cf32(&self.raw, buf);
        }
        Ok(())
    }

    fn tune(&mut self, center_hz: u64) -> Result<(), SdrError> {
        // Neither tool retunes a running stream: restart it on the next read.
        self.center = center_hz;
        self.stop();
        Ok(())
    }

    fn set_gain(&mut self, gain_db: f32) -> Result<(), SdrError> {
        self.gain = gain_db.clamp(0.0, 50.0);
        self.stop();
        Ok(())
    }

    fn info(&self) -> SourceInfo {
        let kind = match &self.kind {
            CliKind::RtlSdr => "rtl_sdr".to_owned(),
            CliKind::Soapy(a) => format!("soapy {a}"),
        };
        SourceInfo {
            kind,
            center_hz: self.center,
            sample_rate: self.rate,
            gain_db: self.gain,
            synthetic: false,
        }
    }

    fn release(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_is_deterministic_and_band_limited() {
        let mut a = Synthetic::new(100_000_000, 2_048_000, 30.0);
        let mut b = Synthetic::new(100_000_000, 2_048_000, 30.0);
        let (mut x, mut y) = (
            vec![Complex32::default(); 512],
            vec![Complex32::default(); 512],
        );
        a.read(&mut x).unwrap();
        b.read(&mut y).unwrap();
        assert_eq!(x, y);
        // Tuned away from every signal, only noise is left.
        let mut quiet = Synthetic::new(50_000_000, 2_048_000, 30.0);
        quiet.read(&mut x).unwrap();
        let power: f32 = x.iter().map(|s| s.norm_sqr()).sum::<f32>() / x.len() as f32;
        assert!(power < 1e-3, "{power}");
        assert!(quiet.info().synthetic);
    }

    #[test]
    fn sample_conversion_and_argv() {
        let mut out = [Complex32::default(); 2];
        convert_u8(&[0, 255, 128, 127], &mut out);
        assert!((out[0].re + 1.0).abs() < 1e-6 && (out[0].im - 1.0).abs() < 1e-6);
        let mut raw = Vec::new();
        for v in [0.5f32, -0.25] {
            raw.extend_from_slice(&v.to_le_bytes());
        }
        convert_cf32(&raw, &mut out[..1]);
        assert_eq!(out[0], Complex32::new(0.5, -0.25));
        let rtl = CliSource::new(CliKind::RtlSdr, 433_920_000, 1_024_000, 20.0);
        assert_eq!(
            rtl.argv().join(" "),
            "rtl_sdr -f 433920000 -s 1024000 -g 20.0 -"
        );
        let soapy = CliSource::new(
            CliKind::Soapy("driver=hackrf".into()),
            1_090_000_000,
            2_000_000,
            30.0,
        );
        assert_eq!(
            soapy.argv().join(" "),
            "rx_sdr -d driver=hackrf -f 1090000000 -s 2000000 -g 30.0 -F CF32 -"
        );
        let mut missing = CliSource::new(CliKind::Soapy(String::new()), 1, 1, 0.0);
        missing.kind = CliKind::Soapy(String::new());
        let _ = missing.info();
    }
}
