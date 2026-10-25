//! A pretend Sonos speaker on localhost for tests: it answers the UPnP calls the player
//! makes, records them, and fetches the songs it is given like a real speaker would.

use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use super::control::{format_time, parse_time};
use super::discovery::Zone;
use super::http::{self, header, xml_escape};

#[derive(Debug, Default)]
pub struct FakeState {
    /// SOAP actions received, in order.
    pub actions: Vec<String>,
    pub uri: String,
    pub metadata: String,
    /// `PLAYING`, `PAUSED_PLAYBACK`, `STOPPED` or `TRANSITIONING`.
    pub transport: String,
    pub position: Duration,
    pub duration: Option<Duration>,
    pub volume: u8,
    /// Whether to fetch songs given by `SetAVTransportURI` (off: as if a firewall blocks us).
    pub fetch: bool,
    /// Bytes of the last song fetched.
    pub fetched: Option<Vec<u8>>,
    /// Actions that fail with this UPnP error code.
    pub fail: HashMap<String, u32>,
    /// Endings of URIs the speaker refuses to play (UPnP error 714), e.g. ".mp3".
    pub refuse: Vec<String>,
}

impl FakeState {
    pub fn count(&self, action: &str) -> usize {
        self.actions.iter().filter(|a| *a == action).count()
    }
}

pub struct FakeSonos {
    pub addr: SocketAddr,
    pub room: String,
    state: Arc<Mutex<FakeState>>,
}

impl FakeSonos {
    pub fn start(room: &str) -> FakeSonos {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(FakeState {
            transport: "STOPPED".into(),
            volume: 25,
            fetch: true,
            ..FakeState::default()
        }));
        let (shared, room_name) = (state.clone(), room.to_string());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (state, room) = (shared.clone(), room_name.clone());
                std::thread::spawn(move || {
                    let _ = answer(stream, state, addr, &room);
                });
            }
        });
        FakeSonos {
            addr,
            room: room.into(),
            state,
        }
    }

    pub fn state(&self) -> MutexGuard<'_, FakeState> {
        self.state.lock().unwrap()
    }

    pub fn zone(&self) -> Zone {
        Zone {
            rooms: vec![self.room.clone()],
            coordinator: self.addr,
            uuid: format!("RINCON_{}", self.addr.port()),
            members: vec![self.addr],
        }
    }
}

/// The text of the argument `name` in a SOAP request body.
fn argument(body: &str, name: &str) -> String {
    roxmltree::Document::parse(body)
        .ok()
        .and_then(|doc| {
            doc.descendants()
                .find(|n| n.has_tag_name(name))
                .map(|n| n.text().unwrap_or("").to_string())
        })
        .unwrap_or_default()
}

/// Fetches `url` (http://host:port/path) the way a speaker does.
fn fetch(url: &str) -> Option<Vec<u8>> {
    let rest = url.strip_prefix("http://")?;
    let (host, path) = rest.split_at(rest.find('/')?);
    let mut stream = TcpStream::connect(host).ok()?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nRange: bytes=0-\r\n\r\n"
    )
    .ok()?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).ok()?;
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&raw[..split]);
    (head.starts_with("HTTP/1.1 200") || head.starts_with("HTTP/1.1 206"))
        .then(|| raw[split + 4..].to_vec())
}

