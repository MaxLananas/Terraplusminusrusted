//! Every decoder on hostile input: byte mutations of real inputs (PNG tiles, gzip/zlib/DEFLATE
//! streams, TIFF rasters, JSON5 configuration) and synthetic pathological cases. Each call must
//! return (a value or an error) without panicking, within a time bound and within its output
//! limit. Seeds are fixed, so failures reproduce. `TERRA_FUZZ_SCALE=<n>` runs n times as many
//! cases.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use terra_codec::{
    JpegLimits, PngLimits, TiffLimits, adler32, crc32, decode_jpeg, decode_png, decode_tiff_argb, decode_tiff_scalar_limited, gunzip, gunzip_streaming, inflate, inflate_streaming,
    parse_java_double, parse_relaxed_json, zlib_decompress,
};

const LIMIT: usize = 8 << 20;

fn repository(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join(relative)
}

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

/// Bit flips, byte overwrites with boundary values, deletions, insertions, duplicated runs and
/// truncation, 1 to 8 of them.
fn mutate(rng: &mut Rng, input: &[u8]) -> Vec<u8> {
    let mut bytes = input.to_vec();
    for _ in 0..1 + rng.below(8) {
        let at = rng.below(bytes.len());
        match rng.below(7) {
            0 if !bytes.is_empty() => bytes[at] ^= 1 << rng.below(8),
            1 if !bytes.is_empty() => bytes[at] = [0x00, 0xff, 0x7f, 0x80, 0x01, 0xfe][rng.below(6)],
            2 if !bytes.is_empty() => {
                bytes.remove(at);
            }
            3 => bytes.insert(at.min(bytes.len()), rng.next() as u8),
            4 => {
                let end = (at + 1 + rng.below(32)).min(bytes.len());
                let run = bytes[at.min(end)..end].to_vec();
                let position = rng.below(bytes.len());
                bytes.splice(position..position, run);
            }
            5 => bytes.truncate(rng.below(bytes.len() + 1)),
            _ if bytes.len() >= 4 => {
                // A 32-bit field set to a boundary value: lengths, offsets, counts.
                let at = rng.below(bytes.len() - 3);
                let value: u32 = [0, 1, 0x7fff_ffff, 0x8000_0000, 0xffff_ffff, 0x0001_0000, 0x4000_0000][rng.below(7)];
                bytes[at..at + 4].copy_from_slice(&value.to_be_bytes());
            }
            _ => {}
        }
    }
    bytes
}

/// `TERRA_FUZZ_SCALE=<n>` multiplies every case count, for a deeper run than the default one.
fn scale() -> usize {
    std::env::var("TERRA_FUZZ_SCALE").ok().and_then(|value| value.parse().ok()).filter(|&n| n >= 1).unwrap_or(1)
}

/// Runs `decode` over every mutated case and reports panics and slow cases with their seeds.
fn fuzz(name: &str, seeds: &[Vec<u8>], cases: usize, seed: u64, decode: impl Fn(&[u8]) -> usize) {
    let cases = cases * scale();
    let mut rng = Rng(seed);
    let mut failures = Vec::new();
    let mut produced = 0usize;
    for case in 0..cases {
        let base = &seeds[case % seeds.len()];
        let input = if case < seeds.len() { base.clone() } else { mutate(&mut rng, base) };
        let started = Instant::now();
        match catch_unwind(AssertUnwindSafe(|| decode(&input))) {
            Ok(size) => produced += usize::from(size > 0),
            Err(payload) => {
                let message = payload.downcast_ref::<String>().cloned().or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned())).unwrap_or_default();
                failures.push(format!("case {case}: panic: {message} (input {} bytes, starts {:02x?})", input.len(), &input[..input.len().min(24)]));
                // `TERRA_FUZZ_DUMP=<directory>` keeps failing inputs, to turn them into regression tests.
                if let Ok(directory) = std::env::var("TERRA_FUZZ_DUMP") {
                    let _ = std::fs::write(PathBuf::from(directory).join(format!("{}-{case}.bin", name.replace(|c: char| !c.is_ascii_alphanumeric(), "_"))), &input);
                }
            }
        }
        let elapsed = started.elapsed();
        if elapsed > Duration::from_secs(2) {
            failures.push(format!("case {case}: took {elapsed:?}"));
        }
    }
    eprintln!("{name}: {cases} cases, {produced} decoded something");
    assert!(produced > 0, "{name}: nothing ever decoded; the seeds are not valid inputs");
    assert!(failures.is_empty(), "{name}: {} failures:\n{}", failures.len(), failures.iter().take(8).cloned().collect::<Vec<_>>().join("\n"));
}

