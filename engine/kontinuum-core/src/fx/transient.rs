//! Transient designer (FX v2, #30): dual envelope followers — a fast
//! peak-follower and a slow one — whose difference isolates the attack and
//! decay regions of a hit. The scaled difference drives a per-sample gain
//! (attack region: boost/cut via [`crate::params::TRANSIENT_ATTACK`], decay
//! region: sustain boost/cut via `TRANSIENT_SUSTAIN`), one-pole smoothed so
//! the gain itself never steps. The difference is normalized by the slow
//! envelope, so the effect is level-independent; both followers are
//! hard-clamped, so the gain stays inside [0.25, 2.5] for any input up to
//! the clamp ceiling — bounded by construction.
//!
//! Per-sample cost: 4 one-pole updates + a few multiplies. No allocation.

use crate::{InsertFx, ParamId};

/// Follower time constants (ms) at the 48 kHz design rate, coefficient-form
/// via the -60 dB settling approximation in `Smoother`.
const FAST_ATTACK_MS: f32 = 0.5;
const FAST_RELEASE_MS: f32 = 40.0;
const SLOW_ATTACK_MS: f32 = 15.0;
const SLOW_RELEASE_MS: f32 = 250.0;
/// Envelope ceiling: the normalization reference floor and the level the
/// followers clamp to (bounds the gain ratio for inputs within ±2).
const CEIL: f32 = 2.0;
const FLOOR: f32 = 1e-4;

fn coeff(sample_rate: f32, ms: f32) -> f32 {
    (-1000.0 / (ms.max(0.01) * sample_rate)).exp()
}

pub struct TransientDesigner {
    fast_a: f32,
    fast_r: f32,
    slow_a: f32,
    slow_r: f32,
    fast: f32,
    slow: f32,
    gain: f32,
    attack: f32,
    sustain: f32,
    mix: f32,
}

impl TransientDesigner {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        TransientDesigner {
            fast_a: coeff(sr, FAST_ATTACK_MS),
            fast_r: coeff(sr, FAST_RELEASE_MS),
            slow_a: coeff(sr, SLOW_ATTACK_MS),
            slow_r: coeff(sr, SLOW_RELEASE_MS),
            fast: 0.0,
            slow: 0.0,
            gain: 1.0,
            attack: 0.5,
            sustain: 0.5,
            mix: 1.0,
        }
    }

    /// One-pole follower: fast coefficient on the way up (attack), slow on
    /// the way down (release).
    fn follow(state: f32, x: f32, up: f32, down: f32) -> f32 {
        let c = if x > state { up } else { down };
        let v = state + (1.0 - c) * (x - state);
        if v < FLOOR {
            0.0
        } else {
            v
        }
    }
}

impl InsertFx for TransientDesigner {
    fn render(&mut self, io: &mut [f32]) {
        for slot in io.iter_mut() {
            let x = *slot;
            let rect = x.abs().min(CEIL);
            self.fast = Self::follow(self.fast, rect, self.fast_a, self.fast_r);
            self.slow = Self::follow(self.slow, rect, self.slow_a, self.slow_r);
            // Level-independent differential: positive over the attack,
            // negative through the decay. The floor keeps silence stable.
            let norm = 1.0 / self.slow.max(FLOOR * 8.0);
            let diff = ((self.fast - self.slow) * norm).clamp(-1.0, 1.0);
            let target = if diff >= 0.0 {
                1.0 + 1.5 * (self.attack - 0.5) * diff
            } else {
                1.0 + 2.0 * (self.sustain - 0.5) * (-diff)
            };
            // One-pole on the gain itself: no zipper when params move.
            self.gain += 0.002 * (target.clamp(0.25, 2.5) - self.gain);
            *slot = x * self.gain * self.mix + x * (1.0 - self.mix);
        }
        if self.slow < FLOOR {
            self.slow = 0.0;
        }
    }

