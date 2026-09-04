//! Frequency-selective carves (#27): one kick-keyed dynamic node on bass
//! resolving the 30–120 Hz kick/bass collision, plus bounded mask nodes
//! fed by the #25 critic's energy-overlap input.
//!
//! The carve primitive is `y = x − depth·(LP_hi(x) − LP_lo(x))`: a band
//! extracted as the difference of two one-pole lowpasses, cut by `depth`
//! (0 = exact passthrough, bit-exact). Depth dynamics are always slewed at
//! audio rate — moves are click-free by construction.
//!
//! Bass node keying is **sidechain-keyed** (chosen over spectral-keyed):
//! kick onsets arrive from the event stream at zero analysis cost, and the
//! collision window is exactly when the kick sounds. Spectral keying is the
//! documented fallback if off-grid kicks ever matter.

use crate::fx::lp_coeff;
use crate::mix::servo::{db_to_lin, lin_to_db};
use crate::voice::flush_denormal;

/// Bass node cut cap — issue #27's ≤4 dB bound.
pub const BASS_CUT_MAX_DB: f32 = 4.0;
/// Kick/bass collision band (#27: 30–120 Hz).
pub const BASS_BAND_LO_HZ: f32 = 30.0;
pub const BASS_BAND_HI_HZ: f32 = 120.0;
/// Node ballistics: fast enough to clear the kick, musical on the way out.
const BASS_ATTACK_MS: f32 = 8.0;
const BASS_RELEASE_MS: f32 = 250.0;

/// Mask carve cap per node (dB).
pub const MASK_CUT_MAX_DB: f32 = 3.0;
/// Mask depth smoothing — slow, the #25 critic moves at arrangement rate.
pub const MASK_SLEW_MS: f32 = 150.0;
/// Overlap fraction at which a node starts carving (0..1 input contract).
pub const MASK_ENGAGE: f32 = 0.4;
/// Mask band configuration limits (Hz).
pub const MASK_BAND_MIN_HZ: f32 = 30.0;
pub const MASK_BAND_MAX_HZ: f32 = 16_000.0;
/// #27 budget: two mask nodes per track.
pub const MASK_NODES_PER_TRACK: usize = 2;

/// Band cut as a difference of one-pole lowpasses. `depth` in 0..1
/// (amplitude removed from the band); depth 0 is bit-exact passthrough.
struct BandCarve {
    hi_lp: f32,
    lo_lp: f32,
    hi_a: f32,
    lo_a: f32,
}

impl BandCarve {
    fn new(sample_rate: f32, lo_hz: f32, hi_hz: f32) -> Self {
        let mut c = BandCarve { hi_lp: 0.0, lo_lp: 0.0, hi_a: 0.0, lo_a: 0.0 };
        c.set_band(sample_rate, lo_hz, hi_hz);
        c
    }

    fn set_band(&mut self, sample_rate: f32, lo_hz: f32, hi_hz: f32) {
        let hi = hi_hz.clamp(MASK_BAND_MIN_HZ, MASK_BAND_MAX_HZ);
        let lo = lo_hz.clamp(MASK_BAND_MIN_HZ, hi - 10.0);
        self.hi_a = lp_coeff(sample_rate, hi);
        self.lo_a = lp_coeff(sample_rate, lo);
    }

    #[inline]
    fn process(&mut self, x: f32, depth: f32) -> f32 {
        self.hi_lp += self.hi_a * (x - self.hi_lp);
        self.lo_lp += self.lo_a * (x - self.lo_lp);
        self.hi_lp = flush_denormal(self.hi_lp);
        self.lo_lp = flush_denormal(self.lo_lp);
        x - depth * (self.hi_lp - self.lo_lp)
    }

    fn reset(&mut self) {
        self.hi_lp = 0.0;
        self.lo_lp = 0.0;
    }
}

/// Amplitude removed from the band for a `cut_db` reduction.
fn depth_for_cut(cut_db: f32) -> f32 {
    1.0 - db_to_lin(-cut_db)
}

/// Depth at which a node counts as active: above a 0.1 dB cut, the
/// residual from a decaying key tail (≈0.07 dB) stays "idle".
const ACTIVE_CUT_DB: f32 = 0.1;

fn is_active_depth(depth: f32) -> bool {
    depth > depth_for_cut(ACTIVE_CUT_DB)
}

