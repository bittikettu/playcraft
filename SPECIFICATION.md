# Technical Specification: `mp3blaster-rs`

**Project Name:** `mp3blaster-rs`

**Language:** Rust (2021 Edition or later)

**Target Platforms:** Linux (ALSA/PulseAudio/PipeWire), macOS (CoreAudio), Windows (WASAPI)

**License:** GPL-3.0 or MIT

## 1. Overview & Vision

`mp3blaster-rs` is a modern, memory-safe reimplementation of the classic terminal audio player **MP3Blaster** (originally written in C++ using `ncurses` in 1997). It preserves the iconic dual-pane workflow, hierarchical playlist grouping, and single-keystroke navigation of the original while replacing legacy audio backends and raw pointer management with pure Rust crates (`ratatui`, `symphonia`, and `cpal`).

### Core Objectives

* **Zero-Panic Stability:** Eliminate segmentation faults caused by malformed ID3 tags or corrupted audio frames.
* **Sub-Millisecond UI Latency:** Maintain a decoupled 60 FPS terminal rendering loop independent of disk I/O or audio decoding.
* **Expanded Codec Support:** Support MP3, FLAC, Ogg Vorbis, WAV, and AAC out of the box via pure-Rust decoding.
* **Faithful Ergonomics:** Retain the original MP3Blaster keybindings and group-based playlist paradigm alongside modern Vi-style navigation.

---

## 2. System Architecture

The application follows an actor-like, multi-threaded architecture using lock-free channels (`crossbeam-channel` or `tokio::sync::mpsc`) to guarantee that UI rendering and filesystem scanning never block audio playback.

```
+-------------------------------------------------------------------+
|                           Main Thread                             |
|  [Crossterm Event Loop] ---> [App State Machine] ---> [Ratatui]   |
+-------------------------------------------------------------------+
         |                              ^                     ^
   (Commands)                     (State Updates)       (60Hz Spectrum)
         v                              |                     |
+----------------------------------+    |                     |
|      Audio Controller Thread     |----+                     |
|  [Symphonia Demuxer & Decoder]   |                          |
+----------------------------------+                          |
         |                                                    |
  (PCM Ring Buffer)                                           |
         v                                                    |
+----------------------------------+                          |
|      CPAL Audio Output Thread    |--------------------------+
|  [Hardware Sink + FFT Analyzer]  |
+----------------------------------+

```

### 2.1 Threading Model

1. **UI / Main Thread:** Polls terminal input events via `crossterm`, updates application view state, and draws frames via `ratatui` at 30–60 FPS.
2. **Filesystem / Metadata Worker Pool:** Asynchronously reads directories and parses ID3v2/Vorbis comments using `lofty` so large folders (10,000+ tracks) do not freeze the UI.
3. **Audio Decoder Thread:** Reads the active audio file, decodes packets into interleaved `f32` PCM samples using `symphonia`, and pushes samples into a lock-free ring buffer (`ringbuf`).
4. **Audio Callback Thread:** Managed by `cpal`. Pulls PCM samples from the ring buffer and writes them to the OS audio device. Copies a sliding window of samples to a secondary buffer for real-time Fast Fourier Transform (FFT) spectrum visualization.

---

## 3. Recommended Crate Ecosystem

| Subsystem | Crate | Justification |
| --- | --- | --- |
| **TUI Rendering** | `ratatui` + `crossterm` | Industry-standard Rust TUI stack with cross-platform input handling and custom layout constraints. |
| **Audio Decoding** | `symphonia` | 100% pure Rust multimedia decoding (MP3, FLAC, OGG, WAV, AAC) with accurate sample-level seeking. |
| **Audio Output** | `cpal` | Low-level, cross-platform audio I/O for granular buffer control and real-time DSP access. |
| **Ring Buffer** | `ringbuf` | Lock-free SPSC (Single-Producer Single-Consumer) FIFO buffer between decoder and audio callback. |
| **Metadata / Tags** | `lofty` | Fast, unified parsing of ID3v1, ID3v2, APE, and Vorbis tags. |
| **Serialization** | `serde` + `toml` + `serde_json` | Parsing configuration files and saving/loading nested group playlists. |
| **CLI Parsing** | `clap` (derive) | Command-line flags, direct file playback, and startup playlist loading. |
| **Spectrum FFT** | `rustfft` | Computes frequency bins for the terminal audio visualizer. |

