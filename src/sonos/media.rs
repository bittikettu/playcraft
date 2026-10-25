//! The media server Sonos speakers fetch songs from. Only files handed to `publish` are
//! served, under a random secret in the URL; byte ranges let the speaker seek. A song can
//! also be published as WAV, converted while it is sent (`wav`), for speakers that cannot
//! play the original.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::http::{self, header};
use super::wav::WavSong;

/// Speakers open a few connections at a time; more than this is not a speaker.
const MAX_CONNECTIONS: usize = 16;
/// A speaker reads at its own pace and may sit on a paused song for a long time.
const WRITE_TIMEOUT: Duration = Duration::from_secs(3600);
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// The MIME type speakers expect for an audio file, by extension.
pub fn mime_type(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("mp3") => "audio/mpeg",
        Some("flac") => "audio/flac",
        Some("ogg" | "oga") => "audio/ogg",
        Some("wav") => "audio/wav",
        Some("aac") => "audio/aac",
        Some("m4a" | "mp4") => "audio/mp4",
        _ => "application/octet-stream",
    }
}

/// The address of this computer on the network that leads to `peer`.
pub fn local_ip_for(peer: SocketAddr) -> io::Result<IpAddr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    // Nothing is sent: connecting a UDP socket only picks the route.
    socket.connect(peer)?;
    Ok(socket.local_addr()?.ip())
}

#[derive(Default)]
struct Published {
    /// Each file, and whether it is sent as WAV.
    files: Vec<(PathBuf, bool)>,
    ids: HashMap<(PathBuf, bool), usize>,
    /// Requests per file, to tell whether a speaker could reach us at all.
    fetches: Vec<u32>,
}

pub struct MediaServer {
    port: u16,
    secret: String,
    published: Arc<Mutex<Published>>,
}

impl MediaServer {
    /// Listens on `ip` (all adapters in the player; localhost in tests) and `port` (0 = any
    /// free port).
    pub fn start(ip: IpAddr, port: u16) -> io::Result<MediaServer> {
        let listener = TcpListener::bind((ip, port))?;
        let port = listener.local_addr()?.port();
        let secret = format!("{:032x}", rand::random::<u128>());
        let published = Arc::new(Mutex::new(Published::default()));
        let open = Arc::new(AtomicUsize::new(0));
        {
            let (secret, published) = (secret.clone(), published.clone());
            std::thread::Builder::new()
                .name("sonos-media".into())
                .spawn(move || {
                    for stream in listener.incoming().flatten() {
                        if open.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
                            open.fetch_sub(1, Ordering::AcqRel);
                            continue;
                        }
                        let (secret, published, open) =
                            (secret.clone(), published.clone(), open.clone());
                        let spawned = std::thread::Builder::new()
                            .name("sonos-media-client".into())
                            .spawn(move || {
                                let peer = stream.peer_addr().ok();
                                if let Err(e) = serve(stream, &secret, &published) {
                                    log::debug!("media request from {peer:?}: {e}");
                                }
                                open.fetch_sub(1, Ordering::AcqRel);
                            });
                        if spawned.is_err() {
                            log::warn!("media server: cannot start a thread");
                        }
                    }
                })?;
        }
        log::info!("media server for Sonos listening on port {port}");
        Ok(MediaServer {
            port,
            secret,
            published,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The URL path that serves `path`, as it is or as WAV (published on first use).
    pub fn publish(&self, path: &Path, wav: bool) -> String {
        let mut p = self.published.lock().unwrap_or_else(|e| e.into_inner());
        let key = (path.to_path_buf(), wav);
        let id = match p.ids.get(&key) {
            Some(&id) => id,
            None => {
                let id = p.files.len();
                p.files.push(key.clone());
                p.fetches.push(0);
                p.ids.insert(key, id);
                id
            }
        };
        let ext = if wav {
            "wav".to_string()
        } else {
            path.extension()
                .and_then(|e| e.to_str())
                .map_or_else(|| "bin".to_string(), str::to_ascii_lowercase)
        };
        format!("/{}/{id}.{ext}", self.secret)
    }

    /// The full URL of `path` (as WAV with `wav`) for a speaker at `speaker`.
    pub fn url_for(&self, path: &Path, wav: bool, speaker: SocketAddr) -> io::Result<String> {
        let ip = local_ip_for(speaker)?;
        Ok(format!(
            "http://{}{}",
            SocketAddr::new(ip, self.port),
            self.publish(path, wav)
        ))
    }

    /// How many requests the file behind `url` (from `url_for`/`publish`) has had.
    pub fn fetches(&self, url: &str) -> u32 {
        let p = self.published.lock().unwrap_or_else(|e| e.into_inner());
        file_id(url, &self.secret)
            .and_then(|id| p.fetches.get(id).copied())
            .unwrap_or(0)
    }
}

/// The file id in a URL or path `.../<secret>/<id>.<ext>`.
fn file_id(url: &str, secret: &str) -> Option<usize> {
    let (_, rest) = url.split_once(&format!("/{secret}/"))?;
    let (id, _ext) = rest.split_once('.')?;
    id.parse().ok()
}

/// A `Range: bytes=...` header for a file of `len` bytes: `Ok(None)` serves the whole file
/// (also for multiple ranges, which we do not do), `Err` is unsatisfiable.
fn parse_range(value: &str, len: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(spec) = value.trim().strip_prefix("bytes=") else {
        return Ok(None);
    };
    if spec.contains(',') {
        return Ok(None);
    }
    let (start, end) = spec.split_once('-').ok_or(())?;
    let (start, end) = (start.trim(), end.trim());
    let range = if start.is_empty() {
        let n: u64 = end.parse().map_err(|_| ())?;
        if n == 0 {
            return Err(());
        }
        (len.saturating_sub(n), len.saturating_sub(1))
    } else {
        let start: u64 = start.parse().map_err(|_| ())?;
        let end = if end.is_empty() {
            len.saturating_sub(1)
        } else {
            end.parse::<u64>()
                .map_err(|_| ())?
                .min(len.saturating_sub(1))
        };
        (start, end)
    };
    if range.0 >= len || range.0 > range.1 {
        return Err(());
    }
    Ok(Some(range))
}

fn reply(stream: &mut TcpStream, status: &str, extra: &str) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n{extra}\r\n"
    )
}

