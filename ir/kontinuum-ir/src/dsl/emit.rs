//! Emission — program and section level: statement AST → [`IrDiff`] values.
//! Every statement maps 1:1 to an IR field or diff op — the DSL exposes
//! nothing the IR does not have (issue #39's rule). Semantic errors
//! (ranges, arity, scoping, required fields) are collected per line and
//! reported together.
//!
//! `section <id> { bars N, energy E }` → `AddSection { after: None, … }`;
//! a later `energy` line in the same block → `SetSectionEnergy`. Pattern
//! and param statements emit as documented in [`super::emit_pattern`].

use crate::diff::IrDiff;
use crate::schema::Section;

use super::emit_pattern::{emit_euclid, push_param, step, steps_from_mask, EuclidArgs};
use super::error::{dsl_err, DslCode, DslError};
use super::grid::{is_unit, SLOTS_PER_BAR};
use super::ast::{Inner, Stmt};
use crate::schema::{Pattern, StepsPattern};

/// Compiles the parsed program into diff ops.
pub fn emit(stmts: &[Stmt], mut errors: Vec<DslError>) -> Result<Vec<IrDiff>, Vec<DslError>> {
    let mut diffs = Vec::new();
    for stmt in stmts {
        match stmt {
            Stmt::Param { track, param, value, line } => {
                push_param(&mut diffs, &mut errors, track, param, *value, *line);
            }
            Stmt::Section { id, line, body } => {
                emit_section(id, *line, body, &mut diffs, &mut errors);
            }
        }
    }
    if errors.is_empty() {
        Ok(diffs)
    } else {
        Err(errors)
    }
}

fn emit_section(
    id: &str,
    line: usize,
    body: &[Inner],
    diffs: &mut Vec<IrDiff>,
    errors: &mut Vec<DslError>,
) {
    // Header pass: `bars` and the first `energy` line make the AddSection.
    let mut bars: Option<i64> = None;
    for inner in body {
        if let Inner::Bars { bars: b, line } = inner {
            if bars.replace(*b).is_some() {
                errors.push(dsl_err(
                    DslCode::E_DSL_DUP_FIELD,
                    *line,
                    format!("/sections/{id}/bars"),
                    "`bars` appears twice in the section",
                    "keep a single `bars` field",
                ));
            }
        }
    }
    let (bars, energy) = match (bars, first_energy(body)) {
        (Some(b), Some(e)) => (b, e),
        (None, _) => {
            errors.push(dsl_err(
                DslCode::E_DSL_BARS_REQUIRED,
                line,
                format!("/sections/{id}/bars"),
                format!("section `{id}` has no `bars` field"),
                format!("add `bars N`, e.g. `section {id} {{ bars 4, energy 0.5 }}`"),
            ));
            return;
        }
        (_, None) => {
            errors.push(dsl_err(
                DslCode::E_DSL_ENERGY_REQUIRED,
                line,
                format!("/sections/{id}/energy_curve"),
                format!("section `{id}` has no `energy` field"),
                "add `energy E` in 0..=1, e.g. `energy 0.5`",
            ));
            return;
        }
    };
    let mut header_ok = true;
    if !(1..=u32::MAX as i64).contains(&bars) {
        errors.push(dsl_err(
            DslCode::E_DSL_BARS_RANGE,
            line,
            format!("/sections/{id}/bars"),
            format!("`bars {bars}` is outside 1..={}", u32::MAX),
            "write at least 1 bar",
        ));
        header_ok = false;
    }
    if !is_unit(energy) {
        errors.push(dsl_err(
            DslCode::E_DSL_ENERGY_RANGE,
            line,
            format!("/sections/{id}/energy_curve"),
            format!("`energy {energy}` is outside 0..=1"),
            "pick an energy in 0..=1",
        ));
        header_ok = false;
    }
    if !header_ok {
        return;
    }
    diffs.push(IrDiff::AddSection {
        after: None,
        section: Section {
            id: id.to_string(),
            bars: bars as u32,
            energy_curve: vec![energy],
            density_curve: Vec::new(),
            brightness_curve: Vec::new(),
            transition_in: None,
            transition_out: None,
            pattern_bindings: Default::default(),
            automation: Default::default(),
        },
    });

    // Body pass: patterns and track-level params in source order; further
    // `energy` lines become SetSectionEnergy ops.
    let mut header_energy_seen = false;
    for inner in body {
        match inner {
            Inner::Bars { .. } => {}
            Inner::Energy { value, .. } => {
                if header_energy_seen {
                    diffs.push(IrDiff::SetSectionEnergy { id: id.to_string(), energy: vec![*value] });
                } else {
                    header_energy_seen = true;
                }
            }
            Inner::Mask { track, bits, line } => match steps_from_mask(bits) {
                Ok(steps) => diffs.push(super::emit_pattern::replace_pattern(
                    id,
                    track,
                    Pattern::Steps(StepsPattern { steps, repeats: 1 }),
                )),
                Err(()) => errors.push(dsl_err(
                    DslCode::E_DSL_MASK_RANGE,
                    *line,
                    super::emit_pattern::pattern_path(id, track),
                    format!("mask spans {} slots; one bar holds {SLOTS_PER_BAR}", bits.len()),
                    format!("keep the mask at 1..={SLOTS_PER_BAR} binary digits"),
                )),
            },
            Inner::Vel { track, values, line } => {
                if values.is_empty() || values.len() > SLOTS_PER_BAR {
                    errors.push(dsl_err(
                        DslCode::E_DSL_VEL_RANGE,
                        *line,
                        super::emit_pattern::pattern_path(id, track),
                        format!(
                            "velocity list holds {} entries; v0 supports 1..={SLOTS_PER_BAR}",
                            values.len()
                        ),
                        format!("write one velocity per 16th, up to {SLOTS_PER_BAR}"),
                    ));
                } else if values.iter().any(|v| !is_unit(*v)) {
                    errors.push(dsl_err(
                        DslCode::E_DSL_VEL_RANGE,
                        *line,
                        super::emit_pattern::pattern_path(id, track),
                        "velocities must lie in 0..=1",
                        "clamp each entry into 0..=1 (0.0 = rest)",
                    ));
                } else {
                    let steps = values
                        .iter()
                        .enumerate()
                        .filter(|(_, v)| **v > 0.0)
                        .map(|(i, v)| step(i as u32, *v))
                        .collect();
                    diffs.push(super::emit_pattern::replace_pattern(
                        id,
                        track,
                        Pattern::Steps(StepsPattern { steps, repeats: 1 }),
                    ));
                }
            }
            Inner::Euclid { track, k, n, rot, swing, line } => {
                emit_euclid(id, &EuclidArgs { track: track.as_str(), k: *k, n: *n, rot: *rot, swing: *swing, line: *line }, diffs, errors);
            }
            Inner::Param { track, param, value, line } => {
                push_param(diffs, errors, track, param, *value, *line);
            }
        }
    }
}

fn first_energy(body: &[Inner]) -> Option<f32> {
    body.iter().find_map(|i| match i {
        Inner::Energy { value, .. } => Some(*value),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extra_energy_lines_become_set_ops_in_order() {
        let src = "section a { bars 2, energy 0.3, energy 0.9; }";
        let (stmts, errs) = super::super::parse::parse(src).expect("parses");
        assert!(errs.is_empty());
        let diffs = emit(&stmts, errs).expect("emits");
        assert_eq!(diffs.len(), 2);
        assert!(matches!(diffs[0], IrDiff::AddSection { .. }));
        assert!(matches!(diffs[1], IrDiff::SetSectionEnergy { .. }));
    }
}
