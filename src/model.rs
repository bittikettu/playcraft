//! Core data structures shared across the player (spec §4).

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PlaybackStatus {
    Playing,
    Paused,
    #[default]
    Stopped,
}

impl PlaybackStatus {
    pub fn label(self) -> &'static str {
        match self {
            PlaybackStatus::Playing => "PLAYING",
            PlaybackStatus::Paused => "PAUSED",
            PlaybackStatus::Stopped => "STOPPED",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RepeatMode {
    #[default]
    Off,
    RepeatTrack,
    RepeatGroup,
    RepeatAll,
}

impl RepeatMode {
    pub fn cycle(self) -> Self {
        match self {
            RepeatMode::Off => RepeatMode::RepeatTrack,
            RepeatMode::RepeatTrack => RepeatMode::RepeatGroup,
            RepeatMode::RepeatGroup => RepeatMode::RepeatAll,
            RepeatMode::RepeatAll => RepeatMode::Off,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RepeatMode::Off => "OFF",
            RepeatMode::RepeatTrack => "TRK",
            RepeatMode::RepeatGroup => "GRP",
            RepeatMode::RepeatAll => "ALL",
        }
    }
}

/// Playlist traversal order, following the play modes of the original MP3Blaster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PlayMode {
    /// Play only the group (and its subgroups) the playing track belongs to.
    CurrentGroup,
    /// Play every group in playlist order.
    #[default]
    AllGroups,
    /// Play every group, choosing the group order randomly; each group plays as a whole.
    AllGroupsShuffled,
    /// Play every track in the playlist in random order.
    AllTracksShuffled,
}

impl PlayMode {
    pub fn cycle(self) -> Self {
        match self {
            PlayMode::CurrentGroup => PlayMode::AllGroups,
            PlayMode::AllGroups => PlayMode::AllGroupsShuffled,
            PlayMode::AllGroupsShuffled => PlayMode::AllTracksShuffled,
            PlayMode::AllTracksShuffled => PlayMode::CurrentGroup,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PlayMode::CurrentGroup => "Group",
            PlayMode::AllGroups => "All",
            PlayMode::AllGroupsShuffled => "Shuffle groups",
            PlayMode::AllTracksShuffled => "Shuffle all",
        }
    }
}

/// ReplayGain values read from tags. Gains are in dB, peaks are linear amplitude.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ReplayGain {
    pub track_gain_db: Option<f32>,
    pub track_peak: Option<f32>,
    pub album_gain_db: Option<f32>,
    pub album_peak: Option<f32>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrackMetadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub year: Option<u32>,
    pub genre: Option<String>,
    pub bitrate_kbps: u32,
    pub sample_rate_hz: u32,
    pub channels: u8,
    pub duration: Duration,
    pub replay_gain: ReplayGain,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Track {
    pub path: PathBuf,
    /// Loaded lazily via the metadata worker pool.
    #[serde(skip)]
    pub metadata: Option<TrackMetadata>,
    /// Set when the file could not be found or opened during playback.
    #[serde(skip)]
    pub missing: bool,
}

impl Track {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Track { path: path.into(), metadata: None, missing: false }
    }

    /// "Artist - Title" when tags are known, otherwise the file name.
    pub fn display_name(&self) -> String {
        if let Some(meta) = &self.metadata {
            match (&meta.artist, &meta.title) {
                (Some(artist), Some(title)) => return format!("{artist} - {title}"),
                (None, Some(title)) => return title.clone(),
                _ => {}
            }
        }
        self.file_name()
    }

    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.to_string_lossy().into_owned())
    }

    pub fn duration(&self) -> Option<Duration> {
        self.metadata.as_ref().map(|m| m.duration).filter(|d| !d.is_zero())
    }
}

/// MP3Blaster's signature feature: hierarchical group playlists.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Group {
    #[serde(default)]
    pub id: usize,
    pub name: String,
    #[serde(default)]
    pub tracks: Vec<Track>,
    #[serde(default)]
    pub subgroups: Vec<Group>,
    /// Play this group's tracks in random order.
    #[serde(default)]
    pub shuffle: bool,
}

impl Group {
    pub fn new(id: usize, name: impl Into<String>) -> Self {
        Group { id, name: name.into(), ..Default::default() }
    }

    /// Number of tracks in this group and all its subgroups.
    pub fn total_tracks(&self) -> usize {
        self.tracks.len() + self.subgroups.iter().map(Group::total_tracks).sum::<usize>()
    }

    /// Calls `f` for this group and every descendant, depth-first, pre-order.
    pub fn walk<'a>(&'a self, f: &mut impl FnMut(&'a Group)) {
        f(self);
        for g in &self.subgroups {
            g.walk(f);
        }
    }

    pub fn walk_mut(&mut self, f: &mut impl FnMut(&mut Group)) {
        f(self);
        for g in &mut self.subgroups {
            g.walk_mut(f);
        }
    }

    /// Assigns fresh, unique ids to this group and all descendants. Returns the next free id.
    pub fn renumber(&mut self, mut next_id: usize) -> usize {
        self.walk_mut(&mut |g| {
            g.id = next_id;
            next_id += 1;
        });
        next_id
    }

    /// Largest id used in this tree.
    pub fn max_id(&self) -> usize {
        let mut max = 0;
        self.walk(&mut |g| max = max.max(g.id));
        max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> Group {
        let mut root = Group::new(0, "Root");
        root.tracks.push(Track::new("a.mp3"));
        let mut rock = Group::new(0, "Rock");
        rock.tracks.push(Track::new("b.mp3"));
        rock.tracks.push(Track::new("c.mp3"));
        let mut prog = Group::new(0, "Prog");
        prog.tracks.push(Track::new("d.mp3"));
        rock.subgroups.push(prog);
        root.subgroups.push(rock);
        root
    }

    #[test]
    fn counts_tracks_recursively() {
        assert_eq!(tree().total_tracks(), 4);
    }

    #[test]
    fn renumber_assigns_unique_ids_preorder() {
        let mut root = tree();
        let next = root.renumber(1);
        assert_eq!(next, 4);
        let mut ids = Vec::new();
        root.walk(&mut |g| ids.push((g.name.clone(), g.id)));
        assert_eq!(ids, vec![("Root".into(), 1), ("Rock".into(), 2), ("Prog".into(), 3)]);
        assert_eq!(root.max_id(), 3);
    }

    #[test]
    fn display_name_falls_back_to_file_name() {
        let mut t = Track::new(PathBuf::from("music").join("song.flac"));
        assert_eq!(t.display_name(), "song.flac");
        t.metadata = Some(TrackMetadata {
            title: Some("Money".into()),
            artist: Some("Pink Floyd".into()),
            ..Default::default()
        });
        assert_eq!(t.display_name(), "Pink Floyd - Money");
    }

    #[test]
    fn modes_cycle_through_all_variants() {
        let mut m = RepeatMode::Off;
        for _ in 0..4 {
            m = m.cycle();
        }
        assert_eq!(m, RepeatMode::Off);
        let mut p = PlayMode::AllGroups;
        for _ in 0..4 {
            p = p.cycle();
        }
        assert_eq!(p, PlayMode::AllGroups);
    }
}
