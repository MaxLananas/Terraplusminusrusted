//! Adversarial TIFF input: declared sizes, counts and compressed output never set an allocation on
//! their own. Review finding F01: a 147-byte file with `max_pixels = 1` declared one 65,536 by
//! 65,536 float64 LZW tile and made the decoder request 32 GiB.
//!
//! `adversarial_inputs_fail_inside_256_mib` re-runs `bounded_child` in a child process limited to
//! 256 MiB of address space, so any attempt at a large allocation aborts the child and fails the
//! test, instead of passing on a machine with enough memory.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use terra_codec::{DecodeError, TiffLimits, decode_tiff_scalar, decode_tiff_scalar_limited};

const CHILD_VARIABLE: &str = "TERRA_TIFF_BOUNDED_CHILD";

/// A little-endian classic TIFF with one directory of `(tag, type, count, value)` entries
/// (values inline) followed by `data`, whose offset is patched into any entry whose value is
/// `u32::MAX`.
fn tiff(entries: &[(u16, u16, u32, u32)], data: &[u8]) -> Vec<u8> {
    let mut sorted = entries.to_vec();
    sorted.sort_by_key(|entry| entry.0);
    let data_offset = 8 + 2 + 12 * sorted.len() as u32 + 4;
    let mut out = b"II\x2a\x00\x08\x00\x00\x00".to_vec();
    out.extend_from_slice(&(sorted.len() as u16).to_le_bytes());
    for (tag, kind, count, value) in sorted {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        let value = if value == u32::MAX { data_offset } else { value };
        if kind == 3 && count == 1 {
            out.extend_from_slice(&(value as u16).to_le_bytes());
            out.extend_from_slice(&[0, 0]);
        } else {
            out.extend_from_slice(&value.to_le_bytes());
        }
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(data);
    out
}

/// The review's shape: a 1x1 float64 image stored as one 65,536 by 65,536 LZW tile, with a
/// short LZW stream (a run of codes that would expand forever if the output were not bounded).
fn huge_tile_tiny_image() -> Vec<u8> {
    // Clear code then literal zero repeated as 9-bit codes, end-of-information missing.
    let stream = [0x80, 0x00, 0x00, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x01];
    tiff(
        &[
            (256, 4, 1, 1),       // ImageWidth
            (257, 4, 1, 1),       // ImageLength
            (258, 3, 1, 64),      // BitsPerSample
            (259, 3, 1, 5),       // Compression: LZW
            (262, 3, 1, 1),       // BlackIsZero
            (277, 3, 1, 1),       // SamplesPerPixel
            (322, 4, 1, 65_536),  // TileWidth
            (323, 4, 1, 65_536),  // TileLength
            (324, 4, 1, u32::MAX), // TileOffsets
            (325, 4, 1, 10),      // TileByteCounts
            (339, 3, 1, 3),       // SampleFormat: IEEE float
        ],
        &stream,
    )
}

/// A strip image whose offsets field claims 16 million values from a tiny file.
fn huge_offset_count() -> Vec<u8> {
    tiff(
        &[(256, 4, 1, 4), (257, 4, 1, 4), (258, 3, 1, 8), (262, 3, 1, 1), (273, 4, 16_000_000, 8), (278, 4, 1, 1), (279, 4, 16_000_000, 8)],
        &[0; 16],
    )
}

/// One-row strips over a 4096 by 4096 image: more segments than the limit allows, declared by a
/// tiny file.
fn too_many_segments() -> Vec<u8> {
    tiff(&[(256, 4, 1, 4096), (257, 4, 1, 4096), (258, 3, 1, 8), (262, 3, 1, 1), (273, 4, 4096, 8), (278, 4, 1, 1), (279, 4, 4096, 8)], &[0; 16])
}

/// An LZW stream that expands past its segment: must stop at the segment size.
fn lzw_bomb(width: u32, height: u32) -> Vec<u8> {
    // 9-bit codes: CLEAR, 0, then codes 258, 259, ... each one longer than the last.
    let mut codes = vec![256u32, 0];
    for code in 258..510 {
        codes.push(code);
    }
    let mut bytes = Vec::new();
    let (mut accumulator, mut bits) = (0u64, 0u32);
    for (index, code) in codes.iter().enumerate() {
        // The width grows to 10 bits once code 511 would be next, which this stream never reaches.
        let _ = index;
        accumulator = (accumulator << 9) | u64::from(*code);
        bits += 9;
        while bits >= 8 {
            bytes.push((accumulator >> (bits - 8)) as u8);
            bits -= 8;
        }
    }
    if bits > 0 {
        bytes.push((accumulator << (8 - bits)) as u8);
    }
    let count = bytes.len() as u32;
    tiff(&[(256, 4, 1, width), (257, 4, 1, height), (258, 3, 1, 8), (259, 3, 1, 5), (262, 3, 1, 1), (273, 4, 1, u32::MAX), (278, 4, 1, height), (279, 4, 1, count)], &bytes)
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/tiff/cases")
}

#[test]
fn the_reviewed_sample_is_refused_with_a_limit_error() {
    let bytes = huge_tile_tiny_image();
    assert!(bytes.len() < 160, "{} bytes", bytes.len());
    match decode_tiff_scalar(&bytes, 1) {
        Err(DecodeError::LimitExceeded(reason)) => assert!(reason.contains("decoded segment size"), "{reason}"),
        other => panic!("expected a limit error, got {other:?}"),
    }
    // The same tile is refused even with a generous pixel limit: 32 GiB is over any segment limit
    // derived from 2^20 pixels.
    assert!(matches!(decode_tiff_scalar(&bytes, 1 << 20), Err(DecodeError::LimitExceeded(_))));
}

#[test]
fn declared_counts_and_segment_totals_are_bounded() {
    assert!(matches!(decode_tiff_scalar(&huge_offset_count(), 1 << 20), Err(DecodeError::LimitExceeded(_))));
    let limits = TiffLimits { max_segments: 1024, ..TiffLimits::for_pixels(1 << 24) };
    match decode_tiff_scalar_limited(&too_many_segments(), &limits) {
        Err(DecodeError::LimitExceeded(reason)) => assert!(reason.contains("segment count"), "{reason}"),
        other => panic!("expected a segment-count error, got {other:?}"),
    }
    let limits = TiffLimits { max_decoded_bytes: 1000, ..TiffLimits::for_pixels(1 << 24) };
    assert!(matches!(decode_tiff_scalar_limited(&too_many_segments(), &limits), Err(DecodeError::LimitExceeded(_))));
}

#[test]
fn compressed_output_stops_at_the_segment_size() {
    // 16 by 16 bytes: the stream expands far beyond 256 bytes; the extra output is an error or is
    // cut at the segment, never decoded past the limit.
    match decode_tiff_scalar(&lzw_bomb(16, 16), 1 << 20) {
        Ok(raster) => assert_eq!(raster.data.len(), 256),
        Err(DecodeError::OutputLimitExceeded { limit }) => assert_eq!(limit, 256),
        Err(other) => panic!("unexpected {other}"),
    }
}

#[test]
fn reference_files_still_decode_under_their_limits() {
    for name in ["u8_lzw_large", "f64_deflate32946_pred3", "u16_lzw_pred2", "f32_none_be"] {
        let bytes = std::fs::read(fixtures().join(format!("{name}.tif"))).unwrap();
        assert!(decode_tiff_scalar(&bytes, 256 * 256).is_ok(), "{name}");
    }
}

/// Deterministic xorshift for the mutation fuzzer.
fn next(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

/// Runs every adversarial input and 4000 seeded mutations of the reference corpus. Only ever
/// run inside the 256 MiB child started by `adversarial_inputs_fail_inside_256_mib`.
#[test]
#[ignore = "run by adversarial_inputs_fail_inside_256_mib in a memory-limited child"]
fn bounded_child() {
    if std::env::var_os(CHILD_VARIABLE).is_none() {
        return;
    }
    assert!(decode_tiff_scalar(&huge_tile_tiny_image(), 1).is_err());
    assert!(decode_tiff_scalar(&huge_tile_tiny_image(), 1 << 20).is_err());
    assert!(decode_tiff_scalar(&huge_offset_count(), 1 << 20).is_err());
    let _ = decode_tiff_scalar(&too_many_segments(), 1 << 24);
    let _ = decode_tiff_scalar(&lzw_bomb(4096, 4096), 1 << 24);
    let mut corpus = Vec::new();
    for entry in std::fs::read_dir(fixtures()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|extension| extension == "tif") {
            corpus.push(std::fs::read(path).unwrap());
        }
    }
    assert_eq!(corpus.len(), 16);
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    for round in 0..4000 {
        let mut bytes = corpus[round % corpus.len()].clone();
        for _ in 0..1 + next(&mut state) % 8 {
            let at = (next(&mut state) % bytes.len() as u64) as usize;
            // Bias towards the header and directories, where sizes and counts live.
            let at = if next(&mut state).is_multiple_of(2) { at % bytes.len().min(512) } else { at };
            bytes[at] = match next(&mut state) % 4 {
                0 => 0xff,
                1 => 0x00,
                2 => bytes[at] ^ (1 << (next(&mut state) % 8)),
                _ => next(&mut state) as u8,
            };
        }
        let _ = decode_tiff_scalar(&bytes, 1 << 20);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn adversarial_inputs_fail_inside_256_mib() {
    let executable = std::env::current_exe().unwrap();
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg("ulimit -v 262144 && ulimit -c 0 && exec \"$0\" bounded_child --exact --ignored --test-threads=1")
        .arg(&executable)
        .env(CHILD_VARIABLE, "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "bounded child failed ({:?}):\n{stdout}\n{stderr}", output.status);
    assert!(stdout.contains("1 passed"), "the child must actually run the corpus:\n{stdout}");
}
