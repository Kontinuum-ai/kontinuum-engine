//! Reverb v2 (#30): 8-line FDN with Householder mixing, per-line damping,
//! and a slow modulation on two lines' read positions (fractional) to
//! spread the early reflections — a denser, less metallic tail than the
//! compact 4-line [`super::Reverb`], which stays the default bus reverb so
//! every pre-existing render and golden pin is untouched. Sessions that opt
//! in (offline `send_fx: "fdn8"`, worlds) get this one via
//! [`AudioGraph::set_send_fx`].
//!
//! Partitioned-convolution upgrade path (documented decision, issue #30):
//! the engine's `BusFx::render` contract is variable-length tiles (≤ 64
//! frames, event-span driven), while partitioned convolution needs a fixed
//! uniform block. Doing it cleanly requires (a) an internal fixed-frame
//! FIFO between the graph tile and the convolution engine, (b) a
//! hand-rolled radix-2 complex FFT (no new deps allowed this issue), and
//! (c) licensed IR captures, which are craft-gated (#30: commissioned).
//! Until all three exist, this algorithmic FDN is the shipped reverb v2;
//! the convolution reverb should land as a separate `BusFx` implementing
//! the same wet-only contract, ~128-sample partitions at 2× overlap-save,
//! with the IR loaded at construction and the wet scale matched to
//! `REVERB_WET`. The `BusFx` seam needs no change.
//!
//! Per-sample cost: 8 ring reads + 8 one-pole damp updates + 2 lerp reads +
//! 2 sin (modulation) — roughly 2× the v1 FDN, still well under the
//! voice-class budget in the cost table.

use super::lp_coeff;
use crate::{BusFx, ParamId};
use std::f32::consts::TAU;

/// Incommensurate loop lengths (fractions of a second, prime-ish spread)
/// keep the modal density high without aligned early echoes.
const LOOP_FRACS: [f32; 8] = [0.0241, 0.0317, 0.0373, 0.0419, 0.0467, 0.0523, 0.0587, 0.0643];

const MOD_LINES: [usize; 2] = [2, 5];
const MOD_DEPTH_FRAMES: f32 = 6.0;
const MOD_RATE_HZ: f32 = 0.43;

pub struct ReverbV2 {
    sr: f32,
    bufs: Vec<Box<[f32]>>,
    pos: Vec<usize>,
    lp: [f32; 8],
    g: f32,
    damp_a: f32,
    wet: f32,
    mod_phase: f32,
}

impl ReverbV2 {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let bufs: Vec<Box<[f32]>> = LOOP_FRACS
            .iter()
            .map(|&frac| vec![0.0f32; ((sr * frac).round() as usize).max(8)].into_boxed_slice())
            .collect();
        let pos = vec![0usize; 8];
        let mut rv = ReverbV2 {
            sr,
            bufs,
            pos,
            lp: [0.0; 8],
            g: 0.89,
            damp_a: lp_coeff(sr, 5000.0),
            wet: 1.0,
            mod_phase: 0.0,
        };
        rv.set_damp_cutoff(sr, 5000.0);
        rv
    }

    pub fn set_size(&mut self, size: f32) {
        self.g = (0.82 + size.clamp(0.0, 1.0) * 0.13).min(0.95);
    }

    pub fn set_damp_cutoff(&mut self, sample_rate: f32, cutoff_hz: f32) {
        self.damp_a = lp_coeff(sample_rate, cutoff_hz);
    }

    pub fn set_wet(&mut self, wet: f32) {
        self.wet = wet.clamp(0.0, 2.0);
    }

    /// Fractional read for the two modulated lines. Slow drift only — the
    /// lerp removes the pitch flutter an integer read would alias.
    fn read_mod(&self, line: usize, offset: f32) -> f32 {
        let buf = &self.bufs[line];
        let cap = buf.len();
        let when = offset.clamp(0.0, MOD_DEPTH_FRAMES).min((cap - 2) as f32);
        let whole = when as usize;
        let frac = when - whole as f32;
        let i0 = (self.pos[line] + cap - whole) % cap;
        let i1 = (i0 + 1) % cap;
        buf[i0] + (buf[i1] - buf[i0]) * frac
    }
}

