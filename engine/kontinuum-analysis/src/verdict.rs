//! Critic verdict layer (issue #25): rule-based, deterministic, no ML.
//! [`CriticVerdict::evaluate`] compares a [`CriticSnapshot`] against
//! versioned [`CriticTargets`] and produces per-axis scores plus the
//! kill-switch flags.
//!
//! Consumers: #15 kill-switch reads the flags (bar cadence), #26 reward
//! model reads the axis scores, #22 composer context gets both. Scores
//! follow the tolerance-normalized violation convention of
//! `profile::QualityProfile::distance`: 0.0 means inside target, larger
//! means worse, normalized so axes are comparable.
//!
//! Targets fixture: `fixtures/critic-targets.json` (versioned; v1 values
//! are the minimal-techno hypotheses — crest floor tied to the #52 genre
//! profile's `crest_db` minimum, loudness/tilt/sub caps from #28).

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::critic::CriticSnapshot;

/// Versioned critic targets. `version` bumps on semantic changes so #26
/// models can pin the schema they were trained against.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CriticTargets {
    pub version: u32,
    pub name: String,
    /// Integrated-loudness bullseye (LUFS).
    pub integrated_target_lufs: f64,
    /// Half-width of the acceptable loudness window (LU).
    pub loudness_tolerance_lu: f64,
    /// Crest below this reads as collapsed dynamics (dB).
    pub crest_floor_db: f64,
    /// Normalization for the dynamics axis score (dB).
    pub crest_tolerance_db: f64,
    /// Expected spectral tilt (dB/octave, 100 Hz–10 kHz).
    pub tilt_target_db_per_oct: f64,
    /// Half-width of the acceptable tilt window (dB/octave).
    pub tilt_tolerance_db_per_oct: f64,
    /// 20–60 Hz energy share above which the low end reads as rumble.
    pub sub_share_cap: f64,
    /// Normalization for the low-end axis score (share units).
    pub sub_share_tolerance: f64,
}

impl CriticTargets {
    /// Mirrors `profile::QualityProfile::load` (JSON, `String` errors).
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        serde_json::from_str(&text).map_err(|e| e.to_string())
    }
}

/// Kill-switch flags — each fires when its axis score leaves zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CriticFlags {
    /// Crest fell below the floor (over-limited / over-clipped master).
    pub dynamics_collapsed: bool,
    /// Tilt outside the target tolerance (harsh or muffled balance).
    pub spectral_imbalance: bool,
    /// Sub-band energy above the cap (rumble / phase trouble).
    pub sub_rumble: bool,
    /// Integrated loudness below target minus tolerance.
    pub loudness_shortfall: bool,
    /// Integrated loudness above target plus tolerance.
    pub loudness_excess: bool,
}

impl CriticFlags {
    pub fn any(self) -> bool {
        self != CriticFlags::default()
    }
}

/// Per-axis scores (0 = inside target, growing with normalized violation)
/// plus the flags. Serde-serializable: this is the #26/#15 feed contract.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CriticVerdict {
    pub dynamics_score: f64,
    pub spectral_score: f64,
    pub low_end_score: f64,
    pub loudness_score: f64,
    pub flags: CriticFlags,
}

impl CriticVerdict {
    /// Evaluate a snapshot against targets. Total over all targets must
    /// be met with tolerances; violations are computed one-sided per
    /// axis and never negative.
    pub fn evaluate(snapshot: &CriticSnapshot, targets: &CriticTargets) -> Self {
        let dynamics = violation(snapshot.crest_db, targets.crest_floor_db, true)
            / targets.crest_tolerance_db.max(1e-9);
        let spectral = violation(
            (snapshot.tilt_db_per_oct - targets.tilt_target_db_per_oct).abs(),
            targets.tilt_tolerance_db_per_oct,
            false,
        ) / targets.tilt_tolerance_db_per_oct.max(1e-9);
        let low_end = violation(snapshot.sub_share, targets.sub_share_cap, false)
            / targets.sub_share_tolerance.max(1e-9);
        let shortfall = violation(
            targets.integrated_target_lufs - snapshot.integrated_lufs,
            targets.loudness_tolerance_lu,
            false,
        ) / targets.loudness_tolerance_lu.max(1e-9);
        let excess = violation(
            snapshot.integrated_lufs - targets.integrated_target_lufs,
            targets.loudness_tolerance_lu,
            false,
        ) / targets.loudness_tolerance_lu.max(1e-9);
        CriticVerdict {
            dynamics_score: dynamics,
            spectral_score: spectral,
            low_end_score: low_end,
            loudness_score: shortfall.max(excess),
            flags: CriticFlags {
                dynamics_collapsed: dynamics > 0.0,
                spectral_imbalance: spectral > 0.0,
                sub_rumble: low_end > 0.0,
                loudness_shortfall: shortfall > 0.0,
                loudness_excess: excess > 0.0,
            },
        }
    }

