//! Finding audio files on disk and reading their metadata.

pub mod fs_browser;
pub mod scanner;

use std::path::{Path, PathBuf};

use walkdir::WalkDir;

/// File extensions the decoder can play (lower case).
pub const AUDIO_EXTENSIONS: &[&str] = &["mp3", "flac", "ogg", "oga", "wav", "m4a", "aac", "mp4"];

pub fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| AUDIO_EXTENSIONS.iter().any(|a| a.eq_ignore_ascii_case(e)))
}

/// Natural ("human") ordering: "track 2" sorts before "track 10", case-insensitively.
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = a.peek().copied().filter(char::is_ascii_digit) {
                    na.push(c);
                    a.next();
                }
                let mut nb = String::new();
                while let Some(c) = b.peek().copied().filter(char::is_ascii_digit) {
                    nb.push(c);
                    b.next();
                }
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                a.next();
                b.next();
            }
        }
    }
}

/// All audio files below `dir`, in natural path order.
pub fn collect_audio_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = WalkDir::new(dir)
        .follow_links(true)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file() && is_audio_file(e.path()))
        .map(|e| e.into_path())
        .collect();
    files.sort_by(|a, b| natural_cmp(&a.to_string_lossy(), &b.to_string_lossy()));
    files
}

/// Expands command-line arguments: folders become their audio files, files pass through.
pub fn expand_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            out.extend(collect_audio_files(p));
        } else {
            out.push(p.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn recognises_extensions_case_insensitively() {
        assert!(is_audio_file(Path::new("a/B.MP3")));
        assert!(is_audio_file(Path::new("x.Flac")));
        assert!(!is_audio_file(Path::new("cover.jpg")));
        assert!(!is_audio_file(Path::new("noext")));
    }

    #[test]
    fn natural_order_compares_numbers_by_value() {
        assert_eq!(natural_cmp("track 2.mp3", "track 10.mp3"), Ordering::Less);
        assert_eq!(natural_cmp("Track 02", "track 2"), Ordering::Equal);
        assert_eq!(natural_cmp("abc", "ABD"), Ordering::Less);
        let mut v = vec!["10 b", "2 a", "1 c", "Alpha", "alpha 2"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["1 c", "2 a", "10 b", "Alpha", "alpha 2"]);
    }

    #[test]
    fn collects_audio_recursively_in_natural_order() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("cd1");
        std::fs::create_dir(&sub).unwrap();
        for name in ["10.mp3", "2.flac", "notes.txt"] {
            std::fs::write(sub.join(name), b"").unwrap();
        }
        std::fs::write(dir.path().join("1.ogg"), b"").unwrap();
        let files = collect_audio_files(dir.path());
        let names: Vec<_> = files.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec!["1.ogg", "2.flac", "10.mp3"]);
    }
}
