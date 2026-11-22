//! `https://` through the system's `curl`, so the default TerraMinusMinus sources (AWS terrain
//! tiles, cloud.daporkchop.net, Overpass) work without a TLS stack in the binary and without
//! `plain_http_hosts`. Certificates are verified by curl against the system trust store.
//!
//! Each request is one `curl` process with hard limits (`--max-time`, `--max-filesize`,
//! `--proto`), no shell, the URL after `--`, and its response headers parsed from
//! `--dump-header`, so status codes and `Retry-After` work as with the plain transport.

use crate::http::{HttpResponse, HttpTransport, PlainHttpTransport, TransportError, TransportLimits};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurlTransport {
    pub program: String,
    pub user_agent: String,
}

impl CurlTransport {
    /// The transport when `program --version` runs and supports HTTPS; `None` otherwise.
    pub fn probe(program: &str, user_agent: &str) -> Option<Self> {
        let output = Command::new(program).arg("--version").stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        let https = text.lines().any(|line| line.starts_with("Protocols:") && line.split_whitespace().any(|protocol| protocol == "https"));
        (output.status.success() && https).then(|| Self { program: program.to_owned(), user_agent: user_agent.to_owned() })
    }

    fn arguments(&self, url: &str, limits: &TransportLimits) -> Vec<String> {
        let seconds = |duration: std::time::Duration| format!("{:.3}", duration.as_secs_f64().max(0.001));
        vec![
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
            if self.user_agent.is_empty() { crate::http::DEFAULT_USER_AGENT.to_owned() } else { self.user_agent.clone() },
            "--header".into(),
            "Accept-Encoding: identity".into(),
            "--dump-header".into(),
            "-".into(),
            "--output".into(),
            "-".into(),
            "--".into(),
            url.to_owned(),
        ]
    }
}

/// Splits curl's `--dump-header -` output (one header block per response, redirects first)
/// from the body that follows, and reads the last block.
pub fn parse_dumped_response(output: &[u8], max_header_bytes: usize) -> Result<HttpResponse, TransportError> {
    let mut rest = output;
    let mut last = None;
    while rest.starts_with(b"HTTP/") {
        let end = rest.windows(4).position(|window| window == b"\r\n\r\n").ok_or_else(|| TransportError::Protocol("unterminated header block".into()))?;
        if end > max_header_bytes {
            return Err(TransportError::Protocol(format!("headers exceed {max_header_bytes} bytes")));
        }
        last = Some(&rest[..end]);
        rest = &rest[end + 4..];
    }
    let block = std::str::from_utf8(last.ok_or_else(|| TransportError::Protocol("no status line".into()))?).map_err(|_| TransportError::Protocol("headers are not UTF-8".into()))?;
    let mut lines = block.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status = status_line.split_whitespace().nth(1).and_then(|code| code.parse::<u16>().ok()).ok_or_else(|| TransportError::Protocol(format!("bad status line `{status_line}`")))?;
    let headers = lines.filter_map(|line| line.split_once(':')).map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned())).collect();
    Ok(HttpResponse { status, headers, body: rest.to_vec() })
}

impl HttpTransport for CurlTransport {
    fn get(&self, url: &str, limits: &TransportLimits) -> Result<HttpResponse, TransportError> {
        if !url.starts_with("https://") || url.bytes().any(|b| b <= b' ' || b == 0x7F) {
            return Err(TransportError::UnsupportedUrl(url.to_owned()));
        }
        let output = Command::new(&self.program)
            .args(self.arguments(url, limits))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|error| TransportError::Connect(format!("{}: {error}", self.program)))?;
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
        let response = parse_dumped_response(&output.stdout, limits.max_header_bytes)?;
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

    #[test]
    fn dumped_headers_are_split_from_the_body() {
        let output = b"HTTP/1.1 301 Moved Permanently\r\nLocation: https://b/\r\n\r\nHTTP/2 200\r\ncontent-type: image/png\r\nRetry-After: 3\r\n\r\n\x89PNG\r\n\r\nrest";
        let response = parse_dumped_response(output, 1 << 16).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.header("retry-after"), Some("3"));
        assert_eq!(response.header("content-type"), Some("image/png"));
        assert_eq!(response.body, b"\x89PNG\r\n\r\nrest");
        let missing = parse_dumped_response(b"HTTP/1.1 404 Not Found\r\n\r\n", 1 << 16).unwrap();
        assert_eq!((missing.status, missing.body.len()), (404, 0));
        for broken in [&b""[..], b"garbage", b"HTTP/1.1 200 OK\r\n", b"HTTP/1.1 abc\r\n\r\n"] {
            assert!(parse_dumped_response(broken, 1 << 16).is_err(), "{broken:?}");
        }
        assert!(parse_dumped_response(b"HTTP/1.1 200 OK\r\nX: yyyyyyyyyyyy\r\n\r\n", 8).is_err());
    }

    #[test]
    fn arguments_are_bounded_and_end_with_the_url() {
        let curl = CurlTransport { program: "curl".into(), user_agent: "Agent/1".into() };
        let arguments = curl.arguments("https://a/b?c=${x}", &TransportLimits::default());
        assert_eq!(arguments.last().map(String::as_str), Some("https://a/b?c=${x}"));
        assert_eq!(arguments[arguments.len() - 2], "--");
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
    }
}

