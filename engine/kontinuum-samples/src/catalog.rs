//! Sample catalog (#20 engine side): the search corpus the slot resolver
//! picks from. Each entry carries hand-engineered features extracted by the
//! build pipeline (duration, spectral centroid, flatness, pitch, transient
//! sharpness, loudness) plus its class, pack, and free-form tags.
//!
//! Strictness mirrors the IR and the recipe schema: `deny_unknown_fields`,
//! and feature bounds validated once at load ([`parse_catalog`]) so the
//! query executor can trust them downstream.

use serde::{Deserialize, Serialize};

/// Version of the catalog + scoring pipeline. Bumped when feature extraction
/// or scoring changes in a way that invalidates existing pins: re-resolution
/// happens only on an explicit version bump, never implicitly (see
/// [`crate::query::resolve_slot`]).
pub const SAMPLE_PIPELINE_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum CatalogError {
    /// JSON shape or an unknown field; carries the serde detail.
    Parse(String),
    /// Document `version` mismatch.
    Version { found: u32 },
    /// A feature value is non-finite or outside its declared range.
    Feature { id: String },
}

impl std::fmt::Display for CatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CatalogError::Parse(e) => write!(f, "catalog parse failed: {e}"),
            CatalogError::Version { found } => {
                write!(f, "catalog version {found} unsupported (want {SAMPLE_PIPELINE_VERSION})")
            }
            CatalogError::Feature { id } => write!(f, "sample `{id}` has an out-of-range feature"),
        }
    }
}

impl std::error::Error for CatalogError {}

/// Hand-engineered feature row for one sample (extracted offline by the #20
/// build pipeline; learned embeddings plug in later via
/// [`crate::query::AudioEmbedding`]).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineeredFeatures {
    /// Length in seconds.
    pub duration_s: f32,
    /// Spectral centroid in Hz (brightness); must be > 0 (log-scale math).
    pub spectral_centroid_hz: f32,
    /// Spectral flatness 0..=1 (noisy vs tonal).
    pub flatness: f32,
    /// Dominant pitch in Hz; 0 = unpitched.
    pub pitch_hz: f32,
    /// Onset sharpness 0..=1.
    pub transient_sharpness: f32,
    /// Integrated loudness, LUFS.
    pub lufs: f32,
}

/// SampleClass — the coarse role used for hard filtering and the palette
/// fallback table ([`crate::query::palette_defaults`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleClass {
    Kick,
    Hat,
    Perc,
    Pad,
    Texture,
}

/// One catalog entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SampleCatalog {
    /// Stable identity; pins reference this string.
    pub id: String,
    pub features: EngineeredFeatures,
    pub class: SampleClass,
    /// Provenance pack (recipe pack name or library path).
    pub pack: String,
    /// Free-form genre/timbre tags matched by query terms.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Precomputed embedding row (optional in v0; the #20 build pipeline
    /// fills it). Consumed via cosine similarity when the query carries a
    /// row of the same length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Vec<f32>>,
}

/// The catalog document as shipped on disk.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogFile {
    /// Must equal [`SAMPLE_PIPELINE_VERSION`].
    pub version: u32,
    pub samples: Vec<SampleCatalog>,
}

impl CatalogFile {
    pub fn iter(&self) -> impl Iterator<Item = &SampleCatalog> {
        self.samples.iter()
    }
}

/// Parses and validates a catalog document. Validation is the load-time
/// boundary: finite features, positive centroid, flatness/sharpness in
/// 0..=1, non-negative durations, unique ids. The query executor trusts
/// these invariants afterwards.
pub fn parse_catalog(json: &str) -> Result<CatalogFile, CatalogError> {
    let doc: CatalogFile =
        serde_json::from_str(json).map_err(|e| CatalogError::Parse(e.to_string()))?;
    if doc.version != SAMPLE_PIPELINE_VERSION {
        return Err(CatalogError::Version { found: doc.version });
    }
    let mut seen = std::collections::BTreeSet::new();
    for s in &doc.samples {
        let f = s.features;
        let ok = f.duration_s.is_finite()
            && f.duration_s >= 0.0
            && f.spectral_centroid_hz.is_finite()
            && f.spectral_centroid_hz > 0.0
            && in_unit(f.flatness)
            && f.pitch_hz.is_finite()
            && f.pitch_hz >= 0.0
            && in_unit(f.transient_sharpness)
            && f.lufs.is_finite();
        if !ok || s.id.is_empty() || !seen.insert(s.id.clone()) {
            return Err(CatalogError::Feature { id: s.id.clone() });
        }
    }
    Ok(doc)
}

fn in_unit(v: f32) -> bool {
    v.is_finite() && v >= 0.0 && v <= 1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committed_fixture_loads_and_validates() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/catalog.json");
        let json = std::fs::read_to_string(path).expect("fixture");
        let doc = parse_catalog(&json).expect("valid catalog");
        assert_eq!(doc.samples.len(), 20);
        for class in [
            SampleClass::Kick,
            SampleClass::Hat,
            SampleClass::Perc,
            SampleClass::Pad,
            SampleClass::Texture,
        ] {
            assert!(
                doc.iter().any(|s| s.class == class),
                "fixture covers every class: {class:?}"
            );
        }
    }

    #[test]
    fn rejects_bad_version_and_features() {
        let bad = parse_catalog(r#"{"version": 9, "samples": []}"#);
        assert!(matches!(bad, Err(CatalogError::Version { found: 9 })));
        let bad = parse_catalog(
            r#"{"version": 1, "samples": [{"id": "k",
                "features": {"duration_s": 1.0, "spectral_centroid_hz": 0.0,
                             "flatness": 0.5, "pitch_hz": 0.0,
                             "transient_sharpness": 0.5, "lufs": -10.0},
                "class": "kick", "pack": "p", "tags": []}]}"#,
        );
        assert!(matches!(bad, Err(CatalogError::Feature { .. })), "centroid must be > 0");
    }

    #[test]
    fn unknown_fields_and_duplicate_ids_rejected() {
        assert!(parse_catalog(r#"{"version": 1, "samples": [], "vibe": 1}"#).is_err());
        let dup = r#"{"version": 1, "samples": [
            {"id": "a", "features": {"duration_s": 1.0, "spectral_centroid_hz": 100.0,
             "flatness": 0.5, "pitch_hz": 0.0, "transient_sharpness": 0.5, "lufs": -10.0},
             "class": "kick", "pack": "p"},
            {"id": "a", "features": {"duration_s": 1.0, "spectral_centroid_hz": 100.0,
             "flatness": 0.5, "pitch_hz": 0.0, "transient_sharpness": 0.5, "lufs": -10.0},
             "class": "kick", "pack": "p"}]}"#;
        assert!(matches!(parse_catalog(dup), Err(CatalogError::Feature { .. })));
    }
}
