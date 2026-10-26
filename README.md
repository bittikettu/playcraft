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
- **Play queue:** line up tracks (or whole groups) to play next.
  - Queued tracks show their place (`Q1`, `Q2`…) in the playlist, and Now Playing shows what comes next.
  - When the queue runs out, normal order resumes from where it was before the queue.
  - The queue is kept between sessions.
- **Jukebox view (F4):** the songs coming up, shown as the title strips of a jukebox. It is drawn with two 24-bit pixels per cell, under an arched cabinet with rainbow neon tubes that pulse with the bass.
  - Each strip shows the song's selection code (`A1` is the playlist's first song, `B1` its eleventh), its title and artist, and how long until it starts. The times count down from what is left of the playing song.
  - A glass panel describes the next song: its album, length, genre and bitrate, and whether you queued it or it is next in play order.
  - The list is what will really play: the queue first, then the play order. It says whether the music stops after the last song, starts over, or is shuffled again.
  - Upcoming songs stand in a rack as records, each with its own label colour. The playing record spins on the turntable, and the tonearm moves inward as the song plays.
  - When the song changes, the arm swings away and the record goes back into the rack. The next record tips over onto the turntable, and the arm swings back.
- **"Look at my horse, my horse is amazing" view (F5):** the same songs as a ride, after Weebl's cartoon song.
  - A white horse with a rainbow mane gallops along a country track to the beat, with the same beat detection as HYPERSPACE. It takes one stride per beat (one per two beats in fast music) and lands on the beat, and sparkles flash with each one. The tempo shows next to the time. Without a beat, it slows down to a walk.
  - There is a rainbow, a sun, drifting clouds and a white fence.
  - The songs coming up are signposts with their selection codes. Each stands as far along the track as the time until its song starts, so the next sign reaches the horse just as its song begins.
  - The horse says in a speech bubble what is next and when it starts. A board below lists the road ahead, with when each song starts.
  - When the song changes, the horse turns into a plane, loops the loop over the meadow, and lands as a horse again.
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
  - Press `v` once more for **HYPERSPACE**, a 3D demoscene scene drawn with two 24-bit pixels per cell (capped at 30 fps):
    - a warp starfield;
    - a rotating point-cloud sphere shaped by the spectrum;
    - a spectrum corona and a circular oscilloscope;
    - beat detection with a live BPM readout, where each beat fires a shockwave and a particle burst;
    - a hyperspace jump on every new track, with the title flying in as glowing pixel text.

    The scene adapts to its size. Small windows and the wide strip get a crisp version without glow or trails, with fewer sphere points. The strip spreads the scene sideways.

    How much the terminal has to draw is tunable with `hyperspace_quality = "high" | "balanced" | "fast"` (default `balanced`). If the terminal still can't keep up, the frame rate drops by itself.
- **Animated progress bar:** `p` switches between four styles:
  - **Pac-Man** (the default): he chomps along the bar, eating pellets on the way to the power pellet at the end of the song, with a ghost on his heels. He chomps faster when the music is louder, the pellets flash on the beat, and the ghost turns blue in the last ten seconds.
  - **Nyan cat:** a cat flies ahead of a rippling rainbow into twinkling stars, and naps while the music is paused.
  - **Waveform:** the loudness of the song so far, drawn as it plays in the spectrum's colours, with a note that flips on the beat.
  - **Classic:** `[=====>-----]`.

  With `charset = "ascii"`, the styles are drawn with plain characters (`C` for Pac-Man, `M` for the ghost).
- **Now Playing:** the pane takes a third of the terminal width (28–48 columns). Titles, artists and albums too long for it scroll after a short pause.
- **ReplayGain** (track or album, limited to avoid clipping) and logarithmic, click-free volume.
- **Audio device handling:** device unplugged → playback pauses with a banner and resumes when the device returns. Windows default device changes → playback follows.
- **Sessions:** the playlist, track, position, volume and modes are restored on the next start. They are saved every 30 s, on quit, and when the console window is closed.
- **Themes:** Dracula (the default), Tokyo Night, One Dark, Monokai, Nord, Catppuccin, Gruvbox, the original Classic Blue, and Monochrome. Press `t` to open the picker, which previews each theme live and saves your choice to `config.toml`.
- **Speakers and TVs:** press `o` to play on a Sonos speaker or group, or a Google Cast device such as an Android TV or a Chromecast, and back on the computer. The playlist, queue, keys and visualizers keep working, in step with the device. See *Speakers and TVs*.
- **Configuration:** an ASCII mode for legacy consoles and fully rebindable keys.