---

## 4. Core Data Structures

```rust
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub enum PlaybackStatus {
    Playing,
    Paused,
    Stopped,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RepeatMode {
    Off,
    RepeatTrack,
    RepeatGroup,
    RepeatAll,
}

#[derive(Debug, Clone)]
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
}

#[derive(Debug, Clone)]
pub struct Track {
    pub path: PathBuf,
    pub metadata: Option<TrackMetadata>, // Loaded lazily via worker thread
}

/// MP3Blaster's signature feature: Hierarchical Group Playlists
#[derive(Debug, Clone)]
pub struct Group {
    pub id: usize,
    pub name: String,
    pub tracks: Vec<Track>,
    pub subgroups: Vec<Group>,
}

/// Commands sent from UI Thread -> Audio Controller Thread
pub enum AudioCommand {
    LoadAndPlay(PathBuf),
    Pause,
    Resume,
    Stop,
    SeekRelative(i64), // Seconds (+/-)
    SeekAbsolute(Duration),
    SetVolume(f32),    // 0.0 to 1.0
    Shutdown,
}

/// Telemetry sent from Audio Controller -> UI Thread
pub struct AudioTelemetry {
    pub status: PlaybackStatus,
    pub position: Duration,
    pub duration: Duration,
    pub current_bitrate: u32,
    pub spectrum_bins: [f32; 16], // 16-band equalizer display
}

```

---

## 5. User Interface Specification (TUI Layout)

The interface reproduces the classic 4-zone MP3Blaster layout, adaptable to modern wide terminals:

```
+---------------------------------------------------+----------------------+
| [F1] Help  [F2] Add Group  [F3] Load/File Manager |      Now Playing     |
|---------------------------------------------------| Title:  Stairway...  |
| Playlist: Root > Classic Rock (14 tracks)         | Artist: Led Zeppelin |
|                                                   | Album:  IV (1971)    |
|  01. [GRP] 70s Prog Rock                          | Format: 320kbps 44.1k|
| >02. Led Zeppelin - Black Dog              [04:55]|                      |
|  03. Led Zeppelin - Rock and Roll          [03:40]| [||I|i||i|...] (FFT) |
|  04. Deep Purple - Highway Star            [06:05]|                      |
|  05. Pink Floyd - Money                    [06:22]| Status: [PLAYING]    |
|  ...                                              | Shuffle: ON  Rep: ALL|
|                                                   | Vol: [||||||||--] 80%|
+---------------------------------------------------+----------------------+
| [===================>------------------------------------] 01:42 / 04:55 |
+--------------------------------------------------------------------------+
| Contextual Keybind Bar / Search Prompt: /led zeppelin_                   |
+--------------------------------------------------------------------------+

```

### 5.1 View Modes

1. **Playlist View (Default):** Displays the currently active `Group`. Users can enter subgroups, reorder tracks, toggle shuffle per-group, or trigger global playback.
2. **File Manager View (Split or Overlay):** Left pane shows the local filesystem directory tree; right pane shows the current playlist group. Allows recursive directory adding and regex/glob selection.
3. **Help / Keybind View:** Scrollable overlay detailing all active shortcuts.

---

## 6. Functional Requirements

### 6.1 Hierarchical "Group" Playlists

Unlike flat `.m3u` players, MP3Blaster organizes tracks into **Groups** within a single playlist.

* Users can create named groups inside the root playlist (e.g., "Synthwave", "Coding Focus", "Podcasts").
* Playback modes include **Play Group**, **Shuffle within Group**, or **Shuffle Across Groups** (playing an entire group in order before picking a random next group, or picking random tracks globally).
* Import/Export support for `.m3u`, `.m3u8`, `.pls`, and native `.blst` (TOML/JSON structured group format).

### 6.2 Playback & DSP Engine

