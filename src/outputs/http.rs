//! Just enough HTTP/1.1 for Sonos: a client for SOAP calls and request parsing for the media
//! server. Every exchange is one request per connection (`Connection: close`).

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

/// Speakers answer within milliseconds on a home network.
pub const CONNECT_TIMEOUT: Duration = Duration::from_millis(1500);
pub const IO_TIMEOUT: Duration = Duration::from_secs(3);
/// A large household's topology is some 100 KB; nothing we ask for comes close to this.
const MAX_BODY: u64 = 4 << 20;
/// Request and response heads are small.
const MAX_HEAD: usize = 16 << 10;

#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub body: String,
}

/// Sends a request to `addr` and reads the whole response.
pub fn request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> io::Result<Response> {
    let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    read_response(BufReader::new(stream))
}

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// Reads lines up to the blank line that ends a head. Returns the first line and the headers.
pub fn read_head(reader: &mut impl BufRead) -> io::Result<(String, Vec<(String, String)>)> {
    let mut first = String::new();
    let mut headers = Vec::new();
    let mut total = 0;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        total += n;
        if total > MAX_HEAD {
            return Err(bad("HTTP head too long"));
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if first.is_empty() {
            if line.is_empty() {
                continue;
            }
            first = line.to_string();
        } else if line.is_empty() {
            return Ok((first, headers));
        } else if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
}

/// The value of header `name` (lowercase).
pub fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
}

fn read_response(mut reader: impl BufRead) -> io::Result<Response> {
    let (status_line, headers) = read_head(&mut reader)?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| bad(format!("bad status line \"{status_line}\"")))?;
    let mut body = Vec::new();
    let chunked = header(&headers, "transfer-encoding")
        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
    if chunked {
        loop {
            let mut size = String::new();
            reader.read_line(&mut size)?;
            let size = size.trim().split(';').next().unwrap_or("");
            let size = u64::from_str_radix(size, 16).map_err(|_| bad("bad chunk size"))?;
            if size == 0 {
                break;
            }
            if body.len() as u64 + size > MAX_BODY {
                return Err(bad("response too large"));
            }
            (&mut reader).take(size).read_to_end(&mut body)?;
            let mut crlf = String::new();
            reader.read_line(&mut crlf)?;
        }
    } else if let Some(len) = header(&headers, "content-length") {
        let len: u64 = len.parse().map_err(|_| bad("bad content length"))?;
        if len > MAX_BODY {
            return Err(bad("response too large"));
        }
        reader.take(len).read_to_end(&mut body)?;
    } else {
        reader.take(MAX_BODY).read_to_end(&mut body)?;
    }
    Ok(Response {
        status,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

/// Escapes text for XML content and attribute values.
pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_plain_and_chunked_responses() {
        let plain = "HTTP/1.1 200 OK\r\nCONTENT-LENGTH: 5\r\nEXT:\r\n\r\nhello and more";
        let r = read_response(plain.as_bytes()).unwrap();
        assert_eq!((r.status, r.body.as_str()), (200, "hello"));

        let chunked = "HTTP/1.1 500 Internal Server Error\r\nTransfer-Encoding: chunked\r\n\r\n\
                       4\r\nfaul\r\n2;x=y\r\nt!\r\n0\r\n\r\n";
        let r = read_response(chunked.as_bytes()).unwrap();
        assert_eq!((r.status, r.body.as_str()), (500, "fault!"));

        let until_close = "HTTP/1.0 404 Not Found\r\n\r\nnope";
        let r = read_response(until_close.as_bytes()).unwrap();
        assert_eq!((r.status, r.body.as_str()), (404, "nope"));
    }

    #[test]
    fn escapes_xml() {
        assert_eq!(
            xml_escape(r#"Tom & Jerry's <"Song">"#),
            "Tom &amp; Jerry&apos;s &lt;&quot;Song&quot;&gt;"
        );
    }
}
