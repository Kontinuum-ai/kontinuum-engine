//! Stage 4 — soft clipper (#28): transient shaving before the limiter,
//! ×4 oversampled so the clipping harmonics stay above the audio band
//! instead of aliasing back in.
//!
//! Curve: a C1-continuous quadratic knee — unity slope below the knee,
//! quadratic roll-off into a hard ceiling, exactly flat at the ceiling.
//! Ceiling sits at −1.2 dBFS, under the limiter's −1.0 dBTP guarantee,
//! so the clipper does the gentle work and the limiter stays clean.
//!
//! Drive is auto-set: a slow seeker holds the mean clip reduction near
//! 0.8 dB, which bounds the stage's loudness push to ≤ 1 dB (issue #28's
//! constraint). Drive is bounded [0, +6] dB and slews ≤ 0.5 dB/s.

use crate::filters::Slew1p;
use crate::oversample::Oversampler4x;

/// Clip ceiling (dBFS) — under the limiter's working point.
pub const CLIP_CEILING_DB: f32 = -1.2;
/// Mean reduction the drive seeker holds (bounds loudness push).
const GR_TARGET_DB: f32 = 0.8;
const DRIVE_MAX_DB: f32 = 6.0;
const DRIVE_SLEW_DB_PER_S: f32 = 0.5;

fn one_pole_coeff(sample_rate: f32, tau_ms: f32) -> f32 {
    (-1000.0 / (tau_ms.max(0.01) * sample_rate)).exp()
}

/// Quadratic-knee soft clip with slope continuity at both joints: unity
/// slope below the knee, parabolic roll-off over a knee of width
/// `2·(ceiling − knee)`, hard ceiling exactly at `ceiling`.
fn clip_sample(x: f32, knee: f32, ceiling: f32) -> f32 {
    let a = x.abs();
    let width = 2.0 * (ceiling - knee);
    if a <= knee {
        return x;
    }
    if a >= knee + width {
        return x.signum() * ceiling;
    }
    // y = a − (a−k)²/(2w): slope 1 at the knee, slope 0 at the ceiling,
    // y(k + w) = knee + w/2 = ceiling.
    x.signum() * (a - (a - knee) * (a - knee) / (2.0 * width))
}

/// Stereo-linked oversampled soft clipper. Two [`Oversampler4x`]s
/// (allocation in `new()` only); per input frame: up ×4, clip each
/// subsample, down ×4.
pub struct SoftClipper {
    sample_rate: f32,
    up: [Oversampler4x; 2],
    ceiling: f32,
    knee: f32,
    drive_db: Slew1p,
    /// Slow mean of clip reduction (positive dB).
    gr_mean: f32,
    gr_mean_coeff: f32,
    /// Breakdown relaxation 0..1 (drive target → 0).
    relax: f32,
    /// Last frame's mean clip reduction, for telemetry.
    last_gr_db: f32,
}

