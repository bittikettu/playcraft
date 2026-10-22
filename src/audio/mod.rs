//! Audio engine: decoding, resampling, the output ring buffer and device handling.

pub mod controller;
pub mod decoder;
pub mod output;
pub mod render;
pub mod resample;
pub mod shared;

use std::time::Duration;

pub use controller::{AudioCommand, AudioConfig, AudioEvent, AudioHandle, StreamInfo};

use crate::model::PlaybackStatus;

/// Number of spectrum analyser bands shown in the UI.
pub const SPECTRUM_BANDS: usize = 16;

/// A snapshot of playback state for one UI frame (spec §4).
#[derive(Debug, Clone, PartialEq)]
pub struct AudioTelemetry {
    pub status: PlaybackStatus,
    pub position: Duration,
    pub duration: Duration,
    pub current_bitrate: u32,
    /// 16-band equaliser display, each 0.0–1.0.
    pub spectrum_bins: [f32; SPECTRUM_BANDS],
}

impl AudioTelemetry {
    pub fn from_shared(shared: &shared::Shared) -> Self {
        AudioTelemetry {
            status: shared.status(),
            position: shared.position(),
            duration: shared.duration(),
            current_bitrate: 0,
            spectrum_bins: [0.0; SPECTRUM_BANDS],
        }
    }
}
