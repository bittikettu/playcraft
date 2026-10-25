//! The body of the real-time audio callback.
//!
//! `Renderer::fill` must never block: no allocation, locks or syscalls. It pulls device-format
//! samples from the ring buffer, applies volume and pause/flush fades, feeds the spectrum tap
//! (and, while a TV stream wants it, the capture) and counts played frames.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use ringbuf::traits::{Consumer, Observer, Producer};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use super::shared::Shared;

/// Pause, resume and seek fade length: 1 / FADE_FRAMES per frame, ~5 ms at 48 kHz.
const FADE_FRAMES: f32 = 240.0;
/// One-pole smoothing coefficient for volume changes (~10 ms time constant at 48 kHz).
const GAIN_SMOOTHING: f32 = 0.002;
/// Mono samples kept for the spectrum analyser.
pub const TAP_CAPACITY: usize = 8192;

/// The audio as it goes to the device, at full level (before the volume), for streaming it
/// elsewhere. Only filled while `Shared::capturing`.
pub struct Capture {
    /// Interleaved samples.
    pub samples: HeapCons<f32>,
    pub rate: u32,
    pub channels: usize,
}

pub struct Renderer {
    cons: HeapCons<f32>,
    tap: HeapProd<f32>,
    capture: HeapProd<f32>,
    shared: Arc<Shared>,
    channels: usize,
    gain: f32,
    fade: f32,
    acked_flush: u64,
}

/// Creates the decoder→callback ring (`capacity_frames` device frames), the spectrum tap and
/// the capture ring (`capture_frames`). Returns the producer for the controller, the renderer
/// for the callback, and the tap and capture consumers.
pub fn create(
    shared: Arc<Shared>,
    channels: usize,
    capacity_frames: usize,
    capture_frames: usize,
) -> (HeapProd<f32>, Renderer, HeapCons<f32>, HeapCons<f32>) {
    let channels = channels.max(1);
    let (prod, cons) =
        ringbuf::traits::Split::split(HeapRb::<f32>::new(capacity_frames * channels));
    let (tap_prod, tap_cons) = ringbuf::traits::Split::split(HeapRb::<f32>::new(TAP_CAPACITY));
    let (capture_prod, capture_cons) =
        ringbuf::traits::Split::split(HeapRb::<f32>::new((capture_frames * channels).max(1)));
    let acked_flush = shared.flush_request.load(Ordering::Acquire);
    shared.flush_ack.store(acked_flush, Ordering::Release);
    let renderer = Renderer {
        cons,
        tap: tap_prod,
        capture: capture_prod,
        gain: shared.gain(),
        shared,
        channels,
        fade: 0.0,
        acked_flush,
    };
    (prod, renderer, tap_cons, capture_cons)
}

