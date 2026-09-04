//! f16 embedding blobs (#20 storage format): half precision halves the
//! memory per vector, and every f16 value is exactly representable in f32,
//! so a stored row decodes bit-exactly. [`StoredEmbedding`] is the
//! storage-only handle the query executor scores through — computing
//! vectors is the build pipeline's job, never the catalog's.

use crate::query::AudioEmbedding;

/// f32 → f16 bit pattern, round-to-nearest-even. Every f16 value is exactly
/// representable in f32, so the decode below is bit-exact.
pub(crate) fn f16_bits_from_f32(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32;
    let mant = bits & 0x007f_ffff;
    if exp == 0xff {
        // Inf stays inf; NaN keeps its top mantissa bit so it stays NaN.
        return sign | 0x7c00 | (u16::from(mant != 0) << 9);
    }
    let new_exp = exp - 112; // f16 bias 15 vs f32 bias 127
    if exp == 0 || new_exp < -10 {
        // f32 subnormal, true zero, or below f16's smallest-subnormal
        // halfway point: all round to (signed) zero — the halfway case
        // rounds to even, which is zero.
        return sign;
    }
    if new_exp >= 31 {
        // Overflow rounds to infinity.
        return sign | 0x7c00;
    }
    if new_exp <= 0 {
        // f16 subnormal: keep the implicit f32 bit, round the tail off.
        let full = mant | 0x0080_0000;
        let shift = (14 - new_exp) as u32;
        let mut half = (full >> shift) as u16;
        let rem = full & ((1 << shift) - 1);
        let halfway = 1 << (shift - 1);
        if rem > halfway || (rem == halfway && half & 1 == 1) {
            half += 1; // may carry to 0x400 = smallest f16 normal
        }
        return sign | half;
    }
    let mut half = sign | ((new_exp as u16) << 10) | ((mant >> 13) as u16);
    let rem = mant & 0x1fff;
    if rem > 0x1000 || (rem == 0x1000 && half & 1 == 1) {
        half += 1; // may carry into the next exponent; reaching inf is correct
    }
    half
}

/// f16 bit pattern → f32 (exact).
pub(crate) fn f32_from_f16_bits(bits: u16) -> f32 {
    let sign = u32::from(bits & 0x8000) << 16;
    let exp = u32::from((bits >> 10) & 0x1f);
    let mant = u32::from(bits & 0x03ff);
    let out = match exp {
        0 if mant == 0 => sign,
        0 => {
            // Subnormal: renormalize into an f32 normal.
            let msb = 31 - mant.leading_zeros(); // 0..=9
            sign | ((msb + 103) << 23) | ((mant - (1 << msb)) << (23 - msb))
        }
        0x1f => sign | 0x7f80_0000 | (mant << 13),
        e => sign | ((e + 112) << 23) | (mant << 13),
    };
    f32::from_bits(out)
}

pub(crate) fn embedding_to_blob(vector: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vector.len() * 2);
    for s in vector {
        out.extend_from_slice(&f16_bits_from_f32(*s).to_le_bytes());
    }
    out
}

/// Undecodable (odd-length) blobs read as absent: a corrupt embedding never
/// blocks catalog reads — integrity hashing catches the corruption.
pub(crate) fn embedding_from_blob(blob: &[u8]) -> Option<Vec<f32>> {
    if blob.len() % 2 != 0 {
        return None;
    }
    Some(
        blob.chunks_exact(2)
            .map(|c| f32_from_f16_bits(u16::from_le_bytes([c[0], c[1]])))
            .collect(),
    )
}

/// A stored embedding row: bytes decoded, ready for cosine scoring behind
/// [`AudioEmbedding`].
pub struct StoredEmbedding {
    vector: Vec<f32>,
}

impl AudioEmbedding for StoredEmbedding {
    fn vector(&self) -> &[f32] {
        &self.vector
    }
}

impl From<Vec<f32>> for StoredEmbedding {
    fn from(vector: Vec<f32>) -> Self {
        StoredEmbedding { vector }
    }
}
