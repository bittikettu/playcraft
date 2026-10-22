//! Converts decoded audio to the output device's sample rate and channel layout.
//!
//! WASAPI shared mode runs at the device mix rate (usually 48 kHz) while most music is
//! 44.1 kHz, so the stream is opened once at the device format and every track is resampled
//! here. While consecutive tracks share a source format the resampler keeps running, so
//! track boundaries stay sample-continuous (gapless).

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Indexing, Resampler as _};

use super::decoder::SourceFormat;

const CHUNK_FRAMES: usize = 1024;
/// Downmix coefficient for centre/surround channels (-3 dB).
const SIDE_GAIN: f32 = std::f32::consts::FRAC_1_SQRT_2;

pub struct Resampler {
    source: SourceFormat,
    device_rate: u32,
    device_channels: usize,
    /// Channels fed to the resampler: 1 for mono sources, otherwise 2.
    work_channels: usize,
    fft: Option<Fft<f32>>,
    /// Work-channel frames waiting for a full resampler chunk.
    pending: Vec<f32>,
    work: Vec<f32>,
    resampled: Vec<f32>,
    /// Output frames still to drop to compensate for the resampler's delay.
    trim_frames: usize,
    input_frames: u64,
    output_frames: u64,
}

impl Resampler {
    pub fn new(source: SourceFormat, device_rate: u32, device_channels: usize) -> Self {
        let work_channels = if source.channels <= 1 { 1 } else { 2 };
        let fft = if source.sample_rate != device_rate && source.sample_rate > 0 && device_rate > 0
        {
            match Fft::<f32>::new(
                source.sample_rate as usize,
                device_rate as usize,
                CHUNK_FRAMES,
                work_channels,
                FixedSync::Input,
            ) {
                Ok(fft) => Some(fft),
                Err(e) => {
                    log::error!(
                        "cannot resample {} Hz -> {device_rate} Hz: {e}; playing at wrong speed",
                        source.sample_rate
                    );
                    None
                }
            }
        } else {
            None
        };
        let trim_frames = fft.as_ref().map_or(0, |f| f.output_delay());
        let resampled_len = fft
            .as_ref()
            .map_or(0, |f| f.output_frames_max() * work_channels);
        Resampler {
            source,
            device_rate,
            device_channels: device_channels.max(1),
            work_channels,
            fft,
            pending: Vec::with_capacity(CHUNK_FRAMES * 4 * work_channels),
            work: Vec::new(),
            resampled: vec![0.0; resampled_len],
            trim_frames,
            input_frames: 0,
            output_frames: 0,
        }
    }

    /// Whether this resampler can continue seamlessly with audio of `source` format.
    pub fn accepts(&self, source: SourceFormat) -> bool {
        self.source == source
    }

    pub fn source(&self) -> SourceFormat {
        self.source
    }

    /// Output frame index (counted from the last reset) at which the next input frame will
    /// appear. Used to place gapless track boundaries exactly.
    pub fn output_frames_for_input(&self) -> u64 {
        match self.fft {
            Some(_) => {
                let ratio = f64::from(self.device_rate) / f64::from(self.source.sample_rate);
                (self.input_frames as f64 * ratio).round() as u64
            }
            None => self.input_frames,
        }
    }

    /// Drops buffered audio and restarts the filter (after a seek or stop).
    pub fn reset(&mut self) {
        self.pending.clear();
        if let Some(fft) = self.fft.as_mut() {
            fft.reset();
            self.trim_frames = fft.output_delay();
        }
        self.input_frames = 0;
        self.output_frames = 0;
    }

