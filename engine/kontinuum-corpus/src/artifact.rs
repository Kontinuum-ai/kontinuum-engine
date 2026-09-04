//! Artifact emission (issue #23): the versioned JSON files the planner
//! (#16) and groove layer (#17) load.
//!
//! DEVIATION from the issue text, documented: the issue names
//! `arrangement-params-{subgenre}.toml` and `groove-templates-{subgenre}.bin`.
//! This crate ships both as JSON — the workspace forbids new dependencies
//! and a `toml` (de)serializer would be one; JSON is serde-native for the
//! consumers and review-diffable. `artifact_version` replaces the binary
//! format's magic number.
//!
//! STATUS: every emitted value is a PLACEHOLDER until the real corpus
//! (issue #23, 100–300 purchased tracks) is analyzed and fitted — the
//! synthetic fixture proves the pipeline shape only. `corpus_size` lets
//! consumers gate on evidence mass.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::arcs::ArcCluster;
use crate::fit::{LengthParams, SubgenreFit};
use crate::groove_fit::GrooveTemplate;
use crate::CorpusError;

/// Current artifact schema version; loading a different version is an
/// error (mirrors `kontinuum-mastering`'s targets gate — silent drift in
/// generation-driving data is dangerous).
pub const ARTIFACT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArrangementParamsArtifact {
    pub artifact_version: u32,
    /// TrackObservations fitted (evidence mass; 0 = hand-seeded).
    pub corpus_size: u32,
    pub subgenre: String,
    pub section_lengths: BTreeMap<String, LengthParams>,
    pub transition_matrix: BTreeMap<String, BTreeMap<String, f32>>,
    pub transition_type_tables: BTreeMap<String, BTreeMap<String, f32>>,
    pub energy_arcs: Vec<ArcCluster>,
    /// Section-grammar block (#16); `None` = pre-extension artifact, the
    /// consumer's hand-seeded base applies. Additive: version stays 1.
    #[serde(default)]
    pub grammar: Option<GrammarBlock>,
}

/// The versioned grammar payload inside an arrangement-params artifact
/// (#16): everything the weighted state machine walks by. All weights are
/// unnormalized (renormalized at load); lengths are bars on the 4-bar grid.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrammarBlock {
    /// Grammar schema version; must match [`GRAMMAR_VERSION`].
    pub grammar_version: u32,
    /// From-kind -> {to-kind -> weight} over the eight grammar kinds.
    pub transitions: BTreeMap<String, BTreeMap<String, f32>>,
    /// Kind -> length distribution (p10/p50/p90 bars, 4-bar grid).
    pub lengths: BTreeMap<String, LengthWindow>,
    /// Kind -> coupled-curve windows: [energy, density, brightness]
    /// (start, end) each in 0..=1.
    pub curves: BTreeMap<String, CurveWindows>,
    /// (from->to) -> recipe weights + bar ranges; selection also gates on
    /// the adjacent-section energy delta.
    pub transition_recipes: BTreeMap<String, BTreeMap<String, RecipeSpec>>,
    /// Arc families the Director picks from (shape + breakdown policy).
    pub arc_families: BTreeMap<String, ArcFamilySpec>,
    /// Hard structural constraints.
    pub constraints: GrammarConstraints,
}

pub const GRAMMAR_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LengthWindow {
    pub p10: u32,
    pub p50: u32,
    pub p90: u32,
}

/// (start, end) windows for the three coupled curves of one kind (#16
/// energy model). Curve values interpolate start->end across the section.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurveWindows {
    pub energy: (f32, f32),
    pub density: (f32, f32),
    pub brightness: (f32, f32),
}

/// One transition recipe's selection entry: a weight in the (from,to) table
/// plus the bar range it emits, and an optional minimum adjacent-energy
/// delta required before the recipe may be drawn (the drama gate).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipeSpec {
    pub weight: f32,
    pub bars: (u32, u32),
    #[serde(default)]
    pub min_delta: f32,
}

