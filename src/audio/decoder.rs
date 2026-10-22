//! File decoding via symphonia into interleaved `f32` samples.
//!
//! Every call into symphonia is wrapped in `catch_unwind`, so a malformed file that trips a
//! panic inside a demuxer or codec turns into a track error instead of killing playback.

use std::fs::File;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::time::Duration;

use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase, Timestamp};

/// Consecutive undecodable packets tolerated before a track is abandoned.
const MAX_CONSECUTIVE_ERRORS: u32 = 100;

#[derive(Debug, thiserror::Error)]
pub enum DecoderError {
    #[error("file not found")]
    NotFound,
    #[error("cannot open file: {0}")]
    Open(#[source] io::Error),
    #[error("unsupported format: {0}")]
    Unsupported(String),
    #[error("no playable audio track")]
    NoAudioTrack,
    #[error("too many corrupt frames")]
    TooManyErrors,
    #[error("decoder failure: {0}")]
    Fatal(String),
    #[error("decoder crashed on malformed data")]
    Panic,
}

/// Sample rate and channel count of decoded audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceFormat {
    pub sample_rate: u32,
    pub channels: usize,
}

pub struct Decoder {
    path: PathBuf,
    reader: Box<dyn FormatReader>,
    codec: Box<dyn AudioDecoder>,
    track_id: u32,
    time_base: Option<TimeBase>,
    format: SourceFormat,
    duration: Option<Duration>,
    codec_name: String,
    /// Position of the next frame returned, in frames since the start of the track.
    position_frames: u64,
    /// After a seek: drop decoded audio before this timestamp (track time base units).
    seek_target: Option<Timestamp>,
    consecutive_errors: u32,
    finished: bool,
    buf: Vec<f32>,
}

struct SelectedTrack {
    codec: Box<dyn AudioDecoder>,
    track_id: u32,
    time_base: Option<TimeBase>,
    format: SourceFormat,
    duration: Option<Duration>,
    codec_name: String,
}

/// Picks the default audio track and builds a codec for it.
fn select_track(reader: &dyn FormatReader) -> Result<SelectedTrack, DecoderError> {
    let track = reader.default_track(TrackType::Audio).ok_or(DecoderError::NoAudioTrack)?;
    let params =
        track.codec_params.as_ref().and_then(|p| p.audio()).ok_or(DecoderError::NoAudioTrack)?;
    let codec = guarded(|| {
        symphonia::default::get_codecs().make_audio_decoder(params, &AudioDecoderOptions::default())
    })?
    .map_err(|e| DecoderError::Unsupported(e.to_string()))?;

    let sample_rate = params.sample_rate.unwrap_or(0);
    let channels = params.channels.as_ref().map(|c| c.count()).unwrap_or(0);
    let duration = match (track.num_frames, sample_rate) {
        (Some(frames), rate) if rate > 0 => Some(frames_to_duration(frames, rate)),
        _ => track
            .time_base
            .zip(track.duration)
            .and_then(|(tb, d)| tb.calc_duration(d))
            .map(time_to_duration),
    };
    Ok(SelectedTrack {
        codec_name: codec.codec_info().short_name.to_string(),
        codec,
        track_id: track.id,
        time_base: track.time_base,
        format: SourceFormat { sample_rate, channels },
        duration,
    })
}

fn guarded<T>(f: impl FnOnce() -> T) -> Result<T, DecoderError> {
    catch_unwind(AssertUnwindSafe(f)).map_err(|_| DecoderError::Panic)
}

fn time_to_duration(t: Time) -> Duration {
    u64::try_from(t.as_nanos()).map(Duration::from_nanos).unwrap_or_default()
}

fn frames_to_duration(frames: u64, rate: u32) -> Duration {
    if rate == 0 {
        return Duration::ZERO;
    }
    Duration::from_secs(frames / u64::from(rate))
        + Duration::from_nanos((frames % u64::from(rate)) * 1_000_000_000 / u64::from(rate))
}

