//! A pretend Google Cast device on localhost for tests: a TLS server that answers the
//! receiver and media messages the player sends, and fetches the songs it is given like a
//! real device would.

use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use serde_json::{Value, json};

use super::channel::{Channel, Message, NS_HEARTBEAT, NS_MEDIA, NS_RECEIVER};
use super::{CastDevice, MEDIA_RECEIVER};
use crate::outputs::sonos::fake::fetch;

const TRANSPORT: &str = "web-7";
const SESSION: &str = "session-7";

#[derive(Debug)]
pub struct FakeCastState {
    /// Messages received, as "<last part of the namespace> <type>", e.g. "media LOAD".
    pub messages: Vec<String>,
    /// The media receiver app is running.
    pub app: bool,
    /// Another app on the screen.
    pub other_app: Option<String>,
    pub content_id: String,
    pub content_type: String,
    /// BUFFERED or LIVE.
    pub stream_type: String,
    /// The `media` of the last LOAD.
    pub media: Value,
    pub metadata: Value,
    /// IDLE, BUFFERING, PLAYING or PAUSED.
    pub player_state: String,
    pub idle_reason: Option<String>,
    pub position: f64,
    pub volume: f64,
    /// Whether to fetch the songs it is given (off: as if a firewall blocks us).
    pub fetch: bool,
    pub fetched: Option<Vec<u8>>,
    /// Content types it refuses to load.
    pub refuse: Vec<String>,
}

impl FakeCastState {
    pub fn count(&self, message: &str) -> usize {
        self.messages.iter().filter(|m| *m == message).count()
    }
}

pub struct FakeCast {
    pub addr: SocketAddr,
    pub name: String,
    state: Arc<Mutex<FakeCastState>>,
}

impl FakeCast {
    pub fn start(name: &str) -> FakeCast {
        let key = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let private = PrivatePkcs8KeyDer::from(key.signing_key.serialize_der());
        let config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(vec![key.cert.der().clone()], PrivateKeyDer::Pkcs8(private))
                .unwrap();
        let config = Arc::new(config);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(FakeCastState {
            messages: Vec::new(),
            app: false,
            other_app: None,
            content_id: String::new(),
            content_type: String::new(),
            stream_type: String::new(),
            media: Value::Null,
            metadata: Value::Null,
            player_state: "IDLE".into(),
            idle_reason: None,
            position: 0.0,
            volume: 0.3,
            fetch: true,
            fetched: None,
            refuse: Vec::new(),
        }));
        let shared = state.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (config, state) = (config.clone(), shared.clone());
                std::thread::spawn(move || {
                    let _ = serve(stream, config, &state);
                });
            }
        });
        FakeCast {
            addr,
            name: name.into(),
            state,
        }
    }

    pub fn state(&self) -> MutexGuard<'_, FakeCastState> {
        self.state.lock().unwrap()
    }

    pub fn device(&self) -> CastDevice {
        CastDevice {
            name: self.name.clone(),
            model: "Smart TV".into(),
            id: format!("fake-cast-{}", self.addr.port()),
            addr: self.addr,
        }
    }
}

fn serve(
    tcp: TcpStream,
    config: Arc<ServerConfig>,
    state: &Arc<Mutex<FakeCastState>>,
) -> std::io::Result<()> {
    tcp.set_read_timeout(Some(Duration::from_millis(20)))?;
    let tls = ServerConnection::new(config).map_err(std::io::Error::other)?;
    let mut channel = Channel::new(StreamOwned::new(tls, tcp));
    loop {
        for message in channel.receive(Duration::from_millis(100))? {
            for reply in answer(&message, state) {
                let reply = Message::new(
                    &message.destination,
                    &message.source,
                    &message.namespace,
                    &reply,
                );
                channel.send(&reply)?;
            }
        }
    }
}

fn receiver_status(s: &FakeCastState, id: &Value) -> Value {
    let mut apps = Vec::new();
    if s.app {
        apps.push(json!({
            "appId": MEDIA_RECEIVER,
            "displayName": "Default Media Receiver",
            "sessionId": SESSION,
            "transportId": TRANSPORT,
        }));
    }
    if let Some(other) = &s.other_app {
        apps.push(json!({
            "appId": "0123ABCD",
            "displayName": other,
            "sessionId": "other",
            "transportId": "web-9",
        }));
    }
    json!({
        "type": "RECEIVER_STATUS",
        "requestId": id,
        "status": { "applications": apps, "volume": { "level": s.volume, "muted": false } },
    })
}

