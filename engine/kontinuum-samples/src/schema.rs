//! Sample recipe schema (issue #53, step 1): a sample pack is data — which
//! voices, which processing chains, how to slice and tag — rendered
//! deterministically. Recipe + seed always yields bit-identical samples, so
//! packs are reproducible build artifacts, fully self-owned, zero licensing
//! risk.
//!
//! Strictness mirrors the IR: `deny_unknown_fields` everywhere, numeric
//! bounds checked by [`validate`], voice references resolved before render.

use serde::{Deserialize, Serialize};

pub const RECIPE_VERSION: u32 = 1;

/// Documented bounds for validated recipe fields.
pub mod bounds {
    pub const VELOCITY: (f32, f32) = (0.0, 1.0);
    pub const PITCH: (f32, f32) = (0.0, 127.0);
    /// Hit position cap (10 minutes of material).
    pub const AT_MS: (f32, f32) = (0.0, 600_000.0);
    pub const TAIL_MS: (f32, f32) = (50.0, 10_000.0);
    pub const CHAIN_STEPS: usize = 4;
    pub const SENSITIVITY: (f32, f32) = (0.0, 1.0);
    pub const MAX_SLICES: (u32, u32) = (1, 64);
    /// Choke group ids start at 1; 0 would read as "no choke".
    pub const CHOKE_GROUP: (f32, f32) = (1.0, 255.0);
    pub const STRETCH_FACTOR: (f32, f32) = (0.25, 4.0);
    pub const MICROTIMING_MS: (f32, f32) = (0.0, 40.0);
    pub const HUMANIZE_GAIN_DB: (f32, f32) = (0.0, 12.0);
    pub const HUMANIZE_PITCH_CENTS: (f32, f32) = (0.0, 100.0);
    pub const ALTERNATE_PROBABILITY: (f32, f32) = (0.0, 1.0);
    pub const GRAIN_MS: (f32, f32) = (20.0, 200.0);
    pub const GRAIN_DENSITY: (f32, f32) = (1.0, 200.0);
    pub const GRAIN_SPRAY_MS: (f32, f32) = (0.0, 1000.0);
    pub const GRAIN_PITCH_JITTER: (f32, f32) = (0.0, 1200.0);
    pub const GRAIN_LEVEL: (f32, f32) = (0.0, 1.0);
    pub const GRAIN_PITCH: (f32, f32) = (0.0, 127.0);
    pub const GRAIN_VELOCITY: (f32, f32) = (0.0, 1.0);
}

#[derive(Debug, thiserror::Error)]
pub enum RecipeError {
    #[error("voice reference not found: {0}")]
    UnknownVoice(String),
    #[error("{field} {value} outside bounds {lo}..{hi}")]
    OutOfBounds {
        field: &'static str,
        value: f64,
        lo: f64,
        hi: f64,
    },
    #[error("recipe version {found} unsupported (want {RECIPE_VERSION})")]
    Version { found: u32 },
}

/// A sample pack recipe: the entire creative content of a pack as data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SampleRecipe {
    pub version: u32,
    pub seed: u64,
    #[serde(default = "d_sample_rate")]
    pub sample_rate: u32,
    pub name: String,
    /// The instruments the pack's hits draw from. Reuses the IR instrument
    /// schema — one vocabulary for every synthesised sound in the product.
    pub voices: Vec<RecipeVoice>,
    /// Explicit hit list. Order in the document is render order.
    pub hits: Vec<RecipeHit>,
    /// Post-render slicing instructions; omit for single-sample packs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slice: Option<SliceSpec>,
    /// Render tail in ms past the last hit so decays finish (default 1000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail_ms: Option<f32>,
    /// Optional granular texture bed layered over the whole pack (#19).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub texture: Option<crate::granular::GrainSpec>,
    /// Free-form genre/timbre tags carried onto the rendered pack.
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipeVoice {
    pub id: String,
    pub instrument: kontinuum_ir::InstrumentDef,
    /// Processing chain applied to each hit's buffer, in order.
    #[serde(default)]
    pub chain: Vec<ChainStep>,
}

/// Per-hit processing: `drive` (tanh saturation) and `filter` (SVF LP/HP).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainStep {
    #[serde(rename = "type")]
    pub kind: ChainKind,
    #[serde(default = "d_amount")]
    pub amount: f32,
    /// Wet/dry blend 0..1.
    #[serde(default = "d_mix")]
    pub mix: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChainKind {
    Drive,
    Lowpass,
    Highpass,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipeHit {
    /// Voice id — must exist in `voices`.
    pub voice: String,
    /// Hit position in milliseconds from the pack start.
    pub at_ms: f32,
    #[serde(default = "d_pitch")]
    pub pitch: f32,
    #[serde(default = "d_velocity")]
    pub velocity: f32,
    /// Choke group (1..): triggering this hit fast-fades other sounding
    /// voices in the same group. Omitted = never chokes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choke_group: Option<u8>,
    /// Per-slot expression (velocity layers/curve, round-robin, alternates,
    /// microtiming, humanize). Omitted = plain render.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expression: Option<crate::expr::HitExpression>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SliceSpec {
    /// "transient" (onset detection) or "fixed_ms".
    pub mode: SliceMode,
    #[serde(default = "d_max_slices")]
    pub max_slices: u32,
    #[serde(default = "d_sensitivity")]
    pub sensitivity: f32,
    /// Interval for `fixed_ms` slicing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_ms: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SliceMode {
    Transient,
    FixedMs,
}

/// The rendered, self-describing artifact.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderedSample {
    pub pcm: Vec<f32>,
    pub sample_rate: u32,
    /// Slice start frames (always includes 0).
    pub slices: Vec<usize>,
    pub tags: Vec<String>,
    /// FNV-1a over the canonical recipe JSON: the session-facing identity
    /// that replaces sample file ids (#53 step 2).
    pub hash: u64,
}

