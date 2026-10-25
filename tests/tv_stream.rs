//! The TV stream with the real ffmpeg: a tone "played" by a pretend sound card becomes an
//! HLS stream of H.264 video and AAC audio. Skipped where ffmpeg is not installed.

use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use mp3blaster_rs::audio::Capture;
use mp3blaster_rs::audio::shared::Shared;
use mp3blaster_rs::outputs::video::{TvInfo, VideoStream};
use ringbuf::HeapRb;
use ringbuf::traits::{Producer, Split};

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[test]
fn a_tone_becomes_a_live_hls_stream() {
    if !have("ffmpeg") {
        eprintln!("ffmpeg not found: skipped");
        return;
    }
    // A pretend sound card playing a 440 Hz tone at 44.1 kHz (the stream is 48 kHz).
    let shared = Arc::new(Shared::default());
    let (mut prod, cons) = HeapRb::<f32>::new(44_100 * 2).split();
    let slot = Arc::new(std::sync::Mutex::new(Some(Capture {
        samples: cons,
        rate: 44_100,
        channels: 2,
    })));
    let done = Arc::new(AtomicBool::new(false));
    let card = {
        let (shared, done) = (shared.clone(), done.clone());
        std::thread::spawn(move || {
            let mut n = 0u64;
            while !done.load(Ordering::Relaxed) {
                if shared.capturing() {
                    let chunk: Vec<f32> = (0..441)
                        .flat_map(|i| {
                            let t = (n + i) as f32 / 44_100.0;
                            let s = (t * 440.0 * std::f32::consts::TAU).sin() * 0.5;
                            [s, s]
                        })
                        .collect();
                    prod.push_slice(&chunk);
                    n += 441;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        })
    };

    let stream = VideoStream::start(
        "ffmpeg",
        "auto",
        slot.clone(),
        shared.clone(),
        TvInfo::default(),
    );
    let dir = stream.dir().to_path_buf();
    let deadline = Instant::now() + Duration::from_secs(25);
    while !stream.ready() {
        assert!(stream.failure().is_none(), "{:?}", stream.failure());
        assert!(Instant::now() < deadline, "no stream after 25 s");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(shared.capturing());
    let playlist = std::fs::read_to_string(dir.join(stream.playlist_name())).unwrap();
    let segment = playlist
        .lines()
        .find(|l| l.ends_with(".ts"))
        .expect("a segment");
    let bytes = std::fs::read(dir.join(segment)).unwrap();
    assert_eq!(bytes[0], 0x47, "MPEG-TS sync byte");
    assert_eq!(bytes[188], 0x47);

    if have("ffprobe") {
        let out = Command::new("ffprobe")
            .args(["-v", "error", "-show_entries"])
            .arg("stream=codec_name,width,height,sample_rate,channels")
            .args(["-of", "compact"])
            .arg(dir.join(segment))
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("codec_name=h264"), "{text}");
        assert!(
            text.contains("width=1280") && text.contains("height=720"),
            "{text}"
        );
        assert!(text.contains("codec_name=aac"), "{text}");
        assert!(
            text.contains("sample_rate=48000") && text.contains("channels=2"),
            "{text}"
        );
    }

    stream.stop(Duration::from_secs(5));
    assert!(!shared.capturing());
    assert!(!dir.exists(), "the temporary folder is removed");
    assert!(slot.lock().unwrap().is_some(), "the capture is given back");
    done.store(true, Ordering::Relaxed);
    card.join().unwrap();
}

/// Streams for `TV_STREAM_SECS` seconds (to watch CPU use from outside):
/// `TV_STREAM_SECS=20 cargo test --release --test tv_stream long_stream -- --ignored`
#[test]
#[ignore]
fn long_stream() {
    let Ok(secs) = std::env::var("TV_STREAM_SECS") else {
        return;
    };
    let secs: u64 = secs.parse().unwrap();
    let shared = Arc::new(Shared::default());
    let (mut prod, cons) = HeapRb::<f32>::new(48_000 * 2).split();
    let slot = Arc::new(std::sync::Mutex::new(Some(Capture {
        samples: cons,
        rate: 48_000,
        channels: 2,
    })));
    let feeder = {
        let shared = shared.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            let mut n = 0u64;
            while started.elapsed() < Duration::from_secs(secs + 5) {
                if shared.capturing() {
                    let chunk: Vec<f32> = (0..480)
                        .flat_map(|i| {
                            let t = (n + i) as f32 / 48_000.0;
                            let kick = (-(t % 0.5) * 10.0).exp();
                            let s = (t * 60.0 * std::f32::consts::TAU).sin() * (0.2 + 0.6 * kick)
                                + (t * 880.0 * std::f32::consts::TAU).sin() * 0.1;
                            [s, s]
                        })
                        .collect();
                    prod.push_slice(&chunk);
                    n += 480;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        })
    };
    let stream = VideoStream::start("ffmpeg", "auto", slot, shared, TvInfo::default());
    std::thread::sleep(Duration::from_secs(secs));
    assert!(stream.failure().is_none(), "{:?}", stream.failure());
    stream.stop(Duration::from_secs(5));
    feeder.join().unwrap();
}
