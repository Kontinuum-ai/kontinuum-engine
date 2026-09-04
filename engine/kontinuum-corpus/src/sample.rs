//! Grammar sampling (issue #23 validation): draw arrangements from a fitted
//! [`SubgenreFit`] so sampled structure statistics can be compared against
//! corpus statistics. Deterministic: a SplitMix64 stream from `seed`.
//!
//! Documented choices:
//! - Successor kinds are drawn from the Laplace-smoothed transition matrix.
//! - Section lengths are Normal(mean, std) via Box–Muller, rounded to whole
//!   bars and clamped to 1..=64; degenerate (std ≈ 0) distributions
//!   collapse to the mean.
//! - A walk starts at "intro" (or the first kind in sorted order when the
//!   corpus vocabulary lacks it) and ends when "outro" is drawn or after
//!   [`MAX_SECTIONS`] sections — the clamp exists so a pathological matrix
//!   cannot loop forever; real fitted matrices decay into outro.
//! - Boundary pairs the classifier never observed (reachable through the
//!   Laplace floor) get the transition type "unknown".

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::fit::SubgenreFit;
use crate::stats::{self, SplitMix64};

pub const MAX_SECTIONS: usize = 32;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SampledSection {
    pub kind: String,
    pub bars: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SampledArrangement {
    pub sections: Vec<SampledSection>,
    /// Sampled transition type per boundary (`sections.len() - 1` entries).
    pub transition_types: Vec<String>,
}

pub fn sample_arrangement(fit: &SubgenreFit, seed: u64) -> SampledArrangement {
    let mut rng = SplitMix64::new(seed);
    let mut sections: Vec<SampledSection> = Vec::new();
    let mut transition_types: Vec<String> = Vec::new();
    if fit.transition_matrix.is_empty() {
        return SampledArrangement { sections, transition_types };
    }
    let mut current = if fit.transition_matrix.contains_key("intro") {
        "intro".to_string()
    } else {
        fit.transition_matrix.keys().next().cloned().unwrap_or_default()
    };
    loop {
        sections.push(SampledSection {
            kind: current.clone(),
            bars: draw_bars(fit, &current, &mut rng),
        });
        if current == "outro" || sections.len() >= MAX_SECTIONS {
            break;
        }
        let Some(row) = fit.transition_matrix.get(&current) else { break };
        let next = draw_key(row, &mut rng);
        transition_types.push(draw_transition_type(fit, (&current, &next), &mut rng));
        current = next;
    }
    SampledArrangement { sections, transition_types }
}

fn draw_bars(fit: &SubgenreFit, kind: &str, rng: &mut SplitMix64) -> u32 {
    // Every matrix kind is an observed section kind, so the lookup hits in
    // practice; the constant keeps the sampler total for synthetic fits.
    match fit.section_lengths.get(kind) {
        Some(p) => stats::normal(rng, p.mean_bars, p.std_bars).round().clamp(1.0, 64.0) as u32,
        None => 8,
    }
}

/// Inverse-CDF draw over the row's (sorted) entries; the last entry absorbs
/// float-rounding residue.
fn draw_key(row: &BTreeMap<String, f32>, rng: &mut SplitMix64) -> String {
    let r = rng.next_f32();
    let mut cum = 0.0f32;
    for (kind, p) in row {
        cum += p;
        if r < cum {
            return kind.clone();
        }
    }
    row.keys().next_back().cloned().unwrap_or_default()
}

fn draw_transition_type(
    fit: &SubgenreFit,
    pair: (&str, &str),
    rng: &mut SplitMix64,
) -> String {
    let key = format!("{}->{}", pair.0, pair.1);
    match fit.transition_types.get(&key) {
        Some(row) if !row.is_empty() => draw_key(row, rng),
        _ => "unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fit::fit_subgenre;
    use crate::schema::{SectionObservation, TrackObservation};

    fn track(id: &str) -> TrackObservation {
        let mut bar = 0;
        let sections: Vec<SectionObservation> = ["intro", "outro"]
            .iter()
            .map(|k| {
                let s = SectionObservation {
                    kind: k.to_string(),
                    start_bar: bar,
                    bars: 8,
                    mean_energy: 0.5,
                    mean_density: 0.5,
                    mean_brightness: 0.5,
                };
                bar += 8;
                s
            })
            .collect();
        TrackObservation {
            track_id: id.into(),
            subgenre: "s".into(),
            bpm: 124.0,
            key: "F minor".into(),
            sections,
            transitions: vec![],
            groove: None,
        }
    }

    #[test]
    fn two_state_grammar_always_starts_intro_ends_outro() {
        let fit = fit_subgenre(&[track("a"), track("b")]).unwrap();
        for seed in 0..25u64 {
            let s = sample_arrangement(&fit, 100 + seed);
            assert_eq!(s.sections[0].kind, "intro");
            assert_eq!(s.sections.last().map(|x| x.kind.as_str()), Some("outro"));
            assert!((2..=MAX_SECTIONS).contains(&s.sections.len()));
            assert_eq!(s.transition_types.len(), s.sections.len() - 1);
            // The fixture records no boundary classifications, so every
            // edge type falls back to "unknown".
            assert!(s.transition_types.iter().all(|t| t == "unknown"));
        }
    }

    #[test]
    fn sampling_is_deterministic_per_seed() {
        let fit = fit_subgenre(&[track("a")]).unwrap();
        assert_eq!(sample_arrangement(&fit, 7), sample_arrangement(&fit, 7));
        let empty = SubgenreFit {
            subgenre: "s".into(),
            corpus_size: 0,
            section_lengths: BTreeMap::new(),
            transition_matrix: BTreeMap::new(),
            transition_types: BTreeMap::new(),
            energy_arcs: vec![],
            groove_templates: vec![],
        };
        assert_eq!(
            sample_arrangement(&empty, 0),
            SampledArrangement { sections: vec![], transition_types: vec![] },
            "empty matrix short-circuits"
        );
    }
}
