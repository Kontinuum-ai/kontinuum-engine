//! Reference profiles and the regression ratchet (issue #52 WS4): a profile
//! states per-metric bounds derived from the reference material; a baseline
//! stores the engine's best measured distance and how much drift the CI
//! tolerates before failing. Numbers gate regressions; ears gate releases.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::metrics::Metrics;

/// A per-metric bound. `min` for "at least this good" metrics (dynamics,
/// crest, cv), `max` for "at most this much" metrics (mid share, centroid).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TargetBound {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
}

impl TargetBound {
    fn violation(&self, value: f64) -> f64 {
        let mut v: f64 = 0.0;
        if let Some(min) = self.min {
            if value < min {
                v = v.max(min - value);
            }
        }
        if let Some(max) = self.max {
            if value > max {
                v = v.max(value - max);
            }
        }
        v
    }
}

/// Named metric bounds; every name maps to a [`Metrics`] field via
/// [`metric_value`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QualityProfile {
    pub name: String,
    /// Tolerance per metric name, used to normalize violations into the
    /// distance. Missing entries use [`default_tolerance`].
    #[serde(default)]
    pub tolerances: std::collections::BTreeMap<String, f64>,
    /// Metric name → bound.
    pub targets: std::collections::BTreeMap<String, TargetBound>,
}

fn default_tolerance(name: &str) -> f64 {
    match name {
        // Scaled to the corrected metric definitions: dynamics is now a
        // percentile spread of ~3..11 dB rather than a max-over-min of ~20..60,
        // and the centroid is magnitude-weighted (low thousands of Hz), so the
        // old 5 dB / 50 Hz normalizers turned ordinary drift into huge
        // distances.
        "short_term_dyn_db" => 2.0,
        "crest_db" => 1.0,
        "hit_cv" => 0.05,
        "transients_per_sec" => 1.0,
        "centroid_hz" => 400.0,
        s if s.starts_with("band_") => 0.02,
        _ => 1.0,
    }
}

/// The metric a profile name refers to.
pub fn metric_value(metrics: &Metrics, name: &str) -> Option<f64> {
    match name {
        "rms_dbfs" => Some(metrics.rms_dbfs),
        "peak_dbfs" => Some(metrics.peak_dbfs),
        "true_peak_dbfs" => Some(metrics.true_peak_dbfs),
        "crest_db" => Some(metrics.crest_db),
        "short_term_dyn_db" => Some(metrics.short_term_dyn_db),
        "centroid_hz" => Some(metrics.centroid_hz),
        "transients_per_sec" => Some(metrics.transients_per_sec),
        "hit_cv" => Some(metrics.hit_cv),
        s => s
            .strip_prefix("band_")
            .map(|band| metrics.band(band)),
    }
}

impl QualityProfile {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        serde_json::from_str(&text).map_err(|e| e.to_string())
    }

    /// Sum of tolerance-normalized violations. 0.0 = inside every target.
    pub fn distance(&self, metrics: &Metrics) -> f64 {
        let mut d = 0.0;
        for (name, bound) in &self.targets {
            let Some(value) = metric_value(metrics, name) else { continue };
            let violation = bound.violation(value);
            if violation > 0.0 {
                let tolerance = self
                    .tolerances
                    .get(name)
                    .copied()
                    .unwrap_or_else(|| default_tolerance(name))
                    .max(1e-9);
                d += violation / tolerance;
            }
        }
        d
    }
}

/// The stored ratchet: the engine's best measured distance for a profile on
/// a fixed seed, plus allowed drift.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Baseline {
    pub profile: String,
    pub seed: u64,
    pub distance: f64,
    /// How much worse than `distance` the gate tolerates.
    pub ratchet: f64,
    /// The metrics the distance was measured on (for the job log).
    pub metrics: Metrics,
}

impl Baseline {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        serde_json::from_str(&text).map_err(|e| e.to_string())
    }

    pub fn passes(&self, metrics: &Metrics, profile: &QualityProfile) -> Result<f64, f64> {
        let d = profile.distance(metrics);
        if d <= self.distance + self.ratchet {
            Ok(d)
        } else {
            Err(d)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(targets: &[(&str, f64, bool)]) -> QualityProfile {
        QualityProfile {
            name: "t".into(),
            tolerances: Default::default(),
            targets: targets
                .iter()
                .map(|(n, v, is_min)| {
                    (
                        n.to_string(),
                        if *is_min {
                            TargetBound { min: Some(*v), max: None }
                        } else {
                            TargetBound { min: None, max: Some(*v) }
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn distance_is_zero_inside_targets_and_grows_outside() {
        let p = profile(&[("crest_db", 14.0, true), ("band_mid", 0.06, false)]);
        let mut m = Metrics {
            rms_dbfs: -12.0,
            peak_dbfs: 0.0,
            true_peak_dbfs: 0.0,
            crest_db: 14.5,
            short_term_dyn_db: 30.0,
            centroid_hz: 400.0,
            band_shares: vec![0.0; 6],
            transients_per_sec: 1.0,
            hit_cv: 0.4,
        };
        m.band_shares[crate::BANDS.iter().position(|b| b.0 == "mid").unwrap()] = 0.05;
        assert_eq!(p.distance(&m), 0.0, "inside targets");
        m.crest_db = 12.0;
        assert!(p.distance(&m) > 0.0, "crest violation counts");
        m.crest_db = 15.0;
        m.band_shares[3] = 0.20;
        let far = p.distance(&m);
        assert!(far > 1.0, "far violation grows: {far}");
    }

    #[test]
    fn ratchet_gate_passes_improvements_and_blocks_regressions() {
        let p = profile(&[("crest_db", 14.0, true)]);
        let base = Baseline {
            profile: "t".into(),
            seed: 7,
            distance: 2.0,
            ratchet: 0.5,
            metrics: Metrics {
                rms_dbfs: 0.0,
                peak_dbfs: 0.0,
                true_peak_dbfs: 0.0,
                crest_db: 13.0,
                short_term_dyn_db: 0.0,
                centroid_hz: 0.0,
                band_shares: vec![0.0; 6],
                transients_per_sec: 0.0,
                hit_cv: 0.0,
            },
        };
        let mut m = base.metrics.clone();
        m.crest_db = 13.4; // distance 0.6 ≤ 2.5
        assert!(base.passes(&m, &p).is_ok());
        let mut worse = base.metrics.clone();
        worse.crest_db = 9.0; // distance 5.0 > 2.5
        assert!(base.passes(&worse, &p).is_err());
    }
}
