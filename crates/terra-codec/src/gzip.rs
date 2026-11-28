//! RFC 1952 gzip container, including concatenated members as accepted by `GZIPInputStream`.

use crate::checksum::Crc32;
use crate::deflate::{inflate, inflate_streaming};
use crate::DecodeError;

const FLAG_HEADER_CRC: u8 = 0x02;
const FLAG_EXTRA: u8 = 0x04;
const FLAG_NAME: u8 = 0x08;
const FLAG_COMMENT: u8 = 0x10;
const FLAG_RESERVED: u8 = 0xE0;

fn skip_zero_terminated(input: &[u8], start: usize) -> Result<usize, DecodeError> {
    let terminator = input
        .get(start..)
        .and_then(|rest| rest.iter().position(|&b| b == 0))
        .ok_or(DecodeError::InvalidGzipHeader("unterminated string field"))?;
    Ok(start + terminator + 1)
}

/// Returns the offset of the DEFLATE payload of the member starting at `input[0]`.
fn parse_header(input: &[u8]) -> Result<usize, DecodeError> {
    let header = input.get(..10).ok_or(DecodeError::InvalidGzipHeader("shorter than 10 bytes"))?;
    if header[0] != 0x1F || header[1] != 0x8B {
        return Err(DecodeError::InvalidGzipHeader("bad magic bytes"));
    }
    if header[2] != 8 {
        return Err(DecodeError::InvalidGzipHeader("compression method is not DEFLATE"));
    }
    let flags = header[3];
    if flags & FLAG_RESERVED != 0 {
        return Err(DecodeError::InvalidGzipHeader("reserved flags set"));
    }
    let mut offset = 10;
    if flags & FLAG_EXTRA != 0 {
        let length = input
            .get(offset..offset + 2)
            .map(|b| usize::from(u16::from_le_bytes([b[0], b[1]])))
            .ok_or(DecodeError::Truncated)?;
        offset += 2 + length;
    }
    if flags & FLAG_NAME != 0 {
        offset = skip_zero_terminated(input, offset)?;
    }
    if flags & FLAG_COMMENT != 0 {
        offset = skip_zero_terminated(input, offset)?;
    }
    if flags & FLAG_HEADER_CRC != 0 {
        offset += 2;
    }
    if offset > input.len() {
        return Err(DecodeError::Truncated);
    }
    Ok(offset)
}

fn read_trailer(input: &[u8], offset: usize) -> Result<(u32, u32), DecodeError> {
    let trailer = input.get(offset..offset + 8).ok_or(DecodeError::Truncated)?;
    let crc = u32::from_le_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
    let size = u32::from_le_bytes([trailer[4], trailer[5], trailer[6], trailer[7]]);
    Ok((crc, size))
}

fn verify(expected_crc: u32, expected_size: u32, crc: Crc32, size: usize) -> Result<(), DecodeError> {
    let actual_crc = crc.finish();
    if actual_crc != expected_crc {
        return Err(DecodeError::ChecksumMismatch { kind: "gzip CRC-32", expected: expected_crc, actual: actual_crc });
    }
    // ISIZE is the uncompressed size modulo 2^32.
    let actual_size = size as u32;
    if actual_size != expected_size {
        return Err(DecodeError::LengthMismatch { expected: expected_size, actual: actual_size });
    }
    Ok(())
}

pub fn gunzip(input: &[u8], output_limit: usize) -> Result<Vec<u8>, DecodeError> {
    let mut output = Vec::new();
    let mut offset = 0;
    loop {
        let member = &input[offset..];
        let payload = parse_header(member)?;
        let remaining_limit = output_limit - output.len();
        let (data, consumed) = inflate(&member[payload..], remaining_limit)?;
        let (expected_crc, expected_size) = read_trailer(member, payload + consumed)?;
        let mut crc = Crc32::new();
        crc.update(&data);
        verify(expected_crc, expected_size, crc, data.len())?;
        output.extend_from_slice(&data);
        offset += payload + consumed + 8;
        if offset == input.len() {
            return Ok(output);
        }
    }
}

/// Streams the decompressed bytes of every member into `sink`. Returns the total size.
pub fn gunzip_streaming(
    input: &[u8],
    output_limit: usize,
    sink: &mut dyn FnMut(&[u8]) -> Result<(), String>,
) -> Result<usize, DecodeError> {
    let mut total = 0usize;
    let mut offset = 0;
    loop {
        let member = &input[offset..];
        let payload = parse_header(member)?;
        let mut crc = Crc32::new();
        let mut checked_sink = |bytes: &[u8]| {
            crc.update(bytes);
            sink(bytes)
        };
        let (size, consumed) =
            inflate_streaming(&member[payload..], output_limit - total, &mut checked_sink)?;
        let (expected_crc, expected_size) = read_trailer(member, payload + consumed)?;
        verify(expected_crc, expected_size, crc, size)?;
        total += size;
        offset += payload + consumed + 8;
        if offset == input.len() {
            return Ok(total);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // CPython: gzip.compress(b"terra plus minus\n" * 3, mtime=0)
    const SAMPLE: &str = "1f8b08000000000002032b492d2a4a5428c8292d56c8cdcc2b2de62a21280000b391cde533000000";

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex")).collect()
    }

    #[test]
    fn decodes_reference_member() {
        let decoded = gunzip(&hex(SAMPLE), 1 << 20).expect("valid gzip");
        assert_eq!(decoded, b"terra plus minus\n".repeat(3));
    }

    #[test]
    fn decodes_concatenated_members() {
        let mut doubled = hex(SAMPLE);
        doubled.extend(hex(SAMPLE));
        let decoded = gunzip(&doubled, 1 << 20).expect("valid gzip");
        assert_eq!(decoded, b"terra plus minus\n".repeat(6));
        let mut streamed = Vec::new();
        let total = gunzip_streaming(&doubled, 1 << 20, &mut |b| {
            streamed.extend_from_slice(b);
            Ok(())
        })
        .expect("valid gzip");
        assert_eq!(total, streamed.len());
        assert_eq!(streamed, decoded);
    }

    #[test]
    fn corrupted_crc_is_rejected() {
        let mut data = hex(SAMPLE);
        let crc_position = data.len() - 8;
        data[crc_position] ^= 0xFF;
        assert!(matches!(gunzip(&data, 1 << 20), Err(DecodeError::ChecksumMismatch { .. })));
    }

    #[test]
    fn truncated_member_is_rejected() {
        let data = hex(SAMPLE);
        for cut in [0, 5, 10, 15, data.len() - 3] {
            assert!(gunzip(&data[..cut], 1 << 20).is_err(), "cut at {cut} must fail");
        }
    }
}
