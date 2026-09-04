//! Emission — pattern and param statements:
//! - `track.mask = 0b…;` → `ReplacePattern` with `StepsPattern` (bit i →
//!   position i·240 ticks at PPQ 960; 16 slots per bar, velocity 0.8)
//! - `track.vel = […]` → `ReplacePattern` with `StepsPattern` (0.0 = rest)
//! - `track: E(k, n, rot)` → `ReplacePattern` with `EuclideanPattern`;
//!   `@ swing s` expands to an on-grid `StepsPattern` with the odd-16th
//!   delay in `microtiming_ticks` (round(s·240), clamped to the IR ±120
//!   bound — no DSL-only fields)
//! - `track.param = v;` → `SetInstrumentParam`

use crate::diff::IrDiff;
use crate::schema::{EuclideanPattern, EuclideanTag, Pattern, Step, StepsPattern};

use super::error::{dsl_err, DslCode, DslError, KNOWN_PARAMS};
use super::grid::{bucket, EUCLID_VELOCITY, MAX_SWING_TICKS, SLOTS_PER_BAR, TICKS_PER_16TH};

/// The destructured [`super::ast::Inner::Euclid`] statement payload.
pub(super) struct EuclidArgs<'a> {
    pub(super) track: &'a str,
    pub(super) k: i64,
    pub(super) n: i64,
    pub(super) rot: i64,
    pub(super) swing: Option<f32>,
    pub(super) line: usize,
}

pub(super) fn emit_euclid(
    section: &str,
    e: &EuclidArgs,
    diffs: &mut Vec<IrDiff>,
    errors: &mut Vec<DslError>,
) {
    let EuclidArgs { track, k, n, rot, swing, line } = *e;
    let path = pattern_path(section, track);
    let in_grid = (1..=SLOTS_PER_BAR as i64).contains(&n)
        && (0..=n).contains(&k)
        && (i32::MIN as i64..=i32::MAX as i64).contains(&rot);
    if !in_grid {
        errors.push(dsl_err(
            DslCode::E_DSL_EUCLID_RANGE,
            line,
            &path,
            format!("E({k}, {n}, {rot}) is out of range (need 1..={SLOTS_PER_BAR} slots, k ≤ n)"),
            format!("write `E(k, n, rot)` with 1..={SLOTS_PER_BAR} slots and k onsets"),
        ));
        return;
    }
    match swing {
        Some(s) if !s.is_finite() || !(0.0..=0.5).contains(&s) => {
            errors.push(dsl_err(
                DslCode::E_DSL_SWING_RANGE,
                line,
                &path,
                format!("swing {s} is outside 0..=0.5"),
                "pick a swing in 0..=0.5 (0.5 = full triplet lean)",
            ));
        }
        Some(s) => {
            let ticks = (s * TICKS_PER_16TH as f32).round().clamp(0.0, MAX_SWING_TICKS) as i16;
            let steps: Vec<Step> = bucket(k as u32, n as u32, rot as i32)
                .iter()
                .enumerate()
                .filter(|(_, on)| **on)
                .map(|(i, _)| step_swing(i, ticks))
                .collect();
            diffs.push(replace_pattern(
                section,
                track,
                Pattern::Steps(StepsPattern { steps, repeats: 1 }),
            ));
        }
        None => diffs.push(replace_pattern(
            section,
            track,
            Pattern::Euclidean(EuclideanPattern {
                generator: EuclideanTag::Euclidean,
                k: k as u32,
                n: n as u32,
                rot: rot as i32,
                velocity: EUCLID_VELOCITY,
                probability: 1.0,
                repeats: 1,
                gate: None,
                pitch: None,
            }),
        )),
    }
}

pub(super) fn push_param(
    diffs: &mut Vec<IrDiff>,
    errors: &mut Vec<DslError>,
    track: &str,
    param: &str,
    value: f32,
    line: usize,
) {
    if !KNOWN_PARAMS.contains(&param) {
        errors.push(dsl_err(
            DslCode::E_DSL_UNKNOWN_PARAM,
            line,
            format!("/tracks/{track}/instrument/{param}"),
            format!("`{param}` is not an IR instrument param"),
            format!("use one of: {}", KNOWN_PARAMS.join(", ")),
        ));
        return;
    }
    if !value.is_finite() {
        errors.push(dsl_err(
            DslCode::E_DSL_PARAM_RANGE,
            line,
            format!("/tracks/{track}/instrument/{param}"),
            format!("param value {value} is not finite"),
            "write a finite decimal number",
        ));
        return;
    }
    diffs.push(IrDiff::SetInstrumentParam {
        track: track.to_string(),
        param: param.to_string(),
        value,
    });
}

/// Mask bits (MSB first) → steps; bit i lights slot i.
pub(super) fn steps_from_mask(bits: &[bool]) -> Result<Vec<Step>, ()> {
    if bits.is_empty() || bits.len() > SLOTS_PER_BAR {
        return Err(());
    }
    Ok(bits
        .iter()
        .enumerate()
        .filter(|(_, on)| **on)
        .map(|(i, _)| step(i as u32, EUCLID_VELOCITY))
        .collect())
}

pub(super) fn step(slot: u32, velocity: f32) -> Step {
    Step {
        position: slot * TICKS_PER_16TH,
        velocity,
        probability: 1.0,
        microtiming_ticks: 0,
        ratchet: 1,
        pitch: None,
        gate: None,
        accent: false,
    }
}

fn step_swing(slot: usize, ticks: i16) -> Step {
    Step {
        position: slot as u32 * TICKS_PER_16TH,
        velocity: EUCLID_VELOCITY,
        probability: 1.0,
        microtiming_ticks: if slot % 2 == 1 { ticks } else { 0 },
        ratchet: 1,
        pitch: None,
        gate: None,
        accent: false,
    }
}

pub(super) fn replace_pattern(section: &str, track: &str, pattern: Pattern) -> IrDiff {
    IrDiff::ReplacePattern {
        section: section.to_string(),
        track: track.to_string(),
        pattern,
    }
}

pub(super) fn pattern_path(section: &str, track: &str) -> String {
    format!("/sections/{section}/pattern_bindings/{track}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_maps_slots_to_step_positions() {
        let steps =
            steps_from_mask(&[true, false, false, false, true, false, false, false]).expect("mask");
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].position, 0);
        assert_eq!(steps[1].position, 4 * TICKS_PER_16TH);
        assert_eq!(steps[0].velocity, EUCLID_VELOCITY);
        assert!(steps_from_mask(&[true; 17]).is_err(), "17 slots cannot fit one bar");
    }

    #[test]
    fn swing_expands_to_grid_microtiming() {
        let mut diffs = Vec::new();
        let mut errors = Vec::new();
        emit_euclid(
            "a",
            &EuclidArgs { track: "perc", k: 16, n: 16, rot: 0, swing: Some(0.125), line: 1 },
            &mut diffs,
            &mut errors,
        );
        assert!(errors.is_empty());
        match diffs.into_iter().next().expect("diff") {
            IrDiff::ReplacePattern { pattern: Pattern::Steps(p), .. } => {
                assert_eq!(p.steps.len(), 16);
                assert_eq!(p.steps[1].microtiming_ticks, 30, "0.125 * 240 ticks");
                assert_eq!(p.steps[1].position, TICKS_PER_16TH, "positions stay on grid");
                assert_eq!(p.steps[0].microtiming_ticks, 0, "even slots stay straight");
            }
            other => panic!("expected steps, got {other:?}"),
        }
    }
}
