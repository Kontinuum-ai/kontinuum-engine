//! Pattern generator toolkit (issue #16): rhythm shapes and groove
//! transforms that operate on IR step data.
//!
//! Determinism contract: every function is pure; the only randomness enters
//! through the caller's [`kontinuum_clock::Rng`] stream, so the same
//! `(seed, track, purpose)` triple reproduces the same output bit-for-bit.
//! All produced values stay inside the `kontinuum-ir` bounds (positions
//! within the bar, microtiming ±120 ticks, velocity 0..=1, ratchet 1..=8).

use kontinuum_clock::Rng;
use kontinuum_ir::schema::{bounds, Step};

/// Ticks per 16th note at `ppq_tick` ticks per quarter note (240 at PPQ 960).
fn ticks_per_sixteenth(ppq_tick: i64) -> i64 {
    (ppq_tick / 4).max(1)
}

/// Deterministic Euclidean rhythm, re-exported from the IR compiler.
///
/// This used to be a second, independent implementation (a bucket/pigeonhole
/// walk) that disagreed with the compiler's: for `E(3, 16)` the bucket version
/// fires at slots 5, 10, 15 while the compiler's fires at 0, 5, 10. Patterns
/// built here become explicit steps and patterns left as `Pattern::Euclidean`
/// are expanded there, so the two algorithms were laying material on two
/// different grids inside one bar. There is one grid now.
pub use kontinuum_ir::compile::expand::euclidean;

/// Rotation (in slots) that brings a grid's first onset to slot 0, keeping
/// generated phrases downbeat-anchored.
pub fn first_onset_rot(grid: &[bool]) -> i32 {
    grid.iter()
        .position(|on| *on)
        .map(|i| i as i32)
        .unwrap_or(0)
}

/// Swing: delays every odd 16th by `swing` (clamped 0..0.5) of a 16th.
/// `swing = 0` is straight time (no-op); `0.5` would place offbeats on the
/// following 8th triplet line, so it is the ceiling. Positions are ticks
/// within one bar; `ppq_tick` is ticks per quarter note.
pub fn apply_swing(positions_ticks: &mut [i64], swing: f32, ppq_tick: i64) {
    let swing = swing.clamp(0.0, 0.5);
    if swing == 0.0 {
        return;
    }
    let sixteenth = ticks_per_sixteenth(ppq_tick);
    let delay = (swing * sixteenth as f32).round() as i64;
    for p in positions_ticks.iter_mut() {
        if *p >= 0 && (*p / sixteenth) % 2 == 1 {
            *p += delay;
        }
    }
}

/// Humanization: seeded microtiming and velocity jitter around a step's
/// nominal position/velocity. Jitter spreads are clamped so results stay in
/// IR bounds: the timing offset never exceeds ±[`bounds::MICROTIMING_TICKS`]
/// (±120 ticks) and velocity stays in 0..=1.
pub fn humanize(
    ticks: i64,
    velocity: f32,
    rng: &mut Rng,
    timing_spread_ticks: i64,
    vel_spread: f32,
) -> (i64, f32) {
    let spread = timing_spread_ticks.clamp(0, i64::from(bounds::MICROTIMING_TICKS.1));
    let dt = rng.range_f32(-(spread as f32), spread as f32).round() as i64;
    let pos = (ticks + dt).max(0);
    let vel = (velocity + rng.range_f32(-vel_spread, vel_spread)).clamp(0.0, 1.0);
    (pos, vel)
}

