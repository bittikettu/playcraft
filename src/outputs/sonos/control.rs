//! UPnP control of one Sonos speaker: SOAP calls to its AVTransport, GroupRenderingControl
//! and ZoneGroupTopology services on port 1400. Transport and group volume commands go to a
//! group's coordinator.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use crate::outputs::http::{self, xml_escape};
use crate::outputs::link::TransportState;

const AV_TRANSPORT: Service = Service {
    path: "/MediaRenderer/AVTransport/Control",
    urn: "urn:schemas-upnp-org:service:AVTransport:1",
};
const GROUP_RENDERING: Service = Service {
    path: "/MediaRenderer/GroupRenderingControl/Control",
    urn: "urn:schemas-upnp-org:service:GroupRenderingControl:1",
};
const ZONE_TOPOLOGY: Service = Service {
    path: "/ZoneGroupTopology/Control",
    urn: "urn:schemas-upnp-org:service:ZoneGroupTopology:1",
};

struct Service {
    path: &'static str,
    urn: &'static str,
}

#[derive(Debug, thiserror::Error)]
pub enum SonosError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{action} failed: {}", upnp_message(*.code))]
    Upnp { action: &'static str, code: u32 },
    #[error("{action}: HTTP status {status}")]
    Http { action: &'static str, status: u16 },
    #[error("{action}: unexpected answer ({detail})")]
    Answer {
        action: &'static str,
        detail: String,
    },
}

impl SonosError {
    pub fn upnp_code(&self) -> Option<u32> {
        match self {
            SonosError::Upnp { code, .. } => Some(*code),
            _ => None,
        }
    }
}

fn upnp_message(code: u32) -> String {
    let text = match code {
        402 => "invalid arguments",
        701 => "not possible in the current state",
        711 => "position out of range",
        714 => "the speaker does not play this file type",
        716 => "the song could not be found",
        800 => "the speaker is not the leader of its group",
        _ => "",
    };
    if text.is_empty() {
        format!("UPnP error {code}")
    } else {
        format!("{text} (UPnP error {code})")
    }
}

/// A speaker's `CurrentTransportState`.
fn parse_state(s: &str) -> TransportState {
    match s {
        "PLAYING" => TransportState::Playing,
        "PAUSED_PLAYBACK" => TransportState::Paused,
        "TRANSITIONING" => TransportState::Transitioning,
        _ => TransportState::Stopped,
    }
}

/// Where in its track a speaker is (`GetPositionInfo`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PositionInfo {
    pub track_uri: String,
    /// Whole seconds into the track.
    pub position: Option<Duration>,
    pub duration: Option<Duration>,
}

/// `H:MM:SS` (as UPnP wants it). Fractions are dropped.
pub fn format_time(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// Parses `H:MM:SS` or `H:MM:SS.fff`; `NOT_IMPLEMENTED` and empty values are `None`.
pub fn parse_time(s: &str) -> Option<Duration> {
    let mut secs = 0.0;
    let mut parts = 0;
    for part in s.trim().split(':') {
        let v: f64 = part.parse().ok()?;
        if !v.is_finite() || v < 0.0 {
            return None;
        }
        secs = secs * 60.0 + v;
        parts += 1;
    }
    (parts == 3).then(|| Duration::from_secs_f64(secs))
}

/// One speaker, addressed by its control endpoint (`ip:1400`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Speaker {
    pub addr: SocketAddr,
}

impl Speaker {
    pub fn new(addr: SocketAddr) -> Self {
        Speaker { addr }
    }

    /// Calls `action` and returns the fields of its response.
    fn call(
        &self,
        service: &Service,
        action: &'static str,
        args: &[(&str, &str)],
    ) -> Result<HashMap<String, String>, SonosError> {
        let mut body = String::from(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
             <s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
             s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body>",
        );
        body.push_str(&format!("<u:{action} xmlns:u=\"{}\">", service.urn));
        for (name, value) in args {
            body.push_str(&format!("<{name}>{}</{name}>", xml_escape(value)));
        }
        body.push_str(&format!("</u:{action}></s:Body></s:Envelope>"));
        let soap_action = format!("\"{}#{action}\"", service.urn);
        let response = http::request(
            self.addr,
            "POST",
            service.path,
            &[
                ("Content-Type", "text/xml; charset=\"utf-8\""),
                ("SOAPACTION", &soap_action),
            ],
            &body,
        )?;
        let answer = |detail: String| SonosError::Answer { action, detail };
        let doc = roxmltree::Document::parse(&response.body)
            .map_err(|e| answer(format!("not XML: {e}")))?;
        if response.status != 200 {
            let code = doc
                .descendants()
                .find(|n| n.has_tag_name("errorCode"))
                .and_then(|n| n.text())
                .and_then(|t| t.trim().parse().ok());
            return Err(match code {
                Some(code) => SonosError::Upnp { action, code },
                None => SonosError::Http {
                    action,
                    status: response.status,
                },
            });
        }
        let wanted = format!("{action}Response");
        let node = doc
            .descendants()
            .find(|n| n.tag_name().name() == wanted)
            .ok_or_else(|| answer(format!("no {wanted}")))?;
        Ok(node
            .children()
            .filter(|n| n.is_element())
            .map(|n| {
                let text = n.text().unwrap_or("").to_string();
                (n.tag_name().name().to_string(), text)
            })
            .collect())
    }

