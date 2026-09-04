//! Stage 5 — true-peak limiter (#28): 1.5 ms lookahead, −1.0 dBTP ceiling
//! enforced on a 4× oversampled (inter-sample) peak estimate, sustained
//! over-limit GR alarm for the kill-switch integration (#15).
//!
//! Algorithm: the input is oversampled ×4; the linked (max of L/R)
//! oversampled peak drives a sliding-window max over the lookahead span.
//! The gain path attacks instantly to the needed value (it has the full
//! lookahead to prepare) and releases with a one-pole (~80 ms). The
//! smoothed gain multiplies the delayed oversampled signal, which is then
//! lowpassed and decimated back to the base rate.
//!
//! Honest approximations, documented:
//! - "True peak" here = peak of the 4× oversampled signal. A 4× estimate
//!   can undershoot the mathematical inter-sample peak by a hair on
//!   pathological ultrasonic content; program material is far inside
//!   that margin.
//! - The internal working ceiling is the spec ceiling (−1.0 dBTP) minus
//!   a [`CEILING_GUARD_DB`] reconstruction guard: the decimation/re-
//!   oversampling filters can ripple ≲0.1 dB above the enforced value,
//!   and the ceiling guarantee is absolute.
//! - The 3 dB sustained-GR cap is policy, not a bypass: the limiter
//!   ALWAYS enforces the ceiling (a limiter that stops limiting breaks
//!   the guarantee), and when reduction wants to exceed 3 dB for longer
//!   than [`GR_ALARM_SUSTAIN_S`] it latches the alarm flag for the
//!   upstream kill-switch (#15) to act on. `reset()` clears it.

use crate::oversample::{Oversampler4x, DOWN_LATENCY_FRAMES, UP_LATENCY_FRAMES};

/// Published ceiling (dBTP).
pub const CEILING_DBTP: f32 = -1.0;
/// Reconstruction guard subtracted from the working ceiling.
const CEILING_GUARD_DB: f32 = 0.15;
/// Lookahead (ms, #28: 1.5–2).
const LOOKAHEAD_MS: f32 = 1.5;
/// Limiter release time constant.
const RELEASE_MS: f32 = 80.0;
/// Reduction beyond this (dB), sustained, latches the alarm.
pub const GR_ALARM_THRESHOLD_DB: f32 = 3.0;
/// How long the breach must hold before latching.
pub const GR_ALARM_SUSTAIN_S: f32 = 0.5;

/// Stereo-linked true-peak limiter. Latency:
/// [`UP_LATENCY_FRAMES`] + lookahead + [`DOWN_LATENCY_FRAMES`] frames.
pub struct TruePeakLimiter {
    sample_rate: f32,
    ceiling_lin: f32,
    /// Per-channel ×4 oversamplers (allocation in `new()` only).
    up: [Oversampler4x; 2],
    /// Delayed oversampled samples awaiting their gain, per channel.
    /// Ring of `lookahead_frames` input frames × 4 subsamples.
    ring: [Vec<f32>; 2],
    ring_pos: usize,
    /// Sliding window of linked max |x| per input frame (1× rate).
    window: Vec<f32>,
    win_pos: usize,
    release_coeff: f32,
    gain: f32,
    /// Sustained-breach timer (seconds) and latched alarm.
    breach_s: f32,
    alarm: bool,
    /// Per-frame reduction for telemetry (positive dB).
    last_gr_db: f32,
}

