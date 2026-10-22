//! Spectrum analyser for the Now Playing pane, the wide strip and the fullscreen view.
//!
//! Runs on the UI thread (not in the real-time audio callback): it drains the mono tap fed
//! by the renderer, applies a Hann window and an FFT, and folds the bins into log-spaced
//! bands from 40 Hz to 16 kHz. The number of bands follows the display width; bars fall
//! smoothly and leave slower-falling peak markers.

use std::sync::Arc;
use std::time::Duration;

use ringbuf::HeapCons;
use ringbuf::traits::Consumer;
use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use crate::audio::SPECTRUM_BANDS;

/// 4096 points give ~12 Hz resolution at 48 kHz, enough for many narrow bass bands.
const FFT_SIZE: usize = 4096;
pub const LOW_HZ: f32 = 40.0;
pub const HIGH_HZ: f32 = 16_000.0;
/// Levels below this many dB under full scale show as empty.
const FLOOR_DB: f32 = 60.0;
/// Music has less energy in the treble; tilt the display by this many dB per octave
/// (around 1 kHz) so high bands are not always empty.
const TILT_DB_PER_OCTAVE: f32 = 3.0;
/// How fast bars fall, in full heights per second.
const FALL_PER_SEC: f32 = 1.6;
/// How fast peak markers fall.
const PEAK_FALL_PER_SEC: f32 = 0.35;
pub const MIN_BANDS: usize = 4;
pub const MAX_BANDS: usize = 256;

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
    /// Normalised amplitude per FFT bin (0..FFT_SIZE/2).
    mags: Vec<f32>,
    drain: Vec<f32>,
    bands: Vec<f32>,
    peaks: Vec<f32>,
}

impl Default for SpectrumAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

/// Lower and upper frequency of band `k` of `n`.
pub fn band_range_hz(k: usize, n: usize) -> (f32, f32) {
    let ratio = HIGH_HZ / LOW_HZ;
    let f = |i: usize| LOW_HZ * ratio.powf(i as f32 / n as f32);
    (f(k), f(k + 1))
}

/// Position of `hz` on the band axis, in bands (0.0 = left edge of band 0).
pub fn band_position(hz: f32, n: usize) -> f32 {
    (hz / LOW_HZ).ln() / (HIGH_HZ / LOW_HZ).ln() * n as f32
}

