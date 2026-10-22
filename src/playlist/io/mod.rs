//! Playlist files: `.m3u`/`.m3u8`, `.pls` and the native `.blst` group format.
//!
//! Path rules (Windows-first): entries may use `\` or `/`, be drive-absolute, UNC, relative to
//! the playlist file, or `file://` URIs. Paths under the playlist's folder are written relative.

pub mod blst;
mod m3u;
mod pls;

use std::path::{Component, Path, PathBuf};

use crate::model::Group;

#[derive(Debug, thiserror::Error)]
pub enum PlaylistError {
    #[error("cannot read playlist: {0}")]
    Io(#[from] std::io::Error),
    #[error("not a playlist file (use .m3u, .m3u8, .pls or .blst)")]
    UnknownFormat,
    #[error("invalid playlist: {0}")]
    Parse(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaylistFormat {
    M3u,
    M3u8,
    Pls,
    Blst,
}

impl PlaylistFormat {
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "m3u" => PlaylistFormat::M3u,
            "m3u8" => PlaylistFormat::M3u8,
            "pls" => PlaylistFormat::Pls,
            "blst" => PlaylistFormat::Blst,
            _ => return None,
        })
    }
}

/// Reads a playlist into a group tree whose root is named after the file.
pub fn load(path: &Path) -> Result<Group, PlaylistError> {
    let format = PlaylistFormat::from_path(path).ok_or(PlaylistError::UnknownFormat)?;
    let bytes = std::fs::read(path)?;
    let base = path.parent().unwrap_or(Path::new("."));
    let mut root = match format {
        PlaylistFormat::M3u | PlaylistFormat::M3u8 => m3u::parse(&decode_text(&bytes, format), base),
        PlaylistFormat::Pls => pls::parse(&decode_text(&bytes, format), base),
        PlaylistFormat::Blst => blst::parse(&decode_text(&bytes, format), base)?,
    };
    if root.name.is_empty() || format != PlaylistFormat::Blst {
        root.name = path.file_stem().map_or_else(|| "Playlist".into(), |s| s.to_string_lossy().into_owned());
    }
    root.renumber(0);
    Ok(root)
}

/// Writes `root` in the format given by the file extension. `.m3u` is written as UTF-8 too.
pub fn save(path: &Path, root: &Group) -> Result<(), PlaylistError> {
    let format = PlaylistFormat::from_path(path).ok_or(PlaylistError::UnknownFormat)?;
    let base = path.parent().unwrap_or(Path::new("."));
    let text = match format {
        PlaylistFormat::M3u | PlaylistFormat::M3u8 => m3u::write(root, base),
        PlaylistFormat::Pls => pls::write(root, base),
        PlaylistFormat::Blst => blst::write(root, base)?,
    };
    // Write next to the target and rename, so a crash never leaves a half-written playlist.
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Decodes playlist bytes: UTF-8 (with or without BOM), else Windows-1252 for legacy `.m3u`/`.pls`.
fn decode_text(bytes: &[u8], format: PlaylistFormat) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) if format == PlaylistFormat::M3u8 || format == PlaylistFormat::Blst => {
            String::from_utf8_lossy(bytes).into_owned()
        }
        Err(_) => encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned(),
    }
}

