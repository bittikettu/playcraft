//! The Cast protocol's transport: a TLS connection to the device carrying length-prefixed
//! protobuf `CastMessage`s, whose payloads are JSON.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme, StreamOwned};
use serde_json::Value;

pub const NS_CONNECTION: &str = "urn:x-cast:com.google.cast.tp.connection";
pub const NS_HEARTBEAT: &str = "urn:x-cast:com.google.cast.tp.heartbeat";
pub const NS_RECEIVER: &str = "urn:x-cast:com.google.cast.receiver";
pub const NS_MEDIA: &str = "urn:x-cast:com.google.cast.media";

/// Cast messages are at most 64 KiB.
const MAX_MESSAGE: usize = 64 << 10;
/// Reads wait this long at a time, so a receive can stop at its deadline.
const READ_SLICE: Duration = Duration::from_millis(20);

/// One message, with its JSON payload as text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub source: String,
    pub destination: String,
    pub namespace: String,
    pub payload: String,
}

impl Message {
    pub fn new(source: &str, destination: &str, namespace: &str, payload: &Value) -> Self {
        Message {
            source: source.into(),
            destination: destination.into(),
            namespace: namespace.into(),
            payload: payload.to_string(),
        }
    }

    pub fn json(&self) -> Value {
        serde_json::from_str(&self.payload).unwrap_or(Value::Null)
    }
}

fn put_varint(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn put_string(field: u8, s: &str, out: &mut Vec<u8>) {
    out.push(field << 3 | 2);
    put_varint(s.len() as u64, out);
    out.extend_from_slice(s.as_bytes());
}

/// The message as sent: a 4-byte big-endian length, then the protobuf `CastMessage`.
pub fn encode(m: &Message) -> Vec<u8> {
    // protocol_version = CASTV2_1_0 (0)
    let mut body = vec![1 << 3, 0];
    put_string(2, &m.source, &mut body);
    put_string(3, &m.destination, &mut body);
    put_string(4, &m.namespace, &mut body);
    // payload_type = STRING (0)
    body.extend_from_slice(&[5 << 3, 0]);
    put_string(6, &m.payload, &mut body);
    let mut out = (body.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(&body);
    out
}

fn get_varint(data: &[u8], pos: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *data.get(*pos)?;
        *pos += 1;
        v |= u64::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

/// Parses a `CastMessage` (without its length prefix).
pub fn decode(body: &[u8]) -> Option<Message> {
    let mut m = Message {
        source: String::new(),
        destination: String::new(),
        namespace: String::new(),
        payload: String::new(),
    };
    let mut pos = 0;
    while pos < body.len() {
        let key = get_varint(body, &mut pos)?;
        match key & 7 {
            0 => {
                get_varint(body, &mut pos)?;
            }
            2 => {
                let len = get_varint(body, &mut pos)? as usize;
                let bytes = body.get(pos..pos.checked_add(len)?)?;
                pos += len;
                let text = || String::from_utf8_lossy(bytes).into_owned();
                match key >> 3 {
                    2 => m.source = text(),
                    3 => m.destination = text(),
                    4 => m.namespace = text(),
                    6 => m.payload = text(),
                    // A binary payload: not used by the namespaces we speak.
                    _ => {}
                }
            }
            _ => return None,
        }
    }
    Some(m)
}

/// Takes the complete messages off the front of `buf`.
fn take_messages(buf: &mut Vec<u8>) -> io::Result<Vec<Message>> {
    let mut messages = Vec::new();
    let mut used = 0;
    while let Some(head) = buf.get(used..used + 4) {
        let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
        if len > MAX_MESSAGE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "oversized Cast message",
            ));
        }
        let Some(body) = buf.get(used + 4..used + 4 + len) else {
            break;
        };
        match decode(body) {
            Some(m) => messages.push(m),
            None => log::debug!("skipping an unreadable Cast message"),
        }
        used += 4 + len;
    }
    buf.drain(..used);
    Ok(messages)
}