fn gzip_fixtures() -> Vec<Vec<u8>> {
    ["fixtures/osm/golden/elements.tsv.gz", "fixtures/commands/golden/double_text.tsv.gz", "fixtures/tiff/cases/f32_nodata.f32.gz", "crates/terra-projection/resources/conformal.gz"]
        .iter()
        .map(|path| std::fs::read(repository(path)).unwrap())
        .collect()
}

/// The raw DEFLATE payload of a gzip member (RFC 1952 header skipped).
fn deflate_payload(gzip: &[u8]) -> Vec<u8> {
    let flags = gzip[3];
    let mut at = 10;
    if flags & 0x04 != 0 {
        at += 2 + usize::from(u16::from_le_bytes([gzip[at], gzip[at + 1]]));
    }
    for bit in [0x08, 0x10] {
        if flags & bit != 0 {
            at += gzip[at..].iter().position(|&b| b == 0).unwrap() + 1;
        }
    }
    if flags & 0x02 != 0 {
        at += 2;
    }
    gzip[at..].to_vec()
}

#[test]
fn gzip_survives_mutation() {
    let seeds = gzip_fixtures();
    fuzz("gunzip", &seeds, 1500, 0x1234_5678_9abc_def1, |input| match gunzip(input, LIMIT) {
        Ok(output) => {
            assert!(output.len() <= LIMIT);
            output.len().max(1)
        }
        Err(_) => 0,
    });
    fuzz("gunzip_streaming", &seeds, 600, 0x0fed_cba9_8765_4321, |input| {
        let mut total = 0usize;
        let result = gunzip_streaming(input, LIMIT, &mut |chunk| {
            total += chunk.len();
            Ok(())
        });
        assert!(total <= LIMIT, "streamed {total} bytes past the limit");
        usize::from(result.is_ok())
    });
}

#[test]
fn deflate_and_zlib_survive_mutation() {
    let gzips = gzip_fixtures();
    let raw: Vec<Vec<u8>> = gzips.iter().map(|g| deflate_payload(g)).collect();
    fuzz("inflate", &raw, 1500, 0x5555_aaaa_3333_cccc, |input| match inflate(input, LIMIT) {
        Ok((output, consumed)) => {
            assert!(output.len() <= LIMIT && consumed <= input.len());
            1
        }
        Err(_) => 0,
    });
    fuzz("inflate_streaming", &raw, 600, 0x7777_1111_9999_2222, |input| {
        let mut total = 0usize;
        let result = inflate_streaming(input, LIMIT, &mut |chunk| {
            total += chunk.len();
            Ok(())
        });
        assert!(total <= LIMIT);
        usize::from(result.is_ok())
    });
    // zlib: header + the payload + the Adler-32 of the decompressed data.
    let zlib: Vec<Vec<u8>> = gzips
        .iter()
        .zip(&raw)
        .map(|(gzip, payload)| {
            let plain = gunzip(gzip, 64 << 20).unwrap();
            let (_, consumed) = inflate(payload, 64 << 20).unwrap();
            let mut stream = vec![0x78, 0x9c];
            stream.extend_from_slice(&payload[..consumed]);
            stream.extend_from_slice(&adler32(&plain).to_be_bytes());
            stream
        })
        .collect();
    fuzz("zlib", &zlib, 1500, 0x2468_ace0_1357_9bdf, |input| match zlib_decompress(input, LIMIT) {
        Ok(output) => {
            assert!(output.len() <= LIMIT);
            1
        }
        Err(_) => 0,
    });
}

