//! What players, proxies, remote servers and configuration files can send, fed back mutated:
//! plugin-message payloads, `/tpll` arguments, update documents and configuration text. Every
//! call must return without panicking, quickly, and what it accepts must respect the invariants
//! the rest of the server relies on. Seeds are fixed, so failures reproduce.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};
use terra_server::bridge::{self, parse_delivery, parse_get_server, read_java_utf, write_java_utf};
use terra_server::updates::latest_tag;
use terra_server::{parse_coordinates, parse_server_config, parse_tpll_arguments};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound.max(1) as u64) as usize
    }
}

fn mutate(rng: &mut Rng, input: &[u8], alphabet: &[u8]) -> Vec<u8> {
    let mut bytes = input.to_vec();
    for _ in 0..1 + rng.below(6) {
        let at = rng.below(bytes.len());
        match rng.below(6) {
            0 if !bytes.is_empty() => bytes[at] ^= 1 << rng.below(8),
            1 if !bytes.is_empty() => bytes[at] = alphabet[rng.below(alphabet.len())],
            2 if !bytes.is_empty() => {
                bytes.remove(at);
            }
            3 => bytes.insert(at.min(bytes.len()), alphabet[rng.below(alphabet.len())]),
            4 => {
                let end = (at + 1 + rng.below(16)).min(bytes.len());
                let run = bytes[at.min(end)..end].to_vec();
                bytes.splice(at.min(bytes.len())..at.min(bytes.len()), run);
            }
            _ => bytes.truncate(rng.below(bytes.len() + 1)),
        }
    }
    bytes
}

/// Runs `check` on `cases` mutations of `seeds`, failing on panics and slow calls.
fn fuzz(name: &str, seeds: &[Vec<u8>], alphabet: &[u8], cases: usize, seed: u64, check: impl Fn(&[u8])) {
    let mut rng = Rng(seed);
    for case in 0..cases {
        let base = &seeds[case % seeds.len()];
        let input = if case < seeds.len() { base.clone() } else { mutate(&mut rng, base, alphabet) };
        let started = Instant::now();
        if let Err(payload) = catch_unwind(AssertUnwindSafe(|| check(&input))) {
            let message = payload.downcast_ref::<String>().cloned().or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned())).unwrap_or_default();
            panic!("{name}, case {case}: {message}\ninput: {:?}", String::from_utf8_lossy(&input));
        }
        assert!(started.elapsed() < Duration::from_secs(1), "{name}, case {case}: slow on {:?}", String::from_utf8_lossy(&input));
    }
}

#[test]
fn bridge_payloads_from_any_client() {
    let mut delivery = Vec::new();
    write_java_utf(&mut delivery, "123e4567-e89b-12d3-a456-426614174000").unwrap();
    write_java_utf(&mut delivery, "48.8583, 2.2945").unwrap();
    let mut answer = Vec::new();
    write_java_utf(&mut answer, "GetServer").unwrap();
    write_java_utf(&mut answer, "lobby-1").unwrap();
    let request = bridge::teleport_request(0x123e_4567_e89b_12d3_a456_4266_1417_4000, "high", -2032, terra_server::LatLng { latitude: 1.5, longitude: -2.25 }).unwrap();
    let seeds = vec![delivery, answer, request, bridge::get_server_request()];
    fuzz("bridge", &seeds, &[0x00, 0xff, 0xc0, 0x80, 0xe0, 0xed, b'-', b',', b' ', b'7'], 60_000, 0xb81d_6e00_0000_0001, |input| {
        if let Some(parsed) = parse_delivery(input) {
            // Whatever was read re-encodes (Java's writeUTF never fails on what readUTF gave).
            let mut again = Vec::new();
            write_java_utf(&mut again, &bridge::uuid_to_string(parsed.uuid)).unwrap();
            write_java_utf(&mut again, &parsed.coordinates).unwrap();
            assert_eq!(bridge::parse_delivery(&again), Some(parsed));
        }
        let _ = parse_get_server(input);
        let mut rest = input;
        while let Some(text) = read_java_utf(&mut rest) {
            assert!(text.len() <= 3 * 65_535);
        }
    });
}

