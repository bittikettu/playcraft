//! The audio controller thread.
//!
//! Owns the decoders, resampler, ring-buffer producer and output stream. It turns
//! `AudioCommand`s into playback, keeps the ring buffer topped up, handles gapless
//! transitions and device loss, and reports `AudioEvent`s plus lock-free position state.

use std::any::Any;
use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, select, unbounded};
use ringbuf::traits::{Observer, Producer};
use ringbuf::{HeapCons, HeapProd};

use super::decoder::{Decoder, DecoderError};
use super::output::{self, DeviceFormat, OutputError, StreamFault};
use super::render::{self, Capture, Renderer};
use super::resample::Resampler;
use super::shared::Shared;
use crate::config::ReplayGainMode;
use crate::model::PlaybackStatus;

/// Open the next queued track this long before the current one ends (spec §6.2).
const PREBUFFER: Duration = Duration::from_secs(2);
/// How long to wait for the audio callback to acknowledge a flush.
const FLUSH_TIMEOUT: Duration = Duration::from_millis(250);
/// Retry interval while no output device is available.
const DEVICE_RETRY: Duration = Duration::from_secs(1);

/// Commands sent from the UI thread to the audio controller.
#[derive(Debug, Clone)]
pub enum AudioCommand {
    LoadAndPlay(PathBuf),
    /// The track to continue with, gaplessly, when the current one ends.
    QueueNext(Option<PathBuf>),
    Pause,
    Resume,
    Stop,
    /// Seconds (+/-).
    SeekRelative(i64),
    SeekAbsolute(Duration),
    /// 0.0 to 1.0.
    SetVolume(f32),
    SetReplayGain(ReplayGainMode),
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamInfo {
    pub codec: String,
    pub sample_rate: u32,
    pub channels: usize,
}

/// Discrete events from the controller to the UI.
#[derive(Debug, Clone)]
pub enum AudioEvent {
    /// A track became audible (sent when its first sample reaches the device).
    TrackStarted {
        path: PathBuf,
        duration: Option<Duration>,
        info: StreamInfo,
    },
    /// A track could not be opened or played. `missing` means the file does not exist.
    TrackFailed {
        path: PathBuf,
        error: String,
        missing: bool,
    },
    /// The last track finished and nothing was queued.
    Finished,
    DeviceLost(String),
    DeviceRestored(String),
    Warning(String),
}

#[derive(Debug, Clone)]
pub struct AudioConfig {
    /// Case-insensitive substring of the output device name; `None` follows the system default.
    pub output_device: Option<String>,
    pub buffer_ms: u32,
    pub gapless: bool,
    pub volume: f32,
    pub replaygain: ReplayGainMode,
}

impl Default for AudioConfig {
    fn default() -> Self {
        AudioConfig {
            output_device: None,
            buffer_ms: 150,
            gapless: true,
            volume: 0.8,
            replaygain: ReplayGainMode::Off,
        }
    }
}

/// Linear gain for `path` under `mode`, limited so the tagged peak does not clip.
pub fn replay_gain_factor(path: &std::path::Path, mode: ReplayGainMode) -> f32 {
    if mode == ReplayGainMode::Off {
        return 1.0;
    }
    let rg = crate::library::scanner::read_replay_gain(path);
    let (gain, peak) = match mode {
        ReplayGainMode::Album => (
            rg.album_gain_db.or(rg.track_gain_db),
            rg.album_peak.or(rg.track_peak),
        ),
        _ => (
            rg.track_gain_db.or(rg.album_gain_db),
            rg.track_peak.or(rg.album_peak),
        ),
    };
    let Some(db) = gain else { return 1.0 };
    let linear = 10f32.powf(db / 20.0);
    match peak.filter(|&p| p > 0.0) {
        Some(p) => linear.min(1.0 / p),
        None => linear,
    }
}

/// A started output: the ring producer plus whatever keeps the stream alive.
pub struct OpenedSink {
    pub producer: HeapProd<f32>,
    pub tap: HeapCons<f32>,
    pub capture: Capture,
    pub format: DeviceFormat,
    pub name: String,
    pub keepalive: Box<dyn Any>,
}

/// Something that can open an output for the controller.
pub trait AudioBackend: Send + 'static {
    fn open(
        &mut self,
        shared: &Arc<Shared>,
        buffer_ms: u32,
        faults: Sender<StreamFault>,
    ) -> Result<OpenedSink, OutputError>;
}

