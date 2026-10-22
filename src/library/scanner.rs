//! Background metadata reading with lofty, so large folders never block the UI.

use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::thread;

use crossbeam_channel::{Receiver, Sender, unbounded};
use lofty::prelude::*;
use lofty::tag::Tag;

use crate::model::{ReplayGain, TrackMetadata};

/// Reads only the ReplayGain values (all `None` when unavailable). Never panics.
pub fn read_replay_gain(path: &Path) -> ReplayGain {
    read_metadata(path)
        .map(|m| m.replay_gain)
        .unwrap_or_default()
}

/// Reads tags and stream properties. Returns `None` for unreadable files; never panics.
pub fn read_metadata(path: &Path) -> Option<TrackMetadata> {
    match catch_unwind(AssertUnwindSafe(|| read_metadata_inner(path))) {
        Ok(Ok(meta)) => Some(meta),
        Ok(Err(e)) => {
            log::debug!("{}: no metadata: {e}", path.display());
            None
        }
        Err(_) => {
            log::warn!("{}: tag reader crashed on malformed data", path.display());
            None
        }
    }
}

fn read_metadata_inner(path: &Path) -> Result<TrackMetadata, lofty::error::FileParseError> {
    let tagged = lofty::read_from_path(path)?;
    let props = tagged.properties();
    let mut meta = TrackMetadata {
        bitrate_kbps: props
            .audio_bitrate()
            .or(props.overall_bitrate())
            .unwrap_or(0),
        sample_rate_hz: props.sample_rate().unwrap_or(0),
        channels: props.channels().unwrap_or(0),
        duration: props.duration(),
        ..Default::default()
    };
    if let Some(tag) = tagged.primary_tag().or_else(|| tagged.tags().first()) {
        fill_from_tag(&mut meta, tag);
    }
    Ok(meta)
}

fn non_empty(s: Option<std::borrow::Cow<'_, str>>) -> Option<String> {
    s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn fill_from_tag(meta: &mut TrackMetadata, tag: &Tag) {
    meta.title = non_empty(tag.title());
    meta.artist = non_empty(tag.artist());
    meta.album = non_empty(tag.album());
    meta.genre = non_empty(tag.genre());
    meta.year = tag.date().map(|d| u32::from(d.year)).filter(|&y| y > 0);
    meta.replay_gain = ReplayGain {
        track_gain_db: parse_gain(tag.get_string(ItemKey::ReplayGainTrackGain)),
        track_peak: parse_gain(tag.get_string(ItemKey::ReplayGainTrackPeak)),
        album_gain_db: parse_gain(tag.get_string(ItemKey::ReplayGainAlbumGain)),
        album_peak: parse_gain(tag.get_string(ItemKey::ReplayGainAlbumPeak)),
    };
}

/// Parses ReplayGain values such as "-6.50 dB" or "0.988525".
fn parse_gain(s: Option<&str>) -> Option<f32> {
    let s = s?.trim();
    let number = s
        .split_whitespace()
        .next()?
        .trim_end_matches(|c: char| c.is_ascii_alphabetic());
    number
        .replace(',', ".")
        .parse::<f32>()
        .ok()
        .filter(|v| v.is_finite())
}

/// A pool of worker threads reading metadata. Results are polled with `results`.
pub struct MetadataScanner {
    jobs: Sender<PathBuf>,
    results: Receiver<(PathBuf, Option<TrackMetadata>)>,
    requested: HashSet<PathBuf>,
}

impl MetadataScanner {
    pub fn new() -> Self {
        let threads = thread::available_parallelism()
            .map_or(2, |n| n.get())
            .clamp(1, 4);
        let (job_tx, job_rx) = unbounded::<PathBuf>();
        let (res_tx, res_rx) = unbounded();
        for i in 0..threads {
            let jobs = job_rx.clone();
            let results = res_tx.clone();
            let _ = thread::Builder::new()
                .name(format!("metadata-{i}"))
                .spawn(move || {
                    for path in jobs {
                        let meta = read_metadata(&path);
                        if results.send((path, meta)).is_err() {
                            break;
                        }
                    }
                });
        }
        MetadataScanner {
            jobs: job_tx,
            results: res_rx,
            requested: HashSet::new(),
        }
    }

    /// Queues `path` unless it has been requested before.
    pub fn request(&mut self, path: &Path) {
        if self.requested.insert(path.to_path_buf()) {
            let _ = self.jobs.send(path.to_path_buf());
        }
    }

    /// Forgets that `path` was read, so a later `request` reads it again.
    pub fn invalidate(&mut self, path: &Path) {
        self.requested.remove(path);
    }

    /// Results that have arrived since the last call.
    pub fn results(&self) -> impl Iterator<Item = (PathBuf, Option<TrackMetadata>)> + '_ {
        self.results.try_iter()
    }

    /// Requests still being worked on (approximate).
    pub fn backlog(&self) -> usize {
        self.jobs.len()
    }
}

