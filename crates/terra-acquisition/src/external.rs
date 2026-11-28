//! `https://` through the system's `curl`, so the default TerraMinusMinus sources (AWS terrain
//! tiles, cloud.daporkchop.net, Overpass) work without a TLS stack in the binary and without
//! `plain_http_hosts`. Certificates are verified by curl against the system trust store.
//!
//! Each request is one `curl` process with hard limits (`--max-time`, `--max-filesize`,
//! `--proto`), no shell, the URL after `--`, and its response headers parsed from
//! `--dump-header`, so status codes and `Retry-After` work as with the plain transport. curl's
//! own limits are not trusted alone: the process runs under [`run_bounded`], which kills it past
//! the deadline or past the size budget (`--max-filesize` cannot stop a chunked response before
//! curl 8.4). `-q` comes first so no `.curlrc` can turn certificate checks or limits off.
//!
//! Headers and body are told apart by the sizes curl reports itself (`--write-out`), never by
//! looking for `HTTP/` in the stream, so a body starting with `HTTP/` cannot pass for headers.

use crate::http::{HttpResponse, HttpTransport, PlainHttpTransport, TransportError, TransportLimits};
use crate::subprocess::{ProcessError, ProcessLimits, run_bounded};
use std::process::Command;
use std::time::Duration;

/// Header blocks curl may print for one request: up to five redirects, the final answer and
/// informational (`100 Continue`) or proxy `CONNECT` answers in between.
const MAX_HEADER_BLOCKS: usize = 16;
/// What curl prints after the body (`--write-out`): `\n<header bytes> <body bytes>`.
const WRITE_OUT: &str = "\n%{size_header} %{size_download}";
/// Extra time past `--max-time` before the process is killed (startup, TLS teardown).
const KILL_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurlTransport {
    pub program: String,
    pub user_agent: String,
}

impl CurlTransport {
    /// The transport when `program --version` runs and supports HTTPS; `None` otherwise.
    pub fn probe(program: &str, user_agent: &str) -> Option<Self> {
        let limits = ProcessLimits { timeout: Duration::from_secs(10), max_stdout: 64 << 10, max_stderr: 4 << 10 };
        let output = run_bounded(Command::new(program).arg("-q").arg("--version"), &limits).ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        let https = text.lines().any(|line| line.starts_with("Protocols:") && line.split_whitespace().any(|protocol| protocol == "https"));
        (output.status.success() && https).then(|| Self { program: program.to_owned(), user_agent: user_agent.to_owned() })
    }

    fn arguments(&self, url: &str, limits: &TransportLimits) -> Vec<String> {
        let seconds = |duration: std::time::Duration| format!("{:.3}", duration.as_secs_f64().max(0.001));
        vec![
            // Must be first: ignore any .curlrc.
            "-q".into(),
            "--silent".into(),
            "--show-error".into(),
            "--globoff".into(),
            "--location".into(),
            "--max-redirs".into(),
            "5".into(),
            "--proto".into(),
            "=https".into(),
            "--proto-redir".into(),
            "=https".into(),
            "--connect-timeout".into(),
            seconds(limits.connect_timeout.min(limits.total_timeout)),
            "--max-time".into(),
            seconds(limits.total_timeout),
            "--max-filesize".into(),
            limits.max_body_bytes.to_string(),
            "--user-agent".into(),
            crate::http::effective_user_agent(&self.user_agent).to_owned(),
            "--header".into(),
            "Accept-Encoding: identity".into(),
            "--dump-header".into(),
            "-".into(),
            "--output".into(),
            "-".into(),
            "--write-out".into(),
            WRITE_OUT.into(),
            "--".into(),
            url.to_owned(),
        ]
    }
}

