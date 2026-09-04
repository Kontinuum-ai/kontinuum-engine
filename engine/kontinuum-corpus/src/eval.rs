//! Segmentation evaluation (issue #23): the annotation file format and
//! the boundary-F1 scorer that gate whether the #5 segmentation can be
//! trusted. The issue's bar is F1 ≥ [`SEGMENTATION_F1_GATE`] on 20
//! hand-annotated tracks before the rest of the corpus analysis is
//! believed.
//!
//! TODO(human, #23): the 20 hand annotations for the real purchased corpus.
//! The format below is what annotators produce (one JSON file per track,
//! section start/length in bars on the detected beat grid, free-text
//! label). Until those files exist in `corpus/annotations/`, the only
//! F1 numbers that exist are the synthetic-corpus self-consistency check
//! in `kontinuum-analysis`'s `tests/corpus_pipeline.rs`, where the ground
//! truth is known by construction.

use serde::{Deserialize, Serialize};

/// The issue's trust gate: below this, segmentation output must not feed
/// the fitters.
pub const SEGMENTATION_F1_GATE: f64 = 0.7;

/// One track's section annotation (the human- or construction-supplied
/// ground truth). `tolerance_bars` is the matching slack for a detected
/// boundary to count as a hit — 1 bar is the annotator-realistic default.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentationAnnotation {
    pub track_id: String,
    #[serde(default = "default_tolerance")]
    pub tolerance_bars: u32,
    /// Sections in playback order; `start_bar` is 0-based on the track's
    /// beat grid.
    pub sections: Vec<AnnotatedSection>,
}

fn default_tolerance() -> u32 {
    1
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnnotatedSection {
    pub start_bar: u32,
    pub bars: u32,
    /// Free text; never compared by the scorer (detected labels are a
    /// separate, honestly-mapped vocabulary — see the corpus README).
    #[serde(default)]
    pub label: Option<String>,
}

impl SegmentationAnnotation {
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    /// Interior boundaries only (the first section's start is the track
    /// start, not a segmentation decision).
    fn truth_boundaries(&self) -> Vec<u32> {
        self.sections.iter().skip(1).map(|s| s.start_bar).collect()
    }
}

/// Precision/recall/F1 over interior section boundaries. A detected
/// boundary at bar `b` hits a truth boundary when `|b − t| ≤
/// tolerance_bars`; each truth boundary counts at most once (one-to-one,
/// greedy in detected order).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoundaryScores {
    pub precision: f64,
    pub recall: f64,
    pub f1: f64,
}

pub fn boundary_f1(
    detected: &[u32],
    truth: &SegmentationAnnotation,
) -> BoundaryScores {
    let targets = truth.truth_boundaries();
    let tol = i64::from(truth.tolerance_bars);
    let mut taken = vec![false; targets.len()];
    let mut hits = 0usize;
    for &d in detected {
        for (i, &t) in targets.iter().enumerate() {
            if !taken[i] && (i64::from(d) - i64::from(t)).abs() <= tol {
                taken[i] = true;
                hits += 1;
                break;
            }
        }
    }
    let precision = hit_ratio(hits, detected.len());
    let recall = hit_ratio(hits, targets.len());
    let f1 = if precision + recall > 0.0 {
        2.0 * precision * recall / (precision + recall)
    } else {
        0.0
    };
    BoundaryScores { precision, recall, f1 }
}

fn hit_ratio(hits: usize, total: usize) -> f64 {
    if total == 0 {
        // No boundaries on either side is perfect agreement, not a divide
        // by zero: a track that is one section throughout detects none.
        if hits == 0 { 1.0 } else { 0.0 }
    } else {
        hits as f64 / total as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ann(sections: &[(u32, u32)]) -> SegmentationAnnotation {
        SegmentationAnnotation {
            track_id: "t".into(),
            tolerance_bars: 1,
            sections: sections
                .iter()
                .map(|&(start_bar, bars)| AnnotatedSection {
                    start_bar,
                    bars,
                    label: None,
                })
                .collect(),
        }
    }

    #[test]
    fn perfect_detection_scores_one() {
        let truth = ann(&[(0, 8), (8, 16), (24, 40)]);
        let s = boundary_f1(&[8, 24], &truth);
        assert_eq!(s, BoundaryScores { precision: 1.0, recall: 1.0, f1: 1.0 });
    }

    #[test]
    fn tolerance_and_one_to_one_matching_hold() {
        let truth = ann(&[(0, 8), (8, 8), (16, 8)]);
        // ±1 bar hits both; a duplicate detection near the same boundary
        // must not double-count it.
        let s = boundary_f1(&[9, 8, 15], &truth);
        assert_eq!(s.precision, 2.0 / 3.0);
        assert_eq!(s.recall, 1.0);
    }

    #[test]
    fn misses_beyond_tolerance_score_zero() {
        let truth = ann(&[(0, 8), (8, 8)]);
        let s = boundary_f1(&[11], &truth);
        assert_eq!(s.f1, 0.0);
    }

    #[test]
    fn boundary_free_track_is_perfect_agreement() {
        let truth = ann(&[(0, 64)]);
        let s = boundary_f1(&[], &truth);
        assert_eq!(s.f1, 1.0);
        let s = boundary_f1(&[32], &truth);
        assert_eq!(s.f1, 0.0);
    }

    #[test]
    fn annotation_json_roundtrips() {
        let a = ann(&[(0, 8), (8, 8)]);
        let text = a.to_json().unwrap();
        assert_eq!(SegmentationAnnotation::from_json(&text).unwrap(), a);
    }
}
