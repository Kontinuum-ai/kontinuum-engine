//! Per-slot expression (issue #19): velocity layers and curves, round-robin
//! takes, probability alternates, per-step microtiming and seeded
//! humanization. Selection is a pure function of `(seed, step, spec,
//! written velocity)` — the same inputs always pick the same take, layer,
//! timing and drift, so renders and live playback agree bit for bit.

use serde::{Deserialize, Serialize};

use kontinuum_clock::stream;

use crate::schema::{bounds, check, RecipeError};

/// RNG purpose selector for expression draws.
const PURPOSE_EXPR: u16 = 0x54;
/// RNG purpose selector for the take/layer nuance (independent stream so a
/// round-robin re-ordering cannot shift the humanize draws).
const PURPOSE_VARIANT: u16 = 0x55;
/// Per-layer gain trim in dB: each softer layer sits this much below the
/// previous, simulating takes recorded at decreasing hit strength.
const LAYER_STEP_DB: f32 = 1.5;
/// Take nuance windows: takes differ by a small tuning/level spread so the
/// cycling is audible even though every take re-synthesises the same voice.
const TAKE_CENTS: f32 = 6.0;
const TAKE_GAIN_DB: f32 = 0.75;

/// Velocity response curve applied to the written velocity before layer
/// selection. `Exponential` is `v²` — the standard response that puts most
/// of the dynamic range under soft playing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VelocityCurve {
    #[default]
    Linear,
    Exponential,
}

impl VelocityCurve {
    /// Map a written velocity in 0..1. Endpoints are exact on both curves.
    pub fn map(self, v: f32) -> f32 {
        match self {
            VelocityCurve::Linear => v,
            VelocityCurve::Exponential => v * v,
        }
    }
}

/// Per-slot expression spec. Every field is optional; an all-default spec
/// behaves exactly like the plain (pre-expression) renderer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HitExpression {
    #[serde(default)]
    pub curve: VelocityCurve,
    /// Velocity layers 2..=4; omitted = single layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity_layers: Option<u8>,
    /// Round-robin take count 2..=8, cycled per step; omitted = one take.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round_robin: Option<u8>,
    /// Chance 0..1 that a step plays a seeded alternate take instead of the
    /// cycling one. With no round-robin configured the alternate is take 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alternate_probability: Option<f32>,
    /// Per-step microtiming window in ms; the draw lands in ±window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub microtiming_ms: Option<f32>,
    /// Humanized level drift in ±dB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub humanize_gain_db: Option<f32>,
    /// Humanized tuning drift in ±cents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub humanize_pitch_cents: Option<f32>,
}

/// The deterministic pick for one slot trigger.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HitSelection {
    /// Final velocity after curve, humanize gain, layer trim and take
    /// nuance, clamped to the playable range.
    pub velocity: f32,
    /// Final pitch offset from the written note, in MIDI notes (cents / 100).
    pub pitch_offset: f32,
    /// Extra timing offset in ms (microtiming only; the pack's ±6 ms
    /// timing jitter applies on top).
    pub timing_ms: f32,
    /// Selected take (round-robin cycle or alternate pick).
    pub take: usize,
    /// Selected velocity layer, loudest first.
    pub layer: usize,
    /// Whether the seeded alternate fired for this step.
    pub alternate: bool,
}

/// L2 bounds for the expression spec, called from `schema::validate`.
pub(crate) fn validate_expression(expr: &HitExpression) -> Result<(), RecipeError> {
    let counts = [("velocity_layers", expr.velocity_layers, 2u8, 4u8), ("round_robin", expr.round_robin, 2, 8)];
    for (field, count, lo, hi) in counts {
        if let Some(n) = count {
            if n < lo || n > hi {
                return Err(RecipeError::OutOfBounds {
                    field,
                    value: f64::from(n),
                    lo: f64::from(lo),
                    hi: f64::from(hi),
                });
            }
        }
    }
    check("alternate_probability", expr.alternate_probability.unwrap_or(0.0), bounds::ALTERNATE_PROBABILITY)?;
    check("microtiming_ms", expr.microtiming_ms.unwrap_or(0.0), bounds::MICROTIMING_MS)?;
    check("humanize_gain_db", expr.humanize_gain_db.unwrap_or(0.0), bounds::HUMANIZE_GAIN_DB)?;
    check("humanize_pitch_cents", expr.humanize_pitch_cents.unwrap_or(0.0), bounds::HUMANIZE_PITCH_CENTS)?;
    Ok(())
}

