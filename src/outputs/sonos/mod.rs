//! Sonos speakers, found by SSDP (`discovery`) and controlled over UPnP (`control`). A
//! group's coordinator is driven through a link that polls its state.

pub mod control;
pub mod discovery;
#[cfg(test)]
pub(crate) mod fake;

use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};

use control::SonosError;
pub use control::Speaker;
pub use discovery::Zone;

use super::http::xml_escape;
use super::link::{Command, Event, Link, Reachability, SongInfo, Status, Timing, TransportState};

/// Seeks shorter than this from the start of a track are not worth a command.
const MIN_SEEK: Duration = Duration::from_secs(1);
/// Sonos reports positions in whole seconds.
const PRECISION: Duration = Duration::from_secs(1);

/// DIDL-Lite metadata for `url`, a `mime` file.
pub fn didl(url: &str, mime: &str, song: &SongInfo) -> String {
    let duration = song
        .duration
        .map(|d| format!(" duration=\"{}\"", control::format_time(d)))
        .unwrap_or_default();
    let mut item = format!(
        "<res protocolInfo=\"http-get:*:{mime}:*\"{duration}>{}</res>\
         <dc:title>{}</dc:title>\
         <upnp:class>object.item.audioItem.musicTrack</upnp:class>",
        xml_escape(url),
        xml_escape(&song.title),
    );
    if let Some(artist) = &song.artist {
        item.push_str(&format!("<dc:creator>{}</dc:creator>", xml_escape(artist)));
    }
    if let Some(album) = &song.album {
        item.push_str(&format!("<upnp:album>{}</upnp:album>", xml_escape(album)));
    }
    if let Some(art) = &song.art {
        item.push_str(&format!(
            "<upnp:albumArtURI>{}</upnp:albumArtURI>",
            xml_escape(art)
        ));
    }
    format!(
        "<DIDL-Lite xmlns:dc=\"http://purl.org/dc/elements/1.1/\" \
         xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\" \
         xmlns:r=\"urn:schemas-rinconnetworks-com:metadata-1-0/\" \
         xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\">\
         <item id=\"-1\" parentID=\"-1\" restricted=\"true\">{item}</item></DIDL-Lite>"
    )
}

/// Connects to the group led by `speaker`.
pub fn start(speaker: Speaker, timing: Timing) -> Link {
    Link::spawn("sonos-link", move |commands, events| {
        run(speaker, timing, &commands, &events)
    })
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
            mime,
            song,
            start,
            play,
        } => {
            *pending_seek = (start >= MIN_SEEK).then_some(start);
            speaker.set_uri(&url, &didl(&url, &mime, &song))?;
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
    let mut reachability = Reachability::default();
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
                reachability.answered();
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
                    precision: PRECISION,
                    duration: position.duration,
                    volume,
                    asked,
                    answered,
                    done,
                };
                // Nobody may be listening any more: the link is closing, and the commands
                // still queued (a last Stop) are carried out before the worker ends.
                let _ = events.send(Event::Status(status));
            }
            Err(e) => {
                if reachability.failed(timing.unreachable_after) {
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
    use crate::outputs::link::tests::{next_failure, next_status, quick};
    use fake::FakeSonos;

    fn load(url: &str, start: u64, play: bool) -> Command {
        Command::Load {
            url: url.into(),
            mime: "audio/flac".into(),
            song: SongInfo::default(),
            start: Duration::from_secs(start),
            play,
        }
    }

    #[test]
    fn describes_songs_in_didl_lite() {
        let song = SongInfo {
            title: "Rock & Roll".into(),
            artist: Some("Led Zeppelin".into()),
            album: None,
            year: None,
            duration: Some(Duration::from_secs(220)),
            art: Some("http://10.0.0.2:5000/s/2.jpg".into()),
        };
        let xml = didl("http://10.0.0.2:5000/s/1.flac", "audio/flac", &song);
        let doc = roxmltree::Document::parse(&xml).unwrap();
        let text = |name: &str| {
            doc.descendants()
                .find(|n| n.tag_name().name() == name)
                .and_then(|n| n.text())
        };
        assert_eq!(text("title"), Some("Rock & Roll"));
        assert_eq!(text("creator"), Some("Led Zeppelin"));
        assert_eq!(text("album"), None);
        assert_eq!(text("albumArtURI"), Some("http://10.0.0.2:5000/s/2.jpg"));
        let res = doc.descendants().find(|n| n.has_tag_name("res")).unwrap();
        assert_eq!(res.text(), Some("http://10.0.0.2:5000/s/1.flac"));
        assert_eq!(
            res.attribute("protocolInfo"),
            Some("http-get:*:audio/flac:*")
        );
        assert_eq!(res.attribute("duration"), Some("0:03:40"));
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
        let mut link = start(Speaker::new(fake.addr), quick());
        link.send(load("x-test:2.flac", 42, true));
        let sent = link.send(Command::SetVolume(40));
        let s = next_status(&link, |s| s.done == sent);
        assert_eq!(s.state, TransportState::Playing);
        assert_eq!(s.track_uri, "x-test:2.flac");
        assert_eq!(s.position, Some(Duration::from_secs(42)));
        assert_eq!(s.precision, PRECISION);
        assert_eq!(s.volume, Some(40));
        assert!(s.asked <= s.answered);
        assert_eq!(
            fake.state().actions[..4],
            ["SetAVTransportURI", "Play", "Seek", "SetGroupVolume"]
        );
        assert!(fake.state().metadata.contains("audio/flac"));

        // A failed command is reported, and the link goes on.
        fake.state().fail.insert("Pause".into(), 402);
        let sent = link.send(Command::Pause);
        let (message, code, load) = next_failure(&link);
        assert!(message.contains("Pause"), "{message}");
        assert_eq!((code, load), (Some(402), false));
        next_status(&link, |s| s.done == sent);

        // Closing lets queued commands finish.
        link.send(Command::Stop);
        link.close(Duration::from_secs(2));
        assert_eq!(fake.state().transport, "STOPPED");
    }

    #[test]
    fn closing_while_polling_still_stops_the_speaker() {
        for _ in 0..20 {
            let fake = FakeSonos::start("Kitchen");
            let mut link = start(Speaker::new(fake.addr), quick());
            let sent = link.send(load("x-test:5.flac", 0, true));
            next_status(&link, |s| s.done == sent);
            link.send(Command::Stop);
            link.close(Duration::ZERO);
            let deadline = Instant::now() + Duration::from_secs(5);
            while fake.state().transport != "STOPPED" {
                assert!(Instant::now() < deadline, "{:?}", fake.state().actions);
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    #[test]
    fn a_refused_seek_waits_for_the_track_to_start() {
        let fake = FakeSonos::start("Kitchen");
        fake.state().fail.insert("Seek".into(), 701);
        let mut link = start(Speaker::new(fake.addr), quick());
        let sent = link.send(load("x-test:3.ogg", 90, false));
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
        // Nothing listens on a port that was taken and given back.
        let addr = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let link = start(Speaker::new(addr), quick());
        match link.events().recv_timeout(Duration::from_secs(10)).unwrap() {
            Event::Unreachable(_) => {}
            other => panic!("{other:?}"),
        }
    }
}