    fn set_param(&mut self, param: ParamId, value: f32) {
        use crate::params::*;
        match param {
            TRANSIENT_ATTACK => self.attack = value.clamp(0.0, 1.0),
            TRANSIENT_SUSTAIN => self.sustain = value.clamp(0.0, 1.0),
            TRANSIENT_MIX => self.mix = value.clamp(0.0, 1.0),
            _ => {}
        }
    }

    fn reset(&mut self) {
        self.fast = 0.0;
        self.slow = 0.0;
        self.gain = 1.0;
        self.attack = 0.5;
        self.sustain = 0.5;
        self.mix = 1.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exponentially decaying pulse — the canonical drum-envelope shape.
    fn drum_hit() -> Vec<f32> {
        let mut buf = vec![0.0f32; 9600];
        let decay = (-1000.0f32 / (120.0 * 48_000.0)).exp();
        let mut env = 0.9f32;
        for (i, slot) in buf.iter_mut().enumerate() {
            *slot = env * (std::f32::consts::TAU * 180.0 * i as f32 / 48_000.0).sin();
            env *= decay;
            if env < 1e-6 {
                env = 0.0;
            }
        }
        buf
    }

    fn render_with(attack: f32, sustain: f32) -> Vec<f32> {
        let mut t = TransientDesigner::new(48_000);
        t.set_param(crate::params::TRANSIENT_ATTACK, attack);
        t.set_param(crate::params::TRANSIENT_SUSTAIN, sustain);
        let mut buf = drum_hit();
        t.render(&mut buf);
        buf
    }

    #[test]
    fn attack_boost_raises_peaks_and_sustain_boost_raises_tails() {
        let dry = drum_hit();
        let boosted = render_with(1.0, 0.5);
        let cut = render_with(0.0, 0.5);
        let peak = |b: &[f32]| b.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(peak(&boosted) > peak(&dry), "attack boost did not raise the peak");
        assert!(peak(&cut) < peak(&dry), "attack cut did not lower the peak");

        let late = |b: &[f32]| {
            let e: f32 = b[3000..9000].iter().map(|s| s * s).sum();
            e.sqrt()
        };
        let sustain_boost = render_with(0.5, 1.0);
        let sustain_cut = render_with(0.5, 0.0);
        assert!(late(&sustain_boost) > late(&dry) * 1.15, "sustain boost did not fatten the tail");
        assert!(late(&sustain_cut) < late(&dry), "sustain cut did not tighten the tail");
    }

    #[test]
    fn silence_in_is_silence_out_and_neutral_is_near_unity() {
        let mut t = TransientDesigner::new(48_000);
        let mut quiet = vec![0.0f32; 4800];
        t.render(&mut quiet);
        assert!(quiet.iter().all(|&s| s == 0.0), "silence in must be silence out");

        let mut neutral = TransientDesigner::new(48_000);
        let mut buf = drum_hit();
        neutral.render(&mut buf);
        let peak = buf.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(peak.is_finite() && (0.8..1.3).contains(&peak), "neutral gain moved the peak: {peak}");
    }

    #[test]
    fn long_run_bounded_across_loud_and_quiet() {
        let mut t = TransientDesigner::new(48_000);
        t.set_param(crate::params::TRANSIENT_ATTACK, 1.0);
        t.set_param(crate::params::TRANSIENT_SUSTAIN, 1.0);
        let mut buf = [0.0f32; 4800];
        for block in 0..60 {
            let amp = if block % 2 == 0 { 1.5 } else { 0.02 };
            for (i, slot) in buf.iter_mut().enumerate() {
                *slot = amp * (std::f32::consts::TAU * 220.0 * ((block * 4800 + i) as f32) / 48_000.0).sin();
            }
            t.render(&mut buf);
            let peak = buf.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
            assert!(buf.iter().all(|s| s.is_finite()));
            assert!(peak < 5.0, "transient designer gain ran away: {peak}");
        }
    }
}
