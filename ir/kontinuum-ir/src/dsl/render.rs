//! Rendering: [`IrDiff`] list → canonical DSL text — the inverse of
//! [`super::compile`] over the subset the v0 grammar covers. Uncovered
//! variants are a hard [`DslCode::E_DSL_UNSUPPORTED_IR`] error, never a
//! lossy approximation.
//!
//! Canonical form (the round-trip contract): `AddSection` blocks render in
//! list order, each as a full block with `bars`/`energy` header fields, then
//! its track patterns (list order, see [`super::render_pattern`]), then its
//! `SetSectionEnergy` ops as extra `energy` lines; top-level
//! `SetInstrumentParam` lines render last. Programs that interleave patterns
//! and energy ops re-render in canonical order — `compile` is injective on
//! the canonical subset.

use crate::diff::IrDiff;
use crate::schema::Section;

use super::error::{dsl_err, DslCode, DslError};
use super::grid::is_unit;
use super::render_pattern::{fmt_f32, render_pattern};

/// Renders diffs into canonical DSL text.
pub fn render(diffs: &[IrDiff]) -> Result<String, DslError> {
    let section_ids: Vec<&str> = diffs
        .iter()
        .filter_map(|d| match d {
            IrDiff::AddSection { section, .. } => Some(section.id.as_str()),
            _ => None,
        })
        .collect();
    let mut blocks = String::new();
    let mut params = String::new();
    for diff in diffs {
        match diff {
            IrDiff::AddSection { after: None, section } => {
                render_section(section, diffs, &mut blocks)?;
            }
            IrDiff::AddSection { after: Some(_), .. } => {
                return Err(unsupported(
                    "/sections",
                    "AddSection with an `after` anchor is outside DSL v0 (v0 appends only)",
                    "render the section without an `after`, or edit the IR directly",
                ));
            }
            IrDiff::SetSectionEnergy { id, .. } => {
                if !section_ids.contains(&id.as_str()) {
                    return Err(unsupported(
                        &format!("/sections/{id}/energy_curve"),
                        "SetSectionEnergy for a section this program does not create",
                        "render the section's AddSection in the same program, or edit the IR directly",
                    ));
                }
            }
            IrDiff::SetInstrumentParam { track, param, value } => {
                params.push_str(&format!("{track}.{param} = {};\n", fmt_f32(*value)));
            }
            // Folded into their section's block by render_section; only
            // homeless patterns (no AddSection) are an error.
            IrDiff::ReplacePattern { section, track, .. } => {
                if !section_ids.contains(&section.as_str()) {
                    return Err(unsupported(
                        &format!("/sections/{section}/pattern_bindings/{track}"),
                        "ReplacePattern without a matching AddSection in this program",
                        "render the section's AddSection in the same program",
                    ));
                }
            }
            IrDiff::ExtendSection { .. } | IrDiff::SetAutomation { .. } => {
                return Err(unsupported(
                    "/sections",
                    "this op has no DSL v0 statement",
                    "edit the IR directly; see issue #39 for planned statements",
                ));
            }
            IrDiff::SwapSample { track, .. } => {
                return Err(unsupported(
                    &format!("/tracks/{track}"),
                    "SwapSample has no DSL v0 statement",
                    "edit the IR directly; see issue #39 for planned statements",
                ));
            }
            IrDiff::SwapInstrument { track, .. } => {
                return Err(unsupported(
                    &format!("/tracks/{track}"),
                    "SwapInstrument has no DSL v0 statement",
                    "edit the IR directly; see issue #39 for planned statements",
                ));
            }
            IrDiff::ScheduleTransition { .. } => {
                return Err(unsupported(
                    "/sections",
                    "ScheduleTransition has no DSL v0 statement",
                    "edit the IR directly; see issue #39 for planned statements",
                ));
            }
            // Global directives are follow-up DSL statements (#39); v0 text
            // cannot represent them losslessly.
            IrDiff::SetTempo { .. } => {
                return Err(unsupported(
                    "/tempo_lane",
                    "SetTempo has no DSL v0 statement",
                    "edit the IR directly; tempo statements are planned for a later rev",
                ));
            }
            IrDiff::SetKey { .. } => {
                return Err(unsupported(
                    "/key",
                    "SetKey has no DSL v0 statement",
                    "edit the IR directly; harmony statements are planned for a later rev",
                ));
            }
        }
    }
    blocks.push_str(&params);
    Ok(blocks)
}

