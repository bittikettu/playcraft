//! Playing on Sonos speakers. Speakers are found by SSDP (`discovery`) and controlled over
//! UPnP (`control`, `link`); they fetch each song from a small media server in the player
//! (`media`). The local playback goes on muted, driving the visualizers and the play order,
//! and is kept in step with the speaker (`sync`).

pub mod control;
pub mod discovery;
#[cfg(test)]
pub(crate) mod fake;
pub mod http;
pub mod link;
pub mod media;
pub mod sync;
pub mod wav;

use std::path::Path;
use std::time::Duration;

pub use discovery::Zone;
use http::xml_escape;

/// Tags shown on the speaker (and in the Sonos app) for a song.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SongInfo {
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration: Option<Duration>,
}

/// DIDL-Lite metadata for `url`, the file at `path`.
pub fn didl(url: &str, path: &Path, song: &SongInfo) -> String {
    let duration = song
        .duration
        .map(|d| format!(" duration=\"{}\"", control::format_time(d)))
        .unwrap_or_default();
    let mut item = format!(
        "<res protocolInfo=\"http-get:*:{}:*\"{duration}>{}</res>\
         <dc:title>{}</dc:title>\
         <upnp:class>object.item.audioItem.musicTrack</upnp:class>",
        media::mime_type(path),
        xml_escape(url),
        xml_escape(&song.title),
    );
    if let Some(artist) = &song.artist {
        item.push_str(&format!("<dc:creator>{}</dc:creator>", xml_escape(artist)));
    }
    if let Some(album) = &song.album {
        item.push_str(&format!("<upnp:album>{}</upnp:album>", xml_escape(album)));
    }
    format!(
        "<DIDL-Lite xmlns:dc=\"http://purl.org/dc/elements/1.1/\" \
         xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\" \
         xmlns:r=\"urn:schemas-rinconnetworks-com:metadata-1-0/\" \
         xmlns=\"urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/\">\
         <item id=\"-1\" parentID=\"-1\" restricted=\"true\">{item}</item></DIDL-Lite>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_songs_in_didl_lite() {
        let song = SongInfo {
            title: "Rock & Roll".into(),
            artist: Some("Led Zeppelin".into()),
            album: None,
            duration: Some(Duration::from_secs(220)),
        };
        let xml = didl("http://10.0.0.2:5000/s/1.flac", Path::new("x.flac"), &song);
        let doc = roxmltree::Document::parse(&xml).unwrap();
        let text = |name: &str| {
            doc.descendants()
                .find(|n| n.tag_name().name() == name)
                .and_then(|n| n.text())
        };
        assert_eq!(text("title"), Some("Rock & Roll"));
        assert_eq!(text("creator"), Some("Led Zeppelin"));
        assert_eq!(text("album"), None);
        let res = doc.descendants().find(|n| n.has_tag_name("res")).unwrap();
        assert_eq!(res.text(), Some("http://10.0.0.2:5000/s/1.flac"));
        assert_eq!(
            res.attribute("protocolInfo"),
            Some("http-get:*:audio/flac:*")
        );
        assert_eq!(res.attribute("duration"), Some("0:03:40"));
    }
}