fn ring_frames(buffer_ms: u32, rate: u32) -> usize {
    (buffer_ms.max(100) as usize * rate as usize / 1000).max(1024)
}

/// Real output through cpal.
pub struct CpalBackend {
    pub preferred: Option<String>,
}

impl AudioBackend for CpalBackend {
    fn open(
        &mut self,
        shared: &Arc<Shared>,
        buffer_ms: u32,
        faults: Sender<StreamFault>,
    ) -> Result<OpenedSink, OutputError> {
        let choice = match output::choose_device(self.preferred.as_deref()) {
            Err(OutputError::DeviceNotFound(name)) => {
                log::warn!("output device \"{name}\" not found, using the system default");
                output::choose_device(None)?
            }
            other => other?,
        };
        let format = choice.format();
        let (producer, renderer, tap, capture) = render::create(
            shared.clone(),
            format.channels,
            ring_frames(buffer_ms, format.sample_rate),
            format.sample_rate as usize,
        );
        let name = choice.name().to_string();
        let output = choice.start(renderer, faults)?;
        Ok(OpenedSink {
            producer,
            tap,
            capture: Capture {
                samples: capture,
                rate: format.sample_rate,
                channels: format.channels,
            },
            format,
            name,
            keepalive: Box::new(output),
        })
    }
}

/// A device-less backend: each opened `Renderer` is sent to the caller, who pulls audio by
/// calling `Renderer::fill`. Used by tests and for offline rendering.
pub struct ManualBackend {
    pub format: DeviceFormat,
    pub renderers: Sender<Renderer>,
}

impl AudioBackend for ManualBackend {
    fn open(
        &mut self,
        shared: &Arc<Shared>,
        buffer_ms: u32,
        _faults: Sender<StreamFault>,
    ) -> Result<OpenedSink, OutputError> {
        let (producer, renderer, tap, capture) = render::create(
            shared.clone(),
            self.format.channels,
            ring_frames(buffer_ms, self.format.sample_rate),
            self.format.sample_rate as usize,
        );
        self.renderers
            .send(renderer)
            .map_err(|_| OutputError::NoDevice)?;
        Ok(OpenedSink {
            producer,
            tap,
            capture: Capture {
                samples: capture,
                rate: self.format.sample_rate,
                channels: self.format.channels,
            },
            format: self.format,
            name: "manual".into(),
            keepalive: Box::new(()),
        })
    }
}

/// Handle owned by the UI thread.
pub struct AudioHandle {
    commands: Sender<AudioCommand>,
    events: Receiver<AudioEvent>,
    shared: Arc<Shared>,
    tap: Arc<Mutex<Option<HeapCons<f32>>>>,
    capture: CaptureSlot,
    thread: Option<JoinHandle<()>>,
}

/// Where the capture of the current output stream waits to be taken.
pub type CaptureSlot = Arc<Mutex<Option<Capture>>>;

impl AudioHandle {
    /// Starts the controller with real audio output.
    pub fn spawn(config: AudioConfig) -> Self {
        let backend = CpalBackend {
            preferred: config.output_device.clone(),
        };
        Self::spawn_with_backend(config, Box::new(backend))
    }

