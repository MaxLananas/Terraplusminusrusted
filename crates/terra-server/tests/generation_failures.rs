//! How chunk generation fails: permanent errors end at once, transient ones are retried a
//! bounded number of times, stopping the server cancels waits and backoffs, and a shut-down
//! loader never leads to another way of generating. Also the startup check that every enabled
//! data source can actually be fetched (F05). Network traffic stays on a loopback server.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use terra_server::{GenerationControl, GenerationFailure, StackError, TerraStack, parse_server_config, terrain_for_generation};

/// Answers every request with `status` and `body`; counts requests.
fn loopback(status: u16, body: &'static [u8]) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&requests);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            counter.fetch_add(1, Ordering::SeqCst);
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer);
            let head = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body);
        }
    });
    (format!("http://{address}"), requests)
}

/// A stack whose only elevation layer is served by `base` over plain HTTP.
fn http_stack(name: &str, base: &str, generation: &str) -> (TerraStack, std::path::PathBuf) {
    let directory = common::scratch(name);
    std::fs::write(
        directory.join("heights.json5"),
        format!(
            r#"[{{"dataset": {{"urls": ["{base}/12/${{x}}/${{z}}.png"], "projection": {{"web_mercator": {{"zoom": 12}}}},
            "resolution": 256, "blend": "CUBIC", "parse": {{"parse_png_terrarium": {{}}}}}},
            "bounds": {{"minX": -180, "maxX": 180, "minZ": -85, "maxZ": 85}}, "zooms": {{"min": 0, "max": 3}}}}]"#
        ),
    )
    .unwrap();
    let config = parse_server_config(&format!(
        r#"{{"elevation": {{"heights_config": "heights.json5"}}, "biomes": {{"use_dataset": false}}, "openstreetmap": {{"enabled": false}},
            "generate_trees": false, "network": {{"attempts": 1, "connect_timeout_ms": 1000, "io_timeout_ms": 1000, "total_timeout_ms": 2000}},
            "world": {{"min_y": -64, "height": 2096}}, "generation": {{"workers": 1, "wait_timeout_ms": 10000 {generation}}}}}"#
    ))
    .unwrap();
    (TerraStack::build(config, &directory).unwrap(), directory)
}

