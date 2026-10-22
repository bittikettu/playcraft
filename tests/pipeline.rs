//! End-to-end tests of the audio controller without a sound card: the test pulls audio from
//! the `Renderer` exactly like the device callback would.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, unbounded};
use mp3blaster_rs::audio::controller::ManualBackend;
use mp3blaster_rs::audio::decoder::Decoder;
use mp3blaster_rs::audio::output::DeviceFormat;
use mp3blaster_rs::audio::render::Renderer;
use mp3blaster_rs::audio::{AudioCommand, AudioConfig, AudioEvent, AudioHandle};
use mp3blaster_rs::config::ReplayGainMode;
use mp3blaster_rs::model::PlaybackStatus;

/// Frames the renderer fades in over at stream start / after a flush.
const FADE_FRAMES: usize = 240;
const CHUNK: usize = 512;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn decode_all(path: &Path) -> Vec<f32> {
    let mut dec = Decoder::open(path).unwrap();
    let mut out = Vec::new();
    while let Some(chunk) = dec.next_chunk().unwrap() {
        out.extend_from_slice(chunk);
    }
    out
}

struct Harness {
    handle: AudioHandle,
    renderer: Renderer,
    events: Vec<AudioEvent>,
}

impl Harness {
    fn new(rate: u32) -> Self {
        Self::with_config(
            rate,
            AudioConfig {
                volume: 1.0,
                ..AudioConfig::default()
            },
        )
    }

