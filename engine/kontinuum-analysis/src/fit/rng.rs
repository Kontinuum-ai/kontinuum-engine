//! Deterministic seeding for the fitter (issue #75): SplitMix64, the
//! standard 64-bit mixing generator. Same seed → same stream on every
//! platform, so restarts and fixtures are reproducible bit-for-bit.

/// SplitMix64 counter-based generator (Steele et al., public domain).
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    pub const fn new(seed: u64) -> Self {
        SplitMix64 { state: seed }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in 0..1 (53-bit mantissa draw).
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// Uniform in `lo..hi`.
    pub fn next_range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + self.next_f64() * (hi - lo)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_stream() {
        let mut a = SplitMix64::new(42);
        let mut b = SplitMix64::new(42);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn draws_stay_in_range_and_vary() {
        let mut r = SplitMix64::new(1);
        let mut sum = 0.0;
        for _ in 0..10_000 {
            let v = r.next_range(-2.0, 5.0);
            assert!((-2.0..5.0).contains(&v));
            sum += v;
        }
        assert!((sum / 10_000.0 - 1.5).abs() < 0.2, "mean drifted: {}", sum / 10_000.0);
    }
}
