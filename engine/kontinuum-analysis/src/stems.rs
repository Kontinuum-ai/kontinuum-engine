//! Per-stem critic (issue #25): the engine has clean stems for free, so
//! each bus (kick / bass / perc / pad) gets its own rolling loudness,
//! spectral centroid and transient density, plus the kick↔bass masking
//! check that auto-mixing (#27) and the mixing telemetry consume.
//!
//! Consumers: #27 gain staging (per-stem loudness), #26 reward model and
//! #15 kill-switch (snapshots are serde-serializable), composer context
//! (#22).
//!
//! Formulas (documented per the #25 spec):
//! * Per-stem loudness — BS.1770 K-weighting on the single stem channel,
//!   mean-square per 100 ms slot, 3 s short-term read (single-channel
//!   sum: G = 1.0 for the one channel, no stereo sum).
//! * Transient density — spectral flux above 3 kHz on 1024/512
//!   window/hop (same shape as `metrics::analyze`), peaks above
//!   mean + 3σ of the trailing flux ring (tighter than the offline
//!   1.5σ — see `dsp::FluxTracker::transients_per_sec`), floor at 10⁻³
//!   of the session max magnitude; density = peaks per second across the
//!   ring span.
//! * Bass↔kick collision — both stems are band-passed 30–120 Hz
//!   (2nd-order HP 30 Hz + LP 120 Hz, Q 0.7071); mean-square per 30 ms
//!   slot gives envelope series x (kick) and y (bass) and the collision
//!   index is C = Σ min(x,y) / Σ max(x,y) over the trailing ≤20 s.
//!   Scale-invariant, 0..1: → 1 when the two are simultaneously active
//!   at similar energy, → 0 when disjoint in time or spectrally
//!   separated (an out-of-band bass drives y → 0).

use serde::{Deserialize, Serialize};

use crate::dsp::{FluxTracker, SlotRing, SpectralTracker};
use crate::filters::{
    highpass_coeffs, lowpass_coeffs, lufs_db, Biquad, KWeighter,
};

/// Engine stem buses, fixed order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StemId {
    Kick,
    Bass,
    Perc,
    Pad,
}

impl StemId {
    pub const ALL: [StemId; 4] = [StemId::Kick, StemId::Bass, StemId::Perc, StemId::Pad];

    /// Position in [`StemId::ALL`] / [`StemBoardSnapshot::stems`].
    pub fn index(self) -> usize {
        match self {
            StemId::Kick => 0,
            StemId::Bass => 1,
            StemId::Perc => 2,
            StemId::Pad => 3,
        }
    }
}

/// Masking band for the kick↔bass collision check (Hz).
pub const MASK_LO_HZ: f64 = 30.0;
pub const MASK_HI_HZ: f64 = 120.0;
const MASK_SLOT_SEC: f64 = 0.03;
/// ~20 s of 30 ms collision slots.
const MASK_RING: usize = 667;
const LOUDNESS_SLOTS: usize = 600; // 60 s
const SHORT_TERM_SLOTS: usize = 30; // 3 s
const STEM_SPECTRAL_WINDOW: usize = 4096;
const FLUX_RING: usize = 256;

/// Folded reading for one stem. All Copy — snapshots never allocate.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct StemSnapshot {
    pub stem: StemId,
    /// 3 s K-weighted solo loudness of the stem (LUFS).
    pub short_term_lufs: f64,
    /// Power-weighted spectral centroid (Hz).
    pub centroid_hz: f64,
    /// Detected transients per second (spectral-flux peaks).
    pub transients_per_sec: f64,
}

/// Folded reading of the whole stem board, including the masking check.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct StemBoardSnapshot {
    /// One entry per [`StemId::ALL`], in that order.
    pub stems: [StemSnapshot; 4],
    /// Kick↔bass 30–120 Hz energy collision index (0..1, see module docs).
    pub bass_kick_collision: f64,
    /// Signal time pushed so far (s) — consumers gate on warm-up.
    pub seconds: f64,
}

impl StemBoardSnapshot {
    pub fn stem(&self, id: StemId) -> StemSnapshot {
        self.stems[id.index()]
    }
}

struct StemTracker {
    slot_len: usize,
    mask_len: usize,
    kw: KWeighter,
    k_slots: SlotRing,
    k_acc: f64,
    k_n: usize,
    spectral: SpectralTracker,
    flux: FluxTracker,
    band_hp: Biquad,
    band_lp: Biquad,
    band_acc: f64,
    band_n: usize,
    band_slots: SlotRing,
}

