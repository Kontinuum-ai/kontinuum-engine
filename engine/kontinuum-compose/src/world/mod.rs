//! Sound worlds (issue #30, "Sound worlds"): curated, timbre-coherent
//! parameter sets that layer **on top of** the genre/palette selection —
//! never instead of it. A world is a user-facing palette coordinate: it
//! re-voices the existing four-track rig by overriding parameters that
//! [`crate::palette`] and [`crate::genre`] already expose, and (v2) can
//! swap a rig slot onto the sound roster v2 engines outright via
//! [`SoundWorld::patch_overrides`] — a full IR instrument per track.
//!
//! Worlds are authored as versioned JSON or the TOML subset (see
//! `compose/fixtures/worlds/` and [`load_toml`]), selected through taste
//! weights ([`select`]), and switched mid-session with a section-boundary
//! crossfade ([`morph`]).
//!
//! Layering contract: with no world the session is byte-identical to the
//! pre-world generator; with one, [`SoundWorld::apply_to_tracks`] runs
//! after `palette::tracks_for_genre`, so genre staging happens first and
//! the world overrides win on the fields they name. Order inside
//! `apply_to_tracks`: patch swap (kind replacement) → palette parameters
//! (v0 kinds only; stands down on swapped kinds) → mix targets.

use std::collections::BTreeMap;

use kontinuum_ir::schema::{InstrumentDef, Track, Wave};
use serde::{Deserialize, Serialize};

pub mod load;
pub mod load_toml;
pub mod morph;
pub mod select;

pub use load::{load_json, WorldError, WORLD_FORMAT_VERSION};
pub use load_toml::load_toml;
pub use morph::{morph_world, MorphError};
pub use select::{select_world, taste_affinity};

/// Strongly-typed world identifier (serde-transparent string).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SoundWorldId(pub String);

/// A curated sound world: overrides keyed by rig track id
/// (`kick` / `perc` / `bass` / `pad`), groove affinities over the hand-made
/// vocabulary ([`crate::groove::ALL`]), and taste tags for selection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SoundWorld {
    /// Must equal [`WORLD_FORMAT_VERSION`] (checked on load).
    pub format_version: u32,
    pub id: SoundWorldId,
    pub name: String,
    /// Free-form, user-facing description of the world's character.
    pub description: String,
    /// Lowercase genre keywords (taste.rs vocabulary) this world serves;
    /// the selection hook in [`select`].
    #[serde(default)]
    pub taste_tags: Vec<String>,
    /// Synth-parameter overrides per track id, on top of the genre rig.
    #[serde(default)]
    pub palette_overrides: BTreeMap<String, VoiceOverride>,
    /// Whole-instrument patches per rig track id (#30 roster v2): the value
    /// replaces the track's instrument outright, so a world can put the
    /// wavetable / FM-perc / texture engines on the rig. Validated with the
    /// same instrument bounds as sessions.
    #[serde(default)]
    pub patch_overrides: BTreeMap<String, InstrumentDef>,
    /// Sample-pack subset (issue #30 sample library v2): pack ids from the
    /// expansion library this world draws from. Curation metadata for the
    /// resolver; unknown ids degrade to the base library at load time.
    #[serde(default)]
    pub sample_packs: Vec<String>,
    /// Mix-target overrides (gain / pan / sends) per track id.
    #[serde(default)]
    pub mix_target_overrides: BTreeMap<String, MixTargetOverride>,
    /// Affinity 0..=1 per groove-template name ([`crate::groove::ALL`]);
    /// the taste-coordinate used by selection.
    #[serde(default)]
    pub groove_affinities: BTreeMap<String, f32>,
}

