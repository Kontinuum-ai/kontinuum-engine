//! Rolling master-bus critic (issue #25). `CriticEngine` accumulates
//! BS.1770-style loudness slots, crest, spectral shape, stereo and peak
//! statistics over rolling windows as blocks are pushed, and
//! `snapshot()` folds them into a `CriticSnapshot`.
//!
//! Consumers: the composer context (#22), the reward model (#26) and the
//! watchdog's kill-switch subset (#15) — all read snapshots, never audio.
//!
//! Real-time contract: `push_block*` allocates nothing (all buffers are
//! sized at construction; the single 8192-pt FFT fires in place at most
//! once per 8192 frames ≈ 6/s at 48 kHz). `snapshot()` allocates one
//! small `Vec` of slot values for the integrated gate and is meant for
//! the analysis thread at bar/phrase cadence. Non-finite input samples
//! are treated as silence so snapshots stay NaN-free.
//!
//! Loudness parity: constants and gating mirror
//! `kontinuum-mastering::loudness` (100 ms slots, 400 ms momentary / 3 s
//! short-term, absolute −70 LUFS then relative −10 LU gate). One honest
//! deviation: the integrated value covers the trailing
//! [`ANALYSIS_SECONDS`] (bounded memory) and floors at ≈ −240.7 LUFS
//! instead of `-inf` so JSON serialization (#26) stays valid.

use serde::{Deserialize, Serialize};

use crate::dsp::{PeakProbe, SlotRing, SpectralTracker};
use crate::filters::{lufs_db, KWeighter};

/// History depth of the integrated window (seconds).
pub const ANALYSIS_SECONDS: f64 = 60.0;
const SLOT_SEC: f64 = 0.1;
const MOMENTARY_SLOTS: usize = 4; // 400 ms
const SHORT_TERM_SLOTS: usize = 30; // 3 s
const SPECTRAL_WINDOW: usize = 8192;

/// Absolute gate (LUFS) of the BS.1770 two-stage integration.
const ABSOLUTE_GATE_LUFS: f64 = -70.0;
/// Relative gate (LU below the ungated mean of blocks above absolute).
const RELATIVE_GATE_LU: f64 = -10.0;

/// One folded reading of the master critic. All Copy, no heap — safe to
/// post across threads and serde-serialize for #26/#15.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CriticSnapshot {
    /// 400 ms K-weighted loudness (LUFS).
    pub momentary_lufs: f64,
    /// 3 s K-weighted loudness (LUFS).
    pub short_term_lufs: f64,
    /// Gated integrated loudness over the trailing [`ANALYSIS_SECONDS`].
    pub integrated_lufs: f64,
    /// Peak/RMS ratio over the short-term window (dB).
    pub crest_db: f64,
    /// Spectral tilt, dB/octave across the band plan (negative = low-
    /// heavy/dark, positive = bright).
    pub tilt_db_per_oct: f64,
    /// Power-weighted spectral centroid (Hz).
    pub centroid_hz: f64,
    /// Share of spectral power in 20–60 Hz (0..1).
    pub sub_share: f64,
    /// Inter-channel correlation over the short-term window (−1..1).
    pub correlation: f64,
    /// Side/mid RMS ratio over the short-term window (dB).
    pub width_db: f64,
    /// 4× interpolated peak estimate over the short-term window (dBFS).
    pub true_peak_dbfs: f64,
    /// Signal time pushed so far (s) — consumers gate on warm-up.
    pub seconds: f64,
}

/// Rolling master-bus critic. Feed it the master bus (post-mastering for
/// "what does the listener hear", pre-mastering for mix telemetry).
pub struct CriticEngine {
    sr_hz: f64,
    slot_len: usize,
    kw: KWeighter,
    peak_l: PeakProbe,
    peak_r: PeakProbe,
    spectral: SpectralTracker,
    // Current (possibly partial) slot accumulators.
    k_acc: f64,
    mono_acc: f64,
    l2_acc: f64,
    r2_acc: f64,
    lr_acc: f64,
    peak_acc: f64,
    slot_n: usize,
    // Slot history rings, oldest → newest.
    k_slots: SlotRing,
    mono_slots: SlotRing,
    l2_slots: SlotRing,
    r2_slots: SlotRing,
    lr_slots: SlotRing,
    peak_slots: SlotRing,
    frames: u64,
}