impl StemTracker {
    fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f64;
        let slot_len = (sr * 0.1).round() as usize;
        let mask_len = (sr * MASK_SLOT_SEC).round() as usize;
        let mut t = StemTracker {
            slot_len,
            mask_len,
            kw: KWeighter::new(sample_rate),
            k_slots: SlotRing::new(LOUDNESS_SLOTS),
            k_acc: 0.0,
            k_n: 0,
            spectral: SpectralTracker::new(sample_rate, STEM_SPECTRAL_WINDOW),
            flux: FluxTracker::new(sample_rate, FLUX_RING),
            band_hp: Biquad::identity(),
            band_lp: Biquad::identity(),
            band_acc: 0.0,
            band_n: 0,
            band_slots: SlotRing::new(MASK_RING),
        };
        t.band_hp.set_coeffs(highpass_coeffs(sr, MASK_LO_HZ, std::f64::consts::FRAC_1_SQRT_2));
        t.band_lp.set_coeffs(lowpass_coeffs(sr, MASK_HI_HZ, std::f64::consts::FRAC_1_SQRT_2));
        t
    }

    fn push(&mut self, x: f32) {
        let x = if x.is_finite() { x } else { 0.0 };
        let k = self.kw.tick(x, 0.0).0;
        self.k_acc += (k * k) as f64;
        self.k_n += 1;
        let mono = x as f64;
        self.spectral.push(mono);
        self.flux.push(mono);
        let band = self.band_lp.tick(self.band_hp.tick(x));
        self.band_acc += (band * band) as f64;
        self.band_n += 1;
        if self.k_n == self.slot_len {
            self.k_slots.push(self.k_acc / self.k_n as f64);
            self.k_acc = 0.0;
            self.k_n = 0;
        }
        if self.band_n == self.mask_len {
            self.band_slots.push(self.band_acc / self.band_n as f64);
            self.band_acc = 0.0;
            self.band_n = 0;
        }
    }
}

/// Rolling per-stem critic board. One `push_block` per stem per block;
/// all state preallocated, the push path allocates nothing.
pub struct StemBoard {
    sr_hz: f64,
    stems: [StemTracker; 4],
    frames: u64,
}

impl StemBoard {
    pub fn new(sample_rate: u32) -> Self {
        StemBoard {
            sr_hz: sample_rate as f64,
            stems: std::array::from_fn(|_| StemTracker::new(sample_rate)),
            frames: 0,
        }
    }

    /// Feed one mono block from a stem bus. Allocates nothing.
    pub fn push_block(&mut self, stem: StemId, block: &[f32]) {
        let t = &mut self.stems[stem.index()];
        for &x in block {
            t.push(x);
        }
        self.frames += block.len() as u64;
    }

    pub fn snapshot(&self) -> StemBoardSnapshot {
        let stems = std::array::from_fn(|i| {
            let t = &self.stems[i];
            StemSnapshot {
                stem: StemId::ALL[i],
                short_term_lufs: lufs_db(
                    t.k_slots.tail(SHORT_TERM_SLOTS).sum::<f64>()
                        / t.k_slots.len().clamp(1, SHORT_TERM_SLOTS) as f64,
                ),
                centroid_hz: t.spectral.centroid_hz,
                transients_per_sec: t.flux.transients_per_sec(),
            }
        });
        // Un-pushed stems read the finite loudness floor, never NaN.
        let seconds = self.frames as f64 / self.sr_hz;
        StemBoardSnapshot {
            stems,
            bass_kick_collision: collision(
                &collect_tail(&self.stems[StemId::Kick.index()].band_slots),
                &collect_tail(&self.stems[StemId::Bass.index()].band_slots),
            ),
            seconds,
        }
    }
}

fn collect_tail(ring: &SlotRing) -> Vec<f64> {
    ring.iter().collect()
}

