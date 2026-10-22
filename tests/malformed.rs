//! Spec §8 hardening: malformed, truncated, empty and vanishing files must never crash the
//! decoder, the tag scanner or the playback engine, and playback must move on past them.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{Harness, fixture};
use mp3blaster_rs::audio::decoder::Decoder;
use mp3blaster_rs::audio::{AudioCommand, AudioEvent};
use mp3blaster_rs::library::scanner::read_metadata;

const SOURCES: &[&str] = &[
    "sine_44k_cbr.mp3",
    "sine_44k_vbr.mp3",
    "sine_44k_stereo.flac",
    "sine_44k_stereo.ogg",
    "sine_44k_stereo.m4a",
    "sine_44k_stereo.wav",
    "tagged.mp3",
];

/// Small deterministic generator so failures are reproducible.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

/// Writes corrupted variants of every source fixture into `dir` and returns their paths.
fn corrupted_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut write = |name: String, bytes: &[u8]| {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        out.push(p);
    };
    for (n, src) in SOURCES.iter().enumerate() {
        let data = std::fs::read(fixture(src)).unwrap();
        let ext = Path::new(src).extension().unwrap().to_str().unwrap();
        let stem = Path::new(src).file_stem().unwrap().to_str().unwrap();

        for pct in [3, 40, 97] {
            write(
                format!("{stem}-trunc{pct}.{ext}"),
                &data[..data.len() * pct / 100],
            );
        }
        for seed in 0..12u64 {
            let mut bytes = data.clone();
            let mut rng = Lcg(seed * 7919 + n as u64);
            // Leave the very start intact sometimes so the probe succeeds and the decoder
            // has to cope with damage mid-stream.
            let start = if seed % 2 == 0 {
                64.min(bytes.len())
            } else {
                0
            };
            let flips = 1 + (rng.next() as usize % 200);
            for _ in 0..flips {
                let i = start + rng.next() as usize % (bytes.len() - start).max(1);
                bytes[i] = rng.next() as u8;
            }
            write(format!("{stem}-flip{seed}.{ext}"), &bytes);
        }
        // A run of zeroes in the middle (a classic disk-corruption pattern).
        let mut zeroed = data.clone();
        let (a, b) = (data.len() / 3, data.len() / 2);
        zeroed[a..b].fill(0);
        write(format!("{stem}-zeroed.{ext}"), &zeroed);
    }
    write("empty.mp3".into(), b"");
    write("empty.flac".into(), b"");
    write(
        "notes.mp3".into(),
        b"These are my notes, not music.\n".repeat(50).as_slice(),
    );
    // ID3v2 header claiming a ~256 MB tag (synchsafe 0x7F7F7F7F), then real MP3 data.
    let mut bogus_id3 = b"ID3\x03\x00\x00\x7f\x7f\x7f\x7f".to_vec();
    bogus_id3.extend(std::fs::read(fixture("sine_44k_cbr.mp3")).unwrap());
    write("bogus-id3.mp3".into(), &bogus_id3);
    let mut rng = Lcg(42);
    for ext in ["mp3", "flac", "ogg", "m4a", "wav", "aac"] {
        let noise: Vec<u8> = (0..20_000).map(|_| rng.next() as u8).collect();
        write(format!("noise.{ext}"), &noise);
    }
    out
}

#[test]
fn decoder_survives_every_corrupted_file() {
    let dir = tempfile::tempdir().unwrap();
    let files = corrupted_files(dir.path());
    let mut opened = 0;
    for path in &files {
        let Ok(mut dec) = Decoder::open(path) else {
            continue;
        };
        opened += 1;
        // Decode to the end (bounded, in case a damaged header claims an endless stream).
        for _ in 0..20_000 {
            match dec.next_chunk() {
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break,
            }
        }
        // Seeking in damaged files must not crash either.
        let _ = dec.seek(Duration::from_millis(700));
        for _ in 0..200 {
            if !matches!(dec.next_chunk(), Ok(Some(_))) {
                break;
            }
        }
    }
    assert!(
        opened > files.len() / 3,
        "only {opened}/{} files opened at all",
        files.len()
    );
}

#[test]
fn tag_reader_survives_every_corrupted_file() {
    let dir = tempfile::tempdir().unwrap();
    for path in corrupted_files(dir.path()) {
        let _ = read_metadata(&path);
    }
}

#[test]
fn playback_moves_past_broken_tracks() {
    let dir = tempfile::tempdir().unwrap();
    let broken = dir.path().join("broken.mp3");
    std::fs::write(&broken, b"definitely not audio").unwrap();
    let damaged = dir.path().join("damaged.flac");
    let mut bytes = std::fs::read(fixture("sine_48k_stereo.flac")).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid..mid + 4000].fill(0x55);
    std::fs::write(&damaged, &bytes).unwrap();
    let vanishing = dir.path().join("vanishing.flac");
    std::fs::copy(fixture("sine_48k_stereo.flac"), &vanishing).unwrap();

    let mut h = Harness::new(48_000);
    h.handle.send(AudioCommand::LoadAndPlay(damaged.clone()));
    // The app would react to the TrackFailed events; here the queue is driven by hand:
    // a broken file, then a file deleted after being queued, then a good one.
    h.handle.send(AudioCommand::QueueNext(Some(broken.clone())));
    let ev = h.wait_for(
        "broken track failure",
        |e| matches!(e, AudioEvent::TrackFailed { path, .. } if path == &broken),
    );
    assert!(matches!(ev, AudioEvent::TrackFailed { missing: false, .. }));
    h.handle
        .send(AudioCommand::QueueNext(Some(vanishing.clone())));
    std::fs::remove_file(&vanishing).unwrap();
    h.wait_for(
        "vanished track failure",
        |e| matches!(e, AudioEvent::TrackFailed { path, missing: true, .. } if path == &vanishing),
    );
    h.handle.send(AudioCommand::QueueNext(Some(fixture(
        "sine_48k_stereo.flac",
    ))));
    let out = h.pull_until_finished(Duration::from_secs(30));

    let started: Vec<PathBuf> = h
        .events
        .iter()
        .filter_map(|e| match e {
            AudioEvent::TrackStarted { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(started.first(), Some(&damaged));
    assert_eq!(started.last(), Some(&fixture("sine_48k_stereo.flac")));
    // The damaged file still plays (corrupt frames are skipped) and the good one plays fully.
    let secs = out.len() as f64 / 2.0 / 48_000.0;
    assert!(secs > 2.5, "only {secs:.2} s of audio rendered");
}