fn serve(mut stream: TcpStream, secret: &str, published: &Mutex<Published>) -> io::Result<()> {
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    let (request_line, headers) = http::read_head(&mut BufReader::new(stream.try_clone()?))?;
    let mut parts = request_line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    if method != "GET" && method != "HEAD" {
        return reply(
            &mut stream,
            "405 Method Not Allowed",
            "Allow: GET, HEAD\r\n",
        );
    }
    let file = file_id(target, secret).and_then(|id| {
        let mut p = published.lock().unwrap_or_else(|e| e.into_inner());
        let (path, wav) = p.files.get(id).cloned()?;
        if p.fetches[id] == 0 {
            log::info!(
                "{} is fetching {}{}",
                stream
                    .peer_addr()
                    .map_or("a speaker".into(), |a| a.ip().to_string()),
                path.display(),
                if wav { " as WAV" } else { "" }
            );
        }
        p.fetches[id] += 1;
        Some((path, wav))
    });
    let Some((path, wav)) = file else {
        return reply(&mut stream, "404 Not Found", "");
    };
    if wav {
        return serve_wav(stream, &path, method, &headers);
    }
    let mut file = match File::open(&path) {
        Ok(f) => f,
        Err(e) => {
            log::warn!("media server: {}: {e}", path.display());
            return reply(&mut stream, "404 Not Found", "");
        }
    };
    let len = file.metadata()?.len();
    let Some((start, count)) = respond(&mut stream, mime_type(&path), len, &headers)? else {
        return Ok(());
    };
    if method == "GET" {
        file.seek(SeekFrom::Start(start))?;
        io::copy(&mut file.take(count), &mut stream)?;
    }
    stream.flush()
}

/// Sends the response head for a file of `len` bytes, honouring a byte range. Returns the
/// range to send, or `None` when the range cannot be satisfied (already answered).
fn respond(
    stream: &mut TcpStream,
    mime: &str,
    len: u64,
    headers: &[(String, String)],
) -> io::Result<Option<(u64, u64)>> {
    let range = match header(headers, "range").map(|r| parse_range(r, len)) {
        Some(Err(())) => {
            reply(
                stream,
                "416 Range Not Satisfiable",
                &format!("Content-Range: bytes */{len}\r\n"),
            )?;
            return Ok(None);
        }
        Some(Ok(range)) => range,
        None => None,
    };
    let (status, start, count, content_range) = match range {
        Some((start, end)) => (
            "206 Partial Content",
            start,
            end - start + 1,
            format!("Content-Range: bytes {start}-{end}/{len}\r\n"),
        ),
        None => ("200 OK", 0, len, String::new()),
    };
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {count}\r\n\
         Accept-Ranges: bytes\r\n{content_range}Connection: close\r\n\r\n"
    )?;
    Ok(Some((start, count)))
}

