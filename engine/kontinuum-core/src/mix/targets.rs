//! Mix targets and the telemetry contract (issue #27): the role → loudness
//! target table relative to the kick anchor, and the per-tile snapshot the
//! engine posts to supervision. Serialization mirrors the mastering crate's
//! telemetry so both chains report through one supervision path.

use serde::{Deserialize, Serialize};

use crate::MAX_TRACKS;

/// Mix roles and their loudness targets, relative to the kick anchor (dB).
///
/// The shipped values are **hypotheses** from published minimal-techno
/// engineering practice (issue #27's ranges, midpoints taken): kick = anchor,
/// bass −2…0 → −1, perc bed −6…−3 → −4.5, pads −10…−6 → −8. They stay
/// hypothesis-flagged until the #23 corpus gives measured per-role targets;
/// the versioned `mix-targets-{subgenre}.toml` lands with the compose-side
/// wiring and must not silently drift from these defaults.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MixRole {
    #[default]
    Unassigned,
    Kick,
    Bass,
    Perc,
    Pad,
}

impl MixRole {
    /// Target short-term level relative to the kick anchor (dB).
    pub fn target_db(self) -> f32 {
        match self {
            MixRole::Kick => 0.0,
            MixRole::Bass => -1.0,
            MixRole::Perc => -4.5,
            MixRole::Pad => -8.0,
            MixRole::Unassigned => 0.0,
        }
    }

    /// Tolerance around the target (dB) — the servo's deadband is tighter.
    pub const TOLERANCE_DB: f32 = 1.5;

    /// Kick-sidechain duck depth (fraction to unity) a role defaults to —
    /// issue #76. Full range to unity stays available per track via
    /// `AutoMixer::set_duck_depth`; these are the starting points:
    ///
    /// - Bass 0.9 — the reference patch ducks the bass line to (almost)
    ///   zero (`duckdepth(1)`); a −20 dB floor keeps a trace of the sub for
    ///   phrase continuity while the #27 bass carve does the precise
    ///   30–120 Hz collision work underneath.
    /// - Pad 0.85 — sustained harmonic content is the main mid-band
    ///   masker; a deep duck is what makes the pump audible (the issue's
    ///   17 dB reference range is unreachable without it).
    /// - Perc 0.5 — hats/claps/snare are transients living between kicks;
    ///   −6 dB tucks the bed under the kick without swallowing the offbeat
    ///   groove that defines it.
    /// - Kick 0.0 — the key source must not duck itself.
    /// - Unassigned 0.0 — no role, no duck (bypass until assigned).
    pub fn duck_depth(self) -> f32 {
        match self {
            MixRole::Kick => 0.0,
            MixRole::Bass => 0.9,
            MixRole::Perc => 0.5,
            MixRole::Pad => 0.85,
            MixRole::Unassigned => 0.0,
        }
    }

    /// Which bus the role sums to.
    pub fn bus(self) -> BusSide {
        match self {
            MixRole::Kick | MixRole::Perc => BusSide::Drums,
            MixRole::Bass | MixRole::Pad => BusSide::Harmonic,
            MixRole::Unassigned => BusSide::Drums,
        }
    }
}

/// Bus identity for compose-side wiring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BusSide {
    Drums,
    Harmonic,
}

/// Snapshot of the auto-mix state after a processed tile (#27 → #25
/// supervision). Copy-able, Serde-serializable, updated per tile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MixTelemetry {
    /// Per-track gain-staging correction (dB, ±6 clamp).
    pub track_gain_db: [f32; MAX_TRACKS],
    /// Bass dynamic node cut (dB, 0 = idle).
    pub bass_cut_db: f32,
    /// Per-track mask carve, worst node (dB, 0 = idle).
    pub mask_cut_db: [f32; MAX_TRACKS],
    /// Drum bus compressor reduction (dB).
    pub drum_gr_db: f32,
    /// Harmonic bus compressor reduction (dB).
    pub harmonic_gr_db: f32,
    /// Latched: any track's servo pinned at its ±6 dB bound — the mix
    /// cannot reach its targets, the composer should look at the arrangement.
    pub any_gain_at_bound: bool,
    pub bass_node_active: bool,
    pub mask_active: bool,
    /// Processed tile counter (deterministic bookkeeping).
    pub tiles: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_targets_stay_inside_the_issue_27_hypothesis_ranges() {
        assert!((-2.0..=0.0).contains(&MixRole::Bass.target_db()));
        assert!((-6.0..=-3.0).contains(&MixRole::Perc.target_db()));
        assert!((-10.0..=-6.0).contains(&MixRole::Pad.target_db()));
        assert_eq!(MixRole::Kick.target_db(), 0.0);
    }

    #[test]
    fn roles_route_to_buses() {
        assert_eq!(MixRole::Kick.bus(), BusSide::Drums);
        assert_eq!(MixRole::Perc.bus(), BusSide::Drums);
        assert_eq!(MixRole::Bass.bus(), BusSide::Harmonic);
        assert_eq!(MixRole::Pad.bus(), BusSide::Harmonic);
    }

    #[test]
    fn telemetry_defaults_are_all_idle() {
        let tel = MixTelemetry::default();
        assert!(tel.track_gain_db.iter().all(|g| g == &0.0));
        assert_eq!(tel.bass_cut_db, 0.0);
        assert!(!tel.any_gain_at_bound);
        assert!(!tel.bass_node_active);
        assert!(!tel.mask_active);
    }

    #[test]
    fn duck_depth_defaults_are_role_keyed_inside_full_range() {
        assert_eq!(MixRole::Bass.duck_depth(), 0.9);
        assert_eq!(MixRole::Pad.duck_depth(), 0.85);
        assert_eq!(MixRole::Perc.duck_depth(), 0.5);
        assert_eq!(MixRole::Kick.duck_depth(), 0.0);
        assert_eq!(MixRole::Unassigned.duck_depth(), 0.0);
        for depth in [MixRole::Kick, MixRole::Bass, MixRole::Perc, MixRole::Pad, MixRole::Unassigned]
            .map(MixRole::duck_depth)
        {
            assert!((0.0..=1.0).contains(&depth));
        }
    }
}