/// Draw the deterministic selection for one slot trigger. `step` is the
/// slot/pattern-step index; `written_velocity` is the pre-curve velocity.
pub fn select_hit(
    seed: u64,
    step: usize,
    expr: &HitExpression,
    written_velocity: f32,
) -> HitSelection {
    let mut rng = stream(seed, step as u8, PURPOSE_EXPR);
    let micro = expr.microtiming_ms.unwrap_or(0.0);
    let timing_ms = if micro > 0.0 { rng.range_f32(-micro, micro) } else { 0.0 };
    let gain_window = expr.humanize_gain_db.unwrap_or(0.0);
    let mut gain_db = if gain_window > 0.0 { rng.range_f32(-gain_window, gain_window) } else { 0.0 };
    let cents_window = expr.humanize_pitch_cents.unwrap_or(0.0);
    let mut cents = if cents_window > 0.0 { rng.range_f32(-cents_window, cents_window) } else { 0.0 };

    let prob = expr.alternate_probability.unwrap_or(0.0);
    let alternate = prob > 0.0 && rng.chance(prob);

    let rr = expr.round_robin.unwrap_or(0) as usize;
    let take = if rr > 0 {
        if alternate {
            rng.below(rr as u64) as usize
        } else {
            step % rr
        }
    } else if alternate {
        1
    } else {
        0
    };

    let layers = expr.velocity_layers.unwrap_or(1) as usize;
    let mapped = expr.curve.map(written_velocity.clamp(0.0, 1.0));
    let layer = ((mapped * layers as f32) as usize).min(layers - 1);

    // Take/layer nuance from its own stream so take ordering cannot
    // perturb the humanize draws.
    let mut nuance = stream(seed, step as u8, PURPOSE_VARIANT + (take + 8 * layer) as u16);
    cents += nuance.range_f32(-TAKE_CENTS, TAKE_CENTS);
    gain_db -= nuance.range_f32(0.0, TAKE_GAIN_DB) + LAYER_STEP_DB * layer as f32;

    let gain_lin = 10.0f32.powf(gain_db / 20.0);
    HitSelection {
        velocity: (mapped * gain_lin).clamp(0.05, 1.0),
        pitch_offset: cents / 100.0,
        timing_ms,
        take,
        layer,
        alternate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expr() -> HitExpression {
        HitExpression {
            curve: VelocityCurve::Exponential,
            velocity_layers: Some(4),
            round_robin: Some(4),
            alternate_probability: Some(0.25),
            microtiming_ms: Some(12.0),
            humanize_gain_db: Some(1.5),
            humanize_pitch_cents: Some(15.0),
        }
    }

    #[test]
    fn selection_is_deterministic() {
        let a = select_hit(42, 3, &expr(), 0.7);
        let b = select_hit(42, 3, &expr(), 0.7);
        assert_eq!(a, b);
    }

    #[test]
    fn curves_hit_the_endpoints_and_stay_monotonic() {
        for curve in [VelocityCurve::Linear, VelocityCurve::Exponential] {
            assert_eq!(curve.map(0.0), 0.0);
            assert_eq!(curve.map(1.0), 1.0);
            let mut prev = -1.0;
            for i in 0..=20 {
                let v = curve.map(i as f32 / 20.0);
                assert!(v > prev, "curve must be monotonic");
                prev = v;
            }
        }
        assert!(VelocityCurve::Exponential.map(0.5) < VelocityCurve::Linear.map(0.5));
    }

    #[test]
    fn layers_select_by_curved_velocity() {
        // v=0.95 on the exponential curve (0.9025) lands in layer 3 of 4.
        assert_eq!(select_hit(1, 0, &expr(), 0.95).layer, 3);
        // v=0.2 maps to 0.04, still layer 0.
        assert_eq!(select_hit(1, 0, &expr(), 0.2).layer, 0);
        // A single-layer spec never leaves layer 0.
        let plain = HitExpression::default();
        assert_eq!(select_hit(1, 0, &plain, 1.0).layer, 0);
    }

    #[test]
    fn round_robin_cycles_takes() {
        let mut e = expr();
        e.alternate_probability = Some(0.0);
        let takes: Vec<usize> =
            (0..8).map(|s| select_hit(7, s, &e, 0.8).take).collect();
        assert_eq!(takes, vec![0, 1, 2, 3, 0, 1, 2, 3]);
    }

    #[test]
    fn alternate_probability_is_seeded_and_bounded() {
        let mut e = expr();
        e.alternate_probability = Some(1.0);
        e.round_robin = Some(4);
        for s in 0..10 {
            let sel = select_hit(9, s, &e, 0.8);
            assert!(sel.alternate);
            assert!(sel.take < 4, "alternate pick stays inside the take set");
        }
        // Without round-robin the alternate is the second take.
        e.round_robin = None;
        assert_eq!(select_hit(9, 0, &e, 0.8).take, 1);
        e.alternate_probability = Some(0.0);
        assert!(!select_hit(9, 0, &e, 0.8).alternate);
    }

    #[test]
    fn microtiming_and_drift_stay_in_window() {
        for s in 0..50 {
            let sel = select_hit(11, s, &expr(), 0.8);
            assert!(sel.timing_ms.abs() <= 12.0);
            assert!((sel.pitch_offset * 100.0).abs() <= 15.0 + TAKE_CENTS + 1e-3);
            assert!((0.05..=1.0).contains(&sel.velocity));
        }
    }

    #[test]
    fn different_seeds_take_different_paths() {
        let a: Vec<HitSelection> = (0..12).map(|s| select_hit(1, s, &expr(), 0.8)).collect();
        let b: Vec<HitSelection> = (0..12).map(|s| select_hit(2, s, &expr(), 0.8)).collect();
        assert!(a.iter().zip(b.iter()).any(|(x, y)| x != y), "seed must matter");
    }
}