## Building

You need a current stable Rust toolchain (developed with 1.99). On Windows, use the MSVC toolchain with the Visual Studio Build Tools. On Linux, install `libasound2-dev` and `pkg-config` first.

```sh
cargo build --release
```

On Windows the C runtime is linked statically (`.cargo/config.toml`). The result is a single `target\release\mp3blaster-rs.exe` with no Visual C++ redistributable needed.

### Docker (Ubuntu)

The `Dockerfile` builds and runs the player on Ubuntu 24.04. It is also a quick way to check the Linux build. Run the commands from the repository folder (PowerShell or cmd on Windows):

```sh
docker build -t mp3blaster .                                # the player image
docker build --target test .                                # fmt, clippy and all tests on Linux
docker run -it --rm mp3blaster                              # play the bundled samples
docker run -it --rm -v "D:\Music:/music:ro" mp3blaster      # play your own music
```

- `-it` is required, because the player is a terminal UI.
- **There is no sound card in a container.** Audio goes to a silent PulseAudio sink that runs in real time, so the progress bar, spectrum and visualizers behave normally, but you hear nothing.
  - To hear it, set `PULSE_SERVER` to a PulseAudio server outside the container. On a Linux desktop, add `-e PULSE_SERVER=unix:/run/user/1000/pulse/native -v /run/user/1000/pulse:/run/user/1000/pulse`.
- **Inside the container:**
  - the music folder is `/music`, which holds the test tones unless you mount your own;
  - the config is `~/.config/mp3blaster-rs/config.toml`;
  - nothing is kept after the container exits.
- **Speakers and TVs:** the container's own network cannot see your devices.
  - On a Linux host, add `--network host`, so the device search and the devices' song requests reach the player.
  - Docker Desktop (Windows and macOS) runs containers in a virtual machine, so the device search usually finds nothing there. Play on them from the Windows build instead.
- **AI control (MCP):** give the container a name with `--name mp3blaster`. Then use `docker exec -i mp3blaster mp3blaster-rs --mcp` as the MCP server command. For Claude Desktop, that is `"command": "docker", "args": ["exec", "-i", "mp3blaster", "mp3blaster-rs", "--mcp"]`.

## Usage

```
mp3blaster-rs [OPTIONS] [PATHS]...

  PATHS                    audio files, folders or playlists (.m3u .m3u8 .pls .blst)
  --device <NAME>          output device (part of its name, see --list-devices)
  --volume <0-100>         initial volume
  --config <FILE>          use another config file
  --no-resume              do not resume the previous session's track
  --list-devices           list audio outputs and exit
  --list-speakers          list Sonos speakers and Cast devices on the network and exit
  --write-default-config   write a commented config.toml and exit
  --headless               no TUI: line commands on stdin (p, s, n, f, b, +, -, q)
  --mcp                    MCP server on stdin/stdout for AI assistants (see AI control)
```

- **Several paths:** files and folders go into the root group, and each playlist becomes a group of its own.
- **One playlist path:** that playlist is opened directly, and Ctrl+S saves back to it.
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
| Load / save playlist | F3 / Ctrl+S | `S` (save) |
| Queue track or group / show queue / clear queue | Ins | `e` / `u` / `X` |
| Jukebox: what plays next and when each song starts | F4 | |
| "Look at my horse, my horse is amazing": the same, on horseback | F5 | |
| Search / filter | `/` | `/` |
| Volume down / up | `<` / `>` | `-` / `+` (also `=`) |
| Help / message log | `?` / `L` | |
| Colour theme picker | | `t` |
| Play on: this computer, a Sonos speaker or a TV (Google Cast) | | `o` |
| Fullscreen spectrum / spectrum layout | | `f` / `w` |
| Visualiser: bars / Matrix rain / defragmenter / HYPERSPACE | | `v` |
| Progress bar: Pac-Man / Nyan cat / waveform / classic | | `p` |
| Quit | `q` | Ctrl+c |

