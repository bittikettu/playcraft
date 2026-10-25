//! A background connection to one output device: a Sonos group's coordinator or a Google
//! Cast device. It carries out commands in order and reports what the device is doing about
//! twice a second, so the UI thread never waits on the network.

use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};

/// How often a link asks its device what it is doing.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    pub poll_playing: Duration,
    pub poll_idle: Duration,
    /// After a command, the next report comes this soon.
    pub after_command: Duration,
    pub volume_every: Duration,
    /// No answers for this long make the device unreachable.
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

/// What a device's player is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportState {
    Playing,
    Paused,
    Stopped,
    /// Buffering, or switching tracks.
    Transitioning,
}

impl TransportState {
    pub fn label(self) -> &'static str {
        match self {
            TransportState::Playing => "playing",
            TransportState::Paused => "paused",
            TransportState::Stopped => "stopped",
            TransportState::Transitioning => "buffering",
        }
    }
}

/// What a device shows about a song.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SongInfo {
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub year: Option<u32>,
    pub duration: Option<Duration>,
    /// URL of the cover art.
    pub art: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Loads `url` (a `mime` file) and goes to `start`; plays it when `play` is set. A
    /// `live` stream (the TV stream) has no start or end.
    Load {
        url: String,
        mime: String,
        song: SongInfo,
        start: Duration,
        play: bool,
        live: bool,
    },
    Play,
    Pause,
    Stop,
    Seek(Duration),
    /// Volume, 0–100 (a Sonos group's, or a Cast device's).
    SetVolume(u8),
}

/// What the device is doing.
#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub state: TransportState,
    /// What it plays: our song's URL, or something else.
    pub track_uri: String,
    pub position: Option<Duration>,
    /// How coarse `position` is: Sonos reports whole seconds, Cast exact times.
    pub precision: Duration,
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
    /// A command failed: a message for the user, the device's error code if it gave one,
    /// and whether it was a `Load` (or the device could not play what it loaded).
    Failed {
        message: String,
        code: Option<u32>,
        load: bool,
    },
    /// The device stopped answering.
    Unreachable(String),
}

pub struct Link {
    commands: Option<Sender<Command>>,
    events: Receiver<Event>,
    sent: u64,
    thread: Option<JoinHandle<()>>,
}

impl Link {
    /// Runs `worker` on a thread of its own. The worker carries out the commands it receives
    /// and returns once the command channel is closed and empty.
    pub fn spawn(
        name: &str,
        worker: impl FnOnce(Receiver<Command>, Sender<Event>) + Send + 'static,
    ) -> Link {
        let (commands, received) = unbounded();
        let (events_tx, events) = unbounded();
        let thread = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                // A bug in a worker ends the cast with a message instead of a dead link.
                let report = events_tx.clone();
                let work = std::panic::AssertUnwindSafe(move || worker(received, events_tx));
                if std::panic::catch_unwind(work).is_err() {
                    let _ = report.send(Event::Unreachable(
                        "the connection failed (see the message log)".into(),
                    ));
                }
            })
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

/// Reports a device that keeps failing to answer, once it has failed for long enough.
#[derive(Debug, Default)]
pub struct Reachability {
    failing_since: Option<Instant>,
    reported: bool,
}

impl Reachability {
    pub fn answered(&mut self) {
        self.failing_since = None;
        self.reported = false;
    }

    /// Records a failure; true when it is time to report the device unreachable.
    pub fn failed(&mut self, after: Duration) -> bool {
        let since = *self.failing_since.get_or_insert_with(Instant::now);
        if !self.reported && since.elapsed() >= after {
            self.reported = true;
            return true;
        }
        false
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn quick() -> Timing {
        Timing {
            poll_playing: Duration::from_millis(20),
            poll_idle: Duration::from_millis(20),
            after_command: Duration::from_millis(5),
            volume_every: Duration::from_millis(20),
            unreachable_after: Duration::from_millis(200),
        }
    }

    /// Waits for a status that passes `check`.
    pub(crate) fn next_status(link: &Link, check: impl Fn(&Status) -> bool) -> Status {
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

    /// Waits for a failure report.
    pub(crate) fn next_failure(link: &Link) -> (String, Option<u32>, bool) {
        loop {
            match link.events().recv_timeout(Duration::from_secs(5)).unwrap() {
                Event::Failed {
                    message,
                    code,
                    load,
                } => return (message, code, load),
                Event::Status(_) => {}
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn a_worker_that_panics_reports_the_device_unreachable() {
        let link = Link::spawn("test-link", |_, _| panic!("a bug"));
        match link.events().recv_timeout(Duration::from_secs(5)).unwrap() {
            Event::Unreachable(msg) => assert!(msg.contains("failed"), "{msg}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn reachability_reports_once() {
        let mut r = Reachability::default();
        assert!(!r.failed(Duration::from_millis(30)));
        std::thread::sleep(Duration::from_millis(40));
        assert!(r.failed(Duration::from_millis(30)));
        assert!(!r.failed(Duration::from_millis(30)), "only once");
        r.answered();
        assert!(r.failed(Duration::ZERO), "reported again after an answer");
    }
}