    fn transport(&self, action: &'static str, args: &[(&str, &str)]) -> Result<(), SonosError> {
        let mut all = vec![("InstanceID", "0")];
        all.extend_from_slice(args);
        self.call(&AV_TRANSPORT, action, &all).map(drop)
    }

    /// Loads `uri` (described by DIDL-Lite `metadata`) without starting it.
    pub fn set_uri(&self, uri: &str, metadata: &str) -> Result<(), SonosError> {
        self.transport(
            "SetAVTransportURI",
            &[("CurrentURI", uri), ("CurrentURIMetaData", metadata)],
        )
    }

    pub fn play(&self) -> Result<(), SonosError> {
        self.transport("Play", &[("Speed", "1")])
    }

    pub fn pause(&self) -> Result<(), SonosError> {
        self.transport("Pause", &[])
    }

    pub fn stop(&self) -> Result<(), SonosError> {
        self.transport("Stop", &[])
    }

    /// Jumps to `to` (whole seconds) in the current track.
    pub fn seek(&self, to: Duration) -> Result<(), SonosError> {
        let target = format_time(to);
        self.transport("Seek", &[("Unit", "REL_TIME"), ("Target", &target)])
    }

    pub fn transport_state(&self) -> Result<TransportState, SonosError> {
        let fields = self.call(&AV_TRANSPORT, "GetTransportInfo", &[("InstanceID", "0")])?;
        let state = fields
            .get("CurrentTransportState")
            .ok_or(SonosError::Answer {
                action: "GetTransportInfo",
                detail: "no CurrentTransportState".into(),
            })?;
        Ok(parse_state(state))
    }

    pub fn position(&self) -> Result<PositionInfo, SonosError> {
        let fields = self.call(&AV_TRANSPORT, "GetPositionInfo", &[("InstanceID", "0")])?;
        let get = |name: &str| fields.get(name).map(String::as_str).unwrap_or("");
        Ok(PositionInfo {
            track_uri: get("TrackURI").to_string(),
            position: parse_time(get("RelTime")),
            duration: parse_time(get("TrackDuration")).filter(|d| !d.is_zero()),
        })
    }

    /// The volume (0–100) of the group this speaker leads.
    pub fn group_volume(&self) -> Result<u8, SonosError> {
        let fields = self.call(&GROUP_RENDERING, "GetGroupVolume", &[("InstanceID", "0")])?;
        fields
            .get("CurrentVolume")
            .and_then(|v| v.trim().parse::<u8>().ok())
            .map(|v| v.min(100))
            .ok_or(SonosError::Answer {
                action: "GetGroupVolume",
                detail: "no CurrentVolume".into(),
            })
    }

    pub fn set_group_volume(&self, volume: u8) -> Result<(), SonosError> {
        let volume = volume.min(100).to_string();
        self.call(
            &GROUP_RENDERING,
            "SetGroupVolume",
            &[("InstanceID", "0"), ("DesiredVolume", &volume)],
        )
        .map(drop)
    }

    /// The household's rooms and groups, as XML (see `discovery::parse_zone_groups`).
    pub fn zone_group_state(&self) -> Result<String, SonosError> {
        let mut fields = self.call(&ZONE_TOPOLOGY, "GetZoneGroupState", &[])?;
        fields.remove("ZoneGroupState").ok_or(SonosError::Answer {
            action: "GetZoneGroupState",
            detail: "no ZoneGroupState".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_round_trip() {
        assert_eq!(format_time(Duration::from_millis(225_900)), "0:03:45");
        assert_eq!(format_time(Duration::from_secs(3725)), "1:02:05");
        assert_eq!(parse_time("0:03:45"), Some(Duration::from_secs(225)));
        assert_eq!(
            parse_time("00:00:01.500"),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(parse_time("NOT_IMPLEMENTED"), None);
        assert_eq!(parse_time(""), None);
        assert_eq!(parse_time("3:45"), None);
    }

    #[test]
    fn explains_upnp_errors() {
        let e = SonosError::Upnp {
            action: "Play",
            code: 714,
        };
        assert_eq!(
            e.to_string(),
            "Play failed: the speaker does not play this file type (UPnP error 714)"
        );
        assert_eq!(e.upnp_code(), Some(714));
    }
}