    /// Converts interleaved source samples and appends device-format samples to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let mut work = std::mem::take(&mut self.work);
        work.clear();
        downmix(input, self.source.channels, self.work_channels, &mut work);
        self.input_frames += (work.len() / self.work_channels) as u64;
        if self.fft.is_none() {
            self.emit(&work, out);
        } else {
            self.pending.extend_from_slice(&work);
            self.run_chunks(out, false);
        }
        self.work = work;
    }

    /// Flushes the resampler's tail at the end of the stream (when the next track cannot
    /// continue seamlessly), producing exactly the expected number of output frames.
    pub fn finish(&mut self, out: &mut Vec<f32>) {
        if self.fft.is_some() {
            self.run_chunks(out, true);
        }
        self.reset();
    }

    fn run_chunks(&mut self, out: &mut Vec<f32>, flush: bool) {
        let ch = self.work_channels;
        let ratio = f64::from(self.device_rate) / f64::from(self.source.sample_rate);
        let expected_total = (self.input_frames as f64 * ratio).round() as u64;
        loop {
            let Some(fft) = self.fft.as_mut() else { return };
            let need = fft.input_frames_next();
            let have = self.pending.len() / ch;
            if have < need && !(flush && self.output_frames < expected_total) {
                return;
            }
            let take = have.min(need);
            if take < need {
                // Zero-pad the final partial chunk.
                self.pending.resize(need * ch, 0.0);
            }
            let out_capacity = self.resampled.len() / ch;
            let indexing = Indexing::new().partial_len(take);
            let result = {
                let input = InterleavedSlice::new(&self.pending[..need * ch], ch, need);
                let output = InterleavedSlice::new_mut(&mut self.resampled[..], ch, out_capacity);
                match (input, output) {
                    (Ok(input), Ok(mut output)) => {
                        fft.process_into_buffer(&input, &mut output, Some(&indexing))
                    }
                    _ => {
                        log::error!("resampler buffer size mismatch");
                        self.pending.clear();
                        return;
                    }
                }
            };
            self.pending.drain(..need * ch);
            let (_, written) = match result {
                Ok(r) => r,
                Err(e) => {
                    log::error!("resampler error: {e}");
                    return;
                }
            };
            let mut frames = &self.resampled[..written * ch];
            let skip = self.trim_frames.min(written);
            self.trim_frames -= skip;
            frames = &frames[skip * ch..];
            if flush {
                let remaining = expected_total.saturating_sub(self.output_frames) as usize;
                frames = &frames[..(frames.len() / ch).min(remaining) * ch];
            }
            self.output_frames += (frames.len() / ch) as u64;
            let frames = frames.to_vec();
            self.emit(&frames, out);
            if flush && self.output_frames >= expected_total && self.pending.is_empty() {
                return;
            }
        }
    }

    /// Maps work-channel frames to the device channel layout.
    fn emit(&self, frames: &[f32], out: &mut Vec<f32>) {
        let dev = self.device_channels;
        let ch = self.work_channels;
        out.reserve(frames.len() / ch * dev);
        for frame in frames.chunks_exact(ch) {
            let (l, r) = if ch == 1 {
                (frame[0], frame[0])
            } else {
                (frame[0], frame[1])
            };
            if dev == 1 {
                out.push((l + r) * 0.5);
            } else {
                out.push(l);
                out.push(r);
                out.extend(std::iter::repeat_n(0.0, dev - 2));
            }
        }
    }
}

