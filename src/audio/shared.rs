//! Lock-free state shared between the controller thread, the audio callback and the UI.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use crate::model::PlaybackStatus;

/// Lowest audible volume step in dB; volume 0.0 is a hard mute.
const VOLUME_RANGE_DB: f32 = 60.0;

/// Maps a 0.0–1.0 volume slider position to a linear gain on a logarithmic (dB) curve.
pub fn volume_to_gain(volume: f32) -> f32 {
    let v = volume.clamp(0.0, 1.0);
    if v <= 0.0 {
        0.0
    } else {
        10f32.powf(VOLUME_RANGE_DB * (v - 1.0) / 20.0)
    }
}

#[derive(Debug)]
pub struct Shared {
    // Written by the controller, read by the callback.
    pub paused: AtomicBool,
    gain_bits: AtomicU32,
    /// Silences the output (while a network speaker plays) without changing the volume.
    muted: AtomicBool,
    pub flush_request: AtomicU64,

    // Written by the callback.
    pub flush_ack: AtomicU64,
    /// Device frames taken from the ring buffer (i.e. audio actually handed to the device).
    pub frames_played: AtomicU64,
    pub underruns: AtomicU64,

    // Published by the controller for the UI.
    status: AtomicU8,
    position_ms: AtomicU64,
    duration_ms: AtomicU64,
    volume_bits: AtomicU32,
    device_rate: AtomicU32,
}

impl Default for Shared {
    fn default() -> Self {
        Shared {
            paused: AtomicBool::new(false),
            gain_bits: AtomicU32::new(1f32.to_bits()),
            muted: AtomicBool::new(false),
            flush_request: AtomicU64::new(0),
            flush_ack: AtomicU64::new(0),
            frames_played: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            status: AtomicU8::new(0),
            position_ms: AtomicU64::new(0),
            duration_ms: AtomicU64::new(0),
            volume_bits: AtomicU32::new(1f32.to_bits()),
            device_rate: AtomicU32::new(0),
        }
    }
}

impl Shared {
    pub fn gain(&self) -> f32 {
        if self.muted() {
            0.0
        } else {
            f32::from_bits(self.gain_bits.load(Ordering::Relaxed))
        }
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    pub fn muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    /// Sets the volume (0.0–1.0). The callback ramps to the matching gain.
    pub fn set_volume(&self, volume: f32) {
        let volume = volume.clamp(0.0, 1.0);
        self.volume_bits.store(volume.to_bits(), Ordering::Relaxed);
        self.gain_bits
            .store(volume_to_gain(volume).to_bits(), Ordering::Relaxed);
    }

    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume_bits.load(Ordering::Relaxed))
    }

    pub fn set_status(&self, status: PlaybackStatus) {
        let v = match status {
            PlaybackStatus::Stopped => 0,
            PlaybackStatus::Playing => 1,
            PlaybackStatus::Paused => 2,
        };
        self.status.store(v, Ordering::Relaxed);
    }

    pub fn status(&self) -> PlaybackStatus {
        match self.status.load(Ordering::Relaxed) {
            1 => PlaybackStatus::Playing,
            2 => PlaybackStatus::Paused,
            _ => PlaybackStatus::Stopped,
        }
    }

    pub fn set_position(&self, position: Duration) {
        self.position_ms
            .store(position.as_millis() as u64, Ordering::Relaxed);
    }

    pub fn position(&self) -> Duration {
        Duration::from_millis(self.position_ms.load(Ordering::Relaxed))
    }

    pub fn set_duration(&self, duration: Duration) {
        self.duration_ms
            .store(duration.as_millis() as u64, Ordering::Relaxed);
    }

    pub fn duration(&self) -> Duration {
        Duration::from_millis(self.duration_ms.load(Ordering::Relaxed))
    }

    pub fn set_device_rate(&self, rate: u32) {
        self.device_rate.store(rate, Ordering::Relaxed);
    }

    /// Output device sample rate, 0 while no device is open.
    pub fn device_rate(&self) -> u32 {
        self.device_rate.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_curve_is_logarithmic() {
        assert_eq!(volume_to_gain(0.0), 0.0);
        assert!((volume_to_gain(1.0) - 1.0).abs() < 1e-6);
        // Half the slider is -30 dB.
        assert!((20.0 * volume_to_gain(0.5).log10() + 30.0).abs() < 1e-3);
        // Monotonic.
        let mut last = -1.0;
        for i in 0..=100 {
            let g = volume_to_gain(i as f32 / 100.0);
            assert!(g > last);
            last = g;
        }
        assert_eq!(volume_to_gain(2.0), 1.0);
    }
}
