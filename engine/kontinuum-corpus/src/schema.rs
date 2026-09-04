//! Observation contract (issue #23): one JSON record per analyzed track.
//!
//! The #5 analysis pipeline (tempo/grid, structural segmentation, boundary
//! classification, groove stats) is the PRODUCER of these records; it writes
//! one JSON object per line (JSONL) into `corpus/features/`. Hardening that
//! pipeline is issue #5's scope — this crate owns the contract and the
//! fitters that consume it.
//!
//! Label fields are free strings: detectors emit their own vocabulary
//! (`intro/build/drop/break/groove/outro/unknown`… for sections,
//! `filter_sweep/silence/fill/hard_cut/riser`… for boundaries). The mapping
//! onto #16's grammar states (`Intro/Dev/Breakdown/Reintro/Outro`) stays
//! with the planner — documented honestly: detected sections never label
//! themselves "reintro".
//!
//! NOTE: until the real corpus (100–300 purchased tracks) is analyzed, the
//! only data flowing through this schema is the synthetic fixture
//! (`fixtures/corpus-sample.jsonl`) — every fitted artifact is a PLACEHOLDER
//! validated for pipeline shape, not musical truth.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// One analyzed track: the unit the fitters aggregate per subgenre.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackObservation {
    pub track_id: String,
    pub subgenre: String,
    pub bpm: f32,
    pub key: String,
    /// Sections in detection order; sorted by `start_bar` before fitting.
    pub sections: Vec<SectionObservation>,
    /// Detected boundary events (indices into `sections`).
    #[serde(default)]
    pub transitions: Vec<TransitionObservation>,
    /// Absent when groove stats did not converge for the track.
    pub groove: Option<GrooveObservation>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SectionObservation {
    /// Free detector label, e.g. intro/build/drop/break/groove/outro/unknown.
    pub kind: String,
    pub start_bar: u32,
    pub bars: u32,
    /// Section means from the #5 feature extraction, all 0..=1.
    pub mean_energy: f32,
    pub mean_density: f32,
    pub mean_brightness: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransitionObservation {
    pub from_section_index: usize,
    pub to_section_index: usize,
    /// Free detector label, e.g. filter_sweep/silence/fill/hard_cut.
    pub kind: String,
}

/// Per-track groove statistics (issue #5's percussive-band method).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrooveObservation {
    /// Shuffle amount, 0..=1.
    pub swing: f32,
    /// Mean velocity per 16th step across the track's grooves.
    pub velocity_profile: [f32; 16],
    /// Mean microtiming offset (ticks, ±120) per 16th step.
    pub microtiming_profile: [f32; 16],
}

/// Parses one JSON object per line; blank lines are skipped. The line
/// number in the error is 1-based for editor navigation.
pub fn load_jsonl(text: &str) -> Result<Vec<TrackObservation>, crate::CorpusError> {
    let mut out = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let obs: TrackObservation = serde_json::from_str(line)
            .map_err(|source| crate::CorpusError::Json { line: idx + 1, source })?;
        out.push(obs);
    }
    Ok(out)
}

/// Reads and parses a JSONL observation file.
pub fn load_jsonl_file(path: &Path) -> Result<Vec<TrackObservation>, crate::CorpusError> {
    load_jsonl(&std::fs::read_to_string(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observation_roundtrips_through_json() {
        let obs = TrackObservation {
            track_id: "t1".into(),
            subgenre: "minimal-techno".into(),
            bpm: 128.0,
            key: "F minor".into(),
            sections: vec![SectionObservation {
                kind: "intro".into(),
                start_bar: 0,
                bars: 8,
                mean_energy: 0.3,
                mean_density: 0.34,
                mean_brightness: 0.79,
            }],
            transitions: vec![TransitionObservation {
                from_section_index: 0,
                to_section_index: 1,
                kind: "filter_sweep".into(),
            }],
            groove: Some(GrooveObservation {
                swing: 0.1,
                velocity_profile: [0.5; 16],
                microtiming_profile: [0.0; 16],
            }),
        };
        let text = serde_json::to_string(&obs).unwrap();
        assert_eq!(load_jsonl(&text).unwrap(), vec![obs]);
    }

    #[test]
    fn blank_lines_are_skipped_and_bad_lines_reported() {
        assert!(load_jsonl("\n \n").unwrap().is_empty());
        let err = load_jsonl("{ not json }").unwrap_err();
        assert!(matches!(err, crate::CorpusError::Json { line: 1, .. }));
    }
}
