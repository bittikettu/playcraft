//! Google Cast devices: Android and Google TVs, Chromecasts, Nest speakers. The player
//! connects to a device (`channel`), starts Google's Default Media Receiver on it, and has
//! that play songs from the media server.

pub mod channel;
pub mod discovery;
#[cfg(test)]
pub(crate) mod fake;

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use serde_json::{Map, Value, json};

use channel::{Channel, Message, NS_CONNECTION, NS_HEARTBEAT, NS_MEDIA, NS_RECEIVER, TlsStream};
pub use discovery::CastDevice;

use super::link::{Command, Event, Link, Reachability, SongInfo, Status, Timing, TransportState};

/// Google's Default Media Receiver, a player every Cast device has.
pub const MEDIA_RECEIVER: &str = "CC1AD845";
const SENDER: &str = "sender-0";
const PLATFORM: &str = "receiver-0";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
/// Starting the player app on a TV, and loading a song (the device fetches it first), can
/// take a while.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(15);
const LOAD_TIMEOUT: Duration = Duration::from_secs(20);
const PING_EVERY: Duration = Duration::from_secs(5);
/// A device that sends nothing (not even a heartbeat) for this long is gone.
const SILENCE_LIMIT: Duration = Duration::from_secs(20);

#[derive(Debug, thiserror::Error)]
pub enum CastError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("no answer to {0}")]
    Timeout(&'static str),
    #[error("the device cannot play this song ({0})")]
    LoadFailed(String),
    #[error("the device refused {0}")]
    Refused(String),
}

impl CastError {
    /// Whether the connection is unusable after this error.
    fn is_fatal(&self) -> bool {
        matches!(self, CastError::Io(_) | CastError::Timeout(_))
    }
}

/// The media receiver app running on the device.
#[derive(Debug, Clone, PartialEq)]
struct App {
    transport: String,
    session: String,
    /// We started it (and so close it when done).
    launched: bool,
    /// We have connected to its transport.
    connected: bool,
}

#[derive(Debug, Default)]
struct Media {
    session: Option<i64>,
    content_id: String,
    state: Option<TransportState>,
    position: Option<f64>,
    duration: Option<f64>,
}

/// Connects to a Cast device.
pub fn start(device: CastDevice, timing: Timing) -> Link {
    Link::spawn("cast-link", move |commands, events| {
        run(&device, timing, &commands, &events)
    })
}

struct Session {
    channel: Channel<TlsStream>,
    next_request: u64,
    last_heard: Instant,
    last_ping: Instant,
    app: Option<App>,
    /// The app the device shows instead of ours, if any.
    other_app: Option<String>,
    /// Our app was running and has gone away.
    app_closed: bool,
    media: Media,
    volume: Option<u8>,
    /// The device could not play what it was given (reported once).
    playback_failed: bool,
}

fn seconds(d: Duration) -> f64 {
    (d.as_secs_f64() * 1000.0).round() / 1000.0
}

impl Session {
    fn open(addr: SocketAddr) -> Result<Session, CastError> {
        let channel = Channel::connect(addr, CONNECT_TIMEOUT)?;
        let mut session = Session {
            channel,
            next_request: 1,
            last_heard: Instant::now(),
            last_ping: Instant::now(),
            app: None,
            other_app: None,
            app_closed: false,
            media: Media::default(),
            volume: None,
            playback_failed: false,
        };
        session.send(NS_CONNECTION, PLATFORM, &json!({ "type": "CONNECT" }))?;
        session.receiver_status()?;
        Ok(session)
    }

    fn send(
        &mut self,
        namespace: &str,
        destination: &str,
        payload: &Value,
    ) -> Result<(), CastError> {
        let message = Message::new(SENDER, destination, namespace, payload);
        Ok(self.channel.send(&message)?)
    }

