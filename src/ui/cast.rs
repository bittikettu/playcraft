//! Playing on a network device: a Sonos speaker or a Google Cast device such as an Android
//! TV. The local playback goes on, muted, and stays in charge of what plays: the play order,
//! the queue, gapless changes and the visualizers all work as usual. The device follows it:
//! it is given each track the local playback starts, and is paused, resumed and seeked along
//! with it, while the local position is kept in step with the device's (`outputs::sync`).
//! Changes made on the device or in its app (pause, play, stop, volume) are followed back.

use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, TryRecvError, bounded};

use crate::audio::AudioCommand;
use crate::model::PlaybackStatus;
use crate::outputs::card::Card;
use crate::outputs::link::{self, Command, Event, Status, TransportState};
use crate::outputs::media::{self, MediaServer};
use crate::outputs::sync::Offset;
use crate::outputs::video::{self, TvInfo, TvVisual, VideoStream};
use crate::outputs::{self, Device, Link, SongInfo};

use super::app::{App, BannerLevel, Overlay};
use super::keymap::Action;

/// How long the network search for devices listens for answers.
const SEARCH_WAIT: Duration = Duration::from_millis(1500);
/// Casting mid-track starts the speaker at the same second; closer to the start, at 0.
const MIN_START: Duration = Duration::from_secs(2);
/// A natural track change waits at most this long for the speaker to finish its track.
const MAX_FINISH_WAIT: Duration = Duration::from_secs(3);
/// Files above this rate are sent as WAV: no Sonos speaker plays them, and not every Cast
/// device does.
const MAX_RATE: u32 = 48_000;

#[derive(Debug, Clone, Copy)]
pub struct CastTiming {
    pub link: link::Timing,
    /// What the speaker reports this long after our last command was done by someone else
    /// (the Sonos app, the speaker's buttons).
    pub quiet: Duration,
    /// A speaker that has not started a track this long after getting it cannot play it.
    pub start_within: Duration,
    /// Positions read this soon after the local playback jumped are ignored.
    pub settle: Duration,
}

impl Default for CastTiming {
    fn default() -> Self {
        CastTiming {
            link: link::Timing::default(),
            quiet: Duration::from_secs(2),
            start_within: Duration::from_secs(10),
            settle: Duration::from_millis(300),
        }
    }
}

/// What devices show as a song's cover art.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverArt {
    /// A card drawn in the player's theme (`outputs::card`).
    Themed,
    /// The album's own art (from the tags or the folder), else a themed card.
    Album,
}

impl CoverArt {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "themed" => Some(CoverArt::Themed),
            "album" => Some(CoverArt::Album),
            _ => None,
        }
    }
}

/// Settings for network outputs, from `[outputs]`.
#[derive(Debug, Clone)]
pub struct OutputSettings {
    pub http_port: u16,
    pub sonos_hosts: Vec<String>,
    pub cover_art: CoverArt,
    /// Search the network (SSDP and mDNS) besides asking `sonos_hosts`; tests do not.
    pub search_network: bool,
    /// Where devices fetch songs: every adapter (tests use localhost).
    pub bind: IpAddr,
    pub timing: CastTiming,
    /// ffmpeg, for streaming pictures to TVs.
    pub ffmpeg: String,
    /// The H.264 encoder for the TV stream, or `auto`.
    pub tv_encoder: String,
    /// Starts a TV stream (a stand-in in tests).
    pub tv_stream: video::Starter,
}

impl Default for OutputSettings {
    fn default() -> Self {
        OutputSettings {
            http_port: 0,
            sonos_hosts: Vec::new(),
            cover_art: CoverArt::Themed,
            search_network: true,
            bind: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            timing: CastTiming::default(),
            ffmpeg: "ffmpeg".into(),
            tv_encoder: "auto".into(),
            tv_stream: VideoStream::start,
        }
    }
}

/// Devices found on the network.
#[derive(Default)]
pub struct Devices {
    pub list: Vec<Device>,
    pub searching: Option<Receiver<Result<Vec<Device>, String>>>,
    /// A search has finished at least once.
    pub searched: bool,
    pub error: Option<String>,
}

/// The track a speaker was given.
struct Loaded {
    /// `App::track_starts` when the local playback started it.
    start: u64,
    /// The URL path, which identifies the song in what the speaker reports.
    path: String,
    /// It is sent as WAV (because the speaker could not play the original).
    wav: bool,
    /// The speaker refused it (and still reports what it had before).
    refused: bool,
    /// When the speaker was told to play it.
    at: Instant,
    /// The speaker has been seen playing it.
    played: bool,
    /// A failure to start it has been dealt with.
    checked: bool,
}

/// A device playing for us.
pub struct Cast {
    pub device: Device,
    link: Link,
    /// The local volume, restored when the cast ends.
    pub(crate) local_volume: f32,
    loaded: Option<Loaded>,
    /// The speaker was last told to play (rather than pause).
    playing: bool,
    last_command: Instant,
    /// The speaker's last report.
    pub status: Option<Status>,
    offset: Offset,
    settle_until: Instant,
    /// When a natural track change stops waiting for the speaker to finish its track.
    switch_at: Option<Instant>,
    /// The speaker's volume has been read.
    volume_known: bool,
    /// A pause or resume from the speaker that the local playback has yet to carry out
    /// (playing or not, and since when).
    following: Option<(bool, Instant)>,
    /// Sound and pictures streamed to a TV, instead of songs one by one.
    stream: Option<VideoStream>,
}

impl Cast {
    /// The device gets the player's pictures too (a live stream).
    pub fn visuals(&self) -> bool {
        self.stream.is_some()
    }

    fn send(&mut self, command: Command) {
        self.link.send(command);
        self.last_command = Instant::now();
    }

    fn reports_ours(&self, status: &Status) -> bool {
        self.loaded
            .as_ref()
            .is_some_and(|l| status.track_uri.contains(&l.path))
    }

    /// What the speaker is doing with our track, as last reported.
    pub fn state(&self) -> Option<TransportState> {
        self.status
            .as_ref()
            .filter(|s| self.reports_ours(s))
            .map(|s| s.state)
    }

    /// Whether the speaker is done with its track, or should be by now.
    fn finished_track(&mut self) -> bool {
        let playing = self
            .status
            .as_ref()
            .filter(|s| s.state == TransportState::Playing && self.reports_ours(s));
        let Some(status) = playing else {
            return true;
        };
        let left = status
            .duration
            .zip(status.position)
            .map_or(Duration::ZERO, |(d, p)| d.saturating_sub(p));
        let answered = status.answered;
        let at = *self
            .switch_at
            .get_or_insert_with(|| (answered + left).min(Instant::now() + MAX_FINISH_WAIT));
        Instant::now() >= at
    }
}

/// What the speaker did on its own, to follow locally.
enum FromSpeaker {
    Paused,
    Resumed,
    Stopped,
    TakenOver,
}

impl App {
    /// Opens the output picker and looks for devices.
    pub(crate) fn open_outputs(&mut self) {
        let active = self.cast.as_ref().and_then(|c| {
            self.output_targets()
                .iter()
                .position(|(d, visuals)| d.id() == c.device.id() && *visuals == c.visuals())
        });
        self.overlay = Overlay::Outputs {
            cursor: active.map_or(0, |i| i + 1),
        };
        self.search_devices();
        // Listening now lets the firewall ask about it while a device is being picked.
        if let Err(e) = self.ensure_media_server() {
            self.banner(BannerLevel::Error, e);
        }
    }