/// Σ min(x,y) / Σ max(x,y) over the shorter of the two slot histories;
/// 0.0 when either history is empty.
fn collision(kick: &[f64], bass: &[f64]) -> f64 {
    let n = kick.len().min(bass.len());
    if n == 0 {
        return 0.0;
    }
    let (kick, bass) = (&kick[kick.len() - n..], &bass[bass.len() - n..]);
    let mut mins = 0.0f64;
    let mut maxs = 0.0f64;
    for i in 0..n {
        mins += kick[i].min(bass[i]);
        maxs += kick[i].max(bass[i]);
    }
    if maxs > 0.0 {
        (mins / maxs).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    fn feed(board: &mut StemBoard, id: StemId, seconds: f64, f: impl Fn(usize) -> f32) {
        let n = (SR as f64 * seconds) as usize;
        let block: Vec<f32> = (0..n).map(f).collect();
        board.push_block(id, &block);
    }

    fn sine(hz: f64, amp: f64, i: usize) -> f32 {
        (amp * (std::f64::consts::TAU * hz * i as f64 / SR as f64).sin()) as f32
    }

    /// Gated tone: `gate_hz` duty-cycled sine bursts.
    fn gated(hz: f64, amp: f64, gate_hz: f64, i: usize) -> f32 {
        let t = i as f64 / SR as f64;
        let on = (t * gate_hz).fract() < 0.5;
        if on {
            sine(hz, amp, i)
        } else {
            0.0
        }
    }

    #[test]
    fn stem_loudness_tracks_level() {
        // 997 Hz is the EBU-anchored calibration point. A stem is a
        // single channel: mean square is half the dual-mono convention,
        // so a soloed sine reads ≈ 20·log10(A) − 3 dB LUFS.
        let mut b = StemBoard::new(SR);
        feed(&mut b, StemId::Bass, 4.0, |i| sine(997.0, 0.5, i));
        let loud = b.snapshot().stem(StemId::Bass).short_term_lufs;
        let mut b = StemBoard::new(SR);
        feed(&mut b, StemId::Bass, 4.0, |i| sine(997.0, 0.25, i));
        let quiet = b.snapshot().stem(StemId::Bass).short_term_lufs;
        assert!(loud > -10.0 && loud < -8.0, "solo loudness {loud}");
        assert!(((loud - quiet) - 6.0).abs() < 0.3, "halving amp must drop ~6 dB: {loud} vs {quiet}");
    }

    #[test]
    fn transient_density_counts_kicks_and_ignores_pads() {
        let mut b = StemBoard::new(SR);
        // Kick-ish: 2 Hz thumps with a hard amplitude step at the gate
        // (phase offset 1.7 rad) so each onset is one broadband click
        // instead of a fade-in smeared across two flux hops.
        feed(&mut b, StemId::Kick, 5.0, |i| {
            let t = (i % (SR as usize / 2)) as f64 / SR as f64;
            (0.9 * (-t * 60.0).exp() * (std::f64::consts::TAU * 60.0 * t + 1.7).sin()) as f32
        });
        let kick = b.snapshot().stem(StemId::Kick);
        assert!((kick.transients_per_sec - 2.0).abs() < 0.7,
            "kick density {}", kick.transients_per_sec);

        let mut b = StemBoard::new(SR);
        feed(&mut b, StemId::Pad, 5.0, |i| sine(220.0, 0.3, i));
        let pad = b.snapshot().stem(StemId::Pad);
        assert!(pad.transients_per_sec < 1.0, "steady pad density {}", pad.transients_per_sec);
    }

    #[test]
    fn collision_separates_planted_overlap_from_separated_spectra() {
        // Planted mud: kick and bass share 50 Hz and fire together.
        let mut b = StemBoard::new(SR);
        feed(&mut b, StemId::Kick, 8.0, |i| gated(50.0, 0.9, 2.0, i));
        feed(&mut b, StemId::Bass, 8.0, |i| gated(50.0, 0.9, 2.0, i));
        let muddy = b.snapshot().bass_kick_collision;

        // Same rhythm, but the bass lives at 200 Hz — outside 30–120 Hz.
        let mut b = StemBoard::new(SR);
        feed(&mut b, StemId::Kick, 8.0, |i| gated(50.0, 0.9, 2.0, i));
        feed(&mut b, StemId::Bass, 8.0, |i| gated(200.0, 0.9, 2.0, i));
        let separated = b.snapshot().bass_kick_collision;

        assert!(muddy > 0.8, "aligned same-band energy must collide: {muddy}");
        assert!(separated < 0.2, "out-of-band bass must not collide: {separated}");
    }

    #[test]
    fn collision_stays_low_when_kick_and_bass_never_overlap_in_time() {
        let mut b = StemBoard::new(SR);
        // Bass on while kick is off, alternating halves of every 0.5 s.
        feed(&mut b, StemId::Kick, 8.0, |i| {
            let on = (i as f64 / SR as f64 * 2.0).fract() < 0.5;
            if on { sine(50.0, 0.9, i) } else { 0.0 }
        });
        feed(&mut b, StemId::Bass, 8.0, |i| {
            let on = (i as f64 / SR as f64 * 2.0).fract() >= 0.5;
            if on { sine(50.0, 0.9, i) } else { 0.0 }
        });
        let c = b.snapshot().bass_kick_collision;
        assert!(c < 0.35, "time-disjoint stems must not collide hard: {c}");
    }

    #[test]
    fn silence_keeps_every_stem_finite() {
        let mut b = StemBoard::new(SR);
        feed(&mut b, StemId::Kick, 2.0, |_| 0.0);
        feed(&mut b, StemId::Bass, 2.0, |_| f32::NAN);
        let s = b.snapshot();
        for st in &s.stems {
            assert!(st.short_term_lufs.is_finite() && st.short_term_lufs < -200.0);
            assert!(st.centroid_hz.is_finite());
            assert!(st.transients_per_sec.is_finite());
        }
        assert_eq!(s.bass_kick_collision, 0.0);
    }
}
