//! The mastering chain (#28): tilt EQ → dynamic low control → glue
//! compression → oversampled soft clipper → true-peak limiter, in fixed
//! order, stereo-linked, with section-aware see-through.
//!
//! CPU-cost notes per stage (per stereo frame, ~48 kHz):
//! 1. tilt EQ — 4 biquads (2/ch), coefficients refreshed per block: ~40 MAC.
//! 2. low control — 4 detection biquads + 2 shelf biquads: ~60 MAC.
//! 3. glue — envelope + gain math only: ~15 flops.
//! 4. clipper — 2× (polyphase up 128 MAC + down 128 MAC) + 8 curve evals:
//!    ~260 MAC — the expensive stage by design (×4 oversampling).
//! 5. limiter — 2× polyphase up (256 MAC) + window scan (lookahead
//!    compares) + 2× down (256 MAC): ~550 MAC total; still well under 1%
//!    of a modern core at 48 kHz.
//!
//! Safety: every adaptive parameter moves through a bounded slew; the
//! limiter ceiling is absolute; sustained over-limit GR latches an alarm
//! flag for the kill-switch (#15). See `stages` docs for per-stage bounds.

use crate::clipper::SoftClipper;
use crate::glue::GlueCompressor;
use crate::limiter::TruePeakLimiter;
use crate::low_control::DynamicLowControl;
use crate::oversample::{DOWN_LATENCY_FRAMES, UP_LATENCY_FRAMES};
use crate::targets::MasteringTargets;
use crate::telemetry::MasteringTelemetry;
use crate::tilt::TiltEq;

/// Speaker-aware output profile (v0 approximation, issue #82): remaps the
/// existing knobs only. `SmallSpeaker` brightens the tilt (+2 dB through
/// the stage's ±3 clamp and 5 s slew) and keeps the low-control discipline
/// partially relaxed; glue makeup is untouched. True low-end harmonic
/// saturation for small speakers is a future stage (issue #82).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OutputProfile {
    /// Full-range playback (headphones, monitors, dock).
    Full,
    /// Built-in device speaker: sub content is unreproducible, so the
    /// master leans bright and the low-end discipline stays soft.
    SmallSpeaker,
}

/// `SmallSpeaker` tilt offset (dB) on top of the host's tilt target.
const SMALL_SPEAKER_TILT_DB: f32 = 2.0;
/// `SmallSpeaker` floor under the chain's smoothed low-control relax
/// (0 = full discipline): sub discipline stays partially relaxed even in
/// full-intensity sections.
const SMALL_SPEAKER_RELAX_FLOOR: f32 = 0.25;

/// Fixed-order mastering chain. `render` is allocation-free.
pub struct MasteringChain {
    sample_rate: u32,
    tilt: TiltEq,
    low: DynamicLowControl,
    glue: GlueCompressor,
    clipper: SoftClipper,
    limiter: TruePeakLimiter,
    /// Breakdown relaxation, smoothed (~0.5 s) toward `relax_target`.
    relax: f32,
    relax_target: f32,
    bypassed: bool,
    blocks: u64,
    /// Block-max limiter reduction for the telemetry snapshot.
    limiter_gr_max: f32,
    profile: OutputProfile,
    /// Host-requested tilt target before the profile offset (dB).
    tilt_user_db: f32,
}

impl MasteringChain {
    /// Production defaults; spectral tilt starts neutral until told.
    pub fn new(sample_rate: u32) -> Self {
        let targets = MasteringTargets::hypothesis();
        Self::new_with_targets(sample_rate, &targets)
    }

    /// Production defaults with the tilt pivot/strength from a targets
    /// profile (tilt_hz / tilt_cdb).
    pub fn new_with_targets(sample_rate: u32, targets: &MasteringTargets) -> Self {
        let mut chain = MasteringChain {
            sample_rate,
            tilt: TiltEq::new(sample_rate, targets.tilt_hz),
            low: DynamicLowControl::new(sample_rate),
            glue: GlueCompressor::new(sample_rate),
            clipper: SoftClipper::new(sample_rate),
            limiter: TruePeakLimiter::new(sample_rate),
            relax: 0.0,
            relax_target: 0.0,
            bypassed: false,
            blocks: 0,
            limiter_gr_max: 0.0,
            profile: OutputProfile::Full,
            tilt_user_db: (targets.tilt_cdb / 100.0) as f32,
        };
        chain.apply_profile();
        chain
    }

