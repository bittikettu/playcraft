//! 16-band spectrum analyser for the Now Playing pane.
//!
//! Runs on the UI thread (not in the real-time audio callback): it drains the mono tap fed
//! by the renderer, applies a Hann window and an FFT, and folds the bins into log-spaced
//! bands from 40 Hz to 16 kHz with a falling-peak display.

use std::sync::Arc;
use std::time::Duration;

use ringbuf::HeapCons;
use ringbuf::traits::Consumer;
use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use crate::audio::SPECTRUM_BANDS;

const FFT_SIZE: usize = 2048;
const LOW_HZ: f32 = 40.0;
const HIGH_HZ: f32 = 16_000.0;
/// Levels below this many dB under full scale show as empty.
const FLOOR_DB: f32 = 60.0;
/// How fast bars fall, in full heights per second.
const FALL_PER_SEC: f32 = 1.6;

pub struct SpectrumAnalyzer {
    tap: Option<HeapCons<f32>>,
    sample_rate: u32,
    history: Vec<f32>,
    write: usize,
    filled: usize,
    /// New samples arrived since the last analysis.
    fresh: bool,
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    scratch: Vec<Complex<f32>>,
    drain: Vec<f32>,
    edges: [usize; SPECTRUM_BANDS + 1],
    bands: [f32; SPECTRUM_BANDS],
}

