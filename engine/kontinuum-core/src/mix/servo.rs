//! Per-track gain staging servo (#27): a slow level follower plus a
//! bounded proportional move toward the role's loudness target, expressed
//! relative to the kick anchor. Hard bounds: ±[`GAIN_CORRECTION_MAX_DB`]
//! of correction (the composer's fader intent survives), a
//! [`SERVO_SLEW_DB_PER_S`] slew cap, and a measurement gate so silence
//! and noise floors are never "corrected". The applied gain passes a
//! [`Smoother`], so servo moves are click-free by construction.

use crate::voice::flush_denormal;
use crate::Smoother;

/// Correction cap (dB) around the declared gain — issue #27's ±6 dB bound.
pub const GAIN_CORRECTION_MAX_DB: f32 = 6.0;
/// Hard slew cap for the correction (dB/s) — slower than any musical move.
pub const SERVO_SLEW_DB_PER_S: f32 = 1.5;
/// Servo time constant (ms) ≈ 2 bars @ 120 BPM: a mixing move, not pumping.
pub const SERVO_TAU_MS: f32 = 4_000.0;
/// Level follower time constant (ms) — short-term-ish, faster than the
/// servo it feeds (measurement faster than actuator = stable cascade).
pub const LEVEL_TAU_MS: f32 = 300.0;
/// Below this measured level (dBFS) the servo holds. Silence is not a mix
/// error; lifting a noise floor is.
pub const GATE_DBFS: f32 = -60.0;
/// Inside this error band (dB) the servo does not move.
pub const DEADBAND_DB: f32 = 0.5;

pub(crate) fn db_to_lin(db: f32) -> f32 {
    10.0f32.powf(db / 20.0)
}

pub(crate) fn lin_to_db(lin: f32) -> f32 {
    20.0 * lin.max(1e-6).log10()
}

/// Slow mean-square level follower (linear domain, denormal-flushed).
pub(crate) struct LevelFollower {
    sq: f32,
    coeff: f32,
}

impl LevelFollower {
    pub(crate) fn new(sample_rate: f32, tau_ms: f32) -> Self {
        let coeff = (-1000.0 / (tau_ms.max(0.01) * sample_rate)).exp();
        LevelFollower { sq: 0.0, coeff }
    }

    pub(crate) fn push(&mut self, x: f32) {
        // `coeff` is the per-frame retention factor; the increment toward
        // the new sample is its complement.
        self.sq += (1.0 - self.coeff) * (x * x - self.sq);
        self.sq = flush_denormal(self.sq);
    }

    /// Mean-square level in dBFS (10·log10 of mean square).
    pub(crate) fn level_db(&self) -> f32 {
        10.0 * self.sq.max(1e-12).log10()
    }

    pub(crate) fn reset(&mut self) {
        self.sq = 0.0;
    }
}

/// One track's gain staging: measure → bounded servo → smoothed gain.
pub(crate) struct TrackServo {
    level: LevelFollower,
    /// Current correction in dB, clamped to ±[`GAIN_CORRECTION_MAX_DB`].
    gain_db: f32,
    gain: Smoother,
    frames_seen: u32,
    /// The servo holds until the follower has charged (~1 time constant);
    /// acting on a charging measurement makes it lift, then reverse.
    warmup_frames: u32,
}

impl TrackServo {
    pub(crate) fn new(sample_rate: f32) -> Self {
        let mut gain = Smoother::new(sample_rate, 30.0);
        gain.snap(1.0);
        let warmup_frames = (sample_rate * LEVEL_TAU_MS / 1000.0).ceil() as u32;
        TrackServo {
            level: LevelFollower::new(sample_rate, LEVEL_TAU_MS),
            gain_db: 0.0,
            gain,
            frames_seen: 0,
            warmup_frames,
        }
    }

    /// Feed one tile and return the measured level (dBFS). Call before any
    /// processing so the servo stages the source, not its own output.
    pub(crate) fn measure(&mut self, buf: &[f32]) -> f32 {
        for &s in buf {
            self.level.push(s);
        }
        self.frames_seen = self.frames_seen.saturating_add(buf.len() as u32);
        self.level.level_db()
    }