fn serve_wav(
    mut stream: TcpStream,
    path: &Path,
    method: &str,
    headers: &[(String, String)],
) -> io::Result<()> {
    let song = match WavSong::probe(path) {
        Ok(song) => song,
        Err(e) => {
            log::warn!("media server: {}: {e}", path.display());
            return reply(&mut stream, "404 Not Found", "");
        }
    };
    let (start, count) = match song.len {
        Some(len) => match respond(&mut stream, "audio/wav", len, headers)? {
            Some((start, count)) => (start, Some(count)),
            None => return Ok(()),
        },
        // Unknown length: the whole song, to the end of the connection.
        None => {
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nConnection: close\r\n\r\n"
            )?;
            (0, None)
        }
    };
    if method == "GET" {
        song.write(path, start, count, &mut stream)?;
    }
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_byte_ranges() {
        assert_eq!(parse_range("bytes=0-", 100), Ok(Some((0, 99))));
        assert_eq!(parse_range("bytes=10-19", 100), Ok(Some((10, 19))));
        assert_eq!(parse_range("bytes=90-200", 100), Ok(Some((90, 99))));
        assert_eq!(parse_range("bytes=-10", 100), Ok(Some((90, 99))));
        assert_eq!(parse_range("bytes=0-1,5-6", 100), Ok(None));
        assert_eq!(parse_range("items=0-1", 100), Ok(None));
        assert_eq!(parse_range("bytes=100-", 100), Err(()));
        assert_eq!(parse_range("bytes=5-4", 100), Err(()));
        assert_eq!(parse_range("bytes=x-", 100), Err(()));
    }

    fn get(addr: SocketAddr, path: &str, range: Option<&str>) -> (u16, String, Vec<u8>) {
        let mut stream = TcpStream::connect(addr).unwrap();
        let range = range.map_or(String::new(), |r| format!("Range: {r}\r\n"));
        write!(stream, "GET {path} HTTP/1.1\r\nHost: x\r\n{range}\r\n").unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).unwrap();
        let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = String::from_utf8_lossy(&raw[..split]).into_owned();
        let status = head[9..12].parse().unwrap();
        (status, head, raw[split + 4..].to_vec())
    }

    #[test]
    fn serves_published_files_with_ranges() {
        let dir = tempfile::tempdir().unwrap();
        let song = dir.path().join("Song.FLAC");
        let data: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
        std::fs::write(&song, &data).unwrap();
        let server = MediaServer::start(Ipv4Addr::LOCALHOST.into(), 0).unwrap();
        let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), server.port());
        let path = server.publish(&song, false);
        assert!(path.ends_with("/0.flac"), "{path}");
        assert_eq!(
            server.publish(&song, false),
            path,
            "the same file keeps its URL"
        );

        let (status, head, body) = get(addr, &path, None);
        assert_eq!(status, 200);
        assert!(head.contains("Content-Type: audio/flac"), "{head}");
        assert!(head.contains("Accept-Ranges: bytes"));
        assert_eq!(body, data);

        let (status, head, body) = get(addr, &path, Some("bytes=4000-"));
        assert_eq!(status, 206);
        assert!(
            head.contains("Content-Range: bytes 4000-4999/5000"),
            "{head}"
        );
        assert_eq!(body, &data[4000..]);
        let (status, _, _) = get(addr, &path, Some("bytes=6000-"));
        assert_eq!(status, 416);
        assert_eq!(server.fetches(&path), 3);

        // Anything not published, or without the secret, is not there.
        let (status, _, _) = get(addr, "/0.flac", None);
        assert_eq!(status, 404);
        let wrong = path.replace(&path[1..9], "00000000");
        assert_eq!(get(addr, &wrong, None).0, 404);
        assert_eq!(get(addr, &path.replace("/0.", "/1."), None).0, 404);
    }

    #[test]
    fn serves_songs_as_wav_with_ranges() {
        let song = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tagged.flac");
        let server = MediaServer::start(Ipv4Addr::LOCALHOST.into(), 0).unwrap();
        let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), server.port());
        let path = server.publish(&song, true);
        assert!(path.ends_with("/0.wav"), "{path}");
        assert_ne!(server.publish(&song, false), path);

        let (status, head, whole) = get(addr, &path, None);
        assert_eq!(status, 200);
        assert!(head.contains("Content-Type: audio/wav"), "{head}");
        assert_eq!(&whole[..4], b"RIFF");
        let len = whole.len();
        assert!(head.contains(&format!("Content-Length: {len}")), "{head}");
        let (status, head, tail) = get(addr, &path, Some("bytes=-1000"));
        assert_eq!(status, 206);
        let from = len - 1000;
        assert!(
            head.contains(&format!("bytes {from}-{}/{len}", len - 1)),
            "{head}"
        );
        assert_eq!(tail.len(), 1000);
    }

    #[test]
    fn urls_use_the_address_facing_the_speaker() {
        let server = MediaServer::start(Ipv4Addr::LOCALHOST.into(), 0).unwrap();
        let speaker: SocketAddr = "127.0.0.1:1400".parse().unwrap();
        let url = server
            .url_for(Path::new("/music/a b.mp3"), false, speaker)
            .unwrap();
        let expected = format!("http://127.0.0.1:{}/", server.port());
        assert!(url.starts_with(&expected), "{url}");
        assert!(url.ends_with("/0.mp3"));
        assert_eq!(server.fetches(&url), 0);
    }
}