    /// Starts a search for devices in the background, unless one is running.
    pub(crate) fn search_devices(&mut self) {
        if self.devices.searching.is_some() {
            return;
        }
        let (tx, rx) = bounded(1);
        let hosts = self.outputs.sonos_hosts.clone();
        let network = self.outputs.search_network;
        let spawned = std::thread::Builder::new()
            .name("output-search".into())
            .spawn(move || {
                let _ = tx.send(outputs::discover(&hosts, network, SEARCH_WAIT));
            });
        match spawned {
            Ok(_) => self.devices.searching = Some(rx),
            Err(e) => self.devices.error = Some(format!("cannot search for devices: {e}")),
        }
    }

    fn ensure_media_server(&mut self) -> Result<(), String> {
        if self.media.is_none() {
            let server =
                MediaServer::start(self.outputs.bind, self.outputs.http_port).map_err(|e| {
                    format!(
                        "Cannot share songs with devices on port {}: {e}",
                        self.outputs.http_port
                    )
                })?;
            self.media = Some(server);
        }
        Ok(())
    }

    /// Keys while the output picker is open.
    /// What the output picker offers after this computer: each device, and each Cast
    /// device again with pictures (`true`).
    pub(crate) fn output_targets(&self) -> Vec<(Device, bool)> {
        let mut targets = Vec::new();
        for device in &self.devices.list {
            targets.push((device.clone(), false));
            if matches!(device, Device::Cast(_)) {
                targets.push((device.clone(), true));
            }
        }
        targets
    }

    pub(crate) fn on_outputs_key(&mut self, action: Action, cursor: usize) {
        let targets = self.output_targets();
        let last = targets.len();
        let cursor = cursor.min(last);
        let moved_to = match action {
            Action::CursorUp => cursor.saturating_sub(1),
            Action::CursorDown => (cursor + 1).min(last),
            Action::Home | Action::PageUp => 0,
            Action::End | Action::PageDown => last,
            Action::Activate => {
                self.overlay = Overlay::None;
                match cursor.checked_sub(1).and_then(|i| targets.get(i)) {
                    Some((device, visuals)) => self.start_cast(device.clone(), *visuals),
                    None => self.play_here(),
                }
                return;
            }
            Action::Close | Action::Back | Action::Quit | Action::OutputSelector | Action::Help => {
                self.overlay = Overlay::None;
                return;
            }
            // Playback keys keep working while the picker is open.
            other => return self.on_action(other),
        };
        self.overlay = Overlay::Outputs { cursor: moved_to };
    }

    /// Name of the output playing: a device, or this computer.
    pub fn output_name(&self) -> String {
        match &self.cast {
            Some(c) if c.visuals() => format!("{} + visuals", c.device.name()),
            Some(c) => c.device.name(),
            None => "this computer".into(),
        }
    }

    /// What the TV picture shows besides the music.
    fn tv_info(&self) -> TvInfo {
        let song = self
            .now
            .as_ref()
            .map(|n| self.song_info(&n.path, n.duration));
        let album = song.as_ref().and_then(|s| match (&s.album, s.year) {
            (Some(album), Some(year)) => Some(format!("{album} ({year})")),
            (Some(album), None) => Some(album.clone()),
            (None, Some(year)) => Some(year.to_string()),
            (None, None) => None,
        });
        TvInfo {
            title: song
                .as_ref()
                .map_or_else(|| "mp3blaster-rs".into(), |s| s.title.clone()),
            artist: song.as_ref().and_then(|s| s.artist.clone()),
            album,
            position: self.audio.shared().position(),
            duration: self.now.as_ref().and_then(|n| n.duration),
            playing: self.status() == PlaybackStatus::Playing,
            colors: self.theme.card_colors(),
            palette: super::views::hyperspace::palette(&self.theme),
            visual: match self.visualizer {
                super::app::Visualizer::Bars => TvVisual::Bars,
                _ => TvVisual::Hyperspace,
            },
            track: self.track_hash(),
        }
    }

    /// Moves playback back to this computer.
    pub(crate) fn play_here(&mut self) {
        match self.cast.as_ref().map(|c| c.device.name()) {
            Some(name) => {
                self.end_cast(false);
                self.banner(
                    BannerLevel::Info,
                    format!("Playing on this computer ({name} stopped)"),
                );
            }
            None => self.banner(BannerLevel::Info, "Playing on this computer"),
        }
    }

    /// Moves playback to `device`, at the current position.
    /// With `visuals`, a Cast device gets the player's sound and pictures as a live stream.
    pub(crate) fn start_cast(&mut self, device: Device, visuals: bool) {
        if self
            .cast
            .as_ref()
            .is_some_and(|c| c.device.id() == device.id() && c.visuals() == visuals)
        {
            self.banner(
                BannerLevel::Info,
                format!("Already playing on {}", self.output_name()),
            );
            return;
        }
        if visuals && !matches!(device, Device::Cast(_)) {
            self.banner(
                BannerLevel::Warning,
                format!("{} cannot show pictures", device.name()),
            );
            return;
        }
        if let Err(e) = self.ensure_media_server() {
            self.banner(BannerLevel::Error, e);
            return;
        }
        let local_volume = match self.cast.take() {
            Some(previous) => {
                let v = previous.local_volume;
                close_link(previous, Duration::ZERO);
                v
            }
            None => self.audio.shared().volume(),
        };
        self.unmute_at = None;
        self.audio.shared().set_muted(true);
        log::info!(
            "playing on {} {} at {}",
            device.kind(),
            device.name(),
            device.addr()
        );
        let link = device.connect(self.outputs.timing.link);
        let stream = visuals.then(|| {
            (self.outputs.tv_stream)(
                &self.outputs.ffmpeg,
                &self.outputs.tv_encoder,
                self.audio.capture_slot(),
                self.audio.shared().clone(),
                self.tv_info(),
            )
        });
        let text = if visuals {
            format!(
                "Starting the stream to {}; it plays a few seconds behind",
                device.name()
            )
        } else {
            format!("Playing on {}", device.name())
        };
        self.banner(BannerLevel::Info, text);
        self.cast = Some(Cast {
            device,
            link,
            local_volume,
            loaded: None,
            playing: false,
            last_command: Instant::now(),
            status: None,
            offset: Offset::default(),
            settle_until: Instant::now(),
            switch_at: None,
            volume_known: false,
            following: None,
            stream,
        });
        self.reconcile_cast();
    }

    /// Stops the device and plays on this computer again. With `pause`, local playback is
    /// paused first instead of suddenly coming out of the computer's speakers.
    pub(crate) fn end_cast(&mut self, pause: bool) {
        let Some(cast) = self.cast.take() else {
            return;
        };
        let local_volume = cast.local_volume;
        close_link(cast, Duration::ZERO);
        if pause && self.status() == PlaybackStatus::Playing {
            self.audio.send(AudioCommand::Pause);
            // Unmuted once the pause has taken effect.
            self.unmute_at = Some(Instant::now() + Duration::from_millis(300));
        } else {
            self.audio.shared().set_muted(false);
        }
        self.set_volume(local_volume);
    }

