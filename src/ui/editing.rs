//! Playlist editing: groups, adding files (file manager), and background metadata work.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::audio::AudioCommand;
use crate::library::fs_browser::{self, EntryKind};
use crate::model::{Group, Track, TrackMetadata};
use crate::playlist::engine::PlayState;

use super::app::{App, BannerLevel, FolderScan, Row, View};
use super::file_manager::FmMode;
use super::keymap::Action;
use super::prompt::{Confirm, Prompt, PromptKind};

/// Rewrites a stored group path after the children of `parent` were renumbered by `map`
/// (old index -> new index, `None` if removed). Returns false if the path no longer exists.
fn remap_path(path: &mut [usize], parent: &[usize], map: &impl Fn(usize) -> Option<usize>) -> bool {
    if path.len() <= parent.len() || !path.starts_with(parent) {
        return true;
    }
    match map(path[parent.len()]) {
        Some(new) => {
            path[parent.len()] = new;
            true
        }
        None => false,
    }
}

impl App {
    /// Call after any playlist edit.
    fn edited(&mut self) {
        self.version += 1;
        self.requeue();
    }

    /// Fixes group paths held elsewhere after subgroups of `parent` were renumbered.
    fn remap_groups(&mut self, parent: &[usize], map: impl Fn(usize) -> Option<usize>) {
        let mut anchor = self.engine.anchor().to_vec();
        if !remap_path(&mut anchor, parent, &map) {
            anchor = parent.to_vec();
        }
        self.engine.set_anchor(anchor);
        for scan in &mut self.scans {
            if !remap_path(&mut scan.target, parent, &map) {
                scan.target = parent.to_vec();
            }
        }
    }

    pub(crate) fn toggle_group_shuffle(&mut self) {
        let cwd = self.cwd.clone();
        if let Some(g) = self.root.group_at_mut(&cwd) {
            g.shuffle = !g.shuffle;
            let text = format!("Shuffle {}: {}", g.name, if g.shuffle { "ON" } else { "OFF" });
            self.banner(BannerLevel::Info, text);
            self.edited();
        }
    }

    pub(crate) fn create_group(&mut self, name: String) {
        if name.is_empty() {
            return;
        }
        let id = self.root.max_id() + 1;
        let cwd = self.cwd.clone();
        let Some(parent) = self.root.group_at_mut(&cwd) else { return };
        parent.subgroups.push(Group::new(id, name.clone()));
        // The new group is the last of the group rows.
        self.cursor = parent.subgroups.len() - 1;
        self.banner(BannerLevel::Info, format!("Created group \"{name}\""));
        self.edited();
    }

    pub(crate) fn start_rename(&mut self) {
        match self.cursor_row() {
            Some(Row::Group(i)) => {
                let name = self.current_group().subgroups[i].name.clone();
                self.prompt = Some(Prompt::new(PromptKind::RenameGroup, "Rename group: ", name));
            }
            _ => self.banner(BannerLevel::Info, "Move the cursor to a group to rename it"),
        }
    }

    pub(crate) fn rename_group(&mut self, name: String) {
        let Some(Row::Group(i)) = self.cursor_row() else { return };
        if name.is_empty() {
            return;
        }
        let cwd = self.cwd.clone();
        if let Some(g) = self.root.group_at_mut(&cwd).and_then(|p| p.subgroups.get_mut(i)) {
            g.name = name;
        }
    }

    pub(crate) fn delete_at_cursor(&mut self) {
        match self.cursor_row() {
            Some(Row::Group(i)) => {
                let g = &self.current_group().subgroups[i];
                let label = format!("Delete group \"{}\" and its {} track(s)?", g.name, g.total_tracks());
                let mut path = self.cwd.clone();
                path.push(i);
                self.prompt = Some(Prompt::new(PromptKind::Confirm(Confirm::DeleteGroup(path)), label, ""));
            }
            Some(Row::Track(i)) => {
                let cwd = self.cwd.clone();
                let Some(uid) = self.current_group().tracks.get(i).map(|t| t.uid) else { return };
                self.remove_tracks(|g, path| {
                    if path == cwd.as_slice() {
                        g.tracks.retain(|t| t.uid != uid);
                    }
                });
            }
            None => {}
        }
    }