**Playlist view:**
- Enter plays a track or opens a group; Backspace goes up a level.
- `R` renames a group, `d`/Del removes an item (groups are confirmed first), and `K`/`J` move an item up or down.
- `m` cycles the play mode, `r` the repeat mode, and `z` toggles shuffle for the current group.
- `e` or Ins queues the track under the cursor; pressing it again takes the track out of the queue. On a group, it queues all of the group's tracks.

**Play queue** (`u`):
- Enter plays the selected track now, and `K`/`J` move it.
- `d` removes a track, `X` clears the queue, and Esc closes the list.
- Playback keys keep working while the list is open.

**Jukebox** (F4) **and the horse** (F5):
- F4 and F5 switch between the two. They share the list of songs and the selection.
- Up and Down move along the title strips (or the horse's board) in play order, and PgUp/PgDn move a whole board.
- Enter plays the selected song now. `e` or Ins queues it, or takes it out of the queue.
- On queued songs, `d` takes them out of the queue and `K`/`J` move them.
- Playback keys keep working. Esc, or the view's own key again, goes back to where you came from.
- Small terminals get a plain list instead of the machine, and with the ASCII charset it is drawn with one pixel per cell.

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
- `[outputs]` is described under *Speakers and TVs*, and `[mcp]` under *AI control (MCP)*.

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
visualizer = "hyperspace"   # bars, matrix (rain of the file's bytes), defrag or hyperspace
hyperspace_quality = "balanced"  # high, balanced or fast (fast suits slow terminals)
progress_style = "pacman"   # or nyan, waveform, classic

[keybindings]
play_pause = ["Space", "c"]
next_track = ["n", "6"]
```

## Speakers and TVs (Sonos, Google Cast)

Press `o` to choose where the music plays: **This computer**, or a device found on your network:
- **Sonos** speakers and groups;
- **Google Cast** devices: Android and Google TVs, Chromecasts, Nest speakers and speaker groups.

Pick one with Enter and the music moves there, from the same spot. Pick **This computer** to bring it back.

- **Everything keeps working.** Play, pause, seek, next and previous, the queue and the play modes all control the device. The volume keys set the device's volume (a Sonos group's, or the TV's).
- **Visualizers stay in time.** The computer keeps playing the song silently, which drives the visualizers and the progress bar, and it is kept within a fraction of a second of the device.
- **The device's own controls still work.** Pausing, playing, stopping or changing the volume in the Sonos app, on the speaker or with the TV's remote is followed by the player.
  - If the device switches to something else (Spotify on the Sonos, Netflix on the TV), the player stops following it and pauses.
- **The TV shows the song:** title, artist, album and a cover drawn in the player's colour theme. It looks like the player: a Now Playing frame, the song in a retro pixel font, and spectrum bars in the theme's gradient, shaped differently for every song. The Sonos app shows the same cover.
  - With `cover_art = "album"`, the album's own art is shown instead, when there is some: a picture in the file's tags, or a `cover.jpg` or `folder.jpg` next to it.
  - A new theme shows from the next song on.
- **Any file plays.** If a device cannot play a file, the player sends it as WAV instead, converted on the fly. Examples are hi-res FLAC above 48 kHz, which no Sonos speaker plays, and formats an older model lacks.
- **Between songs there is a short pause**, because the device loads each song separately. Gapless albums are therefore not gapless there.
- **Now Playing** shows the device, and the message log (`L`) records what it is sent.

**Casting to a TV** starts the TV's own media player (Google's Default Media Receiver), like casting from a phone does. It replaces whatever the TV shows, and it closes again when you switch back to this computer.

**TV visuals.** The picker lists each Cast device twice. Pick **"Televisio + visuals"** (with your TV's name) to send the player's pictures as well as its sound, as one live video stream:
- **The picture** is drawn for the TV at 720p: HYPERSPACE, or the spectrum bars when that is the player's visualizer (Matrix and Defrag show HYPERSPACE on the TV). The theme's colours are used.
- **A "Now Playing" strip** along the bottom shows the title, artist, album, the time, and Pac-Man eating his way through the song.
- **About 6–8 seconds behind.** The TV plays it like a live broadcast (in 2-second pieces, a few of them buffered), so pausing, skipping and seeking show up on the TV that much later. Picture and sound on the TV stay together.
- **Pausing with the TV's remote** pauses the player; playing again jumps back to the live picture.
- **It needs [ffmpeg](https://ffmpeg.org)**, which encodes the stream (`winget install Gyan.FFmpeg` on Windows). The player picks the first H.264 encoder that works: NVIDIA (NVENC), Intel (Quick Sync) or AMD graphics, else the x264 software encoder. With a graphics card's encoder, streaming takes little CPU.

```toml
[outputs]
ffmpeg = "ffmpeg"        # or the full path to ffmpeg.exe
tv_encoder = "auto"      # or e.g. "libx264", "h264_nvenc"
```

**How it works:**
- **Finding devices:** the player uses the same network searches as the Sonos and Google Home apps (SSDP and mDNS).
- **Control:** Sonos speakers over UPnP, Cast devices over the Cast protocol, which is encrypted (TLS).
- **Songs:** the device fetches each song from a small web server in the player.

**Firewall.** That web server must be reachable from the device. The first time you press `o`, Windows asks whether mp3blaster-rs may communicate on networks. Allow it for **private networks**. If it was blocked, the player says so ("cannot fetch songs from this computer"). You can then allow it in *Windows Security → Firewall & network protection → Allow an app through firewall*.

```toml
[outputs]
http_port = 0                    # port devices fetch songs from; 0 = any free port (set one for a firewall rule)
sonos_hosts = ["192.168.1.20"]   # Sonos speakers the network search cannot find, e.g. on another subnet
cover_art = "themed"             # or "album": the album's own art when there is some
```

**If a device is not found:**
- Check that the computer is on the same network as the device. Guest Wi-Fi often keeps devices apart.
- Run `mp3blaster-rs --list-speakers` to see what the search finds.
- For Sonos speakers on another subnet, list one of them under `sonos_hosts`. One speaker is enough, since it reports the rest of the household.
- A TV in standby may not answer. Turn it on, or look in its settings for an option that keeps casting available in standby (often called network standby or remote start).

**Security:**
- **The web server** serves only the songs being played (and their covers), at addresses that contain a random 128-bit key. It does not list folders or serve anything else.
- **Connections to Cast devices** are encrypted. The device's self-signed certificate is accepted without checking, as is usual for Cast software outside Google's own apps.

## AI control (MCP)

AI assistants that speak the [Model Context Protocol](https://modelcontextprotocol.io), such as Claude Code and Claude Desktop, can control the player. You can ask things like:
- "Play some jazz."
- "Queue a few songs from the 80s."
- "What's playing?"
- "Show HYPERSPACE fullscreen."
- "Play this in the living room." / "Put the music on the TV, with the visuals."

**How it works.** `mp3blaster-rs --mcp` is the MCP server the assistant starts. It speaks MCP over stdin/stdout and forwards each tool call to the player running in your terminal, which carries it out and shows a banner such as "Claude Code: queued 5 tracks". Start the player yourself as usual. If it isn't running, the assistant is told so.

There is no port to configure. The player picks a free local port at every start and records it in `control.json`, where `--mcp` finds it.

**Claude Code:**

```sh
claude mcp add mp3blaster -- "C:\path\to\mp3blaster-rs.exe" --mcp
```

**Claude Desktop:**

1. Open **Settings → Developer → Edit Config**. This opens `claude_desktop_config.json`, usually `%APPDATA%\Claude\claude_desktop_config.json`.
2. Add the server, with backslashes doubled. If the file already has an `"mcpServers"` block, add only the `"mp3blaster"` entry to it.

   ```json
   {
     "mcpServers": {
       "mp3blaster": { "command": "C:\\path\\to\\mp3blaster-rs.exe", "args": ["--mcp"] }
     }
   }
   ```
3. Quit Claude Desktop completely (from the tray icon, not just by closing the window), then start it again. **mp3blaster** then appears in the chat's tools menu.

**If it doesn't work:**
- Claude Desktop logs the server's start-up in `mcp-server-mp3blaster.log`, in its `logs` folder (usually `%APPDATA%\Claude\logs`).
- The bridge logs each call to `mcp.log` in the player's state folder (`%LOCALAPPDATA%\mp3blaster-rs`).
- `control.json` exists in that folder only while a player with remote control is running.

**Tools:**

| Tool | Does |
| --- | --- |
| `get_status` | Shows the current track, position, volume, modes, what plays next, the queue and the visualizer. |
| `browse_library` | Counts tracks per genre, artist, album or year. |
| `search_tracks` | Searches by words, genre, artist, album or year range, in the playlist and/or the music folder, optionally in random order. |
| `queue_tracks`, `remove_from_queue`, `clear_queue` | Edit the play queue. `play_now` starts the first track at once. |
| `playback`, `seek`, `set_volume` | Play, pause, toggle, stop, next and previous; seeking; volume. |
| `set_visualizer` | Sets the style (bars, matrix, defrag, hyperspace), fullscreen, the spectrum layout, and the progress bar style. |
| `set_play_options`, `set_theme` | Play mode and repeat; the colour theme for this session. |
| `list_outputs`, `set_output` | List this computer, the Sonos speakers and the Cast devices; move playback to one (by name, or a Sonos room) or back. `visuals` sends a TV the player's pictures too. |

**Search covers the playlist and your music folder.** On the first request, the player indexes the music folder in the background and caches the tags in `library.json` in its state folder. Later starts re-read only the files that changed. A song found only in the folder is added to a "Requests" group in the playlist when it is queued. Every action an assistant takes is also written to the message log (`L`).

```toml
[mcp]
enabled = true                              # false turns remote control off
library_dirs = ["D:/Music", "E:/Archive"]   # folders to search; empty = the music folder
```

**Security.** The player listens on `127.0.0.1` only, on a random port. It writes the port and a random token to `control.json` in your state folder, and accepts only requests that carry that token. So only your own programs can control it, and nothing listens on the network.

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
  - keys: Enter, Backspace, `?`, F3, F4 (the jukebox), F5 (the horse), Ctrl+S/`S`, `L`, `t`, `f`, `w`, `v`, `p`, `m`, `r`, `z`, `R`, `d`, `K`/`J`, `o`, and the play queue (`e`/Ins, `u`, `X`);
  - config: `[general] replaygain`, `[audio] output_device`, `[ui] show_hidden`, `[ui] spectrum_layout`, `[ui] visualizer`, `[ui] progress_style`, and the `[mcp]` and `[outputs]` sections;
  - AI control through MCP (`--mcp`), with a play queue and music folder search;
  - playing on Sonos speakers and Google Cast devices (`o`, `--list-speakers`), with the player's pictures on TVs (needs ffmpeg);
  - the `[keybindings.file_manager]` section.
- **Key conflicts:**
  - The spec's UI mockup shows F1 as Help, but its key table says F1 opens the file manager. The key table wins, so Help is `?`.
  - The spec's key table makes F4 save the playlist. F4 opens the jukebox instead, and Ctrl+S or `S` saves.

## Development

```sh
cargo test                                   # unit, pipeline and malformed-input tests
cargo clippy --all-targets -- -D warnings
tests/fixtures/generate.ps1                  # regenerate audio fixtures (needs ffmpeg)
```

None of the tests need a sound card, and none use your network.

- **Speaker and TV tests:** they talk to a pretend Sonos speaker and a pretend Cast device on localhost (`src/outputs/sonos/fake.rs`, `src/outputs/cast/fake.rs`; the Cast one speaks TLS).
  - The fakes answer the way real devices do and fetch the songs they are given.
  - It can also be told to refuse a file type, fail a command, or act as if blocked by a firewall.
  - To check the real network, run `mp3blaster-rs --list-speakers`. It only searches and plays nothing.
  - To check a real Cast device without changing anything on it: `CAST_DEVICE=<ip>:8009 cargo test --lib real_cast_device -- --ignored --nocapture`.

The terminal backend (`src/ui/backend.rs`) sends only the colours that changed, and each frame is written to the console in a single call. A test replays its output through a terminal emulator (`vt100`) and checks that every cell matches. To measure how much a visualiser sends to the terminal: `cargo test --release --lib hyperspace_terminal_load -- --ignored --nocapture`. The audio pipeline is tested by pulling samples from the renderer exactly as the device callback would. This proves, for example, that a split FLAC file plays back gaplessly and sample-identical to the original.

## License

MIT
