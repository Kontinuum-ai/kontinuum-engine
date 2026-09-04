//! Broadband kick-sidechain duck (#76): the mixer's single ducking
//! implementation. One node per track, keyed by kick onsets exactly like
//! the #27 bass carve; depth is a per-track parameter with full range to
//! unity (1.0 = attenuate to silence at full key), defaulted per
//! [`crate::mix::MixRole::duck_depth`], and the release τ is settable so
//! groove/genre templates can retime the pump (the template plumbing lives
//! outside core — hosts call `AutoMixer::set_duck_release_ms`).
//!
//! Same rules as the rest of the engine: allocation-free render path,
//! deterministic, bounded, and slewed — the applied gain moves through
//! attack/release one-poles, so duck edges are click-free by construction.
//! At depth 0 (or an idle key) the applied gain is exactly 1.0 and the
//! multiply is bit-exact passthrough.

use crate::voice::flush_denormal;

/// Duck release τ (ms), one-pole: ~95 % recovered at 3τ ≈ 480 ms — one
/// full beat at 120–126 BPM (476–500 ms). The duck therefore still
/// releases as the next kick lands: an audible pump across the beat, not
/// transient clearing (the retired fixed 120 ms recovered inside a third
/// of a beat — issue #76's "not a pump" observation).
pub const DUCK_RELEASE_MS: f32 = 160.0;
/// Release setter bounds (ms) — hard-clamped musical range.
pub const DUCK_RELEASE_MIN_MS: f32 = 20.0;
pub const DUCK_RELEASE_MAX_MS: f32 = 1_000.0;
/// Duck attack (ms): fast enough to clear the kick transient, slow enough
/// that the gain step is slewed below audibility without pre-delay.
const DUCK_ATTACK_MS: f32 = 5.0;

/// One track's broadband duck: a key envelope raised by kick onsets, with
/// the applied attenuation slewed toward `depth · key` at audio rate.
pub struct DuckNode {
    /// Attenuation at full key, 0..1 (1.0 = duck to unity/silence).
    depth: f32,
    /// Key envelope 0..1, raised by kick onsets, released per sample.
    key_env: f32,
    /// Current applied attenuation 0..1 (slewed toward `depth · key_env`).
    att: f32,
    key_release: f32,
    /// The configured release τ, for telemetry/tests.
    release_ms: f32,
    attack_coeff: f32,
}

