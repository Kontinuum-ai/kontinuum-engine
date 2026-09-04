//! Expansion packs (sample library v2, #30): the #19 recipe pipeline driven
//! by the sound-roster-v2 engines. A pack is a named set of [`SampleRecipe`]s
//! — every hit rendered through `render_recipe` (registry-constructed
//! voices, so the wavetable / FM-perc / texture engines are pack sources) —
//! and produces three aligned artifacts:
//!
//! - a `.kpack` container ([`crate::pack::build_pack`]) with the PCM;
//! - a `PackManifest` with engineered features + PCM hashes;
//! - a `CatalogFile` with one [`SampleCatalog`] row per sample, embeddings
//!   precomputed (the #20 embedder), ready for `CatalogDb::import_json`
//!   (provenance: license + source are mandatory `PackMeta` fields).
//!
//! Everything is deterministic: recipe seeds drive the voices, features are
//! pure analysis, the embedder is hash-bucketed — rebuilding a pack yields
//! byte-identical artifacts. The shipped packs ([`built_in_packs`]) are
//! generated from the in-house engines only, so they are license-clean by
//! construction (#6): there is no sourced audio anywhere in the pipeline.

use std::collections::BTreeMap;

use crate::catalog::{CatalogFile, SampleCatalog, SampleClass, SAMPLE_PIPELINE_VERSION};
use crate::embedder::SampleEmbedder;
use crate::ingest::IngestedSample;
use crate::pack::{build_pack, ManifestEntry, PackError, PackManifest, PackMeta};
use crate::schema::SampleRecipe;
use crate::{analyze_features, render_recipe, validate};

/// Pack id: FM-percussion one-shots (metallic / tom / bell families).
pub const PACK_FM_PERC: &str = "fm-perc-v1";
/// Pack id: noise beds and vinyl/tape crackle.
pub const PACK_TEXTURE: &str = "texture-crackle-v1";
/// Pack id: pad-voice chord one-shots.
pub const PACK_CHORDS: &str = "chord-oneshots-v1";

/// One expansion pack: provenance + the recipes that render it.
pub struct ExpansionSpec {
    pub pack: String,
    pub license: String,
    pub source: String,
    pub recipes: Vec<SampleRecipe>,
}

