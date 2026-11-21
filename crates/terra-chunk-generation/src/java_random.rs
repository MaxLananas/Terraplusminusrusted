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