impl CriticEngine {
    pub fn new(sample_rate: u32) -> Self {
        let slot_len = (sample_rate as f64 * SLOT_SEC).round() as usize;
        let cap = (ANALYSIS_SECONDS / SLOT_SEC).round() as usize;
        CriticEngine {
            sr_hz: sample_rate as f64,
            slot_len,
            kw: KWeighter::new(sample_rate),
            peak_l: PeakProbe::new(),
            peak_r: PeakProbe::new(),
            spectral: SpectralTracker::new(sample_rate, SPECTRAL_WINDOW),
            k_acc: 0.0,
            mono_acc: 0.0,
            l2_acc: 0.0,
            r2_acc: 0.0,
            lr_acc: 0.0,
            peak_acc: 0.0,
            slot_n: 0,
            k_slots: SlotRing::new(cap),
            mono_slots: SlotRing::new(cap),
            l2_slots: SlotRing::new(cap),
            r2_slots: SlotRing::new(cap),
            lr_slots: SlotRing::new(cap),
            peak_slots: SlotRing::new(cap),
            frames: 0,
        }
    }

    /// Mono convenience: the block is fed as dual-mono (correlation 1.0).
    pub fn push_block(&mut self, block: &[f32]) {
        self.push_block_stereo(block, block);
    }

    /// Feed one stereo, non-interleaved block. Channel length mismatch is
    /// resolved by truncating to the shorter channel.
    pub fn push_block_stereo(&mut self, left: &[f32], right: &[f32]) {
        let n = left.len().min(right.len());
        for i in 0..n {
            // Boundary hygiene: non-finite samples are silence, never NaN.
            let (l, r) = (clean(left[i]), clean(right[i]));
            let (kl, kr) = self.kw.tick(l, r);
            self.k_acc += (kl * kl + kr * kr) as f64;
            let (lf, rf) = (l as f64, r as f64);
            let mono = (lf + rf) * 0.5;
            self.mono_acc += mono * mono;
            self.l2_acc += lf * lf;
            self.r2_acc += rf * rf;
            self.lr_acc += lf * rf;
            let p = self.peak_l.push(l).max(self.peak_r.push(r));
            if p > self.peak_acc {
                self.peak_acc = p;
            }
            self.spectral.push(mono);
            self.slot_n += 1;
            if self.slot_n == self.slot_len {
                let n = self.slot_len as f64;
                self.k_slots.push(self.k_acc / n);
                self.mono_slots.push(self.mono_acc / n);
                self.l2_slots.push(self.l2_acc / n);
                self.r2_slots.push(self.r2_acc / n);
                self.lr_slots.push(self.lr_acc / n);
                self.peak_slots.push(self.peak_acc);
                self.k_acc = 0.0;
                self.mono_acc = 0.0;
                self.l2_acc = 0.0;
                self.r2_acc = 0.0;
                self.lr_acc = 0.0;
                self.peak_acc = 0.0;
                self.slot_n = 0;
            }
        }
        self.frames += n as u64;
    }