/// The three aligned artifacts of a built pack.
pub struct ExpansionArtifact {
    pub manifest: PackManifest,
    pub catalog: CatalogFile,
    pub kpack: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum ExpansionError {
    #[error("recipe: {0}")]
    Recipe(#[from] crate::schema::RecipeError),
    #[error("pack: {0}")]
    Pack(#[from] PackError),
}

/// Class / choke tags ride recipe tags, mirroring the genpack convention.
fn tag_class_and_choke(recipe: &SampleRecipe) -> (SampleClass, Option<u8>) {
    let class = recipe
        .tags
        .iter()
        .find_map(|t| match t.as_str() {
            "kick" => Some(SampleClass::Kick),
            "hat" => Some(SampleClass::Hat),
            "perc" => Some(SampleClass::Perc),
            "pad" => Some(SampleClass::Pad),
            "texture" => Some(SampleClass::Texture),
            _ => None,
        })
        .unwrap_or(SampleClass::Perc);
    let choke = recipe
        .tags
        .iter()
        .find_map(|t| t.strip_prefix("choke:").and_then(|n| n.parse::<u8>().ok()));
    (class, choke)
}

/// Builds the pack: render → analyze → container + manifest + catalog
/// (with embeddings). Recipes are processed in name order so artifacts are
/// independent of input order.
pub fn build_expansion(spec: &ExpansionSpec) -> Result<ExpansionArtifact, ExpansionError> {
    let embedder = crate::embedder::HashedBagEmbedder::default();
    let mut ordered: BTreeMap<&str, &SampleRecipe> = BTreeMap::new();
    for r in &spec.recipes {
        ordered.insert(r.name.as_str(), r);
    }

    let mut ingested = Vec::new();
    let mut entries = Vec::new();
    let mut catalog_rows = Vec::new();
    for (name, recipe) in &ordered {
        validate(recipe)?;
        let rendered = render_recipe(recipe)?;
        let (class, choke) = tag_class_and_choke(recipe);
        let features = analyze_features(&rendered.pcm, rendered.sample_rate);
        let id = format!("{}.{}", spec.pack, name);
        entries.push(ManifestEntry {
            id: id.clone(),
            class,
            sample_rate: rendered.sample_rate,
            frames: rendered.pcm.len() as u32,
            features,
            pcm_hash: format!("{:016x}", kontinuum_core::fnv1a64(&f32_le_bytes(&rendered.pcm))),
            choke_group: choke,
        });
        ingested.push(IngestedSample {
            id: id.clone(),
            class,
            sample_rate: rendered.sample_rate,
            features,
            pcm: rendered.pcm.clone(),
        });
        let tags: Vec<String> = recipe
            .tags
            .iter()
            .filter(|t| !matches!(t.as_str(), "kick" | "hat" | "perc" | "pad" | "texture")
                && !t.starts_with("choke:"))
            .cloned()
            .collect();
        let row = SampleCatalog {
            id,
            features,
            class,
            pack: spec.pack.clone(),
            tags,
            embedding: None,
        };
        let embedding = embedder.embed_sample(&row);
        catalog_rows.push(SampleCatalog { embedding: Some(embedding), ..row });
    }

    let meta = PackMeta {
        pack: spec.pack.clone(),
        license: spec.license.clone(),
        source: spec.source.clone(),
    };
    let manifest = PackManifest {
        version: 1,
        pack: spec.pack.clone(),
        license: spec.license.clone(),
        source: spec.source.clone(),
        samples: entries,
    };
    let kpack = build_pack(&ingested, &meta);
    Ok(ExpansionArtifact {
        manifest,
        catalog: CatalogFile { version: SAMPLE_PIPELINE_VERSION, samples: catalog_rows },
        kpack,
    })
}

fn f32_le_bytes(pcm: &[f32]) -> Vec<u8> {
    pcm.iter().flat_map(|s| s.to_le_bytes()).collect()
}

/// The shipped expansion packs (#30 roster v2): FM percussion, texture /
/// crackle beds, and pad-voice chord one-shots. Deterministic recipe sets —
/// the recipes below ARE the pack, and rebuilding re-derives every sample
/// bit-identically.
pub fn built_in_packs() -> Vec<ExpansionSpec> {
    vec![
        ExpansionSpec {
            pack: PACK_FM_PERC.to_string(),
            license: "CC0-1.0 (self-generated in-repo from the FM percussion voice)".to_string(),
            source: "kontinuum-samples::expansion built_in_packs, rendered via render_recipe".to_string(),
            recipes: fm_perc_recipes(),
        },
        ExpansionSpec {
            pack: PACK_TEXTURE.to_string(),
            license: "CC0-1.0 (self-generated in-repo from the texture voice)".to_string(),
            source: "kontinuum-samples::expansion built_in_packs, rendered via render_recipe".to_string(),
            recipes: texture_recipes(),
        },
        ExpansionSpec {
            pack: PACK_CHORDS.to_string(),
            license: "CC0-1.0 (self-generated in-repo from the pad voice)".to_string(),
            source: "kontinuum-samples::expansion built_in_packs, rendered via render_recipe".to_string(),
            recipes: chord_recipes(),
        },
    ]
}

/// One recipe per hit family/pitch. Tags carry the query vocabulary the
/// gap-analysis mining matches against.
fn fm_perc_recipes() -> Vec<SampleRecipe> {
    let families: [(&str, &str, f32); 3] = [
        ("metallic", "metallic", 0.8),
        ("tom", "tom", 1.0),
        ("bell", "bell", 2.4),
    ];
    let pitches: [(&str, f32); 3] = [("low", 45.0), ("mid", 57.0), ("high", 69.0)];
    let mut out = Vec::new();
    for (fam, preset, decay) in families {
        for (band, pitch) in pitches {
            out.push(serde_json::from_str(&format!(
                r#"{{
                    "version": 1, "seed": {seed}, "sample_rate": 48000,
                    "name": "{fam}-{band}",
                    "voices": [{{"id": "v", "instrument": {{"kind": "fmperc", "ratio": {ratio}, "index": 3.0, "feedback": 0.3, "decay_ms": {decay_ms}, "preset": "{preset}"}}}}],
                    "hits": [{{"voice": "v", "at_ms": 0.0, "pitch": {pitch}, "velocity": 0.9}}],
                    "tail_ms": 2600.0,
                    "tags": ["perc", "fm", "{fam}", "{band}", "roster-v2"]
                }}"#,
                seed = 0x30 + fam.len() * 10 + band.len(),
                ratio = if fam == "tom" { 1.0 } else { 1.5 },
                decay_ms = decay * 320.0,
            ))
            .expect("fm perc recipe parses"));
        }
    }
    out
}

fn texture_recipes() -> Vec<SampleRecipe> {
    let variants = [
        ("crackle-light", true, 0.003, 0.35),
        ("crackle-dusty", true, 0.008, 0.25),
        ("crackle-bright", true, 0.004, 0.8),
        ("bed-dark", false, 0.002, 0.3),
        ("bed-air", false, 0.001, 0.9),
        ("bed-grit", false, 0.004, 0.55),
    ];
    variants
        .iter()
        .enumerate()
        .map(|(i, (name, crackle, density, tone))| {
            serde_json::from_str(&format!(
                r#"{{
                    "version": 1, "seed": {seed}, "sample_rate": 48000,
                    "name": "{name}",
                    "voices": [{{"id": "v", "instrument": {{"kind": "texture", "crackle": {crackle}, "density": {density}, "grain_ms": 30.0, "tone": {tone}}}}}],
                    "hits": [{{"voice": "v", "at_ms": 0.0, "pitch": 60.0, "velocity": 0.8}}],
                    "tail_ms": 4000.0,
                    "tags": ["texture", "noise", "roster-v2", "{flavor}"]
                }}"#,
                seed = 0x40 + i as u32,
                flavor = if *crackle { "vinyl" } else { "bed" },
            ))
            .expect("texture recipe parses")
        })
        .collect()
}

