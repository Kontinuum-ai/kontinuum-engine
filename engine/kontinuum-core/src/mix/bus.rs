//! Per-bus glue (#27): a slow stereo-linked VCA compressor (ratio ≤ 2:1,
//! 10 ms / 100 ms ballistics, gain reduction hard-capped) plus gentle tanh
//! saturation staging with a bounded drive ceiling. Full-scale-normalized,
//! so A/B against bypass is honest. No limiter lives here — the master
//! path is kontinuum-mastering's job (#28).

use crate::Smoother;

/// Bus compressor ratio — under the #27 2:1 ceiling.
pub const BUS_RATIO: f32 = 1.5;
/// Compressor threshold (dBFS RMS of the linked stereo signal).
pub const BUS_THRESHOLD_DB: f32 = -12.0;
/// Hard cap on gain reduction — glue, not ducking.
pub const BUS_GR_CAP_DB: f32 = 3.0;
/// #27 glue ballistics.
const ATTACK_MS: f32 = 10.0;
const RELEASE_MS: f32 = 100.0;
/// Click guard on the applied bus gain.
const GAIN_SMOOTH_MS: f32 = 5.0;
/// Saturation drive range: 1.0 is exact passthrough, the ceiling is the
/// "drive ceiling" of #27. The default ships transparent.
pub const DRIVE_MIN: f32 = 1.0;
pub const DRIVE_MAX: f32 = 2.0;
pub const DRIVE_DEFAULT: f32 = 1.0;

fn one_pole_coeff(sample_rate: f32, tau_ms: f32) -> f32 {
    (-1000.0 / (tau_ms.max(0.01) * sample_rate)).exp()
}

/// One bus (drums or harmonic): compress, then saturate.
pub struct BusChain {
    env: f32,
    attack_coeff: f32,
    release_coeff: f32,
    gain: f32,
    gain_coeff: f32,
    last_gr_db: f32,
    drive: Smoother,
}

impl BusChain {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let mut drive = Smoother::new(sr, 30.0);
        drive.snap(DRIVE_DEFAULT);
        BusChain {
            env: 0.0,
            attack_coeff: 1.0 - one_pole_coeff(sr, ATTACK_MS),
            release_coeff: one_pole_coeff(sr, RELEASE_MS),
            gain: 1.0,
            gain_coeff: one_pole_coeff(sr, GAIN_SMOOTH_MS),
            last_gr_db: 0.0,
            drive,
        }
    }

    /// Drive, clamped to the ceiling; 1.0 = bypass. Smoothed, so changes
    /// on the RT path are click-free.
    pub fn set_drive(&mut self, drive: f32) {
        let d = if drive.is_finite() { drive } else { DRIVE_DEFAULT };
        self.drive.set_target(d.clamp(DRIVE_MIN, DRIVE_MAX));
    }

    /// Configured drive (the clamped target) — the slewed value follows
    /// within milliseconds.
    pub fn drive(&self) -> f32 {
        self.drive.target()
    }

    /// Gain reduction of the last processed frame (positive dB).
    pub fn gr_db(&self) -> f32 {
        self.last_gr_db
    }

    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let linked = l.abs().max(r.abs());
            if linked > self.env {
                self.env += self.attack_coeff * (linked - self.env);
            } else {
                self.env += (1.0 - self.release_coeff) * (linked - self.env);
            }
            self.env = crate::voice::flush_denormal(self.env);
            let env_db = 20.0 * (self.env as f64).max(1e-12).log10() as f32;
            let over_db = (env_db - BUS_THRESHOLD_DB).max(0.0);
            let gr_db = ((1.0 - 1.0 / BUS_RATIO) * over_db).min(BUS_GR_CAP_DB);
            self.last_gr_db = gr_db;
            let target_gain = 10.0f32.powf(-gr_db / 20.0);
            self.gain += (1.0 - self.gain_coeff) * (target_gain - self.gain);

            let d = self.drive.tick();
            if d > DRIVE_MIN + f32::EPSILON {
                let norm = d.tanh().max(1e-6);
                *l = ((*l * self.gain) * d).tanh() / norm;
                *r = ((*r * self.gain) * d).tanh() / norm;
            } else {
                *l *= self.gain;
                *r *= self.gain;
            }
        }
    }

    pub fn reset(&mut self) {
        self.env = 0.0;
        self.gain = 1.0;
        self.last_gr_db = 0.0;
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
    fn bus_compressor_engages_and_stays_bounded_on_hot_input() {
        let mut bus = BusChain::new(SR);
        let mut l = tone(80.0, 1.2, SR as usize * 2);
        let mut r = tone(80.0, 1.2, SR as usize * 2);
        for chunk in l.chunks_mut(crate::BLOCK_FRAMES) {
            let r_chunk = &mut r[..chunk.len()];
            bus.process(chunk, r_chunk);
            assert!(bus.gr_db() >= 0.0);
            assert!(bus.gr_db() <= BUS_GR_CAP_DB + 1e-4, "GR over cap: {}", bus.gr_db());
        }
        assert!(bus.gr_db() > 0.3, "compressor never engaged on hot input: {}", bus.gr_db());
        assert!(l.iter().chain(r.iter()).all(|s| s.is_finite()));
    }

    #[test]
    fn quiet_program_is_untouched_by_bus_glue() {
        let mut bus = BusChain::new(SR);
        let mut l = tone(440.0, 0.05, SR as usize);
        let mut r = l.clone();
        for chunk in l.chunks_mut(crate::BLOCK_FRAMES) {
            bus.process(chunk, &mut r[..chunk.len()]);
        }
        assert!(bus.gr_db() < 0.05, "quiet program compressed: {}", bus.gr_db());
    }

    #[test]
    fn drive_ceiling_is_enforced_and_one_is_passthrough() {
        let mut bus = BusChain::new(SR);
        bus.set_drive(9.9);
        assert!((bus.drive() - DRIVE_MAX).abs() < 1e-5, "drive ceiling not enforced");
        bus.set_drive(f32::NAN);
        assert!((bus.drive() - DRIVE_DEFAULT).abs() < 1e-5);

        // Drive 1.0: bit-exact passthrough apart from gain (1.0 while the
        // program sits below the glue threshold).
        let mut bus = BusChain::new(SR);
        let mut l = tone(300.0, 0.1, SR as usize);
        let mut r = l.clone();
        for chunk in l.chunks_mut(crate::BLOCK_FRAMES) {
            bus.process(chunk, &mut r[..chunk.len()]);
        }
        let dry = tone(300.0, 0.1, SR as usize);
        let ratio = rms(&l) / rms(&dry);
        assert!((ratio - 1.0).abs() < 0.01, "unity drive changed level: {ratio}");
    }

    #[test]
    fn bus_silence_stays_silent() {
        let mut bus = BusChain::new(SR);
        bus.set_drive(1.8);
        let mut l = vec![0.0f32; crate::BLOCK_FRAMES * 4];
        let mut r = vec![0.0f32; crate::BLOCK_FRAMES * 4];
        for k in 0..4 {
            bus.process(&mut l[k * crate::BLOCK_FRAMES..(k + 1) * crate::BLOCK_FRAMES],
                        &mut r[k * crate::BLOCK_FRAMES..(k + 1) * crate::BLOCK_FRAMES]);
        }
        assert!(l.iter().chain(r.iter()).all(|s| *s == 0.0), "bus made silence non-silent");
        assert_eq!(bus.gr_db(), 0.0);
    }
}