    pub fn spawn_with_backend(config: AudioConfig, backend: Box<dyn AudioBackend>) -> Self {
        let (cmd_tx, cmd_rx) = unbounded();
        let (ev_tx, ev_rx) = unbounded();
        let shared = Arc::new(Shared::default());
        shared.set_volume(config.volume);
        let tap = Arc::new(Mutex::new(None));
        let capture: CaptureSlot = Arc::new(Mutex::new(None));
        let thread = {
            let shared = shared.clone();
            let tap = tap.clone();
            let capture = capture.clone();
            thread::Builder::new()
                .name("audio-controller".into())
                .spawn(move || {
                    let events = ev_tx.clone();
                    let result = catch_unwind(AssertUnwindSafe(move || {
                        Controller::new(config, backend, shared, cmd_rx, ev_tx, tap, capture).run();
                    }));
                    if result.is_err() {
                        log::error!("audio controller crashed");
                        let _ = events.send(AudioEvent::Warning("audio engine crashed".into()));
                    }
                })
                .expect("spawn audio controller thread")
        };
        AudioHandle {
            commands: cmd_tx,
            events: ev_rx,
            shared,
            tap,
            capture,
            thread: Some(thread),
        }
    }

    pub fn send(&self, command: AudioCommand) {
        let _ = self.commands.send(command);
    }

