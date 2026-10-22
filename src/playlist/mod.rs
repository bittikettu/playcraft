//! Hierarchical group playlists: play order and file formats.

pub mod engine;
pub mod io;

use std::path::{Path, PathBuf};

use crate::library::{self, fs_browser::is_playlist_file};
use crate::model::{Group, Track};

/// The startup playlist built from command-line arguments.
pub struct Startup {
    pub root: Group,
    /// Set when a single playlist file was given (the default target for saving).
    pub playlist_path: Option<PathBuf>,
    pub errors: Vec<String>,
}

/// A single playlist argument becomes the playlist itself. Otherwise files and folders go to
/// the root group and each playlist becomes a group of its own.
pub fn from_paths(paths: &[PathBuf]) -> Startup {
    if let [only] = paths
        && is_playlist_file(only)
    {
        return match io::load(only) {
            Ok(mut root) => {
                root.name = "Root".into();
                Startup {
                    root,
                    playlist_path: Some(only.clone()),
                    errors: Vec::new(),
                }
            }
            Err(e) => Startup {
                root: Group::new(0, "Root"),
                playlist_path: None,
                errors: vec![format!("{}: {e}", only.display())],
            },
        };
    }
    let mut root = Group::new(0, "Root");
    let mut errors = Vec::new();
    for path in paths {
        if path.is_dir() {
            root.tracks.extend(
                library::collect_audio_files(path)
                    .into_iter()
                    .map(Track::new),
            );
        } else if is_playlist_file(path) {
            match io::load(path) {
                Ok(group) => root.subgroups.push(group),
                Err(e) => errors.push(format!("{}: {e}", path.display())),
            }
        } else {
            root.tracks.push(Track::new(path.clone()));
        }
    }
    root.renumber(0);
    Startup {
        root,
        playlist_path: None,
        errors,
    }
}

/// Every track path of a playlist file, flattened in play order.
pub fn flatten_file(path: &Path) -> Result<Vec<PathBuf>, io::PlaylistError> {
    let root = io::load(path)?;
    let mut out = Vec::new();
    root.walk(&mut |g| out.extend(g.tracks.iter().map(|t| t.path.clone())));
    Ok(out)
}