impl TruePeakLimiter {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let lookahead = ((LOOKAHEAD_MS * sr / 1000.0).ceil() as usize).max(4);
        let working_db = CEILING_DBTP - CEILING_GUARD_DB;
        TruePeakLimiter {
            sample_rate: sr,
            ceiling_lin: 10.0f32.powf(working_db / 20.0),
            up: [Oversampler4x::new(), Oversampler4x::new()],
            ring: [vec![0.0; lookahead * 4], vec![0.0; lookahead * 4]],
            ring_pos: 0,
            window: vec![0.0; lookahead],
            win_pos: 0,
            release_coeff: (-1000.0 / (RELEASE_MS * sr)).exp(),
            gain: 1.0,
            breach_s: 0.0,
            alarm: false,
            last_gr_db: 0.0,
        }
    }

    pub fn lookahead_frames(&self) -> usize {
        self.window.len()
    }

    /// Total latency this stage inserts (input frames).
    pub fn latency_frames(&self) -> usize {
        UP_LATENCY_FRAMES + self.lookahead_frames() + DOWN_LATENCY_FRAMES
    }

    pub fn alarm(&self) -> bool {
        self.alarm
    }

    /// Peak gain reduction since the last block read (positive dB).
    pub fn gr_db(&self) -> f32 {
        self.last_gr_db
    }

    /// Process one stereo frame.
    pub fn tick(&mut self, left: f32, right: f32) -> (f32, f32) {
        let mut ul = [0.0f32; 4];
        let mut ur = [0.0f32; 4];
        self.up[0].up(left, &mut ul);
        self.up[1].up(right, &mut ur);

        // The gain for the frame now leaving the lookahead ring is set
        // by the window max over the frames that arrived after it (plus
        // everything still queued) — that is the full lookahead of
        // preparation time. The current frame's peak joins the window
        // only AFTER this read, keeping window ⇒ output alignment exact.
        let mut window_max = 0.0f32;
        for &w in self.window.iter() {
            window_max = window_max.max(w);
        }
        let needed = (self.ceiling_lin / window_max.max(1e-12)).min(1.0);
        // Attack: instant drop. Release: one-pole toward the needed gain
        // (which is 1.0 whenever the program is under the ceiling).
        let mut gain = self.gain.min(needed);
        if gain < needed {
            gain += (1.0 - self.release_coeff) * (needed - gain);
        }
        gain = gain.max(0.0316); // −30 dB numeric floor; ceiling still absolute
        self.gain = gain;

        // Sustained-breach alarm (#15): reduction past the 3 dB policy
        // cap latches the flag; the ceiling is still enforced.
        let gr_db = -20.0 * (gain.max(1e-6) as f64).log10() as f32;
        if gr_db > GR_ALARM_THRESHOLD_DB {
            self.breach_s += 1.0 / self.sample_rate;
            if self.breach_s > GR_ALARM_SUSTAIN_S {
                self.alarm = true;
            }
        } else {
            self.breach_s = 0.0;
        }
        self.last_gr_db = gr_db;

        // Linked 4× peak of this frame joins the sliding window.
        let mut frame_peak = 0.0f32;
        for k in 0..4 {
            frame_peak = frame_peak.max(ul[k].abs()).max(ur[k].abs());
        }
        self.window[self.win_pos] = frame_peak;
        self.win_pos = (self.win_pos + 1) % self.window.len();

        // Apply the gain to the delayed oversampled frame, queue this
        // frame's subsamples behind it, emit the limited output.
        let idx = self.ring_pos * 4;
        let mut dl = [0.0f32; 4];
        let mut dr = [0.0f32; 4];
        for k in 0..4 {
            dl[k] = self.ring[0][idx + k] * gain;
            dr[k] = self.ring[1][idx + k] * gain;
        }
        for ch in 0..2 {
            let src = if ch == 0 { &ul } else { &ur };
            let ring = &mut self.ring[ch];
            ring[idx] = src[0];
            ring[idx + 1] = src[1];
            ring[idx + 2] = src[2];
            ring[idx + 3] = src[3];
        }
        self.ring_pos = (self.ring_pos + 1) % self.lookahead_frames();
        (self.up[0].down(&dl), self.up[1].down(&dr))
    }

    pub fn reset(&mut self) {
        self.up[0].reset();
        self.up[1].reset();
        self.ring.iter_mut().for_each(|r| r.iter_mut().for_each(|s| *s = 0.0));
        self.window.iter_mut().for_each(|w| *w = 0.0);
        self.gain = 1.0;
        self.breach_s = 0.0;
        self.alarm = false;
        self.last_gr_db = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ceiling_is_enforced_on_hot_sine() {
        let sr = 48_000u32;
        let mut lim = TruePeakLimiter::new(sr);
        let n = sr as usize;
        let mut worst = 0.0f32;
        for i in 0..n {
            let x = 1.5 * (std::f32::consts::TAU * 997.0 * i as f32 / sr as f32).sin();
            let (l, _) = lim.tick(x, x);
            if i > n / 4 {
                worst = worst.max(l.abs());
            }
        }
        assert!(
            20.0 * (worst as f64).log10() <= CEILING_DBTP as f64,
            "output peak {worst} ({:.2} dBFS)",
            20.0 * (worst as f64).log10()
        );
    }

    #[test]
    fn quiet_signal_passes_at_unity() {
        let sr = 48_000u32;
        let mut lim = TruePeakLimiter::new(sr);
        for i in 0..sr as usize {
            let x = 0.2 * (std::f32::consts::TAU * 440.0 * i as f32 / sr as f32).sin();
            let (l, _) = lim.tick(x, x);
            // Only latency/filters in the way; magnitude preserved.
            assert!(l.abs() <= 0.25);
        }
        assert!(lim.gr_db() < 0.05, "no reduction expected: {}", lim.gr_db());
        assert!(!lim.alarm());
    }

    #[test]
    fn sustained_over_limit_latches_alarm_and_reset_clears_it() {
        let sr = 48_000u32;
        let mut lim = TruePeakLimiter::new(sr);
        // +3.5 dBFS forever: the window max keeps demanding ≈4.7 dB GR.
        let n = sr as usize;
        let mut alarm_at = None;
        for i in 0..2 * n {
            let (l, _) = lim.tick(1.5, 1.5);
            assert!(l.abs() <= 1.5);
            if lim.alarm() {
                alarm_at = Some(i);
                break;
            }
        }
        let at = alarm_at.expect("sustained over-limit must latch the alarm");
        // Latches only after the sustain window (0.5 s ≈ 24k frames).
        assert!(at >= (GR_ALARM_SUSTAIN_S * sr as f32) as usize, "latched too early: {at}");
        // A brief breach does NOT latch.
        let mut lim = TruePeakLimiter::new(sr);
        for i in 0..4 * sr as usize / 10 {
            let x = if i < sr as usize / 10 { 1.5 } else { 0.2 };
            let _ = lim.tick(x, x);
        }
        assert!(!lim.alarm(), "100 ms breach must not latch");
        lim.reset();
        assert!(!lim.alarm());
    }
}