    /// Fold the slot history into a [`CriticSnapshot`].
    pub fn snapshot(&self) -> CriticSnapshot {
        let k: Vec<f64> = self.k_slots.iter().collect();
        let momentary = lufs_db(mean(tail(&k, MOMENTARY_SLOTS)));
        let short_term = lufs_db(mean(tail(&k, SHORT_TERM_SLOTS)));
        let integrated = integrated_lufs(&k);

        // Dynamics + stereo over the short-term window.
        let peak = self.peak_slots.tail(SHORT_TERM_SLOTS).fold(0.0f64, f64::max);
        let rms = mean(self.mono_slots.tail(SHORT_TERM_SLOTS).collect::<Vec<_>>().as_slice()).sqrt();
        let crest_db = if peak > 0.0 { 20.0 * (peak / rms.max(1e-12)).log10() } else { 0.0 };
        let (l2, r2, lr) = (
            self.l2_slots.tail(SHORT_TERM_SLOTS).sum::<f64>(),
            self.r2_slots.tail(SHORT_TERM_SLOTS).sum::<f64>(),
            self.lr_slots.tail(SHORT_TERM_SLOTS).sum::<f64>(),
        );
        let correlation = if l2 > 0.0 && r2 > 0.0 { lr / (l2 * r2).sqrt() } else { 0.0 };
        // mid = (l+r)/2, side = (l−r)/2 ⇒ 10·log10(side²/mid²) from sums.
        // The floor is relative to total power (−120 dB) so exact dual-mono
        // (side² = 0) and exact anti-phase (mid² = 0) both read finite.
        let p = (l2 + r2).max(1e-30);
        let mid2 = (l2 + r2 + 2.0 * lr).max(0.0);
        let side2 = (l2 + r2 - 2.0 * lr).max(0.0);
        let width_db = 10.0 * ((side2 + p * 1e-12) / (mid2 + p * 1e-12)).log10();

        CriticSnapshot {
            momentary_lufs: momentary,
            short_term_lufs: short_term,
            integrated_lufs: integrated,
            crest_db,
            tilt_db_per_oct: self.spectral.tilt_db_per_oct,
            centroid_hz: self.spectral.centroid_hz,
            sub_share: self.spectral.sub_share,
            correlation,
            width_db,
            true_peak_dbfs: 20.0 * peak.max(1e-12).log10(),
            seconds: self.frames as f64 / self.sr_hz,
        }
    }
}

fn clean(x: f32) -> f32 {
    if x.is_finite() {
        x
    } else {
        0.0
    }
}

fn mean(v: &[f64]) -> f64 {
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<f64>() / v.len() as f64
    }
}

fn tail(v: &[f64], n: usize) -> &[f64] {
    &v[v.len().saturating_sub(n)..]
}