#[test]
fn decompression_bombs_stop_at_the_limit() {
    // Stored block of 0 bytes followed by fixed-Huffman blocks of maximal back-references: a
    // stream that expands about 1000x. Built by hand: the first literal, then length 258 distance 1
    // repeated.
    let mut bits: Vec<bool> = Vec::new();
    let push = |value: u32, count: u32, bits: &mut Vec<bool>| {
        for i in 0..count {
            bits.push(value >> i & 1 == 1);
        }
    };
    let push_huffman = |code: u32, length: u32, bits: &mut Vec<bool>| {
        for i in (0..length).rev() {
            bits.push(code >> i & 1 == 1);
        }
    };
    push(1, 1, &mut bits); // final block
    push(1, 2, &mut bits); // fixed Huffman
    push_huffman(0x30 + u32::from(b'a'), 8, &mut bits); // literal 'a'
    for _ in 0..200_000 {
        push_huffman(0b1100_0101, 8, &mut bits); // length code 285 = 258
        push_huffman(0, 5, &mut bits); // distance code 0 = 1
    }
    push_huffman(0, 7, &mut bits); // end of block
    let mut stream = vec![0u8; bits.len().div_ceil(8)];
    for (i, bit) in bits.iter().enumerate() {
        if *bit {
            stream[i / 8] |= 1 << (i % 8);
        }
    }
    let expanded = 1 + 258 * 200_000;
    assert!(expanded > LIMIT);
    let started = Instant::now();
    assert!(inflate(&stream, LIMIT).is_err(), "a stream of {expanded} bytes must not decode under a {LIMIT}-byte limit");
    assert!(started.elapsed() < Duration::from_secs(2));
    let (output, _) = inflate(&stream, 64 << 20).unwrap();
    assert_eq!(output.len(), expanded);
    assert!(output.iter().all(|&b| b == b'a'));
}

/// Recomputes the CRC of every complete chunk so mutations reach the decoder past the CRC check.
fn fix_png_crcs(bytes: &mut [u8]) {
    let mut at = 8;
    while at + 12 <= bytes.len() {
        let length = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let Some(end) = (at + 8).checked_add(length).filter(|end| end + 4 <= bytes.len()) else { break };
        let crc = crc32(&bytes[at + 4..end]);
        bytes[end..end + 4].copy_from_slice(&crc.to_be_bytes());
        at = end + 4;
    }
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
}

/// zlib stream of stored (uncompressed) DEFLATE blocks, so scanline bytes are chosen exactly.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = if data.is_empty() { vec![&[][..]] } else { data.chunks(65_535).collect() };
    for (index, block) in blocks.iter().enumerate() {
        out.push(u8::from(index + 1 == blocks.len()));
        let length = block.len() as u16;
        out.extend_from_slice(&length.to_le_bytes());
        out.extend_from_slice(&(!length).to_le_bytes());
        out.extend_from_slice(block);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// A structurally valid PNG with random dimensions, colour type, bit depth (sometimes invalid),
/// interlacing, palette, transparency, filter bytes (sometimes invalid) and data length (sometimes
/// short or long).
fn synthetic_png(rng: &mut Rng) -> Vec<u8> {
    let width = 1 + rng.below(40) as u32;
    let height = 1 + rng.below(40) as u32;
    let colours = if rng.below(10) == 0 { 7 } else { 5 };
    let colour = [0u8, 2, 3, 4, 6, 1, 5][rng.below(colours)];
    let depths: &[u8] = match colour {
        0 => &[1, 2, 4, 8, 16],
        3 => &[1, 2, 4, 8],
        _ => &[8, 16],
    };
    let depth = if rng.below(12) == 0 { [0, 3, 32, 255][rng.below(4)] } else { depths[rng.below(depths.len())] };
    let interlace = u8::from(rng.below(4) == 0);
    let channels = match colour {
        2 => 3,
        4 => 2,
        6 => 4,
        _ => 1,
    };
    let row_bytes = (width as usize * channels * usize::from(depth)).div_ceil(8);
    let mut raw = Vec::new();
    for _ in 0..height {
        raw.push(if rng.below(30) == 0 { 5 + rng.below(250) as u8 } else { rng.below(5) as u8 });
        raw.extend((0..row_bytes).map(|_| rng.next() as u8));
    }
    match rng.below(10) {
        0 => raw.truncate(rng.below(raw.len() + 1)),
        1 => raw.extend((0..1 + rng.below(64)).map(|_| rng.next() as u8)),
        _ => {}
    }
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[depth, colour, 0, 0, interlace]);
    png_chunk(&mut png, b"IHDR", &header);
    if colour == 3 || rng.below(8) == 0 {
        let entries = rng.below(260);
        let palette: Vec<u8> = (0..entries * 3).map(|_| rng.next() as u8).collect();
        png_chunk(&mut png, b"PLTE", &palette);
    }
    if rng.below(4) == 0 {
        let transparency: Vec<u8> = (0..rng.below(300)).map(|_| rng.next() as u8).collect();
        png_chunk(&mut png, b"tRNS", &transparency);
    }
    let stream = zlib_stored(&raw);
    // Split IDAT in up to three chunks, as encoders do.
    let cut1 = rng.below(stream.len() + 1);
    let cut2 = cut1 + rng.below(stream.len() - cut1 + 1);
    for part in [&stream[..cut1], &stream[cut1..cut2], &stream[cut2..]] {
        if !part.is_empty() {
            png_chunk(&mut png, b"IDAT", part);
        }
    }
    png_chunk(&mut png, b"IEND", &[]);
    png
}

