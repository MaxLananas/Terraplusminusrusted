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

/// Accepts connections and runs `handle` on each, in its own thread.
fn serve_with(handle: fn(std::net::TcpStream)) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || handle(stream));
        }
    });
    format!("http://{address}/13/1/2.png")
}

fn timed<T>(run: impl FnOnce() -> T) -> (T, Duration) {
    let started = std::time::Instant::now();
    let value = run();
    (value, started.elapsed())
}

#[test]
fn total_timeout_is_a_hard_deadline_even_with_a_long_io_timeout() {
    // The reviewed case: total 100 ms with 800 ms per read used to take ~816 ms.
    let url = serve_with(|mut stream| {
        let _ = stream.read(&mut [0u8; 1024]);
        std::thread::sleep(Duration::from_secs(3));
    });
    let limits = TransportLimits { io_timeout: Duration::from_millis(800), total_timeout: Duration::from_millis(100), ..TransportLimits::default() };
    let (result, took) = timed(|| PlainHttpTransport::default().get(&url, &limits));
    assert_eq!(result, Err(TransportError::Timeout));
    assert!(took < Duration::from_millis(400), "took {took:?}");
}

#[test]
fn a_dripping_server_cannot_extend_the_deadline() {
    // Each byte arrives well within the per-read timeout; only the total deadline stops it.
    let url = serve_with(|mut stream| {
        let _ = stream.read(&mut [0u8; 1024]);
        for byte in b"HTTP/1.1 200 OK\r\nContent-Length: 100000\r\n\r\n".iter().chain([b'x'; 1000].iter()) {
            if stream.write_all(&[*byte]).is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    let limits = TransportLimits { io_timeout: Duration::from_secs(5), total_timeout: Duration::from_millis(300), ..TransportLimits::default() };
    let (result, took) = timed(|| PlainHttpTransport::default().get(&url, &limits));
    assert_eq!(result, Err(TransportError::Timeout));
    assert!(took < Duration::from_millis(700), "took {took:?}");
}

#[test]
fn one_deadline_covers_every_attempt_and_backoff() {
    let url = serve_with(|mut stream| {
        let _ = stream.read(&mut [0u8; 1024]);
        let _ = stream.write_all(b"HTTP/1.1 500 Broken\r\nContent-Length: 0\r\n\r\n");
    });
    let policy = RetryPolicy {
        max_attempts: 10,
        initial_backoff: Duration::from_millis(100),
        max_backoff: Duration::from_secs(5),
        fetch_deadline: Duration::from_millis(400),
        ..RetryPolicy::default()
    };
    let fetcher = HttpTileFetcher::new(PlainHttpTransport::default(), TransportLimits::default(), policy);
    let (result, took) = timed(|| fetcher.fetch(&url));
    let error = result.unwrap_err().to_string();
    assert!(error.contains("fetch deadline of 400 ms reached"), "{error}");
    assert!(took < Duration::from_millis(800), "took {took:?}");
}

#[test]
fn a_stalled_attempt_is_cut_to_the_remaining_fetch_deadline() {
    let url = serve_with(|mut stream| {
        let _ = stream.read(&mut [0u8; 1024]);
        std::thread::sleep(Duration::from_secs(3));
    });
    let policy = RetryPolicy { max_attempts: 3, fetch_deadline: Duration::from_millis(300), ..RetryPolicy::default() };
    let limits = TransportLimits { io_timeout: Duration::from_secs(10), total_timeout: Duration::from_secs(10), ..TransportLimits::default() };
    let fetcher = HttpTileFetcher::new(PlainHttpTransport::default(), limits, policy);
    let (result, took) = timed(|| fetcher.fetch(&url));
    assert!(result.is_err());
    assert!(took < Duration::from_millis(800), "took {took:?}");
}