impl DuckNode {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let mut node = DuckNode {
            depth: 0.0,
            key_env: 0.0,
            att: 0.0,
            key_release: 1.0,
            release_ms: DUCK_RELEASE_MS,
            attack_coeff: 1.0,
        };
        node.set_release_ms(sr, DUCK_RELEASE_MS);
        node.attack_coeff = 1.0 - (-1000.0 / (DUCK_ATTACK_MS * sr)).exp();
        node
    }

    /// Release τ of the recovery; non-finite falls back to the default and
    /// everything is clamped to the musical range.
    pub fn set_release_ms(&mut self, sample_rate: f32, ms: f32) {
        let ms = if ms.is_finite() { ms } else { DUCK_RELEASE_MS };
        let ms = ms.clamp(DUCK_RELEASE_MIN_MS, DUCK_RELEASE_MAX_MS);
        self.release_ms = ms;
        self.key_release = (-1000.0 / (ms * sample_rate)).exp();
    }

    /// The configured release τ (clamped), ms.
    pub fn release_ms(&self) -> f32 {
        self.release_ms
    }

    /// Attenuation depth 0..1 (clamped); 0 = bypass, 1 = to unity at full
    /// key. Non-finite is treated as 0.
    pub fn set_depth(&mut self, depth: f32) {
        self.depth = if depth.is_finite() { depth.clamp(0.0, 1.0) } else { 0.0 };
    }

    pub fn depth(&self) -> f32 {
        self.depth
    }

    /// Sidechain key from a kick onset (velocity 0..1).
    pub fn key_hit(&mut self, velocity: f32) {
        if velocity.is_finite() {
            self.key_env = self.key_env.max(velocity.clamp(0.0, 1.0));
        }
    }

    pub fn reset(&mut self) {
        self.key_env = 0.0;
        self.att = 0.0;
    }

    pub fn process(&mut self, io: &mut [f32]) {
        // Key is constant across a tile apart from its own release: fold
        // the per-sample release into one per-tile decay of the key, then
        // slew the attenuation toward `depth · key` per sample (same shape
        // as the #27 bass carve). Attack rises fast, recovery falls at the
        // release τ.
        let tile_key_decay = self.key_release.powi(io.len() as i32);
        let target = self.depth * self.key_env;
        for slot in io.iter_mut() {
            let rising = target > self.att;
            let coeff = if rising { self.attack_coeff } else { 1.0 - self.key_release };
            self.att += coeff * (target - self.att);
            self.att = flush_denormal(self.att);
            *slot *= 1.0 - self.att;
        }
        self.key_env = flush_denormal(self.key_env * tile_key_decay);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    fn tone(freq: f32, amp: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| amp * (std::f32::consts::TAU * freq * i as f32 / SR as f32).sin()).collect()
    }

    fn rms(buf: &[f32]) -> f32 {
        (buf.iter().map(|s| s * s).sum::<f32>() / buf.len().max(1) as f32).sqrt()
    }

    #[test]
    fn depth_zero_is_bit_exact_passthrough_even_keyed() {
        let mut node = DuckNode::new(SR);
        node.key_hit(1.0);
        let mut buf = tone(200.0, 0.5, SR as usize);
        let reference = buf.clone();
        for chunk in buf.chunks_mut(crate::BLOCK_FRAMES) {
            node.process(chunk);
        }
        assert!(buf.iter().zip(reference.iter()).all(|(a, b)| a.to_bits() == b.to_bits()),
            "depth 0 ducked the signal");
    }

    #[test]
    fn full_depth_bottoms_near_unity_and_recovers() {
        let mut node = DuckNode::new(SR);
        node.set_depth(1.0);
        let mut frames = tone(200.0, 0.5, SR as usize);
        // Steady key for ~27 ms (≈ 5 attack τ): the last tile's gain has
        // settled at 1 − depth·key ≈ 0.
        for chunk in frames.chunks_mut(crate::BLOCK_FRAMES).take(40) {
            node.key_hit(1.0);
            node.process(chunk);
        }
        let last = 40 * crate::BLOCK_FRAMES;
        let floor_rms = rms(&frames[last - crate::BLOCK_FRAMES..last]);
        assert!(floor_rms < 0.05, "full-depth duck did not approach unity: {floor_rms}");
        // ~24 release τ later the key tail is fully flushed and the tone is
        // back at its full level.
        for chunk in frames.chunks_mut(crate::BLOCK_FRAMES).skip(40) {
            node.process(chunk);
        }
        let recovered = rms(&frames[frames.len() - crate::BLOCK_FRAMES..]);
        assert!(recovered > 0.33, "duck did not recover: {recovered}");
    }

    #[test]
    fn duck_move_is_click_free() {
        let mut node = DuckNode::new(SR);
        node.set_depth(1.0);
        let n = SR as usize;
        let mut buf = tone(200.0, 0.5, n);
        let half = n / 2;
        let mut max_delta = 0.0f32;
        let mut prev = buf[0];
        for (k, chunk) in buf.chunks_mut(crate::BLOCK_FRAMES).enumerate() {
            if k * crate::BLOCK_FRAMES >= half {
                node.key_hit(1.0);
            }
            for slot in chunk.iter_mut() {
                node.process(std::slice::from_mut(slot));
                max_delta = max_delta.max((*slot - prev).abs());
                prev = *slot;
            }
        }
        // 200 Hz tone's natural slew is ~0.013; the 5 ms attack slew adds
        // ~2.6e-3 per sample at the edge. An unslewed step would be ~0.5.
        assert!(max_delta < 0.02, "duck edge clicked: {max_delta} per sample");
    }

    #[test]
    fn release_setter_is_clamped_and_finite_safe() {
        let mut node = DuckNode::new(SR);
        node.set_release_ms(SR as f32, f32::NAN);
        node.set_release_ms(SR as f32, 0.0);
        node.set_release_ms(SR as f32, 60_000.0);
        // Whatever the input, the node must stay usable and bounded.
        node.set_depth(1.0);
        node.key_hit(1.0);
        let mut buf = tone(100.0, 0.5, crate::BLOCK_FRAMES);
        node.process(&mut buf);
        assert!(buf.iter().all(|s| s.is_finite()));
    }
}
