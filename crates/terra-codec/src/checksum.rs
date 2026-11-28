const fn crc32_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
}

static CRC32_TABLE: [u32; 256] = crc32_table();

/// Slicing-by-16 tables: `table[k][b]` is the CRC of byte `b` followed by `k` zero bytes.
const fn crc32_slicing_tables() -> [[u32; 256]; 16] {
    let mut tables = [[0u32; 256]; 16];
    tables[0] = crc32_table();
    let mut k = 1;
    while k < 16 {
        let mut n = 0;
        while n < 256 {
            let previous = tables[k - 1][n];
            tables[k][n] = (previous >> 8) ^ tables[0][(previous & 0xFF) as usize];
            n += 1;
        }
        k += 1;
    }
    tables
}

static CRC32_SLICING: [[u32; 256]; 16] = crc32_slicing_tables();

#[derive(Debug, Clone, Copy)]
pub(crate) struct Crc32(u32);

impl Crc32 {
    pub(crate) fn new() -> Self {
        Self(0xFFFF_FFFF)
    }

    /// Sixteen bytes per step (slicing-by-16), then byte by byte; the gzip trailer check of the
    /// 933 MB Köppen raster made the byte-wise loop the largest part of server startup.
    pub(crate) fn update(&mut self, bytes: &[u8]) {
        let t = &CRC32_SLICING;
        let mut c = self.0;
        let mut blocks = bytes.chunks_exact(16);
        for block in &mut blocks {
            let word = |i: usize| u32::from_le_bytes([block[i], block[i + 1], block[i + 2], block[i + 3]]);
            let first = word(0) ^ c;
            let (second, third, fourth) = (word(4), word(8), word(12));
            let byte = |value: u32, shift: u32| ((value >> shift) & 0xFF) as usize;
            c = t[15][byte(first, 0)]
                ^ t[14][byte(first, 8)]
                ^ t[13][byte(first, 16)]
                ^ t[12][byte(first, 24)]
                ^ t[11][byte(second, 0)]
                ^ t[10][byte(second, 8)]
                ^ t[9][byte(second, 16)]
                ^ t[8][byte(second, 24)]
                ^ t[7][byte(third, 0)]
                ^ t[6][byte(third, 8)]
                ^ t[5][byte(third, 16)]
                ^ t[4][byte(third, 24)]
                ^ t[3][byte(fourth, 0)]
                ^ t[2][byte(fourth, 8)]
                ^ t[1][byte(fourth, 16)]
                ^ t[0][byte(fourth, 24)];
        }
        for &b in blocks.remainder() {
            c = CRC32_TABLE[((c ^ u32::from(b)) & 0xFF) as usize] ^ (c >> 8);
        }
        self.0 = c;
    }

    pub(crate) fn finish(self) -> u32 {
        self.0 ^ 0xFFFF_FFFF
    }
}

pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = Crc32::new();
    crc.update(bytes);
    crc.finish()
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Adler32 {
    a: u32,
    b: u32,
}

impl Adler32 {
    pub(crate) fn new() -> Self {
        Self { a: 1, b: 0 }
    }

    pub(crate) fn update(&mut self, bytes: &[u8]) {
        const MODULUS: u32 = 65_521;
        // 5552 is the largest block length for which `b` cannot overflow u32 before reduction.
        for block in bytes.chunks(5552) {
            for &byte in block {
                self.a += u32::from(byte);
                self.b += self.a;
            }
            self.a %= MODULUS;
            self.b %= MODULUS;
        }
    }

    pub(crate) fn finish(self) -> u32 {
        (self.b << 16) | self.a
    }
}

pub fn adler32(bytes: &[u8]) -> u32 {
    let mut adler = Adler32::new();
    adler.update(bytes);
    adler.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_reference_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    /// The sliced update must equal the byte-wise definition for every length, alignment and
    /// split of the input.
    #[test]
    fn sliced_crc32_matches_the_bytewise_definition() {
        let bytewise = |bytes: &[u8]| {
            let mut c = 0xFFFF_FFFFu32;
            for &b in bytes {
                c = CRC32_TABLE[((c ^ u32::from(b)) & 0xFF) as usize] ^ (c >> 8);
            }
            c ^ 0xFFFF_FFFF
        };
        let mut state = 0x1234_5678u32;
        let data: Vec<u8> = (0..1000)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            })
            .collect();
        for start in 0..17 {
            for end in (start..data.len()).step_by(7) {
                let slice = &data[start..end];
                assert_eq!(crc32(slice), bytewise(slice), "{start}..{end}");
                let split = slice.len() / 3;
                let mut parts = Crc32::new();
                parts.update(&slice[..split]);
                parts.update(&slice[split..]);
                assert_eq!(parts.finish(), bytewise(slice));
            }
        }
    }

    #[test]
    fn adler32_matches_reference_vector() {
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        assert_eq!(adler32(b""), 1);
        let large = vec![0xFFu8; 100_000];
        let mut expected_a: u64 = 1;
        let mut expected_b: u64 = 0;
        for &byte in &large {
            expected_a = (expected_a + u64::from(byte)) % 65_521;
            expected_b = (expected_b + expected_a) % 65_521;
        }
        assert_eq!(adler32(&large), ((expected_b << 16) | expected_a) as u32);
    }
}
