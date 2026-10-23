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
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct ReplayGain {
    pub track_gain_db: Option<f32>,
    pub track_peak: Option<f32>,
    pub album_gain_db: Option<f32>,
    pub album_peak: Option<f32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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

static NEXT_UID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// A process-unique track identity. Track positions shift when the playlist is edited;
/// uids do not, so playback state refers to tracks by uid.
pub fn next_uid() -> u64 {
    NEXT_UID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Track {
    #[serde(skip, default = "next_uid")]
    pub uid: u64,
    pub path: PathBuf,
    /// Loaded lazily via the metadata worker pool.
    #[serde(skip)]
    pub metadata: Option<TrackMetadata>,
    /// Set when the file could not be found during playback.
    #[serde(skip)]
    pub missing: bool,
    /// Set when the file exists but could not be decoded.
    #[serde(skip)]
    pub broken: bool,
}

impl Track {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Track {
            uid: next_uid(),
            path: path.into(),
            metadata: None,
            missing: false,
            broken: false,
        }
    }

    pub fn is_playable(&self) -> bool {
        !self.missing && !self.broken
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
        self.metadata
            .as_ref()
            .map(|m| m.duration)
            .filter(|d| !d.is_zero())
    }
}

/// Address of a track in a group tree: subgroup indices from the root, then the track index.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct TrackRef {
    pub group: Vec<usize>,
    pub index: usize,
}

impl TrackRef {
    pub fn new(group: Vec<usize>, index: usize) -> Self {
        TrackRef { group, index }
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
        Group {
            id,
            name: name.into(),
            ..Default::default()
        }
    }

    /// Number of tracks in this group and all its subgroups.
    pub fn total_tracks(&self) -> usize {
        self.tracks.len()
            + self
                .subgroups
                .iter()
                .map(Group::total_tracks)
                .sum::<usize>()
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

    /// The descendant at `path` (subgroup indices from this group).
    pub fn group_at(&self, path: &[usize]) -> Option<&Group> {
        path.iter().try_fold(self, |g, &i| g.subgroups.get(i))
    }

    pub fn group_at_mut(&mut self, path: &[usize]) -> Option<&mut Group> {
        path.iter().try_fold(self, |g, &i| g.subgroups.get_mut(i))
    }

    pub fn track(&self, r: &TrackRef) -> Option<&Track> {
        self.group_at(&r.group)?.tracks.get(r.index)
    }

    pub fn track_mut(&mut self, r: &TrackRef) -> Option<&mut Track> {
        self.group_at_mut(&r.group)?.tracks.get_mut(r.index)
    }

    /// Every track in this tree, depth-first in playlist order (own tracks before subgroups').
    pub fn track_refs(&self) -> Vec<TrackRef> {
        fn collect(g: &Group, path: &mut Vec<usize>, out: &mut Vec<TrackRef>) {
            out.extend((0..g.tracks.len()).map(|i| TrackRef::new(path.clone(), i)));
            for (i, sub) in g.subgroups.iter().enumerate() {
                path.push(i);
                collect(sub, path, out);
                path.pop();
            }
        }
        let mut out = Vec::new();
        collect(self, &mut Vec::new(), &mut out);
        out
    }

    /// Finds a track by file path, preferring the first match in playlist order.
    pub fn find_path(&self, path: &std::path::Path) -> Option<TrackRef> {
        self.track_refs()
            .into_iter()
            .find(|r| self.track(r).is_some_and(|t| t.path == path))
    }

    /// Where the track with `uid` currently is.
    pub fn locate(&self, uid: u64) -> Option<TrackRef> {
        fn search(g: &Group, uid: u64, path: &mut Vec<usize>) -> Option<TrackRef> {
            if let Some(i) = g.tracks.iter().position(|t| t.uid == uid) {
                return Some(TrackRef::new(path.clone(), i));
            }
            for (i, sub) in g.subgroups.iter().enumerate() {
                path.push(i);
                if let Some(found) = search(sub, uid, path) {
                    return Some(found);
                }
                path.pop();
            }
            None
        }
        search(self, uid, &mut Vec::new())
    }

    pub fn track_by_uid(&self, uid: u64) -> Option<&Track> {
        self.locate(uid).and_then(|r| self.track(&r))
    }

    pub fn track_by_uid_mut(&mut self, uid: u64) -> Option<&mut Track> {
        let r = self.locate(uid)?;
        self.track_mut(&r)
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
        assert_eq!(
            ids,
            vec![("Root".into(), 1), ("Rock".into(), 2), ("Prog".into(), 3)]
        );
        assert_eq!(root.max_id(), 3);
    }

    #[test]
    fn addresses_tracks_by_path() {
        let root = tree();
        let refs = root.track_refs();
        let names: Vec<_> = refs
            .iter()
            .map(|r| root.track(r).unwrap().file_name())
            .collect();
        assert_eq!(names, vec!["a.mp3", "b.mp3", "c.mp3", "d.mp3"]);
        assert_eq!(refs[3], TrackRef::new(vec![0, 0], 0));
        assert_eq!(root.group_at(&[0, 0]).unwrap().name, "Prog");
        assert!(root.group_at(&[1]).is_none());
        assert_eq!(
            root.find_path(std::path::Path::new("c.mp3")),
            Some(TrackRef::new(vec![0], 1))
        );
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