    /// Sends `payload` with a new request id and waits for the answer that carries it.
    fn request(
        &mut self,
        namespace: &str,
        destination: &str,
        mut payload: Value,
        what: &'static str,
        timeout: Duration,
    ) -> Result<Value, CastError> {
        let id = self.next_request;
        self.next_request += 1;
        payload["requestId"] = id.into();
        self.send(namespace, destination, &payload)?;
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(CastError::Timeout(what));
            }
            for message in self.channel.receive(left)? {
                let reply = self.handle(&message);
                if reply.get("requestId").and_then(Value::as_u64) == Some(id) {
                    return Ok(reply);
                }
            }
            self.heartbeat()?;
        }
    }

    /// Takes in a message the device sent; returns its payload.
    fn handle(&mut self, message: &Message) -> Value {
        self.last_heard = Instant::now();
        let payload = message.json();
        let kind = payload.get("type").and_then(Value::as_str).unwrap_or("");
        match (message.namespace.as_str(), kind) {
            (NS_HEARTBEAT, "PING") => {
                let _ = self.send(NS_HEARTBEAT, &message.source, &json!({ "type": "PONG" }));
            }
            (NS_CONNECTION, "CLOSE") => {
                if self
                    .app
                    .as_ref()
                    .is_some_and(|a| a.transport == message.source)
                {
                    self.app = None;
                    self.app_closed = true;
                }
            }
            (NS_RECEIVER, "RECEIVER_STATUS") => self.receiver_update(&payload["status"]),
            (NS_MEDIA, "MEDIA_STATUS") => self.media_update(&payload["status"]),
            _ => {}
        }
        payload
    }

    fn heartbeat(&mut self) -> Result<(), CastError> {
        if self.last_heard.elapsed() > SILENCE_LIMIT {
            return Err(CastError::Timeout("the heartbeat"));
        }
        if self.last_ping.elapsed() >= PING_EVERY {
            self.last_ping = Instant::now();
            self.send(NS_HEARTBEAT, PLATFORM, &json!({ "type": "PING" }))?;
        }
        Ok(())
    }

    fn receiver_update(&mut self, status: &Value) {
        if let Some(level) = status["volume"]["level"].as_f64() {
            self.volume = Some((level.clamp(0.0, 1.0) * 100.0).round() as u8);
        }
        let apps = status["applications"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let ours = apps
            .iter()
            .find(|a| a["appId"].as_str() == Some(MEDIA_RECEIVER));
        self.other_app = apps
            .iter()
            .find(|a| a["appId"].as_str() != Some(MEDIA_RECEIVER))
            .and_then(|a| a["displayName"].as_str())
            .map(str::to_string);
        match ours {
            Some(app) => {
                let transport = app["transportId"].as_str().unwrap_or("").to_string();
                let session = app["sessionId"].as_str().unwrap_or("").to_string();
                let same = self.app.as_ref().filter(|a| a.session == session);
                self.app = Some(App {
                    launched: same.is_some_and(|a| a.launched),
                    connected: same.is_some_and(|a| a.connected && a.transport == transport),
                    transport,
                    session,
                });
            }
            None => {
                if self.app.take().is_some() {
                    self.app_closed = true;
                }
            }
        }
    }

    fn media_update(&mut self, status: &Value) {
        let Some(item) = status.as_array().and_then(|s| s.first()) else {
            // No media session: what played has ended.
            self.media.session = None;
            self.media.state = Some(TransportState::Stopped);
            return;
        };
        self.media.session = item["mediaSessionId"].as_i64();
        self.media.state = Some(match item["playerState"].as_str() {
            Some("PLAYING") => TransportState::Playing,
            Some("PAUSED") => TransportState::Paused,
            Some("BUFFERING" | "LOADING") => TransportState::Transitioning,
            _ => TransportState::Stopped,
        });
        if item["playerState"] == "IDLE" && item["idleReason"] == "ERROR" {
            self.playback_failed = true;
        }
        if let Some(t) = item["currentTime"].as_f64() {
            self.media.position = Some(t.max(0.0));
        }
        if let Some(id) = item["media"]["contentId"].as_str() {
            self.media.content_id = id.to_string();
        }
        if let Some(d) = item["media"]["duration"].as_f64() {
            self.media.duration = Some(d);
        }
    }

    fn receiver_status(&mut self) -> Result<(), CastError> {
        let get = json!({ "type": "GET_STATUS" });
        self.request(NS_RECEIVER, PLATFORM, get, "GET_STATUS", REPLY_TIMEOUT)?;
        Ok(())
    }

    /// Starts (or joins) the media receiver and connects to it. Returns its transport id.
    fn ensure_app(&mut self) -> Result<String, CastError> {
        if self.app.is_none() {
            let launch = json!({ "type": "LAUNCH", "appId": MEDIA_RECEIVER });
            self.request(NS_RECEIVER, PLATFORM, launch, "LAUNCH", LAUNCH_TIMEOUT)?;
            self.app_closed = false;
            if let Some(app) = self.app.as_mut() {
                app.launched = true;
            }
        }
        let Some(app) = self.app.as_mut() else {
            return Err(CastError::Refused("to start its media player".into()));
        };
        let transport = app.transport.clone();
        if !app.connected {
            app.connected = true;
            self.send(NS_CONNECTION, &transport, &json!({ "type": "CONNECT" }))?;
        }
        Ok(transport)
    }

    fn load(
        &mut self,
        url: &str,
        mime: &str,
        song: &SongInfo,
        start: Duration,
        play: bool,
    ) -> Result<(), CastError> {
        let transport = self.ensure_app()?;
        let session = self.app.as_ref().map(|a| a.session.clone());
        let mut metadata = Map::new();
        // MusicTrackMediaMetadata
        metadata.insert("metadataType".into(), 3.into());
        metadata.insert("title".into(), song.title.clone().into());
        if let Some(artist) = &song.artist {
            metadata.insert("artist".into(), artist.clone().into());
        }
        if let Some(album) = &song.album {
            metadata.insert("albumName".into(), album.clone().into());
        }
        if let Some(art) = &song.art {
            metadata.insert("images".into(), json!([{ "url": art }]));
        }
        let mut media = json!({
            "contentId": url,
            "contentType": mime,
            "streamType": "BUFFERED",
            "metadata": metadata,
        });
        if let Some(d) = song.duration {
            media["duration"] = seconds(d).into();
        }
        let load = json!({
            "type": "LOAD",
            "sessionId": session,
            "media": media,
            "autoplay": play,
            "currentTime": seconds(start),
        });
        self.playback_failed = false;
        self.media = Media::default();
        let reply = self.request(NS_MEDIA, &transport, load, "LOAD", LOAD_TIMEOUT)?;
        match reply["type"].as_str() {
            Some("MEDIA_STATUS") => Ok(()),
            other => {
                let reason = reply["reason"]
                    .as_str()
                    .or(reply["detailedErrorCode"].as_str())
                    .or(other)
                    .unwrap_or("no reason given");
                Err(CastError::LoadFailed(reason.to_string()))
            }
        }
    }

    /// PLAY, PAUSE, STOP or SEEK on what is loaded (nothing to do if nothing is).
    fn media_command(&mut self, kind: &'static str, mut extra: Value) -> Result<(), CastError> {
        let (Some(app), Some(session)) = (self.app.clone(), self.media.session) else {
            return Ok(());
        };
        extra["type"] = kind.into();
        extra["mediaSessionId"] = session.into();
        let reply = self.request(NS_MEDIA, &app.transport, extra, kind, REPLY_TIMEOUT)?;
        match reply["type"].as_str() {
            Some("MEDIA_STATUS") => Ok(()),
            _ => Err(CastError::Refused(kind.to_string())),
        }
    }

    fn set_volume(&mut self, volume: u8) -> Result<(), CastError> {
        let level = f64::from(volume.min(100)) / 100.0;
        let set = json!({ "type": "SET_VOLUME", "volume": { "level": level } });
        self.request(NS_RECEIVER, PLATFORM, set, "SET_VOLUME", REPLY_TIMEOUT)?;
        self.volume = Some(volume.min(100));
        Ok(())
    }

    /// Asks the media player what it is doing (or just listens, while it is not running).
    fn media_status(&mut self) -> Result<(), CastError> {
        match self.app.clone().filter(|a| a.connected) {
            Some(app) => {
                let get = json!({ "type": "GET_STATUS" });
                self.request(NS_MEDIA, &app.transport, get, "GET_STATUS", REPLY_TIMEOUT)?;
            }
            None => {
                for message in self.channel.receive(Duration::ZERO)? {
                    self.handle(&message);
                }
            }
        }
        self.heartbeat()
    }

    fn status(&self, asked: Instant, answered: Instant, done: u64) -> Status {
        let (state, track_uri) = match &self.app {
            Some(_) => (
                self.media.state.unwrap_or(TransportState::Stopped),
                self.media.content_id.clone(),
            ),
            // Something else is on the screen, or our player was closed.
            None => {
                let other = self.other_app.clone();
                let uri = match other {
                    Some(name) => format!("app:{name}"),
                    None if self.app_closed => "app:closed".into(),
                    None => String::new(),
                };
                (TransportState::Stopped, uri)
            }
        };
        let ours = self.app.is_some();
        Status {
            state,
            track_uri,
            position: self
                .media
                .position
                .filter(|_| ours)
                .map(Duration::from_secs_f64),
            precision: Duration::ZERO,
            duration: self.media.duration.map(Duration::from_secs_f64),
            volume: self.volume,
            asked,
            answered,
            done,
        }
    }

    /// Closes the media player if we started it, and the connection.
    fn close(&mut self) {
        if let Some(app) = self.app.clone().filter(|a| a.launched) {
            let stop = json!({ "type": "STOP", "sessionId": app.session });
            let _ = self.request(NS_RECEIVER, PLATFORM, stop, "STOP", Duration::from_secs(2));
        }
        let _ = self.send(NS_CONNECTION, PLATFORM, &json!({ "type": "CLOSE" }));
    }
}

