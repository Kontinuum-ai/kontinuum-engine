//! Master chain: smoothed gain, tanh soft-clip (±1.2 asymptote), block-wise
//! safety limiter to 0.99. Allocation-free and deterministic.

use crate::Smoother;

pub struct MasterChain {
    gain: Smoother,
}

fn soft_clip(x: f32) -> f32 {
    1.2 * (x / 1.2).tanh()
}

impl MasterChain {
    pub fn new(sample_rate: u32) -> Self {
        let mut m = MasterChain { gain: Smoother::new(sample_rate as f32, 30.0) };
        m.gain.snap(1.0);
        m
    }

    pub fn set_gain_target(&mut self, value: f32) {
        self.gain.set_target(value.clamp(0.0, 4.0));
    }

    pub fn snap_gain(&mut self, value: f32) {
        self.gain.snap(value.clamp(0.0, 4.0));
    }

    pub fn gain_value(&self) -> f32 {
        self.gain.value()
    }

    pub fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let g = self.gain.tick();
            *l = soft_clip(*l * g);
            *r = soft_clip(*r * g);
        }
        let mut peak = 0.0f32;
        for s in left.iter().chain(right.iter()) {
            peak = peak.max(s.abs());
        }
        if peak > 0.99 {
            let scale = 0.99 / peak;
            for s in left.iter_mut().chain(right.iter_mut()) {
                *s *= scale;
            }
        }
    }

    pub fn reset(&mut self) {
        self.gain.snap(1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_never_exceeds_unity() {
        let mut m = MasterChain::new(48_000);
        let mut l = vec![2.5f32; 256];
        let mut r = vec![-3.0f32; 256];
        m.render(&mut l, &mut r);
        assert!(l.iter().chain(r.iter()).all(|s| s.abs() <= 1.0));
    }
}
