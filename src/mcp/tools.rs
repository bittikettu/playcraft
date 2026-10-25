//! The tools offered over MCP: names, descriptions and input schemas. The bridge lists
//! them; the player (`ui/remote.rs`) runs them.

use serde_json::{Value, json};

pub struct Tool {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub read_only: bool,
    pub destructive: bool,
    /// JSON Schema of the arguments (an object schema).
    pub schema: fn() -> Value,
}

/// What a track looks like in results, for descriptions.
const TRACK_FIELDS: &str = "Tracks are objects with path (their id in every tool), artist, title, \
album, genre, year and duration_s; fields without tags are left out.";

fn no_args() -> Value {
    json!({ "type": "object", "properties": {}, "additionalProperties": false })
}

fn paths_arg(what: &str) -> Value {
    json!({
        "type": "array",
        "items": { "type": "string" },
        "minItems": 1,
        "description": what,
    })
}

fn source_arg() -> Value {
    json!({
        "type": "string",
        "enum": ["all", "playlist", "library"],
        "default": "all",
        "description": "playlist: tracks loaded in the player; library: files in the music \
    folder; all: both.",
    })
}

pub const TOOLS: &[Tool] = &[
    Tool {
        name: "get_status",
        title: "Player status",
        description: "What the player is doing: playback status, the current track with position \
and duration, volume, play mode and repeat, the next track, the play queue, the visualizer and \
the state of the music folder index.",
        read_only: true,
        destructive: false,
        schema: no_args,
    },
    Tool {
        name: "browse_library",
        title: "Browse the music collection",
        description: "Counts tracks per genre, artist, album or year, most first. Use it to see \
what kinds of music exist before searching, e.g. which genres match a mood the user asks for.",
        read_only: true,
        destructive: false,
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "by": {
                        "type": "string",
                        "enum": ["genre", "artist", "album", "year"],
                        "default": "genre",
                    },
                    "source": source_arg(),
                    "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 50 },
                },
                "additionalProperties": false,
            })
        },
    },
    Tool {
        name: "search_tracks",
        title: "Search for songs",
        description: "Finds tracks in the playlist and the music folder. All filters are \
case-insensitive substrings and are combined; query words must all appear in the artist, \
title, album, genre or file name. Use shuffle for a varied pick (e.g. \"play some jazz\"). \
Results include in_playlist and queue_pos (1-based) for queued tracks.",
        read_only: true,
        destructive: false,
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Words to look for anywhere." },
                    "genre": { "type": "string", "description": "e.g. \"jazz\" also matches \"Acid Jazz\"." },
                    "artist": { "type": "string" },
                    "album": { "type": "string" },
                    "year_from": { "type": "integer" },
                    "year_to": { "type": "integer" },
                    "source": source_arg(),
                    "shuffle": {
                        "type": "boolean",
                        "default": false,
                        "description": "Return a random selection of the matches instead of the first ones.",
                    },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 25 },
                },
                "additionalProperties": false,
            })
        },
    },
    Tool {
        name: "queue_tracks",
        title: "Queue songs",
        description: "Puts tracks (paths from search_tracks) in the play queue. Queued tracks play \
before the normal playlist order, which resumes afterwards. Music-folder tracks that are not in \
the playlist are added to its \"Requests\" group first. play_now starts the first one at once.",
        read_only: false,
        destructive: false,
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "paths": paths_arg("Tracks to queue, in order."),
                    "position": {
                        "type": "string",
                        "enum": ["end", "next"],
                        "default": "end",
                        "description": "end: after what is queued; next: before it.",
                    },
                    "play_now": { "type": "boolean", "default": false },
                },
                "required": ["paths"],
                "additionalProperties": false,
            })
        },
    },
    Tool {
        name: "remove_from_queue",
        title: "Remove songs from the queue",
        description: "Takes tracks out of the play queue (they stay in the playlist).",
        read_only: false,
        destructive: false,
        schema: || {
            json!({
                "type": "object",
                "properties": { "paths": paths_arg("Tracks to take out of the queue.") },
                "required": ["paths"],
                "additionalProperties": false,
            })
        },
    },
    Tool {
        name: "clear_queue",
        title: "Clear the queue",
        description: "Empties the play queue. Playback continues in normal order.",
        read_only: false,
        destructive: true,
        schema: no_args,
    },
    Tool {
        name: "playback",
        title: "Play, pause, skip",
        description: "Controls playback like the player's keys: play (resume, or start the queue \
or playlist), pause, toggle, stop, next (the queue first) or previous.",
        read_only: false,
        destructive: false,
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["play", "pause", "toggle", "stop", "next", "previous"],
                    },
                },
                "required": ["action"],
                "additionalProperties": false,
            })
        },
    },
    Tool {
        name: "seek",
        title: "Seek",
        description: "Jumps to a position in the current track, or by an amount when relative.",
        read_only: false,
        destructive: false,
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "seconds": { "type": "number" },
                    "relative": { "type": "boolean", "default": false },
                },
                "required": ["seconds"],
                "additionalProperties": false,
            })
        },
    },
    Tool {
        name: "set_volume",
        title: "Set the volume",
        description: "Sets the volume in percent, or changes it by delta percent.",
        read_only: false,
        destructive: false,
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "percent": { "type": "number", "minimum": 0, "maximum": 100 },
                    "delta": { "type": "number", "minimum": -100, "maximum": 100 },
                },
                "additionalProperties": false,
            })
        },
    },
    Tool {
        name: "set_visualizer",
        title: "Visualizer",
        description: "Changes the visuals: style bars (spectrum analyser), matrix (rain of the \
playing file's bytes), defrag (a Windows 98 disk defragmenter that finishes with the song) or \
hyperspace (an audio-reactive 3D demoscene, the showpiece); fullscreen on or off; layout of the \
small spectrum: wide strip, pane (inside Now Playing) or off; the progress bar: pacman (chomping \
to the power pellet), nyan (a cat with a rainbow), waveform (the song's loudness as it plays) or \
classic. Every argument is optional.",
        read_only: false,
        destructive: false,
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "style": { "type": "string", "enum": ["bars", "matrix", "defrag", "hyperspace"] },
                    "fullscreen": { "type": "boolean" },
                    "layout": { "type": "string", "enum": ["wide", "pane", "off"] },
                    "progress": { "type": "string", "enum": ["pacman", "nyan", "waveform", "classic"] },
                },
                "additionalProperties": false,
            })
        },
    },
    Tool {
        name: "set_play_options",
        title: "Play mode and repeat",
        description: "mode: group (only the group playback started in), all (every group in \
order), shuffle_groups (groups in random order, each played whole) or shuffle_all; repeat: off, \
track, group or all.",
        read_only: false,
        destructive: false,
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "mode": { "type": "string", "enum": ["group", "all", "shuffle_groups", "shuffle_all"] },
                    "repeat": { "type": "string", "enum": ["off", "track", "group", "all"] },
                },
                "additionalProperties": false,
            })
        },
    },
    Tool {
        name: "set_theme",
        title: "Colour theme",
        description: "Changes the player's colour theme for this session.",
        read_only: false,
        destructive: false,
        schema: || {
            let names: Vec<&str> = crate::ui::theme::THEMES.iter().map(|t| t.name).collect();
            json!({
                "type": "object",
                "properties": { "name": { "type": "string", "enum": names } },
                "required": ["name"],
                "additionalProperties": false,
            })
        },
    },
    Tool {
        name: "list_outputs",
        title: "Speakers and TVs",
        description: "Where the player can play: this computer, Sonos speakers (rooms and \
groups) and Google Cast devices (Android TVs, Chromecasts, Nest speakers) found on the \
network, with the one playing marked active. The first call, or one with refresh, searches the \
network, which takes a couple of seconds.",
        read_only: true,
        destructive: false,
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "refresh": { "type": "boolean", "default": false, "description": "Search the network again." },
                },
                "additionalProperties": false,
            })
        },
    },
    Tool {
        name: "set_output",
        title: "Play on a speaker or TV",
        description: "Moves playback to a Sonos speaker or group, or a Google Cast device, by \
name (as list_outputs shows it, or a Sonos room), or back to \"this computer\". The music \
continues where it is; volume then controls that device.",
        read_only: false,
        destructive: false,
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "A speaker, group, room or TV name, or \"this computer\"." },
                },
                "required": ["name"],
                "additionalProperties": false,
            })
        },
    },
];

pub fn find(name: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|t| t.name == name)
}

fn describe(t: &Tool) -> String {
    if matches!(t.name, "get_status" | "search_tracks" | "queue_tracks") {
        format!("{} {TRACK_FIELDS}", t.description)
    } else {
        t.description.to_string()
    }
}

/// The `tools/list` entries.
pub fn list() -> Vec<Value> {
    TOOLS
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "title": t.title,
                "description": describe(t),
                "inputSchema": (t.schema)(),
                "annotations": {
                    "title": t.title,
                    "readOnlyHint": t.read_only,
                    "destructiveHint": t.destructive,
                    "openWorldHint": false,
                },
            })
        })
        .collect()
}
