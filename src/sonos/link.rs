//! A background connection to one speaker group's coordinator. It carries out commands in
//! order and reports the speaker's state twice a second, so the UI thread never waits on the
//! network.

use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};

use super::control::{SonosError, Speaker, TransportState};

/// Seeks shorter than this from the start of a track are not worth a command.
const MIN_SEEK: Duration = Duration::from_secs(1);

/// How often the link asks the speaker what it is doing.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    pub poll_playing: Duration,
    pub poll_idle: Duration,
    /// After a command, the next report comes this soon.
    pub after_command: Duration,
    pub volume_every: Duration,
    /// No answers for this long make the speaker unreachable.
    pub unreachable_after: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            poll_playing: Duration::from_millis(500),
            poll_idle: Duration::from_secs(1),
            after_command: Duration::from_millis(150),
            volume_every: Duration::from_secs(2),
            unreachable_after: Duration::from_secs(6),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Loads `url` (described by DIDL-Lite `metadata`) and goes to `start`; plays it when
    /// `play` is set.
    Load {
        url: String,
        metadata: String,
        start: Duration,
        play: bool,
    },
    Play,
    Pause,
    Stop,
    Seek(Duration),
    /// Group volume, 0–100.
    SetVolume(u8),
}

/// What the speaker is doing.
#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub state: TransportState,
    pub track_uri: String,
    /// Whole seconds into the track.
    pub position: Option<Duration>,
    pub duration: Option<Duration>,
    pub volume: Option<u8>,
    /// The position was read between these two moments.
    pub asked: Instant,
    pub answered: Instant,
    /// How many commands had been carried out when the status was read.
    pub done: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Status(Status),
    /// A command failed: a message for the user, the UPnP error code if the speaker gave
    /// one, and whether it was a `Load`.
    Failed {
        message: String,
        code: Option<u32>,
        load: bool,
    },
    /// The speaker stopped answering.
    Unreachable(String),
}

pub struct Link {
    commands: Option<Sender<Command>>,
    events: Receiver<Event>,
    sent: u64,
    thread: Option<JoinHandle<()>>,
}

impl Link {
    pub fn start(speaker: Speaker, timing: Timing) -> Link {
        let (commands, received) = unbounded();
        let (events_tx, events) = unbounded();
        let thread = std::thread::Builder::new()
            .name("sonos-link".into())
            .spawn(move || run(speaker, timing, &received, &events_tx))
            .ok();
        Link {
            commands: Some(commands),
            events,
            sent: 0,
            thread,
        }
    }

    /// Queues a command. Returns how many have been sent, this one included.
    pub fn send(&mut self, command: Command) -> u64 {
        if let Some(commands) = &self.commands {
            let _ = commands.send(command);
        }
        self.sent += 1;
        self.sent
    }

    pub fn sent(&self) -> u64 {
        self.sent
    }

    pub fn events(&self) -> &Receiver<Event> {
        &self.events
    }

