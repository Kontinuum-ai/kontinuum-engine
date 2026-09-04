//! Equal-power crossfade math (issue #38 step 4) and its beat-aligned
//! quantization.
//!
//! # Curve
//!
//! At fade position `pos ∈ 0..1` (0 = deck A full, 1 = deck B full) the deck
//! gains are the equal-power pair
//!
//! ```text
//! gain_a(pos) = cos(pos·π/2)      gain_b(pos) = sin(pos·π/2)
//! ```
//!
//! so for uncorrelated deck material the summed power is constant
//! (`cos² + sin² = 1`) and neither deck drops in perceived loudness across
//! the move. Each deck is exactly −3.01 dB (factor √½) at the midpoint —
//! the equal-power property the CI test pins. The per-sample gain step is
//! bounded by `π/2 / fade_frames`, so edges are click-free by construction.
//!
//! [`Crossfade`] advances position from an integer frame counter (no float
//! accumulation drift), which keeps the curve exactly test-replicable and
//! deterministic across render chunkings.

/// The equal-power gain pair at fade position `pos` (clamped to 0..1):
/// `(gain_a, gain_b) = (cos, sin)` of `pos·π/2`.
pub fn equal_power_gains(pos: f32) -> (f32, f32) {
    let phase = pos.clamp(0.0, 1.0) * std::f32::consts::FRAC_PI_2;
    (phase.cos(), phase.sin())
}

/// Crossfade duration in frames for a `bars`-long move at `bpm` — the
/// beat-aligned quantization hosts use to land a crossfade on the grid
/// (e.g. 4 bars at 120 BPM, 48 kHz → 384 000 frames = 8 s). Non-finite
/// inputs fall back to their musical defaults; the result is at least one
/// frame.
pub fn crossfade_frames(sample_rate: u32, bars: f32, bpm: f32) -> u32 {
    let bars = if bars.is_finite() { bars.max(0.0) } else { 4.0 };
    let bpm = if bpm.is_finite() { bpm.max(1.0) } else { 120.0 };
    let seconds = bars * 4.0 * 60.0 / bpm;
    ((seconds * sample_rate as f32).round() as u32).max(1)
}

/// Crossfade position state: an integer progress counter over `frames`,
/// driven one frame per output sample by [`Crossfade::tick_gains`].
/// `sweeping` distinguishes an armed A→B move (position advances per
/// sample) from a parked position (holds until the next control move);
/// `frames == 0` is the parked-on-A state.
#[derive(Clone, Copy, Debug)]
pub struct Crossfade {
    frames: u32,
    progress: u32,
    sweeping: bool,
}

/// Position resolution for `park` when no sweep length has been armed
/// (1/1024 ≈ 0.1 % quantization, far below audible).
const PARK_FRAMES: u32 = 1_024;

impl Crossfade {
    /// Parked fully on deck A (holds there until a sweep or park moves it).
    pub fn new() -> Self {
        Crossfade { frames: 0, progress: 0, sweeping: false }
    }

    /// Arm an A→B sweep over `frames` output samples (starts at position 0).
    pub fn begin(&mut self, frames: u32) {
        self.frames = frames.max(1);
        self.progress = 0;
        self.sweeping = true;
    }

    /// Park at fade position `pos` (0 = deck A, 1 = deck B) without sweeping.
    /// The position holds until the next control move.
    pub fn park(&mut self, pos: f32) {
        let pos = pos.clamp(0.0, 1.0);
        self.sweeping = false;
        if pos <= 0.0 {
            self.frames = 0;
            self.progress = 0;
        } else {
            self.frames = self.frames.max(PARK_FRAMES);
            self.progress = (pos * self.frames as f32).round() as u32;
        }
    }

    /// Current fade position 0..1.
    pub fn position(&self) -> f32 {
        if self.frames == 0 {
            0.0
        } else {
            self.progress.min(self.frames) as f32 / self.frames as f32
        }
    }

    /// Advance one output sample and return `(gain_a, gain_b)`. A sweep
    /// advances until it reaches deck B; a parked position holds.
    pub fn tick_gains(&mut self) -> (f32, f32) {
        let pos = if self.frames == 0 {
            0.0
        } else {
            self.progress.min(self.frames) as f32 / self.frames as f32
        };
        if self.sweeping && self.progress < self.frames {
            self.progress += 1;
        }
        let phase = pos * std::f32::consts::FRAC_PI_2;
        (phase.cos(), phase.sin())
    }
}

