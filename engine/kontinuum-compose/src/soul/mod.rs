//! Creative Souls (issue #55, renamed from the issue's working name
//! "Signatures"): shareable artist/producer/theme identity packs — the
//! composer's equivalent of LLM skills. One soul = a versioned JSON pack
//! (see `compose/fixtures/souls/`) whose layers are exactly the plan's
//! validated data types:
//!
//! | Layer | Mechanism |
//! |---|---|
//! | style card | prose prompt fragment for the composer (token-budgeted at blend time) |
//! | harmony vocabulary | looping `Chord` progressions ([`crate::harmony::Chord`]) |
//! | rhythm & groove | template refs + swing/jitter + affinities ([`crate::groove::ALL`]) |
//! | rack & sound | palette overrides (world [`crate::world`] types, reused verbatim) |
//! | sample palette | recipe hashes + queries (#53 — never audio) |
//! | mix profile | concrete per-track band-share targets |
//! | arrangement shapes | dev/breakdown lengths + energy arc |
//! | spatial | reserved for #54 (no engine code yet) |
//!
//! A genre is a soul at genre scope ([`SoulKind::Genre`]); the eight
//! first-party packs in `fixtures/souls/` dogfood the format. Layering
//! contract: genre staging runs first, then the blended soul layer, then an
//! explicit [`crate::world::SoundWorld`] (a later, user-level choice wins
//! on the fields it names), then user diffs — and the validator applies
//! regardless, so a soul can never break the engine.
//!
//! Blending is deterministic and layer-wise (issue #55): prompt fragments
//! concatenate by weight-ranked budget; tables and rack layers resolve to
//! the dominant (highest-weight) soul; mix profiles interpolate by
//! normalized weight; groove affinities weight-average. Ties break by stack
//! order.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub mod blend;
pub mod era;
pub mod load;
pub mod naming;

pub use blend::{
    blend, prepare, BlendError, BlendInput, BlendedSoul, SoulPrepared, SoulStackEntry,
    STYLE_CARD_WORD_BUDGET,
};
pub use era::{set_era, SoulSwitchError};
pub use load::{load_json, SoulError, DEFAULT_ERA, SOUL_FORMAT_VERSION};
pub use naming::{check_shareable_name, NamingError};

/// Strongly-typed soul identifier (serde-transparent string).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SoulId(pub String);

/// What the pack is a soul *of* (issue #55: artist signature, producer/DJ
/// theme, scene, mood-world — and genre, for the first-party unification).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SoulKind {
    Genre,
    Artist,
    Theme,
    Scene,
    Mood,
}

/// A Creative Soul pack: identity metadata plus named eras of layers.
/// The `"default"` era is required (checked at load); any other era may
/// override a subset of layers and falls back to the default per layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreativeSoul {
    /// Must equal [`SOUL_FORMAT_VERSION`] (checked on load).
    pub format_version: u32,
    pub id: SoulId,
    pub name: String,
    /// Free-form, user-facing description ("dusty microhouse, Perlon-school").
    pub description: String,
    pub kind: SoulKind,
    /// Lowercase genre keywords (taste.rs vocabulary) this soul serves.
    #[serde(default)]
    pub taste_tags: Vec<String>,
    /// Named eras/phases; each resolves to a full layer-set via default fallback.
    pub eras: BTreeMap<String, SoulLayers>,
}

/// The layer-set of one era. Every layer is optional so an era can override
/// a subset; resolution fills gaps from the "default" era.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SoulLayers {
    pub style_card: Option<String>,
    pub harmony: Option<SoulHarmony>,
    pub groove: Option<SoulGroove>,
    pub rack: Option<SoulRack>,
    pub samples: Option<SoulSamples>,
    pub mix: Option<SoulMix>,
    pub arrangement: Option<SoulArrangement>,
    /// Reserved for the spatial layout layer (#54); opaque in this format
    /// version so authored packs survive the engine landing it.
    pub spatial: Option<serde_json::Value>,
}

