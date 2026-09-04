//! Kill-switch fades (issue #38 step 5): the per-track mute and the master
//! panic both ride this node. A precomputed linear ramp — one integer frame
//! counter, no per-sample transcendentals, no allocation — moves the level
//! between exactly 1.0 and exactly 0.0, so killed paths are bit-exact silence
//! and re-armed paths are bit-exact passthrough. The ramp is bounded
//! (≤ [`MUTE_FADE_MS`] for a mute), so edges are click-free by construction:
//! the worst per-sample output step is the signal amplitude divided by the
//! ramp length, the same slew argument the duck edge test uses.
//!
//! Counters feed the #15 watchdog via [`KillTelemetry`] (simple getters on
//! the graph / deck mixer; the supervision crate reads them host-side).

use serde::{Deserialize, Serialize};

/// Mute fade length (ms) — inside the ≤ 10 ms kill-switch budget.
pub const MUTE_FADE_MS: f32 = 8.0;
/// Master panic ramp (ms): fast, but long enough that the per-sample slew
/// stays far below the click-free bound at any sane sample rate.
pub const PANIC_FADE_MS: f32 = 15.0;

/// One kill fade: precomputed linear ramp between exact endpoints. `closing`
/// is the requested direction; `progress` counts completed ramp frames, so
/// the endpoints are reached exactly (no one-pole asymptote, no denormal
/// tail) and the node is deterministic by construction.
#[derive(Clone, Copy, Debug)]
pub struct KillFade {
    ramp: u32,
    progress: u32,
    closing: bool,
}

impl KillFade {
    /// Ramp long enough to cover `ms` at `sample_rate` (minimum one frame).
    pub fn new(sample_rate: f32, ms: f32) -> Self {
        let frames = ms * 0.001 * sample_rate;
        let frames = if frames.is_finite() { frames.round().max(1.0) } else { 1.0 };
        KillFade { ramp: frames as u32, progress: 0, closing: false }
    }

    /// Start fading to exact zero.
    pub fn close(&mut self) {
        self.closing = true;
    }

    /// Start fading back to exact unity (re-arm).
    pub fn open(&mut self) {
        self.closing = false;
    }

    /// Snap fully open (used by `reset`, which must leave no half-fades).
    pub fn snap_open(&mut self) {
        self.progress = 0;
        self.closing = false;
    }

    /// Advance one frame and return the current level (1 = open, 0 = silence).
    pub fn tick(&mut self) -> f32 {
        if self.closing {
            if self.progress < self.ramp {
                self.progress += 1;
            }
        } else if self.progress > 0 {
            self.progress -= 1;
        }
        1.0 - self.progress as f32 / self.ramp as f32
    }

    /// Fully open and staying there (bit-exact passthrough territory).
    pub fn is_open(&self) -> bool {
        !self.closing && self.progress == 0
    }

    /// Fully silent (fade completed).
    pub fn is_closed(&self) -> bool {
        self.progress == self.ramp
    }

    /// Whether a close has been requested (the "muted/panicked" flag).
    pub fn closing(&self) -> bool {
        self.closing
    }
}

/// Kill-switch event counters for the #15 watchdog feed. Cumulative over the
/// graph's lifetime; read host-side (no RT involvement — counters move only
/// on control-path calls).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KillTelemetry {
    /// Mute engagements (a mute request on an already-muted track does not
    /// re-count).
    pub mute_events: u32,
    /// Panic engagements (panicking while already panicked does not re-count).
    pub panic_events: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

    #[test]
    fn mute_ramp_fits_the_10ms_budget_at_common_rates() {
        for sr in [44_100.0f32, 48_000.0, 96_000.0] {
            let fade = KillFade::new(sr, MUTE_FADE_MS);
            let ms = fade.ramp as f32 / sr * 1000.0;
            assert!(ms <= 10.0, "ramp is {ms:.4} ms, over the kill-switch budget at {sr}");
        }
        assert_eq!(KillFade::new(48_000.0, MUTE_FADE_MS).ramp, 384);
    }

    #[test]
    fn fade_reaches_exact_endpoints_and_stays_there() {
        let mut fade = KillFade::new(SR, 10.0);
        let ramp = fade.ramp as usize;
        fade.close();
        let mut prev = 1.0f32;
        for i in 0..ramp * 2 {
            let v = fade.tick();
            assert!(v <= prev, "closing fade rose at frame {i}");
            prev = v;
        }
        assert_eq!(prev, 0.0, "close did not land on exact zero");
        assert!(fade.is_closed());
        fade.open();
        prev = 0.0;
        for i in 0..ramp * 2 {
            let v = fade.tick();
            assert!(v >= prev, "opening fade fell at frame {i}");
            prev = v;
        }
        assert_eq!(prev, 1.0, "open did not land on exact unity");
        assert!(fade.is_open());
    }

    #[test]
    fn direction_reversal_mid_fade_is_continuous() {
        let mut fade = KillFade::new(SR, 10.0);
        fade.close();
        for _ in 0..fade.ramp / 2 {
            fade.tick();
        }
        let mid = fade.tick();
        fade.open();
        let next = fade.tick();
        assert!((next - mid).abs() <= 1.0 / fade.ramp as f32 + 1e-6, "direction flip jumped: {mid} → {next}");
    }

    #[test]
    fn non_finite_and_tiny_inputs_degrade_to_a_one_frame_ramp() {
        let mut fade = KillFade::new(SR, f32::NAN);
        assert_eq!(fade.ramp, 1);
        fade.close();
        assert_eq!(fade.tick(), 0.0);
    }
}
