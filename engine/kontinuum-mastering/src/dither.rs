//! TPDF dither to 16-bit (offline export pass, #28 item 6).
//!
//! Triangular probability density function: the sum of two independent
//! uniform random variables, ±1 LSB wide. This decorrelates quantization
//! error from the signal without the noise floor penalty of larger
//! dither shapes. Deterministic: the noise derives from a seeded
//! `kontinuum_clock::Rng`, so the same (render, seed) always produces
//! the same file — no hidden entropy.

use kontinuum_clock::Rng;

/// Dithered 16-bit stereo pair.
#[derive(Clone, Debug, PartialEq)]
pub struct Dithered16 {
    pub left: Vec<i16>,
    pub right: Vec<i16>,
}

/// Quantize a stereo float render (−1..1 nominal) to 16-bit with TPDF
/// dither at the 16-bit LSB. Values are clamped to the i16 range.
pub fn dither_tpdf_16(left: &[f32], right: &[f32], seed: u64) -> Dithered16 {
    let mut rng = Rng::from_seed(seed);
    let quantize = |x: f32, rng: &mut Rng| -> i16 {
        // Two uniforms summed → triangular PDF, ±1 (integer) LSB wide.
        let t = rng.next_f32() as f64 + rng.next_f32() as f64 - 1.0;
        let scaled = x as f64 * 32767.0 + t;
        let rounded = scaled.round();
        rounded.clamp(-32768.0, 32767.0) as i16
    };
    let n = left.len().min(right.len());
    let mut out = Dithered16 { left: Vec::with_capacity(n), right: Vec::with_capacity(n) };
    for i in 0..n {
        out.left.push(quantize(left[i], &mut rng));
        out.right.push(quantize(right[i], &mut rng));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_per_seed_and_in_range() {
        let sr = 48_000u32;
        let input: Vec<f32> =
            (0..sr as usize).map(|i| 0.5 * (i as f32 * 0.013).sin()).collect();
        let a = dither_tpdf_16(&input, &input, 42);
        let b = dither_tpdf_16(&input, &input, 42);
        let c = dither_tpdf_16(&input, &input, 43);
        assert_eq!(a, b, "same seed must reproduce bit-identically");
        assert_ne!(a, c, "different seeds must differ");
        assert!(!a.left.is_empty());
    }

    #[test]
    fn dither_noise_floors_near_one_lsb() {
        // Dithering digital silence must produce tiny noise, not signal:
        // bounded to a couple of LSBs and centered on zero.
        let out = dither_tpdf_16(&[0.0; 4800], &[0.0; 4800], 7);
        let mean: f64 =
            out.left.iter().map(|s| *s as f64).sum::<f64>() / out.left.len() as f64;
        let max_lsb = out.left.iter().map(|s| s.abs()).max().unwrap_or(0);
        assert!(max_lsb <= 2, "silence dither hit {max_lsb} LSBs");
        assert!(mean.abs() < 0.2, "dither must not carry DC: {mean}");
    }
}
