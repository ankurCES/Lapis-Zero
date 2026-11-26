//! FFT spectrum (Hann window, fft-shifted dB, exponential averaging), the waterfall's
//! quantisation and colour map, and band scanning.

use crate::iq::{IqSource, SdrError};
use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};
use std::sync::Arc;

pub struct Spectrum {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    scratch: Vec<Complex32>,
    avg: Vec<f32>,
    /// 0 = no averaging, close to 1 = slow.
    pub smoothing: f32,
}

impl Spectrum {
    pub fn new(size: usize) -> Self {
        let size = size.clamp(64, 16_384).next_power_of_two();
        let fft = FftPlanner::new().plan_fft_forward(size);
        let window = (0..size)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / (size - 1) as f32).cos())
            .collect();
        Spectrum {
            fft,
            window,
            scratch: vec![Complex32::default(); size],
            avg: Vec::new(),
            smoothing: 0.5,
        }
    }

    pub fn size(&self) -> usize {
        self.window.len()
    }

    /// Power in dBFS per bin, DC in the middle (bin 0 = center - rate/2).
    pub fn power_db(&mut self, iq: &[Complex32]) -> Vec<f32> {
        let n = self.size();
        let gain: f32 = self.window.iter().sum::<f32>();
        for (i, s) in self.scratch.iter_mut().enumerate() {
            *s = iq.get(i).copied().unwrap_or_default() * self.window[i];
        }
        self.fft.process(&mut self.scratch);
        let mut out = vec![0.0f32; n];
        for (i, v) in self.scratch.iter().enumerate() {
            let p = v.norm_sqr() / (gain * gain);
            out[(i + n / 2) % n] = 10.0 * (p + 1e-14).log10();
        }
        if self.avg.len() != n || self.smoothing <= 0.0 {
            self.avg = out;
        } else {
            let a = self.smoothing.clamp(0.0, 0.99);
            for (m, x) in self.avg.iter_mut().zip(out) {
                *m = a * *m + (1.0 - a) * x;
            }
        }
        self.avg.clone()
    }

    pub fn reset(&mut self) {
        self.avg.clear();
    }
}

pub fn bin_freq(center_hz: u64, rate: u32, bins: usize, bin: usize) -> f64 {
    center_hz as f64 - f64::from(rate) / 2.0 + (bin as f64 + 0.5) * f64::from(rate) / bins as f64
}

pub fn median(v: &[f32]) -> f32 {
    let mut s: Vec<f32> = v.iter().copied().filter(|x| x.is_finite()).collect();
    if s.is_empty() {
        return f32::NEG_INFINITY;
    }
    s.sort_by(f32::total_cmp);
    s[s.len() / 2]
}

/// A row as 0..=255 between `floor` and `ceil` dB: what goes over the WebSocket.
pub fn quantize(row: &[f32], floor: f32, ceil: f32) -> Vec<u8> {
    let span = (ceil - floor).max(1.0);
    row.iter()
        .map(|db| (((db - floor) / span) * 255.0).clamp(0.0, 255.0) as u8)
        .collect()
}

/// Reduces `row` to `width` bins by taking the max of each group (peaks stay visible).
pub fn decimate_max(row: &[f32], width: usize) -> Vec<f32> {
    if width == 0 || row.len() <= width {
        return row.to_vec();
    }
    (0..width)
        .map(|i| {
            let (a, b) = (
                i * row.len() / width,
                ((i + 1) * row.len() / width).max(i * row.len() / width + 1),
            );
            row[a..b].iter().copied().fold(f32::NEG_INFINITY, f32::max)
        })
        .collect()
}

/// HUD palette: obsidian -> deep blue -> phosphor cyan -> amber -> crimson.
pub fn colormap(level: u8) -> [u8; 3] {
    const STOPS: [(f32, [f32; 3]); 5] = [
        (0.0, [8.0, 10.0, 14.0]),
        (0.35, [10.0, 40.0, 96.0]),
        (0.62, [0.0, 229.0, 255.0]),
        (0.82, [255.0, 176.0, 0.0]),
        (1.0, [255.0, 40.0, 70.0]),
    ];
    let x = f32::from(level) / 255.0;
    for w in STOPS.windows(2) {
        let ((x0, c0), (x1, c1)) = (w[0], w[1]);
        if x <= x1 {
            let t = ((x - x0) / (x1 - x0)).clamp(0.0, 1.0);
            return [0, 1, 2].map(|i| (c0[i] + (c1[i] - c0[i]) * t) as u8);
        }
    }
    [255, 40, 70]
}

#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub struct Peak {
    pub freq_hz: f64,
    pub power_db: f32,
    pub snr_db: f32,
}

