# mp3blaster-rs

A memory-safe Rust reimplementation of **MP3Blaster**, the classic 1997 terminal audio player. It keeps the original's dual-pane workflow, hierarchical *group* playlists and single-keystroke control. Underneath it uses `symphonia` (decoding), `cpal` (audio output) and `ratatui` (terminal UI).

Windows is the primary platform (WASAPI, Windows Terminal and the legacy console). The code stays portable to Linux and macOS.

```
┌ [F1] Files  [F2] Add Group  [F3] Load  [?] Help ─────────────┐┌────── Now Playing ───────┐
│Playlist: Root › Classic Rock (14 tracks)                     ││Title:  Black Dog         │
│ 01. [GRP] 70s Prog Rock                                  (6) ││Artist: Led Zeppelin      │
│>02. Led Zeppelin - Black Dog                         [04:55] ││Album:  IV (1971)         │
│ 03. Led Zeppelin - Rock and Roll                     [03:40] ││Format: 320kbps 44.1k     │
│ 04. Deep Purple - Highway Star                       [06:05] ││      ▃ ▅ █ ▆ ▄ ▂         │
│ 05. Pink Floyd - Money                             [MISSING] ││Status: [PLAYING]         │
└──────────────────────────────────────────────────────────────┘└──────────────────────────┘
[=====================>----------------------------------------] 01:42 / 04:55
Space:Play/Pause  s:Stop  n:Next  b:Prev  Right:Seek  +:Vol  Enter:Play  ?:Help  q:Quit
```

## Features

- **Formats:** MP3, FLAC, Ogg Vorbis, WAV and AAC/M4A, decoded in pure Rust. Corrupt frames are skipped instead of crashing.
- **Gapless playback:** the next track is opened 2 s early and fed into the same output stream, sample for sample. Encoder delay and padding are trimmed for MP3 and AAC.
- **High-quality resampling:** audio is converted to the device's own rate (usually 48 kHz on Windows). Tracks with different sample rates still play back to back.
- **Group playlists:**
  - nested groups;
  - play modes *Group*, *All*, *Shuffle groups* and *Shuffle all*;
  - a shuffle switch for each group;
  - repeat track, group or all.
- **File manager:**
  - drive list on Windows, hidden-file filter;
  - select by glob (`*.flac`) or regex (`re:^0[1-5]`);
  - recursive folder adds that run in the background (10,000+ files without freezing the UI).
- **Playlists:** `.m3u`, `.m3u8` (nested groups via `#EXTGRP:A / B`), `.pls`, and the native `.blst`, which stores the full group tree.
- **Spectrum analyser:**
  - by default a full-width strip under the playlist, with more bands the wider the terminal;
  - colour gradient, falling peak markers and a frequency axis;
  - `f` switches to a fullscreen visualiser; `w` cycles between the wide strip, inside Now Playing, and off.
  - `v` switches to **Matrix rain** of the playing file's own bytes, shown as hex digits from the part of the file being played. Each column follows a spectrum band: louder frequencies make faster, longer streams, over faint bars drawn from the same digits. Fullscreen shows the file offset being read.
  - Press `v` again for the **Disk Defragmenter**, in the style of the Windows 98 tool. Each song gets its own fragmented "disk", and defragmenting keeps pace with playback. Loud passages make the read head grab more clusters. When the song ends, the disk is fully defragmented: "Defragmentation of Drive D: is complete." The drive letter is the one your music file is on.
- **ReplayGain** (track or album, limited to avoid clipping) and logarithmic, click-free volume.
- **Audio device handling:** device unplugged → playback pauses with a banner and resumes when the device returns. Windows default device changes → playback follows.
- **Sessions:** the playlist, track, position, volume and modes are restored on the next start. They are saved every 30 s, on quit, and when the console window is closed.
- **Themes:** Dracula (the default), Tokyo Night, One Dark, Monokai, Nord, Catppuccin, Gruvbox, the original Classic Blue, and Monochrome. Press `t` to open the picker, which previews each theme live and saves your choice to `config.toml`.
- **Configuration:** an ASCII mode for legacy consoles and fully rebindable keys.

## Building

You need a current stable Rust toolchain (developed with 1.99). On Windows, use the MSVC toolchain with the Visual Studio Build Tools. On Linux, install `libasound2-dev` and `pkg-config` first.

```sh
cargo build --release
```

On Windows the C runtime is linked statically (`.cargo/config.toml`). The result is a single `target\release\mp3blaster-rs.exe` with no Visual C++ redistributable needed.

## Usage

```
mp3blaster-rs [OPTIONS] [PATHS]...

  PATHS                    audio files, folders or playlists (.m3u .m3u8 .pls .blst)
  --device <NAME>          output device (part of its name, see --list-devices)
  --volume <0-100>         initial volume
  --config <FILE>          use another config file
  --no-resume              do not resume the previous session's track
  --list-devices           list audio outputs and exit
  --write-default-config   write a commented config.toml and exit
  --headless               no TUI: line commands on stdin (p, s, n, f, b, +, -, q)
```

- **Several paths:** files and folders go into the root group, and each playlist becomes a group of its own.
- **One playlist path:** that playlist is opened directly, and F4 saves back to it.
- **No paths:** the previous session is restored.

## Keys

All keys can be remapped (see *Configuration*). Press `?` in the player to see the active bindings.

