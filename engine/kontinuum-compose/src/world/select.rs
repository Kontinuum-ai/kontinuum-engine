//! Taste-weighted world selection (issue #30: "SessionDirector selects via
//! taste DNA weights"). Pure and deterministic — no RNG: the same profile
//! over the same world set always picks the same world.
//!
//! Two transparent signals, taste.rs convention (auditable, coarse):
//! 1. **Genre keywords** — a taste genre matching one of the world's
//!    `taste_tags` (substring both ways, genre.rs convention) counts
//!    [`KEYWORD_WEIGHT`].
//! 2. **Groove-centroid fit** — the world's groove affinities induce a
//!    centroid on the (energy, darkness) taste plane via per-groove timing
//!    anchors; the closer it sits to the profile, the higher the score.
//!
//! Ties break on the lexicographically smallest id, so equal scores are
//! still deterministic.

use super::SoundWorld;
use crate::taste::TasteProfile;

/// Weight of one genre-keyword match; large enough that a matching world
/// beats any non-matching one unless the groove fit differs by ~1.0.
const KEYWORD_WEIGHT: f32 = 3.0;

/// Timing-signature anchors for the hand-made groove vocabulary
/// ([`crate::groove::ALL`]) on the (energy, darkness) taste plane: what a
/// groove "feels like" independent of any world. Unknown (future corpus)
/// names sit at the neutral center.
fn groove_anchor(name: &str) -> (f32, f32) {
    match name {
        "straight-machine" => (0.60, 0.50),
        "mpc-ish" => (0.55, 0.60),
        "drunk-shuffle" => (0.45, 0.50),
        "pushed-hats" => (0.75, 0.45),
        "laid-back" => (0.35, 0.55),
        "tense" => (0.80, 0.75),
        _ => (0.5, 0.5),
    }
}

/// Affinity-weighted centroid of the world's groove affinities; the
/// neutral center when the world declares none.
fn groove_centroid(world: &SoundWorld) -> (f32, f32) {
    let mut weight = 0.0f32;
    let mut e = 0.0f32;
    let mut d = 0.0f32;
    for (name, affinity) in &world.groove_affinities {
        let (ae, ad) = groove_anchor(name);
        weight += affinity;
        e += ae * affinity;
        d += ad * affinity;
    }
    if weight <= f32::EPSILON {
        (0.5, 0.5)
    } else {
        (e / weight, d / weight)
    }
}

fn keyword_matches(world: &SoundWorld, profile: &TasteProfile) -> usize {
    profile
        .genres
        .iter()
        .filter(|g| {
            let g = g.to_lowercase();
            world.taste_tags.iter().any(|t| {
                let t = t.to_lowercase();
                g.contains(&t) || t.contains(&g)
            })
        })
        .count()
}

/// Selection score of `world` for `profile`. Higher wins; the scale is
/// only meaningful for comparing worlds against each other.
pub fn taste_affinity(world: &SoundWorld, profile: &TasteProfile) -> f32 {
    let (ce, cd) = groove_centroid(world);
    let de = profile.energy.clamp(0.0, 1.0) - ce;
    let dd = profile.darkness.clamp(0.0, 1.0) - cd;
    let fit = (1.0 - (de * de + dd * dd)).max(0.0);
    keyword_matches(world, profile) as f32 * KEYWORD_WEIGHT + fit
}

/// Deterministic pick: the highest-scoring world, ties broken by the
/// lexicographically smallest id. `None` only for an empty world set.
pub fn select_world<'a>(worlds: &'a [SoundWorld], profile: &TasteProfile) -> Option<&'a SoundWorld> {
    worlds.iter().max_by(|a, b| {
        taste_affinity(a, profile)
            .total_cmp(&taste_affinity(b, profile))
            // Reversed id order: `max_by` yields the last maximum, so on a
            // tie this keeps the lexicographically smallest id.
            .then_with(|| b.id.cmp(&a.id))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world(id: &str, tags: &[&str], grooves: &[(&str, f32)]) -> SoundWorld {
        SoundWorld {
            format_version: crate::world::WORLD_FORMAT_VERSION,
            id: super::super::SoundWorldId(id.into()),
            name: id.into(),
            description: String::new(),
            taste_tags: tags.iter().map(|t| (*t).into()).collect(),
            palette_overrides: Default::default(),
        patch_overrides: Default::default(),
        sample_packs: Default::default(),
            mix_target_overrides: Default::default(),
            groove_affinities: grooves
                .iter()
                .map(|(n, a)| ((*n).to_string(), *a))
                .collect(),
        }
    }

    fn profile(genres: &[&str], energy: f32, darkness: f32) -> TasteProfile {
        TasteProfile {
            genres: genres.iter().map(|g| (*g).into()).collect(),
            energy,
            darkness,
            ..TasteProfile::default()
        }
    }

    #[test]
    fn keywords_dominate_but_grooves_break_keyword_ties() {
        let soft = world("soft", &["micro"], &[("laid-back", 1.0)]);
        let hard = world("hard", &["techno"], &[("tense", 1.0)]);
        let micro = profile(&["microhouse"], 0.9, 0.9);
        assert_eq!(select_world(&[soft.clone(), hard.clone()], &micro).map(|w| w.id.0.as_str()), Some("soft"));
        // Both match "techno"-flavored genres: the tense (dark, driving)
        // centroid fits the dark profile better than laid-back.
        let dark_techno = profile(&["dub techno"], 0.75, 0.95);
        assert_eq!(
            select_world(&[soft.clone(), hard], &dark_techno).map(|w| w.id.0.as_str()),
            Some("hard")
        );
    }

    #[test]
    fn selection_is_deterministic_and_order_free() {
        let a = world("alpha", &["techno"], &[("pushed-hats", 0.8), ("straight-machine", 0.7)]);
        let b = world("beta", &["micro"], &[("drunk-shuffle", 0.9)]);
        let p = profile(&["techno"], 0.7, 0.6);
        let one = select_world(&[a.clone(), b.clone()], &p).map(|w| w.id.0.clone());
        let two = select_world(&[b, a], &p).map(|w| w.id.0.clone());
        assert_eq!(one, two, "slice order must not change the pick");
        assert_eq!(one, Some("alpha".to_string()));
    }

    #[test]
    fn id_ties_pick_the_lexicographically_smallest() {
        let a = world("aaa", &["techno"], &[]);
        let b = world("zzz", &["techno"], &[]);
        let p = profile(&["techno"], 0.5, 0.5);
        assert_eq!(select_world(&[b, a], &p).map(|w| w.id.0.as_str()), Some("aaa"));
    }

    #[test]
    fn empty_world_set_picks_none() {
        let p = profile(&["techno"], 0.5, 0.5);
        assert!(select_world(&[], &p).is_none());
    }
}
