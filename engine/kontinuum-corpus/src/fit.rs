//! Distribution fitting (issue #23): per-subgenre aggregation of the
//! observation records into a [`SubgenreFit`].
//!
//! Documented statistical choices:
//! - Transition matrix: consecutive section pairs per track
//!   (`start_bar`-sorted), Laplace-smoothed with α = [`LAPLACE_ALPHA`] over
//!   the subgenre's observed kind vocabulary, so no cell is ever 0 and the
//!   planner can sample every successor. Rows sum to 1.
//! - Section lengths per kind: mean, population std, p10/p50/p90 quantiles
//!   (linear interpolation, see [`crate::stats`]).
//! - Transition-type tables keyed `"from->to"`, Laplace α again over the
//!   subgenre's observed transition-type vocabulary.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::arcs::{self, ArcCluster};
use crate::groove_fit::{self, GrooveTemplate};
use crate::schema::{SectionObservation, TrackObservation};
use crate::{stats, CorpusError};

/// Laplace smoothing constant for every count-based distribution.
pub const LAPLACE_ALPHA: f32 = 1.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LengthParams {
    pub mean_bars: f32,
    pub std_bars: f32,
    pub p10_bars: f32,
    pub p50_bars: f32,
    pub p90_bars: f32,
}

/// Everything the arrangement planner (#16) and groove layer (#17)
/// consume, fitted from one subgenre's observations.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SubgenreFit {
    pub subgenre: String,
    /// Number of TrackObservations the fit is based on.
    pub corpus_size: u32,
    pub section_lengths: BTreeMap<String, LengthParams>,
    /// Rows = from-kind, columns = to-kind; each row sums to 1 (Laplace α).
    pub transition_matrix: BTreeMap<String, BTreeMap<String, f32>>,
    /// Key "from->to" → transition kind → probability (Laplace α).
    pub transition_types: BTreeMap<String, BTreeMap<String, f32>>,
    pub energy_arcs: Vec<ArcCluster>,
    pub groove_templates: Vec<GrooveTemplate>,
}

/// Fits one subgenre. `observations` must be non-empty and carry a single
/// `subgenre` value (callers group by subgenre first). Records are sorted
/// by `track_id` before fitting so input order never affects the result.
pub fn fit_subgenre(observations: &[TrackObservation]) -> Result<SubgenreFit, CorpusError> {
    let subgenre =
        observations.first().ok_or(CorpusError::EmptyCorpus)?.subgenre.clone();
    if observations.iter().any(|o| o.subgenre != subgenre) {
        return Err(CorpusError::MixedSubgenres(subgenre));
    }
    let mut tracks: Vec<TrackObservation> = observations.to_vec();
    tracks.sort_by(|a, b| a.track_id.cmp(&b.track_id));

    let mut kind_set: BTreeSet<String> = BTreeSet::new();
    for t in &tracks {
        for s in &t.sections {
            kind_set.insert(s.kind.clone());
        }
    }
    let kinds: Vec<String> = kind_set.into_iter().collect();

    Ok(SubgenreFit {
        subgenre,
        corpus_size: tracks.len() as u32,
        section_lengths: section_lengths(&tracks),
        transition_matrix: transition_matrix(&tracks, &kinds),
        transition_types: transition_types(&tracks),
        energy_arcs: arcs::fit_arcs(&tracks),
        groove_templates: groove_fit::fit_grooves(&tracks),
    })
}

fn sorted_sections(t: &TrackObservation) -> Vec<&SectionObservation> {
    let mut secs: Vec<&SectionObservation> = t.sections.iter().collect();
    secs.sort_by_key(|s| s.start_bar);
    secs
}

/// Consecutive (from, to) kind pairs across each track's section sequence.
fn consecutive_pairs(tracks: &[TrackObservation]) -> Vec<(&str, &str)> {
    let mut pairs = Vec::new();
    for t in tracks {
        for w in sorted_sections(t).windows(2) {
            pairs.push((w[0].kind.as_str(), w[1].kind.as_str()));
        }
    }
    pairs
}

fn transition_matrix(
    tracks: &[TrackObservation],
    kinds: &[String],
) -> BTreeMap<String, BTreeMap<String, f32>> {
    let mut counts: BTreeMap<&str, BTreeMap<&str, u32>> = BTreeMap::new();
    for (from, to) in consecutive_pairs(tracks) {
        *counts.entry(from).or_default().entry(to).or_insert(0) += 1;
    }
    let k = kinds.len() as f32;
    kinds
        .iter()
        .map(|from| {
            let row_total =
                counts.get(from.as_str()).map(|r| r.values().sum()).unwrap_or(0u32);
            let row = kinds
                .iter()
                .map(|to| {
                    let c = counts
                        .get(from.as_str())
                        .and_then(|r| r.get(to.as_str()))
                        .copied()
                        .unwrap_or(0);
                    let p = (c as f32 + LAPLACE_ALPHA) / (row_total as f32 + k * LAPLACE_ALPHA);
                    (to.clone(), p)
                })
                .collect();
            (from.clone(), row)
        })
        .collect()
}