/// Reads one request's output: the header blocks curl dumped (`--dump-header -`, one per
/// response, redirects first), the body, then the `--write-out` trailer giving both sizes. The
/// last header block is the response.
pub fn parse_curl_output(output: &[u8], max_header_bytes: usize) -> Result<HttpResponse, TransportError> {
    let malformed = |reason: &str| TransportError::Protocol(format!("curl output: {reason}"));
    let newline = output.iter().rposition(|&byte| byte == b'\n').ok_or_else(|| malformed("no size trailer"))?;
    let trailer = std::str::from_utf8(&output[newline + 1..]).map_err(|_| malformed("bad size trailer"))?;
    let (header_size, body_size) = trailer.split_once(' ').ok_or_else(|| malformed("bad size trailer"))?;
    let parse = |text: &str| text.parse::<usize>().map_err(|_| malformed("bad size trailer"));
    let (header_size, body_size) = (parse(header_size)?, parse(body_size)?);
    if header_size.checked_add(body_size) != Some(newline) {
        return Err(malformed("sizes do not add up"));
    }
    let (mut headers, body) = output[..newline].split_at(header_size);
    let mut last = None;
    for _ in 0..MAX_HEADER_BLOCKS {
        if headers.is_empty() {
            break;
        }
        if !headers.starts_with(b"HTTP/") {
            return Err(malformed("header block without a status line"));
        }
        let end = headers.windows(4).take(max_header_bytes.saturating_add(4)).position(|window| window == b"\r\n\r\n");
        let end = end.ok_or_else(|| TransportError::Protocol(format!("headers exceed {max_header_bytes} bytes or are unterminated")))?;
        last = Some(&headers[..end]);
        headers = &headers[end + 4..];
    }
    if !headers.is_empty() {
        return Err(malformed("too many header blocks"));
    }
    let block = std::str::from_utf8(last.ok_or_else(|| malformed("no status line"))?).map_err(|_| malformed("headers are not UTF-8"))?;
    let mut lines = block.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .filter(|code| code.len() == 3)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| TransportError::Protocol(format!("bad status line `{}`", status_line.escape_debug())))?;
    let headers = lines.filter_map(|line| line.split_once(':')).map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned())).collect();
    Ok(HttpResponse { status, headers, body: body.to_vec() })
}

impl HttpTransport for CurlTransport {
    fn get(&self, url: &str, limits: &TransportLimits) -> Result<HttpResponse, TransportError> {
        if !url.starts_with("https://") || url.bytes().any(|b| b <= b' ' || b == 0x7F) {
            return Err(TransportError::UnsupportedUrl(url.to_owned()));
        }
        let budget = ProcessLimits {
            timeout: limits.total_timeout.saturating_add(KILL_GRACE),
            // The body, every header block and the trailer.
            max_stdout: limits.max_body_bytes.saturating_add((limits.max_header_bytes as u64 + 4).saturating_mul(MAX_HEADER_BLOCKS as u64)).saturating_add(64),
            max_stderr: 4 << 10,
        };
        let mut command = Command::new(&self.program);
        command.args(self.arguments(url, limits)).env_remove("CURL_HOME");
        let output = run_bounded(&mut command, &budget).map_err(|error| match error {
            ProcessError::Timeout => TransportError::Timeout,
            ProcessError::OutputTooLarge { .. } => TransportError::BodyTooLarge { limit: limits.max_body_bytes },
            ProcessError::Spawn(reason) | ProcessError::Io(reason) => TransportError::Connect(format!("{}: {reason}", self.program)),
        })?;
        if !output.status.success() {
            let reason = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            // curl's exit codes: 6/7 resolve/connect, 28 timeout, 63 size limit, 35/60 TLS.
            return Err(match output.status.code() {
                Some(28) => TransportError::Timeout,
                Some(63) => TransportError::BodyTooLarge { limit: limits.max_body_bytes },
                Some(5..=7) => TransportError::Connect(reason),
                Some(1 | 3) => TransportError::UnsupportedUrl(url.to_owned()),
                _ => TransportError::Io(format!("curl {}: {reason}", output.status)),
            });
        }
        let response = parse_curl_output(&output.stdout, limits.max_header_bytes)?;
        if response.body.len() as u64 > limits.max_body_bytes {
            return Err(TransportError::BodyTooLarge { limit: limits.max_body_bytes });
        }
        Ok(response)
    }
}

/// `http://` over the built-in client, `https://` over curl when one was found.
#[derive(Debug, Clone, Default)]
pub struct DualTransport {
    pub plain: PlainHttpTransport,
    pub https: Option<CurlTransport>,
}

impl DualTransport {
    pub fn supports_https(&self) -> bool {
        self.https.is_some()
    }
}