impl BusFx for ReverbV2 {
    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        let t = TAU * self.mod_phase;
        let offsets = [
            MOD_DEPTH_FRAMES * (0.5 + 0.25 * t.sin()),
            MOD_DEPTH_FRAMES * (0.5 + 0.25 * (t + 2.1).sin()),
        ];
        self.mod_phase += MOD_RATE_HZ / self.sr;
        if self.mod_phase >= 1.0 {
            self.mod_phase -= 1.0;
        }
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let x = (*l + *r) * 0.5 * 0.6;
            let mut y = [0.0f32; 8];
            for (k, slot) in y.iter_mut().enumerate() {
                *slot = if k == MOD_LINES[0] {
                    self.read_mod(k, offsets[0])
                } else if k == MOD_LINES[1] {
                    self.read_mod(k, offsets[1])
                } else {
                    self.bufs[k][self.pos[k]]
                };
            }
            let sum: f32 = y.iter().sum::<f32>() * 0.25;
            let mut writes = [0.0f32; 8];
            for ((&y_k, lp_k), w) in y.iter().zip(self.lp.iter_mut()).zip(writes.iter_mut()) {
                *lp_k += self.damp_a * ((y_k - sum) - *lp_k);
                if lp_k.abs() < 1e-20 {
                    *lp_k = 0.0;
                }
                *w = *lp_k * self.g;
            }
            for ((buf, pos_k), w) in self.bufs.iter_mut().zip(self.pos.iter_mut()).zip(writes.iter()) {
                buf[*pos_k] = x + *w;
                *pos_k = (*pos_k + 1) % buf.len();
            }
            *l = (y[0] + y[3] + y[5]) * 0.33 * self.wet;
            *r = (y[1] + y[2] + y[6]) * 0.33 * self.wet;
        }
    }

    fn set_param(&mut self, param: ParamId, value: f32) {
        use crate::params::*;
        match param {
            REVERB_SIZE => self.set_size(value),
            REVERB_DAMP => self.set_damp_cutoff(48_000.0, 8000.0 - value.clamp(0.0, 1.0) * 6800.0),
            REVERB_WET => self.set_wet(value),
            _ => {}
        }
    }

    fn reset(&mut self) {
        for b in &mut self.bufs {
            b.fill(0.0);
        }
        self.pos.iter_mut().for_each(|p| *p = 0);
        self.lp = [0.0; 8];
        self.g = 0.89;
        self.damp_a = lp_coeff(self.sr, 5000.0);
        self.wet = 1.0;
        self.mod_phase = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn impulse_tail_is_dense_finite_and_decaying() {
        let mut rv = ReverbV2::new(48_000);
        let mut l = vec![0.0f32; 96_000];
        let mut r = vec![0.0f32; 96_000];
        l[0] = 1.0;
        r[0] = 1.0;
        rv.render(&mut l, &mut r);
        assert!(l.iter().all(|s| s.is_finite()) && r.iter().all(|s| s.is_finite()));
        let energy = |a: usize, b: usize| l[a..b].iter().map(|s| s * s).sum::<f32>();
        let early = energy(1_000, 11_000);
        let mid = energy(41_000, 51_000);
        let late = energy(86_000, 96_000);
        assert!(early > 0.05, "no early energy: {early}");
        assert!(early > mid * 4.0 && mid > late * 2.0, "tail not decaying: {early} {mid} {late}");
    }

    #[test]
    fn determinism_and_reset_to_exact_silence() {
        let run = || {
            let mut rv = ReverbV2::new(48_000);
            let mut l = vec![0.0f32; 48_000];
            let mut r = vec![0.0f32; 48_000];
            l[100] = 0.8;
            r[100] = 0.8;
            rv.render(&mut l, &mut r);
            (l, r)
        };
        let (a_l, a_r) = run();
        let (b_l, b_r) = run();
        assert!(a_l.iter().zip(b_l.iter()).all(|(x, y)| x.to_bits() == y.to_bits()));
        assert!(a_r.iter().zip(b_r.iter()).all(|(x, y)| x.to_bits() == y.to_bits()));

        let mut rv = ReverbV2::new(48_000);
        rv.reset();
        let mut l = vec![0.0f32; 4800];
        let mut r = vec![0.0f32; 4800];
        rv.render(&mut l, &mut r);
        assert!(l.iter().all(|&s| s == 0.0) && r.iter().all(|&s| s == 0.0));
    }
}