/// BS.1770 two-stage gated integration over the slot history (400 ms
/// blocks on a 100 ms hop, absolute then relative gate) — the same math
/// as `kontinuum-mastering::loudness::integrated_lufs`, floored finite.
fn integrated_lufs(slots: &[f64]) -> f64 {
    if slots.len() < MOMENTARY_SLOTS {
        return lufs_db(mean(slots));
    }
    let mut blocks = Vec::with_capacity(slots.len() - MOMENTARY_SLOTS + 1);
    for start in 0..=slots.len() - MOMENTARY_SLOTS {
        blocks.push(mean(&slots[start..start + MOMENTARY_SLOTS]));
    }
    let above_abs: Vec<f64> =
        blocks.iter().copied().filter(|ms| lufs_db(*ms) > ABSOLUTE_GATE_LUFS).collect();
    if above_abs.is_empty() {
        return lufs_db(0.0);
    }
    let relative_gate = lufs_db(mean(&above_abs)) + RELATIVE_GATE_LU;
    let gated: Vec<f64> =
        above_abs.into_iter().filter(|ms| lufs_db(*ms) > relative_gate).collect();
    lufs_db(mean(&gated))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    fn push_seconds(e: &mut CriticEngine, hz: f64, amp: f64, seconds: f64) {
        let n = (SR as f64 * seconds) as usize;
        let block: Vec<f32> = (0..n)
            .map(|i| (amp * (std::f64::consts::TAU * hz * i as f64 / SR as f64).sin()) as f32)
            .collect();
        e.push_block(&block);
    }

    #[test]
    fn full_scale_sine_integrates_near_zero_lufs() {
        // Dual-mono full-scale 997 Hz: BS.1770 expects ≈ 0 LUFS
        // (−0.691 + ~+0.7 dB K-gain at 1 kHz). Tolerance 1 LU covers the
        // RBJ shelf approximation documented in dsp.rs.
        let mut e = CriticEngine::new(SR);
        push_seconds(&mut e, 997.0, 1.0, 4.0);
        let s = e.snapshot();
        assert!(s.integrated_lufs.abs() < 1.0, "integrated {}", s.integrated_lufs);
        assert!(s.momentary_lufs.abs() < 1.0, "momentary {}", s.momentary_lufs);
        assert!(s.short_term_lufs.abs() < 1.0, "short-term {}", s.short_term_lufs);
    }

    #[test]
    fn snapshot_tracks_level_changes_and_stabilizes_when_constant() {
        let mut e = CriticEngine::new(SR);
        push_seconds(&mut e, 997.0, 0.25, 4.0);
        let quiet = e.snapshot();
        push_seconds(&mut e, 997.0, 0.25, 2.0);
        let stable = e.snapshot();
        assert!((stable.short_term_lufs - quiet.short_term_lufs).abs() < 0.05,
            "constant signal must give a stable short-term reading");
        push_seconds(&mut e, 997.0, 0.5, 4.0);
        let loud = e.snapshot();
        let step = loud.short_term_lufs - quiet.short_term_lufs;
        assert!((step - 20.0 * 2.0f64.log10()).abs() < 0.5,
            "2× amplitude must move short-term by ~6 dB, got {step}");
    }

    #[test]
    fn silence_yields_finite_floored_metrics() {
        let mut e = CriticEngine::new(SR);
        e.push_block(&vec![0.0f32; SR as usize * 2]);
        let s = e.snapshot();
        assert!(s.momentary_lufs < -200.0 && s.momentary_lufs.is_finite());
        assert!(s.integrated_lufs < -200.0 && s.integrated_lufs.is_finite());
        assert_eq!(s.crest_db, 0.0);
        assert_eq!(s.correlation, 0.0);
        assert_eq!(s.sub_share, 0.0);
        assert!(s.true_peak_dbfs < -200.0);
    }

    #[test]
    fn sine_centroid_matches_frequency() {
        let mut e = CriticEngine::new(SR);
        push_seconds(&mut e, 750.0, 0.6, 2.0);
        let s = e.snapshot();
        assert!((700.0..=800.0).contains(&s.centroid_hz), "centroid {}", s.centroid_hz);
    }

    #[test]
    fn stereo_correlation_and_width_follow_channel_polarity() {
        let n = SR as usize * 4;
        let l: Vec<f32> = (0..n)
            .map(|i| (0.5 * (std::f64::consts::TAU * 997.0 * i as f64 / SR as f64).sin()) as f32)
            .collect();
        let mut e = CriticEngine::new(SR);
        e.push_block_stereo(&l, &l);
        let mono = e.snapshot();
        assert!(mono.correlation > 0.99, "dual-mono correlation {}", mono.correlation);
        assert!(mono.width_db < -40.0, "dual-mono width {}", mono.width_db);

        let r: Vec<f32> = l.iter().map(|x| -x).collect();
        let mut e = CriticEngine::new(SR);
        e.push_block_stereo(&l, &r);
        let wide = e.snapshot();
        assert!(wide.correlation < -0.99, "anti-phase correlation {}", wide.correlation);
        assert!(wide.width_db > 40.0, "anti-phase width {}", wide.width_db);
    }

    #[test]
    fn true_peak_estimate_tracks_amplitude() {
        let mut e = CriticEngine::new(SR);
        push_seconds(&mut e, 997.0, 0.9, 2.0);
        let s = e.snapshot();
        let expect = 20.0 * 0.9f64.log10();
        assert!((s.true_peak_dbfs - expect).abs() < 0.5,
            "true peak {} vs sample peak {expect}", s.true_peak_dbfs);
    }

    #[test]
    fn identical_pushes_give_identical_snapshots() {
        let mut a = CriticEngine::new(SR);
        let mut b = CriticEngine::new(SR);
        push_seconds(&mut a, 220.0, 0.4, 3.5);
        push_seconds(&mut b, 220.0, 0.4, 3.5);
        assert_eq!(a.snapshot(), b.snapshot(), "the critic must be deterministic");
    }

    #[test]
    fn non_finite_input_never_poisons_the_snapshot() {
        let mut e = CriticEngine::new(SR);
        e.push_block(&[f32::NAN, 0.5, f32::INFINITY, -0.5]);
        let s = e.snapshot();
        assert!(s.momentary_lufs.is_finite() && s.crest_db.is_finite());
    }
}
