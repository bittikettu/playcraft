//! Streaming the player's sound and pictures to a TV: a thread draws the TV picture
//! (`scene`) 30 times a second from the audio the player plays (its capture), and hands both
//! to ffmpeg, which encodes them (`encoder`) into a live HLS stream in a temporary folder.
//! The media server serves that folder; the TV plays it as a live stream.
//!
//! ffmpeg counts video frames and audio samples to time them, so every frame goes out with
//! exactly a frame's worth of audio: silence while nothing plays, and a backlog (the sound
//! card's clock running a little fast) trimmed. Picture and sound stay together however the
//! player pauses, seeks or changes tracks.

pub mod encoder;
pub mod scene;

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, bounded};
use ringbuf::traits::Consumer;

use crate::audio::decoder::SourceFormat;
use crate::audio::resample::Resampler;
use crate::audio::shared::Shared;
use crate::audio::{Capture, CaptureSlot};

use encoder::{FPS, PLAYLIST, RATE};
pub use scene::{Scene, TvInfo, TvVisual};

/// Audio frames (stereo samples) per video frame.
const CHUNK: usize = RATE / FPS;
/// More captured audio than this waiting means the sound card runs fast: drop the excess.
const MAX_BACKLOG: usize = RATE / 2;
/// The stream counts as started once the playlist lists this many segments.
const READY_SEGMENTS: usize = 3;

/// Takes one video frame's audio from `pending` (interleaved stereo): exactly `CHUNK`
/// frames, padded with silence when there is less, after dropping a backlog over
/// `MAX_BACKLOG` frames.
fn take_chunk(pending: &mut Vec<f32>) -> Vec<f32> {
    let backlog = pending.len() / 2;
    if backlog > MAX_BACKLOG {
        pending.drain(..(backlog - CHUNK) * 2);
    }
    let n = (CHUNK * 2).min(pending.len() & !1);
    let mut chunk: Vec<f32> = pending.drain(..n).collect();
    chunk.resize(CHUNK * 2, 0.0);
    chunk
}

fn pcm16(samples: &[f32]) -> Vec<u8> {
    samples
        .iter()
        .flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes())
        .collect()
}

/// Starts a stream: `VideoStream::start`, or a stand-in in tests.
pub type Starter = fn(&str, &str, CaptureSlot, Arc<Shared>, TvInfo) -> VideoStream;

/// A running stream. Dropping it stops it.
pub struct VideoStream {
    dir: PathBuf,
    info: Arc<Mutex<TvInfo>>,
    stop: Arc<AtomicBool>,
    failure: Arc<Mutex<Option<String>>>,
    thread: Option<JoinHandle<()>>,
}

impl VideoStream {
    /// Starts streaming what the player plays (from `capture`), with `ffmpeg` and
    /// `encoder` (`auto` picks one). Finding an encoder and starting ffmpeg happen on the
    /// stream's thread; problems show in `failure`.
    pub fn start(
        ffmpeg: &str,
        encoder: &str,
        capture: CaptureSlot,
        shared: Arc<Shared>,
        info: TvInfo,
    ) -> VideoStream {
        let dir = std::env::temp_dir().join(format!("mp3blaster-rs-tv-{}", std::process::id()));
        let info = Arc::new(Mutex::new(info));
        let stop = Arc::new(AtomicBool::new(false));
        let failure = Arc::new(Mutex::new(None));
        let thread = {
            let (dir, info, stop, failure) =
                (dir.clone(), info.clone(), stop.clone(), failure.clone());
            let (ffmpeg, encoder) = (ffmpeg.to_string(), encoder.to_string());
            std::thread::Builder::new()
                .name("tv-stream".into())
                .spawn(move || {
                    let result = run(&ffmpeg, &encoder, &dir, &capture, &shared, &info, &stop);
                    shared.set_capture(false);
                    if let Err(e) = result {
                        log::warn!("TV stream: {e}");
                        *failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(e);
                    }
                    let _ = std::fs::remove_dir_all(&dir);
                })
                .ok()
        };
        VideoStream {
            dir,
            info,
            stop,
            failure,
            thread,
        }
    }

