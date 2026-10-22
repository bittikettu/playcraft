//! File manager state: browsing, selection and pattern matching.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::library::fs_browser::{self, EntryKind, FsEntry, Location};

/// Why the file manager is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FmMode {
    /// Add files and folders to the playlist.
    AddFiles,
    /// Choose a playlist file to load.
    PickPlaylist,
}

pub struct FileManager {
    pub location: Location,
    pub entries: Vec<FsEntry>,
    pub cursor: usize,
    pub scroll: usize,
    pub selected: HashSet<PathBuf>,
    pub show_hidden: bool,
    pub mode: FmMode,
    pub error: Option<String>,
    /// Rows visible (updated while drawing).
    pub height: usize,
}

impl FileManager {
    pub fn new(start: PathBuf) -> Self {
        let mut fm = FileManager {
            location: Location::Dir(start),
            entries: Vec::new(),
            cursor: 0,
            scroll: 0,
            selected: HashSet::new(),
            show_hidden: false,
            mode: FmMode::AddFiles,
            error: None,
            height: 10,
        };
        fm.reload();
        fm
    }

    /// Re-reads the current location, keeping the cursor on the same entry if possible.
    pub fn reload(&mut self) {
        let current = self.current().map(|e| e.path.clone());
        match fs_browser::list(&self.location, self.show_hidden) {
            Ok(entries) => {
                self.entries = entries;
                self.error = None;
            }
            Err(e) => {
                self.error = Some(format!("{}: {e}", self.location.display()));
                // Fall back to the drive list so the user is never stuck.
                if self.location != Location::Roots {
                    self.location = Location::Roots;
                    self.entries =
                        fs_browser::list(&Location::Roots, self.show_hidden).unwrap_or_default();
                }
            }
        }
        self.cursor = current
            .and_then(|p| self.entries.iter().position(|e| e.path == p))
            .unwrap_or(0)
            .min(self.entries.len().saturating_sub(1));
    }

    pub fn current(&self) -> Option<&FsEntry> {
        self.entries.get(self.cursor)
    }

    pub fn go_to(&mut self, location: Location, focus: Option<&Path>) {
        self.location = location;
        self.selected.clear();
        self.scroll = 0;
        self.cursor = 0;
        self.reload();
        if let Some(focus) = focus {
            if let Some(i) = self.entries.iter().position(|e| e.path == focus) {
                self.cursor = i;
            }
        }
    }

    /// Opens the entry under the cursor if it is a folder, drive or "..".
    /// Returns the entry when it is a file instead.
    pub fn enter(&mut self) -> Option<FsEntry> {
        let entry = self.current()?.clone();
        match entry.kind {
            EntryKind::Parent => self.up(),
            EntryKind::Root | EntryKind::Dir => self.go_to(Location::Dir(entry.path), None),
            EntryKind::Audio | EntryKind::Playlist => return Some(entry),
        }
        None
    }

    pub fn up(&mut self) {
        let came_from = match &self.location {
            Location::Dir(d) => Some(d.clone()),
            Location::Roots => None,
        };
        if let Some(parent) = self.location.parent() {
            self.go_to(parent, came_from.as_deref());
        }
    }

    pub fn toggle_current(&mut self) {
        if let Some(entry) = self.current().filter(|e| e.is_selectable()).cloned() {
            if !self.selected.remove(&entry.path) {
                self.selected.insert(entry.path);
            }
        }
        self.cursor = (self.cursor + 1).min(self.entries.len().saturating_sub(1));
    }

    pub fn invert_selection(&mut self) {
        for entry in self.entries.iter().filter(|e| e.is_selectable()) {
            if !self.selected.remove(&entry.path) {
                self.selected.insert(entry.path.clone());
            }
        }
    }

    /// Selects entries whose name matches `pattern`: a case-insensitive glob such as `*.flac`,
    /// or a regular expression when prefixed with `re:`. Returns how many entries matched.
    pub fn select_matching(&mut self, pattern: &str) -> Result<usize, String> {
        let matcher: Box<dyn Fn(&str) -> bool> = if let Some(re) = pattern.strip_prefix("re:") {
            let re = regex_lite::RegexBuilder::new(re)
                .case_insensitive(true)
                .build()
                .map_err(|e| e.to_string())?;
            Box::new(move |name| re.is_match(name))
        } else {
            let glob = globset::GlobBuilder::new(pattern)
                .case_insensitive(true)
                .literal_separator(false)
                .build()
                .map_err(|e| e.to_string())?
                .compile_matcher();
            Box::new(move |name| glob.is_match(name))
        };
        let mut count = 0;
        for entry in self.entries.iter().filter(|e| e.is_selectable()) {
            if matcher(&entry.name) {
                self.selected.insert(entry.path.clone());
                count += 1;
            }
        }
        Ok(count)
    }

    /// The selection in listing order, or the entry under the cursor if nothing is selected.
    pub fn take_selection(&mut self) -> Vec<FsEntry> {
        let picked: Vec<FsEntry> = if self.selected.is_empty() {
            self.current()
                .filter(|e| e.is_selectable())
                .cloned()
                .into_iter()
                .collect()
        } else {
            self.entries
                .iter()
                .filter(|e| self.selected.contains(&e.path))
                .cloned()
                .collect()
        };
        self.selected.clear();
        picked
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let max = self.entries.len().saturating_sub(1) as isize;
        self.cursor = (self.cursor as isize + delta).clamp(0, max.max(0)) as usize;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (tempfile::TempDir, FileManager) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Album")).unwrap();
        for name in ["01 Intro.flac", "02 Song.mp3", "03 Outro.FLAC", "list.m3u"] {
            std::fs::write(dir.path().join(name), b"").unwrap();
        }
        let fm = FileManager::new(dir.path().to_path_buf());
        (dir, fm)
    }

    #[test]
    fn glob_and_regex_selection() {
        let (_dir, mut fm) = sample();
        assert_eq!(fm.select_matching("*.flac").unwrap(), 2);
        fm.selected.clear();
        assert_eq!(fm.select_matching("re:^0[12]").unwrap(), 2);
        assert!(fm.select_matching("re:(").is_err());
    }

    #[test]
    fn invert_skips_parent_entry() {
        let (_dir, mut fm) = sample();
        fm.invert_selection();
        assert_eq!(fm.selected.len(), 5);
        fm.invert_selection();
        assert!(fm.selected.is_empty());
    }

    #[test]
    fn selection_comes_back_in_listing_order() {
        let (_dir, mut fm) = sample();
        fm.select_matching("*.flac").unwrap();
        fm.select_matching("Album").unwrap();
        let names: Vec<_> = fm.take_selection().into_iter().map(|e| e.name).collect();
        assert_eq!(names, vec!["Album", "01 Intro.flac", "03 Outro.FLAC"]);
        assert!(fm.selected.is_empty());
    }

    #[test]
    fn enter_and_up_keep_focus() {
        let (dir, mut fm) = sample();
        fm.cursor = 1; // "Album"
        assert!(fm.enter().is_none());
        assert_eq!(fm.location, Location::Dir(dir.path().join("Album")));
        fm.up();
        assert_eq!(fm.current().unwrap().name, "Album");
    }
}