fn chord_recipes() -> Vec<SampleRecipe> {
    // (name, chord tones as MIDI pitches, wavetable position)
    let chords: [(&str, [f32; 3], f32); 6] = [
        ("cmin9-dark", [36.0, 46.0, 55.0], 0.2),
        ("ebmaj-warm", [39.0, 50.0, 58.0], 0.3),
        ("fmin-hazy", [41.0, 51.0, 60.0], 0.25),
        ("gmin-open", [31.0, 43.0, 58.0], 0.4),
        ("abmaj-wide", [32.0, 44.0, 56.0], 0.35),
        ("bbmin-deep", [34.0, 46.0, 53.0], 0.15),
    ];
    chords
        .iter()
        .enumerate()
        .map(|(i, (name, tones, position))| {
            serde_json::from_str(&format!(
                r#"{{
                    "version": 1, "seed": {seed}, "sample_rate": 48000,
                    "name": "{name}",
                    "voices": [{{"id": "v", "instrument": {{"kind": "wavetable", "position": {position}, "detune_cents": 14.0, "osc2_level": 0.8, "sub": 0.4, "cutoff_hz": 1800.0, "release_ms": 2400.0}}}}],
                    "hits": [
                        {{"voice": "v", "at_ms": 0.0, "pitch": {p0}, "velocity": 0.7}},
                        {{"voice": "v", "at_ms": 0.0, "pitch": {p1}, "velocity": 0.6}},
                        {{"voice": "v", "at_ms": 0.0, "pitch": {p2}, "velocity": 0.55}}
                    ],
                    "tail_ms": 3500.0,
                    "tags": ["pad", "chord", "oneshot", "roster-v2"]
                }}"#,
                seed = 0x50 + i as u32,
                p0 = tones[0], p1 = tones[1], p2 = tones[2],
            ))
            .expect("chord recipe parses")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::load_pack;
    use crate::catalog::parse_catalog;

    #[test]
    fn built_in_packs_are_complete_and_deterministic() {
        for spec in built_in_packs() {
            let name = spec.pack.clone();
            let a = build_expansion(&spec).expect(name.as_str());
            let b = build_expansion(&spec).expect(name.as_str());
            assert_eq!(a.kpack, b.kpack, "{name}: container not deterministic");
            assert_eq!(a.manifest.samples.len(), spec.recipes.len(), "{name}");
            assert_eq!(a.catalog.samples.len(), spec.recipes.len(), "{name}");
            assert!(a.kpack.iter().all(|_| true));
            assert!(!a.manifest.samples.is_empty());
            for entry in &a.manifest.samples {
                assert!(entry.frames > 480, "{name}/{}: too short", entry.id);
                assert!(entry.features.spectral_centroid_hz.is_finite());
            }
            for row in &a.catalog.samples {
                assert_eq!(row.pack, name, "provenance pack stamp");
                let emb = row.embedding.as_ref().expect("embedding present");
                assert!(!emb.is_empty());
                assert!(emb.iter().all(|v| v.is_finite()));
            }
        }
    }

    #[test]
    fn container_round_trips_and_catalog_parses() {
        for spec in built_in_packs() {
            let artifact = build_expansion(&spec).expect(spec.pack.as_str());
            let pack = load_pack(&artifact.kpack).expect("kpack loads");
            assert_eq!(pack.manifest.samples.len(), artifact.manifest.samples.len());
            let json = serde_json::to_string(&artifact.catalog).unwrap();
            let parsed = parse_catalog(&json).expect("catalog parses");
            assert_eq!(parsed.samples.len(), artifact.catalog.samples.len());
        }
    }

    #[test]
    fn embeddings_carry_the_embedder_stamp() {
        let spec = &built_in_packs()[0];
        let artifact = build_expansion(spec).unwrap();
        let dim = artifact.catalog.samples[0].embedding.as_ref().unwrap().len();
        let embedder = crate::embedder::HashedBagEmbedder;
        assert_eq!(dim, embedder.dim(), "embedding dim must match the #20 embedder");
        assert_eq!(embedder.id(), "hashed-bag");
    }
}
