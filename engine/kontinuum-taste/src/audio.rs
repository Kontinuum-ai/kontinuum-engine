//! Audio-derived DNA (issue #21): per-track abstract features through the
//! #5 on-device analysis subset, aggregated into user DNA as a weighted
//! mean + dispersion per field. Pinned references weigh more.
//!
//! **No audio retention.** [`TrackDna`] carries numbers, never samples;
//! PCM enters [`analyze_pcm`] by reference and is dropped on return. The
//! store persists only the serialized [`TrackDna`] (enforced by test: the
//! stored byte size is independent of the audio's length).

use kontinuum_compose::taste::{Stat, TasteProfile};
use kontinuum_corpus::TrackObservation;

use crate::error::TasteError;
use crate::model::weighted_stat;

/// Weight of a pinned reference relative to an ordinary library track
/// ("like this" exemplars dominate the aggregate — issue #21/#26).
pub const PIN_WEIGHT: f32 = 4.0;
const LIBRARY_WEIGHT: f32 = 1.0;

/// The abstract per-track DNA. Everything 0..=1 unless named otherwise;
/// everything derived on-device from the #5 subset.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TrackDna {
    pub track_id: String,
    pub bpm: f32,
    /// Swing (off-8th lateness share), 0..1. `None` = the groove stats
    /// did not converge for this track.
    pub swing: Option<f32>,
    /// Mean spectral tilt (2k–8 kHz share of >300 Hz power), 0..1.
    pub brightness: f32,
    /// Mean normalized bar energy, 0..1.
    pub energy: f32,
    /// Mean normalized event density, 0..1.
    pub density: f32,
    /// Mean section length in bars.
    pub section_bars: f32,
    /// Which analyzer produced this (pipeline version contract).
    pub pipeline_version: u32,
}

impl TrackDna {
    /// Runs the #5 on-device subset over decoded mono PCM. `bpm_hint`
    /// anchors tempo detection (0 = no hint). The audio is consumed in
    /// place — this function retains nothing.
    pub fn analyze(
        track_id: &str,
        mono: &[f32],
        sr: u32,
        bpm_hint: f64,
    ) -> Result<TrackDna, TasteError> {
        let analysis = kontinuum_analysis::corpus::analyze_track(
            track_id,
            "user",
            mono,
            sr,
            if bpm_hint > 0.0 { bpm_hint } else { 124.0 },
        )
        .map_err(|e| TasteError::Analysis(e.to_string()))?;
        Ok(TrackDna::from_analysis(track_id, &analysis))
    }

    fn from_analysis(track_id: &str, a: &kontinuum_analysis::corpus::TrackAnalysis) -> TrackDna {
        let obs: &TrackObservation = &a.observation;
        let n = a.bar_features.len().max(1) as f64;
        let mean = |get: fn(&kontinuum_analysis::corpus::features::BarFeatures) -> f64| {
            (a.bar_features.iter().map(get).sum::<f64>() / n) as f32
        };
        let max_density = a
            .bar_features
            .iter()
            .map(|f| f.density)
            .fold(1e-9, f64::max);
        let section_bars = if obs.sections.is_empty() {
            8.0
        } else {
            obs.sections.iter().map(|s| s.bars as f32).sum::<f32>() / obs.sections.len() as f32
        };
        TrackDna {
            track_id: track_id.to_string(),
            bpm: obs.bpm,
            swing: obs.groove.as_ref().map(|g| g.swing),
            brightness: mean(|f| f.brightness).clamp(0.0, 1.0),
            energy: mean(|f| f.energy).clamp(0.0, 1.0),
            density: (mean(|f| f.density) / max_density as f32).clamp(0.0, 1.0),
            section_bars: section_bars.clamp(1.0, 128.0),
            pipeline_version: kontinuum_analysis::corpus::PIPELINE_VERSION,
        }
    }
}

/// One (track, weight) contribution to the user DNA.
#[derive(Clone, Debug, PartialEq)]
pub struct Contribution {
    pub dna: TrackDna,
    pub pinned: bool,
}

impl Contribution {
    pub fn library(dna: TrackDna) -> Self {
        Contribution { dna, pinned: false }
    }

    pub fn pinned(dna: TrackDna) -> Self {
        Contribution { dna, pinned: true }
    }

