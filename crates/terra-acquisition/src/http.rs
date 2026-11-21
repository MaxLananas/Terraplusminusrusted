//! A minimal HTTP/1.1 GET client over `std::net` for `http://` URLs, and the transport trait a
//! TLS-capable client implements for `https://`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportLimits {
    pub connect_timeout: Duration,
    /// Per-read socket timeout.
    pub io_timeout: Duration,
    /// Deadline for the whole exchange.
    pub total_timeout: Duration,
    pub max_body_bytes: u64,
    pub max_header_bytes: usize,
}

impl Default for TransportLimits {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(5),
            io_timeout: Duration::from_secs(10),
            total_timeout: Duration::from_secs(30),
            max_body_bytes: terra_elevation::provider::MAX_TILE_BYTES,
            max_header_bytes: 64 << 10,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    /// Lower-cased names.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    UnsupportedUrl(String),
    Connect(String),
    Timeout,
    Io(String),
    Protocol(String),
    BodyTooLarge { limit: u64 },
}

impl TransportError {
    /// Whether a later attempt may succeed.
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Connect(_) | Self::Timeout | Self::Io(_))
    }
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedUrl(url) => write!(f, "unsupported URL {url}"),
            Self::Connect(reason) => write!(f, "connect failed: {reason}"),
            Self::Timeout => write!(f, "timed out"),
            Self::Io(reason) => write!(f, "I/O error: {reason}"),
            Self::Protocol(reason) => write!(f, "malformed response: {reason}"),
            Self::BodyTooLarge { limit } => write!(f, "response body exceeds {limit} bytes"),
        }
    }
}

pub trait HttpTransport: Send + Sync {
    fn get(&self, url: &str, limits: &TransportLimits) -> Result<HttpResponse, TransportError>;
}

/// Plain-text HTTP only; `https://` is refused rather than downgraded.
#[derive(Debug, Clone, Default)]
pub struct PlainHttpTransport {
    pub user_agent: String,
}

struct ParsedUrl<'a> {
    host: &'a str,
    port: u16,
    path: &'a str,
}

fn parse_http_url(url: &str) -> Result<ParsedUrl<'_>, TransportError> {
    let unsupported = || TransportError::UnsupportedUrl(url.to_owned());
    let rest = url.strip_prefix("http://").ok_or_else(unsupported)?;
    let (authority, path) = rest.find('/').map_or((rest, "/"), |index| (&rest[..index], &rest[index..]));
    if authority.is_empty() || authority.contains('@') || path.bytes().any(|b| b <= b' ' || b == 0x7F) {
        return Err(unsupported());
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !host.contains(']') || host.ends_with(']') => (host, port.parse().map_err(|_| unsupported())?),
        _ => (authority, 80),
    };
    Ok(ParsedUrl { host, port, path })
}

struct DeadlineReader<R> {
    inner: R,
    deadline: Instant,
}

impl<R: Read> Read for DeadlineReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if Instant::now() > self.deadline {
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "deadline exceeded"));
        }
        self.inner.read(buffer)
    }
}

fn io_error(error: std::io::Error) -> TransportError {
    match error.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => TransportError::Timeout,
        _ => TransportError::Io(error.to_string()),
    }
}

impl HttpTransport for PlainHttpTransport {
    fn get(&self, url: &str, limits: &TransportLimits) -> Result<HttpResponse, TransportError> {
        let parsed = parse_http_url(url)?;
        let deadline = Instant::now() + limits.total_timeout;
        let addresses = (parsed.host.trim_start_matches('[').trim_end_matches(']'), parsed.port)
            .to_socket_addrs()
            .map_err(|error| TransportError::Connect(error.to_string()))?;
        let mut last_error = TransportError::Connect(format!("no address for {}", parsed.host));
        let mut stream = None;
        for address in addresses {
            match TcpStream::connect_timeout(&address, limits.connect_timeout) {
                Ok(connected) => {
                    stream = Some(connected);
                    break;
                }
                Err(error) => last_error = TransportError::Connect(error.to_string()),
            }
        }
        let mut stream = stream.ok_or(last_error)?;
        stream.set_read_timeout(Some(limits.io_timeout)).map_err(io_error)?;
        stream.set_write_timeout(Some(limits.io_timeout)).map_err(io_error)?;
        let agent = if self.user_agent.is_empty() { "terraplusminus-rust" } else { &self.user_agent };
        let request = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: {agent}\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n",
            parsed.path,
            if parsed.port == 80 { parsed.host.to_owned() } else { format!("{}:{}", parsed.host, parsed.port) }
        );
        stream.write_all(request.as_bytes()).map_err(io_error)?;
        let mut reader = BufReader::new(DeadlineReader { inner: stream, deadline });
        let (status, headers) = read_head(&mut reader, limits.max_header_bytes)?;
        let body = read_body(&mut reader, &headers, limits.max_body_bytes)?;
        Ok(HttpResponse { status, headers, body })
    }
}

fn read_line(reader: &mut impl BufRead, budget: &mut usize) -> Result<String, TransportError> {
    let mut line = Vec::new();
    let read = reader.take(*budget as u64 + 1).read_until(b'\n', &mut line).map_err(io_error)?;
    if read == 0 {
        return Err(TransportError::Protocol("connection closed in header".into()));
    }
    if read > *budget {
        return Err(TransportError::Protocol("header too large".into()));
    }
    *budget -= read;
    if line.last() != Some(&b'\n') {
        return Err(TransportError::Protocol("truncated header line".into()));
    }
    while matches!(line.last(), Some(b'\n' | b'\r')) {
        line.pop();
    }
    String::from_utf8(line).map_err(|_| TransportError::Protocol("non-UTF-8 header".into()))
}