* **Gapless Playback:** The decoder thread pre-buffers the next queued track in the playlist 2 seconds before the active track ends, feeding samples seamlessly into the `cpal` ring buffer without reinitializing the hardware sink.
* **Software Volume & ReplayGain:** Logarithmic volume scaling applied in the PCM sample pipeline, with optional support for reading ReplayGain metadata tags to normalize volume across tracks.
* **Fast Seeking:** Arrow keys seek forward/backward by 5 seconds (`symphonia` coarse/accurate seek).

### 6.3 Keybindings (Classic + Modern Defaults)

All bindings must be customizable via `~/.config/mp3blaster-rs/config.toml`.

| Action | Classic Binding | Modern / Vi Binding |
| --- | --- | --- |
| **Play / Pause Toggle** | `5` (Keypad) / `Space` | `c` or `Space` |
| **Stop Playback** | `2` (Keypad) / `s` | `s` |
| **Previous / Next Track** | `4` / `6` | `h` / `l` or `b` / `n` |
| **Seek -5s / +5s** | `1` / `3` | `Left` / `Right` |
| **Move Cursor Up / Down** | `Up` / `Down` | `k` / `j` |
| **Page Up / Page Down** | `PgUp` / `PgDn` | `Ctrl+u` / `Ctrl+d` |
| **File Manager / Add Files** | `F1` | `a` |
| **Select / Invert Selection** | `Space` (in File View) | `v` / `V` |
| **Create New Group** | `F2` | `g` |
| **Search / Filter** | `/` | `/` |
| **Volume Down / Up** | `<` / `>` or `-` / `+` | `-` / `+` |
| **Quit** | `q` | `q` |

---

## 7. Configuration & Persistence

Configuration follows the XDG Base Directory Specification (`dirs` crate):

* **Config File:** `~/.config/mp3blaster-rs/config.toml`
* **State / Cache:** `~/.local/state/mp3blaster-rs/session.json` (remembers last active playlist, track position, and volume on exit).

### Sample `config.toml`

```toml
[general]
default_music_dir = "~/Music"
resume_on_startup = true
gapless_playback = true
audio_buffer_ms = 150

[ui]
fps = 60
show_spectrum = true
color_theme = "classic_blue" # Options: classic_blue, gruvbox, catppuccin, monochrome
charset = "unicode"          # Options: unicode, ascii

[keybindings]
play_pause = ["Space", "c"]
next_track = ["n", "6"]
prev_track = ["b", "4"]
seek_forward = ["Right", "3"]
seek_backward = ["Left", "1"]

```

---

## 8. Error Handling & Edge Cases

1. **Missing / Unplugged Audio Device:** If `cpal` loses the default output sink (e.g., Bluetooth headphones disconnect), the audio thread catches `StreamError::DeviceNotAvailable`, automatically pauses the decoder, and sends an alert banner to the UI thread without crashing.
2. **Corrupted Audio Frames:** If `symphonia` encounters an invalid MP3 frame (`Error::DecodeError`), it logs a warning to the internal ring-log, skips to the next valid sync word, and continues playback.
3. **Deleted Files in Playlist:** If a queued file no longer exists on disk, the track is marked in red (`[MISSING]`) in the UI, and the playlist engine advances to the next playable item. To prevent infinite loops when an entire playlist is missing, the engine halts after $N$ consecutive failures (where $N$ equals the total playlist length).

---

## 9. Phased Implementation Roadmap

1. **Phase 1: Headless Audio Core (Weeks 1–2)**
* Build the `symphonia` + `cpal` + `ringbuf` pipeline.
* Implement `AudioCommand` channel handling (Play, Pause, Seek, Volume) and verify via a minimal CLI harness.


2. **Phase 2: TUI Skeleton & File Browser (Weeks 3–4)**
* Set up the `ratatui` layout and `crossterm` event loop.
* Build the dual-pane directory browser and asynchronous `lofty` metadata scanner.


3. **Phase 3: Hierarchical Group Engine (Week 5)**
* Implement nested `Group` data structures, recursive shuffle logic, and `.m3u` / `.blst` serialization.


4. **Phase 4: Polish, FFT & Config (Week 6)**
* Add `rustfft` spectrum visualizer, `config.toml` theme/keybind loading, and integration tests for malformed audio files.