impl Default for MetadataScanner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    #[test]
    fn reads_tags_and_replaygain() {
        for name in ["tagged.mp3", "tagged.flac"] {
            let meta = read_metadata(&fixture(name)).unwrap_or_else(|| panic!("{name}"));
            assert_eq!(meta.title.as_deref(), Some("Test Title"), "{name}");
            assert_eq!(meta.artist.as_deref(), Some("Test Artist"), "{name}");
            assert_eq!(meta.album.as_deref(), Some("Test Album"), "{name}");
            assert_eq!(meta.year, Some(1999), "{name}");
            assert_eq!(meta.genre.as_deref(), Some("Rock"), "{name}");
            assert_eq!(meta.replay_gain.track_gain_db, Some(-6.5), "{name}");
            assert_eq!(meta.replay_gain.track_peak, Some(0.5), "{name}");
            assert_eq!(meta.sample_rate_hz, 44_100, "{name}");
            assert_eq!(meta.channels, 2, "{name}");
            assert!((meta.duration.as_secs_f64() - 2.0).abs() < 0.1, "{name}");
        }
    }

    #[test]
    fn reads_properties_of_untagged_files() {
        let meta = read_metadata(&fixture("sine_44k_cbr.mp3")).unwrap();
        // lofty averages over file size and duration, so a short CBR file reads slightly high.
        assert!(
            (310..=330).contains(&meta.bitrate_kbps),
            "{} kbps",
            meta.bitrate_kbps
        );
        assert!(meta.title.is_none());
    }

    #[test]
    fn unreadable_files_yield_none() {
        assert!(read_metadata(&fixture("missing.mp3")).is_none());
        let dir = tempfile::tempdir().unwrap();
        let junk = dir.path().join("junk.flac");
        std::fs::write(&junk, b"fLaC but then garbage").unwrap();
        assert!(read_metadata(&junk).is_none());
    }

    #[test]
    fn parses_gain_strings() {
        assert_eq!(parse_gain(Some("-6.50 dB")), Some(-6.5));
        assert_eq!(parse_gain(Some("+1.20dB")), Some(1.2));
        assert_eq!(parse_gain(Some("0.988525")), Some(0.988525));
        assert_eq!(parse_gain(Some("-3,25 dB")), Some(-3.25));
        assert_eq!(parse_gain(Some("loud")), None);
        assert_eq!(parse_gain(None), None);
    }

    #[test]
    fn scanner_deduplicates_and_delivers() {
        let mut scanner = MetadataScanner::new();
        scanner.request(&fixture("tagged.flac"));
        scanner.request(&fixture("tagged.flac"));
        scanner.request(&fixture("sine_44k_stereo.ogg"));
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut got = Vec::new();
        while got.len() < 2 {
            got.extend(scanner.results());
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(50));
        got.extend(scanner.results());
        assert_eq!(got.len(), 2, "duplicate request was not deduplicated");
        assert!(got.iter().all(|(_, m)| m.is_some()));
    }
}