    /// Bit-exact passthrough (A/B reference, kill-switch). Enabling
    /// reinitializes stage buffers so no stale lookahead leaks through.
    pub fn set_bypassed(&mut self, bypassed: bool) {
        self.bypassed = bypassed;
        if !bypassed {
            self.reset();
        }
    }

    pub fn is_bypassed(&self) -> bool {
        self.bypassed
    }

    /// Corrective tilt target, dB (positive brightens, hard-capped ±3).
    pub fn set_tilt_target_db(&mut self, db: f32) {
        self.tilt_user_db = db;
        self.apply_profile();
    }

    /// Speaker-aware output profile (v0 approximation): remaps the
    /// existing knobs only — see [`OutputProfile`].
    pub fn set_output_profile(&mut self, profile: OutputProfile) {
        self.profile = profile;
        self.apply_profile();
    }

    /// Re-derives the tilt target from the host request plus the profile
    /// offset; the ±3 dB clamp and slew live in the tilt stage.
    fn apply_profile(&mut self) {
        let offset = match self.profile {
            OutputProfile::Full => 0.0,
            OutputProfile::SmallSpeaker => SMALL_SPEAKER_TILT_DB,
        };
        self.tilt.set_tilt_target_db(self.tilt_user_db + offset);
    }

    /// Section-aware see-through (#28): 0 = full-intensity section,
    /// 1 = breakdown. Internally smoothed (~0.5 s) so section switches
    /// glide; relaxation lifts the comp threshold, unwinds the clipper
    /// drive and stops the glue seeker from chasing quiet sections.
    pub fn set_section_energy(&mut self, energy: f32) {
        let e = if energy.is_finite() { energy.clamp(0.0, 1.0) } else { 0.0 };
        self.relax_target = 1.0 - e;
    }

    /// Latency the chain inserts (input frames) — the host must
    /// compensate when monitoring through it. 0 while bypassed.
    pub fn latency_frames(&self) -> usize {
        if self.bypassed {
            0
        } else {
            UP_LATENCY_FRAMES + self.limiter.lookahead_frames() + DOWN_LATENCY_FRAMES
        }
    }

    /// Process a stereo block in place. Any block length works (the
    /// limiter's internal lookahead is length-independent); coefficients
    /// and seekers update once per call.
    pub fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        if self.bypassed {
            return;
        }
        let frames = left.len().min(right.len());
        // Relax smoothing: ~0.5 s one-pole per frame, evaluated per block.
        // The profile can hold a relax floor (small speakers lean less on
        // sub discipline).
        let relax_floor = match self.profile {
            OutputProfile::Full => 0.0,
            OutputProfile::SmallSpeaker => SMALL_SPEAKER_RELAX_FLOOR,
        };
        let relax_target = self.relax_target.max(relax_floor);
        let per_frame = (-1.0 / (0.5 * self.sample_rate as f32)).exp();
        let block_coeff = 1.0 - per_frame.powi(frames as i32);
        self.relax += block_coeff * (relax_target - self.relax);
        self.low.set_relax(self.relax);
        self.glue.set_relax(self.relax);
        self.clipper.set_relax(self.relax);

        self.tilt.update_block(frames);
        self.low.update_block();
        self.glue.update_block(frames);