impl Renderer {
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Fills `out` (interleaved, device channel count) with the next audio.
    pub fn fill(&mut self, out: &mut [f32]) {
        let ch = self.channels;
        let paused = self.shared.paused.load(Ordering::Relaxed);
        let request = self.shared.flush_request.load(Ordering::Acquire);
        let flushing = request != self.acked_flush;

        // Once the fade-out has reached silence, carry out a pending flush and/or stay paused.
        if self.fade <= 0.0 && (paused || flushing) {
            if flushing {
                self.cons.clear();
                self.acked_flush = request;
                self.shared.flush_ack.store(request, Ordering::Release);
            }
            if paused {
                out.fill(0.0);
                return;
            }
        }
        let fading_out = paused || request != self.acked_flush;

        let wanted = out.len() - out.len() % ch;
        let got = self.cons.pop_slice(&mut out[..wanted]);
        let got = got - got % ch;
        out[got..].fill(0.0);
        if got < wanted && !fading_out {
            self.shared.underruns.fetch_add(1, Ordering::Relaxed);
        }

        // Spectrum tap (pre-volume, mono mix). Drops samples when the UI is not draining it.
        let inv_ch = 1.0 / ch as f32;
        self.tap.push_iter(
            out[..got]
                .chunks_exact(ch)
                .map(|f| f.iter().sum::<f32>() * inv_ch),
        );

        let target_gain = self.shared.gain();
        let target_fade = if fading_out { 0.0 } else { 1.0 };
        let fade_step = 1.0 / FADE_FRAMES;
        // The capture gets the fades but not the volume (the TV has its own).
        let capture = self.shared.capturing();
        for frame in out[..got].chunks_exact_mut(ch) {
            self.gain += (target_gain - self.gain) * GAIN_SMOOTHING;
            self.fade += (target_fade - self.fade).clamp(-fade_step, fade_step);
            let g = self.gain * self.fade;
            for s in frame {
                if capture {
                    let _ = self.capture.try_push(*s * self.fade);
                }
                *s *= g;
            }
        }
        if got == 0 && fading_out {
            // Nothing left to fade: jump straight to silence so the flush/pause can complete.
            self.fade = 0.0;
        }
        self.shared
            .frames_played
            .fetch_add((got / ch) as u64, Ordering::Relaxed);
    }

