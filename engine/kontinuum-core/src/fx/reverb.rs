//! Compact 4-channel FDN reverb with Householder feedback and damping.
//! Wet-only; ~2.2 s tail at default size.

use super::lp_coeff;
use crate::{BusFx, ParamId};

const LOOP_FRACS: [f32; 4] = [0.0297, 0.0371, 0.0411, 0.0437];

pub struct Reverb {
    bufs: Vec<Box<[f32]>>,
    pos: Vec<usize>,
    lp: [f32; 4],
    g: f32,
    damp_a: f32,
    wet: f32,
}

impl Reverb {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let bufs: Vec<Box<[f32]>> = LOOP_FRACS
            .iter()
            .map(|&frac| vec![0.0f32; ((sr * frac).round() as usize).max(8)].into_boxed_slice())
            .collect();
        let pos = vec![0usize; 4];
        let mut rv = Reverb { bufs, pos, lp: [0.0; 4], g: 0.89, damp_a: lp_coeff(sr, 5000.0), wet: 1.0 };
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
}

impl BusFx for Reverb {
    fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let x = (*l + *r) * 0.5 * 0.7;
            let y: [f32; 4] = [
                self.bufs[0][self.pos[0]],
                self.bufs[1][self.pos[1]],
                self.bufs[2][self.pos[2]],
                self.bufs[3][self.pos[3]],
            ];
            let sum = (y[0] + y[1] + y[2] + y[3]) * 0.5;
            let mut writes = [0.0f32; 4];
            for ((&y_k, lp_k), w) in y.iter().zip(self.lp.iter_mut()).zip(writes.iter_mut()) {
                *lp_k += self.damp_a * ((y_k - sum) - *lp_k);
                if lp_k.abs() < 1e-20 {
                    *lp_k = 0.0;
                }
                *w = *lp_k * self.g;
            }
            for ((buf, pos_k), w) in
                self.bufs.iter_mut().zip(self.pos.iter_mut()).zip(writes.iter())
            {
                buf[*pos_k] = x + *w;
                *pos_k = (*pos_k + 1) % buf.len();
            }
            *l = (y[0] + y[3]) * 0.5 * self.wet;
            *r = (y[1] + y[2]) * 0.5 * self.wet;
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
        self.lp = [0.0; 4];
        self.g = 0.89;
        self.damp_a = lp_coeff(48_000.0, 5000.0);
        self.wet = 1.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn impulse_tail_decays_and_is_finite() {
        // First reflections arrive at ~30ms (loop lengths), so decay is compared
        // over 1 s windows, not the first millisecond.
        let mut reverb = Reverb::new(48_000);
        let mut l = vec![0.0f32; 48_000];
        let mut r = vec![0.0f32; 48_000];
        l[0] = 1.0;
        r[0] = 1.0;
        reverb.render(&mut l, &mut r);
        assert!(l.iter().all(|s| s.is_finite()) && r.iter().all(|s| s.is_finite()));
        let head: f32 = l[1000..11_000].iter().map(|s| s * s).sum();
        let tail: f32 = l[38_000..].iter().map(|s| s * s).sum();
        assert!(head > 0.05, "reverb produced no energy: {head}");
        assert!(head > tail * 10.0, "reverb tail not decaying: head {head} tail {tail}");
    }
}