fn media_status(s: &FakeCastState, id: &Value) -> Value {
    let status = if s.content_id.is_empty() {
        json!([])
    } else {
        let mut item = json!({
            "mediaSessionId": 1,
            "playerState": s.player_state,
            "currentTime": s.position,
            "media": { "contentId": s.content_id, "contentType": s.content_type, "duration": 10.0 },
        });
        if let Some(reason) = &s.idle_reason {
            item["idleReason"] = reason.as_str().into();
        }
        json!([item])
    };
    json!({ "type": "MEDIA_STATUS", "requestId": id, "status": status })
}

fn answer(message: &Message, state: &Arc<Mutex<FakeCastState>>) -> Vec<Value> {
    let request = message.json();
    let kind = request["type"].as_str().unwrap_or("").to_string();
    let id = request["requestId"].clone();
    let mut s = state.lock().unwrap();
    let tail = message.namespace.rsplit('.').next().unwrap_or("");
    s.messages.push(format!("{tail} {kind}"));
    match (message.namespace.as_str(), kind.as_str()) {
        (NS_HEARTBEAT, "PING") => vec![json!({ "type": "PONG" })],
        (NS_RECEIVER, "GET_STATUS") => vec![receiver_status(&s, &id)],
        (NS_RECEIVER, "LAUNCH") => {
            s.app = true;
            s.other_app = None;
            vec![receiver_status(&s, &id)]
        }
        (NS_RECEIVER, "STOP") => {
            s.app = false;
            vec![receiver_status(&s, &id)]
        }
        (NS_RECEIVER, "SET_VOLUME") => {
            if let Some(level) = request["volume"]["level"].as_f64() {
                s.volume = level;
            }
            vec![receiver_status(&s, &id)]
        }
        (NS_MEDIA, _) if !s.app || message.destination != TRANSPORT => {
            vec![json!({ "type": "INVALID_REQUEST", "requestId": id, "reason": "INVALID_COMMAND" })]
        }
        (NS_MEDIA, "LOAD") => {
            let media = &request["media"];
            let content_type = media["contentType"].as_str().unwrap_or("").to_string();
            if s.refuse.contains(&content_type) {
                return vec![json!({ "type": "LOAD_FAILED", "requestId": id })];
            }
            s.content_id = media["contentId"].as_str().unwrap_or("").to_string();
            s.content_type = content_type;
            s.stream_type = media["streamType"].as_str().unwrap_or("").to_string();
            s.media = media.clone();
            s.metadata = media["metadata"].clone();
            s.position = request["currentTime"].as_f64().unwrap_or(0.0);
            s.idle_reason = None;
            s.fetched = None;
            let http = s.content_id.starts_with("http://");
            s.player_state = if http && !s.fetch {
                // It never gets the song.
                "BUFFERING".into()
            } else if request["autoplay"].as_bool().unwrap_or(true) {
                "PLAYING".into()
            } else {
                "PAUSED".into()
            };
            if http && s.fetch {
                let (url, state) = (s.content_id.clone(), state.clone());
                std::thread::spawn(move || {
                    let bytes = fetch(&url);
                    state.lock().unwrap().fetched = bytes;
                });
            }
            vec![media_status(&s, &id)]
        }
        (NS_MEDIA, "GET_STATUS") => vec![media_status(&s, &id)],
        (NS_MEDIA, "PLAY") => {
            s.player_state = "PLAYING".into();
            vec![media_status(&s, &id)]
        }
        (NS_MEDIA, "PAUSE") => {
            s.player_state = "PAUSED".into();
            vec![media_status(&s, &id)]
        }
        (NS_MEDIA, "STOP") => {
            s.player_state = "IDLE".into();
            s.idle_reason = Some("CANCELLED".into());
            vec![media_status(&s, &id)]
        }
        (NS_MEDIA, "SEEK") => {
            s.position = request["currentTime"].as_f64().unwrap_or(s.position);
            vec![media_status(&s, &id)]
        }
        _ => Vec::new(),
    }
}
