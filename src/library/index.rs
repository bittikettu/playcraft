//! The music folder index behind AI search: every audio file under the library folders with
//! its tags. It is built in the background on first use and cached in `library.json`, so
//! later scans only re-read files whose size or modification time changed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::model::TrackMetadata;

/// A later use rescans when the last scan is older than this.
const RESCAN_AFTER: Duration = Duration::from_secs(600);
const CACHE_VERSION: u32 = 1;
/// Tag results are published in batches of this many files.
const BATCH: usize = 200;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexEntry {
    pub path: PathBuf,
    /// Modification time (seconds since the epoch) and size when the tags were read.
    pub modified: u64,
    pub size: u64,
    pub meta: Option<TrackMetadata>,
}

#[derive(Serialize, Deserialize)]
struct Cache {
    version: u32,
    entries: Vec<IndexEntry>,
}

#[derive(Debug, Default)]
pub struct IndexState {
    pub entries: Vec<IndexEntry>,
    pub scanning: bool,
    /// Files found by the running scan whose tags are still being read.
    pub pending: usize,
}

pub struct LibraryIndex {
    dirs: Vec<PathBuf>,
    cache: Option<PathBuf>,
    state: Arc<RwLock<IndexState>>,
    last_scan: Option<Instant>,
}

impl LibraryIndex {
    /// An index of `dirs`, cached in the file `cache`. Nothing happens until `refresh`.
    pub fn new(dirs: Vec<PathBuf>, cache: Option<PathBuf>) -> Self {
        LibraryIndex {
            dirs,
            cache,
            state: Arc::default(),
            last_scan: None,
        }
    }

    pub fn dirs(&self) -> &[PathBuf] {
        &self.dirs
    }

    /// Changes the folders; the next `refresh` scans them.
    pub fn set_dirs(&mut self, dirs: Vec<PathBuf>) {
        if dirs != self.dirs {
            self.dirs = dirs;
            self.last_scan = None;
        }
    }

    /// Starts a background scan unless one is running or ran recently. The first scan
    /// publishes the cached entries at once.
    pub fn refresh(&mut self) {
        if self.read().scanning || self.last_scan.is_some_and(|t| t.elapsed() < RESCAN_AFTER) {
            return;
        }
        self.last_scan = Some(Instant::now());
        self.write().scanning = true;
        let (dirs, cache, state) = (self.dirs.clone(), self.cache.clone(), self.state.clone());
        let spawned = std::thread::Builder::new()
            .name("library-index".into())
            .spawn(move || scan(&dirs, cache.as_deref(), &state));
        if let Err(e) = spawned {
            log::warn!("library index: cannot start: {e}");
            self.write().scanning = false;
        }
    }

    pub fn read(&self) -> RwLockReadGuard<'_, IndexState> {
        self.state.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> RwLockWriteGuard<'_, IndexState> {
        lock(&self.state)
    }
}

fn lock(state: &RwLock<IndexState>) -> RwLockWriteGuard<'_, IndexState> {
    state.write().unwrap_or_else(|e| e.into_inner())
}

fn load_cache(file: &Path) -> Vec<IndexEntry> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    match serde_json::from_str::<Cache>(&text) {
        Ok(c) if c.version == CACHE_VERSION => c.entries,
        _ => Vec::new(),
    }
}

/// (modified, size) of a file.
fn stamp(entry: &walkdir::DirEntry) -> Option<(u64, u64)> {
    let meta = entry.metadata().ok()?;
    let modified = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some((modified, meta.len()))
}

