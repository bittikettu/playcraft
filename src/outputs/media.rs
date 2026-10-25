//! The media server Sonos speakers and Cast devices fetch songs from. Only files handed to
//! `publish` are served, under a random secret in the URL; byte ranges let the device seek.
//! A song can also be published as WAV, converted while it is sent (`wav`), for devices that
//! cannot play the original, and with its cover art.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::card::Card;
use super::http::{self, header};
use super::wav::WavSong;

/// Speakers open a few connections at a time; more than this is not a speaker.
const MAX_CONNECTIONS: usize = 16;
/// A speaker reads at its own pace and may sit on a paused song for a long time.
const WRITE_TIMEOUT: Duration = Duration::from_secs(3600);
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// The MIME type devices expect for an audio (or cover image) file, by extension.
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
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("m3u8") => "application/vnd.apple.mpegurl",
        Some("ts") => "video/mp2t",
        Some("png") => "image/png",
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

/// Image files in an album's folder that hold its cover, best first.
const COVER_NAMES: &[&str] = &["cover", "folder", "front", "albumart", "album"];

/// The image type of picture data, by its first bytes (JPEG and PNG, which devices show).
fn image_type(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if data.starts_with(b"\x89PNG") {
        Some("image/png")
    } else {
        None
    }
}

/// The front cover in `song`'s tags (else its first picture), with its MIME type.
fn embedded_art(song: &Path) -> Option<(Vec<u8>, &'static str)> {
    use lofty::file::TaggedFileExt;
    use lofty::picture::PictureType;
    let tagged = lofty::read_from_path(song).ok()?;
    let pictures: Vec<_> = tagged
        .tags()
        .iter()
        .flat_map(|t| t.pictures())
        .filter(|p| image_type(p.data()).is_some())
        .collect();
    let picture = pictures
        .iter()
        .find(|p| p.pic_type() == PictureType::CoverFront)
        .or(pictures.first())?;
    let data = picture.data().to_vec();
    let mime = image_type(&data)?;
    Some((data, mime))
}

/// A cover image next to `song`, such as `cover.jpg` or `folder.png`.
fn folder_art(song: &Path) -> Option<PathBuf> {
    let rank = |p: &Path| {
        let stem = p.file_stem()?.to_str()?.to_ascii_lowercase();
        let ext = p.extension()?.to_str()?.to_ascii_lowercase();
        if !["jpg", "jpeg", "png"].contains(&ext.as_str()) {
            return None;
        }
        COVER_NAMES.iter().position(|n| *n == stem)
    };
    std::fs::read_dir(song.parent()?)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter_map(|p| rank(&p).map(|r| (r, p)))
        .min()
        .map(|(_, p)| p)
}

/// How a published file is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    /// As it is.
    File,
    /// Converted to WAV.
    Wav,
    /// The picture in its tags.
    Art,
    /// A cover card drawn for it (by the hash of what the card shows).
    Card(u64),
    /// A folder whose files are served by name (the TV stream).
    Dir,
}

#[derive(Default)]
struct Published {
    files: Vec<(PathBuf, Kind)>,
    ids: HashMap<(PathBuf, Kind), usize>,
    /// What the cover cards show, by file id; they are drawn when fetched.
    cards: HashMap<usize, Arc<Card>>,
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

    fn publish_as(&self, path: &Path, kind: Kind, ext: &str) -> String {
        let mut p = self.published.lock().unwrap_or_else(|e| e.into_inner());
        let key = (path.to_path_buf(), kind);
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
        format!("/{}/{id}.{ext}", self.secret)
    }

    /// The URL path that serves `path`, as it is or as WAV (published on first use).
    pub fn publish(&self, path: &Path, wav: bool) -> String {
        if wav {
            return self.publish_as(path, Kind::Wav, "wav");
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map_or_else(|| "bin".to_string(), str::to_ascii_lowercase);
        self.publish_as(path, Kind::File, &ext)
    }

    /// The URL path of `song`'s cover art: the picture in its tags, or a cover image in its
    /// folder. `None` if it has neither.
    pub fn publish_art(&self, song: &Path) -> Option<String> {
        if let Some((_, mime)) = embedded_art(song) {
            let ext = if mime == "image/png" { "png" } else { "jpg" };
            return Some(self.publish_as(song, Kind::Art, ext));
        }
        folder_art(song).map(|cover| self.publish(&cover, false))
    }

    fn url(&self, path: &str, device: SocketAddr) -> io::Result<String> {
        let ip = local_ip_for(device)?;
        Ok(format!("http://{}{path}", SocketAddr::new(ip, self.port)))
    }

    /// The full URL of `path` (as WAV with `wav`) for a device at `device`.
    pub fn url_for(&self, path: &Path, wav: bool, device: SocketAddr) -> io::Result<String> {
        self.url(&self.publish(path, wav), device)
    }

    /// The full URL of `song`'s cover art for a device at `device`, if it has any.
    pub fn art_url_for(&self, song: &Path, device: SocketAddr) -> Option<String> {
        let path = self.publish_art(song)?;
        self.url(&path, device).ok()
    }

    /// The URL path of a cover card for `song`.
    pub fn publish_card(&self, song: &Path, card: Card) -> String {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&card, &mut hasher);
        let hash = std::hash::Hasher::finish(&hasher);
        let path = self.publish_as(song, Kind::Card(hash), "png");
        if let Some(id) = file_id(&path, &self.secret) {
            let mut p = self.published.lock().unwrap_or_else(|e| e.into_inner());
            p.cards.entry(id).or_insert_with(|| Arc::new(card));
        }
        path
    }