    /// The weight this contribution carries in the aggregate
    /// ([`PIN_WEIGHT`] when pinned, 1.0 otherwise).
    pub fn weight(&self) -> f32 {
        if self.pinned { PIN_WEIGHT } else { LIBRARY_WEIGHT }
    }
}

/// The audio half of the user DNA, merged into `profile`:
/// swing/brightness/section-length stats (weighted mean + dispersion),
/// tempo prior (measured BPM outranks metadata), and the energy/density
/// point fields when audio evidence exists.
pub fn apply_audio_dna(profile: &mut TasteProfile, contributions: &[Contribution]) {
    if contributions.is_empty() {
        return;
    }
    let weights: Vec<f32> = contributions.iter().map(|c| c.weight()).collect();
    let get = |f: fn(&TrackDna) -> Option<f32>| -> Option<Stat> {
        let samples: Vec<(f32, f32)> = contributions
            .iter()
            .zip(&weights)
            .filter_map(|(c, w)| f(&c.dna).map(|v| (v, *w)))
            .collect();
        weighted_stat(&samples)
    };
    let swings = get(|d| d.swing);
    if let Some(s) = swings {
        profile.swing = Some(s);
    }
    profile.brightness = get(|d| Some(d.brightness));
    profile.section_bars = get(|d| Some(d.section_bars));
    // Measured tempo is the strongest evidence: it replaces metadata BPM.
    let bpms: Vec<(f32, f32)> = contributions
        .iter()
        .zip(&weights)
        .map(|(c, w)| (c.dna.bpm, *w))
        .collect();
    if let Some(t) = weighted_stat(&bpms) {
        profile.bpm = Some(f64::from(t.mean));
        profile.tempo_dispersion = Some(f64::from(t.dispersion));
    }
    if let Some(e) = get(|d| Some(d.energy)) {
        profile.energy = e.mean.clamp(0.05, 1.0);
    }
    if let Some(d) = get(|d| Some(d.density)) {
        profile.density = d.mean.clamp(0.1, 1.0);
    }
    profile.dna_version = kontinuum_compose::taste::DNA_VERSION;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dna(id: &str, swing: f32, brightness: f32, bpm: f32) -> TrackDna {
        TrackDna {
            track_id: id.into(),
            bpm,
            swing: Some(swing),
            brightness,
            energy: 0.5,
            density: 0.5,
            section_bars: 16.0,
            pipeline_version: 1,
        }
    }

    #[test]
    fn pinned_references_dominate_the_aggregate() {
        let mut p = TasteProfile::default();
        let contribs = vec![
            Contribution::library(dna("a", 0.0, 0.2, 120.0)),
            Contribution::library(dna("b", 0.0, 0.2, 120.0)),
            Contribution::pinned(dna("pin", 0.3, 0.8, 126.0)),
        ];
        apply_audio_dna(&mut p, &contribs);
        let s = p.swing.unwrap();
        // Two library (w=1, 0.0) + one pin (w=4, 0.3): mean = 1.2/6 = 0.2.
        assert!((s.mean - 0.2).abs() < 1e-4, "swing mean {}", s.mean);
        assert!(s.dispersion > 0.0, "mixed evidence disperses");
        // BPM follows the pin's 126 pull: (120*2 + 126*4)/6 = 124.
        assert!((p.bpm.unwrap() - 124.0).abs() < 1e-6);
    }

    #[test]
    fn dispersion_grows_with_taste_width() {
        let narrow = vec![Contribution::library(dna("a", 0.1, 0.3, 125.0))];
        let wide = vec![
            Contribution::library(dna("a", 0.0, 0.1, 118.0)),
            Contribution::library(dna("b", 0.4, 0.9, 134.0)),
        ];
        let mut pn = TasteProfile::default();
        let mut pw = TasteProfile::default();
        apply_audio_dna(&mut pn, &narrow);
        apply_audio_dna(&mut pw, &wide);
        assert!(pw.swing.unwrap().dispersion > pn.swing.unwrap().dispersion);
        assert!(pw.tempo_dispersion.unwrap() > pn.tempo_dispersion.unwrap());
    }

    #[test]
    fn empty_contributions_change_nothing() {
        let mut p = TasteProfile::default();
        p.swing = Some(Stat::new(0.5, 0.1));
        apply_audio_dna(&mut p, &[]);
        assert_eq!(p.swing, Some(Stat::new(0.5, 0.1)));
    }
}