impl Default for SpectrumAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl SpectrumAnalyzer {
    pub fn new() -> Self {
        let window = (0..FFT_SIZE)
            .map(|i| {
                0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (FFT_SIZE - 1) as f32).cos()
            })
            .collect();
        let mut s = SpectrumAnalyzer {
            tap: None,
            sample_rate: 48_000,
            history: vec![0.0; FFT_SIZE],
            write: 0,
            filled: 0,
            fresh: false,
            fft: FftPlanner::new().plan_fft_forward(FFT_SIZE),
            window,
            scratch: vec![Complex::default(); FFT_SIZE],
            drain: vec![0.0; 4096],
            edges: [0; SPECTRUM_BANDS + 1],
            bands: [0.0; SPECTRUM_BANDS],
        };
        s.compute_edges();
        s
    }

    /// Starts reading from a new audio stream's tap.
    pub fn set_tap(&mut self, tap: HeapCons<f32>, sample_rate: u32) {
        self.tap = Some(tap);
        if sample_rate > 0 && sample_rate != self.sample_rate {
            self.sample_rate = sample_rate;
            self.compute_edges();
        }
    }

    fn compute_edges(&mut self) {
        let nyquist_bin = FFT_SIZE / 2;
        let ratio = HIGH_HZ / LOW_HZ;
        let mut last = 0;
        for (k, edge) in self.edges.iter_mut().enumerate() {
            let hz = LOW_HZ * ratio.powf(k as f32 / SPECTRUM_BANDS as f32);
            let bin = (hz * FFT_SIZE as f32 / self.sample_rate as f32).round() as usize;
            // Every band gets at least one bin.
            let bin = bin.max(last + 1).min(nyquist_bin);
            *edge = bin;
            last = bin;
        }
    }

    pub fn bands(&self) -> [f32; SPECTRUM_BANDS] {
        self.bands
    }

    /// Whether any bar is still visible (the display keeps animating while bars fall).
    pub fn is_active(&self) -> bool {
        self.bands.iter().any(|&b| b > 0.0)
    }

    /// Pushes samples directly (used by tests and when no tap is attached).
    pub fn push_samples(&mut self, samples: &[f32]) {
        for &s in samples {
            self.history[self.write] = s;
            self.write = (self.write + 1) % FFT_SIZE;
        }
        self.filled = (self.filled + samples.len()).min(FFT_SIZE);
        self.fresh |= !samples.is_empty();
    }

    /// Drains the tap and recomputes the bands. `dt` is the time since the last update.
    pub fn update(&mut self, dt: Duration) {
        if let Some(mut tap) = self.tap.take() {
            let mut drain = std::mem::take(&mut self.drain);
            loop {
                let n = tap.pop_slice(&mut drain);
                if n == 0 {
                    break;
                }
                self.push_samples(&drain[..n]);
            }
            self.drain = drain;
            self.tap = Some(tap);
        }
        let fall = FALL_PER_SEC * dt.as_secs_f32();
        let fresh = std::mem::take(&mut self.fresh);
        if !fresh || self.filled < FFT_SIZE {
            for b in &mut self.bands {
                *b = (*b - fall).max(0.0);
            }
            return;
        }
        let levels = self.analyse();
        for (band, level) in self.bands.iter_mut().zip(levels) {
            *band = level.max(*band - fall);
        }
    }

    /// Band levels (0.0–1.0) of the most recent `FFT_SIZE` samples.
    fn analyse(&mut self) -> [f32; SPECTRUM_BANDS] {
        for i in 0..FFT_SIZE {
            let s = self.history[(self.write + i) % FFT_SIZE];
            self.scratch[i] = Complex::new(s * self.window[i], 0.0);
        }
        self.fft.process(&mut self.scratch);
        // A full-scale sine through a Hann window peaks at N/4.
        let norm = 4.0 / FFT_SIZE as f32;
        let mut out = [0.0; SPECTRUM_BANDS];
        for (k, level) in out.iter_mut().enumerate() {
            let (lo, hi) = (self.edges[k], self.edges[k + 1].max(self.edges[k] + 1));
            let peak = self.scratch[lo..hi.min(FFT_SIZE / 2)]
                .iter()
                .map(|c| c.norm())
                .fold(0.0, f32::max);
            let db = 20.0 * (peak * norm).max(1e-9).log10();
            *level = ((db + FLOOR_DB) / FLOOR_DB).clamp(0.0, 1.0);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn band_center(analyzer: &SpectrumAnalyzer, k: usize) -> f32 {
        let ratio = HIGH_HZ / LOW_HZ;
        let lo = LOW_HZ * ratio.powf(k as f32 / SPECTRUM_BANDS as f32);
        let hi = LOW_HZ * ratio.powf((k + 1) as f32 / SPECTRUM_BANDS as f32);
        let _ = analyzer;
        (lo * hi).sqrt()
    }

    fn sine(freq: f32, rate: u32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| 0.5 * (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin())
            .collect()
    }

    #[test]
    fn sine_lights_up_its_own_band() {
        for k in [3, 8, 12] {
            let mut a = SpectrumAnalyzer::new();
            a.push_samples(&sine(band_center(&a, k), 48_000, FFT_SIZE));
            let levels = a.analyse();
            let loudest = (0..SPECTRUM_BANDS)
                .max_by(|&x, &y| levels[x].total_cmp(&levels[y]))
                .unwrap();
            assert_eq!(loudest, k, "levels {levels:?}");
            assert!(
                levels[k] > 0.8,
                "half-scale sine should be near the top: {}",
                levels[k]
            );
        }
    }

    #[test]
    fn silence_is_empty_and_bars_fall() {
        let mut a = SpectrumAnalyzer::new();
        a.push_samples(&vec![0.0; FFT_SIZE]);
        assert!(a.analyse().iter().all(|&l| l == 0.0));
        a.bands = [1.0; SPECTRUM_BANDS];
        a.update(Duration::from_millis(250));
        assert!(a.bands.iter().all(|&b| (b - 0.6).abs() < 1e-3));
        a.update(Duration::from_secs(1));
        assert!(!a.is_active());
    }

    #[test]
    fn band_edges_are_strictly_increasing() {
        let mut a = SpectrumAnalyzer::new();
        for rate in [22_050, 44_100, 48_000, 96_000, 192_000] {
            a.sample_rate = rate;
            a.compute_edges();
            assert!(
                a.edges.windows(2).all(|w| w[0] < w[1]),
                "{rate}: {:?}",
                a.edges
            );
        }
    }
}