    /// The full URL of `name` in the folder `dir` (published on first use), for a device
    /// at `device`. Only plain file names in it are served.
    pub fn dir_url_for(&self, dir: &Path, name: &str, device: SocketAddr) -> io::Result<String> {
        let path = self.publish_as(dir, Kind::Dir, "hls");
        self.url(&format!("{path}/{name}"), device)
    }

    /// The full URL of a cover card for `song`, for a device at `device`.
    pub fn card_url_for(&self, song: &Path, card: Card, device: SocketAddr) -> Option<String> {
        self.url(&self.publish_card(song, card), device).ok()
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
    if method == "OPTIONS" {
        // A web player (the TV's) asking whether it may fetch from us.
        return reply(
            &mut stream,
            "204 No Content",
            "Access-Control-Allow-Origin: *\r\n\
             Access-Control-Allow-Methods: GET, HEAD, OPTIONS\r\n\
             Access-Control-Allow-Headers: *\r\n",
        );
    }
    if method != "GET" && method != "HEAD" {
        return reply(
            &mut stream,
            "405 Method Not Allowed",
            "Allow: GET, HEAD\r\n",
        );
    }
    let file = file_id(target, secret).and_then(|id| {
        let mut p = published.lock().unwrap_or_else(|e| e.into_inner());
        let (path, kind) = p.files.get(id).cloned()?;
        let card = p.cards.get(&id).cloned();
        if p.fetches[id] == 0 {
            let how = match kind {
                Kind::File => "",
                Kind::Wav => " as WAV",
                Kind::Art => "'s cover",
                Kind::Card(_) => "'s cover card",
                Kind::Dir => " (a stream)",
            };
            log::info!(
                "{} is fetching {}{how}",
                stream
                    .peer_addr()
                    .map_or("a device".into(), |a| a.ip().to_string()),
                path.display(),
            );
        }
        p.fetches[id] += 1;
        Some((path, kind, card))
    });
    let Some((path, kind, card)) = file else {
        return reply(&mut stream, "404 Not Found", "");
    };
    match kind {
        Kind::File => {}
        Kind::Dir => return serve_from_dir(stream, &path, target, method),
        Kind::Wav => return serve_wav(stream, &path, method, &headers),
        Kind::Art => {
            let Some((data, mime)) = embedded_art(&path) else {
                return reply(&mut stream, "404 Not Found", "");
            };
            let Some((start, count)) = respond(&mut stream, mime, data.len() as u64, &headers)?
            else {
                return Ok(());
            };
            if method == "GET" {
                stream.write_all(&data[start as usize..(start + count) as usize])?;
            }
            return stream.flush();
        }
        Kind::Card(_) => {
            let Some(card) = card else {
                return reply(&mut stream, "404 Not Found", "");
            };
            let data = card.png();
            let Some((start, count)) =
                respond(&mut stream, "image/png", data.len() as u64, &headers)?
            else {
                return Ok(());
            };
            if method == "GET" {
                stream.write_all(&data[start as usize..(start + count) as usize])?;
            }
            return stream.flush();
        }
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

/// A plain file name: letters, digits, `_`, `-` and dots, but not `..`.
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains("..")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// Serves `.../<id>.hls/<name>` from the folder `dir`, never cached (it changes as the
/// stream goes on).
fn serve_from_dir(mut stream: TcpStream, dir: &Path, target: &str, method: &str) -> io::Result<()> {
    let name = target
        .split_once(".hls/")
        .map(|(_, name)| name)
        .filter(|name| plain_name(name));
    let data = name.and_then(|name| std::fs::read(dir.join(name)).ok().map(|d| (name, d)));
    let Some((name, data)) = data else {
        log::info!("stream: {target} not found");
        return reply(&mut stream, "404 Not Found", "");
    };
    let started = std::time::Instant::now();
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
         Cache-Control: no-cache\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n",
        mime_type(Path::new(name)),
        data.len()
    )?;
    if method == "GET" {
        stream.write_all(&data)?;
    }
    stream.flush()?;
    // How fast the TV takes the stream: slow sends mean too little bandwidth.
    let took = started.elapsed();
    if name.ends_with(".ts") || took > std::time::Duration::from_millis(300) {
        log::info!(
            "stream: sent {name} ({} KB) in {} ms",
            data.len() / 1024,
            took.as_millis()
        );
    }
    Ok(())
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

    /// A minimal PNG: the signature and an empty IHDR-like chunk are enough to be recognised.
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x02\0\0\0";

    #[test]
    fn serves_cover_art_from_tags_or_the_folder() {
        use lofty::config::WriteOptions;
        use lofty::file::TaggedFileExt;
        use lofty::picture::{Picture, PictureType};
        use lofty::tag::TagExt;

        let dir = tempfile::tempdir().unwrap();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tagged.flac");
        let server = MediaServer::start(Ipv4Addr::LOCALHOST.into(), 0).unwrap();
        let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), server.port());

        // No art at all.
        let bare = dir.path().join("bare.flac");
        std::fs::copy(&fixture, &bare).unwrap();
        assert_eq!(server.publish_art(&bare), None);

        // A cover image in the folder ("folder.jpg" loses to "cover.jpg").
        let album = dir.path().join("album");
        std::fs::create_dir(&album).unwrap();
        let song = album.join("01.flac");
        std::fs::copy(&fixture, &song).unwrap();
        std::fs::write(album.join("folder.jpg"), b"\xFF\xD8\xFFfolder").unwrap();
        std::fs::write(album.join("Cover.JPG"), b"\xFF\xD8\xFFcover").unwrap();
        let art = server.publish_art(&song).unwrap();
        assert!(art.ends_with(".jpg"), "{art}");
        let (status, head, body) = get(addr, &art, None);
        assert_eq!(status, 200);
        assert!(head.contains("Content-Type: image/jpeg"), "{head}");
        assert_eq!(body, b"\xFF\xD8\xFFcover");

        // A picture in the tags wins.
        let mut tagged = lofty::read_from_path(&song).unwrap();
        let tag = tagged.primary_tag_mut().unwrap();
        let mut picture = Picture::from_reader(&mut &PNG[..]).unwrap();
        picture.set_pic_type(PictureType::CoverFront);
        tag.push_picture(picture);
        tag.save_to_path(&song, WriteOptions::default()).unwrap();
        let art = server.publish_art(&song).unwrap();
        assert!(art.ends_with(".png"), "{art}");
        let (status, head, body) = get(addr, &art, None);
        assert_eq!(status, 200);
        assert!(head.contains("Content-Type: image/png"), "{head}");
        assert_eq!(body, PNG);
        let url = server.art_url_for(&song, addr).unwrap();
        assert!(url.starts_with("http://127.0.0.1:"), "{url}");
    }

