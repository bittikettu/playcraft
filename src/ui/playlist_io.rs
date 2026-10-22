//! Loading, importing and saving playlist files from the UI.

use std::path::{Path, PathBuf};

use crate::library::fs_browser::Location;
use crate::playlist::io;

use super::app::{App, BannerLevel};
use super::prompt::{Confirm, Prompt, PromptKind};

impl App {
    /// Replaces the playlist with the contents of a playlist file.
    pub fn load_playlist(&mut self, path: &Path) {
        match io::load(path) {
            Ok(root) => {
                let n = root.total_tracks();
                self.root = root;
                self.root.name = "Root".into();
                self.cwd.clear();
                self.cursor = 0;
                self.scroll = 0;
                self.playlist_path = Some(path.to_path_buf());
                self.version += 1;
                self.engine.set_anchor(Vec::new());
                self.request_all_metadata();
                self.requeue();
                self.banner(BannerLevel::Info, format!("Loaded {} ({n} tracks)", file_name(path)));
            }
            Err(e) => self.banner(BannerLevel::Error, format!("{}: {e}", file_name(path))),
        }
    }

    /// Adds a playlist file as a new group (named after the file) in the current group.
    pub fn import_playlist(&mut self, path: &Path) {
        match io::load(path) {
            Ok(mut group) => {
                let n = group.total_tracks();
                let next_id = self.root.max_id() + 1;
                group.renumber(next_id);
                let cwd = self.cwd.clone();
                if let Some(parent) = self.root.group_at_mut(&cwd) {
                    parent.subgroups.push(group);
                }
                self.version += 1;
                self.request_all_metadata();
                self.requeue();
                self.banner(BannerLevel::Info, format!("Imported {} as a group ({n} tracks)", file_name(path)));
            }
            Err(e) => self.banner(BannerLevel::Error, format!("{}: {e}", file_name(path))),
        }
    }

    /// Asks for a file name to save the playlist to.
    pub fn start_save(&mut self) {
        let default = self.playlist_path.clone().unwrap_or_else(|| self.save_dir().join("playlist.blst"));
        self.prompt = Some(Prompt::new(
            PromptKind::SavePlaylist,
            "Save as (.blst/.m3u8/.m3u/.pls): ",
            crate::paths::display(&default),
        ));
    }

    /// Folder used for relative file names typed into the save prompt.
    fn save_dir(&self) -> PathBuf {
        match &self.fm.location {
            Location::Dir(d) => d.clone(),
            Location::Roots => crate::paths::default_music_dir(),
        }
    }

    pub(crate) fn save_answer(&mut self, answer: String) {
        if answer.is_empty() {
            return;
        }
        let mut path = crate::paths::expand_tilde(&answer);
        if path.is_relative() {
            path = self.save_dir().join(path);
        }
        if io::PlaylistFormat::from_path(&path).is_none() {
            path.set_extension("blst");
        }
        if path.exists() && self.playlist_path.as_deref() != Some(path.as_path()) {
            let label = format!("{} exists. Overwrite?", file_name(&path));
            self.prompt = Some(Prompt::new(PromptKind::Confirm(Confirm::Overwrite(path)), label, ""));
        } else {
            self.save_playlist_to(&path);
        }
    }

    pub fn save_playlist_to(&mut self, path: &Path) {
        match io::save(path, &self.root) {
            Ok(()) => {
                self.playlist_path = Some(path.to_path_buf());
                self.banner(BannerLevel::Info, format!("Saved {}", crate::paths::display(path)));
            }
            Err(e) => self.banner(BannerLevel::Error, format!("Cannot save {}: {e}", file_name(path))),
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}
