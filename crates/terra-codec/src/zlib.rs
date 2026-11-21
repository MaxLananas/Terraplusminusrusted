//! RFC 1950 zlib container, as used inside PNG `IDAT` chunks.

use crate::checksum::Adler32;
use crate::deflate::inflate;
use crate::DecodeError;

pub fn zlib_decompress(input: &[u8], output_limit: usize) -> Result<Vec<u8>, DecodeError> {
    let header = input.get(..2).ok_or(DecodeError::Truncated)?;
    let (cmf, flg) = (header[0], header[1]);
    if cmf & 0x0F != 8 {
        return Err(DecodeError::InvalidZlibHeader("compression method is not DEFLATE"));
    }
    if cmf >> 4 > 7 {
        return Err(DecodeError::InvalidZlibHeader("window size larger than 32 KiB"));
    }
    if (u16::from(cmf) << 8 | u16::from(flg)) % 31 != 0 {
        return Err(DecodeError::InvalidZlibHeader("header check bits are wrong"));
    }
    if flg & 0x20 != 0 {
        return Err(DecodeError::InvalidZlibHeader("preset dictionaries are not allowed"));
    }
    let (data, consumed) = inflate(&input[2..], output_limit)?;
    let trailer = input.get(2 + consumed..2 + consumed + 4).ok_or(DecodeError::Truncated)?;
    let expected = u32::from_be_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
    let mut adler = Adler32::new();
    adler.update(&data);
    let actual = adler.finish();
    if actual != expected {
        return Err(DecodeError::ChecksumMismatch { kind: "zlib Adler-32", expected, actual });
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_reference_stream() {
        // CPython: zlib.compress(b"terrarium", 9)
        let stream = [0x78, 0xDA, 0x2B, 0x49, 0x2D, 0x2A, 0x4A, 0x2C, 0xCA, 0x2C, 0xCD, 0x05, 0x00, 0x13, 0x4E, 0x03, 0xDC];
        assert_eq!(zlib_decompress(&stream, 64).expect("valid zlib"), b"terrarium");
        let mut corrupted = stream;
        corrupted[16] ^= 1;
        assert!(zlib_decompress(&corrupted, 64).is_err());
    }
}