impl SoftClipper {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let ceiling = 10.0f32.powf(CLIP_CEILING_DB / 20.0);
        let mut clipper = SoftClipper {
            sample_rate: sr,
            up: [Oversampler4x::new(), Oversampler4x::new()],
            ceiling,
            knee: ceiling * 0.5,
            drive_db: Slew1p::new(sr, 1_000.0),
            gr_mean: 0.0,
            gr_mean_coeff: one_pole_coeff(sr, 1_500.0),
            relax: 0.0,
            last_gr_db: 0.0,
        };
        clipper.drive_db.snap(0.0);
        clipper
    }

    /// Section-aware relaxation (0 = full intensity, 1 = breakdown).
    pub fn set_relax(&mut self, relax: f32) {
        self.relax = relax.clamp(0.0, 1.0);
    }

    /// Process one stereo frame through the ×4 oversampled curve.
    /// Returns the mean clip reduction (positive dB) of this frame.
    pub fn tick(&mut self, left: f32, right: f32) -> (f32, f32) {
        let drive = 10.0f32.powf(self.drive_db.value() / 20.0);
        let mut ul = [0.0f32; 4];
        let mut ur = [0.0f32; 4];
        self.up[0].up(left * drive, &mut ul);
        self.up[1].up(right * drive, &mut ur);
        let mut cut_db_sum = 0.0f32;
        let mut hot = 0usize;
        for k in 0..4 {
            let a = ul[k].abs();
            if a > self.knee {
                let y = clip_sample(ul[k], self.knee, self.ceiling);
                cut_db_sum += 20.0 * (a / y.max(1e-9)).log10();
                hot += 1;
                ul[k] = y;
            }
            let b = ur[k].abs();
            if b > self.knee {
                let y = clip_sample(ur[k], self.knee, self.ceiling);
                cut_db_sum += 20.0 * (b / y.max(1e-9)).log10();
                hot += 1;
                ur[k] = y;
            }
        }
        // Mean per-subsample reduction in dB; 0 when nothing clips.
        let gr_db = if hot > 0 { cut_db_sum / hot as f32 } else { 0.0 };
        self.gr_mean += self.gr_mean_coeff * (gr_db - self.gr_mean);
        self.last_gr_db = gr_db;

        // Drive seeker: hold mean reduction at the target (drive only
        // ever ADDS push, so signal that already exceeds the ceiling
        // clips by physics — the seeker adds gain only while the mix is
        // leaving headroom unused). Breakdowns unwind the drive to zero.
        // Explicit rate cap on top of the 1 s one-pole.
        let want = if self.relax < 0.5 {
            ((GR_TARGET_DB - self.gr_mean) * 2.0).clamp(0.0, DRIVE_MAX_DB)
        } else {
            0.0
        };
        let max_step = DRIVE_SLEW_DB_PER_S / self.sample_rate;
        let cur = self.drive_db.value();
        self.drive_db.set_target(want.clamp(cur - max_step, cur + max_step));
        self.drive_db.tick();

        let ol = self.up[0].down(&ul);
        let or = self.up[1].down(&ur);
        (ol, or)
    }

    pub fn gr_db(&self) -> f32 {
        self.gr_mean
    }

    pub fn drive_db(&self) -> f32 {
        self.drive_db.value()
    }

    pub fn last_frame_gr_db(&self) -> f32 {
        self.last_gr_db
    }

    pub fn reset(&mut self) {
        self.up[0].reset();
        self.up[1].reset();
        self.gr_mean = 0.0;
        self.last_gr_db = 0.0;
        self.drive_db.snap(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_is_c1_and_never_exceeds_ceiling() {
        let ceiling = 10.0f32.powf(CLIP_CEILING_DB / 20.0);
        let knee = ceiling * 0.5;
        // Continuity of value and slope across both joints.
        let mut prev = clip_sample(knee - 0.001, knee, ceiling);
        let mut max_slope = 0.0f32;
        let mut x = knee - 0.001;
        while x <= ceiling + 0.5 {
            let y = clip_sample(x, knee, ceiling);
            assert!(y.abs() <= ceiling + 1e-6, "above ceiling at {x}: {y}");
            let slope = (y - prev) / 0.001;
            max_slope = max_slope.max(slope.abs());
            assert!(slope.abs() <= 1.0 + 0.05, "slope spike {slope} at {x}");
            prev = y;
            x += 0.001;
        }
    }

    #[test]
    fn hot_peaks_are_shaved_toward_ceiling() {
        let sr = 48_000u32;
        let mut c = SoftClipper::new(sr);
        let ceiling = 10.0f32.powf(CLIP_CEILING_DB / 20.0);
        for i in 0..sr as usize {
            let x = 1.4 * (std::f32::consts::TAU * 997.0 * i as f32 / sr as f32).sin();
            let (l, _) = c.tick(x, x);
            assert!(l.abs() <= ceiling * 1.02 + 1e-4, "leak {}: {}", l, ceiling);
        }
        assert!(c.gr_db() > 0.2, "hot sine must register reduction: {}", c.gr_db());
        // Signal well below the knee: drive unwinds, GR returns to ~0.
        for i in 0..4 * sr as usize {
            let x = 0.1 * (std::f32::consts::TAU * 997.0 * i as f32 / sr as f32).sin();
            let _ = c.tick(x, x);
        }
        assert!(c.gr_db() < 0.15, "GR must relax when signal is small: {}", c.gr_db());
    }
}