/// Kick-keyed dynamic node on bass: bounded cut in the 30–120 Hz band,
/// fast attack on the kick onset, musical release.
pub struct BassNode {
    carve: BandCarve,
    /// Key envelope 0..1, raised by kick onsets, released per sample.
    key_env: f32,
    release_coeff: f32,
    attack_coeff: f32,
    depth: f32,
}

impl BassNode {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        BassNode {
            carve: BandCarve::new(sr, BASS_BAND_LO_HZ, BASS_BAND_HI_HZ),
            key_env: 0.0,
            release_coeff: (-1000.0 / (BASS_RELEASE_MS * sr)).exp(),
            attack_coeff: 1.0 - (-1000.0 / (BASS_ATTACK_MS * sr)).exp(),
            depth: 0.0,
        }
    }

    /// Sidechain key from a kick onset (velocity 0..1).
    pub fn key_hit(&mut self, velocity: f32) {
        if velocity.is_finite() {
            self.key_env = self.key_env.max(velocity.clamp(0.0, 1.0));
        }
    }

    pub fn cut_db(&self) -> f32 {
        -lin_to_db(1.0 - self.depth)
    }

    pub fn is_active(&self) -> bool {
        is_active_depth(self.depth)
    }

    pub fn process(&mut self, io: &mut [f32]) {
        // Key is constant across a tile apart from its own release: fold
        // the per-sample release into one per-tile decay of the key, then
        // slew the depth toward it per sample.
        let tile_key_decay = self.release_coeff.powi(io.len() as i32);
        let target = depth_for_cut(BASS_CUT_MAX_DB * self.key_env);
        for slot in io.iter_mut() {
            let rising = target > self.depth;
            let coeff = if rising { self.attack_coeff } else { 1.0 - self.release_coeff };
            self.depth += coeff * (target - self.depth);
            self.depth = flush_denormal(self.depth);
            *slot = self.carve.process(*slot, self.depth);
        }
        self.key_env = flush_denormal(self.key_env * tile_key_decay);
    }

    pub fn reset(&mut self) {
        self.carve.reset();
        self.key_env = 0.0;
        self.depth = 0.0;
    }
}

/// Mask node: bounded carve driven by the #25 critic's overlap input.
///
/// Input contract (`AutoMixer::set_masking`): `overlap` is the normalized
/// energy overlap of the masked source inside this node's band, 0.0 = none,
/// 1.0 = total. Carving begins at [`MASK_ENGAGE`] and scales linearly to
/// [`MASK_CUT_MAX_DB`] at full overlap. Non-finite values are treated as 0.
pub struct MaskNode {
    carve: BandCarve,
    depth_target: f32,
    slew_coeff: f32,
    depth: f32,
}

impl MaskNode {
    /// Defaults per #27: node 0 = mud (200–500 Hz), node 1 = harshness
    /// (5–8 kHz). Bands are reconfigured via `set_band`.
    pub fn new(sample_rate: u32, lo_hz: f32, hi_hz: f32) -> Self {
        let sr = sample_rate as f32;
        MaskNode {
            carve: BandCarve::new(sr, lo_hz, hi_hz),
            depth_target: 0.0,
            slew_coeff: 1.0 - (-1000.0 / (MASK_SLEW_MS * sr)).exp(),
            depth: 0.0,
        }
    }

    pub fn set_band(&mut self, sample_rate: u32, lo_hz: f32, hi_hz: f32) {
        self.carve.set_band(sample_rate as f32, lo_hz, hi_hz);
    }

    /// #25 critic input — see the module contract above.
    pub fn set_overlap(&mut self, overlap: f32) {
        let o = if overlap.is_finite() { overlap.clamp(0.0, 1.0) } else { 0.0 };
        let over = ((o - MASK_ENGAGE).max(0.0) / (1.0 - MASK_ENGAGE)).min(1.0);
        self.depth_target = depth_for_cut(MASK_CUT_MAX_DB * over);
    }

    pub fn cut_db(&self) -> f32 {
        -lin_to_db(1.0 - self.depth)
    }

    pub fn is_active(&self) -> bool {
        is_active_depth(self.depth)
    }

    pub fn process(&mut self, io: &mut [f32]) {
        for slot in io.iter_mut() {
            self.depth += self.slew_coeff * (self.depth_target - self.depth);
            self.depth = flush_denormal(self.depth);
            *slot = self.carve.process(*slot, self.depth);
        }
    }