    pub(crate) fn confirmed(&mut self, confirm: Confirm) {
        match confirm {
            Confirm::DeleteGroup(path) => {
                let Some((&index, parent)) = path.split_last() else { return };
                let parent = parent.to_vec();
                let mut removed = None;
                self.remove_tracks(|g, p| {
                    if p == parent.as_slice() && index < g.subgroups.len() {
                        removed = Some(g.subgroups.remove(index).name);
                    }
                });
                self.remap_groups(&parent, |i| match i.cmp(&index) {
                    std::cmp::Ordering::Less => Some(i),
                    std::cmp::Ordering::Equal => None,
                    std::cmp::Ordering::Greater => Some(i - 1),
                });
                if let Some(name) = removed {
                    self.banner(BannerLevel::Info, format!("Deleted group \"{name}\""));
                }
            }
            Confirm::Overwrite(path) => self.save_playlist_to(&path),
        }
    }

    /// Applies a removal to the group tree, keeping playback continuous: if the playing
    /// track disappears, its successor (computed beforehand) stays queued.
    fn remove_tracks(&mut self, mut edit: impl FnMut(&mut Group, &[usize])) {
        let follow = self.playing.and_then(|p| {
            let st = PlayState { root: &self.root, mode: self.play_mode, repeat: self.repeat, version: self.version };
            self.engine.successor(&st, p)
        });
        fn apply(g: &mut Group, path: &mut Vec<usize>, edit: &mut impl FnMut(&mut Group, &[usize])) {
            edit(g, path);
            for i in 0..g.subgroups.len() {
                path.push(i);
                apply(&mut g.subgroups[i], path, edit);
                path.pop();
            }
        }
        apply(&mut self.root, &mut Vec::new(), &mut edit);
        self.version += 1;
        let playing_gone = self.playing.is_some_and(|p| self.root.locate(p).is_none());
        if playing_gone {
            let next = follow.filter(|&u| self.root.locate(u).is_some());
            self.pending_queue = next;
            let path = next.and_then(|u| self.root.track_by_uid(u)).map(|t| t.path.clone());
            self.audio.send(AudioCommand::QueueNext(path));
        } else {
            self.requeue();
        }
        let rows = self.rows().len();
        self.cursor = self.cursor.min(rows.saturating_sub(1));
    }

    pub(crate) fn move_at_cursor(&mut self, delta: isize) {
        let cwd = self.cwd.clone();
        let Some(row) = self.cursor_row() else { return };
        let Some(group) = self.root.group_at_mut(&cwd) else { return };
        match row {
            Row::Group(i) => {
                let j = i as isize + delta;
                if j < 0 || j as usize >= group.subgroups.len() {
                    return;
                }
                let j = j as usize;
                group.subgroups.swap(i, j);
                self.cursor = j;
                self.remap_groups(&cwd, |k| Some(if k == i { j } else if k == j { i } else { k }));
            }
            Row::Track(i) => {
                let j = i as isize + delta;
                if j < 0 || j as usize >= group.tracks.len() {
                    return;
                }
                group.tracks.swap(i, j as usize);
                self.cursor = (self.cursor as isize + delta) as usize;
            }
        }
        self.edited();
    }

    // ---- file manager -------------------------------------------------------------------

    /// Handles file-manager specific actions. Returns false for actions it does not handle.
    pub(crate) fn on_file_manager_action(&mut self, action: Action) -> bool {
        let page = self.fm.height.max(1) as isize;
        match action {
            Action::CursorUp => self.fm.move_cursor(-1),
            Action::CursorDown => self.fm.move_cursor(1),
            Action::PageUp => self.fm.move_cursor(-page),
            Action::PageDown => self.fm.move_cursor(page),
            Action::Home => self.fm.cursor = 0,
            Action::End => self.fm.cursor = self.fm.entries.len().saturating_sub(1),
            Action::Back => self.fm.up(),
            Action::Activate => {
                if let Some(entry) = self.fm.enter() {
                    match (self.fm.mode, entry.kind) {
                        (FmMode::AddFiles, EntryKind::Audio) => self.add_files(vec![entry.path]),
                        (FmMode::AddFiles, EntryKind::Playlist) => self.import_playlist(&entry.path),
                        (FmMode::PickPlaylist, EntryKind::Playlist) => {
                            self.load_playlist(&entry.path);
                            self.view = View::Playlist;
                        }
                        _ => {}
                    }
                }
            }
            Action::Select => self.fm.toggle_current(),
            Action::InvertSelection => self.fm.invert_selection(),
            Action::SelectGlob => {
                self.prompt = Some(Prompt::new(PromptKind::SelectGlob, "Select (glob, or re:regex): ", "*."));
            }
            Action::AddSelected => self.add_selection(),
            Action::ToggleHidden => {
                self.fm.show_hidden = !self.fm.show_hidden;
                self.fm.reload();
                let state = if self.fm.show_hidden { "shown" } else { "hidden" };
                self.banner(BannerLevel::Info, format!("Hidden files {state}"));
            }
            Action::Close | Action::FileManager => self.view = View::Playlist,
            _ => return false,
        }
        true
    }