    /// One servo step: feed-forward alignment of the staged gain to the
    /// role target — the error includes the gain already applied, since
    /// the follower measures the pre-gain source. Slew-capped, bound-
    /// clamped, no integral term: the move shrinks its own error
    /// monotonically, so the loop cannot oscillate.
    pub(crate) fn update(&mut self, dt: f32, target_db: f32) {
        if self.frames_seen < self.warmup_frames {
            return;
        }
        let level_db = self.level.level_db();
        if level_db < GATE_DBFS {
            return;
        }
        let err = target_db - level_db - self.gain_db;
        if err.abs() <= DEADBAND_DB {
            return;
        }
        // Stop at the deadband edge instead of hunting across zero.
        let effective = err - err.signum() * DEADBAND_DB;
        let cap = SERVO_SLEW_DB_PER_S * dt;
        let step = (effective * (dt * 1000.0 / SERVO_TAU_MS)).clamp(-cap, cap);
        self.gain_db =
            (self.gain_db + step).clamp(-GAIN_CORRECTION_MAX_DB, GAIN_CORRECTION_MAX_DB);
        self.gain.set_target(db_to_lin(self.gain_db));
    }

    pub(crate) fn tick_gain(&mut self) -> f32 {
        self.gain.tick()
    }

    pub(crate) fn gain_db(&self) -> f32 {
        self.gain_db
    }

    pub(crate) fn at_bound(&self) -> bool {
        self.gain_db.abs() >= GAIN_CORRECTION_MAX_DB - 1e-3
    }

    pub(crate) fn reset(&mut self) {
        self.level.reset();
        self.gain_db = 0.0;
        self.gain.snap(1.0);
        self.frames_seen = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    fn sine_frames(freq: f32, amp: f32, seconds: f32) -> Vec<f32> {
        let n = (SR as f32 * seconds) as usize;
        (0..n)
            .map(|i| amp * (std::f32::consts::TAU * freq * i as f32 / SR as f32).sin())
            .collect()
    }

    #[test]
    fn loud_track_pulled_down_to_role_target_and_converges() {
        let mut servo = TrackServo::new(SR as f32);
        let frames = sine_frames(55.0, 0.9, 12.0);
        // Kick anchor measured at −9 dBFS (sine RMS): target sits 1 dB below.
        let target_db = -9.03 - 1.0;
        let mut prev = 0.0f32;
        for chunk in frames.chunks(crate::BLOCK_FRAMES) {
            servo.measure(chunk);
            servo.update(chunk.len() as f32 / SR as f32, target_db);
            let g = servo.gain_db();
            assert!(g <= prev + 1e-4, "servo reversed upward: {g} after {prev}");
            assert!(g >= -GAIN_CORRECTION_MAX_DB, "servo blew past the bound: {g}");
            prev = g;
        }
        assert!(prev < -1.0, "loud track not pulled down: {prev}");
        // The servo stages the pre-gain source: convergence is judged on
        // the staged level (source + correction).
        let staged_db = servo.level.level_db() + servo.gain_db();
        assert!((staged_db - target_db).abs() <= 1.5, "did not converge: staged {staged_db} vs target {target_db}");
    }

    #[test]
    fn quiet_track_lifted_toward_target_and_capped() {
        let mut servo = TrackServo::new(SR as f32);
        // −48 dBFS sine vs a −17 dBFS target: correction must pin at +6 dB.
        let frames = sine_frames(220.0, 0.05, 14.0);
        for chunk in frames.chunks(crate::BLOCK_FRAMES) {
            servo.measure(chunk);
            servo.update(chunk.len() as f32 / SR as f32, -17.0);
        }
        assert!(servo.at_bound(), "quiet track should hit the +6 dB cap: {}", servo.gain_db());
    }

    #[test]
    fn silence_holds_gain_untouched() {
        let mut servo = TrackServo::new(SR as f32);
        let silence = vec![0.0f32; SR as usize];
        for chunk in silence.chunks(crate::BLOCK_FRAMES) {
            servo.measure(chunk);
            servo.update(chunk.len() as f32 / SR as f32, -10.0);
        }
        assert_eq!(servo.gain_db(), 0.0, "servo must not chase silence");
    }

    #[test]
    fn servo_moves_are_slew_capped() {
        let mut servo = TrackServo::new(SR as f32);
        let frames = sine_frames(55.0, 0.9, 2.0);
        let mut max_step = 0.0f32;
        let mut prev = servo.gain_db();
        for chunk in frames.chunks(crate::BLOCK_FRAMES) {
            servo.measure(chunk);
            servo.update(chunk.len() as f32 / SR as f32, -30.0);
            let g = servo.gain_db();
            max_step = max_step.max((g - prev).abs());
            prev = g;
        }
        let cap_per_tile = SERVO_SLEW_DB_PER_S * (crate::BLOCK_FRAMES as f32 / SR as f32);
        assert!(max_step <= cap_per_tile + 1e-5, "slew cap violated: {max_step} > {cap_per_tile}");
    }
}