#[test]
fn png_survives_mutation() {
    let tile = std::fs::read(repository("fixtures/elevation/terrarium/12/759/1608.png")).unwrap();
    let limits = PngLimits::default();
    let check = |input: &[u8]| match decode_png(input, &limits) {
        Ok(image) => {
            assert!(image.width <= limits.max_width && image.height <= limits.max_height);
            assert_eq!(image.argb.len(), image.width as usize * image.height as usize);
            1
        }
        Err(_) => 0,
    };
    fuzz("png", std::slice::from_ref(&tile), 800, 0xdead_beef_cafe_f00d, check);
    fuzz("png (CRCs repaired)", std::slice::from_ref(&tile), 800, 0xfeed_face_dead_10cc, |input| {
        let mut repaired = input.to_vec();
        fix_png_crcs(&mut repaired);
        check(&repaired)
    });
    let mut rng = Rng(0x0123_4567_89ab_cdef);
    let synthetic: Vec<Vec<u8>> = (0..400).map(|_| synthetic_png(&mut rng)).collect();
    fuzz("png (synthetic)", &synthetic, 400, 1, check);
    fuzz("png (synthetic, mutated, CRCs repaired)", &synthetic, 2000, 0x5eed_5eed_5eed_5eed, |input| {
        let mut repaired = input.to_vec();
        fix_png_crcs(&mut repaired);
        check(&repaired)
    });
}

#[test]
fn tiff_survives_mutation() {
    let cases = std::fs::read_dir(repository("fixtures/tiff/cases")).unwrap();
    let mut seeds: Vec<Vec<u8>> = cases.map(|entry| entry.unwrap().path()).filter(|path| path.extension().is_some_and(|e| e == "tif")).map(|path| std::fs::read(path).unwrap()).collect();
    seeds.sort();
    assert!(seeds.len() >= 8);
    let limits = TiffLimits::for_pixels(512 * 512);
    fuzz("tiff", &seeds, 3000, 0x0bad_c0de_0bad_c0de, |input| match decode_tiff_scalar_limited(input, &limits) {
        Ok(raster) => {
            assert!((raster.width as u64) * (raster.height as u64) <= limits.max_pixels);
            1
        }
        Err(_) => 0,
    });
}

/// The oracle's image fixtures (`fixtures/images/input`) whose name ends with one of `extensions`.
fn image_fixtures(extensions: &[&str]) -> Vec<Vec<u8>> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(repository("fixtures/images/input"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().and_then(|e| e.to_str()).is_some_and(|e| extensions.contains(&e)))
        .collect();
    paths.sort();
    paths.into_iter().map(|path| std::fs::read(path).unwrap()).collect()
}

#[test]
fn jpeg_survives_mutation() {
    let seeds = image_fixtures(&["jpg", "jpeg"]);
    assert!(seeds.len() >= 5, "{} JPEG seeds", seeds.len());
    let limits = JpegLimits { max_width: 512, max_height: 512 };
    fuzz("jpeg", &seeds, 4000, 0x1bad_b002_5eed_0001, |input| match decode_jpeg(input, &limits) {
        Ok(image) => {
            assert!(image.width <= limits.max_width && image.height <= limits.max_height);
            assert_eq!(image.argb.len(), image.width as usize * image.height as usize);
            1
        }
        Err(_) => 0,
    });
}