    /// Samples currently buffered (for tests and diagnostics).
    pub fn buffered(&self) -> usize {
        self.cons.occupied_len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(capacity_frames: usize) -> (HeapProd<f32>, Renderer, HeapCons<f32>, Arc<Shared>) {
        let shared = Arc::new(Shared::default());
        let (prod, r, tap, _capture) = create(shared.clone(), 2, capacity_frames, 1024);
        (prod, r, tap, shared)
    }

    /// Runs the renderer until the fade-in completes so gain checks see full amplitude.
    fn warm_up(prod: &mut HeapProd<f32>, r: &mut Renderer) {
        prod.push_iter(std::iter::repeat_n(0.0, 1024));
        let mut out = vec![0.0; 1024];
        r.fill(&mut out);
    }

    #[test]
    fn underrun_outputs_silence() {
        let (_prod, mut r, _tap, shared) = setup(1024);
        let mut out = vec![1.0; 64];
        r.fill(&mut out);
        assert!(out.iter().all(|&s| s == 0.0));
        assert_eq!(shared.underruns.load(Ordering::Relaxed), 1);
        assert_eq!(shared.frames_played.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn passes_audio_and_counts_frames() {
        let (mut prod, mut r, _tap, shared) = setup(4096);
        warm_up(&mut prod, &mut r);
        prod.push_iter(std::iter::repeat_n(0.5, 200));
        let mut out = vec![0.0; 200];
        r.fill(&mut out);
        assert!(
            out.iter().all(|&s| (s - 0.5).abs() < 1e-6),
            "{:?}",
            &out[..4]
        );
        assert_eq!(shared.frames_played.load(Ordering::Relaxed), 512 + 100);
    }

    #[test]
    fn volume_change_ramps_without_jumps() {
        let (mut prod, mut r, _tap, shared) = setup(65536);
        warm_up(&mut prod, &mut r);
        shared.set_volume(0.0);
        prod.push_iter(std::iter::repeat_n(1.0, 8192));
        let mut out = vec![0.0; 8192];
        r.fill(&mut out);
        let max_step = out
            .windows(2)
            .map(|w| (w[0] - w[1]).abs())
            .fold(0.0, f32::max);
        assert!(max_step < 0.01, "volume jumped by {max_step}");
        assert!(
            out[out.len() - 1] < 0.01,
            "gain did not settle: {}",
            out[out.len() - 1]
        );
    }

    #[test]
    fn pause_fades_out_then_holds_buffer() {
        let (mut prod, mut r, _tap, shared) = setup(65536);
        warm_up(&mut prod, &mut r);
        prod.push_iter(std::iter::repeat_n(1.0, 20000));
        shared.paused.store(true, Ordering::Relaxed);
        let mut out = vec![0.0; 1024];
        r.fill(&mut out);
        assert!(out[0] > 0.9 && out[out.len() - 1] < 0.01, "fade-out shape");
        let before = r.buffered();
        r.fill(&mut out);
        assert!(out.iter().all(|&s| s == 0.0));
        assert_eq!(r.buffered(), before, "paused callback must not consume");
        shared.paused.store(false, Ordering::Relaxed);
        r.fill(&mut out);
        assert!(out[0] < 0.01 && out[out.len() - 1] > 0.9, "fade-in shape");
    }

    #[test]
    fn flush_clears_ring_after_fade_and_acks() {
        let (mut prod, mut r, _tap, shared) = setup(65536);
        warm_up(&mut prod, &mut r);
        prod.push_iter(std::iter::repeat_n(1.0, 20000));
        shared.flush_request.fetch_add(1, Ordering::AcqRel);
        let mut out = vec![0.0; 1024];
        r.fill(&mut out); // fades out
        assert_eq!(shared.flush_ack.load(Ordering::Acquire), 0);
        r.fill(&mut out); // clears + acks
        assert_eq!(shared.flush_ack.load(Ordering::Acquire), 1);
        assert_eq!(r.buffered(), 0);
    }

    #[test]
    fn flush_while_paused_still_completes() {
        let (mut prod, mut r, _tap, shared) = setup(65536);
        warm_up(&mut prod, &mut r);
        shared.paused.store(true, Ordering::Relaxed);
        let mut out = vec![0.0; 1024];
        r.fill(&mut out);
        r.fill(&mut out);
        prod.push_iter(std::iter::repeat_n(1.0, 1000));
        shared.flush_request.fetch_add(1, Ordering::AcqRel);
        r.fill(&mut out);
        assert_eq!(shared.flush_ack.load(Ordering::Acquire), 1);
        assert_eq!(r.buffered(), 0);
    }

    #[test]
    fn capture_gets_full_level_audio_only_when_asked() {
        let shared = Arc::new(Shared::default());
        let (mut prod, mut r, _tap, mut capture) = create(shared.clone(), 2, 65536, 4096);
        warm_up(&mut prod, &mut r);
        let mut out = vec![0.0; 256];
        prod.push_iter(std::iter::repeat_n(0.5, 256));
        r.fill(&mut out);
        assert_eq!(capture.occupied_len(), 0, "off by default");

        shared.set_capture(true);
        // Muted while a TV plays: the device gets silence, the capture the music.
        shared.set_muted(true);
        prod.push_iter(std::iter::repeat_n(0.5, 8192));
        let mut out = vec![0.0; 8192];
        r.fill(&mut out);
        assert!(out[out.len() - 1].abs() < 0.01, "muted output");
        let mut got = vec![0.0; 8192];
        assert_eq!(capture.pop_slice(&mut got), 8192);
        assert!(got.iter().all(|&s| (s - 0.5).abs() < 1e-6));

        // Paused: nothing is played, so nothing is captured.
        shared.paused.store(true, Ordering::Relaxed);
        prod.push_iter(std::iter::repeat_n(0.5, 4096));
        r.fill(&mut out[..1024]);
        capture.clear();
        r.fill(&mut out[..1024]);
        assert_eq!(capture.occupied_len(), 0);
    }

    #[test]
    fn spectrum_tap_receives_mono_mix() {
        let (mut prod, mut r, mut tap, _shared) = setup(4096);
        warm_up(&mut prod, &mut r);
        tap.clear();
        prod.push_iter([0.2, 0.4].into_iter().cycle().take(64));
        let mut out = vec![0.0; 64];
        r.fill(&mut out);
        let mut mono = vec![0.0; 64];
        let n = tap.pop_slice(&mut mono);
        assert_eq!(n, 32);
        assert!(mono[..n].iter().all(|&s| (s - 0.3).abs() < 1e-6));
    }
}