pub(super) fn unsupported(path: &str, message: &str, fix: &str) -> DslError {
    dsl_err(
        DslCode::E_DSL_UNSUPPORTED_IR,
        0,
        path,
        message.to_string(),
        fix.to_string(),
    )
}

fn render_section(section: &Section, all: &[IrDiff], out: &mut String) -> Result<(), DslError> {
    if section.transition_in.is_some()
        || section.transition_out.is_some()
        || !section.automation.is_empty()
        || !section.pattern_bindings.is_empty()
    {
        return Err(unsupported(
            &format!("/sections/{}", section.id),
            "section carries transitions, automation, or bound patterns — outside DSL v0",
            "render a bare section (bars + energy only), or edit the IR directly",
        ));
    }
    if section.energy_curve.len() != 1 {
        return Err(unsupported(
            &format!("/sections/{}/energy_curve", section.id),
            &format!("v0 renders single-value energy curves, found {}", section.energy_curve.len()),
            "give the section a one-point energy curve",
        ));
    }
    let energy = section.energy_curve[0];
    if !is_unit(energy) {
        return Err(unsupported(
            &format!("/sections/{}/energy_curve", section.id),
            &format!("energy {energy} is outside 0..=1"),
            "pick an energy in 0..=1",
        ));
    }
    if section.bars == 0 {
        return Err(unsupported(
            &format!("/sections/{}/bars", section.id),
            "sections need at least one bar",
            "set bars >= 1",
        ));
    }
    out.push_str(&format!(
        "section {} {{\n  bars {},\n  energy {},\n",
        section.id,
        section.bars,
        fmt_f32(energy)
    ));
    for diff in all {
        if let IrDiff::ReplacePattern { section: sec, track, pattern } = diff {
            if sec == &section.id {
                render_pattern(out, &section.id, track, pattern)?;
            }
        }
    }
    for diff in all {
        if let IrDiff::SetSectionEnergy { id, energy } = diff {
            if id == &section.id {
                if energy.len() != 1 || !is_unit(energy[0]) {
                    return Err(unsupported(
                        &format!("/sections/{id}/energy_curve"),
                        "v0 renders single-value energy curves in 0..=1",
                        "give the op a one-point curve in 0..=1",
                    ));
                }
                out.push_str(&format!("  energy {},\n", fmt_f32(energy[0])));
            }
        }
    }
    out.push_str("}\n");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{EuclideanPattern, EuclideanTag, Pattern};

    fn bare_section(id: &str) -> Section {
        Section {
            id: id.into(),
            bars: 4,
            energy_curve: vec![0.5],
            density_curve: Vec::new(),
            brightness_curve: Vec::new(),
            transition_in: None,
            transition_out: None,
            pattern_bindings: Default::default(),
            automation: Default::default(),
        }
    }

    fn euclid(k: u32, n: u32, rot: i32) -> Pattern {
        Pattern::Euclidean(EuclideanPattern {
            generator: EuclideanTag::Euclidean,
            k,
            n,
            rot,
            velocity: 0.8,
            probability: 1.0,
            repeats: 1,
            gate: None,
            pitch: None,
        })
    }

    #[test]
    fn euclid_diffs_round_trip_through_text() {
        let diffs = vec![
            IrDiff::AddSection { after: None, section: bare_section("a") },
            IrDiff::ReplacePattern { section: "a".into(), track: "hat".into(), pattern: euclid(4, 16, 2) },
        ];
        let text = render(&diffs).expect("render");
        assert!(text.contains("hat: E(4, 16, 2);"), "text: {text}");
        assert_eq!(super::super::compile(&text).expect("compile"), diffs);
    }

    #[test]
    fn uncovered_variants_are_rejected() {
        let diffs = vec![IrDiff::SwapSample { track: "s".into(), sample_id: 3 }];
        assert_eq!(render(&diffs).expect_err("swap").code, DslCode::E_DSL_UNSUPPORTED_IR);
    }
}