#[test]
fn tpll_arguments_from_any_player() {
    let seeds: Vec<Vec<u8>> = [
        "48.858370 2.294481 330",
        "48.858370, 2.294481",
        "48°51'30.1\"N 2°17'40.1\"E",
        "-33.8568 151.2153 -12.5",
        "1e308 1e308",
        "NaN Infinity 3",
        "0 0 0 0 0 0 0",
        "",
    ]
    .iter()
    .map(|text| text.as_bytes().to_vec())
    .collect();
    fuzz("tpll", &seeds, b"0123456789.,-+ eE\xc2\xb0'\"NSEWnaI", 60_000, 0x7011_0000_0000_0002, |input| {
        let text = String::from_utf8_lossy(input);
        if let Some(target) = parse_tpll_arguments(&text) {
            // Upstream (GBIF's parser) keeps a "presumed swapped" pair as is, so a latitude may
            // reach 180; the projection then refuses it.
            let (lat, lon) = (target.position.latitude.abs(), target.position.longitude.abs());
            assert!((lat <= 90.0 && lon <= 180.0) || (lat <= 180.0 && lon <= 90.0), "{text}: {target:?}");
            assert!(target.height.is_none_or(f64::is_finite), "{text}: {target:?}");
        }
        if let Some(position) = parse_coordinates(&text) {
            assert!(position.latitude.is_finite() && position.longitude.is_finite(), "{text}: {position:?}");
        }
    });
    // Very long arguments stay fast.
    let long = "1 ".repeat(100_000);
    let started = Instant::now();
    assert!(parse_tpll_arguments(&long).is_none());
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn release_documents_from_any_mirror() {
    let seeds: Vec<Vec<u8>> = [r#"{"tag_name": "v1.2.3", "name": "Release", "assets": [{"size": 1}]}"#, r#"{"message": "Not Found"}"#, "[1, 2, {\"tag_name\": 5}]"]
        .iter()
        .map(|text| text.as_bytes().to_vec())
        .collect();
    fuzz("release", &seeds, b"{}[]\":,\\v0.1-\xa7\n", 30_000, 0x0e1e_a5e0_0000_0003, |input| {
        if let Some(tag) = latest_tag(&String::from_utf8_lossy(input)) {
            assert!(!tag.is_empty() && tag.len() <= 64 && tag.bytes().all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b)), "{tag:?}");
        }
    });
}

#[test]
fn configuration_text_never_panics() {
    let example = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/aws-terrain-tiles/terraplusminus.json5")).unwrap();
    let offline = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/offline-grand-canyon/terraplusminus.json5")).unwrap();
    let everything = br#"{
        "world": {"min_y": -64, "height": 384}, "y_offset": 5, "terrain_offset": {"x": 1, "y": 2, "z": 3},
        "linked_worlds": {"enabled": true, "method": "SERVER", "server_name": "a", "worlds": [{"name": "a", "offset": 0}, {"name": "b", "offset": 2032}]},
        "network": {"https_command": "curl", "max_concurrent_requests": ["2: https://x/"], "cache_ttl_minutes": 5, "user_agent": "A/1"},
        "updates": {"enabled": true, "repository": "a/b", "url": "file:///tmp/x"},
        "generation": {"workers": 2, "queue_capacity": 10, "retry_attempts": 3}
    }"#
    .to_vec();
    let seeds = vec![example, offline, everything];
    fuzz("config", &seeds, b"{}[]\":,-0123456789 truefalsnul\n", 6_000, 0xc0f1_9000_0000_0004, |input| {
        let _ = parse_server_config(&String::from_utf8_lossy(input));
    });
}