    pub fn reset(&mut self) {
        self.carve.reset();
        self.depth_target = 0.0;
        self.depth = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    fn tone(freq: f32, amp: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (std::f32::consts::TAU * freq * i as f32 / SR as f32).sin())
            .collect()
    }

    fn rms(buf: &[f32]) -> f32 {
        (buf.iter().map(|s| s * s).sum::<f32>() / buf.len().max(1) as f32).sqrt()
    }

    #[test]
    fn bass_node_engages_on_kick_and_is_bounded() {
        let mut carved = BassNode::new(SR);
        let dry = tone(70.0, 0.5, SR as usize);
        let mut wet = dry.clone();
        for chunk in wet.chunks_mut(crate::BLOCK_FRAMES) {
            // Steady 4-on-the-floor key through the measured window.
            carved.key_hit(1.0);
            carved.process(chunk);
        }
        let cut_db = -20.0 * (rms(&wet) / rms(&dry)).log10();
        assert!(cut_db > 0.5, "bass node did not engage: {cut_db} dB");
        assert!(cut_db <= BASS_CUT_MAX_DB + 0.2, "cut exceeded the cap: {cut_db} dB");
        assert!(carved.is_active());
    }

    #[test]
    fn bass_node_releases_musically() {
        let mut node = BassNode::new(SR);
        let mut frames = tone(70.0, 0.5, SR as usize * 2);
        // One hit, then silence from the key: must recover within ~1 s.
        node.key_hit(1.0);
        let mut recovered_at = None;
        for (k, chunk) in frames.chunks_mut(crate::BLOCK_FRAMES).enumerate() {
            node.process(chunk);
            let done = k * crate::BLOCK_FRAMES;
            if recovered_at.is_none() && !node.is_active() && done > SR as usize / 2 {
                recovered_at = Some(done);
            }
        }
        let at = recovered_at.expect("bass node never released");
        assert!(at < SR as usize * 3 / 2, "release slower than 1.5 s: {at} frames");
        assert!(node.cut_db() < 0.1, "residual cut after release: {}", node.cut_db());
    }

    #[test]
    fn mask_cut_is_bounded_and_engagement_gated() {
        let mut node = MaskNode::new(SR, 200.0, 500.0);
        // Below the engagement point: no carve.
        node.set_overlap(0.3);
        let quiet = tone(300.0, 0.5, SR as usize);
        let mut buf = quiet.clone();
        for chunk in buf.chunks_mut(crate::BLOCK_FRAMES) {
            node.process(chunk);
        }
        assert!(node.cut_db() < 0.05, "sub-engagement overlap carved: {}", node.cut_db());
        // Full overlap: bounded cut ≈ cap.
        node.set_overlap(1.0);
        let mut buf = tone(300.0, 0.5, SR as usize);
        for chunk in buf.chunks_mut(crate::BLOCK_FRAMES) {
            node.process(chunk);
        }
        let cut_db = node.cut_db();
        assert!(cut_db > 1.0, "full overlap did not carve: {cut_db} dB");
        assert!(cut_db <= MASK_CUT_MAX_DB + 0.2, "mask cut exceeded cap: {cut_db} dB");
        // Non-finite input is ignored (treated as 0): the node must not
        // jump — depth keeps slewing from wherever it was.
        node.set_overlap(f32::NAN);
        assert!((node.cut_db() - MASK_CUT_MAX_DB).abs() < 0.3, "NaN overlap moved the node: {}", node.cut_db());
    }

    #[test]
    fn mask_move_is_click_free_on_dc() {
        let mut node = MaskNode::new(SR, 200.0, 500.0);
        let n = SR as usize;
        let mut buf = vec![0.3f32; n];
        let half = n / 2;
        let mut max_delta = 0.0f32;
        let mut prev = 0.3f32;
        for (k, chunk) in buf.chunks_mut(crate::BLOCK_FRAMES).enumerate() {
            if k * crate::BLOCK_FRAMES >= half {
                node.set_overlap(1.0);
            }
            for slot in chunk.iter_mut() {
                let before = prev;
                let mut tmp = [*slot];
                node.process(&mut tmp);
                *slot = tmp[0];
                max_delta = max_delta.max((*slot - before).abs());
                prev = *slot;
            }
        }
        assert!(max_delta < 1e-4, "mask move clicked: {max_delta} per sample");
        assert!(buf.iter().all(|s| (s - 0.3).abs() < 0.01), "DC wandered");
    }
}