fn read_head(reader: &mut impl BufRead, max_bytes: usize) -> Result<(u16, Vec<(String, String)>), TransportError> {
    let mut budget = max_bytes;
    let status_line = read_line(reader, &mut budget)?;
    let mut parts = status_line.splitn(3, ' ');
    let (Some(version), Some(code)) = (parts.next(), parts.next()) else {
        return Err(TransportError::Protocol(format!("bad status line {status_line:?}")));
    };
    if !version.starts_with("HTTP/1.") || code.len() != 3 {
        return Err(TransportError::Protocol(format!("bad status line {status_line:?}")));
    }
    let status = code.parse().map_err(|_| TransportError::Protocol(format!("bad status code {code:?}")))?;
    let mut headers = Vec::new();
    loop {
        let line = read_line(reader, &mut budget)?;
        if line.is_empty() {
            return Ok((status, headers));
        }
        let (name, value) = line.split_once(':').ok_or_else(|| TransportError::Protocol(format!("bad header {line:?}")))?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
    }
}

fn read_limited(reader: &mut impl Read, length: u64, limit: u64, out: &mut Vec<u8>) -> Result<(), TransportError> {
    if out.len() as u64 + length > limit {
        return Err(TransportError::BodyTooLarge { limit });
    }
    let before = out.len();
    reader.take(length).read_to_end(out).map_err(io_error)?;
    if (out.len() - before) as u64 != length {
        return Err(TransportError::Protocol("body shorter than declared".into()));
    }
    Ok(())
}

fn read_body(reader: &mut impl BufRead, headers: &[(String, String)], limit: u64) -> Result<Vec<u8>, TransportError> {
    let header = |name: &str| headers.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str());
    let mut body = Vec::new();
    if let Some(encoding) = header("content-encoding").filter(|encoding| !encoding.eq_ignore_ascii_case("identity")) {
        return Err(TransportError::Protocol(format!("unrequested content-encoding {encoding}")));
    }
    if header("transfer-encoding").is_some_and(|value| value.to_ascii_lowercase().contains("chunked")) {
        let mut budget = 1 << 16;
        loop {
            let size_line = read_line(reader, &mut budget)?;
            let size_text = size_line.split(';').next().unwrap_or("").trim();
            let size = u64::from_str_radix(size_text, 16).map_err(|_| TransportError::Protocol(format!("bad chunk size {size_text:?}")))?;
            if size == 0 {
                while !read_line(reader, &mut budget)?.is_empty() {}
                return Ok(body);
            }
            read_limited(reader, size, limit, &mut body)?;
            if !read_line(reader, &mut budget)?.is_empty() {
                return Err(TransportError::Protocol("missing chunk terminator".into()));
            }
        }
    }
    if let Some(length) = header("content-length") {
        let length: u64 = length.parse().map_err(|_| TransportError::Protocol(format!("bad content-length {length:?}")))?;
        read_limited(reader, length, limit, &mut body)?;
        return Ok(body);
    }
    reader.take(limit + 1).read_to_end(&mut body).map_err(io_error)?;
    if body.len() as u64 > limit {
        return Err(TransportError::BodyTooLarge { limit });
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_urls_and_refuses_others() {
        let parsed = parse_http_url("http://127.0.0.1:8080/a/b.png").unwrap();
        assert_eq!((parsed.host, parsed.port, parsed.path), ("127.0.0.1", 8080, "/a/b.png"));
        assert_eq!(parse_http_url("http://example.org").unwrap().path, "/");
        for url in ["https://example.org/x", "http://user@host/x", "http://host/a b", "ftp://x", "http:///x"] {
            assert!(parse_http_url(url).is_err(), "{url}");
        }
    }

    #[test]
    fn decodes_chunked_bodies_within_limits() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;x=y\r\nde\r\n0\r\n\r\n";
        let mut reader = &raw[..];
        let (status, headers) = read_head(&mut reader, 1024).unwrap();
        assert_eq!(status, 200);
        assert_eq!(read_body(&mut reader, &headers, 10).unwrap(), b"abcde");
        let mut reader = &raw[..];
        let (_, headers) = read_head(&mut reader, 1024).unwrap();
        assert_eq!(read_body(&mut reader, &headers, 4), Err(TransportError::BodyTooLarge { limit: 4 }));
    }

    #[test]
    fn rejects_oversized_or_truncated_responses() {
        let mut reader = &b"HTTP/1.1 200 OK\r\nContent-Length: 999999999\r\n\r\nabc"[..];
        let (_, headers) = read_head(&mut reader, 1024).unwrap();
        assert!(matches!(read_body(&mut reader, &headers, 1 << 20), Err(TransportError::BodyTooLarge { .. })));
        let mut reader = &b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc"[..];
        let (_, headers) = read_head(&mut reader, 1024).unwrap();
        assert!(matches!(read_body(&mut reader, &headers, 1 << 20), Err(TransportError::Protocol(_))));
        let long_header = format!("HTTP/1.1 200 OK\r\nX: {}\r\n\r\n", "a".repeat(2000));
        assert!(read_head(&mut long_header.as_bytes(), 1024).is_err());
        let mut gzip = &b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 0\r\n\r\n"[..];
        let (_, headers) = read_head(&mut gzip, 1024).unwrap();
        assert!(read_body(&mut gzip, &headers, 10).is_err());
    }
}