fn percent_decode(s: &str) -> String {
    fn hex(b: u8) -> Option<u8> {
        (b as char).to_digit(16).map(|d| d as u8)
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(hi << 4 | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether `s` is absolute in Windows terms (drive letter or UNC), on any OS.
fn is_windows_absolute(s: &str) -> bool {
    let b = s.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/'))
        || s.starts_with("\\\\")
        || s.starts_with("//")
}

/// Removes `.` and resolves `..` components without touching the filesystem.
fn lexical_clean(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Turns a playlist entry into a path, resolving it against the playlist's folder.
pub fn resolve_entry(base: &Path, entry: &str) -> PathBuf {
    let mut s = entry.trim().to_string();
    if let Some(rest) = s.strip_prefix("file://") {
        let decoded = percent_decode(rest);
        s = match decoded.strip_prefix('/') {
            // file:///C:/Music/a.mp3
            Some(local) if is_windows_absolute(local) => local.to_string(),
            // file:///home/user/a.mp3
            Some(_) => decoded.clone(),
            // file://server/share/a.mp3
            None => format!("//{decoded}"),
        };
    }
    if cfg!(windows) {
        s = s.replace('/', "\\");
    } else if !is_windows_absolute(&s) {
        s = s.replace('\\', "/");
    }
    let p = PathBuf::from(&s);
    if p.is_absolute() || is_windows_absolute(&s) || p.has_root() {
        lexical_clean(&p)
    } else {
        lexical_clean(&base.join(p))
    }
}

/// How an entry is written: relative to `base` when the file is inside it, else absolute.
pub fn entry_for(base: &Path, path: &Path) -> String {
    let base = lexical_clean(&crate::paths::normalize(base));
    let path = lexical_clean(&if path.is_absolute() { path.to_path_buf() } else { crate::paths::normalize(path) });
    match path.strip_prefix(&base) {
        Ok(rel) if !rel.as_os_str().is_empty() => rel.display().to_string(),
        _ => crate::paths::display(&path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Track;

    fn sample_tree(dir: &Path) -> Group {
        let mut root = Group::new(0, "Root");
        root.tracks.push(Track::new(dir.join("intro.mp3")));
        let mut synth = Group::new(1, "Synthwave");
        synth.shuffle = true;
        synth.tracks.push(Track::new(dir.join("synth").join("a.flac")));
        let mut deep = Group::new(2, "Deep Cuts");
        deep.tracks.push(Track::new(dir.join("synth").join("deep").join("b.ogg")));
        synth.subgroups.push(deep);
        root.subgroups.push(synth);
        let mut pods = Group::new(3, "Podcasts");
        pods.tracks.push(Track::new(dir.join("pod.mp3")));
        root.subgroups.push(pods);
        root
    }

    fn shape(g: &Group) -> Vec<(String, Vec<PathBuf>)> {
        let mut out = Vec::new();
        g.walk(&mut |g| out.push((g.name.clone(), g.tracks.iter().map(|t| t.path.clone()).collect())));
        out
    }

    #[test]
    fn blst_round_trips_tree_flags_and_paths() {
        let dir = tempfile::tempdir().unwrap();
        let base = crate::paths::normalize(dir.path());
        let root = sample_tree(&base);
        let file = base.join("mix.blst");
        save(&file, &root).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("\"format\": \"blst\""), "{text}");
        assert!(!text.contains(&*base.to_string_lossy().replace('\\', "\\\\")), "paths should be relative: {text}");
        let loaded = load(&file).unwrap();
        let mut expected = shape(&root);
        expected[0].0 = loaded.name.clone();
        assert_eq!(shape(&loaded), expected);
        assert!(loaded.subgroups[0].shuffle);
    }

    #[test]
    fn m3u8_round_trips_nested_groups_via_extgrp() {
        let dir = tempfile::tempdir().unwrap();
        let base = crate::paths::normalize(dir.path());
        let root = sample_tree(&base);
        let file = base.join("mix.m3u8");
        save(&file, &root).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.starts_with("#EXTM3U"));
        assert!(text.contains("#EXTGRP:Synthwave / Deep Cuts"), "{text}");
        let loaded = load(&file).unwrap();
        let mut expected = shape(&root);
        expected[0].0 = "mix".into();
        assert_eq!(shape(&loaded), expected);
    }

    #[test]
    fn reads_windows_1252_m3u() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("old.m3u");
        // "Mötley Crüe.mp3" in Windows-1252.
        let mut bytes = b"#EXTM3U\r\n#EXTINF:123,Something\r\nM".to_vec();
        bytes.extend([0xF6]);
        bytes.extend(b"tley Cr");
        bytes.extend([0xFC]);
        bytes.extend(b"e.mp3\r\n");
        std::fs::write(&file, bytes).unwrap();
        let loaded = load(&file).unwrap();
        assert_eq!(loaded.tracks.len(), 1);
        assert_eq!(loaded.tracks[0].file_name(), "Mötley Crüe.mp3");
    }

    #[test]
    fn resolves_relative_absolute_and_uri_entries() {
        let base = if cfg!(windows) { PathBuf::from(r"D:\Music\Lists") } else { PathBuf::from("/music/lists") };
        assert_eq!(resolve_entry(&base, "song.mp3"), base.join("song.mp3"));
        assert_eq!(resolve_entry(&base, "../Album/01.flac"), base.parent().unwrap().join("Album").join("01.flac"));
        assert_eq!(resolve_entry(&base, "..\\Album\\01.flac"), base.parent().unwrap().join("Album").join("01.flac"));
        if cfg!(windows) {
            assert_eq!(resolve_entry(&base, r"C:\Other\x.mp3"), PathBuf::from(r"C:\Other\x.mp3"));
            assert_eq!(resolve_entry(&base, "C:/Other/x.mp3"), PathBuf::from(r"C:\Other\x.mp3"));
            assert_eq!(resolve_entry(&base, "file:///C:/My%20Music/x.mp3"), PathBuf::from(r"C:\My Music\x.mp3"));
            assert_eq!(resolve_entry(&base, r"\\nas\share\x.mp3"), PathBuf::from(r"\\nas\share\x.mp3"));
            assert_eq!(resolve_entry(&base, "file://nas/share/x.mp3"), PathBuf::from(r"\\nas\share\x.mp3"));
        } else {
            assert_eq!(resolve_entry(&base, "file:///home/u/My%20Music/x.mp3"), PathBuf::from("/home/u/My Music/x.mp3"));
        }
    }

    #[test]
    fn pls_reads_entries_in_numeric_order_and_writes_flat() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("radio.pls");
        std::fs::write(
            &file,
            "[playlist]\nFile2=b.mp3\nTitle2=B\nFile10=c.mp3\nFile1=a.mp3\nLength1=60\nNumberOfEntries=3\nVersion=2\n",
        )
        .unwrap();
        let loaded = load(&file).unwrap();
        let names: Vec<_> = loaded.tracks.iter().map(|t| t.file_name()).collect();
        assert_eq!(names, vec!["a.mp3", "b.mp3", "c.mp3"]);

        let out = dir.path().join("out.pls");
        let base = crate::paths::normalize(dir.path());
        save(&out, &sample_tree(&base)).unwrap();
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(text.contains("NumberOfEntries=4"));
        assert_eq!(load(&out).unwrap().tracks.len(), 4);
    }

    #[test]
    fn unknown_extension_is_rejected() {
        assert!(matches!(load(Path::new("x.txt")), Err(PlaylistError::UnknownFormat)));
    }
}