impl Default for Crossfade {
    fn default() -> Self {
        Crossfade::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_match_analytic_values_at_quarter_positions() {
        const TOL: f32 = 1e-6;
        for pos in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
            let (ga, gb) = equal_power_gains(pos);
            let phase = pos * std::f32::consts::FRAC_PI_2;
            assert!((ga - phase.cos()).abs() < TOL, "deck A curve off at {pos}");
            assert!((gb - phase.sin()).abs() < TOL, "deck B curve off at {pos}");
            // The equal-power invariant: unit total power at every position.
            let power = ga as f64 * ga as f64 + gb as f64 * gb as f64;
            assert!((power - 1.0).abs() < 1e-6, "power {power} at pos {pos}");
        }
        assert_eq!(equal_power_gains(0.0).0.to_bits(), 1.0f32.to_bits(), "start position is not deck A full");
    }

    #[test]
    fn midpoint_is_minus_3db_per_deck() {
        for g in [equal_power_gains(0.5).0, equal_power_gains(0.5).1] {
            let db = 20.0 * (g as f64).log10();
            assert!((db + 3.0103).abs() < 0.01, "midpoint gain {db:.4} dB, expected −3.01 dB");
        }
    }

    #[test]
    fn bars_and_bpm_map_to_frames() {
        // 4 bars @ 120 BPM = 8 s = 384 000 frames @ 48 kHz.
        assert_eq!(crossfade_frames(48_000, 4.0, 120.0), 384_000);
        // 1 bar @ 174 BPM ≈ 1.37931 s ≈ 66 207 frames @ 48 kHz.
        assert_eq!(crossfade_frames(48_000, 1.0, 174.0), 66_207);
        // Degenerate inputs clamp: zero bars → one frame; non-finite bars
        // fall back to the 4-bar default.
        assert_eq!(crossfade_frames(48_000, 0.0, 120.0), 1);
        assert_eq!(crossfade_frames(48_000, f32::NAN, 120.0), 384_000);
    }

    #[test]
    fn sweep_matches_the_parked_curve_per_frame() {
        // Integer-counter position: the swept gains must equal the analytic
        // curve evaluated at k/frames for every frame k.
        let frames = 1_000u32;
        let mut xf = Crossfade::new();
        xf.begin(frames);
        for k in 0..=frames {
            let (ga, gb) = xf.tick_gains();
            let (ea, eb) = equal_power_gains(k as f32 / frames as f32);
            assert_eq!(ga.to_bits(), ea.to_bits(), "deck A gain drift at frame {k}");
            assert_eq!(gb.to_bits(), eb.to_bits(), "deck B gain drift at frame {k}");
        }
        assert_eq!(xf.position(), 1.0, "sweep did not park on deck B");
        // Holding past the end stays at the B end of the curve.
        let (ga, gb) = xf.tick_gains();
        let (ea, eb) = equal_power_gains(1.0);
        assert_eq!(ga.to_bits(), ea.to_bits());
        assert_eq!(gb.to_bits(), eb.to_bits());
    }

    #[test]
    fn parked_positions_hold_across_samples() {
        // Parked on A: every sample is the (1, 0) pair.
        let mut xf = Crossfade::new();
        for k in 0..10 {
            let (ga, gb) = xf.tick_gains();
            assert_eq!(ga.to_bits(), 1.0f32.to_bits(), "parked A drifted at sample {k}");
            assert_eq!(gb.to_bits(), 0.0f32.to_bits(), "parked A leaked deck B at sample {k}");
        }
        // Parked mid and on B: the position holds, sample after sample.
        for pos in [0.25f32, 0.5, 1.0] {
            let mut xf = Crossfade::new();
            xf.park(pos);
            assert!((xf.position() - pos).abs() < 1.0 / PARK_FRAMES as f32);
            let first = xf.tick_gains();
            for _ in 0..10 {
                let (ga, gb) = xf.tick_gains();
                assert_eq!(ga.to_bits(), first.0.to_bits(), "park {pos} drifted");
                assert_eq!(gb.to_bits(), first.1.to_bits(), "park {pos} drifted");
            }
        }
    }
}