    fn add_selection(&mut self) {
        let picked = self.fm.take_selection();
        if picked.is_empty() {
            return;
        }
        for playlist in picked.iter().filter(|e| e.kind == EntryKind::Playlist) {
            self.import_playlist(&playlist.path);
        }
        let has_dirs = picked.iter().any(|e| e.kind == EntryKind::Dir);
        let paths: Vec<PathBuf> = picked
            .into_iter()
            .filter(|e| matches!(e.kind, EntryKind::Dir | EntryKind::Audio))
            .map(|e| e.path)
            .collect();
        if paths.is_empty() {
            return;
        }
        if !has_dirs {
            self.add_files(paths);
            return;
        }
        // Folders are walked in the background; files go through the same worker so the
        // playlist keeps the order of the selection.
        self.scans.push(FolderScan {
            files: fs_browser::scan_recursive(paths),
            target: self.cwd.clone(),
            added: 0,
        });
        self.banner(BannerLevel::Info, "Scanning folders…");
    }

    /// Appends files to the group shown in the playlist view.
    pub fn add_files(&mut self, files: Vec<PathBuf>) {
        let target = self.cwd.clone();
        let n = self.add_tracks_to(&target, files);
        self.banner(BannerLevel::Info, format!("Added {n} file(s) to {}", self.current_group().name));
    }

    pub(crate) fn add_tracks_to(&mut self, group: &[usize], files: Vec<PathBuf>) -> usize {
        let n = files.len();
        for path in &files {
            self.scanner.request(path);
        }
        let cache = &self.meta_cache;
        let tracks = files.into_iter().map(|path| {
            let mut t = Track::new(path);
            t.metadata = cache.get(&t.path).cloned();
            t
        });
        let target = if self.root.group_at(group).is_some() { group } else { &[] };
        if let Some(g) = self.root.group_at_mut(target) {
            g.tracks.extend(tracks);
        }
        if n > 0 {
            self.edited();
        }
        n
    }

    // ---- background work ----------------------------------------------------------------

    pub(crate) fn request_all_metadata(&mut self) {
        let mut paths = Vec::new();
        self.root.walk(&mut |g| paths.extend(g.tracks.iter().filter(|t| t.metadata.is_none()).map(|t| t.path.clone())));
        for p in paths {
            self.scanner.request(&p);
        }
    }

    /// Applies metadata results and folder-scan batches. Returns true if anything changed.
    pub fn process_background(&mut self) -> bool {
        let mut changed = false;

        let results: HashMap<PathBuf, TrackMetadata> =
            self.scanner.results().filter_map(|(p, m)| m.map(|m| (p, m))).collect();
        if !results.is_empty() {
            self.root.walk_mut(&mut |g| {
                for t in g.tracks.iter_mut().filter(|t| t.metadata.is_none()) {
                    if let Some(m) = results.get(&t.path) {
                        t.metadata = Some(m.clone());
                    }
                }
            });
            self.meta_cache.extend(results);
            changed = true;
        }

        let mut finished = Vec::new();
        for i in 0..self.scans.len() {
            let mut batches = Vec::new();
            let done = loop {
                match self.scans[i].files.try_recv() {
                    Ok(batch) => batches.push(batch),
                    Err(crossbeam_channel::TryRecvError::Empty) => break false,
                    Err(crossbeam_channel::TryRecvError::Disconnected) => break true,
                }
            };
            for batch in batches {
                let target = self.scans[i].target.clone();
                self.scans[i].added += self.add_tracks_to(&target, batch);
                changed = true;
            }
            if done {
                finished.push(i);
            }
        }
        for i in finished.into_iter().rev() {
            let scan = self.scans.remove(i);
            self.banner(BannerLevel::Info, format!("Added {} file(s) from folders", scan.added));
            changed = true;
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remaps_paths_below_parent_only() {
        let shift = |i: usize| if i == 1 { None } else if i > 1 { Some(i - 1) } else { Some(i) };
        let mut p = vec![0, 2, 5];
        assert!(remap_path(&mut p, &[0], &shift));
        assert_eq!(p, vec![0, 1, 5]);
        let mut p = vec![0, 1];
        assert!(!remap_path(&mut p, &[0], &shift));
        let mut p = vec![3, 2];
        assert!(remap_path(&mut p, &[0], &shift));
        assert_eq!(p, vec![3, 2]);
        let mut p = vec![0];
        assert!(remap_path(&mut p, &[0], &shift));
    }
}