/// Accepts whatever certificate the device shows: Cast devices have self-signed ones, and
/// the player only talks to devices on the local network. The handshake itself is still
/// verified.
#[derive(Debug)]
struct AnyCertificate(Arc<CryptoProvider>);

impl ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

pub type TlsStream = StreamOwned<ClientConnection, TcpStream>;

/// A connection that sends and receives Cast messages.
pub struct Channel<S> {
    stream: S,
    buf: Vec<u8>,
}

impl Channel<TlsStream> {
    /// Connects to a Cast device and completes the TLS handshake within `timeout`.
    pub fn connect(addr: SocketAddr, timeout: Duration) -> io::Result<Self> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .map_err(io::Error::other)?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AnyCertificate(provider)))
            .with_no_client_auth();
        let name = ServerName::IpAddress(addr.ip().into());
        let mut tls = ClientConnection::new(Arc::new(config), name).map_err(io::Error::other)?;
        let mut tcp = TcpStream::connect_timeout(&addr, timeout)?;
        tcp.set_nodelay(true)?;
        tcp.set_read_timeout(Some(timeout))?;
        tcp.set_write_timeout(Some(timeout))?;
        while tls.is_handshaking() {
            tls.complete_io(&mut tcp)?;
        }
        tcp.set_read_timeout(Some(READ_SLICE))?;
        Ok(Channel::new(StreamOwned::new(tls, tcp)))
    }
}

impl<S: Read + Write> Channel<S> {
    pub fn new(stream: S) -> Self {
        Channel {
            stream,
            buf: Vec::new(),
        }
    }

    pub fn send(&mut self, m: &Message) -> io::Result<()> {
        self.stream.write_all(&encode(m))?;
        self.stream.flush()
    }

    /// Returns the messages that arrive within `wait` (as soon as there are some). The
    /// connection is read at least once, even with no time to wait.
    pub fn receive(&mut self, wait: Duration) -> io::Result<Vec<Message>> {
        let deadline = Instant::now() + wait;
        let mut chunk = [0u8; 16 << 10];
        let mut read = false;
        loop {
            let messages = take_messages(&mut self.buf)?;
            if !messages.is_empty() || (read && Instant::now() >= deadline) {
                return Ok(messages);
            }
            read = true;
            match self.stream.read(&mut chunk) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "the device closed the connection",
                    ));
                }
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(e) => return Err(e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn messages_round_trip() {
        let m = Message::new(
            "sender-0",
            "receiver-0",
            NS_RECEIVER,
            &json!({ "type": "GET_STATUS", "requestId": 1, "name": "Televisiö" }),
        );
        let bytes = encode(&m);
        let len = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
        assert_eq!(len, bytes.len() - 4);
        assert_eq!(decode(&bytes[4..]), Some(m.clone()));
        assert_eq!(m.json()["name"], "Televisiö");
        // A long payload needs a multi-byte length.
        let long = Message::new("a", "b", NS_MEDIA, &json!({ "x": "y".repeat(500) }));
        assert_eq!(decode(&encode(&long)[4..]), Some(long));
    }

    #[test]
    fn messages_split_across_reads_are_put_together() {
        let a = Message::new("s", "r", NS_HEARTBEAT, &json!({ "type": "PING" }));
        let b = Message::new("s", "r", NS_HEARTBEAT, &json!({ "type": "PONG" }));
        let mut stream: Vec<u8> = encode(&a);
        stream.extend(encode(&b));
        let mut buf = stream[..7].to_vec();
        assert!(take_messages(&mut buf).unwrap().is_empty());
        buf.extend_from_slice(&stream[7..stream.len() - 1]);
        assert_eq!(take_messages(&mut buf).unwrap(), [a]);
        buf.push(*stream.last().unwrap());
        assert_eq!(take_messages(&mut buf).unwrap(), [b]);
        assert!(buf.is_empty());

        let mut huge = (1u32 << 20).to_be_bytes().to_vec();
        huge.extend([0; 8]);
        assert!(take_messages(&mut huge).is_err());
    }
}
