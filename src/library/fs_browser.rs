//! Directory listing for the file manager, and recursive adding on a worker thread.

use std::path::{Path, PathBuf};
use std::thread;

use crossbeam_channel::{Receiver, bounded};
use walkdir::WalkDir;

use super::{is_audio_file, natural_cmp};
use crate::platform;

/// File extensions recognised as playlists.
pub const PLAYLIST_EXTENSIONS: &[&str] = &["m3u", "m3u8", "pls", "blst"];

pub fn is_playlist_file(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        PLAYLIST_EXTENSIONS
            .iter()
            .any(|p| p.eq_ignore_ascii_case(e))
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EntryKind {
    Parent,
    Root,
    Dir,
    Playlist,
    Audio,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsEntry {
    pub path: PathBuf,
    pub name: String,
    pub kind: EntryKind,
}

impl FsEntry {
    /// Entries that can be selected and added (folders, audio files and playlists).
    pub fn is_selectable(&self) -> bool {
        matches!(
            self.kind,
            EntryKind::Dir | EntryKind::Audio | EntryKind::Playlist
        )
    }
}

/// The location shown by the file manager: a directory, or the list of drives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    Roots,
    Dir(PathBuf),
}

impl Location {
    /// Where "up" goes: the parent directory, or the drive list from a drive root.
    pub fn parent(&self) -> Option<Location> {
        match self {
            Location::Roots => None,
            Location::Dir(dir) => Some(match dir.parent() {
                Some(p) if !p.as_os_str().is_empty() => Location::Dir(p.to_path_buf()),
                _ => Location::Roots,
            }),
        }
    }

    pub fn display(&self) -> String {
        match self {
            Location::Roots => "Drives".into(),
            Location::Dir(d) => crate::paths::display(d),
        }
    }
}

/// Lists a location: folders first, then playlists and audio files, in natural order.
/// Other files are not shown.
pub fn list(location: &Location, show_hidden: bool) -> std::io::Result<Vec<FsEntry>> {
    let dir = match location {
        Location::Roots => {
            return Ok(platform::roots()
                .into_iter()
                .map(|r| FsEntry {
                    name: format!("{}  ({})", r.path.display(), r.kind),
                    path: r.path,
                    kind: EntryKind::Root,
                })
                .collect());
        }
        Location::Dir(dir) => dir,
    };
    let mut entries = vec![FsEntry {
        path: PathBuf::new(),
        name: "..".into(),
        kind: EntryKind::Parent,
    }];
    let mut items = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        // Follows symlinks/junctions so linked music folders behave like real ones.
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if !show_hidden && platform::is_hidden(&path, &meta) {
            continue;
        }
        let kind = if meta.is_dir() {
            EntryKind::Dir
        } else if is_audio_file(&path) {
            EntryKind::Audio
        } else if is_playlist_file(&path) {
            EntryKind::Playlist
        } else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        items.push(FsEntry { path, name, kind });
    }
    items.sort_by(|a, b| {
        let rank = |k: EntryKind| if k == EntryKind::Dir { 0 } else { 1 };
        rank(a.kind)
            .cmp(&rank(b.kind))
            .then_with(|| natural_cmp(&a.name, &b.name))
    });
    entries.extend(items);
    Ok(entries)
}

/// Expands `paths` on a worker thread: folders are walked recursively in natural order,
/// audio files pass through, keeping the order of `paths`. Results arrive in batches; the
/// channel closes when done.
pub fn scan_recursive(paths: Vec<PathBuf>) -> Receiver<Vec<PathBuf>> {
    let (tx, rx) = bounded(64);
    let _ = thread::Builder::new()
        .name("folder-scan".into())
        .spawn(move || {
            let mut batch = Vec::with_capacity(256);
            for dir in paths {
                if !dir.is_dir() {
                    if is_audio_file(&dir) {
                        batch.push(dir);
                    }
                    continue;
                }
                let walk = WalkDir::new(&dir).follow_links(true).sort_by(|a, b| {
                    let rank = |e: &walkdir::DirEntry| u8::from(!e.file_type().is_dir());
                    rank(a).cmp(&rank(b)).then_with(|| {
                        natural_cmp(
                            &a.file_name().to_string_lossy(),
                            &b.file_name().to_string_lossy(),
                        )
                    })
                });
                for entry in walk.into_iter().filter_map(Result::ok) {
                    if entry.file_type().is_file() && is_audio_file(entry.path()) {
                        batch.push(entry.into_path());
                        if batch.len() == batch.capacity() {
                            if tx.send(std::mem::take(&mut batch)).is_err() {
                                return;
                            }
                            batch.reserve(256);
                        }
                    }
                }
            }
            if !batch.is_empty() {
                let _ = tx.send(batch);
            }
        });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(p: &Path) {
        std::fs::write(p, b"").unwrap();
    }

    #[test]
    fn lists_dirs_first_then_supported_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("b album")).unwrap();
        std::fs::create_dir(dir.path().join("A album")).unwrap();
        for name in ["10 song.mp3", "2 song.FLAC", "cover.jpg", "list.m3u8"] {
            touch(&dir.path().join(name));
        }
        let entries = list(&Location::Dir(dir.path().to_path_buf()), false).unwrap();
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "..",
                "A album",
                "b album",
                "2 song.FLAC",
                "10 song.mp3",
                "list.m3u8"
            ]
        );
        assert_eq!(entries[5].kind, EntryKind::Playlist);
        assert!(!entries[0].is_selectable());
    }

    #[cfg(windows)]
    #[test]
    fn hides_files_with_hidden_attribute() {
        let dir = tempfile::tempdir().unwrap();
        let hidden = dir.path().join("secret.mp3");
        touch(&hidden);
        let status = std::process::Command::new("attrib")
            .arg("+h")
            .arg(&hidden)
            .status()
            .unwrap();
        assert!(status.success());
        let loc = Location::Dir(dir.path().to_path_buf());
        assert!(
            !list(&loc, false)
                .unwrap()
                .iter()
                .any(|e| e.name == "secret.mp3")
        );
        assert!(
            list(&loc, true)
                .unwrap()
                .iter()
                .any(|e| e.name == "secret.mp3")
        );
    }

    #[test]
    fn parent_of_drive_root_is_drive_list() {
        let root = platform::roots().into_iter().next().unwrap().path;
        assert_eq!(Location::Dir(root).parent(), Some(Location::Roots));
        assert_eq!(Location::Roots.parent(), None);
        let roots = list(&Location::Roots, false).unwrap();
        assert!(roots.iter().all(|e| e.kind == EntryKind::Root));
        assert!(!roots.is_empty());
    }

    #[test]
    fn recursive_scan_streams_all_audio_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let cd1 = dir.path().join("CD1");
        let cd2 = dir.path().join("CD2");
        std::fs::create_dir_all(&cd1).unwrap();
        std::fs::create_dir_all(&cd2).unwrap();
        for i in 1..=300 {
            touch(&cd1.join(format!("{i}.mp3")));
        }
        touch(&cd2.join("1.flac"));
        touch(&dir.path().join("intro.ogg"));
        touch(&dir.path().join("readme.txt"));
        let files: Vec<PathBuf> = scan_recursive(vec![dir.path().to_path_buf()])
            .iter()
            .flatten()
            .collect();
        assert_eq!(files.len(), 302);
        assert!(files[0].ends_with(Path::new("CD1").join("1.mp3")));
        assert!(files[1].ends_with(Path::new("CD1").join("2.mp3")));
        assert!(files[300].ends_with(Path::new("CD2").join("1.flac")));
        assert!(files[301].ends_with("intro.ogg"));
    }
}
