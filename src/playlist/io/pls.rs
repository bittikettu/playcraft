//! PLS (INI-style) playlists. PLS has no groups, so the tree is flattened on write.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::Path;

use super::{entry_for, resolve_entry};
use crate::model::{Group, Track};

pub fn parse(text: &str, base: &Path) -> Group {
    let mut files = BTreeMap::new();
    for line in text.lines() {
        let Some((key, value)) = line.trim().split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.len() > 4
            && key[..4].eq_ignore_ascii_case("file")
            && let Ok(n) = key[4..].parse::<u32>()
        {
            files.insert(n, value.trim().to_string());
        }
    }
    let mut root = Group::new(0, "");
    root.tracks = files
        .values()
        .map(|f| Track::new(resolve_entry(base, f)))
        .collect();
    root
}

pub fn write(root: &Group, base: &Path) -> String {
    let mut tracks = Vec::new();
    root.walk(&mut |g| tracks.extend(g.tracks.iter()));
    let mut out = String::from("[playlist]\r\n");
    for (i, t) in tracks.iter().enumerate() {
        let n = i + 1;
        let secs = t.duration().map_or(-1, |d| d.as_secs() as i64);
        let _ = write!(
            out,
            "File{n}={}\r\nTitle{n}={}\r\nLength{n}={secs}\r\n",
            entry_for(base, &t.path),
            t.display_name()
        );
    }
    let _ = write!(out, "NumberOfEntries={}\r\nVersion=2\r\n", tracks.len());
    out
}