    fn with_config(rate: u32, config: AudioConfig) -> Self {
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

    fn drain_events(&mut self) {
        let rx: &Receiver<AudioEvent> = self.handle.events();
        while let Ok(ev) = rx.try_recv() {
            self.events.push(ev);
        }
    }

    fn finished(&self) -> bool {
        self.events
            .iter()
            .any(|e| matches!(e, AudioEvent::Finished))
    }

    /// Pulls audio like a device would, but only when a full chunk is buffered so that the
    /// test never causes underruns. Stops when playback has finished and the ring is empty.
    fn pull_until_finished(&mut self, timeout: Duration) -> Vec<f32> {
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
    fn eof_pending(&self) -> bool {
        // The controller only emits Finished after the ring drains, so poll its frame counters:
        // nothing is being written anymore when the ring stays below a chunk for a while.
        std::thread::sleep(Duration::from_millis(30));
        self.renderer.buffered() < CHUNK * 2
    }

    fn wait_for(&mut self, what: &str, pred: impl Fn(&AudioEvent) -> bool) -> AudioEvent {
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

#[test]
fn gapless_pair_renders_identically_to_the_whole_file() {
    let full = decode_all(&fixture("chirp_full.flac"));
    let mut h = Harness::new(48_000);
    h.handle
        .send(AudioCommand::LoadAndPlay(fixture("chirp_part1.flac")));
    h.handle
        .send(AudioCommand::QueueNext(Some(fixture("chirp_part2.flac"))));
    let out = h.pull_until_finished(Duration::from_secs(20));

    let started: Vec<_> = h
        .events
        .iter()
        .filter_map(|e| match e {
            AudioEvent::TrackStarted { path, .. } => path.file_name().map(|n| n.to_owned()),
            _ => None,
        })
        .collect();
    assert_eq!(started, vec!["chirp_part1.flac", "chirp_part2.flac"]);

    assert_eq!(
        out.len(),
        full.len(),
        "rendered length differs from the unsplit file"
    );
    let skip = FADE_FRAMES * 2;
    let mismatch = out[skip..]
        .iter()
        .zip(&full[skip..])
        .position(|(a, b)| a != b);
    assert_eq!(
        mismatch, None,
        "rendered audio differs at sample {mismatch:?}"
    );
    assert_eq!(h.handle.shared().status(), PlaybackStatus::Stopped);
}

#[test]
fn replaygain_scales_tagged_tracks() {
    let peak = |mode| {
        let mut h = Harness::with_config(
            48_000,
            AudioConfig {
                volume: 1.0,
                replaygain: mode,
                ..AudioConfig::default()
            },
        );
        h.handle
            .send(AudioCommand::LoadAndPlay(fixture("tagged.flac")));
        let out = h.pull_until_finished(Duration::from_secs(20));
        out.iter().fold(0f32, |m, s| m.max(s.abs()))
    };
    let off = peak(ReplayGainMode::Off);
    let track = peak(ReplayGainMode::Track);
    // The fixture is tagged -6.50 dB with peak 0.5, so the peak limit (x2) does not apply.
    let expected = 10f32.powf(-6.5 / 20.0);
    assert!(
        (track / off - expected).abs() < 0.01,
        "gain ratio {} (expected {expected})",
        track / off
    );
}

#[test]
fn resampled_playback_has_expected_length() {
    // 2 s at 44.1 kHz rendered on a 48 kHz device.
    let mut h = Harness::new(48_000);
    h.handle
        .send(AudioCommand::LoadAndPlay(fixture("sine_44k_stereo.flac")));
    let out = h.pull_until_finished(Duration::from_secs(20));
    let frames = out.len() / 2;
    assert!(
        (frames as i64 - 96_000).abs() <= 2,
        "rendered {frames} frames"
    );
}

#[test]
fn seek_moves_position_and_shortens_remaining_audio() {
    let mut h = Harness::new(48_000);
    h.handle
        .send(AudioCommand::LoadAndPlay(fixture("sine_48k_stereo.flac")));
    h.wait_for("track start", |e| {
        matches!(e, AudioEvent::TrackStarted { .. })
    });
    h.handle
        .send(AudioCommand::SeekAbsolute(Duration::from_millis(1500)));
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut buf = vec![0.0; 64];
    while h.handle.shared().position() < Duration::from_millis(1500) {
        h.renderer.fill(&mut buf);
        std::thread::sleep(Duration::from_millis(1));
        assert!(
            Instant::now() < deadline,
            "position never reached the seek target"
        );
    }
    let out = h.pull_until_finished(Duration::from_secs(20));
    let secs = out.len() as f64 / 2.0 / 48_000.0;
    assert!(
        (secs - 0.5).abs() < 0.05,
        "{secs:.3} s left after seeking to 1.5 s"
    );
    // Seeking within a track must not announce it again.
    let starts = h
        .events
        .iter()
        .filter(|e| matches!(e, AudioEvent::TrackStarted { .. }))
        .count();
    assert_eq!(starts, 1);
}

#[test]
fn missing_file_reports_failure_without_panicking() {
    let mut h = Harness::new(48_000);
    h.handle
        .send(AudioCommand::LoadAndPlay(fixture("no-such-file.mp3")));
    let ev = h.wait_for("failure", |e| matches!(e, AudioEvent::TrackFailed { .. }));
    match ev {
        AudioEvent::TrackFailed { missing, .. } => assert!(missing),
        _ => unreachable!(),
    }
    assert_eq!(h.handle.shared().status(), PlaybackStatus::Stopped);
}

#[test]
fn failed_queued_track_is_reported_and_playback_finishes() {
    let mut h = Harness::new(48_000);
    h.handle
        .send(AudioCommand::LoadAndPlay(fixture("sine_48k_stereo.flac")));
    h.handle
        .send(AudioCommand::QueueNext(Some(fixture("gone.flac"))));
    let out = h.pull_until_finished(Duration::from_secs(20));
    assert!(
        h.events
            .iter()
            .any(|e| matches!(e, AudioEvent::TrackFailed { missing: true, .. }))
    );
    let secs = out.len() as f64 / 2.0 / 48_000.0;
    assert!((secs - 2.0).abs() < 0.05, "{secs:.3} s rendered");
}

#[test]
fn pause_and_stop_update_status() {
    let mut h = Harness::new(48_000);
    h.handle
        .send(AudioCommand::LoadAndPlay(fixture("sine_48k_stereo.flac")));
    h.wait_for("track start", |e| {
        matches!(e, AudioEvent::TrackStarted { .. })
    });
    h.handle.send(AudioCommand::Pause);
    let deadline = Instant::now() + Duration::from_secs(2);
    while h.handle.shared().status() != PlaybackStatus::Paused {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    h.handle.send(AudioCommand::Resume);
    h.handle.send(AudioCommand::Stop);
    let mut buf = vec![0.0; 64];
    while h.handle.shared().status() != PlaybackStatus::Stopped {
        h.renderer.fill(&mut buf);
        assert!(Instant::now() < deadline + Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(h.handle.shared().position(), Duration::ZERO);
}