    #[test]
    fn serves_a_stream_folder_by_name_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("live.m3u8"),
            "#EXTM3U
",
        )
        .unwrap();
        std::fs::write(dir.path().join("seg00001.ts"), [0x47u8; 376]).unwrap();
        std::fs::write(dir.path().parent().unwrap().join("secret.txt"), "no").ok();
        let server = MediaServer::start(Ipv4Addr::LOCALHOST.into(), 0).unwrap();
        let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), server.port());
        let url = server.dir_url_for(dir.path(), "live.m3u8", addr).unwrap();
        let rest = url.strip_prefix("http://").unwrap();
        let path = &rest[rest.find('/').unwrap()..];
        let (status, head, body) = get(addr, path, None);
        assert_eq!(status, 200);
        assert!(
            head.contains("Content-Type: application/vnd.apple.mpegurl"),
            "{head}"
        );
        assert!(head.contains("Cache-Control: no-cache"), "{head}");
        assert!(head.contains("Access-Control-Allow-Origin: *"), "{head}");
        assert_eq!(
            body,
            b"#EXTM3U
"
        );
        let segment = path.replace("live.m3u8", "seg00001.ts");
        let (status, head, body) = get(addr, &segment, None);
        assert_eq!(status, 200);
        assert!(head.contains("Content-Type: video/mp2t"), "{head}");
        assert_eq!(body.len(), 376);
        for bad in [
            "../secret.txt",
            "..%2Fsecret.txt",
            "",
            "a/b.ts",
            "missing.ts",
        ] {
            let (status, _, _) = get(addr, &path.replace("live.m3u8", bad), None);
            assert_eq!(status, 404, "{bad}");
        }
        let mut options = TcpStream::connect(addr).unwrap();
        write!(options, "OPTIONS {path} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut answer = String::new();
        options.read_to_string(&mut answer).unwrap();
        assert!(answer.starts_with("HTTP/1.1 204"), "{answer}");
        assert!(
            answer.contains("Access-Control-Allow-Origin: *"),
            "{answer}"
        );
        assert!(server.fetches(path) >= 2);
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