fn scan(dirs: &[PathBuf], cache: Option<&Path>, state: &RwLock<IndexState>) {
    let started = Instant::now();
    // What we already know: the entries of the last scan, or else the cache file.
    let mut known: Vec<IndexEntry> = std::mem::take(&mut lock(state).entries);
    if known.is_empty()
        && let Some(file) = cache
    {
        known = load_cache(file);
    }
    lock(state).entries = known.clone();
    let mut known: HashMap<PathBuf, IndexEntry> =
        known.into_iter().map(|e| (e.path.clone(), e)).collect();

    let mut fresh = Vec::new();
    let mut to_read = Vec::new();
    for dir in dirs {
        let files = WalkDir::new(dir)
            .follow_links(true)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file() && super::is_audio_file(e.path()));
        for file in files {
            let Some((modified, size)) = stamp(&file) else {
                continue;
            };
            match known.remove(file.path()) {
                Some(e) if e.modified == modified && e.size == size => fresh.push(e),
                _ => to_read.push(IndexEntry {
                    path: file.into_path(),
                    modified,
                    size,
                    meta: None,
                }),
            }
        }
    }
    let unchanged = fresh.len();
    {
        // Deleted files drop out now; changed and new ones follow as their tags are read.
        let mut s = lock(state);
        s.entries = fresh;
        s.pending = to_read.len();
    }

    let read = to_read.len();
    let workers = std::thread::available_parallelism().map_or(2, |n| n.get().min(4));
    let (jobs, queue) = crossbeam_channel::unbounded::<IndexEntry>();
    let (done, results) = crossbeam_channel::unbounded::<IndexEntry>();
    for entry in to_read {
        let _ = jobs.send(entry);
    }
    drop(jobs);
    for _ in 0..workers {
        let (queue, done) = (queue.clone(), done.clone());
        std::thread::spawn(move || {
            for mut entry in queue {
                entry.meta = super::scanner::read_metadata(&entry.path);
                if done.send(entry).is_err() {
                    break;
                }
            }
        });
    }
    drop(done);
    let mut batch = Vec::with_capacity(BATCH);
    let publish = |batch: &mut Vec<IndexEntry>| {
        let mut s = lock(state);
        s.pending = s.pending.saturating_sub(batch.len());
        s.entries.append(batch);
    };
    for entry in results {
        batch.push(entry);
        if batch.len() >= BATCH {
            publish(&mut batch);
        }
    }
    publish(&mut batch);

    // The cache is written before the scan counts as finished.
    if let Some(file) = cache {
        let json = serde_json::to_string(&Cache {
            version: CACHE_VERSION,
            entries: lock(state).entries.clone(),
        })
        .unwrap_or_default();
        if let Err(e) = crate::session::save_json(file, &json) {
            log::warn!("library index: cannot save {}: {e}", file.display());
        }
    }
    {
        let mut s = lock(state);
        s.scanning = false;
        s.pending = 0;
    }
    log::info!(
        "library index: {} tracks ({unchanged} unchanged, {read} read) in {:.1} s",
        unchanged + read,
        started.elapsed().as_secs_f32()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    fn wait(index: &LibraryIndex) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while index.read().scanning {
            assert!(Instant::now() < deadline, "index never finished");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn indexes_tags_and_reuses_the_cache() {
        let music = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let album = music.path().join("Album");
        std::fs::create_dir(&album).unwrap();
        std::fs::copy(fixture("tagged.flac"), album.join("01 tagged.flac")).unwrap();
        std::fs::copy(fixture("sine_44k_cbr.mp3"), album.join("02 sine.mp3")).unwrap();
        std::fs::write(album.join("cover.jpg"), b"not audio").unwrap();
        let cache = state.path().join("library.json");

        let mut index = LibraryIndex::new(vec![music.path().to_path_buf()], Some(cache.clone()));
        assert!(
            index.read().entries.is_empty(),
            "nothing before the first use"
        );
        index.refresh();
        wait(&index);
        {
            let s = index.read();
            assert_eq!(s.entries.len(), 2, "{:?}", s.entries);
            let tagged = s
                .entries
                .iter()
                .find(|e| e.path.ends_with("01 tagged.flac"));
            let meta = tagged.and_then(|e| e.meta.as_ref()).expect("tags read");
            assert_eq!(meta.genre.as_deref(), Some("Rock"));
            assert_eq!(meta.artist.as_deref(), Some("Test Artist"));
        }
        assert!(cache.exists());

        // A new player starts from the cache; unchanged files are not read again, a
        // deleted one drops out and a new one is added.
        std::fs::remove_file(album.join("02 sine.mp3")).unwrap();
        std::fs::copy(fixture("sine_44k_stereo.ogg"), album.join("03 sine.ogg")).unwrap();
        let mut again = LibraryIndex::new(vec![music.path().to_path_buf()], Some(cache));
        again.refresh();
        wait(&again);
        let s = again.read();
        let mut names: Vec<_> = s
            .entries
            .iter()
            .map(|e| e.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["01 tagged.flac", "03 sine.ogg"]);
    }

    #[test]
    fn a_recent_scan_is_not_repeated() {
        let music = tempfile::tempdir().unwrap();
        let mut index = LibraryIndex::new(vec![music.path().to_path_buf()], None);
        index.refresh();
        wait(&index);
        let first = index.last_scan;
        index.refresh();
        assert_eq!(index.last_scan, first);
        index.set_dirs(vec![music.path().join("other")]);
        index.refresh();
        assert_ne!(index.last_scan, first, "new folders are scanned at once");
        wait(&index);
    }
}