fn execute(session: &mut Session, command: Command) -> Result<(), CastError> {
    match command {
        Command::Load {
            url,
            mime,
            song,
            start,
            play,
        } => session.load(&url, &mime, &song, start, play),
        Command::Play => session.media_command("PLAY", json!({})),
        Command::Pause => session.media_command("PAUSE", json!({})),
        Command::Stop => session.media_command("STOP", json!({})),
        Command::Seek(to) => session.media_command("SEEK", json!({ "currentTime": seconds(to) })),
        Command::SetVolume(volume) => session.set_volume(volume),
    }
}

fn connected<'a>(
    session: &'a mut Option<Session>,
    device: &CastDevice,
) -> Result<&'a mut Session, CastError> {
    let open = match session.take() {
        Some(open) => open,
        None => Session::open(device.addr)?,
    };
    Ok(session.insert(open))
}

fn run(device: &CastDevice, timing: Timing, commands: &Receiver<Command>, events: &Sender<Event>) {
    let mut session: Option<Session> = None;
    let mut done = 0u64;
    let mut next_poll = Instant::now();
    let mut next_receiver = Instant::now();
    let mut reachability = Reachability::default();
    loop {
        match commands.recv_timeout(next_poll.saturating_duration_since(Instant::now())) {
            Ok(command) => {
                let what = format!("{command:?}");
                let load = matches!(command, Command::Load { .. });
                let result = connected(&mut session, device).and_then(|s| execute(s, command));
                if let Err(e) = result {
                    log::warn!("Cast {}: {what}: {e}", device.name);
                    if e.is_fatal() {
                        session = None;
                    }
                    let _ = events.send(Event::Failed {
                        message: e.to_string(),
                        code: None,
                        load,
                    });
                }
                done += 1;
                next_poll = next_poll.min(Instant::now() + timing.after_command);
                continue;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                if let Some(s) = session.as_mut() {
                    s.close();
                }
                return;
            }
        }

        let polled = connected(&mut session, device).and_then(|s| {
            if Instant::now() >= next_receiver {
                s.receiver_status()?;
                next_receiver = Instant::now() + timing.volume_every;
            }
            let asked = Instant::now();
            s.media_status()?;
            Ok(asked)
        });
        let mut playing = false;
        match (polled, session.as_mut()) {
            (Ok(asked), Some(s)) => {
                reachability.answered();
                if std::mem::take(&mut s.playback_failed) {
                    let _ = events.send(Event::Failed {
                        message: "the device cannot play this song".into(),
                        code: None,
                        load: true,
                    });
                }
                let status = s.status(asked, Instant::now(), done);
                playing = status.state == TransportState::Playing;
                // Nobody may be listening any more: the link is closing, and the commands
                // still queued (a last Stop) are carried out before the worker ends.
                let _ = events.send(Event::Status(status));
            }
            (Err(e), _) => {
                session = None;
                if reachability.failed(timing.unreachable_after) {
                    log::warn!("Cast {} does not answer: {e}", device.name);
                    let _ = events.send(Event::Unreachable(e.to_string()));
                }
            }
            (Ok(_), None) => {}
        }
        let every = if playing {
            timing.poll_playing
        } else {
            timing.poll_idle
        };
        next_poll = Instant::now() + every;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outputs::link::tests::{next_failure, next_status, quick};
    use fake::FakeCast;

    fn load(url: &str, mime: &str, start: Duration, play: bool) -> Command {
        Command::Load {
            url: url.into(),
            mime: mime.into(),
            song: SongInfo {
                title: "So What".into(),
                artist: Some("Miles Davis".into()),
                album: Some("Kind of Blue".into()),
                duration: Some(Duration::from_secs(562)),
                art: Some("http://x/art.jpg".into()),
            },
            start,
            play,
        }
    }

    #[test]
    fn plays_through_the_media_receiver() {
        let fake = FakeCast::start("Televisio");
        fake.state().other_app = Some("Netflix".into());
        let mut link = start(fake.device(), quick());
        // Before anything is loaded, the status says what else is on the screen.
        let s = next_status(&link, |_| true);
        assert_eq!(s.track_uri, "app:Netflix");
        assert_eq!(s.volume, Some(30));

        let sent = link.send(load(
            "x-test:1.flac",
            "audio/flac",
            Duration::from_millis(12_500),
            true,
        ));
        let s = next_status(&link, |s| s.done == sent);
        assert_eq!(s.state, TransportState::Playing);
        assert_eq!(s.track_uri, "x-test:1.flac");
        assert_eq!(s.position, Some(Duration::from_millis(12_500)));
        assert_eq!(s.precision, Duration::ZERO);
        assert_eq!(s.duration, Some(Duration::from_secs(10)));
        {
            let st = fake.state();
            assert!(st.app && st.other_app.is_none());
            assert_eq!(st.content_type, "audio/flac");
            assert_eq!(st.metadata["title"], "So What");
            assert_eq!(st.metadata["artist"], "Miles Davis");
            assert_eq!(st.metadata["albumName"], "Kind of Blue");
            assert_eq!(st.metadata["images"][0]["url"], "http://x/art.jpg");
            assert_eq!(st.count("receiver LAUNCH"), 1);
            assert_eq!(
                st.count("connection CONNECT"),
                2,
                "to the device and to the player"
            );
        }

        link.send(Command::Pause);
        let sent = link.send(Command::Seek(Duration::from_secs(30)));
        let s = next_status(&link, |s| s.done == sent);
        assert_eq!(s.state, TransportState::Paused);
        assert_eq!(s.position, Some(Duration::from_secs(30)));
        let sent = link.send(Command::SetVolume(45));
        next_status(&link, |s| s.done == sent && s.volume == Some(45));
        assert!((fake.state().volume - 0.45).abs() < 1e-9);

        // Closing stops the song and the player app we started.
        link.send(Command::Stop);
        link.close(Duration::from_secs(3));
        let st = fake.state();
        assert_eq!(st.player_state, "IDLE");
        assert!(!st.app);
        assert_eq!(st.count("connection CLOSE"), 1);
    }

    #[test]
    fn songs_the_device_cannot_play_are_reported() {
        let fake = FakeCast::start("Televisio");
        fake.state().refuse.push("audio/ogg".into());
        let mut link = start(fake.device(), quick());
        link.send(load("x-test:2.ogg", "audio/ogg", Duration::ZERO, true));
        let (message, code, load_failed) = next_failure(&link);
        assert!(message.contains("cannot play"), "{message}");
        assert_eq!((code, load_failed), (None, true));

        // It loads, then fails while playing.
        let sent = link.send(load("x-test:3.flac", "audio/flac", Duration::ZERO, true));
        next_status(&link, |s| {
            s.done == sent && s.state == TransportState::Playing
        });
        {
            let mut st = fake.state();
            st.player_state = "IDLE".into();
            st.idle_reason = Some("ERROR".into());
        }
        let (_, _, load_failed) = next_failure(&link);
        assert!(load_failed);
    }

    #[test]
    fn another_app_on_the_screen_is_reported() {
        let fake = FakeCast::start("Televisio");
        let mut link = start(fake.device(), quick());
        let sent = link.send(load("x-test:4.mp3", "audio/mpeg", Duration::ZERO, true));
        next_status(&link, |s| s.done == sent && s.track_uri == "x-test:4.mp3");
        {
            let mut st = fake.state();
            st.app = false;
            st.other_app = Some("YouTube".into());
        }
        next_status(&link, |s| s.track_uri == "app:YouTube");
    }

    /// Talks to a real device without changing anything on it: it connects and asks what is
    /// on the screen. `CAST_DEVICE=192.168.1.201:8009 cargo test --lib real_cast_device --
    /// --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_cast_device_answers() {
        let Ok(addr) = std::env::var("CAST_DEVICE") else {
            return;
        };
        let mut session = Session::open(addr.parse().unwrap()).unwrap();
        println!(
            "volume {:?}, media player {:?}, other app {:?}",
            session.volume, session.app, session.other_app
        );
        let started = Instant::now();
        session.media_status().unwrap();
        println!("listened {:?}", started.elapsed());
        session.close();
    }

    #[test]
    fn a_device_that_does_not_answer_is_reported() {
        let addr = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let device = CastDevice {
            name: "Gone".into(),
            model: String::new(),
            id: "gone".into(),
            addr,
        };
        let link = start(device, quick());
        match link.events().recv_timeout(Duration::from_secs(10)).unwrap() {
            Event::Unreachable(_) => {}
            other => panic!("{other:?}"),
        }
    }
}
