//! The #23 corpus batch analysis: audio → `kontinuum_corpus::
//! TrackObservation`. This is the hardened #5 producer half of the
//! contract — tempo/key, beat grid, structural segmentation (gated by the
//! corpus crate's F1 harness), per-section energy/density/brightness,
//! boundary-type classification, and groove/microtiming stats.
//!
//! Pipeline version [`PIPELINE_VERSION`] must be bumped whenever feature
//! definitions change; observations record nothing version-specific
//! themselves, so a corpus re-run is always a full re-run (cheap, and the
//! only way to keep features comparable).
//!
//! Limits, stated honestly: strict 4/4 dance material, 60–200 BPM,
//! mono-folded stereo WAVs. Everything here is deterministic — same
//! bytes in, same observation out.

pub mod features;
pub mod groove;
pub mod key;
pub mod onsets;
pub mod segment;
pub mod tempo;

use kontinuum_corpus::{
    SectionObservation, TrackObservation, TransitionObservation,
};

/// Bump on any change to feature definitions or detection parameters.
pub const PIPELINE_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum AnalysisError {
    #[error("audio too short to analyze ({samples} samples)")]
    TooShort { samples: usize },
    #[error("tempo/beat grid not found (arrhythmic or silent material)")]
    TempoFailed,
    #[error("WAV decode failed: {0}")]
    Decode(String),
}

/// One track's pipeline result: the schema observation plus the
/// intermediate state the validation harness grades against.
pub struct TrackAnalysis {
    pub observation: TrackObservation,
    /// Detected interior boundary bars (the segmentation-F1 input).
    pub boundary_bars: Vec<u32>,
    pub grid: tempo::BeatGrid,
    pub bar_features: Vec<features::BarFeatures>,
    pub segmentation: segment::Segmentation,
}

/// Analyzes one mono render. `bpm_hint` is the manifest's declared BPM —
/// it anchors tempo detection (see [`tempo::detect`]). Deterministic.
pub fn analyze_track(
    track_id: &str,
    subgenre: &str,
    mono: &[f32],
    sr: u32,
    bpm_hint: f64,
) -> Result<TrackAnalysis, AnalysisError> {
    let track_sec = mono.len() as f64 / f64::from(sr);
    if mono.len() < sr as usize * 8 {
        return Err(AnalysisError::TooShort { samples: mono.len() });
    }
    let detected = onsets::pick_onsets(mono, sr);
    let percussive = onsets::pick_percussive_onsets(mono, sr);
    let grid = tempo::detect(mono, sr, bpm_hint)?;
    let mut bars = features::per_bar(mono, sr, &grid, &detected);
    // Feature curves must cover exactly the bars the grid defines.
    bars.truncate(grid.total_bars(track_sec) as usize);
    let segmentation = segment::segment(&bars);

    // mean_density is normalized to 0..=1 (schema contract) against the
    // track's densest bar.
    let max_density = bars.iter().map(|f| f.density).fold(1e-9, f64::max);
    let sections: Vec<SectionObservation> = segmentation
        .sections
        .iter()
        .map(|s| {
            let a = s.start_bar as usize;
            let span = &bars[a..a + s.bars as usize];
            let mean = |get: fn(&features::BarFeatures) -> f64| {
                (span.iter().map(|f| get(f)).sum::<f64>() / span.len().max(1) as f64) as f32
            };
            SectionObservation {
                kind: s.kind.to_string(),
                start_bar: s.start_bar,
                bars: s.bars,
                mean_energy: mean(|f| f.energy),
                mean_density: (mean(|f| f.density) / max_density as f32).clamp(0.0, 1.0),
                mean_brightness: mean(|f| f.brightness),
            }
        })
        .collect();
    let transitions: Vec<TransitionObservation> = segmentation
        .boundaries
        .iter()
        .enumerate()
        .map(|(i, b)| TransitionObservation {
            from_section_index: i,
            to_section_index: i + 1,
            kind: b.kind.to_string(),
        })
        .collect();

    let observation = TrackObservation {
        track_id: track_id.to_string(),
        subgenre: subgenre.to_string(),
        bpm: grid.bpm as f32,
        key: key::detect(mono, sr),
        sections,
        transitions,
        groove: groove::observe(&grid, &percussive),
    };
    Ok(TrackAnalysis {
        observation,
        boundary_bars: segmentation.boundaries.iter().map(|b| b.bar).collect(),
        grid,
        bar_features: bars,
        segmentation,
    })
}

/// Decodes a WAV (i16 PCM or f32, any channel count folded to mono) from
/// raw file bytes.
pub fn decode_wav(bytes: &[u8]) -> Result<(Vec<f32>, u32), AnalysisError> {
    let cursor = std::io::Cursor::new(bytes);
    let mut reader = hound::WavReader::new(cursor).map_err(|e| AnalysisError::Decode(e.to_string()))?;
    let spec = reader.spec();
    let channels = spec.channels as usize;
    let mut all = Vec::new();
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for s in reader.samples::<f32>() {
                all.push(s.map_err(|e| AnalysisError::Decode(e.to_string()))?);
            }
        }
        hound::SampleFormat::Int => {
            for s in reader.samples::<i16>() {
                all.push(s.map_err(|e| AnalysisError::Decode(e.to_string()))? as f32 / 32768.0);
            }
        }
    }
    let mono: Vec<f32> = all
        .chunks(channels.max(1))
        .map(|frame| frame.iter().sum::<f32>() / channels.max(1) as f32)
        .collect();
    Ok((mono, spec.sample_rate))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthgen;

    #[test]
    fn analyze_recovers_a_fixture_track_end_to_end() {
        let preset = synthgen::preset_by_id("mt-a").unwrap();
        let mono = synthgen::render(preset);
        let a = analyze_track(preset.track_id, preset.subgenre, &mono, synthgen::SYNTH_SAMPLE_RATE, preset.bpm)
            .expect("fixture analyzes");
        assert!(
            (f64::from(a.observation.bpm) - preset.bpm).abs() <= 0.5,
            "bpm {} vs planted {}",
            a.observation.bpm,
            preset.bpm
        );
        let ann = synthgen::planted_annotation(preset);
        let scores = kontinuum_corpus::boundary_f1(&a.boundary_bars, &ann);
        assert!(
            scores.f1 >= kontinuum_corpus::SEGMENTATION_F1_GATE,
            "segmentation F1 {} below gate",
            scores.f1
        );
        assert!(a.observation.groove.is_some());
        assert_eq!(a.observation.transitions.len(), a.boundary_bars.len());
    }
}