    /// Stops the device when the player quits.
    pub fn close_outputs(&mut self) {
        if let Some(cast) = self.cast.take() {
            // So the session remembers the computer's volume, not the speaker's.
            self.audio.shared().set_volume(cast.local_volume);
            close_link(cast, Duration::from_secs(2));
        }
    }

    /// Seeks `secs` from the current position.
    pub(crate) fn seek_by(&mut self, secs: i64) {
        if self
            .cast
            .as_ref()
            .is_some_and(|c| c.loaded.is_some() && !c.visuals())
        {
            let position = self.audio.shared().position().as_secs() as i64;
            self.seek_to(Duration::from_secs((position + secs).max(0) as u64));
        } else {
            self.audio.send(AudioCommand::SeekRelative(secs));
        }
    }

    pub(crate) fn seek_to(&mut self, target: Duration) {
        let duration = self.now.as_ref().and_then(|n| n.duration);
        let settle = self.outputs.timing.settle;
        // A TV stream just carries on: the seek shows in it.
        let target = match self
            .cast
            .as_mut()
            .filter(|c| c.loaded.is_some() && !c.visuals())
        {
            Some(cast) => {
                // Seeks go to whole seconds (all Sonos takes), the local playback to the same.
                let mut secs = target.as_secs();
                if let Some(d) = duration {
                    secs = secs.min(d.as_secs().saturating_sub(1));
                }
                let target = Duration::from_secs(secs);
                cast.send(Command::Seek(target));
                cast.offset.reset();
                cast.settle_until = Instant::now() + settle;
                target
            }
            None => target,
        };
        self.audio.send(AudioCommand::SeekAbsolute(target));
    }

    /// Sends a volume change to the device, if one is playing.
    pub(crate) fn cast_volume(&mut self, volume: f32) {
        if let Some(cast) = self.cast.as_mut() {
            cast.send(Command::SetVolume((volume * 100.0).round() as u8));
        }
    }

    /// Device searches and the device's reports; keeps the device following the local
    /// playback. Call once per loop iteration. Returns true if anything changed on screen.
    pub fn process_outputs(&mut self) -> bool {
        let mut changed = false;
        if let Some(at) = self.unmute_at
            && Instant::now() >= at
        {
            self.unmute_at = None;
            if self.cast.is_none() {
                self.audio.shared().set_muted(false);
            }
        }
        let result = match self.devices.searching.as_ref().map(Receiver::try_recv) {
            Some(Ok(result)) => Some(result),
            Some(Err(TryRecvError::Disconnected)) => Some(Err("the search failed".into())),
            _ => None,
        };
        if let Some(result) = result {
            self.devices.searching = None;
            self.devices.searched = true;
            match result {
                Ok(list) => {
                    log::info!("found {} device(s) to play on", list.len());
                    self.devices.list = list;
                    self.devices.error = None;
                }
                Err(e) => self.devices.error = Some(e),
            }
            for call in std::mem::take(&mut self.waiting_for_devices) {
                self.answer_call(call);
            }
            changed = true;
        }
        if self.cast.is_some() {
            changed |= self.cast_events();
            self.reconcile_cast();
        }
        changed
    }

    fn cast_events(&mut self) -> bool {
        let Some(cast) = &self.cast else {
            return false;
        };
        let events: Vec<Event> = cast.link.events().try_iter().collect();
        let changed = !events.is_empty();
        for event in events {
            let name = self.output_name();
            match event {
                Event::Status(status) => self.on_cast_status(status),
                Event::Failed {
                    load: true,
                    message,
                    ..
                } => {
                    // Dealt with as a song the speaker cannot play.
                    log::info!("{name} refused a song: {message}");
                    if let Some(loaded) = self.cast.as_mut().and_then(|c| c.loaded.as_mut()) {
                        loaded.refused = true;
                    }
                }
                Event::Failed { message, .. } => {
                    self.banner(BannerLevel::Warning, format!("{name}: {message}"))
                }
                Event::Unreachable(msg) => {
                    self.end_cast(true);
                    self.banner(
                        BannerLevel::Error,
                        format!("Lost {name} ({msg}); paused, back on this computer"),
                    );
                }
            }
            if self.cast.is_none() {
                break;
            }
        }
        changed
    }

    fn on_cast_status(&mut self, status: Status) {
        let local = self.status();
        let local_position = self.audio.shared().position();
        let local_left = self
            .now
            .as_ref()
            .and_then(|n| n.duration)
            .map(|d| d.saturating_sub(local_position));
        let track_starts = self.track_starts;
        let timing = self.outputs.timing;
        let Some(cast) = self.cast.as_mut() else {
            return;
        };
        // Reports read before our last command was carried out show the old state.
        let settled = status.done == cast.link.sent();
        let quiet = settled && cast.last_command.elapsed() >= timing.quiet;
        if let Some(v) = status.volume
            && settled
            && (quiet || !cast.volume_known)
        {
            cast.volume_known = true;
            let v = f32::from(v) / 100.0;
            if (v - self.audio.shared().volume()).abs() > 0.004 {
                self.audio.shared().set_volume(v);
                self.audio.send(AudioCommand::SetVolume(v));
            }
        }
        let ours = cast.reports_ours(&status);
        let streaming = cast.visuals();
        let fresh = settled && status.asked >= cast.settle_until;
        cast.status = Some(status.clone());
        let Some(loaded) = cast.loaded.as_mut() else {
            return;
        };
        if ours && status.state == TransportState::Playing {
            loaded.played = true;
        }
        if !fresh {
            return;
        }
        let from_speaker = match (ours, status.state, cast.playing) {
            // Something else on the speaker, though it took our track.
            (false, state, _) => (quiet
                && !loaded.refused
                && !status.track_uri.is_empty()
                && state != TransportState::Transitioning)
                .then_some(FromSpeaker::TakenOver),
            (true, TransportState::Paused, true) if quiet => Some(FromSpeaker::Paused),
            (true, TransportState::Playing, false) if quiet => Some(FromSpeaker::Resumed),
            // The TV stopped the stream (it never ends by itself).
            (true, TransportState::Stopped, true) if quiet && loaded.played && streaming => {
                Some(FromSpeaker::Stopped)
            }
            // Stopped before the end of the track (at the end, the local playback has moved
            // on or is about to).
            (true, TransportState::Stopped, true)
                if quiet
                    && loaded.played
                    && loaded.start == track_starts
                    && local_left.is_some_and(|l| l > MIN_START) =>
            {
                Some(FromSpeaker::Stopped)
            }
            _ => None,
        };
        let Some(from_speaker) = from_speaker else {
            // Keep the local playback in step with the speaker (a TV stream is behind on
            // purpose).
            let playing = status.state == TransportState::Playing && cast.playing && !streaming;
            if let Some(position) = status.position.filter(|_| playing && ours)
                && local == PlaybackStatus::Playing
            {
                let now = Instant::now();
                cast.offset.observe(
                    position.as_secs_f64(),
                    status.precision.as_secs_f64(),
                    (now - status.asked).as_secs_f64(),
                    (now - status.answered).as_secs_f64(),
                    local_position.as_secs_f64(),
                );
                if let Some(error) = cast.offset.correction() {
                    let mut target = (local_position.as_secs_f64() + error).max(0.0);
                    if let Some(d) = self.now.as_ref().and_then(|n| n.duration) {
                        target = target.min((d.as_secs_f64() - 0.5).max(0.0));
                    }
                    log::debug!("Sonos sync: moving local playback by {error:+.2} s");
                    cast.offset.reset();
                    cast.settle_until = now + timing.settle;
                    self.audio
                        .send(AudioCommand::SeekAbsolute(Duration::from_secs_f64(target)));
                }
            }
            return;
        };
        let name = cast.device.name();
        match from_speaker {
            FromSpeaker::Paused => {
                cast.playing = false;
                cast.following = Some((false, Instant::now()));
                self.audio.send(AudioCommand::Pause);
                self.banner(BannerLevel::Info, format!("Paused on {name}"));
            }
            FromSpeaker::Resumed => {
                cast.playing = true;
                cast.following = Some((true, Instant::now()));
                cast.offset.reset();
                if streaming {
                    // Back to the live edge of the stream.
                    cast.loaded = None;
                }
                self.audio.send(AudioCommand::Resume);
                self.banner(BannerLevel::Info, format!("Playing on {name}"));
            }
            FromSpeaker::Stopped if streaming => {
                self.end_cast(true);
                self.banner(
                    BannerLevel::Info,
                    format!("{name} stopped the stream; paused here"),
                );
            }
            FromSpeaker::Stopped => {
                self.stop();
                self.banner(BannerLevel::Info, format!("Stopped on {name}"));
            }
            FromSpeaker::TakenOver => {
                self.end_cast(true);
                self.banner(
                    BannerLevel::Info,
                    format!("{name} switched to something else; paused here"),
                );
            }
        }
    }

