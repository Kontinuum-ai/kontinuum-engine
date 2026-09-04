//! Corpus-informed arrangement structure (issue #16 residual, #23): loads
//! the corpus crate's arrangement-params artifact and exposes the fitted
//! section lengths and energy arcs to [`crate::arrangement`]'s planner.
//!
//! The artifact's kind labels are free strings from the #5 detector; the
//! detected→grammar mapping lives here (per the corpus crate's contract).
//! Kinds that do not map onto the compose grammar are ignored, and every
//! query falls back to `None` when nothing maps — the planner keeps its
//! hand-seeded behavior in that case, so an artifact that carries no
//! usable structure is behaviorally identical to no artifact at all.

use std::collections::BTreeMap;

use kontinuum_corpus::{ArrangementParamsArtifact, LengthParams};

use crate::arrangement::Kind;

#[derive(Clone, Debug, PartialEq)]
pub struct StructureParams {
    pub subgenre: String,
    /// `TrackObservation`s fitted (0 = placeholder fit).
    pub corpus_size: u32,
    lengths: BTreeMap<Kind, Vec<LengthParams>>,
    /// Highest-weight energy-arc centroid (peak-normalized, 8 points).
    arc: Vec<f32>,
    /// The artifact's section-grammar block (#16), resolved and ready for
    /// the planner; `None` on pre-extension artifacts (the embedded base
    /// applies).
    pub grammar: Option<crate::grammar::GrammarData>,
}

impl StructureParams {
    /// Parses and version-gates an arrangement-params artifact from JSON
    /// text.
    pub fn load_json(text: &str) -> Result<StructureParams, kontinuum_corpus::CorpusError> {
        Ok(Self::from_artifact(&kontinuum_corpus::load_arrangement(text)?))
    }

    pub fn from_artifact(a: &ArrangementParamsArtifact) -> StructureParams {
        let mut lengths: BTreeMap<Kind, Vec<LengthParams>> = BTreeMap::new();
        for (label, params) in &a.section_lengths {
            if let Some(kind) = map_kind(label) {
                lengths.entry(kind).or_default().push(params.clone());
            }
        }
        let arc = a
            .energy_arcs
            .iter()
            .max_by(|x, y| x.weight.total_cmp(&y.weight))
            .map(|c| c.centroid.clone())
            .unwrap_or_default();
        let grammar = a.grammar.as_ref().and_then(crate::grammar::GrammarData::from_block);
        StructureParams { subgenre: a.subgenre.clone(), corpus_size: a.corpus_size, lengths, arc, grammar }
    }

    /// True when no artifact kind mapped onto the grammar — the planner
    /// treats this exactly like an absent artifact.
    pub fn is_empty(&self) -> bool {
        self.lengths.is_empty()
    }

    /// Median length for a grammar kind across the mapped artifact kinds,
    /// snapped to the planner's 4-bar grid.
    pub fn median_bars(&self, kind: Kind) -> Option<u32> {
        let medians: Vec<f32> =
            self.lengths.get(&kind)?.iter().map(|p| p.p50_bars).collect();
        let median = medians.iter().sum::<f32>() / medians.len() as f32;
        Some((median / 4.0).round().max(1.0) as u32 * 4)
    }

    /// The fitted arc resampled to `dev_count` dev sections: position `i`
    /// of the development block reads the arc at its relative offset.
    /// Peaks are normalized in the artifact, so values are scaled into the
    /// planner's 0.2..0.9 working band.
    pub fn dev_energy(&self, i: usize, dev_count: usize) -> Option<f32> {
        if self.arc.is_empty() || dev_count == 0 {
            return None;
        }
        let t = if dev_count <= 1 {
            0.5
        } else {
            i as f32 / (dev_count - 1) as f32
        };
        let last = self.arc.len() - 1;
        let raw = self.arc[(t * last as f32).round() as usize];
        Some((0.2 + raw * 0.7).clamp(0.05, 0.95))
    }
}

/// Detector label → grammar kind: substring match on the corpus's free
/// labels ("groove_dev", "breakdown", "re-intro", …); unrecognized labels
/// stay unmapped so a foreign artifact never silently reshapes a session.
fn map_kind(label: &str) -> Option<Kind> {
    let l = label.to_lowercase();
    if l.contains("break") {
        Some(Kind::Breakdown)
    } else if l.contains("intro") && (l.contains("re") || l.contains("back")) {
        Some(Kind::Reintro)
    } else if l.contains("intro") {
        Some(Kind::Intro)
    } else if l.contains("outro") {
        Some(Kind::Outro)
    } else if l.contains("tension") || l.contains("build") {
        Some(Kind::Tension)
    } else if l.contains("drop") || l.contains("release") || l.contains("peak") {
        Some(Kind::Release)
    } else if l.contains("variat") {
        Some(Kind::Variation)
    } else if l.contains("dev") || l.contains("groove") || l.contains("main") {
        Some(Kind::Dev)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(kinds: &[&str]) -> ArrangementParamsArtifact {
        let mut lengths = BTreeMap::new();
        for (i, k) in kinds.iter().enumerate() {
            lengths.insert(
                k.to_string(),
                LengthParams {
                    mean_bars: 16.0 + i as f32 * 8.0,
                    std_bars: 4.0,
                    p10_bars: 8.0,
                    p50_bars: 16.0 + i as f32 * 8.0,
                    p90_bars: 32.0,
                },
            );
        }
        ArrangementParamsArtifact {
            artifact_version: kontinuum_corpus::ARTIFACT_VERSION,
            corpus_size: 5,
            subgenre: "minimal-techno".into(),
            section_lengths: lengths,
            transition_matrix: BTreeMap::new(),
            transition_type_tables: BTreeMap::new(),
            energy_arcs: vec![],
            grammar: None,
        }
    }

    #[test]
    fn kinds_map_onto_the_grammar() {
        assert_eq!(map_kind("intro"), Some(Kind::Intro));
        assert_eq!(map_kind("groove_dev"), Some(Kind::Dev));
        assert_eq!(map_kind("Breakdown"), Some(Kind::Breakdown));
        assert_eq!(map_kind("reintro"), Some(Kind::Reintro));
        assert_eq!(map_kind("outro"), Some(Kind::Outro));
        // The #16 remap: builds are tension, drops/peaks are release.
        assert_eq!(map_kind("build"), Some(Kind::Tension));
        assert_eq!(map_kind("drop"), Some(Kind::Release));
        assert_eq!(map_kind("peak"), Some(Kind::Release));
        assert_eq!(map_kind("variation"), Some(Kind::Variation));
        assert_eq!(map_kind("vox_section"), None, "unknown labels stay unmapped");
    }

    #[test]
    fn medians_snap_to_the_four_bar_grid() {
        let s = StructureParams::from_artifact(&artifact(&["groove_dev", "breakdown"]));
        assert_eq!(s.median_bars(Kind::Dev), Some(16));
        assert_eq!(s.median_bars(Kind::Breakdown), Some(24));
        assert_eq!(s.median_bars(Kind::Intro), None, "unmapped kinds fall back");
        assert!(!s.is_empty());
    }

    #[test]
    fn artifact_without_mappable_kinds_is_empty() {
        let s = StructureParams::from_artifact(&artifact(&["vox_section"]));
        assert!(s.is_empty());
        assert_eq!(s.median_bars(Kind::Dev), None);
        assert_eq!(s.dev_energy(0, 2), None);
    }
}