| Action | Classic | Modern / Vi |
| --- | --- | --- |
| Play / pause | `5` / Space | `c` / Space |
| Stop | `2` | `s` |
| Previous / next track | `4` / `6` | `b` / `n`, `h` / `l` |
| Seek −5 s / +5 s | `1` / `3` | Left / Right |
| Cursor up / down | Up / Down | `k` / `j` |
| Page up / down | PgUp / PgDn | Ctrl+u / Ctrl+d |
| File manager (add files) | F1 | `a` |
| Create group | F2 | `g` |
| Load / save playlist | F3 / F4 | |
| Search / filter | `/` | `/` |
| Volume down / up | `<` / `>` | `-` / `+` (also `=`) |
| Help / message log | `?` / `L` | |
| Colour theme picker | | `t` |
| Fullscreen spectrum / spectrum layout | | `f` / `w` |
| Visualiser: bars / Matrix rain / defragmenter | | `v` |
| Quit | `q` | Ctrl+c |

**Playlist view:**
- Enter plays a track or opens a group; Backspace goes up a level.
- `R` renames a group, `d`/Del removes an item (groups are confirmed first), and `K`/`J` move an item up or down.
- `m` cycles the play mode, `r` the repeat mode, and `z` toggles shuffle for the current group.

**File manager:**
- Enter, `l` or Right opens a folder (or adds a file); Backspace, `h` or Left goes up. Above `C:\`, the drive list appears.
- Space or `v` selects, `V` inverts the selection, and `*` selects by pattern.
- `a` or Ins adds the selection, `.` shows hidden files, and Esc closes the file manager.

The keypad digits work with NumLock on. On non-US layouts, AltGr characters are recognised as plain characters.

## Configuration

| | Windows | Linux | macOS |
| --- | --- | --- | --- |
| Config | `%APPDATA%\mp3blaster-rs\config.toml` | `~/.config/mp3blaster-rs/config.toml` | `~/Library/Application Support/mp3blaster-rs/config.toml` |
| Session and log | `%LOCALAPPDATA%\mp3blaster-rs\` | `~/.local/state/mp3blaster-rs/` | `~/Library/Application Support/mp3blaster-rs/` |

`XDG_CONFIG_HOME` and `XDG_STATE_HOME` override these locations on any OS.

Run `mp3blaster-rs --write-default-config` to get a commented file, or see [`config.example.toml`](config.example.toml).

- Every setting is optional, and an invalid value falls back to its default with a warning.
- `[keybindings]` changes keys everywhere; `[keybindings.file_manager]` changes them only inside the file manager.
- A binding replaces that action's default keys.

```toml
[general]
default_music_dir = "~/Music"
replaygain = "track"        # off, track, album

[audio]
output_device = "Arctis"    # part of a device name; empty follows the Windows default

[ui]
color_theme = "dracula"     # or tokyo_night, one_dark, monokai, nord, catppuccin, gruvbox, classic_blue, monochrome
charset = "ascii"           # for the legacy console with raster fonts
spectrum_layout = "wide"    # wide (full-width strip) or pane (inside Now Playing)
visualizer = "defrag"       # bars, matrix (rain of the file's bytes) or defrag

[keybindings]
play_pause = ["Space", "c"]
next_track = ["n", "6"]
```

## The `.blst` format

`.blst` is JSON that stores the full group tree. Track paths inside the playlist's own folder are stored relative to it.

```json
{
  "format": "blst",
  "version": 1,
  "root": {
    "name": "Root",
    "tracks": ["intro.mp3"],
    "subgroups": [{ "name": "Synthwave", "shuffle": true, "tracks": ["synth/a.flac"] }]
  }
}
```

## Differences from the specification

- **Audio commands:**
  - `AudioCommand` gains `QueueNext` (for gapless playback) and `SetReplayGain`.
  - One-off events (track started or failed, device lost or restored) travel as an `AudioEvent` enum.
  - The `AudioTelemetry` snapshot is assembled per frame from lock-free shared state.
- **Spectrum FFT:** it runs on the UI thread from a tap buffer instead of inside the real-time audio callback, so the callback never does heavy work. The display has as many bands as the width allows; `AudioTelemetry.spectrum_bins` still holds the spec's 16 bands.
- **Resampling:** `rubato` resamples to the device rate. The spec's crate list has no resampler, but WASAPI shared mode needs one.
- **Groups:** groups gain a `shuffle` flag, and the original MP3Blaster play modes are added.
- **Themes:** the default is Dracula instead of `classic_blue`. The spec's four themes are kept, five VS Code themes are added, and there is an in-app picker (`t`).
- **Additions:**
  - keys: Enter, Backspace, `?`, F3/F4, `L`, `t`, `f`, `w`, `v`, `m`, `r`, `z`, `R`, `d`, `K`/`J`;
  - config: `[general] replaygain`, `[audio] output_device`, `[ui] show_hidden`, `[ui] spectrum_layout`, `[ui] visualizer`;
  - the `[keybindings.file_manager]` section.
- **Key conflict:** the spec's UI mockup shows F1 as Help, but its key table says F1 opens the file manager. The key table wins, so Help is `?`.

## Development

```sh
cargo test                                   # unit, pipeline and malformed-input tests
cargo clippy --all-targets -- -D warnings
tests/fixtures/generate.ps1                  # regenerate audio fixtures (needs ffmpeg)
```

None of the tests need a sound card. The audio pipeline is tested by pulling samples from the renderer exactly as the device callback would. This proves, for example, that a split FLAC file plays back gaplessly and sample-identical to the original.

## License

MIT