fn d_sample_rate() -> u32 {
    48_000
}
fn d_pitch() -> f32 {
    60.0
}
fn d_velocity() -> f32 {
    0.8
}
fn d_amount() -> f32 {
    1.0
}
fn d_mix() -> f32 {
    1.0
}
fn d_max_slices() -> u32 {
    8
}
fn d_sensitivity() -> f32 {
    0.5
}

pub(crate) fn check(field: &'static str, value: f32, (lo, hi): (f32, f32)) -> Result<(), RecipeError> {
    if value.is_finite() && value >= lo && value <= hi {
        Ok(())
    } else {
        Err(RecipeError::OutOfBounds { field, value: f64::from(value), lo: f64::from(lo), hi: f64::from(hi) })
    }
}

/// L2 validation: bounds and reference resolution. L1 strictness is serde.
pub fn validate(recipe: &SampleRecipe) -> Result<(), RecipeError> {
    if recipe.version != RECIPE_VERSION {
        return Err(RecipeError::Version { found: recipe.version });
    }
    for voice in &recipe.voices {
        for (i, step) in voice.chain.iter().enumerate() {
            if step.amount.is_nan() || step.mix.is_nan() {
                return Err(RecipeError::OutOfBounds {
                    field: "chain param",
                    value: f64::NAN,
                    lo: 0.0,
                    hi: 0.0,
                });
            }
            let _ = i;
        }
        if voice.chain.len() > bounds::CHAIN_STEPS {
            return Err(RecipeError::OutOfBounds {
                field: "chain length",
                value: voice.chain.len() as f64,
                lo: 0.0,
                hi: bounds::CHAIN_STEPS as f64,
            });
        }
    }
    let known: Vec<&str> = recipe.voices.iter().map(|v| v.id.as_str()).collect();
    for hit in &recipe.hits {
        if !known.contains(&hit.voice.as_str()) {
            return Err(RecipeError::UnknownVoice(hit.voice.clone()));
        }
        check("velocity", hit.velocity, bounds::VELOCITY)?;
        check("pitch", hit.pitch, bounds::PITCH)?;
        check("at_ms", hit.at_ms, bounds::AT_MS)?;
        if let Some(group) = hit.choke_group {
            let (lo, hi) = bounds::CHOKE_GROUP;
            if f32::from(group) < lo || f32::from(group) > hi {
                return Err(RecipeError::OutOfBounds {
                    field: "choke_group",
                    value: f64::from(group),
                    lo: f64::from(lo),
                    hi: f64::from(hi),
                });
            }
        }
        if let Some(expr) = &hit.expression {
            crate::expr::validate_expression(expr)?;
        }
    }
    if let Some(texture) = &recipe.texture {
        crate::granular::validate_grain(texture, &known)?;
    }
    if let Some(slice) = &recipe.slice {
        check("sensitivity", slice.sensitivity, bounds::SENSITIVITY)?;
        let (lo, hi) = bounds::MAX_SLICES;
        if slice.max_slices < lo || slice.max_slices > hi {
            return Err(RecipeError::OutOfBounds {
                field: "max_slices",
                value: f64::from(slice.max_slices),
                lo: f64::from(lo),
                hi: f64::from(hi),
            });
        }
        if slice.mode == SliceMode::FixedMs {
            check("interval_ms", slice.interval_ms.unwrap_or(0.0), (1.0, bounds::AT_MS.1))?;
        }
    }
    Ok(())
}

/// FNV-1a over the canonical (serde) serialization of the recipe — the
/// session-facing pack identity.
pub fn recipe_hash(recipe: &SampleRecipe) -> u64 {
    fn fnv1a(bytes: &[u8]) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    }
    fnv1a(serde_json::to_string(recipe).unwrap_or_default().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recipe() -> SampleRecipe {
        serde_json::from_str(
            r#"{
            "version": 1, "seed": 42, "name": "kit",
            "voices": [{"id": "cl", "instrument": {"kind": "hat"}}],
            "hits": [{"voice": "cl", "at_ms": 0.0}],
            "tags": ["test"]
        }"#,
        )
        .expect("parse")
    }

    #[test]
    fn validates_clean_and_rejects_unknown_voice() {
        assert!(validate(&recipe()).is_ok());
        let mut bad = recipe();
        bad.hits[0].voice = "ghost".into();
        assert!(matches!(validate(&bad), Err(RecipeError::UnknownVoice(g)) if g == "ghost"));
    }

    #[test]
    fn bounds_are_enforced() {
        let mut bad = recipe();
        bad.hits[0].velocity = 5.0;
        assert!(matches!(validate(&bad), Err(RecipeError::OutOfBounds { field: "velocity", .. })));
        bad = recipe();
        bad.hits[0].at_ms = -1.0;
        assert!(validate(&bad).is_err());
    }

    #[test]
    fn unknown_fields_rejected() {
        let bad: Result<SampleRecipe, _> =
            serde_json::from_str(r#"{"version":1,"seed":1,"name":"x","voices":[],"hits":[],"vibe":1}"#);
        assert!(bad.is_err());
    }

    #[test]
    fn hash_tracks_content() {
        let a = recipe_hash(&recipe());
        let mut b = recipe();
        b.name = "other".into();
        assert_ne!(a, recipe_hash(&b));
        assert_eq!(a, recipe_hash(&recipe()), "stable across runs");
    }
}