/// Peaks at least `snr_db` over the median, merged when closer than `min_gap` bins.
pub fn find_peaks(
    row: &[f32],
    center_hz: u64,
    rate: u32,
    snr_db: f32,
    min_gap: usize,
) -> Vec<Peak> {
    let floor = median(row);
    let mut peaks: Vec<(usize, f32)> = Vec::new();
    for i in 1..row.len().saturating_sub(1) {
        let v = row[i];
        if v - floor >= snr_db && v >= row[i - 1] && v >= row[i + 1] {
            match peaks.last_mut() {
                Some(last) if i - last.0 < min_gap => {
                    if v > last.1 {
                        *last = (i, v);
                    }
                }
                _ => peaks.push((i, v)),
            }
        }
    }
    let mut out: Vec<Peak> = peaks
        .into_iter()
        .map(|(i, v)| Peak {
            freq_hz: bin_freq(center_hz, rate, row.len(), i),
            power_db: v,
            snr_db: v - floor,
        })
        .collect();
    out.sort_by(|a, b| b.power_db.total_cmp(&a.power_db));
    out
}

/// Hops across `start..=end` in `step` increments, averaging a few FFTs per hop.
/// `stop()` aborts between hops (the kill switch).
pub fn scan_band(
    src: &mut dyn IqSource,
    start_hz: u64,
    end_hz: u64,
    step_hz: u64,
    fft_size: usize,
    stop: &dyn Fn() -> bool,
) -> Result<Vec<Peak>, SdrError> {
    if end_hz < start_hz || step_hz == 0 {
        return Err(SdrError::Invalid(
            "need start_hz <= end_hz and step_hz > 0".into(),
        ));
    }
    if (end_hz - start_hz) / step_hz > 400 {
        return Err(SdrError::Invalid("at most 400 hops per scan".into()));
    }
    let rate = src.info().sample_rate;
    let mut spec = Spectrum::new(fft_size);
    spec.smoothing = 0.6;
    let mut buf = vec![Complex32::default(); spec.size()];
    let mut found = Vec::new();
    let mut f = start_hz;
    while f <= end_hz {
        if stop() {
            return Err(SdrError::Invalid("scan stopped".into()));
        }
        src.tune(f)?;
        spec.reset();
        let mut row = Vec::new();
        for _ in 0..6 {
            src.read(&mut buf)?;
            row = spec.power_db(&buf);
        }
        // Only the middle of the band a hop covers (the edges roll off on real hardware).
        let n = row.len();
        let keep = (n as f64 * (step_hz as f64 / f64::from(rate)).min(0.8)) as usize;
        let (a, b) = ((n - keep) / 2, (n + keep) / 2);
        let mut slice = vec![f32::NEG_INFINITY; n];
        slice[a..b].copy_from_slice(&row[a..b]);
        let floor = median(&row[a..b]);
        for p in find_peaks(&slice, f, rate, 12.0, (n / 64).max(2)) {
            found.push(Peak {
                snr_db: p.power_db - floor,
                ..p
            });
        }
        f = match f.checked_add(step_hz) {
            Some(n) => n,
            None => break,
        };
    }
    found.sort_by(|a, b| b.power_db.total_cmp(&a.power_db));
    found.truncate(64);
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iq::Synthetic;

    #[test]
    fn spectrum_finds_the_band_plan() {
        let mut src = Synthetic::new(100_000_000, 2_048_000, 30.0);
        let mut spec = Spectrum::new(1024);
        let mut buf = vec![Complex32::default(); 1024];
        let mut row = Vec::new();
        for _ in 0..8 {
            src.read(&mut buf).unwrap();
            row = spec.power_db(&buf);
        }
        let peaks = find_peaks(&row, 100_000_000, 2_048_000, 15.0, 16);
        assert!(
            peaks.iter().any(|p| (p.freq_hz - 100.1e6).abs() < 120e3),
            "{peaks:?}"
        );
        assert!(
            peaks.iter().any(|p| (p.freq_hz - 100.7e6).abs() < 120e3),
            "{peaks:?}"
        );
        let q = quantize(&row, -100.0, 0.0);
        assert_eq!(q.len(), 1024);
        assert_eq!(decimate_max(&row, 256).len(), 256);
        assert_eq!(colormap(0), [8, 10, 14]);
        assert_eq!(colormap(255), [255, 40, 70]);
    }

    #[test]
    fn scan_finds_ism_and_stops() {
        let mut src = Synthetic::new(430_000_000, 2_048_000, 30.0);
        let peaks = scan_band(&mut src, 432_000_000, 436_000_000, 1_000_000, 1024, &|| {
            false
        })
        .unwrap();
        assert!(
            peaks.iter().any(|p| (p.freq_hz - 433.92e6).abs() < 50e3),
            "{peaks:?}"
        );
        assert!(scan_band(&mut src, 1, 0, 1, 1024, &|| false).is_err());
        assert!(scan_band(&mut src, 1, 1_000_000_000, 1, 1024, &|| false).is_err());
        assert!(
            scan_band(&mut src, 432_000_000, 436_000_000, 1_000_000, 1024, &|| {
                true
            })
            .is_err()
        );
    }
}
