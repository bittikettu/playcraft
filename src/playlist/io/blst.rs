//! The native `.blst` format: the full group tree as versioned JSON.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{PlaylistError, entry_for, resolve_entry};
use crate::model::{Group, Track};

const FORMAT: &str = "blst";
const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct BlstFile {
    format: String,
    version: u32,
    root: BlstGroup,
}

#[derive(Serialize, Deserialize)]
struct BlstGroup {
    name: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    shuffle: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tracks: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    subgroups: Vec<BlstGroup>,
}

fn to_blst(g: &Group, base: &Path) -> BlstGroup {
    BlstGroup {
        name: g.name.clone(),
        shuffle: g.shuffle,
        tracks: g.tracks.iter().map(|t| entry_for(base, &t.path)).collect(),
        subgroups: g.subgroups.iter().map(|s| to_blst(s, base)).collect(),
    }
}

fn from_blst(g: BlstGroup, base: &Path) -> Group {
    Group {
        id: 0,
        name: g.name,
        shuffle: g.shuffle,
        tracks: g.tracks.iter().map(|p| Track::new(resolve_entry(base, p))).collect(),
        subgroups: g.subgroups.into_iter().map(|s| from_blst(s, base)).collect(),
    }
}

pub fn parse(text: &str, base: &Path) -> Result<Group, PlaylistError> {
    let file: BlstFile = serde_json::from_str(text).map_err(|e| PlaylistError::Parse(e.to_string()))?;
    if file.format != FORMAT {
        return Err(PlaylistError::Parse(format!("unexpected format \"{}\"", file.format)));
    }
    if file.version > VERSION {
        return Err(PlaylistError::Parse(format!("made by a newer version (format {})", file.version)));
    }
    Ok(from_blst(file.root, base))
}

pub fn write(root: &Group, base: &Path) -> Result<String, PlaylistError> {
    let file = BlstFile { format: FORMAT.into(), version: VERSION, root: to_blst(root, base) };
    serde_json::to_string_pretty(&file).map_err(|e| PlaylistError::Parse(e.to_string()))
}

/// Serialises a tree with absolute paths (used for the session file).
pub fn to_json_value(root: &Group) -> serde_json::Value {
    serde_json::to_value(to_blst(root, Path::new(""))).unwrap_or(serde_json::Value::Null)
}

/// Inverse of `to_json_value`.
pub fn from_json_value(value: serde_json::Value) -> Option<Group> {
    let g: BlstGroup = serde_json::from_value(value).ok()?;
    let mut root = from_blst(g, Path::new(""));
    root.renumber(0);
    Some(root)
}
