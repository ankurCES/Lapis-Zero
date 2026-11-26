//! Demodulators for the RF workspace presets: WFM, NFM, AM, USB and LSB. Mix the wanted
//! signal to DC, low-pass and decimate, demodulate, then filter down to ~48 kHz audio.

use rustfft::num_complex::Complex32;
use std::f64::consts::TAU;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Wfm,
    Nfm,
    Am,
    Usb,
    Lsb,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s.to_ascii_lowercase().as_str() {
            "wfm" | "fm" => Some(Mode::Wfm),
            "nfm" => Some(Mode::Nfm),
            "am" => Some(Mode::Am),
            "usb" => Some(Mode::Usb),
            "lsb" => Some(Mode::Lsb),
            _ => None,
        }
    }

    /// (channel bandwidth, intermediate rate, FM deviation).
    fn params(self) -> (f64, f64, f64) {
        match self {
            Mode::Wfm => (100e3, 240e3, 75e3),
            Mode::Nfm => (6e3, 48e3, 5e3),
            Mode::Am => (5e3, 48e3, 0.0),
            Mode::Usb | Mode::Lsb => (3e3, 48e3, 0.0),
        }
    }
}

/// Windowed-sinc low-pass taps for a cutoff of `fc` (fraction of the sample rate).
fn lowpass(fc: f64, taps: usize) -> Vec<f32> {
    let m = taps - 1;
    let mut h: Vec<f64> = (0..taps)
        .map(|i| {
            let x = i as f64 - m as f64 / 2.0;
            let sinc = if x == 0.0 {
                2.0 * fc
            } else {
                (TAU * fc * x).sin() / (std::f64::consts::PI * x)
            };
            sinc * (0.54 - 0.46 * (TAU * i as f64 / m as f64).cos())
        })
        .collect();
    let sum: f64 = h.iter().sum();
    h.iter_mut().for_each(|v| *v /= sum);
    h.into_iter().map(|v| v as f32).collect()
}

fn fir_decimate_c(x: &[Complex32], h: &[f32], decim: usize) -> Vec<Complex32> {
    let decim = decim.max(1);
    (0..x.len().saturating_sub(h.len()) / decim)
        .map(|k| {
            let base = k * decim;
            h.iter()
                .enumerate()
                .fold(Complex32::default(), |acc, (j, t)| acc + x[base + j] * *t)
        })
        .collect()
}

fn fir_decimate_r(x: &[f32], h: &[f32], decim: usize) -> Vec<f32> {
    let decim = decim.max(1);
    (0..x.len().saturating_sub(h.len()) / decim)
        .map(|k| {
            h.iter()
                .enumerate()
                .map(|(j, t)| x[k * decim + j] * t)
                .sum()
        })
        .collect()
}

fn mix(x: &[Complex32], shift_hz: f64, rate: f64) -> Vec<Complex32> {
    x.iter()
        .enumerate()
        .map(|(n, s)| {
            let ph = -TAU * shift_hz * n as f64 / rate;
            s * Complex32::new(ph.cos() as f32, ph.sin() as f32)
        })
        .collect()
}

/// Demodulates the signal `offset_hz` from the tuned center. Returns (audio, audio rate).
pub fn demodulate(iq: &[Complex32], rate: u32, offset_hz: f64, mode: Mode) -> (Vec<f32>, u32) {
    let rate_f = f64::from(rate.max(1));
    let (bw, if_rate, dev) = mode.params();
    let decim = (rate_f / if_rate).floor().max(1.0) as usize;
    let if_rate = rate_f / decim as f64;
    // SSB: center the passband (0..+bw or -bw..0) before the complex low-pass.
    let ssb_shift = match mode {
        Mode::Usb => bw / 2.0,
        Mode::Lsb => -bw / 2.0,
        _ => 0.0,
    };
    let base = mix(iq, offset_hz + ssb_shift, rate_f);
    let cutoff = if matches!(mode, Mode::Usb | Mode::Lsb) {
        bw / 2.0
    } else {
        bw
    };
    let chan = fir_decimate_c(&base, &lowpass((cutoff / rate_f).min(0.45), 63), decim);
    let mut audio: Vec<f32> = match mode {
        Mode::Wfm | Mode::Nfm => {
            let k = (if_rate / (TAU * dev)) as f32;
            chan.windows(2)
                .map(|w| (w[1] * w[0].conj()).arg() * k)
                .collect()
        }
        Mode::Am => chan.iter().map(|s| s.norm()).collect(),
        Mode::Usb | Mode::Lsb => mix(&chan, -ssb_shift, if_rate)
            .iter()
            .map(|s| s.re)
            .collect(),
    };
    // DC block.
    let mean = audio.iter().sum::<f32>() / audio.len().max(1) as f32;
    audio.iter_mut().for_each(|v| *v -= mean);
    let (audio, out_rate) = if matches!(mode, Mode::Wfm) {
        let d2 = (if_rate / 48e3).round().max(1.0) as usize;
        (
            fir_decimate_r(&audio, &lowpass(15e3 / if_rate, 63), d2),
            if_rate / d2 as f64,
        )
    } else {
        (audio, if_rate)
    };
    let peak = audio.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-9);
    (
        audio.iter().map(|v| v / peak * 0.9).collect(),
        out_rate.round() as u32,
    )
}

