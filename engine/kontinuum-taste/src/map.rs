//! DNA → generation mapping (issue #21). The accounting lives in
//! `docs/dna-mapping.md` (every field → a knob, or explicitly unused);
//! this module is the code half of that contract.
//!
//! Zero-network by construction: every function here takes data, never a
//! transport. The playback path (`session_from_dna`, `gen_params_for_dna`)
//! cannot reach the network because nothing it calls knows how.

use kontinuum_compose::arrangement::GenParams;
use kontinuum_compose::reward::ComposerBias;
use kontinuum_compose::taste::TasteProfile;
use kontinuum_preference::priors::TastePriors;

use crate::error::TasteError;

/// DNA → `GenParams`. Delegates to compose's mapping (v1 scalar fields +
/// the swing→groove pin added in v2) so there is exactly one
/// profile→knob path.
pub fn gen_params_for_dna(profile: &TasteProfile, seed: u64) -> GenParams {
    kontinuum_compose::taste::gen_params_for_taste(profile, seed)
}

/// DNA → a validated session. The playback entry point: no transport in
/// scope, so a taste-layer network call during playback is impossible.
pub fn session_from_dna(profile: &TasteProfile, seed: u64) -> kontinuum_ir::Session {
    kontinuum_compose::taste::session_from_taste(profile, seed)
}

/// Adventurousness → the composer's exploration budget (#24/#26's knob).
/// Mapping mirrors `reward::evaluate` (preference axis → budget
/// 0.1..=0.4): a monogenous catalog pins the floor, a wide one the
/// ceiling. No adventurousness measured → neutral bias.
pub fn composer_bias_for_dna(profile: &TasteProfile) -> ComposerBias {
    let exploration = match profile.adventurousness {
        Some(a) => (0.1 + a * 0.3).clamp(0.1, 0.4),
        None => ComposerBias::default().exploration_budget,
    };
    ComposerBias { energy_delta: 0.0, density_delta: 0.0, bass_energy_delta: 0.0, exploration_budget: exploration }
}

/// DNA → #24's `TastePriors` (the B0/B1/B2 ladder's bounded DNA band).
/// Thin adapter over `TastePriors::from_profile_point`; vocabularies
/// (palettes/grooves) stay with the director, not the profile.
pub fn taste_priors_for_dna(
    profile: &TasteProfile,
    palettes: Vec<u32>,
    grooves: Vec<u16>,
) -> Result<TastePriors, TasteError> {
    TastePriors::from_profile_point(
        profile.bpm,
        profile.energy,
        profile.darkness,
        profile.density,
        palettes,
        grooves,
    )
    .map_err(|e| TasteError::Other(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adventurousness_maps_to_exploration_budget() {
        let mut wide = TasteProfile::default();
        wide.adventurousness = Some(1.0);
        let mut narrow = TasteProfile::default();
        narrow.adventurousness = Some(0.0);
        let unmeasured = TasteProfile::default();
        assert!((composer_bias_for_dna(&wide).exploration_budget - 0.4).abs() < 1e-6);
        assert!((composer_bias_for_dna(&narrow).exploration_budget - 0.1).abs() < 1e-6);
        assert_eq!(
            composer_bias_for_dna(&unmeasured).exploration_budget,
            ComposerBias::default().exploration_budget,
            "unmeasured stays neutral"
        );
    }

    #[test]
    fn priors_expand_inside_the_dna_points() {
        let mut p = TasteProfile::default();
        p.bpm = Some(128.0);
        p.energy = 0.7;
        p.darkness = 0.6;
        p.density = 0.55;
        let priors = taste_priors_for_dna(&p, vec![1, 2], vec![0, 1]).unwrap();
        assert_eq!(priors.bpm, 128.0);
        let in_band = |b: &kontinuum_preference::priors::DnaBand, v: f32| b.lo <= v && v <= b.hi;
        assert!(in_band(&priors.energy, p.energy), "point stays inside its band");
        assert!(in_band(&priors.density, p.density));
        assert!(in_band(&priors.darkness, p.darkness));
        assert_eq!(priors.palettes, vec![1, 2]);
        assert!(taste_priors_for_dna(&TasteProfile::default(), vec![], vec![]).is_ok());
    }

    #[test]
    fn session_from_dna_is_deterministic_and_transport_free() {
        let p = TasteProfile { bpm: Some(126.0), ..Default::default() };
        let a = session_from_dna(&p, 9);
        let b = session_from_dna(&p, 9);
        assert_eq!(serde_json::to_string(&a).unwrap(), serde_json::to_string(&b).unwrap());
        assert!(kontinuum_ir::validate_session(&a).is_ok());
    }
}
