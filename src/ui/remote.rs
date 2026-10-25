//! Tool calls from AI assistants (MCP; see `remote.rs` and `mcp/`). They arrive on the UI
//! thread and act on the player like key presses do, with a banner saying who did what.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rand::seq::SliceRandom;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use crate::audio::AudioCommand;
use crate::library::index::IndexState;
use crate::model::{Group, PlayMode, PlaybackStatus, RepeatMode, TrackMetadata};
use crate::outputs::Device;
use crate::progress_fx::ProgressStyle;
use crate::remote::RemoteCall;

use super::app::{App, BannerLevel, SpectrumLayout, View, Visualizer};
use super::theme::{THEMES, Theme, supports_truecolor, theme_info};

/// The playlist group that receives music-folder tracks an assistant queues.
pub const REQUESTS_GROUP: &str = "Requests";
/// Queue entries listed by `get_status`.
const QUEUE_SHOWN: usize = 50;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Source {
    #[default]
    All,
    Playlist,
    Library,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct SearchArgs {
    query: Option<String>,
    genre: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    year_from: Option<u32>,
    year_to: Option<u32>,
    source: Source,
    shuffle: bool,
    limit: Option<usize>,
}

#[derive(Debug, Default, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum By {
    #[default]
    Genre,
    Artist,
    Album,
    Year,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct BrowseArgs {
    by: By,
    source: Source,
    limit: Option<usize>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Position {
    #[default]
    End,
    Next,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QueueArgs {
    paths: Vec<String>,
    #[serde(default)]
    position: Position,
    #[serde(default)]
    play_now: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PathsArgs {
    paths: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum PlaybackAction {
    Play,
    Pause,
    Toggle,
    Stop,
    Next,
    Previous,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaybackArgs {
    action: PlaybackAction,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SeekArgs {
    seconds: f64,
    #[serde(default)]
    relative: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct OutputsArgs {
    refresh: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputArgs {
    name: String,
    /// A TV gets the player's pictures too.
    #[serde(default)]
    visuals: bool,
}

/// Names that mean this computer in `set_output`.
const LOCAL_NAMES: &[&str] = &["this computer", "computer", "local", "pc", "here"];

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct VolumeArgs {
    percent: Option<f64>,
    delta: Option<f64>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Style {
    Bars,
    Matrix,
    Defrag,
    Hyperspace,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Layout {
    Wide,
    Pane,
    Off,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct VisualizerArgs {
    style: Option<Style>,
    fullscreen: Option<bool>,
    layout: Option<Layout>,
    progress: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Mode {
    Group,
    All,
    ShuffleGroups,
    ShuffleAll,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Repeat {
    Off,
    Track,
    Group,
    All,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct PlayOptionsArgs {
    mode: Option<Mode>,
    repeat: Option<Repeat>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeArgs {
    name: String,
}

fn parse<T: DeserializeOwned>(args: Value) -> Result<T, String> {
    serde_json::from_value(args).map_err(|e| format!("invalid arguments: {e}"))
}

fn visualizer_name(v: Visualizer) -> &'static str {
    match v {
        Visualizer::Bars => "bars",
        Visualizer::Matrix => "matrix",
        Visualizer::Defrag => "defrag",
        Visualizer::Hyperspace => "hyperspace",
    }
}

fn layout_name(l: SpectrumLayout) -> &'static str {
    match l {
        SpectrumLayout::Wide => "wide",
        SpectrumLayout::Pane => "pane",
        SpectrumLayout::Off => "off",
    }
}

fn mode_name(m: PlayMode) -> &'static str {
    match m {
        PlayMode::CurrentGroup => "group",
        PlayMode::AllGroups => "all",
        PlayMode::AllGroupsShuffled => "shuffle_groups",
        PlayMode::AllTracksShuffled => "shuffle_all",
    }
}

fn repeat_name(r: RepeatMode) -> &'static str {
    match r {
        RepeatMode::Off => "off",
        RepeatMode::RepeatTrack => "track",
        RepeatMode::RepeatGroup => "group",
        RepeatMode::RepeatAll => "all",
    }
}

fn status_name(s: PlaybackStatus) -> &'static str {
    match s {
        PlaybackStatus::Playing => "playing",
        PlaybackStatus::Paused => "paused",
        PlaybackStatus::Stopped => "stopped",
    }
}

/// A track as tools report it; untagged fields are left out.
/// A network device: name, kind and rooms.
fn device_json(device: &Device) -> Value {
    let kind = match device {
        Device::Sonos(_) => "sonos",
        Device::Cast(_) => "google_cast",
    };
    json!({ "name": device.name(), "kind": kind, "rooms": device.rooms() })
}

fn track_json(path: &Path, meta: Option<&TrackMetadata>) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("path".into(), crate::paths::display(path).into());
    if let Some(meta) = meta {
        for (key, value) in [
            ("artist", &meta.artist),
            ("title", &meta.title),
            ("album", &meta.album),
            ("genre", &meta.genre),
        ] {
            if let Some(v) = value {
                m.insert(key.into(), v.clone().into());
            }
        }
        if let Some(year) = meta.year {
            m.insert("year".into(), year.into());
        }
        if !meta.duration.is_zero() {
            m.insert("duration_s".into(), meta.duration.as_secs().into());
        }
    }
    m
}

/// A track that search and browse look at.
struct Candidate<'a> {
    path: &'a Path,
    meta: Option<&'a TrackMetadata>,
    /// Set when the track is in the playlist.
    uid: Option<u64>,
}

/// Tracks from the playlist and/or the index, each path once (the playlist's copy first).
fn candidates<'a>(root: &'a Group, index: &'a IndexState, source: Source) -> Vec<Candidate<'a>> {
    let mut playlist: Vec<Candidate<'a>> = Vec::new();
    let mut in_playlist: HashMap<&'a Path, u64> = HashMap::new();
    root.walk(&mut |g| {
        for t in &g.tracks {
            if in_playlist.insert(&t.path, t.uid).is_none() {
                playlist.push(Candidate {
                    path: &t.path,
                    meta: t.metadata.as_ref(),
                    uid: Some(t.uid),
                });
            }
        }
    });
    let library = index.entries.iter().map(|e| Candidate {
        path: &e.path,
        meta: e.meta.as_ref(),
        uid: in_playlist.get(e.path.as_path()).copied(),
    });
    match source {
        Source::Playlist => playlist,
        Source::Library => library.collect(),
        Source::All => {
            let mut all = playlist;
            all.extend(library.filter(|c| c.uid.is_none()));
            all
        }
    }
}

fn contains(field: Option<&String>, needle: &str) -> bool {
    field.is_some_and(|f| f.to_lowercase().contains(needle))
}

impl SearchArgs {
    fn matches(&self, c: &Candidate<'_>, words: &[String]) -> bool {
        let meta = c.meta;
        let tag = |f: fn(&TrackMetadata) -> Option<&String>| meta.and_then(f);
        let wanted = |filter: &Option<String>, f: fn(&TrackMetadata) -> Option<&String>| {
            filter
                .as_deref()
                .is_none_or(|needle| contains(tag(f), &needle.to_lowercase()))
        };
        if !wanted(&self.genre, |m| m.genre.as_ref())
            || !wanted(&self.artist, |m| m.artist.as_ref())
            || !wanted(&self.album, |m| m.album.as_ref())
        {
            return false;
        }
        if self.year_from.is_some() || self.year_to.is_some() {
            let Some(year) = meta.and_then(|m| m.year) else {
                return false;
            };
            if self.year_from.is_some_and(|y| year < y) || self.year_to.is_some_and(|y| year > y) {
                return false;
            }
        }
        if words.is_empty() {
            return true;
        }
        let mut haystack = c
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if let Some(m) = meta {
            for field in [&m.artist, &m.title, &m.album, &m.genre]
                .into_iter()
                .flatten()
            {
                haystack.push(' ');
                haystack.push_str(&field.to_lowercase());
            }
        }
        words.iter().all(|w| haystack.contains(w.as_str()))
    }
}

impl App {
    /// Runs the tool calls waiting from the remote control. Returns true if any ran.
    pub fn process_remote_calls(&mut self) -> bool {
        let Some(calls) = &self.remote else {
            return false;
        };
        let calls: Vec<RemoteCall> = calls.try_iter().collect();
        let any = !calls.is_empty();
        for call in calls {
            if self.needs_device_search(&call) {
                // Answered when the search is done (it takes a second or two).
                self.search_devices();
                self.waiting_for_devices.push(call);
            } else {
                self.answer_call(call);
            }
        }
        any
    }

    /// Whether `call` should wait for a device search first.
    fn needs_device_search(&self, call: &RemoteCall) -> bool {
        if self.devices.searching.is_some() {
            return matches!(call.tool.as_str(), "list_outputs" | "set_output");
        }
        match call.tool.as_str() {
            "list_outputs" => {
                !self.devices.searched || call.args.get("refresh") == Some(&Value::Bool(true))
            }
            "set_output" => {
                let name = call.args.get("name").and_then(Value::as_str).unwrap_or("");
                let local = LOCAL_NAMES.contains(&name.trim().to_lowercase().as_str());
                !local && !self.devices.searched
            }
            _ => false,
        }
    }

    /// Runs a call and sends back its result.
    pub(crate) fn answer_call(&mut self, call: RemoteCall) {
        let result = self.run_tool(&call.client, &call.tool, call.args.clone());
        match &result {
            Ok(_) => log::info!("{}: {} {}", call.client, call.tool, call.args),
            Err(e) => log::warn!("{}: {} {} failed: {e}", call.client, call.tool, call.args),
        }
        let _ = call.reply.send(result);
    }

    /// Runs one tool for `client` (the assistant's name, for banners).
    pub fn run_tool(&mut self, client: &str, tool: &str, args: Value) -> Result<Value, String> {
        let args = if args.is_null() { json!({}) } else { args };
        // The first call from an assistant starts indexing the music folder.
        self.library.refresh();
        match tool {
            "get_status" => Ok(self.status_json()),
            "browse_library" => Ok(self.browse(&parse(args)?)),
            "search_tracks" => Ok(self.search(&parse(args)?)),
            "queue_tracks" => self.queue_remote(client, parse(args)?),
            "remove_from_queue" => Ok(self.unqueue_remote(client, &parse(args)?)),
            "clear_queue" => {
                let cleared = self.queue.len();
                self.queue.clear();
                self.requeue();
                self.banner(BannerLevel::Info, format!("{client}: cleared the queue"));
                Ok(json!({ "cleared": cleared }))
            }
            "playback" => Ok(self.playback_remote(client, &parse(args)?)),
            "seek" => self.seek_remote(&parse(args)?),
            "set_volume" => self.volume_remote(&parse(args)?),
            "set_visualizer" => self.visualizer_remote(client, &parse(args)?),
            "set_play_options" => Ok(self.play_options_remote(client, &parse(args)?)),
            "set_theme" => self.theme_remote(client, &parse(args)?),
            "list_outputs" => {
                let _: OutputsArgs = parse(args)?;
                Ok(self.outputs_json())
            }
            "set_output" => self.output_remote(client, &parse(args)?),
            other => Err(format!("unknown tool \"{other}\"")),
        }
    }

    fn uid_json(&self, uid: u64) -> Option<Value> {
        let t = self.root.track_by_uid(uid)?;
        Some(Value::Object(track_json(&t.path, t.metadata.as_ref())))
    }

    fn status_json(&self) -> Value {
        let shared = self.audio.shared();
        let mut status = Map::new();
        status.insert("status".into(), status_name(self.status()).into());
        if let Some(now) = &self.now {
            let meta = now
                .track
                .and_then(|u| self.root.track_by_uid(u))
                .and_then(|t| t.metadata.as_ref());
            status.insert("track".into(), Value::Object(track_json(&now.path, meta)));
            let position = (shared.position().as_secs_f64() * 10.0).round() / 10.0;
            status.insert("position_s".into(), position.into());
            if let Some(d) = now.duration {
                status.insert("duration_s".into(), d.as_secs().into());
            }
        }
        status.insert(
            "volume".into(),
            ((shared.volume() * 100.0).round() as u32).into(),
        );
        status.insert("play_mode".into(), mode_name(self.play_mode).into());
        status.insert("repeat".into(), repeat_name(self.repeat).into());
        let next = self.pending_next.or_else(|| self.queue.first().copied());
        if let Some(next) = next.and_then(|u| self.uid_json(u)) {
            status.insert("up_next".into(), next);
        }
        let queue: Vec<Value> = self
            .queue
            .iter()
            .take(QUEUE_SHOWN)
            .filter_map(|&u| self.uid_json(u))
            .collect();
        status.insert("queue".into(), queue.into());
        status.insert("queue_length".into(), self.queue.len().into());
        status.insert(
            "visualizer".into(),
            json!({
                "style": visualizer_name(self.visualizer),
                "fullscreen": self.view == View::Visualizer,
                "layout": layout_name(self.spectrum_layout),
                "progress": self.progress_style.name(),
            }),
        );
        status.insert("theme".into(), self.theme.name.into());
        status.insert("output".into(), self.output_json());
        status.insert("playlist_tracks".into(), self.root.total_tracks().into());
        let index = self.library.read();
        let folders: Vec<String> = self
            .library
            .dirs()
            .iter()
            .map(|d| crate::paths::display(d))
            .collect();
        status.insert(
            "music_folder_index".into(),
            json!({
                "folders": folders,
                "tracks": index.entries.len(),
                "indexing": index.scanning,
                "files_left": index.pending,
            }),
        );
        Value::Object(status)
    }

    /// A note for results while the index is incomplete.
    fn index_note(&self, source: Source) -> Option<String> {
        let index = self.library.read();
        (source != Source::Playlist && index.scanning).then(|| {
            format!(
                "The music folder is still being indexed ({} tracks so far, {} files left); \
                 results may be incomplete.",
                index.entries.len(),
                index.pending
            )
        })
    }

    fn search(&self, args: &SearchArgs) -> Value {
        let index = self.library.read();
        let words: Vec<String> = args
            .query
            .as_deref()
            .unwrap_or("")
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        let mut found: Vec<Candidate<'_>> = candidates(&self.root, &index, args.source)
            .into_iter()
            .filter(|c| args.matches(c, &words))
            .collect();
        let total = found.len();
        if args.shuffle {
            found.shuffle(&mut rand::rng());
        } else {
            found.sort_by(|a, b| {
                crate::library::natural_cmp(&a.path.to_string_lossy(), &b.path.to_string_lossy())
            });
        }
        let limit = args.limit.unwrap_or(25).clamp(1, 200);
        let tracks: Vec<Value> = found
            .iter()
            .take(limit)
            .map(|c| {
                let mut t = track_json(c.path, c.meta);
                t.insert("in_playlist".into(), c.uid.is_some().into());
                if let Some(pos) = c.uid.and_then(|u| self.queue_position(u)) {
                    t.insert("queue_pos".into(), pos.into());
                }
                Value::Object(t)
            })
            .collect();
        let mut result = json!({ "total": total, "returned": tracks.len(), "tracks": tracks });
        drop(index);
        if let Some(note) = self.index_note(args.source) {
            result["note"] = note.into();
        }
        result
    }

    fn browse(&self, args: &BrowseArgs) -> Value {
        let index = self.library.read();
        // Case-insensitive counts, shown with the first spelling met.
        let mut counts: HashMap<String, (String, usize)> = HashMap::new();
        let mut untagged = 0;
        for c in candidates(&self.root, &index, args.source) {
            let value = c.meta.and_then(|m| match args.by {
                By::Genre => m.genre.clone(),
                By::Artist => m.artist.clone(),
                By::Album => m.album.clone(),
                By::Year => m.year.map(|y| y.to_string()),
            });
            match value {
                Some(v) => counts.entry(v.to_lowercase()).or_insert((v, 0)).1 += 1,
                None => untagged += 1,
            }
        }
        let mut values: Vec<(String, usize)> = counts.into_values().collect();
        values.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let distinct = values.len();
        let limit = args.limit.unwrap_or(50).clamp(1, 500);
        let listed: Vec<Value> = values
            .into_iter()
            .take(limit)
            .map(|(value, tracks)| json!({ "value": value, "tracks": tracks }))
            .collect();
        let by = match args.by {
            By::Genre => "genre",
            By::Artist => "artist",
            By::Album => "album",
            By::Year => "year",
        };
        let mut result = json!({
            "by": by,
            "distinct": distinct,
            "values": listed,
            "untagged_tracks": untagged,
        });
        drop(index);
        if let Some(note) = self.index_note(args.source) {
            result["note"] = note.into();
        }
        result
    }

    /// The "Requests" group's index in the root, created when missing.
    fn requests_group(&mut self) -> usize {
        if let Some(i) = self
            .root
            .subgroups
            .iter()
            .position(|g| g.name == REQUESTS_GROUP)
        {
            return i;
        }
        let id = self.root.max_id() + 1;
        self.root.subgroups.push(Group::new(id, REQUESTS_GROUP));
        // Group rows come before tracks: keep the cursor on its row in the root view.
        let groups = self.root.subgroups.len() - 1;
        if self.cwd.is_empty() && self.cursor >= groups {
            self.cursor += 1;
        }
        groups
    }

    fn queue_remote(&mut self, client: &str, args: QueueArgs) -> Result<Value, String> {
        enum Wanted {
            InPlaylist(u64),
            /// Index into the files to add to "Requests".
            New(usize),
        }
        let in_playlist: HashMap<PathBuf, u64> = {
            let mut map = HashMap::new();
            self.root.walk(&mut |g| {
                for t in &g.tracks {
                    map.entry(t.path.clone()).or_insert(t.uid);
                }
            });
            map
        };
        let mut wanted = Vec::new();
        let mut new_files: Vec<PathBuf> = Vec::new();
        let mut skipped = Vec::new();
        for p in &args.paths {
            let path = PathBuf::from(p);
            if let Some(&uid) = in_playlist.get(&path) {
                wanted.push(Wanted::InPlaylist(uid));
            } else if path.is_file() && crate::library::is_audio_file(&path) {
                match new_files.iter().position(|f| *f == path) {
                    Some(i) => wanted.push(Wanted::New(i)),
                    None => {
                        wanted.push(Wanted::New(new_files.len()));
                        new_files.push(path);
                    }
                }
            } else {
                skipped.push(json!({ "path": p, "reason": "no such audio file" }));
            }
        }

        // Music-folder tracks join the "Requests" group, with their tags from the index.
        let mut new_uids = Vec::new();
        if !new_files.is_empty() {
            {
                let index = self.library.read();
                let tags: HashMap<&Path, &TrackMetadata> = index
                    .entries
                    .iter()
                    .filter_map(|e| Some((e.path.as_path(), e.meta.as_ref()?)))
                    .collect();
                for f in &new_files {
                    if let Some(meta) = tags.get(f.as_path()) {
                        self.meta_cache.insert(f.clone(), (*meta).clone());
                    }
                }
            }
            let group = self.requests_group();
            let n = self.add_tracks_to(&[group], new_files);
            let tracks = &self.root.subgroups[group].tracks;
            new_uids = tracks[tracks.len() - n..].iter().map(|t| t.uid).collect();
        }

        let mut queued = Vec::new();
        for w in wanted {
            let uid = match w {
                Wanted::InPlaylist(uid) => uid,
                Wanted::New(i) => new_uids[i],
            };
            let Some(track) = self.root.track_by_uid(uid) else {
                continue;
            };
            let path = crate::paths::display(&track.path);
            if !track.is_playable() {
                skipped.push(json!({ "path": path, "reason": "missing or unplayable" }));
            } else if self.queue.contains(&uid) || queued.contains(&uid) {
                skipped.push(json!({ "path": path, "reason": "already queued" }));
            } else {
                queued.push(uid);
            }
        }
        if queued.is_empty() {
            return Err(format!("nothing was queued: {}", Value::from(skipped)));
        }
        let first = self.uid_json(queued[0]);
        let count = queued.len();
        if args.play_now || args.position == Position::Next {
            self.queue.splice(0..0, queued.iter().copied());
        } else {
            self.queue.extend(queued.iter().copied());
        }
        let name = |app: &App| {
            app.root
                .track_by_uid(queued[0])
                .map(|t| t.display_name())
                .unwrap_or_default()
        };
        let text = if args.play_now {
            let name = name(self);
            // Starts the head of the queue; the normal order stays where it was.
            self.next_track();
            match count {
                1 => format!("{client}: playing {name}"),
                n => format!("{client}: playing {name}, {} more queued", n - 1),
            }
        } else {
            self.requeue();
            match count {
                1 => format!("{client}: queued {}", name(self)),
                n => format!("{client}: queued {n} tracks"),
            }
        };
        self.banner(BannerLevel::Info, text);
        let queued: Vec<Value> = queued
            .iter()
            .filter_map(|&u| {
                let mut t = self.uid_json(u)?;
                t["queue_pos"] = self.queue_position(u).into();
                Some(t)
            })
            .collect();
        Ok(json!({
            "queued": queued,
            "skipped": skipped,
            "queue_length": self.queue.len(),
            "playing_now": if args.play_now { first } else { None },
        }))
    }

    fn unqueue_remote(&mut self, client: &str, args: &PathsArgs) -> Value {
        let paths: HashSet<PathBuf> = args.paths.iter().map(PathBuf::from).collect();
        let before = self.queue.len();
        let root = &self.root;
        self.queue.retain(|&u| {
            root.track_by_uid(u)
                .is_none_or(|t| !paths.contains(&t.path))
        });
        let removed = before - self.queue.len();
        if removed > 0 {
            self.requeue();
            self.banner(
                BannerLevel::Info,
                format!("{client}: removed {removed} track(s) from the queue"),
            );
        }
        json!({ "removed": removed, "queue_length": self.queue.len() })
    }

    fn playback_remote(&mut self, client: &str, args: &PlaybackArgs) -> Value {
        let status = self.status();
        let label = match args.action {
            PlaybackAction::Play => {
                match status {
                    PlaybackStatus::Paused => self.audio.send(AudioCommand::Resume),
                    PlaybackStatus::Stopped => self.play_pause(),
                    PlaybackStatus::Playing => {}
                }
                "play"
            }
            PlaybackAction::Pause => {
                if status == PlaybackStatus::Playing {
                    self.audio.send(AudioCommand::Pause);
                }
                "pause"
            }
            PlaybackAction::Toggle => {
                self.play_pause();
                "play/pause"
            }
            PlaybackAction::Stop => {
                self.stop();
                "stop"
            }
            PlaybackAction::Next => {
                self.next_track();
                "next track"
            }
            PlaybackAction::Previous => {
                self.prev_track();
                "previous track"
            }
        };
        // `next_track` explains itself when the playlist has ended.
        if self.pending_load.is_some() || !matches!(args.action, PlaybackAction::Next) {
            self.banner(BannerLevel::Info, format!("{client}: {label}"));
        }
        let mut result = json!({ "done": label, "was": status_name(status) });
        if let Some(track) = self.pending_load.and_then(|u| self.uid_json(u)) {
            result["starting"] = track;
        } else if matches!(args.action, PlaybackAction::Next) {
            result["note"] = "end of the playlist: nothing to play next".into();
        }
        result
    }

    fn seek_remote(&mut self, args: &SeekArgs) -> Result<Value, String> {
        if self.status() == PlaybackStatus::Stopped {
            return Err("nothing is playing".into());
        }
        if !args.seconds.is_finite() {
            return Err("seconds must be a number".into());
        }
        if args.relative {
            self.seek_by(args.seconds.round() as i64);
        } else {
            self.seek_to(Duration::from_secs_f64(args.seconds.max(0.0)));
        }
        Ok(json!({ "seeked": args.seconds, "relative": args.relative }))
    }

    fn volume_remote(&mut self, args: &VolumeArgs) -> Result<Value, String> {
        let current = f64::from(self.audio.shared().volume()) * 100.0;
        let percent = match (args.percent, args.delta) {
            (Some(p), _) => p,
            (None, Some(d)) => current + d,
            (None, None) => return Err("give percent or delta".into()),
        };
        let percent = percent.clamp(0.0, 100.0);
        self.set_volume(percent as f32 / 100.0);
        Ok(json!({ "volume": percent.round() }))
    }

    fn visualizer_remote(&mut self, client: &str, args: &VisualizerArgs) -> Result<Value, String> {
        if let Some(name) = &args.progress {
            self.progress_style = ProgressStyle::from_name(name).ok_or_else(|| {
                format!("progress must be pacman, nyan, waveform or classic, not \"{name}\"")
            })?;
        }
        if let Some(style) = args.style {
            self.visualizer = match style {
                Style::Bars => Visualizer::Bars,
                Style::Matrix => Visualizer::Matrix,
                Style::Defrag => Visualizer::Defrag,
                Style::Hyperspace => Visualizer::Hyperspace,
            };
        }
        if let Some(layout) = args.layout {
            self.spectrum_layout = match layout {
                Layout::Wide => SpectrumLayout::Wide,
                Layout::Pane => SpectrumLayout::Pane,
                Layout::Off => SpectrumLayout::Off,
            };
        }
        if let Some(fullscreen) = args.fullscreen
            && fullscreen != (self.view == View::Visualizer)
        {
            self.toggle_visualizer();
        }
        let fullscreen = self.view == View::Visualizer;
        let mut text = format!("{client}: {}", self.visualizer.label());
        if fullscreen {
            text.push_str(", fullscreen");
        }
        if args.progress.is_some() {
            text.push_str(&format!(", progress bar {}", self.progress_style.label()));
        }
        self.banner(BannerLevel::Info, text);
        Ok(json!({
            "style": visualizer_name(self.visualizer),
            "fullscreen": fullscreen,
            "layout": layout_name(self.spectrum_layout),
            "progress": self.progress_style.name(),
        }))
    }

    fn play_options_remote(&mut self, client: &str, args: &PlayOptionsArgs) -> Value {
        if let Some(mode) = args.mode {
            self.play_mode = match mode {
                Mode::Group => PlayMode::CurrentGroup,
                Mode::All => PlayMode::AllGroups,
                Mode::ShuffleGroups => PlayMode::AllGroupsShuffled,
                Mode::ShuffleAll => PlayMode::AllTracksShuffled,
            };
        }
        if let Some(repeat) = args.repeat {
            self.repeat = match repeat {
                Repeat::Off => RepeatMode::Off,
                Repeat::Track => RepeatMode::RepeatTrack,
                Repeat::Group => RepeatMode::RepeatGroup,
                Repeat::All => RepeatMode::RepeatAll,
            };
        }
        self.requeue();
        self.banner(
            BannerLevel::Info,
            format!(
                "{client}: play mode {}, repeat {}",
                self.play_mode.label(),
                self.repeat.label()
            ),
        );
        json!({ "play_mode": mode_name(self.play_mode), "repeat": repeat_name(self.repeat) })
    }

    /// Where the player plays, for `get_status` and `list_outputs`.
    fn output_json(&self) -> Value {
        match &self.cast {
            None => json!({ "name": "this computer", "kind": "computer" }),
            Some(cast) => {
                let mut out = device_json(&cast.device);
                out["visuals"] = cast.visuals().into();
                if let Some(state) = cast.state() {
                    out["device_state"] = state.label().into();
                }
                out
            }
        }
    }

    fn outputs_json(&self) -> Value {
        let active = self.cast.as_ref().map(|c| (c.device.id(), c.visuals()));
        let mut outputs = vec![json!({
            "name": "this computer",
            "kind": "computer",
            "active": active.is_none(),
        })];
        outputs.extend(self.devices.list.iter().map(|d| {
            let mut out = device_json(d);
            out["active"] = active.is_some_and(|(id, _)| id == d.id()).into();
            if let Device::Cast(_) = d {
                // It can also get the player's pictures (set_output with visuals).
                out["visuals"] = active
                    .filter(|(id, _)| *id == d.id())
                    .map_or(Value::String("available".into()), |(_, on)| on.into());
            }
            out
        }));
        let mut result = json!({ "outputs": outputs, "playing_on": self.output_name() });
        if let Some(e) = &self.devices.error {
            result["error"] = e.as_str().into();
        } else if self.devices.list.is_empty() {
            result["note"] = "No Sonos speakers or Google Cast devices were found on this \
                              network. Sonos speakers on another subnet can be listed in \
                              config.toml under [outputs] sonos_hosts."
                .into();
        }
        result
    }

    fn output_remote(&mut self, client: &str, args: &OutputArgs) -> Result<Value, String> {
        let wanted = args.name.trim().to_lowercase();
        if LOCAL_NAMES.contains(&wanted.as_str()) {
            if self.cast.is_some() {
                self.end_cast(false);
            }
            self.banner(
                BannerLevel::Info,
                format!("{client}: playing on this computer"),
            );
            return Ok(self.output_json());
        }
        let devices = &self.devices.list;
        let found = devices
            .iter()
            .find(|d| d.name().to_lowercase() == wanted)
            .or_else(|| {
                devices
                    .iter()
                    .find(|d| d.rooms().iter().any(|r| r.to_lowercase() == wanted))
            })
            .or_else(|| {
                devices
                    .iter()
                    .find(|d| d.name().to_lowercase().contains(&wanted))
            })
            .cloned();
        let Some(device) = found else {
            let mut names: Vec<String> = vec!["this computer".into()];
            names.extend(devices.iter().map(|d| d.name()));
            return Err(format!(
                "no output called \"{}\" (outputs: {})",
                args.name,
                names.join(", ")
            ));
        };
        self.start_cast(device, args.visuals);
        match &self.cast {
            Some(_) => {
                let text = format!("{client}: playing on {}", self.output_name());
                self.banner(BannerLevel::Info, text);
                Ok(self.output_json())
            }
            None => Err(self
                .banner
                .as_ref()
                .map_or_else(|| "could not start".into(), |b| b.text.clone())),
        }
    }

    fn theme_remote(&mut self, client: &str, args: &ThemeArgs) -> Result<Value, String> {
        let Some(info) = theme_info(&args.name) else {
            let names: Vec<&str> = THEMES.iter().map(|t| t.name).collect();
            return Err(format!(
                "unknown theme \"{}\" (use {})",
                args.name,
                names.join(", ")
            ));
        };
        let (theme, warning) = Theme::by_name(info.name, supports_truecolor());
        self.theme = theme;
        self.banner(BannerLevel::Info, format!("{client}: theme {}", info.label));
        let mut result = json!({ "theme": self.theme.name });
        if let Some(w) = warning {
            result["warning"] = w.into();
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::library::index::LibraryIndex;
    use crate::model::Track;
    use crate::ui::tests::test_app;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    fn tagged(path: &str, artist: &str, title: &str, genre: &str, year: u32) -> Track {
        let mut t = Track::new(path);
        t.metadata = Some(TrackMetadata {
            artist: Some(artist.into()),
            title: Some(title.into()),
            album: Some(format!("{title} (album)")),
            genre: Some(genre.into()),
            year: Some(year),
            duration: Duration::from_secs(300),
            ..Default::default()
        });
        t
    }

    /// A player with two tagged playlist tracks and a music folder holding a tagged song
    /// (Test Artist, Rock, 1999) and an untagged one.
    fn setup() -> (App, tempfile::TempDir) {
        let music = tempfile::tempdir().unwrap();
        for (dir, from, to) in [
            ("Rock", "tagged.flac", "tagged.flac"),
            ("Other", "sine_44k_cbr.mp3", "sine.mp3"),
        ] {
            std::fs::create_dir_all(music.path().join(dir)).unwrap();
            std::fs::copy(fixture(from), music.path().join(dir).join(to)).unwrap();
        }
        let mut root = Group::new(0, "Root");
        root.tracks = vec![
            tagged("so-what.flac", "Miles Davis", "So What", "Jazz", 1959),
            tagged(
                "one-more-time.flac",
                "Daft Punk",
                "One More Time",
                "French House",
                2000,
            ),
        ];
        let mut app = test_app(root);
        app.library = LibraryIndex::new(vec![music.path().to_path_buf()], None);
        app.library.refresh();
        let deadline = Instant::now() + Duration::from_secs(20);
        while app.library.read().scanning {
            assert!(Instant::now() < deadline, "index never finished");
            std::thread::sleep(Duration::from_millis(5));
        }
        (app, music)
    }

    fn tool(app: &mut App, name: &str, args: Value) -> Value {
        app.run_tool("Claude", name, args)
            .unwrap_or_else(|e| panic!("{name} failed: {e}"))
    }

    fn titles(result: &Value) -> Vec<String> {
        result["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["title"].as_str().unwrap_or("-").to_string())
            .collect()
    }

    #[test]
    fn search_finds_playlist_and_folder_tracks() {
        let (mut app, _music) = setup();
        let jazz = tool(&mut app, "search_tracks", json!({ "genre": "JAZZ" }));
        assert_eq!(titles(&jazz), ["So What"]);
        assert_eq!(jazz["tracks"][0]["in_playlist"], true);
        assert_eq!(jazz["tracks"][0]["year"], 1959);

        let rock = tool(&mut app, "search_tracks", json!({ "genre": "rock" }));
        assert_eq!(titles(&rock), ["Test Title"]);
        assert_eq!(rock["tracks"][0]["in_playlist"], false);
        assert_eq!(rock["tracks"][0]["artist"], "Test Artist");

        let words = tool(&mut app, "search_tracks", json!({ "query": "daft TIME" }));
        assert_eq!(titles(&words), ["One More Time"]);
        let older = tool(&mut app, "search_tracks", json!({ "year_to": 1999 }));
        assert_eq!(older["total"], 2, "So What and the tagged file: {older}");
        let house = tool(
            &mut app,
            "search_tracks",
            json!({ "genre": "house", "artist": "punk" }),
        );
        assert_eq!(house["total"], 1);

        let everything = tool(&mut app, "search_tracks", json!({}));
        assert_eq!(everything["total"], 4);
        let folder = tool(&mut app, "search_tracks", json!({ "source": "library" }));
        assert_eq!(folder["total"], 2);
        let playlist = tool(&mut app, "search_tracks", json!({ "source": "playlist" }));
        assert_eq!(playlist["total"], 2);
        let some = tool(
            &mut app,
            "search_tracks",
            json!({ "shuffle": true, "limit": 3 }),
        );
        assert_eq!(some["total"], 4);
        assert_eq!(some["returned"], 3);
    }

    #[test]
    fn browse_counts_genres_and_untagged_tracks() {
        let (mut app, _music) = setup();
        let genres = tool(&mut app, "browse_library", json!({}));
        assert_eq!(genres["distinct"], 3);
        assert_eq!(genres["untagged_tracks"], 1);
        let values: Vec<&str> = genres["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["value"].as_str().unwrap())
            .collect();
        assert_eq!(values, ["French House", "Jazz", "Rock"]);
        let years = tool(
            &mut app,
            "browse_library",
            json!({ "by": "year", "source": "playlist" }),
        );
        assert_eq!(years["distinct"], 2);
    }

    #[test]
    fn queueing_a_folder_track_adds_it_to_requests() {
        let (mut app, _music) = setup();
        let rock = tool(&mut app, "search_tracks", json!({ "genre": "rock" }));
        let path = rock["tracks"][0]["path"].clone();
        let done = tool(&mut app, "queue_tracks", json!({ "paths": [path] }));
        assert_eq!(done["queue_length"], 1);
        assert_eq!(done["queued"][0]["queue_pos"], 1);
        let requests = &app.root.subgroups[0];
        assert_eq!(requests.name, REQUESTS_GROUP);
        assert_eq!(requests.tracks.len(), 1);
        let meta = requests.tracks[0]
            .metadata
            .as_ref()
            .expect("tags from the index");
        assert_eq!(meta.genre.as_deref(), Some("Rock"));
        assert_eq!(app.queue, [requests.tracks[0].uid]);
        assert!(
            app.banner
                .as_ref()
                .unwrap()
                .text
                .starts_with("Claude: queued")
        );

        // Again: nothing new to queue.
        let again = app.run_tool("Claude", "queue_tracks", json!({ "paths": [path] }));
        assert!(again.unwrap_err().contains("already queued"));
        // A playlist track goes before it with position "next".
        let jazz = tool(&mut app, "search_tracks", json!({ "genre": "jazz" }));
        let jazz_path = jazz["tracks"][0]["path"].clone();
        tool(
            &mut app,
            "queue_tracks",
            json!({ "paths": [jazz_path], "position": "next" }),
        );
        assert_eq!(app.queue[0], app.root.tracks[0].uid);
        let found = tool(&mut app, "search_tracks", json!({ "genre": "rock" }));
        assert_eq!(found["tracks"][0]["queue_pos"], 2);
        assert_eq!(found["tracks"][0]["in_playlist"], true);

        let status = tool(&mut app, "get_status", json!({}));
        assert_eq!(status["queue_length"], 2);
        assert_eq!(status["queue"][0]["title"], "So What");
        assert_eq!(status["music_folder_index"]["tracks"], 2);

        let removed = tool(
            &mut app,
            "remove_from_queue",
            json!({ "paths": [jazz_path] }),
        );
        assert_eq!(removed["removed"], 1);
        assert_eq!(tool(&mut app, "clear_queue", json!({}))["cleared"], 1);
        assert!(app.queue.is_empty());
    }

    #[test]
    fn play_now_starts_the_first_and_queues_the_rest() {
        let (mut app, _music) = setup();
        let paths: Vec<Value> = app
            .root
            .tracks
            .iter()
            .map(|t| t.path.display().to_string().into())
            .collect();
        let done = tool(
            &mut app,
            "queue_tracks",
            json!({ "paths": paths, "play_now": true }),
        );
        let (so_what, daft) = (app.root.tracks[0].uid, app.root.tracks[1].uid);
        assert_eq!(app.pending_load, Some(so_what));
        // The first leaves the queue once it is audible.
        assert_eq!(app.queue, [so_what, daft]);
        assert_eq!(done["playing_now"]["title"], "So What");
        assert!(app.banner.as_ref().unwrap().text.contains("1 more queued"));
    }

    #[test]
    fn visuals_volume_modes_and_theme() {
        let (mut app, _music) = setup();
        let v = tool(
            &mut app,
            "set_visualizer",
            json!({ "style": "hyperspace", "fullscreen": true }),
        );
        assert_eq!(
            v,
            json!({
                "style": "hyperspace",
                "fullscreen": true,
                "layout": "wide",
                "progress": "pacman",
            })
        );
        assert_eq!(app.view, View::Visualizer);
        assert_eq!(app.visualizer, Visualizer::Hyperspace);
        let v = tool(&mut app, "set_visualizer", json!({ "progress": "nyan" }));
        assert_eq!(v["progress"], "nyan");
        assert_eq!(app.progress_style, ProgressStyle::Nyan);
        let err = app
            .run_tool("Claude", "set_visualizer", json!({ "progress": "disco" }))
            .unwrap_err();
        assert!(err.contains("pacman, nyan"), "{err}");
        tool(
            &mut app,
            "set_visualizer",
            json!({ "fullscreen": false, "layout": "off" }),
        );
        assert_eq!(app.view, View::Playlist);
        assert_eq!(app.spectrum_layout, SpectrumLayout::Off);

        let set = tool(&mut app, "set_volume", json!({ "percent": 35 }));
        assert_eq!(set["volume"], 35.0);
        let set = tool(&mut app, "set_volume", json!({ "delta": -10 }));
        assert_eq!(set["volume"], 25.0);
        // The audio thread applies both commands in order, so the level settles at 25 %.
        let deadline = Instant::now() + Duration::from_secs(2);
        while (app.audio.shared().volume() - 0.25).abs() > 1e-6 {
            assert!(
                Instant::now() < deadline,
                "volume {}",
                app.audio.shared().volume()
            );
            std::thread::sleep(Duration::from_millis(2));
        }

        let o = tool(
            &mut app,
            "set_play_options",
            json!({ "mode": "shuffle_all", "repeat": "all" }),
        );
        assert_eq!(o, json!({ "play_mode": "shuffle_all", "repeat": "all" }));
        assert_eq!(app.play_mode, PlayMode::AllTracksShuffled);

        tool(&mut app, "set_theme", json!({ "name": "monochrome" }));
        assert_eq!(app.theme.name, "monochrome");
        let bad = app.run_tool("Claude", "set_theme", json!({ "name": "barbie" }));
        assert!(bad.unwrap_err().contains("dracula"));
        let seek = app.run_tool("Claude", "seek", json!({ "seconds": 30 }));
        assert_eq!(seek.unwrap_err(), "nothing is playing");
    }

    #[test]
    fn bad_arguments_are_explained() {
        let (mut app, _music) = setup();
        let e = app.run_tool("C", "search_tracks", json!({ "colour": "red" }));
        assert!(e.unwrap_err().contains("unknown field"));
        let e = app.run_tool("C", "playback", json!({ "action": "dance" }));
        assert!(e.unwrap_err().contains("unknown variant"));
        let e = app.run_tool("C", "queue_tracks", json!({ "paths": ["C:/nope.mp3"] }));
        assert!(e.unwrap_err().contains("no such audio file"));
        // Every tool the MCP server lists is one the player runs.
        for t in crate::mcp::tools::TOOLS {
            if let Err(e) = app.run_tool("C", t.name, json!({})) {
                assert!(!e.contains("unknown tool"), "{}: {e}", t.name);
            }
        }
    }

    #[test]
    fn calls_over_the_control_socket_run_on_the_ui_thread() {
        let (mut app, _music) = setup();
        let state = tempfile::tempdir().unwrap();
        let (_server, calls) = crate::remote::ControlServer::start(state.path()).unwrap();
        app.remote = Some(calls);
        let dir = state.path().to_path_buf();
        let client = std::thread::spawn(move || {
            let args = json!({ "style": "matrix" });
            crate::remote::call(&dir, "Claude Code", "set_visualizer", args)
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !client.is_finished() {
            app.process_remote_calls();
            assert!(Instant::now() < deadline, "no answer");
            std::thread::sleep(Duration::from_millis(2));
        }
        let result = client.join().unwrap().unwrap();
        assert_eq!(result["style"], "matrix");
        assert_eq!(app.visualizer, Visualizer::Matrix);
        let banner = &app.banner.as_ref().unwrap().text;
        assert!(banner.starts_with("Claude Code: Matrix"), "{banner}");
    }

    /// Calls `tool` over the control socket, running the player's loop until it answers.
    fn call_over_socket(app: &mut App, tool: &str, args: Value) -> Result<Value, String> {
        let state = tempfile::tempdir().unwrap();
        let (_server, calls) = crate::remote::ControlServer::start(state.path()).unwrap();
        app.remote = Some(calls);
        let dir = state.path().to_path_buf();
        let tool = tool.to_string();
        let client = std::thread::spawn(move || crate::remote::call(&dir, "Claude", &tool, args));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !client.is_finished() {
            app.process_remote_calls();
            app.process_outputs();
            assert!(Instant::now() < deadline, "no answer");
            std::thread::sleep(Duration::from_millis(2));
        }
        client.join().unwrap().map_err(|e| format!("{e:?}"))
    }

    #[test]
    fn speakers_are_searched_for_listed_and_chosen() {
        let (mut app, _music) = setup();
        let fake = crate::outputs::sonos::fake::FakeSonos::start("Kitchen");
        app.outputs.sonos_hosts = vec![fake.addr.to_string()];

        // The first call waits for the search.
        let listed = call_over_socket(&mut app, "list_outputs", json!({})).unwrap();
        let names: Vec<&str> = listed["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["this computer", "Kitchen"]);
        assert_eq!(listed["outputs"][0]["active"], true);
        assert_eq!(listed["playing_on"], "this computer");

        let err = app
            .run_tool("Claude", "set_output", json!({ "name": "Garage" }))
            .unwrap_err();
        assert!(err.contains("outputs: this computer, Kitchen"), "{err}");
        let out = tool(&mut app, "set_output", json!({ "name": "kitchen" }));
        assert_eq!(out["name"], "Kitchen");
        assert_eq!(out["kind"], "sonos");
        assert_eq!(app.output_name(), "Kitchen");
        let banner = &app.banner.as_ref().unwrap().text;
        assert_eq!(banner, "Claude: playing on Kitchen");
        let status = tool(&mut app, "get_status", json!({}));
        assert_eq!(status["output"]["name"], "Kitchen");

        let out = tool(&mut app, "set_output", json!({ "name": "This computer" }));
        assert_eq!(out["kind"], "computer");
        assert!(app.cast.is_none());
        let status = tool(&mut app, "get_status", json!({}));
        assert_eq!(status["output"]["name"], "this computer");
    }

    #[test]
    fn choosing_a_speaker_before_any_search_waits_for_one() {
        let (mut app, _music) = setup();
        let fake = crate::outputs::sonos::fake::FakeSonos::start("Den");
        app.outputs.sonos_hosts = vec![fake.addr.to_string()];
        let out = call_over_socket(&mut app, "set_output", json!({ "name": "Den" })).unwrap();
        assert_eq!(out["name"], "Den");
        assert!(app.devices.searched);
    }
}