fn section_lengths(tracks: &[TrackObservation]) -> BTreeMap<String, LengthParams> {
    let mut by_kind: BTreeMap<&str, Vec<f32>> = BTreeMap::new();
    for t in tracks {
        for s in &t.sections {
            by_kind.entry(s.kind.as_str()).or_default().push(s.bars as f32);
        }
    }
    by_kind
        .into_iter()
        .map(|(kind, mut bars)| {
            bars.sort_by(f32::total_cmp);
            (kind.to_string(), LengthParams {
                mean_bars: stats::mean(&bars),
                std_bars: stats::std(&bars),
                p10_bars: stats::quantile(&bars, 0.10),
                p50_bars: stats::quantile(&bars, 0.50),
                p90_bars: stats::quantile(&bars, 0.90),
            })
        })
        .collect()
}

/// Boundary records with out-of-range section indices are skipped (the #5
/// producer guarantees in-range indices; the loader stays total regardless).
fn transition_types(tracks: &[TrackObservation]) -> BTreeMap<String, BTreeMap<String, f32>> {
    let mut vocab: BTreeSet<&str> = BTreeSet::new();
    let mut counts: BTreeMap<(&str, &str), BTreeMap<&str, u32>> = BTreeMap::new();
    for t in tracks {
        let secs = sorted_sections(t);
        for tr in &t.transitions {
            let (Some(from), Some(to)) =
                (secs.get(tr.from_section_index), secs.get(tr.to_section_index))
            else {
                continue;
            };
            vocab.insert(tr.kind.as_str());
            *counts
                .entry((from.kind.as_str(), to.kind.as_str()))
                .or_default()
                .entry(tr.kind.as_str())
                .or_insert(0) += 1;
        }
    }
    let v = vocab.len() as f32;
    counts
        .into_iter()
        .map(|((from, to), row)| {
            let total: u32 = row.values().sum();
            let probs = vocab
                .iter()
                .map(|kind| {
                    let c = row.get(kind).copied().unwrap_or(0);
                    (kind.to_string(), (c as f32 + LAPLACE_ALPHA) / (total as f32 + v * LAPLACE_ALPHA))
                })
                .collect();
            (format!("{from}->{to}"), probs)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{SectionObservation, TransitionObservation};

    fn section(kind: &str, start_bar: u32, bars: u32, energy: f32) -> SectionObservation {
        SectionObservation {
            kind: kind.into(),
            start_bar,
            bars,
            mean_energy: energy,
            mean_density: energy,
            mean_brightness: energy,
        }
    }

    fn track(id: &str, kinds: &[&str]) -> TrackObservation {
        let mut bar = 0;
        let sections: Vec<SectionObservation> = kinds
            .iter()
            .map(|k| {
                let s = section(k, bar, 8, 0.5);
                bar += 8;
                s
            })
            .collect();
        let transitions: Vec<TransitionObservation> = (0..kinds.len().saturating_sub(1))
            .map(|i| TransitionObservation {
                from_section_index: i,
                to_section_index: i + 1,
                kind: "fill".into(),
            })
            .collect();
        TrackObservation {
            track_id: id.into(),
            subgenre: "s".into(),
            bpm: 124.0,
            key: "F minor".into(),
            sections,
            transitions,
            groove: None,
        }
    }

    #[test]
    fn matrix_rows_sum_to_one_with_laplace_floor() {
        let obs = vec![track("a", &["intro", "build"]), track("b", &["intro", "drop"])];
        let m = fit_subgenre(&obs).unwrap().transition_matrix;
        let intro_row = &m["intro"];
        let total: f32 = intro_row.values().sum();
        assert!((total - 1.0).abs() < 1e-5);
        // intro→build and intro→drop observed once each; smoothed tie.
        assert!((intro_row["build"] - intro_row["drop"]).abs() < 1e-6);
        // Unobserved cells keep the Laplace floor above zero.
        assert!(m["build"]["intro"] > 0.0);
    }

    #[test]
    fn empty_or_mixed_subgenres_are_rejected() {
        assert!(matches!(
            fit_subgenre(&[]),
            Err(CorpusError::EmptyCorpus)
        ));
        let mixed = vec![track("a", &["intro"]), {
            let mut t = track("b", &["intro"]);
            t.subgenre = "other".into();
            t
        }];
        assert!(matches!(
            fit_subgenre(&mixed),
            Err(CorpusError::MixedSubgenres(_))
        ));
    }

    #[test]
    fn input_order_does_not_change_the_fit() {
        let a = vec![track("a", &["intro", "build"]), track("b", &["intro", "build"])];
        let mut b = a.clone();
        b.reverse();
        assert_eq!(fit_subgenre(&a).unwrap(), fit_subgenre(&b).unwrap());
    }
}