#[test]
fn tiff_argb_survives_mutation() {
    let mut seeds = image_fixtures(&["tif", "tiff"]);
    seeds.extend(std::fs::read_dir(repository("fixtures/tiff/cases")).unwrap().map(|entry| entry.unwrap().path()).filter(|path| path.extension().is_some_and(|e| e == "tif")).map(|path| std::fs::read(path).unwrap()));
    assert!(seeds.len() >= 40, "{} TIFF seeds", seeds.len());
    let limits = TiffLimits::for_pixels(256 * 256);
    fuzz("tiff (ARGB)", &seeds, 6000, 0x7aff_a0eb_0000_0001, |input| match decode_tiff_argb(input, &limits) {
        Ok(image) => {
            assert!(u64::from(image.width) * u64::from(image.height) <= limits.max_pixels);
            assert_eq!(image.argb.len(), image.width as usize * image.height as usize);
            1
        }
        Err(_) => 0,
    });
}

#[test]
fn json5_survives_mutation_and_pathological_nesting() {
    let seeds: Vec<Vec<u8>> = [
        "crates/terra-osm/resources/osm.json5",
        "crates/terra-elevation/resources/heights_world.json5",
        "crates/terra-elevation/resources/tree_cover.json5",
        "fixtures/osm/mappings/exhaustive.json5",
        "examples/offline-grand-canyon/terraplusminus.json5",
    ]
    .iter()
    .map(|path| std::fs::read(repository(path)).unwrap())
    .collect();
    fuzz("relaxed_json", &seeds, 2500, 0x1357_2468_aaaa_5555, |input| usize::from(parse_relaxed_json(&String::from_utf8_lossy(input)).is_ok()));

    for text in [
        "[".repeat(100_000),
        "{\"a\":".repeat(100_000),
        format!("{}1{}", "[".repeat(63), "]".repeat(63)),
        "/*".to_owned(),
        "\"\\u".to_owned(),
        "\"\\uD800\"".to_owned(),
        "1e999999999999".to_owned(),
        "-".to_owned(),
        "0x".to_owned(),
        "'".to_owned(),
        format!("\"{}\"", "\\\\".repeat(1_000_000)),
        "[1,]".to_owned(),
        "{a:1,}".to_owned(),
        "\u{feff}{}".to_owned(),
    ] {
        let started = Instant::now();
        let outcome = catch_unwind(AssertUnwindSafe(|| parse_relaxed_json(&text).is_ok()));
        assert!(outcome.is_ok(), "panic on {:?}", text.chars().take(40).collect::<String>());
        assert!(started.elapsed() < Duration::from_secs(2), "slow on {:?}", text.chars().take(40).collect::<String>());
    }
}

#[test]
fn java_double_parsing_survives_hostile_text() {
    let mut rng = Rng(0x9999_8888_7777_6666);
    let alphabet = b"0123456789+-.eExXpPaAfFnNiItTyY_ dD";
    let mut fixed = vec![
        "1".repeat(100_000),
        format!("0.{}1e-400", "0".repeat(5000)),
        format!("1e{}", "9".repeat(5000)),
        "0x1p-1075".to_owned(),
        "0x.p1".to_owned(),
        "NaN".to_owned(),
        "-Infinity".to_owned(),
        "1e-2147483648".to_owned(),
        "1e2147483648".to_owned(),
        ".e1".to_owned(),
        "".to_owned(),
    ];
    for _ in 0..20_000 {
        let length = rng.below(24);
        fixed.push((0..length).map(|_| alphabet[rng.below(alphabet.len())] as char).collect());
    }
    for text in fixed {
        let started = Instant::now();
        let outcome = catch_unwind(AssertUnwindSafe(|| parse_java_double(&text)));
        assert!(outcome.is_ok(), "panic on {:?}", text.chars().take(40).collect::<String>());
        assert!(started.elapsed() < Duration::from_secs(1), "slow on {:?}", text.chars().take(40).collect::<String>());
    }
}