#[test]
fn malformed_tiles_fail_the_chunk_without_retrying() {
    let (base, requests) = loopback(200, b"definitely not a PNG");
    let (stack, _) = http_stack("permanent", &base, r#", "retry_attempts": 0"#);
    let control = GenerationControl::default();
    let (chunk_x, chunk_z) = land_chunk(&stack);
    let started = Instant::now();
    let result = terrain_for_generation(&stack, &control, chunk_x, chunk_z, &mut |message| panic!("permanent errors are not retried: {message}"));
    assert!(matches!(result, Err(GenerationFailure::Permanent(_))), "{result:?}");
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(control.counters.data_retries.load(Ordering::Relaxed), 0);
    assert!(requests.load(Ordering::SeqCst) >= 1);
}

#[test]
fn transient_failures_are_retried_a_bounded_number_of_times() {
    let (base, requests) = loopback(503, b"busy");
    let (stack, _) = http_stack("transient", &base, r#", "retry_attempts": 3"#);
    let control = GenerationControl::default();
    let mut logged = Vec::new();
    let (chunk_x, chunk_z) = land_chunk(&stack);
    let result = terrain_for_generation(&stack, &control, chunk_x, chunk_z, &mut |message| logged.push(message));
    let Err(GenerationFailure::RetriesExhausted { attempts, last }) = result else { panic!("{result:?}") };
    assert_eq!(attempts, 3);
    assert!(last.contains("503"), "{last}");
    assert_eq!(logged.len(), 2, "{logged:?}");
    assert_eq!(control.counters.data_retries.load(Ordering::Relaxed), 2);
    assert!(requests.load(Ordering::SeqCst) >= 3);
}

#[test]
fn stopping_the_server_cancels_an_unbounded_retry_promptly() {
    let (base, _) = loopback(503, b"busy");
    let (stack, _) = http_stack("cancel", &base, r#", "retry_attempts": 0"#);
    let stack = Arc::new(stack);
    let control = Arc::new(GenerationControl::default());
    let (chunk_x, chunk_z) = land_chunk(&stack);
    let worker = {
        let (stack, control) = (Arc::clone(&stack), Arc::clone(&control));
        std::thread::spawn(move || terrain_for_generation(&stack, &control, chunk_x, chunk_z, &mut |_| {}))
    };
    // Let it fail and back off a few times, then stop the server.
    let deadline = Instant::now() + Duration::from_secs(20);
    while control.counters.data_retries.load(Ordering::Relaxed) < 2 {
        assert!(Instant::now() < deadline, "no retries observed");
        std::thread::sleep(Duration::from_millis(20));
    }
    let stopped = Instant::now();
    control.gate.shut_down();
    stack.shut_down();
    let result = worker.join().unwrap();
    assert_eq!(result, Err(GenerationFailure::Cancelled));
    assert!(stopped.elapsed() < Duration::from_secs(5), "cancellation took {:?}", stopped.elapsed());
}

#[test]
fn a_shut_down_loader_cancels_instead_of_generating_directly() {
    let (stack, _) = common::stack("loader-shut-down", "");
    let control = GenerationControl::default();
    control.gate.set(terra_server::GateState::Open);
    stack.shut_down();
    let result = terrain_for_generation(&stack, &control, 0, 0, &mut |message| panic!("{message}"));
    assert_eq!(result, Err(GenerationFailure::Cancelled));
    assert!(!stack.loader().is_ready(0, 0), "nothing was generated after the shutdown");
}

/// A land chunk (Grand Canyon), so elevation tiles are really requested.
fn land_chunk(stack: &TerraStack) -> (i32, i32) {
    let block = stack.generator().projection().from_geo(terra_projection::GeoCoordinate::new(-113.247, 35.995)).unwrap();
    ((block.x.floor() as i32) >> 4, (block.y.floor() as i32) >> 4)
}

fn build(directory: &Path, text: &str) -> Result<TerraStack, StackError> {
    TerraStack::build(parse_server_config(text).unwrap(), directory)
}

#[test]
fn https_only_sources_are_refused_before_anything_starts() {
    let directory = common::scratch("unreachable");
    // The defaults: AWS elevation, OpenStreetMap and tree cover, all HTTPS.
    // Without an HTTPS program (turned off, or missing): the HTTPS defaults are refused.
    for network in [r#""https_command": "off""#, r#""https_command": "/nonexistent/curl-for-tests""#] {
        let Err(StackError::Unreachable(reason)) = build(&directory, &format!("{{\"network\": {{{network}}}}}")) else { panic!("the default HTTPS sources must be refused") };
        for expected in ["elevation layer", "OpenStreetMap", "tree cover layer", "needs TLS", "network.mirrors", "network.plain_http_hosts", "curl"] {
            assert!(reason.contains(expected), "{expected:?} missing from {reason}");
        }
    }
    // Elevation over plain HTTP, but OpenStreetMap and trees still on: still refused, naming them.
    let Err(StackError::Unreachable(reason)) = build(&directory, r#"{"network": {"https_command": "off", "plain_http_hosts": ["s3.amazonaws.com"]}}"#) else { panic!() };
    assert!(reason.contains("OpenStreetMap") && reason.contains("tree cover layer") && !reason.contains("elevation layer"), "{reason}");
    // Every source reachable (plain HTTP, mirrors) or explicitly off: accepted, no network used.
    std::fs::create_dir_all(directory.join("osm")).unwrap();
    build(&directory, r#"{"network": {"https_command": "off", "plain_http_hosts": ["s3.amazonaws.com"], "mirrors": {"https://cloud.daporkchop.net/gis/": "osm"}}}"#).unwrap();
    build(&directory, r#"{"network": {"https_command": "off", "plain_http_hosts": ["s3.amazonaws.com"]}, "generate_trees": false, "openstreetmap": {"enabled": false}}"#).unwrap();
    // With the system curl, the defaults work as upstream's do (nothing is fetched here).
    if terra_acquisition::CurlTransport::probe("curl", "test").is_some() {
        build(&directory, "{}").unwrap();
    }
}