/// Which voice of the rig an override targets; must match the track id it
/// is keyed under (checked on load).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceTag {
    Kick,
    Perc,
    Bass,
    Pad,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KickOverride {
    pub voice: VoiceTag,
    #[serde(default)]
    pub tune_hz: Option<f32>,
    #[serde(default)]
    pub decay_ms: Option<f32>,
    #[serde(default)]
    pub click: Option<f32>,
    #[serde(default)]
    pub drive: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PercOverride {
    pub voice: VoiceTag,
    #[serde(default)]
    pub decay_ms: Option<f32>,
    #[serde(default)]
    pub tone: Option<f32>,
    #[serde(default)]
    pub open: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BassOverride {
    pub voice: VoiceTag,
    #[serde(default)]
    pub cutoff_hz: Option<f32>,
    #[serde(default)]
    pub resonance: Option<f32>,
    #[serde(default)]
    pub wave: Option<Wave>,
    #[serde(default)]
    pub glide_ms: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PadOverride {
    pub voice: VoiceTag,
    #[serde(default)]
    pub attack_ms: Option<f32>,
    #[serde(default)]
    pub release_ms: Option<f32>,
    #[serde(default)]
    pub detune_cents: Option<f32>,
    #[serde(default)]
    pub cutoff_hz: Option<f32>,
}

/// Untagged with an explicit `voice` discriminant (schema.rs convention:
/// internally-tagged enums cannot carry `deny_unknown_fields`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum VoiceOverride {
    Kick(KickOverride),
    Perc(PercOverride),
    Bass(BassOverride),
    Pad(PadOverride),
}

/// Mix-target overrides for one track; `None` leaves the genre value.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MixTargetOverride {
    #[serde(default)]
    pub gain: Option<f32>,
    #[serde(default)]
    pub pan: Option<f32>,
    #[serde(default)]
    pub send_delay: Option<f32>,
    #[serde(default)]
    pub send_reverb: Option<f32>,
}

/// Effective mix values of one track (what a morph crossfades between).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MixValues {
    pub gain: f32,
    pub pan: f32,
    pub send_delay: f32,
    pub send_reverb: f32,
}

impl MixValues {
    pub(crate) fn of(track: &Track) -> MixValues {
        MixValues {
            gain: track.gain,
            pan: track.pan,
            send_delay: track.sends.delay,
            send_reverb: track.sends.reverb,
        }
    }
}

/// Applies the world's overrides on top of the (already genre-staged) rig.
/// Order: patch swap → voice parameters → mix targets. A voice-parameter
/// override whose kind no longer matches (because the patch swapped it)
/// stands down: the patch's own params win.
pub fn apply_to_tracks(tracks: &mut [Track], world: &SoundWorld) {
    for track in tracks.iter_mut() {
        if let Some(patch) = world.patch_overrides.get(&track.id) {
            track.instrument = patch.clone();
        }
        if let Some(o) = world.palette_overrides.get(&track.id) {
            apply_voice(track, *o);
        }
        if let Some(m) = world.mix_target_overrides.get(&track.id) {
            apply_mix(track, m);
        }
    }
}

fn apply_voice(track: &mut Track, o: VoiceOverride) {
    match (&mut track.instrument, o) {
        (InstrumentDef::Kick(k), VoiceOverride::Kick(o)) => {
            if let Some(v) = o.tune_hz {
                k.tune_hz = v;
            }
            if let Some(v) = o.decay_ms {
                k.decay_ms = v;
            }
            if let Some(v) = o.click {
                k.click = v;
            }
            if let Some(v) = o.drive {
                k.drive = v;
            }
        }
        (InstrumentDef::Hat(h), VoiceOverride::Perc(o)) => {
            if let Some(v) = o.decay_ms {
                h.decay_ms = v;
            }
            if let Some(v) = o.tone {
                h.tone = v;
            }
            if let Some(v) = o.open {
                h.open = v;
            }
        }
        (InstrumentDef::Bass(b), VoiceOverride::Bass(o)) => {
            if let Some(v) = o.cutoff_hz {
                b.cutoff_hz = v;
            }
            if let Some(v) = o.resonance {
                b.resonance = v;
            }
            if let Some(v) = o.wave {
                b.wave = v;
            }
            if let Some(v) = o.glide_ms {
                b.glide_ms = v;
            }
        }
        (InstrumentDef::Pad(p), VoiceOverride::Pad(o)) => {
            if let Some(v) = o.attack_ms {
                p.attack_ms = v;
            }
            if let Some(v) = o.release_ms {
                p.release_ms = v;
            }
            if let Some(v) = o.detune_cents {
                p.detune_cents = v;
            }
            if let Some(v) = o.cutoff_hz {
                p.cutoff_hz = v;
            }
        }
        // Voice/kind mismatch: the genre rig owns the kind, the world
        // stands down on this track.
        _ => {}
    }
}

fn apply_mix(track: &mut Track, m: &MixTargetOverride) {
    if let Some(v) = m.gain {
        track.gain = v;
    }
    if let Some(v) = m.pan {
        track.pan = v;
    }
    if let Some(v) = m.send_delay {
        track.sends.delay = v;
    }
    if let Some(v) = m.send_reverb {
        track.sends.reverb = v;
    }
}