impl SoulLayers {
    /// Era resolution: `over` wins per layer, `self` (the default era)
    /// fills the gaps.
    pub fn merged(&self, over: &SoulLayers) -> SoulLayers {
        SoulLayers {
            style_card: over.style_card.clone().or_else(|| self.style_card.clone()),
            harmony: over.harmony.clone().or_else(|| self.harmony.clone()),
            groove: over.groove.clone().or_else(|| self.groove.clone()),
            rack: over.rack.clone().or_else(|| self.rack.clone()),
            samples: over.samples.clone().or_else(|| self.samples.clone()),
            mix: over.mix.clone().or_else(|| self.mix.clone()),
            arrangement: over.arrangement.clone().or_else(|| self.arrangement.clone()),
            spatial: over.spatial.clone().or_else(|| self.spatial.clone()),
        }
    }
}

/// Harmony vocabulary: looping progressions of absolute-MIDI chords; the
/// seed picks the table and rotation, never hard-pinning a take.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SoulHarmony {
    pub progressions: Vec<Vec<crate::harmony::Chord>>,
}

/// Groove identity: a template pin from the hand-made six, swing/jitter
/// character, and taste affinities over the vocabulary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SoulGroove {
    #[serde(default)]
    pub template: Option<String>,
    #[serde(default)]
    pub swing: Option<f32>,
    #[serde(default)]
    pub jitter_ticks: Option<f32>,
    #[serde(default)]
    pub affinities: BTreeMap<String, f32>,
}

/// Rack & sound: parameter-space overrides on the rig, reusing the world
/// override types (voice-tag matching and IR bounds checked at load).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SoulRack {
    pub palette_overrides: BTreeMap<String, crate::world::VoiceOverride>,
}

/// Sample palette as recipes (#19/#53): deterministic re-derivation, no
/// audio embedded — hashes point at rendered recipe results, queries at the
/// store catalog.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SoulSamples {
    pub recipe_hashes: Vec<u64>,
    pub queries: Vec<String>,
}

/// Mix profile: concrete band-share targets per rig track id
/// (kick/perc/bass/pad); blending interpolates these by normalized weight.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SoulMix {
    pub profile: BTreeMap<String, SoulMixTarget>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SoulMixTarget {
    pub gain: f32,
    pub pan: f32,
    pub send_delay: f32,
    pub send_reverb: f32,
}

/// Arrangement shapes (#16/#23): section-length medians and a normalized
/// energy arc the planner samples when corpus artifacts are absent.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SoulArrangement {
    pub dev_bars: Option<u32>,
    pub breakdown_bars: Option<u32>,
    pub energy_arc: Option<Vec<f32>>,
}

/// Applies the blended soul layer to the (already genre-staged) rig.
/// Palette overrides reuse the world stand-down rule — an override whose
/// voice does not match the track's instrument kind is skipped. Mix-profile
/// targets apply statically here; era switching mid-session crossfades via
/// [`era::set_era`] instead.
pub fn apply_to_tracks(tracks: &mut [kontinuum_ir::schema::Track], blended: &BlendedSoul) {
    let synth = crate::world::SoundWorld {
        format_version: crate::world::WORLD_FORMAT_VERSION,
        id: crate::world::SoundWorldId(String::new()),
        name: String::new(),
        description: String::new(),
        taste_tags: Vec::new(),
        palette_overrides: blended.palette_overrides.clone(),
        patch_overrides: std::collections::BTreeMap::new(),
        sample_packs: Vec::new(),
        mix_target_overrides: blended
            .mix_profile
            .iter()
            .map(|(id, t)| {
                (
                    id.clone(),
                    crate::world::MixTargetOverride {
                        gain: Some(t.gain),
                        pan: Some(t.pan),
                        send_delay: Some(t.send_delay),
                        send_reverb: Some(t.send_reverb),
                    },
                )
            })
            .collect(),
        groove_affinities: blended.groove_affinities.clone(),
    };
    crate::world::apply_to_tracks(tracks, &synth);
}