impl SpectrumAnalyzer {
    pub fn new() -> Self {
        let window = (0..FFT_SIZE)
            .map(|i| {
                0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (FFT_SIZE - 1) as f32).cos()
            })
            .collect();
        SpectrumAnalyzer {
            tap: None,
            sample_rate: 48_000,
            history: vec![0.0; FFT_SIZE],
            write: 0,
            filled: 0,
            fresh: false,
            fft: FftPlanner::new().plan_fft_forward(FFT_SIZE),
            window,
            scratch: vec![Complex::default(); FFT_SIZE],
            mags: vec![0.0; FFT_SIZE / 2],
            drain: vec![0.0; 4096],
            bands: vec![0.0; SPECTRUM_BANDS],
            peaks: vec![0.0; SPECTRUM_BANDS],
        }
    }

    /// Starts reading from a new audio stream's tap.
    pub fn set_tap(&mut self, tap: HeapCons<f32>, sample_rate: u32) {
        self.tap = Some(tap);
        if sample_rate > 0 {
            self.sample_rate = sample_rate;
        }
    }

    /// Changes how many bands are shown. Current levels are carried over so a resize does
    /// not make the bars jump.
    pub fn set_band_count(&mut self, n: usize) {
        let n = n.clamp(MIN_BANDS, MAX_BANDS);
        if n == self.bands.len() {
            return;
        }
        let resample =
            |old: &[f32]| -> Vec<f32> { (0..n).map(|i| old[i * old.len() / n]).collect() };
        self.bands = resample(&self.bands);
        self.peaks = resample(&self.peaks);
    }

    pub fn band_count(&self) -> usize {
        self.bands.len()
    }

    /// Current bar heights, 0.0–1.0, from low to high frequency.
    pub fn bands(&self) -> &[f32] {
        &self.bands
    }

    /// Peak markers (always at or above the bars).
    pub fn peaks(&self) -> &[f32] {
        &self.peaks
    }

    /// The display folded into the spec's 16 telemetry bins.
    pub fn bins16(&self) -> [f32; SPECTRUM_BANDS] {
        let n = self.bands.len();
        let mut out = [0.0; SPECTRUM_BANDS];
        for (k, bin) in out.iter_mut().enumerate() {
            let lo = k * n / SPECTRUM_BANDS;
            let hi = ((k + 1) * n / SPECTRUM_BANDS).max(lo + 1).min(n);
            *bin = self.bands[lo..hi].iter().copied().fold(0.0, f32::max);
        }
        out
    }

    /// Whether any bar or peak is still visible (the display keeps animating while they fall).
    pub fn is_active(&self) -> bool {
        self.bands.iter().chain(&self.peaks).any(|&b| b > 0.0)
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
        let peak_fall = PEAK_FALL_PER_SEC * dt.as_secs_f32();
        let fresh = std::mem::take(&mut self.fresh);
        let levels = (fresh && self.filled == FFT_SIZE).then(|| self.analyse());
        for (k, band) in self.bands.iter_mut().enumerate() {
            let target = levels.as_ref().map_or(0.0, |l| l[k]);
            *band = target.max(*band - fall).max(0.0);
            let peak = &mut self.peaks[k];
            *peak = band.max(*peak - peak_fall).max(0.0);
        }
    }

    /// Band levels (0.0–1.0) of the most recent `FFT_SIZE` samples.
    fn analyse(&mut self) -> Vec<f32> {
        for i in 0..FFT_SIZE {
            let s = self.history[(self.write + i) % FFT_SIZE];
            self.scratch[i] = Complex::new(s * self.window[i], 0.0);
        }
        self.fft.process(&mut self.scratch);
        // A full-scale sine through a Hann window peaks at N/4.
        let norm = 4.0 / FFT_SIZE as f32;
        for (m, c) in self.mags.iter_mut().zip(&self.scratch) {
            *m = c.norm() * norm;
        }
        let bin_hz = self.sample_rate as f32 / FFT_SIZE as f32;
        let n = self.bands.len();
        let last = self.mags.len() - 1;
        (0..n)
            .map(|k| {
                let (lo_hz, hi_hz) = band_range_hz(k, n);
                let (lo, hi) = (lo_hz / bin_hz, hi_hz / bin_hz);
                let (first, end) = (lo.ceil() as usize, (hi.floor() as usize).min(last));
                let amp = if first <= end {
                    self.mags[first..=end].iter().copied().fold(0.0, f32::max)
                } else {
                    // Band narrower than one FFT bin (deep bass): interpolate at its centre.
                    let x = ((lo + hi) / 2.0).min(last as f32);
                    let i = x.floor() as usize;
                    let t = x - i as f32;
                    self.mags[i] * (1.0 - t) + self.mags[(i + 1).min(last)] * t
                };
                let center = (lo_hz * hi_hz).sqrt();
                let tilt = TILT_DB_PER_OCTAVE * (center / 1000.0).log2();
                let db = 20.0 * amp.max(1e-9).log10() + tilt;
                ((db + FLOOR_DB) / FLOOR_DB).clamp(0.0, 1.0)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, rate: u32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| 0.5 * (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin())
            .collect()
    }

    fn loudest(levels: &[f32]) -> usize {
        (0..levels.len())
            .max_by(|&x, &y| levels[x].total_cmp(&levels[y]))
            .unwrap()
    }

    #[test]
    fn sine_lights_up_its_own_band_at_any_width() {
        for n in [16, 40, 96] {
            for k in [n / 6, n / 2, n * 5 / 6] {
                let (lo, hi) = band_range_hz(k, n);
                let mut a = SpectrumAnalyzer::new();
                a.set_band_count(n);
                a.push_samples(&sine((lo * hi).sqrt(), 48_000, FFT_SIZE));
                let levels = a.analyse();
                // Bands narrower than an FFT bin (deep bass at high band counts) can only
                // be resolved to a neighbour.
                let narrow = (hi - lo) < 48_000.0 / FFT_SIZE as f32;
                let tolerance = if narrow { 1 } else { 0 };
                assert!(
                    loudest(&levels).abs_diff(k) <= tolerance,
                    "n={n} k={k}: loudest {} in {levels:?}",
                    loudest(&levels)
                );
                assert!(levels[k] > 0.6, "n={n} k={k}: {}", levels[k]);
            }
        }
    }

    #[test]
    fn silence_is_empty_and_bars_fall_faster_than_peaks() {
        let mut a = SpectrumAnalyzer::new();
        a.push_samples(&vec![0.0; FFT_SIZE]);
        assert!(a.analyse().iter().all(|&l| l == 0.0));
        a.bands = vec![1.0; SPECTRUM_BANDS];
        a.peaks = vec![1.0; SPECTRUM_BANDS];
        a.update(Duration::from_millis(250));
        assert!(a.bands.iter().all(|&b| (b - 0.6).abs() < 1e-3));
        assert!(a.peaks.iter().all(|&p| p > 0.9 && p < 1.0));
        a.update(Duration::from_secs(5));
        assert!(!a.is_active());
    }

    #[test]
    fn band_count_changes_keep_levels() {
        let mut a = SpectrumAnalyzer::new();
        a.bands = (0..16).map(|i| i as f32 / 16.0).collect();
        a.set_band_count(64);
        assert_eq!(a.band_count(), 64);
        assert!(a.bands.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(a.bins16()[15], a.bands[63]);
        a.set_band_count(1);
        assert_eq!(a.band_count(), MIN_BANDS);
    }

    #[test]
    fn band_axis_positions() {
        assert!((band_position(LOW_HZ, 32) - 0.0).abs() < 1e-4);
        assert!((band_position(HIGH_HZ, 32) - 32.0).abs() < 1e-3);
        let (lo, _) = band_range_hz(10, 32);
        assert!((band_position(lo, 32) - 10.0).abs() < 1e-3);
    }
}
