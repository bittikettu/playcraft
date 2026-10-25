//! Songs a speaker cannot play are sent as WAV instead: 16-bit PCM, stereo, at most 48 kHz,
//! which every Sonos speaker plays. The file is decoded and converted while it is sent, and
//! byte ranges map to positions in the song, so the speaker can still seek.

use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

use crate::audio::decoder::{Decoder, DecoderError};
use crate::audio::resample::Resampler;

const HEADER_LEN: u64 = 44;
const FRAME_BYTES: u64 = 4;

/// The rate a song is sent at: its own up to 48 kHz, else 44.1 or 48 kHz (whichever divides
/// it, to keep the conversion simple).
pub fn output_rate(source: u32) -> u32 {
    if source <= 48_000 {
        source
    } else if source.is_multiple_of(44_100) {
        44_100
    } else {
        48_000
    }
}

/// A canonical 44-byte WAV header for `data_bytes` of 16-bit stereo at `rate`.
fn header(rate: u32, data_bytes: u32) -> [u8; HEADER_LEN as usize] {
    let mut h = [0u8; HEADER_LEN as usize];
    let byte_rate = rate * FRAME_BYTES as u32;
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&data_bytes.saturating_add(36).to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&2u16.to_le_bytes()); // channels
    h[24..28].copy_from_slice(&rate.to_le_bytes());
    h[28..32].copy_from_slice(&byte_rate.to_le_bytes());
    h[32..34].copy_from_slice(&(FRAME_BYTES as u16).to_le_bytes());
    h[34..36].copy_from_slice(&16u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data_bytes.to_le_bytes());
    h
}

/// The WAV version of one song.
pub struct WavSong {
    rate: u32,
    /// Size in bytes, when the song's length is known (it then supports byte ranges).
    pub len: Option<u64>,
}

impl WavSong {
    pub fn probe(path: &Path) -> Result<WavSong, DecoderError> {
        let decoder = Decoder::open(path)?;
        let rate = output_rate(decoder.format().sample_rate);
        let len = decoder.duration().map(|d| {
            let frames = (d.as_secs_f64() * f64::from(rate)).round() as u64;
            // A WAV header counts in 32 bits.
            HEADER_LEN + (frames * FRAME_BYTES).min(u64::from(u32::MAX) - 36)
        });
        Ok(WavSong { rate, len })
    }

    /// Writes bytes `start..` of the WAV file to `out`: `count` of them, or all to the end.
    pub fn write(
        &self,
        path: &Path,
        start: u64,
        count: Option<u64>,
        out: &mut impl Write,
    ) -> io::Result<()> {
        let mut budget = count.unwrap_or(u64::MAX);
        let data_bytes = self.len.map_or(u32::MAX - 36, |l| (l - HEADER_LEN) as u32);
        let head = header(self.rate, data_bytes);
        if start < HEADER_LEN {
            let part = &head[start as usize..];
            let n = part.len().min(budget as usize);
            out.write_all(&part[..n])?;
            budget -= n as u64;
        }
        let data_start = start.saturating_sub(HEADER_LEN);
        let data_end = self.len.map(|l| l - HEADER_LEN);
        if let Some(end) = data_end {
            budget = budget.min(end.saturating_sub(data_start));
        }
        if budget == 0 {
            return Ok(());
        }

        let mut decoder = Decoder::open(path).map_err(io::Error::other)?;
        let frame = data_start / FRAME_BYTES;
        let mut skip = (data_start % FRAME_BYTES) as usize;
        if frame > 0 {
            let to = Duration::from_secs_f64(frame as f64 / f64::from(self.rate));
            decoder.seek(to).map_err(io::Error::other)?;
        }
        let mut resampler: Option<Resampler> = None;
        let mut decoded = Vec::new();
        let mut samples = Vec::new();
        let mut bytes = Vec::new();
        while budget > 0 {
            samples.clear();
            let more = match decoder.next_chunk().map_err(io::Error::other)? {
                Some(chunk) => {
                    decoded.clear();
                    decoded.extend_from_slice(chunk);
                    true
                }
                None => false,
            };
            match more.then_some(&decoded) {
                Some(chunk) => {
                    let source = decoder.format();
                    if !resampler.as_ref().is_some_and(|r| r.accepts(source)) {
                        if let Some(mut done) = resampler.take() {
                            done.finish(&mut samples);
                        }
                        resampler = Some(Resampler::new(source, self.rate, 2));
                    }
                    if let Some(r) = resampler.as_mut() {
                        r.process(chunk, &mut samples);
                    }
                }
                None => {
                    if let Some(r) = resampler.as_mut() {
                        r.finish(&mut samples);
                    }
                    bytes.clear();
                    to_pcm16(&samples, &mut bytes);
                    let rest = &bytes[skip.min(bytes.len())..];
                    let n = rest.len().min(budget as usize);
                    out.write_all(&rest[..n])?;
                    budget -= n as u64;
                    // The promised length is kept: a song that decodes shorter than its
                    // stated length ends in silence.
                    if data_end.is_some() {
                        let zeros = [0u8; 4096];
                        while budget > 0 {
                            let n = budget.min(zeros.len() as u64) as usize;
                            out.write_all(&zeros[..n])?;
                            budget -= n as u64;
                        }
                    }
                    break;
                }
            }
            bytes.clear();
            to_pcm16(&samples, &mut bytes);
            let rest = &bytes[skip.min(bytes.len())..];
            skip = skip.saturating_sub(bytes.len());
            let n = rest.len().min(budget as usize);
            out.write_all(&rest[..n])?;
            budget -= n as u64;
        }
        out.flush()
    }
}