impl HttpTransport for DualTransport {
    fn get(&self, url: &str, limits: &TransportLimits) -> Result<HttpResponse, TransportError> {
        match (&self.https, url.starts_with("https://")) {
            (Some(https), true) => https.get(url, limits),
            _ => self.plain.get(url, limits),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What curl prints: headers, body, then `\n<header bytes> <body bytes>`.
    fn curl_output(headers: &[u8], body: &[u8]) -> Vec<u8> {
        let mut out = [headers, body].concat();
        out.extend_from_slice(format!("\n{} {}", headers.len(), body.len()).as_bytes());
        out
    }

    #[test]
    fn dumped_headers_are_split_from_the_body() {
        let headers = b"HTTP/1.1 301 Moved Permanently\r\nLocation: https://b/\r\n\r\nHTTP/2 200\r\ncontent-type: image/png\r\nRetry-After: 3\r\n\r\n";
        let response = parse_curl_output(&curl_output(headers, b"\x89PNG\r\n\r\nrest"), 1 << 16).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.header("retry-after"), Some("3"));
        assert_eq!(response.header("content-type"), Some("image/png"));
        assert_eq!(response.body, b"\x89PNG\r\n\r\nrest");
        let missing = parse_curl_output(&curl_output(b"HTTP/1.1 404 Not Found\r\n\r\n", b""), 1 << 16).unwrap();
        assert_eq!((missing.status, missing.body.len()), (404, 0));
        for broken in [&b""[..], b"garbage", b"HTTP/1.1 200 OK\r\n\n17 0", b"HTTP/1.1 abc\r\n\r\n\n16 0", b"HTTP/1.1 200 OK\r\n\r\n\n19 5", b"\n0 0"] {
            assert!(parse_curl_output(broken, 1 << 16).is_err(), "{broken:?}");
        }
        assert!(parse_curl_output(&curl_output(b"HTTP/1.1 200 OK\r\nX: yyyyyyyyyyyy\r\n\r\n", b""), 8).is_err());
    }

    /// The old parser looked for `HTTP/` after each header block, so a body that starts with it
    /// (a text file, an error page) was read as one more header block.
    #[test]
    fn a_body_that_looks_like_headers_stays_the_body() {
        let body = b"HTTP/1.1 500 Fake\r\nX-Evil: 1\r\n\r\npayload";
        let response = parse_curl_output(&curl_output(b"HTTP/1.1 200 OK\r\n\r\n", body), 1 << 16).unwrap();
        assert_eq!((response.status, response.header("x-evil"), response.body.as_slice()), (200, None, &body[..]));
        let flood = b"HTTP/1.1 100 Continue\r\n\r\n".repeat(MAX_HEADER_BLOCKS + 1);
        assert!(parse_curl_output(&curl_output(&flood, b""), 1 << 16).is_err());
    }

    #[test]
    fn arguments_are_bounded_and_end_with_the_url() {
        let curl = CurlTransport { program: "curl".into(), user_agent: "Agent/1".into() };
        let arguments = curl.arguments("https://a/b?c=${x}", &TransportLimits::default());
        assert_eq!(arguments.last().map(String::as_str), Some("https://a/b?c=${x}"));
        assert_eq!(arguments[arguments.len() - 2], "--");
        assert_eq!(arguments[0], "-q", "a .curlrc must not apply");
        for flag in ["--max-time", "--max-filesize", "--connect-timeout", "--proto", "--globoff"] {
            assert!(arguments.iter().any(|argument| argument == flag), "{flag}");
        }
        assert!(arguments.windows(2).any(|pair| pair[0] == "--user-agent" && pair[1] == "Agent/1"));
        assert!(curl.get("http://a/", &TransportLimits::default()).is_err(), "plain HTTP is not curl's job");
        assert!(curl.get("https://a/ b", &TransportLimits::default()).is_err());
    }

    #[test]
    fn a_missing_program_is_not_a_transport() {
        assert_eq!(CurlTransport::probe("/nonexistent/curl-for-tests", "x"), None);
        let dual = DualTransport::default();
        assert!(!dual.supports_https());
        assert!(matches!(dual.get("https://a/", &TransportLimits::default()), Err(TransportError::UnsupportedUrl(_))));
    }

    /// Needs the network and curl: `cargo test -p terra-acquisition -- --ignored live_`.
    #[test]
    #[ignore]
    fn live_https_through_curl() {
        let curl = CurlTransport::probe("curl", crate::http::DEFAULT_USER_AGENT).expect("curl with HTTPS");
        let response = curl.get("https://api.github.com/repos/BTE-Germany/TerraPlusMinus/releases/latest", &TransportLimits::default()).unwrap();
        assert_eq!(response.status, 200);
        assert!(String::from_utf8_lossy(&response.body).contains("tag_name"));
        let missing = curl.get("https://api.github.com/repos/BTE-Germany/TerraPlusMinus/releases/tags/no-such-tag-xyz", &TransportLimits::default()).unwrap();
        assert_eq!(missing.status, 404);
        // github.com redirects archives to codeload.github.com: the headers of both responses
        // are skipped exactly, and the archive (megabytes) is cut at the body limit.
        let small = TransportLimits { max_body_bytes: 256 << 10, ..TransportLimits::default() };
        let archive = curl.get("https://github.com/MaxLananas/Terraplusminusrusted/archive/27da0ac.zip", &small);
        assert_eq!(archive, Err(TransportError::BodyTooLarge { limit: 256 << 10 }));
        let roomy = TransportLimits { max_body_bytes: 64 << 20, total_timeout: std::time::Duration::from_secs(60), ..TransportLimits::default() };
        let archive = curl.get("https://github.com/MaxLananas/Terraplusminusrusted/archive/27da0ac.zip", &roomy).unwrap();
        assert_eq!((archive.status, &archive.body[..4]), (200, &b"PK\x03\x04"[..]));
    }
}