/// Reduces `channels`-channel interleaved audio to `work_channels` (1 or 2).
fn downmix(input: &[f32], channels: usize, work_channels: usize, out: &mut Vec<f32>) {
    let channels = channels.max(1);
    match (channels, work_channels) {
        (1, _) | (2, 2) => out.extend_from_slice(input),
        (_, 1) => out.extend(
            input
                .chunks_exact(channels)
                .map(|f| f.iter().sum::<f32>() / channels as f32),
        ),
        _ => {
            // Standard order: FL, FR, FC, LFE, (B|S)L, (B|S)R, ... LFE is dropped.
            let norm = 1.0 / (1.0 + 2.0 * SIDE_GAIN);
            for f in input.chunks_exact(channels) {
                let c = if channels > 2 { f[2] * SIDE_GAIN } else { 0.0 };
                let sl = if channels > 4 { f[4] * SIDE_GAIN } else { 0.0 };
                let sr = if channels > 5 { f[5] * SIDE_GAIN } else { 0.0 };
                out.push((f[0] + c + sl) * norm);
                out.push((f[1] + c + sr) * norm);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, channels: usize, secs: f64, freq: f64) -> Vec<f32> {
        let n = (f64::from(rate) * secs) as usize;
        let mut v = Vec::with_capacity(n * channels);
        for i in 0..n {
            let s =
                (2.0 * std::f64::consts::PI * freq * i as f64 / f64::from(rate)).sin() as f32 * 0.5;
            v.extend(std::iter::repeat_n(s, channels));
        }
        v
    }

    /// Dominant frequency of a mono signal via rustfft.
    fn peak_frequency(mono: &[f32], rate: u32) -> f64 {
        use rustfft::{FftPlanner, num_complex::Complex};
        let n = 8192.min(mono.len()).next_power_of_two() / 2;
        let mut buf: Vec<Complex<f32>> = mono[..n].iter().map(|&s| Complex::new(s, 0.0)).collect();
        FftPlanner::new().plan_fft_forward(n).process(&mut buf);
        let (bin, _) = buf[1..n / 2]
            .iter()
            .enumerate()
            .map(|(i, c)| (i + 1, c.norm()))
            .fold((0, 0.0), |best, x| if x.1 > best.1 { x } else { best });
        bin as f64 * f64::from(rate) / n as f64
    }

    #[test]
    fn upsamples_44k_to_48k_with_correct_length_and_pitch() {
        let src = SourceFormat {
            sample_rate: 44_100,
            channels: 2,
        };
        let input = sine(44_100, 2, 2.0, 440.0);
        let mut r = Resampler::new(src, 48_000, 2);
        let mut out = Vec::new();
        for chunk in input.chunks(1152 * 2) {
            r.process(chunk, &mut out);
        }
        r.finish(&mut out);
        assert_eq!(out.len() / 2, 96_000, "output frames");
        let mono: Vec<f32> = out.chunks_exact(2).map(|f| f[0]).collect();
        let f = peak_frequency(&mono[10_000..], 48_000);
        assert!((f - 440.0).abs() < 8.0, "peak at {f} Hz");
    }

    #[test]
    fn same_rate_is_passthrough() {
        let src = SourceFormat {
            sample_rate: 48_000,
            channels: 2,
        };
        let input = sine(48_000, 2, 0.1, 1000.0);
        let mut r = Resampler::new(src, 48_000, 2);
        let mut out = Vec::new();
        r.process(&input, &mut out);
        r.finish(&mut out);
        assert_eq!(out, input);
    }

    #[test]
    fn maps_mono_to_stereo_and_stereo_to_surround() {
        let mut out = Vec::new();
        Resampler::new(
            SourceFormat {
                sample_rate: 48_000,
                channels: 1,
            },
            48_000,
            2,
        )
        .process(&[0.25, -0.5], &mut out);
        assert_eq!(out, vec![0.25, 0.25, -0.5, -0.5]);

        out.clear();
        Resampler::new(
            SourceFormat {
                sample_rate: 48_000,
                channels: 2,
            },
            48_000,
            6,
        )
        .process(&[0.1, 0.2], &mut out);
        assert_eq!(out, vec![0.1, 0.2, 0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn downmixes_surround_without_clipping() {
        let mut out = Vec::new();
        Resampler::new(
            SourceFormat {
                sample_rate: 48_000,
                channels: 6,
            },
            48_000,
            2,
        )
        .process(&[1.0; 6], &mut out);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|&s| s > 0.9 && s <= 1.0), "{out:?}");
    }

    #[test]
    fn continuous_stream_has_no_seam() {
        // Feeding two halves back to back must equal feeding the whole signal at once.
        let src = SourceFormat {
            sample_rate: 44_100,
            channels: 2,
        };
        let input = sine(44_100, 2, 1.0, 523.25);
        let mut whole = Vec::new();
        let mut r = Resampler::new(src, 48_000, 2);
        r.process(&input, &mut whole);
        r.finish(&mut whole);

        let mut split = Vec::new();
        let mut r = Resampler::new(src, 48_000, 2);
        let (a, b) = input.split_at(12_345 * 2);
        r.process(a, &mut split);
        assert!(r.accepts(src));
        r.process(b, &mut split);
        r.finish(&mut split);
        assert_eq!(whole.len(), split.len());
        let max_diff = whole
            .iter()
            .zip(&split)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0, f32::max);
        assert!(max_diff < 1e-5, "seam difference {max_diff}");
    }
}
