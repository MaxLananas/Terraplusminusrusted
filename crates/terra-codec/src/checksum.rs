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

#[derive(Debug, Clone, Copy)]
pub(crate) struct Crc32(u32);

impl Crc32 {
    pub(crate) fn new() -> Self {
        Self(0xFFFF_FFFF)
    }

    pub(crate) fn update(&mut self, bytes: &[u8]) {
        let mut c = self.0;
        for &b in bytes {
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