/// Arc family shape (#16): the energy targets the family imposes on the
/// middle block (resampled to the block's section count), whether a
/// breakdown may land before the constraint bar, and the draw weight used
/// when the Director names no family.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArcFamilySpec {
    /// Peak-normalized energy targets, resampled over the middle block.
    pub arc: Vec<f32>,
    pub allows_early_breakdown: bool,
    pub weight: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrammarConstraints {
    /// No breakdown may start before this bar unless the arc family says
    /// otherwise (issue: bar 64 in a 10-minute piece).
    pub min_breakdown_bar: u32,
    /// Per-phrase grid for the micro-variation schedule.
    pub phrase_bars: u32,
    /// Bound on |Δenergy| between adjacent sections; `breakdown` and
    /// `release` boundaries are exempt (the drama points).
    pub max_adjacent_energy_delta: f32,
    /// Silence-drop ceiling in bars.
    pub max_silence_bars: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrooveTemplatesArtifact {
    pub artifact_version: u32,
    pub corpus_size: u32,
    pub subgenre: String,
    pub templates: Vec<GrooveTemplate>,
}

/// Emits the arrangement-params artifact for a fit.
pub fn emit(fit: &SubgenreFit) -> ArrangementParamsArtifact {
    ArrangementParamsArtifact {
        artifact_version: ARTIFACT_VERSION,
        corpus_size: fit.corpus_size,
        subgenre: fit.subgenre.clone(),
        section_lengths: fit.section_lengths.clone(),
        transition_matrix: fit.transition_matrix.clone(),
        transition_type_tables: fit.transition_types.clone(),
        energy_arcs: fit.energy_arcs.clone(),
        grammar: None,
    }
}

/// Emits the groove-templates artifact for a fit.
pub fn emit_groove(fit: &SubgenreFit) -> GrooveTemplatesArtifact {
    GrooveTemplatesArtifact {
        artifact_version: ARTIFACT_VERSION,
        corpus_size: fit.corpus_size,
        subgenre: fit.subgenre.clone(),
        templates: fit.groove_templates.clone(),
    }
}

pub fn arrangement_file_name(subgenre: &str) -> String {
    format!("arrangement-params-{subgenre}.json")
}

pub fn groove_file_name(subgenre: &str) -> String {
    format!("groove-templates-{subgenre}.json")
}

/// Writes both artifacts (pretty JSON; BTreeMap keys serialize in sorted
/// order, so bytes are deterministic) into `dir`. Returns
/// (arrangement path, groove path).
pub fn write_artifacts(fit: &SubgenreFit, dir: &Path) -> std::io::Result<(PathBuf, PathBuf)> {
    let arrangement = dir.join(arrangement_file_name(&fit.subgenre));
    let groove = dir.join(groove_file_name(&fit.subgenre));
    let text = serde_json::to_string_pretty(&emit(fit))
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&arrangement, text)?;
    let text = serde_json::to_string_pretty(&emit_groove(fit))
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&groove, text)?;
    Ok((arrangement, groove))
}

/// Loads and version-gates an arrangement-params artifact from JSON text.
pub fn load_arrangement(text: &str) -> Result<ArrangementParamsArtifact, CorpusError> {
    let a: ArrangementParamsArtifact = serde_json::from_str(text)?;
    check_version(a.artifact_version)?;
    Ok(a)
}

/// Loads and version-gates a groove-templates artifact from JSON text.
pub fn load_groove(text: &str) -> Result<GrooveTemplatesArtifact, CorpusError> {
    let g: GrooveTemplatesArtifact = serde_json::from_str(text)?;
    check_version(g.artifact_version)?;
    Ok(g)
}

fn check_version(found: u32) -> Result<(), CorpusError> {
    if found == ARTIFACT_VERSION {
        Ok(())
    } else {
        Err(CorpusError::Version { found, want: ARTIFACT_VERSION })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{GrooveObservation, SectionObservation, TrackObservation};

    fn fit() -> SubgenreFit {
        let t = TrackObservation {
            track_id: "t".into(),
            subgenre: "minimal-techno".into(),
            bpm: 126.0,
            key: "F minor".into(),
            sections: vec![SectionObservation {
                kind: "intro".into(),
                start_bar: 0,
                bars: 8,
                mean_energy: 0.4,
                mean_density: 0.4,
                mean_brightness: 0.4,
            }],
            transitions: vec![],
            groove: Some(GrooveObservation {
                swing: 0.0,
                velocity_profile: [0.5; 16],
                microtiming_profile: [0.0; 16],
            }),
        };
        crate::fit_subgenre(&[t]).unwrap()
    }

    #[test]
    fn emit_roundtrips_and_versions() {
        let f = fit();
        let a = emit(&f);
        let text = serde_json::to_string(&a).unwrap();
        assert_eq!(load_arrangement(&text).unwrap(), a);
        assert_eq!(a.artifact_version, ARTIFACT_VERSION);
        assert_eq!(a.corpus_size, 1);
        let g = emit_groove(&f);
        assert_eq!(load_groove(&serde_json::to_string(&g).unwrap()).unwrap(), g);
    }

    #[test]
    fn wrong_version_is_rejected() {
        let f = fit();
        let mut a = emit(&f);
        a.artifact_version = 99;
        let err = load_arrangement(&serde_json::to_string(&a).unwrap()).unwrap_err();
        assert!(matches!(err, CorpusError::Version { found: 99, want: 1 }));
    }

    #[test]
    fn file_names_match_the_issue_contract() {
        assert_eq!(arrangement_file_name("minimal-techno"), "arrangement-params-minimal-techno.json");
        assert_eq!(groove_file_name("microhouse"), "groove-templates-microhouse.json");
    }
}