impl Decoder {
    pub fn open(path: &Path) -> Result<Self, DecoderError> {
        let file = File::open(path).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => DecoderError::NotFound,
            _ => DecoderError::Open(e),
        })?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(&ext.to_ascii_lowercase());
        }
        let reader = guarded(|| {
            symphonia::default::get_probe().probe(
                &hint,
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
        })?
        .map_err(|e| DecoderError::Unsupported(e.to_string()))?;

        let selected = select_track(reader.as_ref())?;
        Ok(Decoder {
            path: path.to_path_buf(),
            reader,
            codec: selected.codec,
            track_id: selected.track_id,
            time_base: selected.time_base,
            format: selected.format,
            duration: selected.duration,
            codec_name: selected.codec_name,
            position_frames: 0,
            seek_target: None,
            consecutive_errors: 0,
            finished: false,
            buf: Vec::new(),
        })
    }

    /// Re-selects the track after symphonia reports `ResetRequired` (e.g. chained Ogg streams).
    fn reselect_track(&mut self) -> Result<(), DecoderError> {
        let selected = select_track(self.reader.as_ref())?;
        self.codec = selected.codec;
        self.track_id = selected.track_id;
        self.time_base = selected.time_base;
        self.format = selected.format;
        self.duration = selected.duration;
        self.codec_name = selected.codec_name;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Format of the audio returned by the most recent `next_chunk` (or the container's
    /// declared format before the first chunk).
    pub fn format(&self) -> SourceFormat {
        self.format
    }

    pub fn duration(&self) -> Option<Duration> {
        self.duration
    }

    /// Short codec name, e.g. "mp3" or "flac".
    pub fn codec_name(&self) -> &str {
        &self.codec_name
    }

    /// Track position of the next frame `next_chunk` will return.
    pub fn position(&self) -> Duration {
        frames_to_duration(self.position_frames, self.format.sample_rate)
    }

    /// Decodes the next chunk of interleaved `f32` samples. Returns `Ok(None)` at end of track.
    ///
    /// Corrupt packets are logged and skipped; the track is only abandoned after
    /// `MAX_CONSECUTIVE_ERRORS` in a row.
    pub fn next_chunk(&mut self) -> Result<Option<&[f32]>, DecoderError> {
        loop {
            if self.finished {
                return Ok(None);
            }
            let packet = match guarded(|| self.reader.next_packet())? {
                Ok(Some(packet)) => packet,
                Ok(None) => {
                    self.finished = true;
                    return Ok(None);
                }
                Err(SymError::IoError(e)) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    self.finished = true;
                    return Ok(None);
                }
                Err(SymError::ResetRequired) => {
                    self.reselect_track()?;
                    continue;
                }
                Err(SymError::DecodeError(msg)) => {
                    self.note_error(msg)?;
                    continue;
                }
                Err(e) => return Err(DecoderError::Fatal(e.to_string())),
            };
            if packet.track_id != self.track_id {
                continue;
            }

            let codec = &mut self.codec;
            let buf = &mut self.buf;
            let decoded = guarded(|| {
                codec.decode(&packet).map(|audio| {
                    audio.copy_to_vec_interleaved(buf);
                    let spec = audio.spec();
                    (audio.frames(), spec.rate(), spec.channels().count())
                })
            })?;
            let (frames, rate, channels) = match decoded {
                Ok(v) => v,
                Err(SymError::DecodeError(msg)) => {
                    self.note_error(msg)?;
                    continue;
                }
                Err(SymError::IoError(e)) => {
                    log::warn!("{}: skipped packet: {e}", self.path.display());
                    self.note_error("io error")?;
                    continue;
                }
                Err(SymError::ResetRequired) => {
                    self.reselect_track()?;
                    continue;
                }
                Err(e) => return Err(DecoderError::Fatal(e.to_string())),
            };
            self.consecutive_errors = 0;
            if frames == 0 || channels == 0 || rate == 0 {
                continue;
            }
            self.format = SourceFormat { sample_rate: rate, channels };

            let mut skip = 0usize;
            if let Some(target) = self.seek_target {
                let pts = packet.pts;
                let end = pts.checked_add(packet.dur).unwrap_or(Timestamp::MAX);
                if end <= target {
                    continue;
                }
                self.seek_target = None;
                if pts < target {
                    skip = self.ts_delta_to_frames(pts, target, rate).min(frames as u64) as usize;
                }
            }
            let samples = &self.buf[skip * channels..frames * channels];
            self.position_frames += (frames - skip) as u64;
            return Ok(Some(samples));
        }
    }

    fn note_error(&mut self, msg: &str) -> Result<(), DecoderError> {
        self.consecutive_errors += 1;
        log::warn!("{}: skipped corrupt packet ({msg})", self.path.display());
        if self.consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
            Err(DecoderError::TooManyErrors)
        } else {
            Ok(())
        }
    }

    fn ts_delta_to_frames(&self, from: Timestamp, to: Timestamp, rate: u32) -> u64 {
        let delta = to.get().saturating_sub(from.get()).max(0) as u64;
        match self.time_base {
            Some(tb) => {
                let secs = f64::from(tb) * delta as f64;
                (secs * f64::from(rate)).round() as u64
            }
            None => delta,
        }
    }

    /// Seeks to `to` (clamped to the track). Returns the position playback resumes from.
    pub fn seek(&mut self, to: Duration) -> Result<Duration, DecoderError> {
        let to = match self.duration {
            Some(d) => to.min(d),
            None => to,
        };
        let time = Time::try_from_nanos_u128(to.as_nanos()).unwrap_or(Time::ZERO);
        let track_id = self.track_id;
        let seeked = guarded(|| {
            self.reader.seek(SeekMode::Accurate, SeekTo::Time { time, track_id: Some(track_id) })
        })?;
        match seeked {
            Ok(seeked) => {
                guarded(|| self.codec.reset())?;
                self.finished = false;
                self.consecutive_errors = 0;
                self.seek_target = Some(seeked.required_ts);
                let resumed = self
                    .time_base
                    .and_then(|tb| tb.calc_time(seeked.required_ts))
                    .map(time_to_duration)
                    .unwrap_or(to);
                self.position_frames = (resumed.as_secs_f64()
                    * f64::from(self.format.sample_rate))
                .round() as u64;
                Ok(resumed)
            }
            // Seeking past the end (or into an unseekable tail) simply ends the track.
            Err(SymError::SeekError(_)) | Err(SymError::IoError(_)) => {
                self.finished = true;
                Ok(self.position())
            }
            Err(e) => Err(DecoderError::Fatal(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join(name)
    }

    fn decode_all(dec: &mut Decoder) -> Vec<f32> {
        let mut out = Vec::new();
        while let Some(chunk) = dec.next_chunk().expect("decode") {
            out.extend_from_slice(chunk);
        }
        out
    }

    #[test]
    fn decodes_every_codec_to_about_two_seconds() {
        for name in [
            "sine_44k_stereo.wav",
            "sine_44k_cbr.mp3",
            "sine_44k_vbr.mp3",
            "sine_44k_stereo.flac",
            "sine_44k_stereo.ogg",
            "sine_44k_stereo.m4a",
            "sine_48k_stereo.flac",
            "sine_44k_mono.mp3",
        ] {
            let mut dec = Decoder::open(&fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
            let samples = decode_all(&mut dec);
            let fmt = dec.format();
            let secs = samples.len() as f64 / fmt.channels as f64 / f64::from(fmt.sample_rate);
            assert!((secs - 2.0).abs() < 0.06, "{name}: decoded {secs:.3} s");
            let expected_channels = if name.contains("mono") { 1 } else { 2 };
            assert_eq!(fmt.channels, expected_channels, "{name}");
            let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs()));
            assert!(peak > 0.05, "{name}: silent output");
            assert!(dec.duration().is_some(), "{name}: no duration");
        }
    }

    #[test]
    fn lossless_seek_is_frame_accurate() {
        for name in ["sine_44k_stereo.wav", "sine_44k_stereo.flac", "chirp_full.flac"] {
            let mut full = Decoder::open(&fixture(name)).unwrap();
            let all = decode_all(&mut full);
            let fmt = full.format();

            let mut dec = Decoder::open(&fixture(name)).unwrap();
            let resumed = dec.seek(Duration::from_secs(1)).unwrap();
            assert_eq!(resumed, Duration::from_secs(1), "{name}");
            let after = decode_all(&mut dec);
            let offset = fmt.sample_rate as usize * fmt.channels;
            assert_eq!(after.len(), all.len() - offset, "{name}: length after seek");
            assert_eq!(&after[..256], &all[offset..offset + 256], "{name}: samples after seek");
        }
    }

    #[test]
    fn lossy_seek_lands_near_target() {
        for name in ["sine_44k_cbr.mp3", "sine_44k_stereo.ogg", "sine_44k_stereo.m4a"] {
            let mut dec = Decoder::open(&fixture(name)).unwrap();
            let resumed = dec.seek(Duration::from_millis(1500)).unwrap();
            assert!(resumed <= Duration::from_millis(1500), "{name}");
            let rest = decode_all(&mut dec);
            let fmt = dec.format();
            let secs = rest.len() as f64 / fmt.channels as f64 / f64::from(fmt.sample_rate);
            assert!((secs - 0.5).abs() < 0.06, "{name}: {secs:.3} s left after seek");
        }
    }

    #[test]
    fn seek_past_end_finishes_track() {
        let mut dec = Decoder::open(&fixture("sine_44k_stereo.flac")).unwrap();
        dec.seek(Duration::from_secs(60)).unwrap();
        let rest = decode_all(&mut dec);
        assert!(rest.len() < 4096 * 2);
    }

    #[test]
    fn missing_file_is_not_found() {
        assert!(matches!(
            Decoder::open(&fixture("does-not-exist.mp3")),
            Err(DecoderError::NotFound)
        ));
    }

    #[test]
    fn garbage_file_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("garbage.mp3");
        std::fs::write(&path, b"this is definitely not an mp3 file\n".repeat(200)).unwrap();
        match Decoder::open(&path) {
            Err(_) => {}
            Ok(mut dec) => {
                // Some probes accept anything; decoding must still terminate without panicking.
                while let Ok(Some(_)) = dec.next_chunk() {}
            }
        }
    }

    #[test]
    fn gapless_halves_match_full_file() {
        let mut full = Decoder::open(&fixture("chirp_full.flac")).unwrap();
        let all = decode_all(&mut full);
        let mut joined = decode_all(&mut Decoder::open(&fixture("chirp_part1.flac")).unwrap());
        joined.extend(decode_all(&mut Decoder::open(&fixture("chirp_part2.flac")).unwrap()));
        assert_eq!(joined.len(), all.len());
        assert!(joined == all, "split halves differ from the full chirp");
    }
}
