//! Loading, importing and saving playlist files from the UI.

use std::path::Path;

use super::app::{App, BannerLevel};

impl App {
    /// Replaces the playlist with the contents of a playlist file.
    pub fn load_playlist(&mut self, path: &Path) {
        self.banner(BannerLevel::Info, format!("Loading {}: not available yet", path.display()));
    }

    /// Adds a playlist file's tracks (and groups) to the current group.
    pub fn import_playlist(&mut self, path: &Path) {
        self.banner(BannerLevel::Info, format!("Importing {}: not available yet", path.display()));
    }

    pub fn save_playlist_to(&mut self, path: &Path) {
        self.banner(BannerLevel::Info, format!("Saving {}: not available yet", path.display()));
    }
}
