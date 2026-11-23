//! Elevation configuration and tiles from untrusted places, mutated: heights and tree-cover
//! configurations (an operator's file, or one copied from anywhere), and tile bytes served by a
//! remote host through every parser chain. Each call must return without panicking, quickly,
//! and a parsed tile must hold exactly `resolution²` values. Seeds are fixed.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use terra_elevation::{DEFAULT_TREE_COVER, DEFAULT_WORLD_HEIGHTS, UnsupportedLayerPolicy, load_heights_config, load_tree_cover_config};

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
                let end = (at + 1 + rng.below(32)).min(bytes.len());
                let run = bytes[at.min(end)..end].to_vec();
                bytes.splice(at.min(bytes.len())..at.min(bytes.len()), run);
            }
            _ => bytes.truncate(rng.below(bytes.len() + 1)),
        }
    }
    bytes
}

fn guarded(label: &str, input: &[u8], limit: Duration, call: impl FnOnce()) {
    let started = Instant::now();
    if let Err(payload) = catch_unwind(AssertUnwindSafe(call)) {
        let message = payload.downcast_ref::<String>().cloned().or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned())).unwrap_or_default();
        panic!("{label}: {message}\ninput: {:?}", String::from_utf8_lossy(&input[..input.len().min(400)]));
    }
    assert!(started.elapsed() < limit, "{label}: took {:?}", started.elapsed());
}

/// A one-layer config exercising every projection family, transform and parser kind.
const RICH_CONFIG: &str = r#"[
    {"dataset": {"urls": ["https://a.example/${x}/${z}.png"], "projection": {"scale": {"delegate": {"web_mercator": {"zoom": 3}}, "x": 2, "y": 2}},
     "resolution": 256, "blend": "CUBIC", "parse": {"divide": {"delegate": {"from_int": {"delegate": {"and": {"delegate": {"rgb_extract": {"delegate": {"parse_png": {}}}}, "mask": 255}}}}, "value": 100}}},
     "bounds": [{"minX": 0, "maxX": 1, "minZ": 0, "maxZ": 1}, {"minX": 5, "maxX": 6, "minZ": 5, "maxZ": 6}], "zooms": {"min": 0, "max": 3}, "priority": 2,
     "condition": {"and": [{"not": {"delegate": {"equal": 0}}}, {"greater_than": -5}]}},
    {"dataset": {"urls": ["mirror://t/${x}/${z}.tiff"], "projection": {"transverse_mercator": {}}, "resolution": 512, "blend": "LINEAR", "parse": {"flip_z": {"delegate": {"parse_tiff": {}}}}},
     "bounds": [{"minX": -10, "maxX": 10, "minZ": -5, "maxZ": 5}], "zooms": {"min": 0, "max": 1}}
]"#;

#[test]
fn heights_configurations_never_panic() {
    let seeds = [DEFAULT_WORLD_HEIGHTS.as_bytes().to_vec(), RICH_CONFIG.as_bytes().to_vec(), DEFAULT_TREE_COVER.as_bytes().to_vec()];
    // The seeds are valid, so mutations explore the paths past the first error.
    for seed in &seeds[..2] {
        load_heights_config(std::str::from_utf8(seed).unwrap(), UnsupportedLayerPolicy::Reject).unwrap();
    }
    load_tree_cover_config(DEFAULT_TREE_COVER, UnsupportedLayerPolicy::Reject).unwrap();
    let mut rng = Rng(0xe1e7_0000_0000_0001);
    for case in 0..6_000 {
        let base = &seeds[case % seeds.len()];
        let input = if case < seeds.len() { base.clone() } else { mutate(&mut rng, base, b"{}[]\":,-.0123456789eE nulltruefalse\\$") };
        let text = String::from_utf8_lossy(&input).into_owned();
        guarded(&format!("config case {case}"), &input, Duration::from_secs(2), || {
            for policy in [UnsupportedLayerPolicy::Reject, UnsupportedLayerPolicy::Skip] {
                let _ = load_heights_config(&text, policy);
                let _ = load_tree_cover_config(&text, policy);
            }
        });
    }
}

#[test]
fn hostile_tiles_through_every_parser_chain() {
    let chains = [
        r#"{"parse_png_terrarium": {}}"#,
        r#"{"parse_tiff": {}}"#,
        r#"{"divide": {"delegate": {"parse_tiff": {}}, "value": 100}}"#,
        r#"{"swap_axes": {"delegate": {"flip_x": {"delegate": {"parse_tiff": {}}}}}}"#,
        r#"{"from_int": {"delegate": {"parse_png": {}}}}"#,
        r#"{"from_int": {"delegate": {"parse_jpg": {}}}}"#,
        r#"{"from_int": {"delegate": {"parse_tiff": {}}}}"#,
        r#"{"from_int": {"delegate": {"add": {"delegate": {"require_opaque": {"delegate": {"grayscale_extract": {"delegate": {"parse_png": {}}}}}}, "value": 2147483647}}}}"#,
        r#"{"from_int": {"delegate": {"swap_axes": {"delegate": {"flip_z": {"delegate": {"rgb_extract": {"delegate": {"parse_tiff": {}}}}}}}}}}"#,
    ];
    let formats: Vec<_> = chains
        .iter()
        .map(|parse| {
            let config = format!(
                r#"[{{"dataset": {{"urls": ["https://a.example/${{x}}/${{z}}"], "projection": {{"equirectangular": {{}}}}, "resolution": 16, "blend": "NEAR", "parse": {parse}}},
                "bounds": [{{"minX": -1, "maxX": 1, "minZ": -1, "maxZ": 1}}], "zooms": {{"min": 0, "max": 3}}}}]"#
            );
            load_heights_config(&config, UnsupportedLayerPolicy::Reject).unwrap_or_else(|error| panic!("{parse}: {error}")).dataset.layers()[0].dataset.format.clone()
        })
        .collect();
    // 16-pixel squares from the oracle's images, plus the terrarium fixture (another size).
    let input = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/images/input");
    let mut seeds: Vec<Vec<u8>> = std::fs::read_dir(&input).unwrap().map(|entry| std::fs::read(entry.unwrap().path()).unwrap()).filter(|bytes| bytes.len() < 64 << 10).collect();
    seeds.sort();
    seeds.push(std::fs::read(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/elevation/terrarium/12/759/1608.png")).unwrap());
    assert!(seeds.len() >= 50);
    let mut rng = Rng(0x7117_0000_0000_0002);
    let mut parsed = 0;
    for case in 0..12_000 {
        let base = &seeds[case % seeds.len()];
        let input = if case < seeds.len() { base.clone() } else { mutate(&mut rng, base, &[0x00, 0xff, 0x7f, 0x80, 0x10, 0x01]) };
        let format = &formats[case % formats.len()];
        guarded(&format!("tile case {case} through {}", chains[case % chains.len()]), &input, Duration::from_secs(2), || {
            if let Ok(values) = format.parse(&input, 16) {
                assert_eq!(values.len(), 256);
                parsed += 1;
            }
        });
    }
    assert!(parsed > 0, "no tile ever parsed: the seeds are not valid inputs");
}