    /// Sum of axis scores — 0.0 means every target is met.
    pub fn total(&self) -> f64 {
        self.dynamics_score
            + self.spectral_score
            + self.low_end_score
            + self.loudness_score
    }
}

/// Distance beyond a one-sided bound: `min`-style when `floor` is true
/// (value must stay at/above the bound), `max`-style otherwise. Never
/// negative; NaN input reads as maximally violated (0.0 → 0 only for
/// finite in-tolerance values, so a poisoned metric cannot hide).
fn violation(value: f64, bound: f64, floor: bool) -> f64 {
    if value.is_nan() {
        return f64::INFINITY;
    }
    if floor {
        (bound - value).max(0.0)
    } else {
        (value - bound).max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn targets() -> CriticTargets {
        CriticTargets {
            version: 1,
            name: "test".into(),
            integrated_target_lufs: -10.0,
            loudness_tolerance_lu: 2.5,
            crest_floor_db: 13.0,
            crest_tolerance_db: 1.0,
            tilt_target_db_per_oct: -3.0,
            tilt_tolerance_db_per_oct: 2.0,
            sub_share_cap: 0.45,
            sub_share_tolerance: 0.05,
        }
    }

    fn snapshot(crest: f64, tilt: f64, sub: f64, lufs: f64) -> CriticSnapshot {
        CriticSnapshot {
            crest_db: crest,
            tilt_db_per_oct: tilt,
            sub_share: sub,
            integrated_lufs: lufs,
            ..CriticSnapshot::default()
        }
    }

    #[test]
    fn on_target_snapshot_stays_quiet() {
        let t = targets();
        let v = CriticVerdict::evaluate(&snapshot(14.0, -3.0, 0.40, -10.0), &t);
        assert_eq!(v.total(), 0.0);
        assert!(!v.flags.any());
    }

    #[test]
    fn planted_defects_fire_exactly_their_flags() {
        let t = targets();
        fn assert_only(got: CriticFlags, want: CriticFlags) {
            assert_eq!(got, want, "exactly one flag must fire");
        }
        let dynamics = CriticVerdict::evaluate(&snapshot(8.0, -3.0, 0.40, -10.0), &t);
        assert_only(
            dynamics.flags,
            CriticFlags { dynamics_collapsed: true, ..CriticFlags::default() },
        );
        assert!((dynamics.dynamics_score - 5.0).abs() < 1e-9, "5 dB under a 1 dB tolerance");

        let harsh = CriticVerdict::evaluate(&snapshot(14.0, 2.0, 0.40, -10.0), &t);
        assert_only(
            harsh.flags,
            CriticFlags { spectral_imbalance: true, ..CriticFlags::default() },
        );

        let rumble = CriticVerdict::evaluate(&snapshot(14.0, -3.0, 0.60, -10.0), &t);
        assert_only(rumble.flags, CriticFlags { sub_rumble: true, ..CriticFlags::default() });

        let quiet = CriticVerdict::evaluate(&snapshot(14.0, -3.0, 0.40, -20.0), &t);
        assert_only(
            quiet.flags,
            CriticFlags { loudness_shortfall: true, ..CriticFlags::default() },
        );

        let blaring = CriticVerdict::evaluate(&snapshot(14.0, -3.0, 0.40, -5.0), &t);
        assert_only(
            blaring.flags,
            CriticFlags { loudness_excess: true, ..CriticFlags::default() },
        );
    }

    #[test]
    fn tolerance_edges_stay_quiet_and_crossings_fire() {
        let t = targets();
        let edge = CriticVerdict::evaluate(&snapshot(13.0, -5.0, 0.45, -12.5), &t);
        assert_eq!(edge.total(), 0.0, "bounds are inclusive");
        let just_out = CriticVerdict::evaluate(&snapshot(13.0, -5.01, 0.45, -12.5), &t);
        assert!(just_out.flags.spectral_imbalance);
    }

    #[test]
    fn nan_metrics_cannot_hide_inside_tolerance() {
        let t = targets();
        let v = CriticVerdict::evaluate(&snapshot(f64::NAN, -3.0, 0.4, -10.0), &t);
        assert!(v.flags.dynamics_collapsed && v.dynamics_score.is_infinite());
    }
}
