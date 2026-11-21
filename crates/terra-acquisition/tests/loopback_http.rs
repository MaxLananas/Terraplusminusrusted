//! Exercises the plain HTTP transport and retry policy against a loopback server.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;
use terra_acquisition::{HttpTileFetcher, HttpTransport, PlainHttpTransport, RetryPolicy, TransportError, TransportLimits};
use terra_elevation::{FetchOutcome, TileFetcher};

fn serve(responses: Vec<Vec<u8>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for response in responses {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            if !response.is_empty() {
                let _ = stream.write_all(&response);
            } else {
                std::thread::sleep(Duration::from_millis(600));
            }
        }
    });
    format!("http://{address}/13/1/2.png")
}

#[test]
fn fetches_through_retries_and_reports_missing() {
    let url = serve(vec![
        b"HTTP/1.1 503 Busy\r\nRetry-After: 0\r\nContent-Length: 0\r\n\r\n".to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ntile".to_vec(),
        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec(),
    ]);
    let policy = RetryPolicy { initial_backoff: Duration::from_millis(5), ..RetryPolicy::default() };
    let fetcher = HttpTileFetcher::new(PlainHttpTransport::default(), TransportLimits::default(), policy);
    assert_eq!(fetcher.fetch(&url).unwrap(), FetchOutcome::Found(b"tile".to_vec()));
    assert_eq!(fetcher.fetch(&url).unwrap(), FetchOutcome::Missing);
}

#[test]
fn stalled_server_times_out() {
    let url = serve(vec![Vec::new()]);
    let limits = TransportLimits { io_timeout: Duration::from_millis(200), ..TransportLimits::default() };
    assert_eq!(PlainHttpTransport::default().get(&url, &limits), Err(TransportError::Timeout));
}

#[test]
fn https_is_refused_not_downgraded() {
    let error = PlainHttpTransport::default().get("https://s3.amazonaws.com/x", &TransportLimits::default()).unwrap_err();
    assert!(matches!(error, TransportError::UnsupportedUrl(_)));
}
