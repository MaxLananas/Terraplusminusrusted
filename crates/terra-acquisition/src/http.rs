//! A minimal HTTP/1.1 GET client over `std::net` for `http://` URLs, and the transport trait a
//! TLS-capable client implements for `https://`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportLimits {
    /// Per connection attempt (each resolved address), within `total_timeout`.
    pub connect_timeout: Duration,
    /// Per socket read or write, within `total_timeout`.
    pub io_timeout: Duration,
    /// Hard deadline for the whole exchange: name resolution, connection, request and response.
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

/// Sent when no user agent is configured, in upstream's `createHttpUserAgent` shape
/// (`<plugin>/<version> (<library>/<version>; +<website>)`).
pub const DEFAULT_USER_AGENT: &str = concat!("Terraplusminus/", env!("CARGO_PKG_VERSION"), " (Terra--/rust; +https://github.com/MaxLananas/Terraplusminusrusted)");

pub trait HttpTransport: Send + Sync {
    fn get(&self, url: &str, limits: &TransportLimits) -> Result<HttpResponse, TransportError>;
}

/// Plain-text HTTP only; `https://` is refused rather than downgraded.
#[derive(Debug, Clone, Default)]
pub struct PlainHttpTransport {
    pub user_agent: String,
}

struct ParsedUrl<'a> {
    /// As written in the URL, brackets included for IPv6: what the `Host` header carries.
    host: &'a str,
    /// What to resolve or connect to: `host` without IPv6 brackets.
    address: &'a str,
    port: u16,
    path: &'a str,
}

/// `http://host[:port][/path]`, nothing else. Everything written into the request line or the
/// `Host` header is checked here, so a configured or redirected URL cannot smuggle a header
/// (CR, LF), split the request line (space) or address another host than it shows (`@`,
/// unbracketed IPv6, a fragment or query in the authority).
fn parse_http_url(url: &str) -> Result<ParsedUrl<'_>, TransportError> {
    let unsupported = || TransportError::UnsupportedUrl(url.to_owned());
    let rest = url.strip_prefix("http://").ok_or_else(unsupported)?;
    let (authority, path) = rest.find('/').map_or((rest, "/"), |index| (&rest[..index], &rest[index..]));
    if !path.bytes().all(|b| b.is_ascii_graphic()) || path.contains('#') {
        return Err(unsupported());
    }
    let (host, address, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        // IPv6 literal: `[addr]` or `[addr]:port`, and the address must really be IPv6.
        let (inner, after) = bracketed.split_once(']').ok_or_else(unsupported)?;
        if inner.parse::<std::net::Ipv6Addr>().is_err() {
            return Err(unsupported());
        }
        let port = match after {
            "" => None,
            _ => Some(after.strip_prefix(':').ok_or_else(unsupported)?),
        };
        (&authority[..inner.len() + 2], inner, port)
    } else {
        let (host, port) = authority.split_once(':').map_or((authority, None), |(host, port)| (host, Some(port)));
        // A registered name or IPv4 literal: letters, digits, `-`, `.` and `_` only.
        if host.is_empty() || !host.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_')) {
            return Err(unsupported());
        }
        (host, host, port)
    };
    let port = match port {
        None => 80,
        Some(digits) if !digits.is_empty() && digits.len() <= 5 && digits.bytes().all(|b| b.is_ascii_digit()) => match digits.parse::<u16>() {
            Ok(0) | Err(_) => return Err(unsupported()),
            Ok(port) => port,
        },
        Some(_) => return Err(unsupported()),
    };
    Ok(ParsedUrl { host, address, port, path })
}

/// The `User-Agent` actually sent: the configured one when it is a valid header value (visible
/// ASCII and spaces, at most 256 bytes), else the default. The configuration already refuses
/// bad values; this keeps a transport built directly from code from writing a header break.
pub(crate) fn effective_user_agent(configured: &str) -> &str {
    let valid = !configured.trim().is_empty() && configured.len() <= 256 && configured.bytes().all(|b| b == b' ' || b.is_ascii_graphic());
    if valid { configured } else { DEFAULT_USER_AGENT }
}

/// Time left before `deadline`, or `Timeout` when none is.
fn remaining(deadline: Instant) -> Result<Duration, TransportError> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() { Err(TransportError::Timeout) } else { Ok(left) }
}

/// Resolves `host` within the deadline. The system resolver cannot be interrupted, so it runs on
/// a helper thread that is abandoned (and ends on its own) when the deadline passes first.
fn resolve(host: &str, port: u16, deadline: Instant) -> Result<Vec<SocketAddr>, TransportError> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let (sender, receiver) = mpsc::channel();
    let name = host.to_owned();
    std::thread::Builder::new()
        .name("terra-dns".into())
        .spawn(move || {
            let result = (name.as_str(), port).to_socket_addrs().map(Iterator::collect::<Vec<_>>).map_err(|error| error.to_string());
            let _ = sender.send(result);
        })
        .map_err(|error| TransportError::Connect(format!("cannot start name resolution: {error}")))?;
    match receiver.recv_timeout(remaining(deadline)?) {
        Ok(result) => result.map_err(TransportError::Connect),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(TransportError::Timeout),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(TransportError::Connect("name resolution failed".into())),
    }
}