/// Flam/ratchet expansion: `sub_hits` (clamped 1..=8) sub-steps spaced 14
/// ticks apart (~⅒ of a 16th) with a 0.85 geometric velocity falloff. All
/// sub-steps stay inside the bar; positions that would overflow the last
/// 16th are clamped.
pub fn ratchet_steps(position_ticks: u32, velocity: f32, sub_hits: u8) -> Vec<Step> {
    const SPACING_TICKS: u32 = 14;
    const FALLOFF: f32 = 0.85;
    let bar_end = kontinuum_clock::TICKS_PER_BAR as u32;
    let count = sub_hits.clamp(bounds::RATCHET.0, bounds::RATCHET.1);
    (0..count)
        .map(|j| Step {
            position: (position_ticks + j as u32 * SPACING_TICKS).min(bar_end - 1),
            velocity: (velocity * FALLOFF.powi(i32::from(j))).clamp(0.0, 1.0),
            probability: 1.0,
            microtiming_ticks: 0,
            ratchet: 1,
            pitch: None,
            gate: None,
            accent: false,
        })
        .collect()
}

/// Boolean combination of two rhythm masks (issue #17): polyrhythmic layers
/// are built by overlaying grids — `union` fires either onset, `intersect`
/// only shared onsets, `xor` only the asymmetry. Masks of different lengths
/// wrap cyclically to the longer of the two.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaskOp {
    Union,
    Intersect,
    Xor,
}

pub fn combine_masks(op: MaskOp, a: &[bool], b: &[bool]) -> Vec<bool> {
    let len = a.len().max(b.len()).max(1);
    let at = |m: &[bool], i: usize| m.get(i % m.len().max(1)).copied().unwrap_or(false);
    (0..len)
        .map(|i| match op {
            MaskOp::Union => at(a, i) || at(b, i),
            MaskOp::Intersect => at(a, i) && at(b, i),
            MaskOp::Xor => at(a, i) ^ at(b, i),
        })
        .collect()
}

/// Velocity-contour archetypes (issue #17): the named dynamics shapes a
/// percussion layer rides over the bar. `slot` is the 16th slot 0..16.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VelContour {
    /// Firm downbeats, ghosted everything else — the anchor.
    DownbeatHeavy,
    /// The offbeat-eighth accent: downbeats pulled back, offbeats pushed —
    /// the techno/house engine-room shape.
    OffAccent,
    /// Every slot equal — the machine look.
    FlatMachine,
    /// Mildly rising across the bar with a seeded wobble (applied by the
    /// caller's jitter on top) — the humanized ± shape.
    Humanized,
}

impl VelContour {
    /// Velocity multiplier for `slot` (16 slots per bar); callers clamp the
    /// product into 0..=1.
    pub fn multiplier(self, slot: usize) -> f32 {
        let on_beat = slot % 4 == 0;
        let on_eighth = slot % 2 == 0;
        match self {
            VelContour::DownbeatHeavy => {
                if on_beat { 1.0 } else if on_eighth { 0.7 } else { 0.5 }
            }
            VelContour::OffAccent => {
                if on_beat { 0.78 } else if on_eighth { 1.0 } else { 0.58 }
            }
            VelContour::FlatMachine => 1.0,
            VelContour::Humanized => 0.85 + 0.15 * (slot as f32 / 15.0),
        }
    }
}

/// Seeded standard-normal sample (Box–Muller, one uniform pair per draw).
/// Deterministic in the caller's stream like every other generator here.
pub fn gauss(rng: &mut Rng) -> f32 {
    let u = rng.next_f32().max(f32::EPSILON);
    let v = rng.next_f32();
    (-2.0 * u.ln()).sqrt() * (std::f32::consts::TAU * v).cos()
}

/// Per-step jitter σ mapped from the density curve (issue #17): quiet
/// sections sit near 1 tick, hot sections near 4 — 1–4 ticks @ 960 PPQ.
pub fn jitter_sigma(density: f32) -> f32 {
    1.0 + 3.0 * density.clamp(0.0, 1.0)
}