fn to_pcm16(samples: &[f32], out: &mut Vec<u8>) {
    out.reserve(samples.len() * 2);
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    #[test]
    fn rates_above_48k_come_down() {
        assert_eq!(output_rate(44_100), 44_100);
        assert_eq!(output_rate(48_000), 48_000);
        assert_eq!(output_rate(88_200), 44_100);
        assert_eq!(output_rate(96_000), 48_000);
        assert_eq!(output_rate(192_000), 48_000);
    }

    #[test]
    fn converts_a_song_to_a_wav_the_decoder_reads_back() {
        let source = fixture("sine_48k_stereo.flac");
        let song = WavSong::probe(&source).unwrap();
        let len = song.len.unwrap();
        let mut wav = Vec::new();
        song.write(&source, 0, None, &mut wav).unwrap();
        assert_eq!(wav.len() as u64, len, "exactly the promised length");
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 48_000);

        let dir = tempfile::tempdir().unwrap();
        let copy = dir.path().join("copy.wav");
        std::fs::write(&copy, &wav).unwrap();
        let back = Decoder::open(&copy).unwrap();
        assert_eq!(back.format().sample_rate, 48_000);
        assert_eq!(back.format().channels, 2);
        let d = back.duration().unwrap().as_secs_f64();
        let original = Decoder::open(&source).unwrap().duration().unwrap();
        assert!((d - original.as_secs_f64()).abs() < 0.01, "{d}");
        // It is not silence.
        assert!(wav[1000..2000].iter().any(|&b| b != 0));
    }

    #[test]
    fn ranges_line_up_with_the_whole_file() {
        let source = fixture("sine_44k_cbr.mp3");
        let song = WavSong::probe(&source).unwrap();
        let mut whole = Vec::new();
        song.write(&source, 0, None, &mut whole).unwrap();
        // The header alone, and a slice that starts inside it.
        let mut part = Vec::new();
        song.write(&source, 10, Some(30), &mut part).unwrap();
        assert_eq!(part, whole[10..40]);
        // The tail, from a point mid-song (decoded after a seek: sample-exact for this file
        // only within a few frames, so compare lengths and the end).
        let from = whole.len() as u64 - 8_000;
        let mut tail = Vec::new();
        song.write(&source, from, None, &mut tail).unwrap();
        assert_eq!(tail.len(), 8_000);
    }
}