    pub fn events(&self) -> &Receiver<AudioEvent> {
        &self.events
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    /// Takes the spectrum tap of the current output stream, if a new one is available.
    pub fn take_spectrum_tap(&self) -> Option<HeapCons<f32>> {
        self.tap.try_lock().ok().and_then(|mut slot| slot.take())
    }

    /// Where each output stream's capture is left for whoever streams the audio (a new one
    /// after every device change). It only fills while `Shared::set_capture` is on.
    pub fn capture_slot(&self) -> CaptureSlot {
        self.capture.clone()
    }
}

impl Drop for AudioHandle {
    fn drop(&mut self) {
        let _ = self.commands.send(AudioCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Sink {
    producer: HeapProd<f32>,
    format: DeviceFormat,
    name: String,
    _keepalive: Box<dyn Any>,
}

#[derive(Debug)]
struct TrackInfo {
    path: PathBuf,
    duration: Option<Duration>,
    info: StreamInfo,
}

/// A stretch of the output stream belonging to one track, starting at `start_frame`
/// (counted in device frames of the current ring's `frames_played` timeline).
struct Segment {
    start_frame: u64,
    track_pos: Duration,
    track: Arc<TrackInfo>,
    announced: bool,
}

struct Current {
    decoder: Decoder,
    info: Arc<TrackInfo>,
    /// ReplayGain factor applied to this track's samples.
    gain: f32,
}

fn track_info(decoder: &Decoder) -> Arc<TrackInfo> {
    let fmt = decoder.format();
    Arc::new(TrackInfo {
        path: decoder.path().to_path_buf(),
        duration: decoder.duration(),
        info: StreamInfo {
            codec: decoder.codec_name().to_string(),
            sample_rate: fmt.sample_rate,
            channels: fmt.channels,
        },
    })
}

fn frames_to_duration(frames: u64, rate: u32) -> Duration {
    if rate == 0 {
        Duration::ZERO
    } else {
        Duration::from_secs_f64(frames as f64 / f64::from(rate))
    }
}

struct Controller {
    config: AudioConfig,
    backend: Box<dyn AudioBackend>,
    shared: Arc<Shared>,
    commands: Receiver<AudioCommand>,
    events: Sender<AudioEvent>,
    tap_slot: Arc<Mutex<Option<HeapCons<f32>>>>,
    capture_slot: CaptureSlot,
    fault_tx: Sender<StreamFault>,
    fault_rx: Receiver<StreamFault>,

    sink: Option<Sink>,
    retry_at: Option<Instant>,
    device_lost_reported: bool,

    current: Option<Current>,
    /// The current decoder is exhausted and no next track could be started.
    eof: bool,
    queued: Option<PathBuf>,
    preopened: Option<Decoder>,
    resampler: Option<Resampler>,
    /// Stream frame where the resampler's output frame 0 lands.
    rs_epoch: u64,
    scratch: Vec<f32>,
    out_buf: Vec<f32>,
    out_pos: usize,
    frames_written: u64,
    /// A flush the callback has not acknowledged yet: (request id, frames_written at flush).
    pending_flush: Option<(u64, u64)>,
    segments: VecDeque<Segment>,
    status: PlaybackStatus,
    shutdown: bool,
}

impl Controller {
    fn new(
        config: AudioConfig,
        backend: Box<dyn AudioBackend>,
        shared: Arc<Shared>,
        commands: Receiver<AudioCommand>,
        events: Sender<AudioEvent>,
        tap_slot: Arc<Mutex<Option<HeapCons<f32>>>>,
        capture_slot: CaptureSlot,
    ) -> Self {
        let (fault_tx, fault_rx) = unbounded();
        Controller {
            config,
            backend,
            shared,
            commands,
            events,
            tap_slot,
            capture_slot,
            fault_tx,
            fault_rx,
            sink: None,
            retry_at: None,
            device_lost_reported: false,
            current: None,
            eof: false,
            queued: None,
            preopened: None,
            resampler: None,
            rs_epoch: 0,
            scratch: Vec::new(),
            out_buf: Vec::new(),
            out_pos: 0,
            frames_written: 0,
            pending_flush: None,
            segments: VecDeque::new(),
            status: PlaybackStatus::Stopped,
            shutdown: false,
        }
    }

    fn emit(&self, event: AudioEvent) {
        let _ = self.events.send(event);
    }

    fn run(mut self) {
        self.shared.paused.store(true, Ordering::Relaxed);
        self.shared.set_status(PlaybackStatus::Stopped);
        if let Err(e) = self.open_sink() {
            self.report_device_lost(format!("no audio output: {e}"));
        }
        while !self.shutdown {
            while let Ok(cmd) = self.commands.try_recv() {
                self.handle(cmd);
            }
            if self.shutdown {
                break;
            }
            while let Ok(fault) = self.fault_rx.try_recv() {
                self.handle_fault(fault);
            }
            self.retry_device_if_due();
            self.pump();
            self.update_position();

            let timeout = match (&self.sink, self.status) {
                (None, _) => Duration::from_millis(100),
                (_, PlaybackStatus::Playing) => Duration::from_millis(10),
                _ => Duration::from_millis(50),
            };
            select! {
                recv(self.commands) -> cmd => match cmd {
                    Ok(cmd) => self.handle(cmd),
                    Err(_) => self.shutdown = true,
                },
                recv(self.fault_rx) -> fault => if let Ok(fault) = fault {
                    self.handle_fault(fault);
                },
                default(timeout) => {}
            }
        }
        self.shared.paused.store(true, Ordering::Relaxed);
        self.sink = None;
    }

    // ---- commands -------------------------------------------------------------------------

    fn handle(&mut self, cmd: AudioCommand) {
        match cmd {
            AudioCommand::LoadAndPlay(path) => self.load_and_play(path),
            AudioCommand::QueueNext(path) => self.queue_next(path),
            AudioCommand::Pause => {
                if self.status == PlaybackStatus::Playing {
                    self.shared.paused.store(true, Ordering::Relaxed);
                    self.set_status(PlaybackStatus::Paused);
                }
            }
            AudioCommand::Resume => {
                if self.status == PlaybackStatus::Paused {
                    if self.sink.is_none() {
                        self.retry_at = Some(Instant::now());
                        self.retry_device_if_due();
                    }
                    if self.sink.is_some() {
                        self.shared.paused.store(false, Ordering::Relaxed);
                        self.set_status(PlaybackStatus::Playing);
                    }
                }
            }
            AudioCommand::Stop => self.stop(),
            AudioCommand::SeekRelative(secs) => {
                let pos = self.shared.position();
                let delta = Duration::from_secs(secs.unsigned_abs());
                let target = if secs < 0 {
                    pos.saturating_sub(delta)
                } else {
                    pos + delta
                };
                self.seek_to(target);
            }
            AudioCommand::SeekAbsolute(target) => self.seek_to(target),
            AudioCommand::SetVolume(volume) => self.shared.set_volume(volume),
            AudioCommand::SetReplayGain(mode) => {
                self.config.replaygain = mode;
                if let Some(cur) = self.current.as_mut() {
                    cur.gain = replay_gain_factor(&cur.info.path, mode);
                }
            }
            AudioCommand::Shutdown => self.shutdown = true,
        }
    }

    fn set_status(&mut self, status: PlaybackStatus) {
        self.status = status;
        self.shared.set_status(status);
    }

    fn report_failure(&self, path: PathBuf, err: &DecoderError) {
        log::warn!("{}: {err}", path.display());
        let missing = matches!(err, DecoderError::NotFound);
        self.emit(AudioEvent::TrackFailed {
            path,
            error: err.to_string(),
            missing,
        });
    }

    fn load_and_play(&mut self, path: PathBuf) {
        let decoder = match Decoder::open(&path) {
            Ok(d) => d,
            Err(e) => {
                self.report_failure(path, &e);
                return;
            }
        };
        self.flush();
        self.reset_pipeline();
        let info = track_info(&decoder);
        self.segments.push_back(Segment {
            start_frame: self.frames_written,
            track_pos: Duration::ZERO,
            track: info.clone(),
            announced: false,
        });
        let gain = replay_gain_factor(decoder.path(), self.config.replaygain);
        self.current = Some(Current {
            decoder,
            info,
            gain,
        });
        if self.sink.is_none() {
            self.retry_at = Some(Instant::now());
            self.retry_device_if_due();
        }
        if self.sink.is_some() {
            self.shared.paused.store(false, Ordering::Relaxed);
            self.set_status(PlaybackStatus::Playing);
        } else {
            self.set_status(PlaybackStatus::Paused);
        }
    }

    fn queue_next(&mut self, path: Option<PathBuf>) {
        self.queued = path;
        self.preopened = None;
        // The current track already ran out while nothing was queued: continue right away.
        if self.eof
            && self.current.is_some()
            && self.queued.is_some()
            && let Some(decoder) = self.open_queued()
        {
            self.continue_with(decoder);
        }
    }

    fn stop(&mut self) {
        self.flush();
        self.reset_pipeline();
        self.current = None;
        self.shared.paused.store(true, Ordering::Relaxed);
        self.shared.set_position(Duration::ZERO);
        self.shared.set_duration(Duration::ZERO);
        self.set_status(PlaybackStatus::Stopped);
    }

    fn seek_to(&mut self, target: Duration) {
        if self.current.is_none() {
            return;
        }
        let already_announced = match (self.segments.front(), &self.current) {
            (Some(seg), Some(cur)) => seg.announced && Arc::ptr_eq(&seg.track, &cur.info),
            _ => false,
        };
        self.flush();
        self.out_buf.clear();
        self.out_pos = 0;
        self.resampler = None;
        self.eof = false;
        let Some(cur) = self.current.as_mut() else {
            return;
        };
        match cur.decoder.seek(target) {
            Ok(resumed) => {
                self.segments.push_back(Segment {
                    start_frame: self.frames_written,
                    track_pos: resumed,
                    track: cur.info.clone(),
                    announced: already_announced,
                });
                self.shared.set_position(resumed);
            }
            Err(e) => {
                let path = cur.info.path.clone();
                self.report_failure(path, &e);
                self.on_track_end();
            }
        }
    }

    /// Drops all audio that has not been played yet, waiting for the callback to clear the ring.
    fn flush(&mut self) {
        self.out_buf.clear();
        self.out_pos = 0;
        self.segments.clear();
        if self.sink.is_none() {
            return;
        }
        // The ring is already empty: nothing to discard.
        let played = self.shared.frames_played.load(Ordering::Acquire);
        if self.pending_flush.is_none() && played >= self.frames_written {
            return;
        }
        let written_at_flush = self.pending_flush.map_or(self.frames_written, |(_, w)| w);
        let request = self.shared.flush_request.fetch_add(1, Ordering::AcqRel) + 1;
        let deadline = Instant::now() + FLUSH_TIMEOUT;
        while self.shared.flush_ack.load(Ordering::Acquire) < request {
            if Instant::now() > deadline {
                // The device is not pulling audio right now. New audio must wait until it
                // acknowledges, or the late flush would throw it away.
                log::debug!("flush not acknowledged yet; deferring");
                self.pending_flush = Some((request, written_at_flush));
                return;
            }
            thread::sleep(Duration::from_millis(1));
        }
        self.pending_flush = None;
        self.frames_written = self.shared.frames_played.load(Ordering::Acquire);
    }

    /// Completes a deferred flush once the callback has acknowledged it.
    /// Returns false while it is still outstanding.
    fn settle_flush(&mut self) -> bool {
        let Some((request, written_at_flush)) = self.pending_flush else {
            return true;
        };
        if self.shared.flush_ack.load(Ordering::Acquire) < request {
            return false;
        }
        self.pending_flush = None;
        let played = self.shared.frames_played.load(Ordering::Acquire);
        // Positions recorded since the flush assumed the old write position; rebase them.
        let rebase = |frame: u64| frame.saturating_sub(written_at_flush) + played;
        for seg in &mut self.segments {
            seg.start_frame = rebase(seg.start_frame);
        }
        self.rs_epoch = rebase(self.rs_epoch);
        self.frames_written = rebase(self.frames_written);
        true
    }

    fn reset_pipeline(&mut self) {
        self.out_buf.clear();
        self.out_pos = 0;
        self.segments.clear();
        self.resampler = None;
        self.preopened = None;
        self.queued = None;
        self.eof = false;
    }

    // ---- decoding -------------------------------------------------------------------------

    fn queued_frames(&self) -> u64 {
        let ch = self.sink.as_ref().map_or(1, |s| s.format.channels);
        self.frames_written + ((self.out_buf.len() - self.out_pos) / ch) as u64
    }

    /// Keeps the ring buffer full: pushes converted audio and decodes more when there is room.
    fn pump(&mut self) {
        if !self.settle_flush() {
            return;
        }
        loop {
            let Some(sink) = self.sink.as_mut() else {
                return;
            };
            if self.out_pos < self.out_buf.len() {
                let ch = sink.format.channels;
                let vacant = sink.producer.vacant_len() / ch * ch;
                if vacant == 0 {
                    return;
                }
                let end = (self.out_pos + vacant).min(self.out_buf.len());
                let pushed = sink.producer.push_slice(&self.out_buf[self.out_pos..end]);
                self.out_pos += pushed;
                self.frames_written += (pushed / ch) as u64;
                if self.out_pos < self.out_buf.len() {
                    return;
                }
                self.out_buf.clear();
                self.out_pos = 0;
            }
            if !self.decode_step() {
                return;
            }
        }
    }

    /// Decodes one chunk into `out_buf`. Returns false when there is nothing to decode.
    fn decode_step(&mut self) -> bool {
        if self.eof {
            return false;
        }
        let Some(device) = self.sink.as_ref().map(|s| s.format) else {
            return false;
        };
        let Some(cur) = self.current.as_mut() else {
            return false;
        };
        let chunk = match cur.decoder.next_chunk() {
            Ok(Some(samples)) => {
                self.scratch.clear();
                self.scratch.extend_from_slice(samples);
                if cur.gain != 1.0 {
                    let gain = cur.gain;
                    self.scratch.iter_mut().for_each(|s| *s *= gain);
                }
                Some(cur.decoder.format())
            }
            Ok(None) => None,
            Err(e) => {
                let msg = format!("{}: {e}", cur.info.path.display());
                log::warn!("{msg}");
                self.emit(AudioEvent::Warning(msg));
                None
            }
        };
        let Some(format) = chunk else {
            self.on_track_end();
            return true;
        };
        if self.resampler.as_ref().is_some_and(|r| !r.accepts(format)) {
            // The format changed mid-track: drain the old resampler first.
            if let Some(mut old) = self.resampler.take() {
                old.finish(&mut self.out_buf);
            }
        }
        if self.resampler.is_none() {
            self.rs_epoch = self.queued_frames();
            self.resampler = Some(Resampler::new(format, device.sample_rate, device.channels));
        }
        if let Some(r) = self.resampler.as_mut() {
            r.process(&self.scratch, &mut self.out_buf);
        }
        self.maybe_preopen();
        true
    }

    fn open_queued(&mut self) -> Option<Decoder> {
        let path = self.queued.take()?;
        match Decoder::open(&path) {
            Ok(decoder) => Some(decoder),
            Err(e) => {
                self.report_failure(path, &e);
                None
            }
        }
    }

    fn maybe_preopen(&mut self) {
        if !self.config.gapless || self.preopened.is_some() || self.queued.is_none() {
            return;
        }
        let near_end = self.current.as_ref().is_some_and(|cur| {
            cur.decoder
                .duration()
                .is_some_and(|d| d.saturating_sub(cur.decoder.position()) <= PREBUFFER)
        });
        if near_end {
            self.preopened = self.open_queued();
        }
    }

    fn on_track_end(&mut self) {
        let next = match self.preopened.take() {
            Some(decoder) => Some(decoder),
            None => self.open_queued(),
        };
        match next {
            Some(decoder) => self.continue_with(decoder),
            None => {
                if let Some(mut r) = self.resampler.take() {
                    r.finish(&mut self.out_buf);
                }
                self.eof = true;
            }
        }
    }

    /// Continues the output stream with `decoder`, seamlessly when the format allows it.
    fn continue_with(&mut self, decoder: Decoder) {
        let format = decoder.format();
        let seamless =
            self.config.gapless && self.resampler.as_ref().is_some_and(|r| r.accepts(format));
        let start_frame = if seamless {
            self.rs_epoch
                + self
                    .resampler
                    .as_ref()
                    .map_or(0, |r| r.output_frames_for_input())
        } else {
            if let Some(mut r) = self.resampler.take() {
                r.finish(&mut self.out_buf);
            }
            self.queued_frames()
        };
        let info = track_info(&decoder);
        self.segments.push_back(Segment {
            start_frame,
            track_pos: Duration::ZERO,
            track: info.clone(),
            announced: false,
        });
        let gain = replay_gain_factor(decoder.path(), self.config.replaygain);
        self.current = Some(Current {
            decoder,
            info,
            gain,
        });
        self.eof = false;
    }

    // ---- position & events ----------------------------------------------------------------

    fn update_position(&mut self) {
        if self.pending_flush.is_some() {
            return;
        }
        let Some(sink) = self.sink.as_ref() else {
            return;
        };
        let rate = sink.format.sample_rate;
        let played = self.shared.frames_played.load(Ordering::Acquire);
        while self.segments.len() > 1 && self.segments[1].start_frame <= played {
            self.segments.pop_front();
        }
        let mut started = None;
        if let Some(seg) = self.segments.front_mut()
            && seg.start_frame <= played
        {
            if !seg.announced {
                seg.announced = true;
                started = Some(seg.track.clone());
            }
            let mut pos = seg.track_pos + frames_to_duration(played - seg.start_frame, rate);
            if let Some(d) = seg.track.duration {
                pos = pos.min(d);
            }
            self.shared.set_position(pos);
        }
        if let Some(track) = started {
            self.shared.set_duration(track.duration.unwrap_or_default());
            self.emit(AudioEvent::TrackStarted {
                path: track.path.clone(),
                duration: track.duration,
                info: track.info.clone(),
            });
        }
        let drained = self.out_pos >= self.out_buf.len() && played >= self.frames_written;
        if self.eof && drained && self.status != PlaybackStatus::Stopped {
            self.current = None;
            self.reset_pipeline();
            self.shared.paused.store(true, Ordering::Relaxed);
            self.set_status(PlaybackStatus::Stopped);
            self.emit(AudioEvent::Finished);
        }
    }

    // ---- devices --------------------------------------------------------------------------

    fn open_sink(&mut self) -> Result<(), OutputError> {
        self.sink = None;
        self.shared.frames_played.store(0, Ordering::Release);
        let opened =
            self.backend
                .open(&self.shared, self.config.buffer_ms, self.fault_tx.clone())?;
        // Faults queued by a previous stream are stale now.
        while self.fault_rx.try_recv().is_ok() {}
        self.shared.set_device_rate(opened.format.sample_rate);
        if let Ok(mut slot) = self.tap_slot.lock() {
            *slot = Some(opened.tap);
        }
        if let Ok(mut slot) = self.capture_slot.lock() {
            *slot = Some(opened.capture);
        }
        self.sink = Some(Sink {
            producer: opened.producer,
            format: opened.format,
            name: opened.name,
            _keepalive: opened.keepalive,
        });
        self.frames_written = 0;
        self.pending_flush = None;
        self.retry_at = None;
        Ok(())
    }

    /// Re-opens output and resumes the current track from `position` on the new stream.
    fn reopen_at(&mut self, position: Duration) -> Result<(), OutputError> {
        self.open_sink()?;
        self.out_buf.clear();
        self.out_pos = 0;
        self.segments.clear();
        self.resampler = None;
        if let Some(cur) = self.current.as_mut() {
            match cur.decoder.seek(position) {
                Ok(resumed) => {
                    self.eof = false;
                    self.segments.push_back(Segment {
                        start_frame: 0,
                        track_pos: resumed,
                        track: cur.info.clone(),
                        announced: true,
                    });
                }
                Err(e) => log::warn!("cannot resume after device change: {e}"),
            }
        }
        Ok(())
    }

    fn report_device_lost(&mut self, message: String) {
        log::warn!("{message}");
        if !self.device_lost_reported {
            self.device_lost_reported = true;
            self.emit(AudioEvent::DeviceLost(message));
        }
        self.retry_at = Some(Instant::now() + DEVICE_RETRY);
    }

    fn handle_fault(&mut self, fault: StreamFault) {
        let Some(old) = self.sink.take() else { return };
        let position = self.shared.position();
        drop(old);
        if fault == StreamFault::Invalidated {
            match self.reopen_at(position) {
                Ok(()) => {
                    let name = self
                        .sink
                        .as_ref()
                        .map(|s| s.name.clone())
                        .unwrap_or_default();
                    log::info!("audio output moved to {name}");
                    self.emit(AudioEvent::DeviceRestored(name));
                    return;
                }
                Err(e) => log::warn!("cannot reopen audio output: {e}"),
            }
        }
        if self.status == PlaybackStatus::Playing {
            self.shared.paused.store(true, Ordering::Relaxed);
            self.set_status(PlaybackStatus::Paused);
        }
        self.device_lost_reported = false;
        self.report_device_lost("audio device disconnected; playback paused".into());
    }

    fn retry_device_if_due(&mut self) {
        if self.sink.is_some() || self.retry_at.is_none_or(|t| Instant::now() < t) {
            return;
        }
        let position = self.shared.position();
        match self.reopen_at(position) {
            Ok(()) => {
                self.device_lost_reported = false;
                let name = self
                    .sink
                    .as_ref()
                    .map(|s| s.name.clone())
                    .unwrap_or_default();
                log::info!("audio output available: {name}");
                self.emit(AudioEvent::DeviceRestored(name));
            }
            Err(e) => {
                log::debug!("audio output still unavailable: {e}");
                self.retry_at = Some(Instant::now() + DEVICE_RETRY);
            }
        }
    }
}
