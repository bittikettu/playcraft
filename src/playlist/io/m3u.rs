//! Extended M3U. Groups are stored as `#EXTGRP:` lines; nested groups as "Parent / Child".

use std::fmt::Write;
use std::path::Path;

use super::{entry_for, resolve_entry};
use crate::model::{Group, Track};

const NEST_SEPARATOR: &str = " / ";

/// Finds or creates the group at the `/`-separated `path` below `root`. Returns its index path.
fn ensure_group(root: &mut Group, path: &str) -> Vec<usize> {
    let mut indices = Vec::new();
    let mut g = root;
    for name in path.split(NEST_SEPARATOR).map(str::trim).filter(|n| !n.is_empty()) {
        let i = match g.subgroups.iter().position(|s| s.name == name) {
            Some(i) => i,
            None => {
                g.subgroups.push(Group::new(0, name));
                g.subgroups.len() - 1
            }
        };
        indices.push(i);
        g = &mut g.subgroups[i];
    }
    indices
}

pub fn parse(text: &str, base: &Path) -> Group {
    let mut root = Group::new(0, "");
    let mut current: Vec<usize> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix("#EXTGRP:") {
            current = ensure_group(&mut root, name);
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let track = Track::new(resolve_entry(base, line));
        if let Some(g) = root.group_at_mut(&current) {
            g.tracks.push(track);
        }
    }
    root
}

pub fn write(root: &Group, base: &Path) -> String {
    let mut out = String::from("#EXTM3U\r\n");
    fn visit(g: &Group, names: &mut Vec<String>, base: &Path, out: &mut String) {
        if !g.tracks.is_empty() {
            if !names.is_empty() {
                let _ = write!(out, "#EXTGRP:{}\r\n", names.join(NEST_SEPARATOR));
            }
            for t in &g.tracks {
                let secs = t.duration().map_or(-1, |d| d.as_secs() as i64);
                let _ = write!(out, "#EXTINF:{secs},{}\r\n{}\r\n", t.display_name(), entry_for(base, &t.path));
            }
        }
        for sub in &g.subgroups {
            names.push(sub.name.replace(NEST_SEPARATOR, " - "));
            visit(sub, names, base, out);
            names.pop();
        }
    }
    visit(root, &mut Vec::new(), base, &mut out);
    out
}