fn answer(
    mut stream: TcpStream,
    state: Arc<Mutex<FakeState>>,
    addr: SocketAddr,
    room: &str,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let (_, headers) = http::read_head(&mut reader)?;
    let len: usize = header(&headers, "content-length")
        .and_then(|l| l.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; len];
    reader.read_exact(&mut body)?;
    let body = String::from_utf8_lossy(&body).into_owned();
    let soap_action = header(&headers, "soapaction")
        .unwrap_or("")
        .trim_matches('"');
    let (urn, action) = soap_action.split_once('#').unwrap_or(("", ""));

    let mut out: Vec<(&str, String)> = Vec::new();
    let mut fetch_url = None;
    let failure = {
        let mut s = state.lock().unwrap();
        s.actions.push(action.to_string());
        let mut failure = s.fail.get(action).copied();
        if action == "SetAVTransportURI" {
            let uri = argument(&body, "CurrentURI");
            if s.refuse.iter().any(|end| uri.ends_with(end.as_str())) {
                failure = Some(714);
            }
        }
        if failure.is_none() {
            match action {
                "SetAVTransportURI" => {
                    s.uri = argument(&body, "CurrentURI");
                    s.metadata = argument(&body, "CurrentURIMetaData");
                    s.transport = "STOPPED".into();
                    s.position = Duration::ZERO;
                    s.fetched = None;
                    if s.fetch && s.uri.starts_with("http://") {
                        fetch_url = Some(s.uri.clone());
                    }
                }
                // A speaker that cannot fetch the song never gets past buffering.
                "Play" if !s.fetch && s.uri.starts_with("http") => {
                    s.transport = "TRANSITIONING".into()
                }
                "Play" => s.transport = "PLAYING".into(),
                "Pause" => s.transport = "PAUSED_PLAYBACK".into(),
                "Stop" => s.transport = "STOPPED".into(),
                "Seek" => s.position = parse_time(&argument(&body, "Target")).unwrap_or_default(),
                "GetTransportInfo" => {
                    out.push(("CurrentTransportState", s.transport.clone()));
                    out.push(("CurrentTransportStatus", "OK".into()));
                }
                "GetPositionInfo" => {
                    out.push(("Track", "1".into()));
                    let duration = s.duration.map_or("NOT_IMPLEMENTED".into(), format_time);
                    out.push(("TrackDuration", duration));
                    out.push(("TrackMetaData", s.metadata.clone()));
                    out.push(("TrackURI", s.uri.clone()));
                    out.push(("RelTime", format_time(s.position)));
                }
                "GetGroupVolume" => out.push(("CurrentVolume", s.volume.to_string())),
                "SetGroupVolume" => {
                    s.volume = argument(&body, "DesiredVolume").parse().unwrap_or(0);
                }
                "GetZoneGroupState" => {
                    let xml = format!(
                        "<ZoneGroupState><ZoneGroups><ZoneGroup Coordinator=\"RINCON_{port}\" \
                         ID=\"RINCON_{port}:1\"><ZoneGroupMember UUID=\"RINCON_{port}\" \
                         Location=\"http://{addr}/xml/device_description.xml\" \
                         ZoneName=\"{room}\"/></ZoneGroup></ZoneGroups></ZoneGroupState>",
                        port = addr.port(),
                        room = xml_escape(room),
                    );
                    out.push(("ZoneGroupState", xml));
                }
                _ => {}
            }
        }
        failure
    };
    // Like a speaker, fetch the song while answering.
    if let Some(url) = fetch_url {
        let state = state.clone();
        std::thread::spawn(move || {
            let bytes = fetch(&url);
            state.lock().unwrap().fetched = bytes;
        });
    }
    let envelope = |inner: String| {
        format!(
            "<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
             s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body>{inner}\
             </s:Body></s:Envelope>"
        )
    };
    let (status, xml) = match failure {
        Some(code) => (
            "500 Internal Server Error",
            envelope(format!(
                "<s:Fault><faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring>\
                 <detail><UPnPError xmlns=\"urn:schemas-upnp-org:control-1-0\">\
                 <errorCode>{code}</errorCode></UPnPError></detail></s:Fault>"
            )),
        ),
        None => {
            let fields: String = out
                .iter()
                .map(|(k, v)| format!("<{k}>{}</{k}>", xml_escape(v)))
                .collect();
            (
                "200 OK",
                envelope(format!(
                    "<u:{action}Response xmlns:u=\"{urn}\">{fields}</u:{action}Response>"
                )),
            )
        }
    };
    write!(
        stream,
        "HTTP/1.1 {status}\r\nCONTENT-LENGTH: {}\r\nCONTENT-TYPE: text/xml; charset=\"utf-8\"\r\n\
         SERVER: Linux UPnP/1.0 Sonos/80.1 (ZPS27)\r\nConnection: close\r\n\r\n{xml}",
        xml.len()
    )
}