    /// Tags for the speaker's display.
    fn song_info(&self, path: &std::path::Path, duration: Option<Duration>) -> SongInfo {
        let meta = self
            .now
            .as_ref()
            .and_then(|n| n.track)
            .and_then(|u| self.root.track_by_uid(u))
            .and_then(|t| t.metadata.as_ref())
            .or_else(|| self.meta_cache.get(path));
        let title = meta.and_then(|m| m.title.clone()).unwrap_or_else(|| {
            path.file_stem()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        SongInfo {
            title,
            artist: meta.and_then(|m| m.artist.clone()),
            album: meta.and_then(|m| m.album.clone()),
            year: meta.and_then(|m| m.year),
            duration,
            art: None,
        }
    }

    /// A cover card for `song` in the current theme.
    fn cover_card(&self, path: &std::path::Path, song: &SongInfo) -> Card {
        let album = match (&song.album, song.year) {
            (Some(album), Some(year)) => Some(format!("{album} ({year})")),
            (Some(album), None) => Some(album.clone()),
            (None, Some(year)) => Some(year.to_string()),
            (None, None) => None,
        };
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(path, &mut hasher);
        Card {
            title: song.title.clone(),
            artist: song.artist.clone(),
            album,
            colors: self.theme.card_colors(),
            seed: std::hash::Hasher::finish(&hasher),
        }
    }

    /// Makes the speaker play what the local playback plays.
    pub(crate) fn reconcile_cast(&mut self) {
        if self.cast.as_ref().is_some_and(Cast::visuals) {
            return self.reconcile_stream();
        }
        let local = self.status();
        let want = match local {
            PlaybackStatus::Stopped => None,
            _ => self
                .now
                .as_ref()
                .map(|n| (self.track_starts, n.path.clone(), n.duration)),
        };
        let Some(cast) = self.cast.as_mut() else {
            return;
        };
        let have = cast.loaded.as_ref().map(|l| l.start);
        if want.as_ref().map(|w| w.0) == have {
            if self.pending_load.is_none() {
                // An explicit play or stop has been followed (or there was nothing to do).
                self.cut_now = false;
            }
        } else {
            // A track that ends by itself first finishes on the speaker too.
            if !self.cut_now && have.is_some() && !cast.finished_track() {
                return;
            }
            self.cut_now = false;
            self.switch_cast_track(local, want);
        }
        self.mirror_play_pause(local);
    }

    fn switch_cast_track(
        &mut self,
        local: PlaybackStatus,
        want: Option<(u64, std::path::PathBuf, Option<Duration>)>,
    ) {
        let Some(cast) = self.cast.as_mut() else {
            return;
        };
        cast.switch_at = None;
        let Some((start, path, duration)) = want else {
            if cast.loaded.take().is_some() {
                cast.send(Command::Stop);
            }
            cast.playing = false;
            return;
        };
        // Hi-res files go as WAV right away.
        let wav = self
            .now
            .as_ref()
            .is_some_and(|n| n.info.sample_rate > MAX_RATE);
        self.send_track(local, start, &path, duration, wav);
    }

    /// Gives the speaker `path` (the local playback's track number `start`), as WAV with
    /// `wav`, at the local position.
    fn send_track(
        &mut self,
        local: PlaybackStatus,
        start: u64,
        path: &std::path::Path,
        duration: Option<Duration>,
        wav: bool,
    ) {
        let settle = self.outputs.timing.settle;
        let song = self.song_info(path, duration);
        let card = self.cover_card(path, &song);
        let cover_art = self.outputs.cover_art;
        let (Some(cast), Some(media)) = (self.cast.as_mut(), self.media.as_ref()) else {
            return;
        };
        let addr = cast.device.addr();
        let url = match media.url_for(path, wav, addr) {
            Ok(url) => url,
            Err(e) => {
                let name = cast.device.name();
                self.end_cast(true);
                self.banner(BannerLevel::Error, format!("Cannot reach {name}: {e}"));
                return;
            }
        };
        let position = self.audio.shared().position();
        let begin = if position < MIN_START {
            Duration::ZERO
        } else {
            Duration::from_secs(position.as_secs())
        };
        let play = local == PlaybackStatus::Playing;
        log::info!(
            "{}: playing {}{} from {} s",
            cast.device.name(),
            path.display(),
            if wav { " as WAV" } else { "" },
            begin.as_secs()
        );
        let mime = if wav {
            "audio/wav"
        } else {
            media::mime_type(path)
        };
        let mut song = song;
        song.art = match cover_art {
            CoverArt::Album => media
                .art_url_for(path, addr)
                .or_else(|| media.card_url_for(path, card, addr)),
            CoverArt::Themed => media.card_url_for(path, card, addr),
        };
        cast.send(Command::Load {
            url,
            mime: mime.into(),
            song,
            start: begin,
            play,
            live: false,
        });
        cast.loaded = Some(Loaded {
            start,
            path: media.publish(path, wav),
            wav,
            refused: false,
            at: Instant::now(),
            played: false,
            checked: false,
        });
        cast.playing = play;
        cast.offset.reset();
        cast.settle_until = Instant::now() + settle;
        if !begin.is_zero() {
            // The local playback goes to the same second as the speaker.
            self.audio.send(AudioCommand::SeekAbsolute(begin));
        }
    }

    /// Pauses and resumes the speaker with the local playback, and notices a speaker that
    /// cannot play what it was given.
    fn mirror_play_pause(&mut self, local: PlaybackStatus) {
        let start_within = self.outputs.timing.start_within;
        let (Some(cast), Some(media)) = (self.cast.as_mut(), self.media.as_ref()) else {
            return;
        };
        let Some(loaded) = cast.loaded.as_mut() else {
            return;
        };
        let playing = local == PlaybackStatus::Playing;
        if let Some((want, since)) = cast.following {
            // Not an answer to send back: the local playback is still catching up.
            if playing != want && since.elapsed() < Duration::from_secs(1) {
                return;
            }
            cast.following = None;
        }
        if playing != cast.playing {
            if playing {
                loaded.at = Instant::now();
            }
            cast.playing = playing;
            cast.offset.reset();
            cast.send(if playing {
                Command::Play
            } else {
                Command::Pause
            });
            return;
        }
        // Only a speaker that answers can be blamed (one that does not is reported lost).
        let answering = cast.status.as_ref().is_some_and(|s| s.answered > loaded.at);
        let due = loaded.refused || loaded.at.elapsed() >= start_within;
        if !cast.playing || !answering || loaded.played || loaded.checked || !due {
            return;
        }
        loaded.checked = true;
        let fetched = media.fetches(&loaded.path) > 0;
        let (name, port) = (cast.device.name(), media.port());
        let (start, wav) = (loaded.start, loaded.wav);
        let file = self
            .now
            .as_ref()
            .and_then(|n| n.path.file_name())
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        // A Sonos speaker refuses file types before fetching anything; a Cast device always
        // tries to fetch first, so one that never did cannot reach us.
        let reached = fetched || (loaded.refused && matches!(cast.device, Device::Sonos(_)));
        if reached && !wav {
            // Every device plays WAV.
            log::info!("{name} cannot play {file}; sending it as WAV");
            if let Some(now) = self.now.as_ref().map(|n| (n.path.clone(), n.duration)) {
                self.send_track(local, start, &now.0, now.1, true);
            }
        } else if reached {
            self.banner(
                BannerLevel::Warning,
                format!("{name} cannot play {file}; skipping it"),
            );
            self.next_track();
        } else {
            log::warn!("{name} never fetched a song from port {port}");
            self.end_cast(true);
            self.banner(
                BannerLevel::Error,
                format!(
                    "{name} cannot fetch songs from this computer. Allow mp3blaster-rs \
                     through the firewall for private networks (port {port}), then pick it \
                     again with o."
                ),
            );
        }
    }
}

/// Stops the device if it plays for us, and ends the link (waiting at most `wait`), and
/// the TV stream if there is one.
fn close_link(mut cast: Cast, wait: Duration) {
    if let Some(stream) = cast.stream.take() {
        stream.stop(wait.min(Duration::from_millis(300)));
    }
    if cast.loaded.is_some() {
        cast.send(Command::Stop);
    }
    cast.link.close(wait);
}

impl App {
    /// The TV stream: once it has started, the TV is given it once; after that it just
    /// carries whatever the player plays. Problems end the cast.
    fn reconcile_stream(&mut self) {
        let info = self.tv_info();
        let start_within = self.outputs.timing.start_within;
        let (Some(cast), Some(media)) = (self.cast.as_mut(), self.media.as_ref()) else {
            return;
        };
        let Some(stream) = cast.stream.as_ref() else {
            return;
        };
        stream.set_info(info);
        let name = cast.device.name();
        if let Some(why) = stream.failure() {
            self.end_cast(true);
            self.banner(
                BannerLevel::Error,
                format!("The stream to {name} stopped: {why}"),
            );
            return;
        }
        let Some(loaded) = cast.loaded.as_mut() else {
            if !stream.ready() {
                return;
            }
            let addr = cast.device.addr();
            let url = match media.dir_url_for(stream.dir(), stream.playlist_name(), addr) {
                Ok(url) => url,
                Err(e) => {
                    self.end_cast(true);
                    self.banner(BannerLevel::Error, format!("Cannot reach {name}: {e}"));
                    return;
                }
            };
            let path = url
                .strip_prefix("http://")
                .and_then(|rest| rest.find('/').map(|i| rest[i..].to_string()))
                .unwrap_or_else(|| url.clone());
            log::info!("{name}: streaming {url}");
            cast.send(Command::Load {
                url,
                mime: "application/x-mpegURL".into(),
                song: SongInfo {
                    title: "mp3blaster-rs".into(),
                    artist: Some("Live from the player".into()),
                    ..SongInfo::default()
                },
                start: Duration::ZERO,
                play: true,
                live: true,
            });
            cast.loaded = Some(Loaded {
                start: self.track_starts,
                path,
                wav: false,
                refused: false,
                at: Instant::now(),
                played: false,
                checked: false,
            });
            cast.playing = true;
            return;
        };
        // A TV that never plays the stream: refused, or it cannot reach us.
        let answering = cast.status.as_ref().is_some_and(|s| s.answered > loaded.at);
        let due = loaded.refused || loaded.at.elapsed() >= start_within;
        if !answering || loaded.played || loaded.checked || !due {
            return;
        }
        loaded.checked = true;
        let fetched = media.fetches(&loaded.path) > 0;
        let port = media.port();
        self.end_cast(true);
        let why = if fetched {
            format!("{name} could not play the stream")
        } else {
            format!(
                "{name} cannot fetch the stream from this computer. Allow mp3blaster-rs \
                 through the firewall for private networks (port {port}), then pick it again \
                 with o."
            )
        };
        self.banner(BannerLevel::Error, why);
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    use super::*;
    use crate::audio::{AudioEvent, StreamInfo};
    use crate::model::{Group, Track};
    use crate::outputs::sonos::fake::{FakeSonos, FakeState};
    use crate::ui::keymap::KeyBinding;
    use crate::ui::tests::{render, test_app};

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    const SONGS: [&str; 3] = ["sine_44k_cbr.mp3", "chirp_full.flac", "tagged.mp3"];

    fn setup() -> (App, FakeSonos) {
        let mut root = Group::new(0, "Root");
        root.tracks = SONGS.iter().map(|s| Track::new(fixture(s))).collect();
        let mut app = test_app(root);
        app.outputs.timing = CastTiming {
            link: link::Timing {
                poll_playing: Duration::from_millis(20),
                poll_idle: Duration::from_millis(20),
                after_command: Duration::from_millis(5),
                volume_every: Duration::from_millis(20),
                unreachable_after: Duration::from_millis(300),
            },
            quiet: Duration::from_millis(100),
            start_within: Duration::from_millis(600),
            settle: Duration::ZERO,
        };
        (app, FakeSonos::start("Living Room"))
    }

    /// What the audio thread reports when track `i` becomes audible.
    fn started(app: &mut App, i: usize) {
        let path = app.root.tracks[i].path.clone();
        app.on_audio_event(AudioEvent::TrackStarted {
            path,
            duration: Some(Duration::from_secs(10)),
            info: StreamInfo {
                codec: "mp3".into(),
                sample_rate: 44_100,
                channels: 2,
            },
        });
    }

    /// Plays track `i` locally, as from the keyboard. (The test audio output never plays, so
    /// the position stays where it is put; the songs are two seconds long.)
    fn play(app: &mut App, i: usize) {
        let uid = app.root.tracks[i].uid;
        app.play(uid, true);
        wait_app(app, "local playback", |a| {
            a.status() == PlaybackStatus::Playing && a.playing == Some(uid)
        });
    }

    fn enter() -> KeyBinding {
        KeyBinding::new(KeyCode::Enter, KeyModifiers::NONE)
    }

    /// Runs the app's loop until `check` holds for the speaker.
    fn wait_speaker(
        app: &mut App,
        fake: &FakeSonos,
        what: &str,
        check: impl Fn(&FakeState) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !check(&fake.state()) {
            app.process_outputs();
            assert!(
                Instant::now() < deadline,
                "speaker never got to: {what}; actions: {:?}; banner: {:?}",
                fake.state().actions,
                app.banner
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn wait_app(app: &mut App, what: &str, check: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !check(app) {
            app.process_outputs();
            app.process_audio_events();
            assert!(
                Instant::now() < deadline,
                "never got to: {what}; banner: {:?}",
                app.banner
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn banner(app: &App) -> String {
        app.banner
            .as_ref()
            .map(|b| b.text.clone())
            .unwrap_or_default()
    }

    #[test]
    fn the_speaker_plays_what_the_player_plays() {
        let (mut app, fake) = setup();
        app.set_volume(0.8);
        play(&mut app, 0);
        app.start_cast(Device::Sonos(fake.zone()), false);
        assert!(app.audio.shared().muted(), "this computer goes quiet");
        assert_eq!(banner(&app), "Playing on Living Room");

        // The speaker gets the song, with tags, and fetches it from us.
        let song = std::fs::read(fixture(SONGS[0])).unwrap();
        wait_speaker(&mut app, &fake, "fetch the first song", |s| {
            s.transport == "PLAYING" && s.fetched.as_deref() == Some(&song[..])
        });
        let first_uri = fake.state().uri.clone();
        assert!(first_uri.ends_with(".mp3"), "{first_uri}");
        assert!(
            fake.state()
                .metadata
                .contains("<dc:title>sine_44k_cbr</dc:title>")
        );
        // The speaker's volume is shown (and controlled) from now on.
        wait_app(&mut app, "speaker volume", |a| {
            (a.audio.shared().volume() - 0.25).abs() < 0.001
        });
        app.on_action(Action::VolumeUp);
        wait_speaker(&mut app, &fake, "volume 30", |s| s.volume == 30);

        app.on_action(Action::PlayPause);
        wait_speaker(&mut app, &fake, "pause", |s| {
            s.transport == "PAUSED_PLAYBACK"
        });
        app.on_action(Action::PlayPause);
        wait_speaker(&mut app, &fake, "resume", |s| s.transport == "PLAYING");
        app.seek_to(Duration::from_millis(1_400));
        wait_speaker(&mut app, &fake, "seek", |s| {
            s.position == Duration::from_secs(1)
        });

        // Next track: the speaker switches at once.
        app.on_action(Action::NextTrack);
        started(&mut app, 1);
        let song = std::fs::read(fixture(SONGS[1])).unwrap();
        wait_speaker(&mut app, &fake, "fetch the second song", |s| {
            s.fetched.as_deref() == Some(&song[..])
        });
        assert_ne!(fake.state().uri, first_uri);
        let screen = render(&mut app, 120, 30);
        assert!(screen.contains("On:     Living Room"), "{screen}");

        // Back to this computer: the speaker stops, the computer's volume returns.
        app.play_here();
        wait_speaker(&mut app, &fake, "stop", |s| s.transport == "STOPPED");
        assert!(app.cast.is_none());
        assert!(!app.audio.shared().muted());
        assert!((app.audio.shared().volume() - 0.8).abs() < 0.001);
        assert_eq!(app.snapshot().volume, 0.8);
    }

    #[test]
    fn a_track_that_ends_by_itself_finishes_on_the_speaker_first() {
        let (mut app, fake) = setup();
        play(&mut app, 0);
        app.start_cast(Device::Sonos(fake.zone()), false);
        wait_speaker(&mut app, &fake, "play", |s| s.transport == "PLAYING");
        {
            let mut s = fake.state();
            s.duration = Some(Duration::from_secs(10));
            s.position = Duration::from_secs(9);
        }
        // Let a report with the new position arrive.
        let asked = Instant::now();
        wait_app(&mut app, "position report", |a| {
            a.cast
                .as_ref()
                .and_then(|c| c.status.as_ref())
                .is_some_and(|s| s.asked > asked)
        });
        // The local playback moves on gaplessly while the speaker has a second left.
        started(&mut app, 1);
        let until = Instant::now() + Duration::from_millis(300);
        while Instant::now() < until {
            app.process_outputs();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(fake.state().count("SetAVTransportURI"), 1, "not cut short");
        assert!(app.cast.is_some() && app.status() == PlaybackStatus::Playing);
        // The speaker reaches the end: now it gets the next track.
        fake.state().transport = "STOPPED".into();
        wait_speaker(&mut app, &fake, "next track", |s| {
            s.count("SetAVTransportURI") == 2 && s.transport == "PLAYING"
        });
        assert!(app.status() == PlaybackStatus::Playing, "{}", banner(&app));
    }

    #[test]
    fn the_local_playback_keeps_in_step_with_the_speaker() {
        let (mut app, fake) = setup();
        play(&mut app, 0);
        app.start_cast(Device::Sonos(fake.zone()), false);
        wait_speaker(&mut app, &fake, "play", |s| s.transport == "PLAYING");
        // The speaker is a second in, the (stalled, in tests) local playback at 0.
        fake.state().position = Duration::from_secs(1);
        wait_app(&mut app, "local position follows", |a| {
            let p = a.audio.shared().position().as_secs_f64();
            (0.9..=1.6).contains(&p)
        });
    }

    #[test]
    fn changes_made_on_the_speaker_are_followed() {
        let (mut app, fake) = setup();
        play(&mut app, 0);
        app.start_cast(Device::Sonos(fake.zone()), false);
        wait_speaker(&mut app, &fake, "play", |s| s.transport == "PLAYING");

        fake.state().transport = "PAUSED_PLAYBACK".into();
        wait_app(&mut app, "paused here", |a| {
            a.status() == PlaybackStatus::Paused
        });
        assert_eq!(banner(&app), "Paused on Living Room");
        fake.state().transport = "PLAYING".into();
        wait_app(&mut app, "playing here", |a| {
            a.status() == PlaybackStatus::Playing
        });
        fake.state().volume = 60;
        wait_app(&mut app, "volume from the speaker", |a| {
            (a.audio.shared().volume() - 0.6).abs() < 0.001
        });

        // Another app takes the speaker: stop following it, and pause instead of playing
        // out loud here.
        fake.state().uri = "x-sonos-spotify:spotify%3atrack%3a1".into();
        wait_app(&mut app, "cast ended", |a| a.cast.is_none());
        let text = banner(&app);
        assert!(text.contains("switched to something else"), "{text}");
        wait_app(&mut app, "paused here", |a| {
            a.status() == PlaybackStatus::Paused
        });
        wait_app(&mut app, "unmuted", |a| !a.audio.shared().muted());
    }

    #[test]
    fn a_speaker_that_cannot_fetch_songs_is_explained() {
        let (mut app, fake) = setup();
        fake.state().fetch = false;
        play(&mut app, 0);
        app.start_cast(Device::Sonos(fake.zone()), false);
        wait_app(&mut app, "cast ended", |a| a.cast.is_none());
        let text = banner(&app);
        assert!(text.contains("firewall"), "{text}");
        let port = app.media.as_ref().unwrap().port().to_string();
        assert!(text.contains(&port), "{text}");
        wait_app(&mut app, "paused here", |a| {
            a.status() == PlaybackStatus::Paused
        });
    }

    #[test]
    fn a_song_the_speaker_cannot_play_is_sent_as_wav() {
        let (mut app, fake) = setup();
        fake.state().refuse.push(".mp3".into());
        play(&mut app, 0);
        app.start_cast(Device::Sonos(fake.zone()), false);
        wait_speaker(&mut app, &fake, "WAV", |s| {
            s.uri.ends_with(".wav") && s.transport == "PLAYING"
        });
        wait_speaker(&mut app, &fake, "fetch the WAV", |s| s.fetched.is_some());
        assert_eq!(&fake.state().fetched.as_ref().unwrap()[..4], b"RIFF");
        assert!(fake.state().metadata.contains("audio/wav"));
        assert_eq!(fake.state().count("SetAVTransportURI"), 2);
        assert_eq!(app.banner.as_ref().unwrap().text, "Playing on Living Room");
    }

    #[test]
    fn a_song_the_speaker_cannot_play_at_all_is_skipped() {
        let (mut app, fake) = setup();
        fake.state().fail.insert("Play".into(), 701);
        play(&mut app, 0);
        app.start_cast(Device::Sonos(fake.zone()), false);
        let next = app.root.tracks[1].uid;
        wait_app(&mut app, "skipped", |a| a.pending_load == Some(next));
        let text = banner(&app);
        assert!(text.contains("cannot play sine_44k_cbr.mp3"), "{text}");
        assert!(app.cast.is_some());
        assert_eq!(
            fake.state().count("SetAVTransportURI"),
            2,
            "tried as WAV first"
        );
        assert!(fake.state().uri.ends_with(".wav"));
    }

    #[test]
    fn a_cast_device_plays_what_the_player_plays() {
        use crate::outputs::cast::fake::FakeCast;
        let (mut app, _) = setup();
        let tv = FakeCast::start("Televisio");
        tv.state().other_app = Some("Netflix".into());
        app.set_volume(0.8);
        play(&mut app, 0);
        app.start_cast(Device::Cast(tv.device()), false);
        assert!(app.audio.shared().muted());

        // The TV starts its media player and fetches the song, with its tags.
        let song = std::fs::read(fixture(SONGS[0])).unwrap();
        let fetched = |s: &crate::outputs::cast::fake::FakeCastState, song: &[u8]| {
            s.player_state == "PLAYING" && s.fetched.as_deref() == Some(song)
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !fetched(&tv.state(), &song) {
            app.process_outputs();
            assert!(Instant::now() < deadline, "{:?}", tv.state().messages);
            std::thread::sleep(Duration::from_millis(5));
        }
        {
            let s = tv.state();
            assert!(s.app && s.other_app.is_none());
            assert_eq!(s.content_type, "audio/mpeg");
            assert_eq!(s.metadata["title"], "sine_44k_cbr");
        }
        // Its cover art is a card drawn in the player's theme.
        let art = tv.state().metadata["images"][0]["url"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(art.ends_with(".png"), "{art}");
        let card = crate::outputs::sonos::fake::fetch(&art).unwrap();
        assert_eq!(&card[1..4], b"PNG", "a PNG file");
        // Another theme makes another card (with an address of its own).
        let dracula = app.media.as_ref().unwrap().publish_card(
            &fixture(SONGS[0]),
            app.cover_card(&fixture(SONGS[0]), &app.song_info(&fixture(SONGS[0]), None)),
        );
        app.theme = crate::ui::theme::Theme::nord();
        let nord = app.media.as_ref().unwrap().publish_card(
            &fixture(SONGS[0]),
            app.cover_card(&fixture(SONGS[0]), &app.song_info(&fixture(SONGS[0]), None)),
        );
        assert_ne!(dracula, nord);
        // The TV's volume is shown and set.
        wait_app(&mut app, "TV volume", |a| {
            (a.audio.shared().volume() - 0.3).abs() < 0.001
        });
        app.on_action(Action::VolumeUp);
        let deadline = Instant::now() + Duration::from_secs(5);
        while (tv.state().volume - 0.35).abs() > 1e-6 {
            app.process_outputs();
            assert!(Instant::now() < deadline, "volume {}", tv.state().volume);
            std::thread::sleep(Duration::from_millis(5));
        }

        // Paused with the TV's remote: followed here.
        tv.state().player_state = "PAUSED".into();
        wait_app(&mut app, "paused here", |a| {
            a.status() == PlaybackStatus::Paused
        });
        app.on_action(Action::PlayPause);
        let deadline = Instant::now() + Duration::from_secs(5);
        while tv.state().player_state != "PLAYING" {
            app.process_outputs();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }

        // Back to this computer: the TV's player app is closed again.
        app.play_here();
        let deadline = Instant::now() + Duration::from_secs(5);
        while tv.state().app {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!app.audio.shared().muted());
        assert!((app.audio.shared().volume() - 0.8).abs() < 0.001);
    }

    #[test]
    fn a_tv_that_cannot_reach_us_is_explained() {
        use crate::outputs::cast::fake::FakeCast;
        let (mut app, _) = setup();
        let tv = FakeCast::start("Televisio");
        // It cannot fetch, and so refuses the song (as a TV behind a firewall does).
        tv.state().fetch = false;
        tv.state().refuse.push("audio/mpeg".into());
        play(&mut app, 0);
        app.start_cast(Device::Cast(tv.device()), false);
        wait_app(&mut app, "cast ended", |a| a.cast.is_none());
        let text = banner(&app);
        assert!(text.contains("firewall"), "{text}");
        assert_eq!(tv.state().count("media LOAD"), 1, "no WAV retry");
    }

    #[test]
    fn another_app_on_the_tv_hands_back_to_this_computer_paused() {
        use crate::outputs::cast::fake::FakeCast;
        let (mut app, _) = setup();
        let tv = FakeCast::start("Televisio");
        play(&mut app, 0);
        app.start_cast(Device::Cast(tv.device()), false);
        wait_app(&mut app, "playing on the TV", |a| {
            a.cast.as_ref().and_then(|c| c.state()) == Some(TransportState::Playing)
        });
        {
            let mut s = tv.state();
            s.app = false;
            s.other_app = Some("YouTube".into());
        }
        wait_app(&mut app, "cast ended", |a| a.cast.is_none());
        let text = banner(&app);
        assert_eq!(text, "Televisio switched to something else; paused here");
        wait_app(&mut app, "paused here", |a| {
            a.status() == PlaybackStatus::Paused
        });
    }

    /// Runs the app's loop until `check` holds for the pretend TV.
    fn wait_tv(
        app: &mut App,
        tv: &crate::outputs::cast::fake::FakeCast,
        what: &str,
        check: impl Fn(&crate::outputs::cast::fake::FakeCastState) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !check(&tv.state()) {
            app.process_outputs();
            app.process_audio_events();
            assert!(
                Instant::now() < deadline,
                "TV never got to: {what}; messages: {:?}; banner: {:?}",
                tv.state().messages,
                app.banner
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_tv_gets_sound_and_pictures_as_one_live_stream() {
        use crate::outputs::cast::fake::FakeCast;
        let (mut app, _) = setup();
        app.outputs.tv_stream = VideoStream::stand_in;
        let tv = FakeCast::start("Televisio");
        app.devices.list = vec![Device::Cast(tv.device())];
        play(&mut app, 0);
        // The picker offers the TV twice: as a speaker, and with pictures.
        app.on_action(Action::OutputSelector);
        let screen = render(&mut app, 100, 30);
        assert!(screen.contains("Televisio + visuals"), "{screen}");
        app.on_key(KeyBinding::plain('j'));
        app.on_key(KeyBinding::plain('j'));
        app.on_key(enter());
        assert!(app.cast.as_ref().is_some_and(Cast::visuals));
        assert_eq!(app.output_name(), "Televisio + visuals");
        assert!(app.audio.shared().muted());

        // The TV gets the live stream, fetches its playlist from us, and plays it.
        wait_tv(&mut app, &tv, "the stream", |s| {
            s.stream_type == "LIVE" && s.player_state == "PLAYING" && s.fetched.is_some()
        });
        assert_eq!(tv.state().content_type, "application/x-mpegURL");
        assert!(String::from_utf8_lossy(tv.state().fetched.as_ref().unwrap()).contains("#EXTINF"));
        let status = app
            .run_tool("Claude", "get_status", serde_json::json!({}))
            .unwrap();
        assert_eq!(status["output"]["visuals"], true);

        // A new song, a seek, a pause: all in the stream, nothing sent to the TV.
        app.on_action(Action::NextTrack);
        started(&mut app, 1);
        app.seek_to(Duration::from_millis(1_400));
        app.on_action(Action::PlayPause);
        wait_app(&mut app, "paused", |a| a.status() == PlaybackStatus::Paused);
        let until = Instant::now() + Duration::from_millis(300);
        while Instant::now() < until {
            app.process_outputs();
            std::thread::sleep(Duration::from_millis(5));
        }
        {
            let s = tv.state();
            assert_eq!(s.count("media LOAD"), 1, "{:?}", s.messages);
            assert_eq!(s.count("media SEEK") + s.count("media PAUSE"), 0);
        }
        let info = app.cast.as_ref().unwrap().stream.as_ref().unwrap().info();
        assert_eq!(info.title, "chirp_full");
        assert!(!info.playing);
        app.on_action(Action::PlayPause);

        // Paused with the TV's remote: the player pauses; resumed: back to the live edge.
        wait_app(&mut app, "playing again", |a| {
            a.status() == PlaybackStatus::Playing
        });
        tv.state().player_state = "PAUSED".into();
        wait_app(&mut app, "paused here", |a| {
            a.status() == PlaybackStatus::Paused
        });
        tv.state().player_state = "PLAYING".into();
        wait_tv(&mut app, &tv, "a fresh load", |s| {
            s.count("media LOAD") == 2
        });
        assert_eq!(app.status(), PlaybackStatus::Playing);

        // Back to this computer: the TV's player app closes, the stream stops.
        let dir = app
            .cast
            .as_ref()
            .unwrap()
            .stream
            .as_ref()
            .unwrap()
            .dir()
            .to_path_buf();
        app.play_here();
        let deadline = Instant::now() + Duration::from_secs(5);
        while tv.state().app {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!dir.exists());
        assert!(!app.audio.shared().muted());
    }

    #[test]
    fn a_stream_that_fails_ends_the_cast() {
        use crate::outputs::cast::fake::FakeCast;
        let (mut app, _) = setup();
        app.outputs.tv_stream = VideoStream::stand_in;
        let tv = FakeCast::start("Televisio");
        play(&mut app, 0);
        app.start_cast(Device::Cast(tv.device()), true);
        wait_tv(&mut app, &tv, "the stream", |s| s.stream_type == "LIVE");
        let stream = app.cast.as_ref().unwrap().stream.as_ref().unwrap();
        stream.fail("ffmpeg stopped");
        wait_app(&mut app, "cast ended", |a| a.cast.is_none());
        assert_eq!(
            banner(&app),
            "The stream to Televisio stopped: ffmpeg stopped"
        );
        wait_app(&mut app, "paused here", |a| {
            a.status() == PlaybackStatus::Paused
        });

        // Sonos speakers cannot show pictures.
        let sonos = FakeSonos::start("Kitchen");
        app.start_cast(Device::Sonos(sonos.zone()), true);
        assert!(app.cast.is_none());
        assert_eq!(banner(&app), "Kitchen cannot show pictures");
    }

    #[test]
    fn a_speaker_that_goes_away_hands_back_to_this_computer_paused() {
        let (mut app, fake) = setup();
        play(&mut app, 0);
        let gone = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let mut zone = fake.zone();
        zone.coordinator = gone;
        app.start_cast(Device::Sonos(zone), false);
        wait_app(&mut app, "cast ended", |a| a.cast.is_none());
        let text = banner(&app);
        assert!(text.starts_with("Lost Living Room"), "{text}");
        wait_app(&mut app, "paused here", |a| {
            a.status() == PlaybackStatus::Paused
        });
    }

    #[test]
    fn the_picker_lists_speakers_and_starts_one() {
        let (mut app, fake) = setup();
        app.outputs.sonos_hosts = vec![fake.addr.to_string()];
        play(&mut app, 0);
        app.on_action(Action::OutputSelector);
        assert_eq!(app.overlay, Overlay::Outputs { cursor: 0 });
        let screen = render(&mut app, 100, 30);
        assert!(screen.contains("This computer"), "{screen}");
        assert!(screen.contains("[playing]"));
        assert!(screen.contains("Searching for speakers and TVs"));
        wait_app(&mut app, "search", |a| a.devices.searched);
        let screen = render(&mut app, 100, 30);
        println!("{screen}");
        assert!(screen.contains("Living Room"), "{screen}");

        app.on_key(KeyBinding::plain('j'));
        app.on_key(KeyBinding::plain('j'));
        assert_eq!(
            app.overlay,
            Overlay::Outputs { cursor: 1 },
            "stops at the end"
        );
        app.on_key(enter());
        assert_eq!(app.overlay, Overlay::None);
        assert_eq!(app.output_name(), "Living Room");
        wait_speaker(&mut app, &fake, "play", |s| s.transport == "PLAYING");

        // Opening it again puts the cursor on the speaker playing.
        app.on_action(Action::OutputSelector);
        assert_eq!(app.overlay, Overlay::Outputs { cursor: 1 });
        app.on_key(KeyBinding::plain('k'));
        app.on_key(enter());
        assert!(app.cast.is_none());
        let text = banner(&app);
        assert!(text.contains("Living Room stopped"), "{text}");
    }
}