    /// The folder the stream is written to (served to the TV).
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The stream's playlist, inside `dir`.
    pub fn playlist_name(&self) -> &'static str {
        PLAYLIST
    }

    /// What the picture shows besides the music.
    pub fn set_info(&self, info: TvInfo) {
        *self.info.lock().unwrap_or_else(|e| e.into_inner()) = info;
    }

    /// Whether enough of the stream exists for a TV to start playing it.
    pub fn ready(&self) -> bool {
        std::fs::read_to_string(self.dir.join(PLAYLIST))
            .is_ok_and(|p| encoder::segments(&p) >= READY_SEGMENTS)
    }

    /// Why the stream stopped, if it did.
    pub fn failure(&self) -> Option<String> {
        self.failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Stops the stream, waiting at most `wait` for ffmpeg to finish.
    pub fn stop(mut self, wait: Duration) {
        self.stop.store(true, Ordering::Relaxed);
        match self.thread.take() {
            Some(thread) => {
                let deadline = Instant::now() + wait;
                while !thread.is_finished() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            None => {
                let _ = std::fs::remove_dir_all(&self.dir);
            }
        }
    }

    /// A stand-in for tests: a stream that is already going, without ffmpeg.
    #[cfg(test)]
    pub(crate) fn stand_in(
        _ffmpeg: &str,
        _encoder: &str,
        _capture: CaptureSlot,
        _shared: Arc<Shared>,
        info: TvInfo,
    ) -> VideoStream {
        let dir = std::env::temp_dir().join(format!(
            "mp3blaster-rs-tv-test-{:016x}",
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut playlist = String::from("#EXTM3U\n#EXT-X-TARGETDURATION:1\n");
        for n in 0..READY_SEGMENTS {
            playlist.push_str(&format!("#EXTINF:1.000,\nseg{n:05}.ts\n"));
            std::fs::write(dir.join(format!("seg{n:05}.ts")), [0x47u8; 188]).unwrap();
        }
        std::fs::write(dir.join(PLAYLIST), playlist).unwrap();
        VideoStream {
            dir,
            info: Arc::new(Mutex::new(info)),
            stop: Arc::new(AtomicBool::new(false)),
            failure: Arc::new(Mutex::new(None)),
            thread: None,
        }
    }

    /// What the picture shows now (for tests).
    #[cfg(test)]
    pub(crate) fn info(&self) -> TvInfo {
        self.info.lock().unwrap().clone()
    }

    /// Makes a stand-in fail (for tests).
    #[cfg(test)]
    pub(crate) fn fail(&self, why: &str) {
        *self.failure.lock().unwrap() = Some(why.to_string());
    }
}

impl Drop for VideoStream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Writes what arrives on `items` to `out` until either ends. Returns false on a write
/// error.
fn writer(mut out: impl Write, items: &Receiver<Vec<u8>>) -> bool {
    for item in items {
        if out.write_all(&item).is_err() {
            return false;
        }
    }
    let _ = out.flush();
    true
}

/// Gets captured audio, as interleaved stereo at `RATE`.
struct Audio {
    slot: CaptureSlot,
    current: Option<Capture>,
    resampler: Option<Resampler>,
    raw: Vec<f32>,
    converted: Vec<f32>,
}

impl Audio {
    fn new(slot: CaptureSlot) -> Self {
        Audio {
            slot,
            current: None,
            resampler: None,
            raw: vec![0.0; 16384],
            converted: Vec::new(),
        }
    }

    /// Appends what was captured since the last call to `pending`.
    fn drain_into(&mut self, pending: &mut Vec<f32>) {
        // A new output device brings a new capture.
        if let Ok(mut slot) = self.slot.try_lock()
            && let Some(mut fresh) = slot.take()
        {
            fresh.samples.clear();
            self.current = Some(fresh);
            self.resampler = None;
        }
        let Some(capture) = self.current.as_mut() else {
            return;
        };
        let source = SourceFormat {
            sample_rate: capture.rate,
            channels: capture.channels,
        };
        let resampler = self
            .resampler
            .get_or_insert_with(|| Resampler::new(source, RATE as u32, 2));
        loop {
            let n = capture.samples.pop_slice(&mut self.raw);
            let n = n - n % capture.channels.max(1);
            if n == 0 {
                break;
            }
            self.converted.clear();
            resampler.process(&self.raw[..n], &mut self.converted);
            pending.extend_from_slice(&self.converted);
        }
    }

    /// Leaves the capture for the next stream.
    fn give_back(self) {
        if let (Some(capture), Ok(mut slot)) = (self.current, self.slot.lock())
            && slot.is_none()
        {
            *slot = Some(capture);
        }
    }
}

fn run(
    ffmpeg: &str,
    encoder: &str,
    dir: &Path,
    capture: &CaptureSlot,
    shared: &Shared,
    info: &Mutex<TvInfo>,
    stop: &AtomicBool,
) -> Result<(), String> {
    let encoder = encoder::choose(ffmpeg, encoder)?;
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .map_err(|e| format!("cannot open the audio socket: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let mut child = Command::new(ffmpeg)
        .args(encoder::arguments(&encoder, port, dir))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start ffmpeg: {e}"))?;
    log::info!(
        "TV stream: ffmpeg encodes with {encoder} into {}",
        dir.display()
    );

    // ffmpeg's complaints go to the log; the last one explains a failure.
    let last_error = Arc::new(Mutex::new(String::new()));
    if let Some(stderr) = child.stderr.take() {
        let last = last_error.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                log::warn!("ffmpeg: {line}");
                *last.lock().unwrap_or_else(|e| e.into_inner()) = line;
            }
        });
    }
    let result = stream(&mut child, &listener, capture, shared, info, stop);
    // Closing its inputs lets ffmpeg finish; it is stopped if it does not.
    let deadline = Instant::now() + Duration::from_secs(2);
    while child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    result.map_err(|e| {
        let detail = last_error.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if detail.is_empty() {
            e
        } else {
            format!("{e} ({detail})")
        }
    })
}

/// Feeds ffmpeg until `stop`. Video and audio have threads of their own, so a stall on one
/// of ffmpeg's inputs never blocks the other.
fn stream(
    child: &mut Child,
    listener: &TcpListener,
    capture: &CaptureSlot,
    shared: &Shared,
    info: &Mutex<TvInfo>,
    stop: &AtomicBool,
) -> Result<(), String> {
    let stdin = child.stdin.take().ok_or("ffmpeg has no input")?;
    let (video_tx, video_rx): (Sender<Vec<u8>>, _) = bounded(FPS / 2);
    let (audio_tx, audio_rx): (Sender<Vec<u8>>, _) = bounded(FPS * 2);
    let video = std::thread::spawn(move || writer(stdin, &video_rx));
    // ffmpeg connects for the audio after reading the first video frames.
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let audio_socket: Arc<Mutex<Option<TcpStream>>> = Arc::new(Mutex::new(None));
    let audio = {
        let socket = audio_socket.clone();
        let listener = listener.try_clone().map_err(|e| e.to_string())?;
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(20);
            let stream = loop {
                match listener.accept() {
                    Ok((s, _)) => break Some(s),
                    Err(_) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break None,
                }
            };
            let Some(stream) = stream else { return false };
            let _ = stream.set_nonblocking(false);
            let _ = stream.set_nodelay(true);
            *socket.lock().unwrap_or_else(|e| e.into_inner()) = stream.try_clone().ok();
            writer(stream, &audio_rx)
        })
    };

    shared.set_capture(true);
    let mut sound = Audio::new(capture.clone());
    let mut scene = Scene::new();
    let mut pending: Vec<f32> = Vec::new();
    let frame_time = Duration::from_secs_f64(1.0 / FPS as f64);
    let started = Instant::now();
    let mut frames: u32 = 0;
    let mut last_frame: Vec<u8> = Vec::new();
    let result = loop {
        if stop.load(Ordering::Relaxed) {
            break Ok(());
        }
        if video.is_finished() || audio.is_finished() {
            break Err("ffmpeg stopped".to_string());
        }
        sound.drain_into(&mut pending);
        let chunk = take_chunk(&mut pending);
        let mono: Vec<f32> = chunk.chunks(2).map(|f| (f[0] + f[1]) * 0.5).collect();
        let due = started + frame_time * frames;
        // Behind (a slow frame): repeat the last picture instead of drawing, to catch up.
        let behind = Instant::now() > due + frame_time && !last_frame.is_empty();
        if !behind {
            let now_info = info.lock().unwrap_or_else(|e| e.into_inner()).clone();
            last_frame = scene
                .frame(frame_time.as_secs_f32(), &mono, &now_info)
                .to_vec();
        }
        if video_tx.send(last_frame.clone()).is_err() || audio_tx.send(pcm16(&chunk)).is_err() {
            break Err("ffmpeg stopped".to_string());
        }
        frames += 1;
        let next = started + frame_time * frames;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        }
    };
    shared.set_capture(false);
    sound.give_back();
    drop(video_tx);
    drop(audio_tx);
    let _ = video.join();
    // The audio thread may still wait for ffmpeg to connect.
    if let Some(socket) = audio_socket
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    {
        let _ = socket.shutdown(std::net::Shutdown::Both);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_frame_gets_exactly_its_audio() {
        // Short: padded with silence.
        let mut pending = vec![0.5; 1000];
        let chunk = take_chunk(&mut pending);
        assert_eq!(chunk.len(), CHUNK * 2);
        assert_eq!(chunk[999], 0.5);
        assert_eq!(chunk[1000], 0.0);
        assert!(pending.is_empty());
        // Plenty: one frame's worth taken, the rest kept.
        let mut pending = vec![0.25; CHUNK * 2 * 3];
        assert_eq!(take_chunk(&mut pending).len(), CHUNK * 2);
        assert_eq!(pending.len(), CHUNK * 2 * 2);
        // A backlog over half a second is cut down to what this frame needs.
        let mut pending: Vec<f32> = (0..(MAX_BACKLOG + 5000) * 2).map(|i| i as f32).collect();
        let newest = *pending.last().unwrap();
        let chunk = take_chunk(&mut pending);
        assert_eq!(*chunk.last().unwrap(), newest);
        assert!(pending.is_empty());
        // Frames stay whole (left and right together).
        let mut pending = vec![0.1; 7];
        take_chunk(&mut pending);
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn samples_become_16_bit() {
        assert_eq!(
            pcm16(&[0.0, 1.0, -1.0, 2.0]),
            [0, 0, 255, 127, 1, 128, 255, 127]
        );
    }
}
