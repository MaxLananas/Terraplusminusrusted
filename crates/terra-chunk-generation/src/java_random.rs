//! `LegacyRandomSource` (the `java.util.Random` LCG) as wrapped by Paper's `RandomWrapper`.

const MULTIPLIER: i64 = 0x5_DEEC_E66D;
const ADDEND: i64 = 0xB;
const MASK: i64 = (1 << 48) - 1;

#[derive(Debug, Clone)]
pub struct LegacyRandom {
    seed: i64,
}

impl LegacyRandom {
    pub fn new(seed: i64) -> Self {
        Self { seed: (seed ^ MULTIPLIER) & MASK }
    }

    /// `WorldgenRandom.setLargeFeatureWithSalt(seed, x, z, salt)`, which Paper's
    /// `CustomChunkGenerator.buildSurface` applies with seed 0 and salt 0.
    pub fn large_feature_with_salt(seed: i64, x: i32, z: i32, salt: i32) -> Self {
        let mixed = seed
            .wrapping_add(i64::from(x).wrapping_mul(341_873_128_712))
            .wrapping_add(i64::from(z).wrapping_mul(132_897_987_541))
            .wrapping_add(i64::from(salt));
        Self::new(mixed)
    }

    /// `WorldgenRandom.setDecorationSeed(levelSeed, x, z)`, which Paper applies with the chunk
    /// coordinates before calling each Bukkit `BlockPopulator`.
    pub fn decoration(level_seed: i64, x: i32, z: i32) -> Self {
        let mut random = Self::new(level_seed);
        let a = random.next_long() | 1;
        let b = random.next_long() | 1;
        Self::new(i64::from(x).wrapping_mul(a).wrapping_add(i64::from(z).wrapping_mul(b)) ^ level_seed)
    }

    /// The raw LCG state as `setSeed` would take it (for diagnostics and tests).
    pub fn scrambled_seed(&self) -> i64 {
        self.seed ^ MULTIPLIER
    }

    pub fn next_int(&mut self) -> i32 {
        self.next(32)
    }

    pub fn next_long(&mut self) -> i64 {
        (i64::from(self.next(32)) << 32).wrapping_add(i64::from(self.next(32)))
    }

    /// `java.util.Random.nextBytes`, which Paper's `RandomWrapper` inherits: one `nextInt()` per
    /// four bytes, low byte first.
    pub fn next_bytes(&mut self, bytes: &mut [u8]) {
        for chunk in bytes.chunks_mut(4) {
            let mut value = self.next_int();
            for byte in chunk {
                *byte = value as u8;
                value >>= 8;
            }
        }
    }

    fn next(&mut self, bits: u32) -> i32 {
        self.seed = (self.seed.wrapping_mul(MULTIPLIER).wrapping_add(ADDEND)) & MASK;
        (self.seed >> (48 - bits)) as i32
    }

    /// `BitRandomSource.nextInt(bound)`; `bound` must be positive.
    pub fn next_int_bounded(&mut self, bound: i32) -> i32 {
        if bound & (bound - 1) == 0 {
            return ((i64::from(bound) * i64::from(self.next(31))) >> 31) as i32;
        }
        loop {
            let bits = self.next(31);
            let value = bits % bound;
            if bits.wrapping_sub(value).wrapping_add(bound - 1) >= 0 {
                return value;
            }
        }
    }

    /// `RandomSource.nextInt(origin, bound)`: `origin + nextInt(bound - origin)`.
    pub fn next_int_between(&mut self, origin: i32, bound: i32) -> i32 {
        origin + self.next_int_bounded(bound - origin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Expected values printed on Temurin 25 by `java.util.Random` driven exactly as
    /// `WorldgenRandom.setDecorationSeed` drives `LegacyRandomSource` (same LCG), then
    /// `nextBytes(new byte[256])`, `nextInt(15)` twice, `nextInt()` and `nextBytes(new byte[7])`.
    #[test]
    fn decoration_seed_and_next_bytes_match_java() {
        /// Level seed, chunk x and z, decoration seed, bytes 0..6 and 255, two `nextInt(15)`,
        /// `nextInt()`, then seven more bytes.
        type Case = (i64, i32, i32, i64, [u8; 7], i32, i32, i32, [u8; 7]);
        let cases: [Case; 4] = [
            (0, 0, 0, 0, [96, 180, 32, 187, 56, 81, 171], 3, 12, 615_126_903, [172, 66, 242, 91, 108, 103, 132]),
            (42, 3, -7, 7_381_035_266_058_969_068, [119, 80, 146, 208, 176, 88, 98], 5, 6, -681_242_427, [27, 203, 206, 197, 55, 117, 221]),
            (
                -6_142_734_517_983_913_473,
                -123_456,
                98_765,
                -8_927_214_303_040_496_458,
                [255, 117, 156, 233, 145, 246, 77],
                0,
                9,
                -2_042_485_633,
                [7, 199, 127, 154, 13, 59, 207],
            ),
            (1_234_567_890_123, 1_875_000, -1, 6_837_280_280_407_045_202, [89, 186, 20, 33, 240, 155, 62], 0, 4, 402_398_463, [255, 190, 134, 102, 79, 218, 102]),
        ];
        for (level_seed, x, z, seed, bytes, first, second, int, odd) in cases {
            let mut random = LegacyRandom::decoration(level_seed, x, z);
            assert_eq!(random.scrambled_seed(), seed & MASK);
            let mut buffer = [0u8; 256];
            random.next_bytes(&mut buffer);
            assert_eq!(&buffer[..6], &bytes[..6]);
            assert_eq!(buffer[255], bytes[6]);
            assert_eq!((random.next_int_bounded(15), random.next_int_bounded(15), random.next_int()), (first, second, int));
            let mut seven = [0u8; 7];
            random.next_bytes(&mut seven);
            assert_eq!(seven, odd);
        }
    }

    /// Expected values printed by `java.util.Random` on Temurin 25 (same LCG and bounded-int
    /// algorithm as Minecraft's `LegacyRandomSource`).
    #[test]
    fn matches_java_util_random_sequences() {
        let mut random = LegacyRandom::new(42);
        assert_eq!([random.next_int_bounded(100), random.next_int_bounded(100), random.next_int_bounded(100)], [30, 63, 48]);
        assert_eq!(LegacyRandom::new(0).next_int_bounded(16), 11);
        let mut chunk = LegacyRandom::large_feature_with_salt(0, 5, -7, 0);
        let heights: Vec<i32> = (0..8).map(|_| chunk.next_int_between(7500, 7520)).collect();
        assert_eq!(heights, [7501, 7505, 7519, 7502, 7517, 7516, 7511, 7509]);
    }
}