    /// Lets the queued commands finish, waiting at most `wait` for them, and ends the link.
    pub fn close(mut self, wait: Duration) {
        self.commands = None;
        let Some(thread) = self.thread.take() else {
            return;
        };
        let deadline = Instant::now() + wait;
        while !thread.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Carries out one command. `pending_seek` holds a seek the speaker refused because it had
/// not started the track yet; it is retried once the track plays.
fn execute(
    speaker: &Speaker,
    command: Command,
    pending_seek: &mut Option<Duration>,
) -> Result<(), SonosError> {
    // 701: "not possible now", e.g. pausing what is still buffering.
    let unless_701 = |r: Result<(), SonosError>| match r {
        Err(e) if e.upnp_code() == Some(701) => Ok(()),
        other => other,
    };
    match command {
        Command::Load {
            url,
            metadata,
            start,
            play,
        } => {
            *pending_seek = (start >= MIN_SEEK).then_some(start);
            speaker.set_uri(&url, &metadata)?;
            if play {
                speaker.play()?;
            }
            if let Some(to) = *pending_seek
                && speaker.seek(to).is_ok()
            {
                *pending_seek = None;
            }
            Ok(())
        }
        Command::Play => {
            speaker.play()?;
            if let Some(to) = *pending_seek
                && speaker.seek(to).is_ok()
            {
                *pending_seek = None;
            }
            Ok(())
        }
        Command::Pause => unless_701(speaker.pause()),
        Command::Stop => {
            *pending_seek = None;
            unless_701(speaker.stop())
        }
        Command::Seek(to) => match speaker.seek(to) {
            Err(e) if e.upnp_code() == Some(701) => {
                *pending_seek = Some(to);
                Ok(())
            }
            other => other,
        },
        Command::SetVolume(volume) => speaker.set_group_volume(volume),
    }
}

fn run(speaker: Speaker, timing: Timing, commands: &Receiver<Command>, events: &Sender<Event>) {
    let mut done = 0u64;
    let mut next_poll = Instant::now();
    let mut next_volume = Instant::now();
    let mut volume = None;
    let mut pending_seek = None;
    let mut failing_since: Option<Instant> = None;
    let mut unreachable = false;
    loop {
        match commands.recv_timeout(next_poll.saturating_duration_since(Instant::now())) {
            Ok(command) => {
                if let Command::SetVolume(v) = command {
                    volume = Some(v);
                }
                let what = format!("{command:?}");
                let load = matches!(command, Command::Load { .. });
                if let Err(e) = execute(&speaker, command, &mut pending_seek) {
                    log::warn!("Sonos {}: {what}: {e}", speaker.addr);
                    let _ = events.send(Event::Failed {
                        message: e.to_string(),
                        code: e.upnp_code(),
                        load,
                    });
                }
                done += 1;
                next_poll = next_poll.min(Instant::now() + timing.after_command);
                continue;
            }
            Err(RecvTimeoutError::Timeout) => {}
            // Every command has been carried out and the link is closed.
            Err(RecvTimeoutError::Disconnected) => return,
        }

        let read = speaker.transport_state().and_then(|state| {
            let asked = Instant::now();
            let position = speaker.position()?;
            Ok((state, position, asked, Instant::now()))
        });
        let mut playing = false;
        match read {
            Ok((state, position, asked, answered)) => {
                failing_since = None;
                unreachable = false;
                playing = state == TransportState::Playing;
                if Instant::now() >= next_volume {
                    volume = speaker.group_volume().ok().or(volume);
                    next_volume = Instant::now() + timing.volume_every;
                }
                if let Some(to) = pending_seek
                    && matches!(state, TransportState::Playing | TransportState::Paused)
                {
                    pending_seek = None;
                    if let Err(e) = speaker.seek(to) {
                        log::warn!("Sonos {}: seek: {e}", speaker.addr);
                    }
                }
                let status = Status {
                    state,
                    track_uri: position.track_uri,
                    position: position.position,
                    duration: position.duration,
                    volume,
                    asked,
                    answered,
                    done,
                };
                if events.send(Event::Status(status)).is_err() {
                    return;
                }
            }
            Err(e) => {
                let since = *failing_since.get_or_insert_with(Instant::now);
                if !unreachable && since.elapsed() >= timing.unreachable_after {
                    unreachable = true;
                    log::warn!("Sonos {} does not answer: {e}", speaker.addr);
                    let _ = events.send(Event::Unreachable(e.to_string()));
                }
            }
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
    use crate::sonos::fake::FakeSonos;

    fn quick() -> Timing {
        Timing {
            poll_playing: Duration::from_millis(20),
            poll_idle: Duration::from_millis(20),
            after_command: Duration::from_millis(5),
            volume_every: Duration::from_millis(20),
            unreachable_after: Duration::from_millis(200),
        }
    }

    fn next_status(link: &Link, check: impl Fn(&Status) -> bool) -> Status {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match link.events().recv_timeout(left) {
                Ok(Event::Status(s)) if check(&s) => return s,
                Ok(_) => {}
                Err(_) => panic!("no matching status"),
            }
        }
    }

    #[test]
    fn speaker_calls_and_errors() {
        let fake = FakeSonos::start("Kitchen");
        let speaker = Speaker::new(fake.addr);
        speaker.set_uri("x-test:1.mp3", "<DIDL-Lite/>").unwrap();
        speaker.play().unwrap();
        speaker.seek(Duration::from_secs(75)).unwrap();
        assert_eq!(speaker.transport_state().unwrap(), TransportState::Playing);
        let pos = speaker.position().unwrap();
        assert_eq!(pos.track_uri, "x-test:1.mp3");
        assert_eq!(pos.position, Some(Duration::from_secs(75)));
        assert_eq!(pos.duration, None);
        assert_eq!(fake.state().metadata, "<DIDL-Lite/>");
        speaker.set_group_volume(130).unwrap();
        assert_eq!(speaker.group_volume().unwrap(), 100);

        fake.state().fail.insert("Play".into(), 714);
        let e = speaker.play().unwrap_err();
        assert_eq!(e.upnp_code(), Some(714));
        assert!(
            e.to_string().contains("does not play this file type"),
            "{e}"
        );
    }

    #[test]
    fn carries_out_commands_in_order_and_reports() {
        let fake = FakeSonos::start("Kitchen");
        let mut link = Link::start(Speaker::new(fake.addr), quick());
        link.send(Command::Load {
            url: "x-test:2.flac".into(),
            metadata: String::new(),
            start: Duration::from_secs(42),
            play: true,
        });
        let sent = link.send(Command::SetVolume(40));
        let s = next_status(&link, |s| s.done == sent);
        assert_eq!(s.state, TransportState::Playing);
        assert_eq!(s.track_uri, "x-test:2.flac");
        assert_eq!(s.position, Some(Duration::from_secs(42)));
        assert_eq!(s.volume, Some(40));
        assert!(s.asked <= s.answered);
        assert_eq!(
            fake.state().actions[..4],
            ["SetAVTransportURI", "Play", "Seek", "SetGroupVolume"]
        );

        // A failed command is reported, and the link goes on.
        fake.state().fail.insert("Pause".into(), 402);
        let sent = link.send(Command::Pause);
        loop {
            match link.events().recv_timeout(Duration::from_secs(5)).unwrap() {
                Event::Failed {
                    message,
                    code,
                    load,
                } => {
                    assert!(message.contains("Pause"), "{message}");
                    assert_eq!((code, load), (Some(402), false));
                    break;
                }
                Event::Status(_) => {}
                other => panic!("{other:?}"),
            }
        }
        next_status(&link, |s| s.done == sent);

        // Closing lets queued commands finish.
        link.send(Command::Stop);
        link.close(Duration::from_secs(2));
        assert_eq!(fake.state().transport, "STOPPED");
    }

    #[test]
    fn a_refused_seek_waits_for_the_track_to_start() {
        let fake = FakeSonos::start("Kitchen");
        fake.state().fail.insert("Seek".into(), 701);
        let mut link = Link::start(Speaker::new(fake.addr), quick());
        let sent = link.send(Command::Load {
            url: "x-test:3.ogg".into(),
            metadata: String::new(),
            start: Duration::from_secs(90),
            play: false,
        });
        let s = next_status(&link, |s| s.done == sent);
        assert_eq!(s.position, Some(Duration::ZERO));
        fake.state().fail.clear();
        let sent = link.send(Command::Play);
        let s = next_status(&link, |s| s.done == sent);
        assert_eq!(s.position, Some(Duration::from_secs(90)));
        assert_eq!(s.state, TransportState::Playing);
    }

    #[test]
    fn reports_a_speaker_that_does_not_answer() {
        // Nothing listens on the fake's port once it is taken and dropped.
        let addr = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let link = Link::start(Speaker::new(addr), quick());
        match link.events().recv_timeout(Duration::from_secs(10)).unwrap() {
            Event::Unreachable(_) => {}
            other => panic!("{other:?}"),
        }
    }
}