/// A socket whose every read and write waits at most `min(io_timeout, time left)`, so the
/// exchange cannot outlive the deadline by more than one scheduling delay.
struct DeadlineStream {
    inner: TcpStream,
    deadline: Instant,
    io_timeout: Duration,
}

impl DeadlineStream {
    fn arm(&self, write: bool) -> std::io::Result<()> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "deadline exceeded"));
        }
        let timeout = Some(left.min(self.io_timeout));
        if write { self.inner.set_write_timeout(timeout) } else { self.inner.set_read_timeout(timeout) }
    }
}

impl Read for DeadlineStream {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.arm(false)?;
        self.inner.read(buffer)
    }
}

impl Write for DeadlineStream {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.arm(true)?;
        self.inner.write(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
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
        let addresses = resolve(parsed.address, parsed.port, deadline)?;
        let mut last_error = TransportError::Connect(format!("no address for {}", parsed.host));
        let mut stream = None;
        for address in addresses {
            let timeout = remaining(deadline)?.min(limits.connect_timeout);
            match TcpStream::connect_timeout(&address, timeout) {
                Ok(connected) => {
                    stream = Some(connected);
                    break;
                }
                Err(error) => last_error = TransportError::Connect(error.to_string()),
            }
        }
        let mut stream = DeadlineStream { inner: stream.ok_or(last_error)?, deadline, io_timeout: limits.io_timeout };
        let agent = effective_user_agent(&self.user_agent);
        let request = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: {agent}\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n",
            parsed.path,
            if parsed.port == 80 { parsed.host.to_owned() } else { format!("{}:{}", parsed.host, parsed.port) }
        );
        stream.write_all(request.as_bytes()).map_err(io_error)?;
        let mut reader = BufReader::new(stream);
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
    // Checked: a declared size near u64::MAX must not wrap past the limit.
    if (out.len() as u64).checked_add(length).is_none_or(|total| total > limit) {
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
        // Each chunk-size line and terminator gets its own small budget (the chunk count is
        // already bounded by the body limit, every chunk carrying at least one byte); the
        // trailer section shares one.
        const CHUNK_LINE_BYTES: usize = 4096;
        loop {
            let mut budget = CHUNK_LINE_BYTES;
            let size_line = read_line(reader, &mut budget)?;
            let size_text = size_line.split(';').next().unwrap_or("").trim();
            let size = u64::from_str_radix(size_text, 16).map_err(|_| TransportError::Protocol(format!("bad chunk size {size_text:?}")))?;
            if size == 0 {
                let mut trailer_budget = 1 << 16;
                while !read_line(reader, &mut trailer_budget)?.is_empty() {}
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
    reader.take(limit.saturating_add(1)).read_to_end(&mut body).map_err(io_error)?;
    if body.len() as u64 > limit {
        return Err(TransportError::BodyTooLarge { limit });
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hostile servers: mutated responses (status lines, header sizes, chunk sizes, lengths,
    /// truncation) must give a response or an error within the limits, never a panic or a body
    /// over the limit, and curl's output parser likewise.
    #[test]
    fn mutated_responses_never_panic_or_exceed_limits() {
        let seeds: [&[u8]; 5] = [
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nRetry-After: 3\r\n\r\nhello",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;x=y\r\nde\r\n0\r\nTrailer: 1\r\n\r\n",
            b"HTTP/1.0 404 Not Found\r\n\r\n",
            b"HTTP/1.1 503 Busy\r\nRetry-After: 120\r\nContent-Length: 0\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Encoding: identity\r\n\r\nuntil the end",
        ];
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..30_000 {
            let mut bytes = seeds[case % seeds.len()].to_vec();
            for _ in 0..1 + next() % 6 {
                let at = (next() as usize) % bytes.len().max(1);
                match next() % 6 {
                    0 if !bytes.is_empty() => bytes[at] ^= 1 << (next() % 8),
                    1 if !bytes.is_empty() => bytes[at] = b"0f9\r\n: ;-x"[(next() % 10) as usize],
                    2 if !bytes.is_empty() => {
                        bytes.remove(at);
                    }
                    3 => bytes.insert(at.min(bytes.len()), next() as u8),
                    4 => bytes.truncate(at),
                    _ => {
                        let digits = format!("{:x}", next() >> (next() % 64));
                        bytes.splice(at.min(bytes.len())..at.min(bytes.len()), digits.bytes());
                    }
                }
            }
            let mut reader = bytes.as_slice();
            if let Ok((_, headers)) = read_head(&mut reader, 256)
                && let Ok(body) = read_body(&mut reader, &headers, 64)
            {
                assert!(body.len() <= 64, "case {case}: {} bytes", body.len());
            }
            // The same bytes as curl output with an arbitrary size trailer.
            let mut curl = bytes.clone();
            curl.extend_from_slice(format!("\n{} {}", next() % 200, next() % 200).as_bytes());
            let _ = crate::external::parse_curl_output(&curl, 256);
        }
    }

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
    fn hostile_authorities_are_refused() {
        let ipv6 = parse_http_url("http://[::1]:8080/t.png").unwrap();
        assert_eq!((ipv6.host, ipv6.address, ipv6.port, ipv6.path), ("[::1]", "::1", 8080, "/t.png"));
        let ipv6 = parse_http_url("http://[2001:db8::7]").unwrap();
        assert_eq!((ipv6.host, ipv6.address, ipv6.port), ("[2001:db8::7]", "2001:db8::7", 80));
        assert_eq!(parse_http_url("http://tiles.example-1.org:81/a?x=1").unwrap().port, 81);
        for url in [
            "http://host\r\nX-Injected: 1/x",
            "http://host\n/x",
            "http://ho st/x",
            "http://host:80\r\n/x",
            "http://host/x\r\nX: y",
            "http://host/x\ty",
            "http://host/x#frag",
            "http://host/caf\u{e9}",
            "http://h\u{f6}st/x",
            "http://host?x=1",
            "http://host#x",
            "http://::1/x",
            "http://[::1/x",
            "http://[::1]x/x",
            "http://[::1]:/x",
            "http://[example.org]/x",
            "http://host:/x",
            "http://host:0/x",
            "http://host:65536/x",
            "http://host:+80/x",
            "http://host:080080/x",
            "http://host:1:2/x",
            "http://:80/x",
            "http://a@b/x",
        ] {
            assert!(parse_http_url(url).is_err(), "{url:?}");
        }
    }

    #[test]
    fn user_agents_that_would_break_the_header_fall_back_to_the_default() {
        assert_eq!(effective_user_agent("Agent/1 (+https://x)"), "Agent/1 (+https://x)");
        for bad in ["", "   ", "a\r\nX-Injected: 1", "a\nb", "tab\there", "caf\u{e9}", &"x".repeat(257)] {
            assert_eq!(effective_user_agent(bad), DEFAULT_USER_AGENT, "{bad:?}");
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

    /// A declared size near `u64::MAX` used to overflow the limit check: a panic with overflow
    /// checks, and without them a wrapped sum that passed the check and read the rest of the
    /// stream unbounded.
    #[test]
    fn huge_declared_sizes_are_refused_not_wrapped() {
        for raw in [
            "HTTP/1.1 200 OK\r\nContent-Length: 18446744073709551615\r\n\r\nabc".to_owned(),
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\nffffffffffffffff\r\nxyz".to_owned(),
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\nfffffffffffffffe\r\nxyz".to_owned(),
        ] {
            let mut reader = raw.as_bytes();
            let (_, headers) = read_head(&mut reader, 1024).unwrap();
            assert_eq!(read_body(&mut reader, &headers, 1 << 20), Err(TransportError::BodyTooLarge { limit: 1 << 20 }), "{raw:?}");
        }
        let mut reader = &b"HTTP/1.1 200 OK\r\n\r\nabc"[..];
        let (_, headers) = read_head(&mut reader, 1024).unwrap();
        assert_eq!(read_body(&mut reader, &headers, u64::MAX).unwrap(), b"abc");
    }

    /// The chunk-size lines of a body used to share one 64 KiB budget, so a valid response sent
    /// in many small chunks failed as "header too large".
    #[test]
    fn many_small_chunks_are_accepted() {
        let mut raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        for _ in 0..20_000 {
            raw.extend_from_slice(b"1;name=value\r\nx\r\n");
        }
        raw.extend_from_slice(b"0\r\nX-Trailer: 1\r\n\r\n");
        let mut reader = &raw[..];
        let (_, headers) = read_head(&mut reader, 1024).unwrap();
        assert_eq!(read_body(&mut reader, &headers, 1 << 20).unwrap().len(), 20_000);
        // Still bounded: an endless chunk-size line or trailer section fails.
        let mut endless = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        endless.extend(std::iter::repeat_n(b'0', 100_000));
        let mut reader = &endless[..];
        let (_, headers) = read_head(&mut reader, 1024).unwrap();
        assert!(read_body(&mut reader, &headers, 1 << 20).is_err());
        let mut trailers = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n".to_vec();
        for _ in 0..10_000 {
            trailers.extend_from_slice(b"X-Trailer: 1234567890\r\n");
        }
        let mut reader = &trailers[..];
        let (_, headers) = read_head(&mut reader, 1024).unwrap();
        assert!(read_body(&mut reader, &headers, 1 << 20).is_err());
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
