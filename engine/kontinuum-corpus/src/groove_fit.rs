//! Groove template extraction (issue #23 → #17): cluster per-track
//! swing/velocity/microtiming profiles into named templates for the groove
//! layer's vocabulary.
//!
//! Documented choices:
//! - Feature vector = `[swing] ++ velocity_profile(16) ++
//!   microtiming_profile(16)` (33 dims); Euclidean distance; microtiming in
//!   ticks as measured by the #5 pipeline.
//! - k = [`GROOVE_K`] (the issue's k≈5), clamped to the count of tracks
//!   WITH a `GrooveObservation`; tracks without groove stats are skipped.
//! - Names are deterministic: clusters are sorted by (swing, velocity
//!   profile) ascending and named `t0`..`tk-1`, so the same corpus always
//!   yields the same names — a content-addressed ordering, not a discovery
//!   order that would shuffle with the input.

use serde::{Deserialize, Serialize};

use crate::schema::{GrooveObservation, TrackObservation};
use crate::stats;

pub const GROOVE_K: usize = 5;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GrooveTemplate {
    /// Deterministic name: "t0".. in (swing, velocity) sorted order.
    pub name: String,
    pub swing: f32,
    pub velocity_profile: [f32; 16],
    pub microtiming_profile: [f32; 16],
    /// Tracks clustered here (diagnostic for corpus iteration).
    pub members: u32,
}

/// The 33-dim clustering feature for one track's groove stats.
pub fn groove_feature(g: &GrooveObservation) -> Vec<f32> {
    let mut v = Vec::with_capacity(33);
    v.push(g.swing);
    v.extend_from_slice(&g.velocity_profile);
    v.extend_from_slice(&g.microtiming_profile);
    v
}

/// Fits the groove templates for one subgenre's observations.
pub fn fit_grooves(tracks: &[TrackObservation]) -> Vec<GrooveTemplate> {
    let feats: Vec<Vec<f32>> =
        tracks.iter().filter_map(|t| t.groove.as_ref()).map(groove_feature).collect();
    let mut templates: Vec<GrooveTemplate> = stats::kmeans(&feats, GROOVE_K)
        .into_iter()
        .map(|c| {
            let (swing, velocity_profile, microtiming_profile) = unpack(&c.centroid);
            GrooveTemplate {
                name: String::new(), // named after the sort below
                swing,
                velocity_profile,
                microtiming_profile,
                members: c.members.len() as u32,
            }
        })
        .collect();
    templates.sort_by(|a, b| {
        a.swing
            .total_cmp(&b.swing)
            .then_with(|| stats::lex_cmp(&a.velocity_profile, &b.velocity_profile))
    });
    for (i, t) in templates.iter_mut().enumerate() {
        t.name = format!("t{i}");
    }
    templates
}

/// Splits a centroid back into (swing, velocity, microtiming). Length is 33
/// by construction (`groove_feature`); the bounds-checked reads keep the
/// library path panic-free regardless.
fn unpack(c: &[f32]) -> (f32, [f32; 16], [f32; 16]) {
    let mut vel = [0.0f32; 16];
    let mut micro = [0.0f32; 16];
    for (i, slot) in vel.iter_mut().enumerate() {
        *slot = c.get(1 + i).copied().unwrap_or(0.0);
    }
    for (i, slot) in micro.iter_mut().enumerate() {
        *slot = c.get(17 + i).copied().unwrap_or(0.0);
    }
    (c.first().copied().unwrap_or(0.0), vel, micro)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(swing: f32, vel: f32) -> TrackObservation {
        TrackObservation {
            track_id: format!("g{swing}"),
            subgenre: "s".into(),
            bpm: 124.0,
            key: "F minor".into(),
            sections: vec![],
            transitions: vec![],
            groove: Some(GrooveObservation {
                swing,
                velocity_profile: [vel; 16],
                microtiming_profile: [0.0; 16],
            }),
        }
    }

    #[test]
    fn names_are_assigned_in_swing_order() {
        let tracks = vec![obs(0.2, 0.6), obs(0.0, 0.4), obs(0.1, 0.5)];
        let ts = fit_grooves(&tracks);
        assert_eq!(ts.len(), 3);
        assert_eq!(ts.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["t0", "t1", "t2"]);
        assert!(ts[0].swing < ts[1].swing && ts[1].swing < ts[2].swing);
    }

    #[test]
    fn tracks_without_groove_stats_are_skipped() {
        let mut t = obs(0.1, 0.5);
        t.groove = None;
        assert!(fit_grooves(&[t]).is_empty());
    }

    #[test]
    fn feature_is_swing_then_velocity_then_microtiming() {
        let g = GrooveObservation {
            swing: 0.25,
            velocity_profile: [1.0; 16],
            microtiming_profile: [-1.0; 16],
        };
        let f = groove_feature(&g);
        assert_eq!(f.len(), 33);
        assert_eq!(f[0], 0.25);
        assert_eq!(f[1], 1.0);
        assert_eq!(f[17], -1.0);
    }
}
