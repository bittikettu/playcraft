//! End-to-end tests of the audio controller without a sound card.

mod common;

use std::time::{Duration, Instant};

use common::{FADE_FRAMES, Harness, decode_all, fixture};
use mp3blaster_rs::audio::{AudioCommand, AudioConfig, AudioEvent};
use mp3blaster_rs::config::ReplayGainMode;
use mp3blaster_rs::model::PlaybackStatus;

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
