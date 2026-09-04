//! Energy-arc families (issue #23): cluster normalized per-track energy
//! curves into k≈5 families for the #16 planner.
//!
//! Documented choices:
//! - A track's arc is the sequence of per-section `mean_energy` values in
//!   `start_bar` order, resampled to [`ARC_POINTS`] points by
//!   piecewise-linear interpolation (tracks have different section counts)
//!   and normalized so the peak equals 1.0 — clusters capture ARC SHAPE,
//!   not loudness.
//! - k = [`ARC_K`] (the issue's k≈5), clamped down to the track count;
//!   fixed-iteration deterministic k-means (see [`crate::stats`]).
//! - `spread` is the mean Euclidean distance of members to the centroid;
//!   `weight` is the fraction of the subgenre's tracks in the family.

use serde::{Deserialize, Serialize};

use crate::schema::{SectionObservation, TrackObservation};
use crate::stats;

pub const ARC_POINTS: usize = 8;
pub const ARC_K: usize = 5;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArcCluster {
    /// Resampled ([`ARC_POINTS`]-point) centroid, peak-normalized.
    pub centroid: Vec<f32>,
    /// Mean member-to-centroid Euclidean distance.
    pub spread: f32,
    /// Fraction of the subgenre's tracks in this family.
    pub weight: f32,
}

/// The track's normalized energy arc (sections sorted by `start_bar`).
pub fn track_arc(track: &TrackObservation) -> Vec<f32> {
    let mut secs: Vec<&SectionObservation> = track.sections.iter().collect();
    secs.sort_by_key(|s| s.start_bar);
    let curve: Vec<f32> = secs.iter().map(|s| s.mean_energy).collect();
    normalize_peak(resample(&curve, ARC_POINTS))
}

/// Fits the arc families for one subgenre's observations.
pub fn fit_arcs(tracks: &[TrackObservation]) -> Vec<ArcCluster> {
    let arcs: Vec<Vec<f32>> = tracks.iter().map(track_arc).collect();
    stats::kmeans(&arcs, ARC_K)
        .into_iter()
        .map(|c| {
            let n = c.members.len() as f32;
            let spread =
                c.members.iter().map(|&m| stats::sq_dist(&arcs[m], &c.centroid).sqrt()).sum::<f32>()
                    / n;
            ArcCluster { centroid: c.centroid, spread, weight: n / tracks.len() as f32 }
        })
        .collect()
}

/// Nearest-centroid assignment index (diagnostics and tests).
pub fn assign(arc: &[f32], centroids: &[Vec<f32>]) -> usize {
    let mut best = 0;
    let mut best_d = f32::INFINITY;
    for (ci, c) in centroids.iter().enumerate() {
        let d = stats::sq_dist(arc, c);
        if d < best_d {
            best_d = d;
            best = ci;
        }
    }
    best
}

fn resample(curve: &[f32], n: usize) -> Vec<f32> {
    if curve.is_empty() {
        return vec![0.0; n];
    }
    if curve.len() == 1 {
        return vec![curve[0]; n];
    }
    let last = curve.len() - 1;
    (0..n)
        .map(|j| {
            let t = j as f32 / (n - 1) as f32 * last as f32;
            let i = (t as usize).min(last - 1);
            let frac = t - i as f32;
            curve[i] * (1.0 - frac) + curve[i + 1] * frac
        })
        .collect()
}

/// Scales the curve so its peak is 1.0 (all-zero curves stay all-zero).
fn normalize_peak(mut v: Vec<f32>) -> Vec<f32> {
    let max = v.iter().copied().fold(0.0f32, f32::max);
    if max > f32::EPSILON {
        for x in &mut v {
            *x /= max;
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_is_linear_and_endpoint_exact() {
        assert_eq!(resample(&[0.0, 1.0], 3), vec![0.0, 0.5, 1.0]);
        let flat = resample(&[4.0], 8);
        assert!(flat.iter().all(|x| *x == 4.0));
    }

    #[test]
    fn normalization_sets_peak_to_one() {
        let v = normalize_peak(vec![0.2, 0.8, 0.4]);
        assert_eq!(v, vec![0.25, 1.0, 0.5]);
        assert_eq!(normalize_peak(vec![0.0; 4]), vec![0.0; 4]);
    }

    #[test]
    fn track_arc_sorts_sections_by_start_bar() {
        let t = crate::schema::TrackObservation {
            track_id: "t".into(),
            subgenre: "s".into(),
            bpm: 124.0,
            key: "F minor".into(),
            sections: vec![
                crate::schema::SectionObservation {
                    kind: "outro".into(),
                    start_bar: 40,
                    bars: 8,
                    mean_energy: 0.2,
                    mean_density: 0.2,
                    mean_brightness: 0.2,
                },
                crate::schema::SectionObservation {
                    kind: "intro".into(),
                    start_bar: 0,
                    bars: 8,
                    mean_energy: 0.1,
                    mean_density: 0.2,
                    mean_brightness: 0.2,
                },
            ],
            transitions: vec![],
            groove: None,
        };
        let arc = track_arc(&t);
        assert!(arc[ARC_POINTS - 1] > arc[0], "intro (0.1) sorts first, arc rises: {arc:?}");
        assert!((arc.iter().cloned().fold(0.0f32, f32::max) - 1.0).abs() < 1e-6);
    }
}
