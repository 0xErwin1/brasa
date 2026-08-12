//! Shared `std::rand` glue (BRS-35, `docs/spec/05-stdlib.md`): one
//! hand-rolled, documented PRNG both backends hold per run, so a
//! seeded sequence is identical on the walker and the VM (and pinnable
//! in parity tests). Not cryptographic — scripting randomness only.
//!
//! The generator is xoshiro256** (Blackman & Vigna, public domain),
//! seeded through SplitMix64 as its authors recommend. An unseeded run
//! draws its seed from the system clock; `rand.seed(n)` resets the
//! state deterministically at any point.

/// The per-run PRNG state.
pub struct Rng {
    state: [u64; 4],
}

impl Rng {
    /// A deterministic generator: `seed` expands through SplitMix64
    /// into the full xoshiro256** state.
    pub fn seeded(seed: u64) -> Self {
        let mut x = seed;
        let mut split_mix = move || {
            x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = x;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        };

        Rng {
            state: [split_mix(), split_mix(), split_mix(), split_mix()],
        }
    }

    /// An entropy-seeded generator for unseeded runs: the system
    /// clock's nanoseconds, expanded through the same SplitMix64 path.
    pub fn from_entropy() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        Self::seeded(nanos as u64 ^ (nanos >> 64) as u64)
    }

    /// The next raw output (xoshiro256** step).
    pub fn next_u64(&mut self) -> u64 {
        let [s0, s1, s2, s3] = self.state;
        let result = s1.wrapping_mul(5).rotate_left(7).wrapping_mul(9);

        let t = s1 << 17;
        let s2 = s2 ^ s0;
        let s3 = s3 ^ s1;
        let s1 = s1 ^ s2;
        let s0 = s0 ^ s3;
        let s2 = s2 ^ t;
        let s3 = s3.rotate_left(45);
        self.state = [s0, s1, s2, s3];

        result
    }

    /// Uniform float in `[0, 1)`: the top 53 bits over 2^53.
    pub fn float(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// Uniform integer in `[0, n)` for `n > 0`, by modulo rejection
    /// sampling (no modulo bias).
    pub fn below(&mut self, n: u64) -> u64 {
        debug_assert!(n > 0, "below(0) has no value to pick");

        let threshold = n.wrapping_neg() % n;
        loop {
            let raw = self.next_u64();
            if raw >= threshold {
                return raw % n;
            }
        }
    }

    /// Uniform integer over a range literal's values; `None` when the
    /// range is empty. `inclusive` mirrors `..=`.
    pub fn int_in(&mut self, lo: i64, hi: i64, inclusive: bool) -> Option<i64> {
        let span = (hi as i128) - (lo as i128) + i128::from(inclusive);
        if span <= 0 {
            return None;
        }

        let offset = if span > u64::MAX as i128 {
            // Only `i64::MIN ..= i64::MAX`: every u64 is a valid offset.
            self.next_u64()
        } else {
            self.below(span as u64)
        };
        Some(((lo as i128) + offset as i128) as i64)
    }

    /// Fisher–Yates shuffle, so both backends permute identically for
    /// the same seed.
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i as u64 + 1) as usize;
            items.swap(i, j);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Rng;

    #[test]
    fn seeded_sequences_are_deterministic() {
        let mut a = Rng::seeded(42);
        let mut b = Rng::seeded(42);
        for _ in 0..16 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn float_stays_in_unit_interval() {
        let mut rng = Rng::seeded(7);
        for _ in 0..1_000 {
            let v = rng.float();
            assert!((0.0..1.0).contains(&v));
        }
    }

    #[test]
    fn int_in_respects_bounds_and_emptiness() {
        let mut rng = Rng::seeded(1);
        for _ in 0..1_000 {
            let v = rng.int_in(-3, 3, false).expect("non-empty");
            assert!((-3..3).contains(&v));
            let w = rng.int_in(-3, 3, true).expect("non-empty");
            assert!((-3..=3).contains(&w));
        }

        assert_eq!(rng.int_in(5, 5, false), None);
        assert_eq!(rng.int_in(6, 5, true), None);
        assert_eq!(rng.int_in(5, 5, true), Some(5));
        assert!(rng.int_in(i64::MIN, i64::MAX, true).is_some());
    }

    #[test]
    fn shuffle_is_a_permutation() {
        let mut rng = Rng::seeded(9);
        let mut items: Vec<i64> = (0..100).collect();
        rng.shuffle(&mut items);
        let mut sorted = items.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..100).collect::<Vec<_>>());
    }
}