pub fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

/// The strongest audio frequency (Hz), by FFT.
pub fn dominant_freq(pcm: &[f32], rate: u32) -> f32 {
    let n = pcm.len().min(16_384).next_power_of_two() / 2;
    if n < 64 {
        return 0.0;
    }
    let mut buf: Vec<Complex32> = pcm[..n].iter().map(|v| Complex32::new(*v, 0.0)).collect();
    rustfft::FftPlanner::new()
        .plan_fft_forward(n)
        .process(&mut buf);
    let (best, _) = buf[1..n / 2]
        .iter()
        .enumerate()
        .fold((0, 0f32), |b, (i, v)| {
            if v.norm() > b.1 {
                (i + 1, v.norm())
            } else {
                b
            }
        });
    best as f32 * rate as f32 / n as f32
}

/// 16-bit mono PCM WAV.
pub fn wav(pcm: &[f32], rate: u32) -> Vec<u8> {
    let data_len = (pcm.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for v in pcm {
        out.extend_from_slice(&((v.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iq::{IqSource, Kind, Signal, Synthetic};

    fn capture(signal: Signal, center: u64, n: usize) -> Vec<Complex32> {
        let mut src = Synthetic::with_signals(center, 1_024_000, 30.0, vec![signal]);
        let mut buf = vec![Complex32::default(); n];
        src.read(&mut buf).unwrap();
        buf
    }

    #[test]
    fn am_fm_and_ssb_recover_the_tone() {
        let center = 145_000_000;
        let am = capture(
            Signal {
                freq_hz: center as f64 + 50e3,
                amp: 0.3,
                kind: Kind::Am(0.8, 1000.0),
            },
            center,
            120_000,
        );
        let (pcm, rate) = demodulate(&am, 1_024_000, 50e3, Mode::Am);
        assert!(
            (dominant_freq(&pcm, rate) - 1000.0).abs() < 40.0,
            "{}",
            dominant_freq(&pcm, rate)
        );

        let nfm = capture(
            Signal {
                freq_hz: center as f64 - 120e3,
                amp: 0.3,
                kind: Kind::Fm(5e3, 700.0),
            },
            center,
            120_000,
        );
        let (pcm, rate) = demodulate(&nfm, 1_024_000, -120e3, Mode::Nfm);
        assert!(
            (dominant_freq(&pcm, rate) - 700.0).abs() < 40.0,
            "{}",
            dominant_freq(&pcm, rate)
        );

        let wfm = capture(
            Signal {
                freq_hz: center as f64 + 200e3,
                amp: 0.3,
                kind: Kind::Fm(75e3, 1000.0),
            },
            center,
            240_000,
        );
        let (pcm, rate) = demodulate(&wfm, 1_024_000, 200e3, Mode::Wfm);
        assert!((40_000..=52_000).contains(&rate), "{rate}");
        assert!(
            (dominant_freq(&pcm, rate) - 1000.0).abs() < 60.0,
            "{}",
            dominant_freq(&pcm, rate)
        );

        // A tone 1.5 kHz above the dial frequency is upper sideband.
        let tone = capture(
            Signal {
                freq_hz: center as f64 + 10e3 + 1500.0,
                amp: 0.3,
                kind: Kind::Carrier,
            },
            center,
            120_000,
        );
        let (usb, rate) = demodulate(&tone, 1_024_000, 10e3, Mode::Usb);
        assert!(
            (dominant_freq(&usb, rate) - 1500.0).abs() < 60.0,
            "{}",
            dominant_freq(&usb, rate)
        );
        assert_eq!(Mode::parse("LSB"), Some(Mode::Lsb));
        assert_eq!(Mode::parse("cw"), None);

        let w = wav(&usb[..100], rate);
        assert_eq!((&w[..4], w.len()), (&b"RIFF"[..], 44 + 200));
        assert!(rms(&usb) > 0.1);
    }
}
