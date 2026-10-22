//! Shared helpers for integration tests: fixtures and a device-less audio harness that pulls
//! audio from the `Renderer` exactly like the device callback would.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, unbounded};
use mp3blaster_rs::audio::controller::ManualBackend;
use mp3blaster_rs::audio::decoder::Decoder;
use mp3blaster_rs::audio::output::DeviceFormat;
use mp3blaster_rs::audio::render::Renderer;
use mp3blaster_rs::audio::{AudioConfig, AudioEvent, AudioHandle};

/// Frames the renderer fades in over at stream start / after a flush.
pub const FADE_FRAMES: usize = 240;
pub const CHUNK: usize = 512;

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

pub fn decode_all(path: &Path) -> Vec<f32> {
    let mut dec = Decoder::open(path).unwrap();
    let mut out = Vec::new();
    while let Some(chunk) = dec.next_chunk().unwrap() {
        out.extend_from_slice(chunk);
    }
    out
}

pub struct Harness {
    pub handle: AudioHandle,
    pub renderer: Renderer,
    pub events: Vec<AudioEvent>,
}

impl Harness {
    pub fn new(rate: u32) -> Self {
        Self::with_config(
            rate,
            AudioConfig {
                volume: 1.0,
                ..AudioConfig::default()
            },
        )
    }

    pub fn with_config(rate: u32, config: AudioConfig) -> Self {
        let (tx, rx) = unbounded();
        let backend = ManualBackend {
            format: DeviceFormat {
                sample_rate: rate,
                channels: 2,
            },
            renderers: tx,
        };
        let handle = AudioHandle::spawn_with_backend(config, Box::new(backend));
        let renderer = rx.recv_timeout(Duration::from_secs(5)).expect("renderer");
        Harness {
            handle,
            renderer,
            events: Vec::new(),
        }
    }

    pub fn drain_events(&mut self) {
        let rx: &Receiver<AudioEvent> = self.handle.events();
        while let Ok(ev) = rx.try_recv() {
            self.events.push(ev);
        }
    }

    pub fn finished(&self) -> bool {
        self.events
            .iter()
            .any(|e| matches!(e, AudioEvent::Finished))
    }

    /// Pulls audio like a device would, but only when a full chunk is buffered so that the
    /// test never causes underruns. Stops when playback has finished and the ring is empty.
    pub fn pull_until_finished(&mut self, timeout: Duration) -> Vec<f32> {
        let deadline = Instant::now() + timeout;
        let mut out = Vec::new();
        let mut buf = vec![0.0; CHUNK * 2];
        loop {
            self.drain_events();
            let buffered = self.renderer.buffered();
            if buffered >= buf.len() {
                self.renderer.fill(&mut buf);
                out.extend_from_slice(&buf);
            } else if buffered > 0 && self.eof_pending() {
                let mut tail = vec![0.0; buffered];
                self.renderer.fill(&mut tail);
                out.extend_from_slice(&tail);
            } else if self.finished() && buffered == 0 {
                return out;
            } else {
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(
                Instant::now() < deadline,
                "timed out; events: {:?}",
                self.events
            );
        }
    }

    /// True once the controller can produce no more audio (all of it is in the ring).
    pub fn eof_pending(&self) -> bool {
        // The controller only emits Finished after the ring drains, so poll its frame counters:
        // nothing is being written anymore when the ring stays below a chunk for a while.
        std::thread::sleep(Duration::from_millis(30));
        self.renderer.buffered() < CHUNK * 2
    }

    pub fn wait_for(&mut self, what: &str, pred: impl Fn(&AudioEvent) -> bool) -> AudioEvent {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut buf = vec![0.0; 64];
        loop {
            self.drain_events();
            if let Some(ev) = self.events.iter().find(|e| pred(e)) {
                return ev.clone();
            }
            // Keep the "device" running so flushes and announcements can progress.
            self.renderer.fill(&mut buf);
            std::thread::sleep(Duration::from_millis(1));
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; events: {:?}",
                self.events
            );
        }
    }
}