        let mut limiter_gr = 0.0f32;
        for i in 0..frames {
            let (mut l, mut r) = (left[i], right[i]);
            (l, r) = self.tilt.tick(l, r);
            let _ = self.low.tick(l, r);
            (l, r) = self.low.apply(l, r);
            let _ = self.glue.tick(l, r);
            (l, r) = self.glue.apply(l, r);
            (l, r) = self.clipper.tick(l, r);
            (l, r) = self.limiter.tick(l, r);
            left[i] = l;
            right[i] = r;
            limiter_gr = limiter_gr.max(self.limiter.gr_db());
        }
        self.limiter_gr_max = limiter_gr;
        self.blocks += 1;
    }

    /// Snapshot of stage working points after the last render.
    pub fn telemetry(&self) -> MasteringTelemetry {
        MasteringTelemetry {
            tilt_db: self.tilt.tilt_target_db(),
            low_control_gr_db: self.low.gr_db(),
            glue_gr_db: self.glue.gr_db(),
            glue_threshold_db: self.glue.threshold_db(),
            glue_makeup_db: self.glue.makeup_db(),
            clipper_gr_db: self.clipper.gr_db(),
            clipper_drive_db: self.clipper.drive_db(),
            limiter_gr_db: self.limiter_gr_max,
            limiter_gr_alarm: self.limiter.alarm(),
            section_relax: self.relax,
            blocks: self.blocks,
            bypassed: self.bypassed,
        }
    }

    pub fn limiter_alarm(&self) -> bool {
        self.limiter.alarm()
    }

    pub fn reset(&mut self) {
        self.tilt.reset();
        self.low.reset();
        self.glue.reset();
        self.clipper.reset();
        self.limiter.reset();
        self.relax = self.relax_target;
        self.limiter_gr_max = 0.0;
        self.blocks = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limiter::TruePeakLimiter;

    fn sine(freq_hz: f32, amp: f32, sr: u32, i: usize) -> f32 {
        amp * (std::f32::consts::TAU * freq_hz * i as f32 / sr as f32).sin()
    }

    #[test]
    fn reset_restores_the_fresh_state_bit_for_bit() {
        // The graph-level determinism contract (and the engine's stop-path
        // reset) requires a reset chain to render identically to a never-
        // used chain — every seeker, smoother and history must seed back.
        let sr = 48_000u32;
        let render = |chain: &mut MasteringChain| {
            let mut l: Vec<f32> = (0..48_000).map(|i| sine(997.0, 1.2, sr, i)).collect();
            let mut r = l.clone();
            for chunk in l.chunks_mut(64).zip(r.chunks_mut(64)) {
                let (cl, cr) = chunk;
                chain.render(cl, cr);
            }
            (l, r)
        };
        let mut fresh = MasteringChain::new(sr);
        let fresh_out = render(&mut fresh);

        let mut reused = MasteringChain::new(sr);
        let _ = render(&mut reused);
        reused.reset();
        let reused_out = render(&mut reused);

        assert_eq!(fresh_out.0, reused_out.0, "reset diverged on the left channel");
        assert_eq!(fresh_out.1, reused_out.1, "reset diverged on the right channel");
        assert_eq!(fresh.telemetry(), reused.telemetry());
    }

    #[test]
    fn bypassed_chain_is_bit_exact() {
        let sr = 48_000u32;
        let mut chain = MasteringChain::new(sr);
        chain.set_bypassed(true);
        let mut l: Vec<f32> = (0..2048).map(|i| sine(997.0, 1.4, sr, i)).collect();
        let mut r = l.clone();
        let in_l = l.clone();
        chain.render(&mut l, &mut r);
        assert_eq!(l, in_l, "bypass must not touch samples");
        assert_eq!(chain.latency_frames(), 0);
    }

    #[test]
    fn silence_stays_exactly_silent_through_the_live_chain() {
        let sr = 48_000u32;
        let mut chain = MasteringChain::new(sr);
        let mut l = vec![0.0f32; sr as usize];
        let mut r = vec![0.0f32; sr as usize];
        chain.render(&mut l, &mut r);
        assert!(l.iter().chain(r.iter()).all(|s| s.abs() < 1e-9), "silence must stay silent");
    }

    #[test]
    fn limiter_alarm_latches_and_resets() {
        // Stage-level contract (see limiter::tests): sustained over-limit
        // reduction latches the alarm; reset clears it.
        let sr = 48_000u32;
        let mut lim = TruePeakLimiter::new(sr);
        let mut latched = false;
        for _ in 0..2 * sr as usize {
            let _ = lim.tick(1.5, 1.5);
            if lim.alarm() {
                latched = true;
                break;
            }
        }
        assert!(latched, "sustained over-limit must latch the alarm");
        lim.reset();
        assert!(!lim.alarm(), "reset must clear the alarm");
    }
}