/// Gaussian humanization: seeded timing and velocity jitter around a step's
/// nominal position/velocity, with the same bounds contract as
/// [`humanize`] — timing offset never exceeds ±[`bounds::MICROTIMING_TICKS`],
/// velocity stays in 0..=1.
pub fn humanize_gauss(
    ticks: i64,
    velocity: f32,
    rng: &mut Rng,
    sigma_ticks: f32,
    sigma_vel: f32,
) -> (i64, f32) {
    let sigma = sigma_ticks.clamp(0.0, f32::from(bounds::MICROTIMING_TICKS.1) / 3.0);
    let dt = (gauss(rng) * sigma).round() as i64;
    let pos = (ticks + dt).max(0);
    let vel = (velocity + gauss(rng) * sigma_vel).clamp(0.0, 1.0);
    (pos, vel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn euclidean_spreads_and_rotates() {
        let g = euclidean(4, 16, 0);
        assert_eq!(g.iter().filter(|x| **x).count(), 4);
        assert_eq!(euclidean(0, 16, 0).iter().filter(|x| **x).count(), 0);
        assert_eq!(euclidean(20, 8, 0).iter().filter(|x| **x).count(), 8);
        assert!(euclidean(3, 0, 0).is_empty());
        // Rotation preserves the onset count and wraps cyclically.
        assert_eq!(euclidean(4, 16, 5).iter().filter(|x| **x).count(), 4);
        assert_eq!(euclidean(4, 16, -1), euclidean(4, 16, 15));
        assert_eq!(euclidean(4, 16, 16), euclidean(4, 16, 0));
        // One grid, shared with the IR compiler: the first slot always fires,
        // so E(3,16) is 0, 5, 10 and E(4,16) is four-on-the-floor. The
        // duplicate implementation this crate used to carry started its onsets
        // at slot 5 instead, which put explicit steps built here on a different
        // grid from patterns expanded there.
        let onsets = |k: u32| -> Vec<usize> {
            euclidean(k, 16, 0)
                .iter()
                .enumerate()
                .filter(|(_, on)| **on)
                .map(|(i, _)| i)
                .collect()
        };
        assert_eq!(onsets(3), vec![0, 6, 11]);
        assert_eq!(onsets(4), vec![0, 4, 8, 12], "four-on-the-floor");
    }

    #[test]
    fn swing_delays_odd_sixteenths_only() {
        let straight = vec![0i64, 240, 480, 600, 3600];
        let mut p = straight.clone();
        apply_swing(&mut p, 0.0, 960);
        assert_eq!(p, straight, "swing 0 is straight");
        let mut p = straight.clone();
        apply_swing(&mut p, 0.25, 960);
        // Slots 1 and 3 (240, 3600) delay by 60; 480 and 600 sit on/off even
        // slots and stay put.
        assert_eq!(p, vec![0, 300, 480, 600, 3660]);
        // Extreme swing clamps at half a 16th.
        let mut p = vec![240i64];
        apply_swing(&mut p, 0.9, 960);
        assert_eq!(p, vec![360]);
    }

    #[test]
    fn humanize_is_deterministic_and_in_bounds() {
        let mut a = Rng::from_seed(99);
        let mut b = Rng::from_seed(99);
        for _ in 0..1000 {
            let (ta, va) = humanize(480, 0.8, &mut a, 30, 0.05);
            let (tb, vb) = humanize(480, 0.8, &mut b, 30, 0.05);
            assert_eq!((ta, va), (tb, vb));
            assert!((ta - 480).abs() <= 120, "microtiming within IR bounds");
            assert!((0.0..=1.0).contains(&va));
        }
        // Oversized spreads clamp to the schema ceiling.
        let mut r = Rng::from_seed(1);
        for _ in 0..1000 {
            let (t, v) = humanize(10_000, 0.9, &mut r, 5_000, 5.0);
            assert!((t - 10_000).abs() <= 120);
            assert!((0.0..=1.0).contains(&v));
            assert!(t >= 0);
        }
    }

    #[test]
    fn ratchet_expands_within_bar() {
        let steps = ratchet_steps(3600, 0.8, 0);
        assert_eq!(steps.len(), 1, "clamped to at least one sub-hit");
        let steps = ratchet_steps(3600, 0.8, 99);
        assert_eq!(steps.len(), 8, "clamped to the ratchet ceiling");
        assert_eq!(steps[0].position, 3600);
        assert_eq!(steps[7].position, 3600 + 7 * 14);
        assert!(steps[1].velocity < steps[0].velocity, "falloff");
        assert!(steps.iter().all(|s| s.position < kontinuum_clock::TICKS_PER_BAR as u32));
        assert!(steps.iter().all(|s| s.ratchet == 1), "explicit steps, no re-expansion");
    }

    #[test]
    fn mask_combinators_overlay_grids() {
        let e3 = euclidean(3, 8, 0);
        let four_floor = [true, false, false, false].repeat(2);
        assert_eq!(e3.len(), 8);
        // E(3,8) anchors slot 0 but never touches slot 4, so exactly one
        // onset is shared with the four-on-the-floor grid.
        let shared = e3
            .iter()
            .zip(&four_floor)
            .filter(|(a, b)| **a && **b)
            .count();
        assert_eq!(shared, 1);
        let union = combine_masks(MaskOp::Union, &e3, &four_floor);
        let inter = combine_masks(MaskOp::Intersect, &e3, &four_floor);
        let xor = combine_masks(MaskOp::Xor, &e3, &four_floor);
        assert_eq!(
            union.iter().filter(|x| **x).count(),
            e3.iter().filter(|x| **x).count() + 2 - shared
        );
        assert_eq!(
            xor.iter().filter(|x| **x).count(),
            e3.iter().filter(|x| **x).count() + 2 - 2 * shared
        );
        assert_eq!(inter.iter().filter(|x| **x).count(), shared);
        // XOR with an empty mask is the identity; union is commutative.
        assert_eq!(combine_masks(MaskOp::Xor, &e3, &[]), e3);
        assert_eq!(
            combine_masks(MaskOp::Union, &e3, &four_floor),
            combine_masks(MaskOp::Union, &four_floor, &e3)
        );
    }

    #[test]
    fn velocity_contours_have_distinct_shapes() {
        let downbeat = VelContour::DownbeatHeavy.multiplier(0);
        assert!(downbeat > VelContour::DownbeatHeavy.multiplier(2), "downbeat anchors");
        assert!(
            VelContour::OffAccent.multiplier(2) > VelContour::OffAccent.multiplier(0),
            "offbeat is the accent"
        );
        assert_eq!(VelContour::FlatMachine.multiplier(7), 1.0, "machine is flat");
        assert!(
            VelContour::Humanized.multiplier(12) > VelContour::Humanized.multiplier(0),
            "humanized rises"
        );
    }

    #[test]
    fn gaussian_jitter_is_deterministic_bounded_and_density_scaled() {
        let mut a = Rng::from_seed(5);
        let mut b = Rng::from_seed(5);
        for _ in 0..1000 {
            let (ta, va) = humanize_gauss(480, 0.7, &mut a, jitter_sigma(0.9), 0.05);
            let (tb, vb) = humanize_gauss(480, 0.7, &mut b, jitter_sigma(0.9), 0.05);
            assert_eq!((ta, va), (tb, vb));
            assert!((ta - 480).abs() <= 120, "within IR microtiming bounds");
            assert!((0.0..=1.0).contains(&va));
        }
        assert!((jitter_sigma(0.0) - 1.0).abs() < 1e-5);
        assert!((jitter_sigma(1.0) - 4.0).abs() < 1e-5);
        // Central-limit sanity: the sampled σ lands in the 1..4 tick window's
        // neighbourhood, never flat-line (a uniform in disguise would drift).
        let mut r = Rng::from_seed(6);
        let draws: Vec<f32> = (0..2000).map(|_| gauss(&mut r)).collect();
        let mean = draws.iter().sum::<f32>() / draws.len() as f32;
        let var = draws.iter().map(|d| (d - mean).powi(2)).sum::<f32>() / draws.len() as f32;
        assert!((var.sqrt() - 1.0).abs() < 0.15, "standard normal σ=1, got {}", var.sqrt());
    }
}
