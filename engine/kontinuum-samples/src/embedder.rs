//! Sample embedding backends (#20): the pluggable seam that turns text and
//! audio features into fixed-dimension vectors, plus the offline local
//! default. Computing embeddings is *never* on the audio thread and never
//! needs the network for the shipped backend — the on-device-first
//! invariant (#36's tier model) holds with zero configuration.
//!
//! The trait is the seam #36's learned providers plug into: a CLAP-class
//! text encoder or a cloud batch embedder implements [`SampleEmbedder`]
//! and the index, retrieval API, and query executor are unchanged. The
//! embedder's [`SampleEmbedder::version`] travels with every index built
//! from it: vectors from different embedders are not comparable, so a
//! version change invalidates the index by construction (pinned by test in
//! `search_index`).
//!
//! [`HashedBagEmbedder`] is the honest v1 local backend: every signal —
//! lexical tokens, character trigrams, quantized audio-feature bins — is a
//! feature string hashed into a shared bucket space (FNV-1a, the crate's
//! existing hash), weighted, and L2-normalized. Text and audio land in the
//! same space by construction, so text→samples and by-example retrieval
//! work cross-modal with no model at all. Semantic quality is limited —
//! "punchy" will not find a kick tagged "fat" — that is the documented v1
//! ceiling; stronger text embeddings arrive through #36's providers.

use crate::catalog::{EngineeredFeatures, SampleCatalog, SampleClass};

/// The embedding backend seam. Implementations must be deterministic (same
/// input → same vector, always), emit exactly [`SampleEmbedder::dim`]
/// components, and L2-normalize (zero vector for empty input is allowed;
/// cosine scoring treats it as no evidence).
pub trait SampleEmbedder {
    /// Algorithm identity stamped into every index (e.g. `"hashed-bag"`).
    /// Two embedders with the same identity must agree on vector semantics.
    fn id(&self) -> &'static str;

    /// Bumped on any change that alters vectors (bucket count, feature
    /// strings, weights). Indexes built by an older version are invalid,
    /// never silently reused.
    fn version(&self) -> u32;

    /// Fixed vector dimension.
    fn dim(&self) -> usize;

    /// Embeds free text (a composer query like "short woody percussion").
    fn embed_text(&self, text: &str) -> Vec<f32>;

    /// Embeds one sample's engineered features + class (the catalog row's
    /// audio side; the same shape `crate::features::analyze_features`
    /// produces).
    fn embed_audio(&self, features: &EngineeredFeatures, class: SampleClass) -> Vec<f32>;

    /// Embeds a library sample for indexing: lexical identity (id segments,
    /// tags, class word) merged with the audio-feature vector, renormalized.
    /// A text query hits the lexical half; a by-example query hits the
    /// audio half; both rank the same rows.
    fn embed_sample(&self, sample: &SampleCatalog) -> Vec<f32> {
        let text = sample_text(sample);
        let lexical = self.embed_text(&text);
        let audio = self.embed_audio(&sample.features, sample.class);
        let mut out = vec![0.0f32; self.dim()];
        for (o, v) in out.iter_mut().zip(&lexical) {
            *o += v;
        }
        for (o, v) in out.iter_mut().zip(&audio) {
            *o += v;
        }
        normalize(&mut out);
        out
    }
}

/// The sample's lexical identity: id segments, tags, and the class word,
/// space-joined. `kick.punch.01` + tags `["punchy", "four-floor"]` reads as
/// "kick punch 01 punchy four-floor kick" — the bag-of-features needs no
/// structure beyond the words.
fn sample_text(sample: &SampleCatalog) -> String {
    let mut text = sample.id.replace(['.', '-', '_'], " ");
    text.push(' ');
    text.push_str(&sample.tags.join(" "));
    text.push(' ');
    text.push_str(class_word(sample.class));
    text
}

fn class_word(class: SampleClass) -> &'static str {
    match class {
        SampleClass::Kick => "kick",
        SampleClass::Hat => "hat",
        SampleClass::Perc => "perc",
        SampleClass::Pad => "pad",
        SampleClass::Texture => "texture",
    }
}

/// Local, offline, deterministic hashed bag-of-features embedder (v1
/// default; see the module docs for the honest scope). Bucket count and
/// weights are constants of version 1 — changing any of them requires
/// bumping [`HashedBagEmbedder::VERSION`].
pub struct HashedBagEmbedder;

impl HashedBagEmbedder {
    /// Embedder version stamped into indexes.
    pub const VERSION: u32 = 1;

    /// Shared bucket space dimension.
    pub const DIM: usize = 256;

    /// Weight of one whole-word occurrence.
    const TOKEN_W: f32 = 1.0;
    /// Weight of one character trigram (fuzzy token overlap: "kicks" still
    /// grazes "kick").
    const TRIGRAM_W: f32 = 0.25;
    /// Weight of one audio-feature bin.
    const BIN_W: f32 = 1.0;
}

impl Default for HashedBagEmbedder {
    fn default() -> Self {
        HashedBagEmbedder
    }
}

impl SampleEmbedder for HashedBagEmbedder {
    fn id(&self) -> &'static str {
        "hashed-bag"
    }

    fn version(&self) -> u32 {
        Self::VERSION
    }

    fn dim(&self) -> usize {
        Self::DIM
    }

    fn embed_text(&self, text: &str) -> Vec<f32> {
        let mut out = vec![0.0f32; Self::DIM];
        for token in tokenize(text) {
            add_feature(&mut out, &format!("w:{token}"), Self::TOKEN_W);
            for tri in trigrams(&token) {
                add_feature(&mut out, &format!("g:{tri}"), Self::TRIGRAM_W);
            }
        }
        normalize(&mut out);
        out
    }

    fn embed_audio(&self, f: &EngineeredFeatures, class: SampleClass) -> Vec<f32> {
        let mut out = vec![0.0f32; Self::DIM];
        // One bin per engineered feature, quantized on the scale a sound
        // designer would use. Non-finite fields contribute nothing (the
        // same boundary guard `query::score` applies to catalog floats).
        if f.spectral_centroid_hz.is_finite() && f.spectral_centroid_hz > 0.0 {
            // Octave bands over 20 Hz .. 20 kHz: sub → hiss.
            let octave = (f.spectral_centroid_hz / 20.0).log2().floor();
            let octave = octave.clamp(0.0, 10.0) as u32;
            add_feature(&mut out, &format!("c:{octave}"), Self::BIN_W);
        }
        if f.flatness.is_finite() {
            let k = (f.flatness.clamp(0.0, 1.0) * 4.0).floor() as u32; // tonal .. noisy
            add_feature(&mut out, &format!("f:{k}"), Self::BIN_W);
        }
        if f.transient_sharpness.is_finite() {
            let k = (f.transient_sharpness.clamp(0.0, 1.0) * 4.0).floor() as u32; // swell .. click
            add_feature(&mut out, &format!("s:{k}"), Self::BIN_W);
        }
        if f.lufs.is_finite() {
            // 6 LU bins from −60: whisper .. hot.
            let k = ((f.lufs + 60.0) / 6.0).floor().clamp(0.0, 15.0) as u32;
            add_feature(&mut out, &format!("l:{k}"), Self::BIN_W);
        }
        if f.duration_s.is_finite() && f.duration_s >= 0.0 {
            // Power-of-two bands: 16 ms one-shots .. 16 s+ beds.
            let k = f.duration_s.log2().floor().clamp(0.0, 10.0) as u32;
            add_feature(&mut out, &format!("d:{k}"), Self::BIN_W);
        }
        add_feature(&mut out, &format!("k:{}", class_word(class)), Self::BIN_W);
        normalize(&mut out);
        out
    }
}

/// Lowercases and splits on non-alphanumeric characters.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_ascii_lowercase())
        .collect()
}

/// Character trigrams of a token (fewer than 3 characters → none).
fn trigrams(token: &str) -> Vec<String> {
    let chars: Vec<char> = token.chars().collect();
    if chars.len() < 3 {
        return Vec::new();
    }
    chars.windows(3).map(|w| w.iter().collect()).collect()
}

/// Adds `weight` to the bucket `feature` hashes to. FNV-1a (the crate's
/// existing hash) keeps the mapping stable across platforms and releases.
fn add_feature(out: &mut [f32], feature: &str, weight: f32) {
    let bucket = kontinuum_core::fnv1a64(feature.as_bytes()) as usize % out.len();
    out[bucket] += weight;
}

/// L2-normalizes in place; an all-zero vector stays all-zero (cosine then
/// reports no evidence, exactly like `query::cosine`'s guard).
fn normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emb() -> HashedBagEmbedder {
        HashedBagEmbedder
    }

    fn sample(id: &str, class: SampleClass, tags: &[&str], centroid: f32) -> SampleCatalog {
        SampleCatalog {
            id: id.into(),
            features: EngineeredFeatures {
                duration_s: 0.5,
                spectral_centroid_hz: centroid,
                flatness: 0.5,
                pitch_hz: 0.0,
                transient_sharpness: 0.5,
                lufs: -12.0,
            },
            class,
            pack: "p".into(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            embedding: None,
        }
    }

    #[test]
    fn deterministic_and_normalized() {
        let e = emb();
        let a = e.embed_text("short woody percussion");
        let b = e.embed_text("short woody percussion");
        assert_eq!(a, b, "same text → same vector");
        assert_eq!(a.len(), HashedBagEmbedder::DIM);
        let norm: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "unit norm, got {norm}");

        let a = e.embed_audio(&sample("x", SampleClass::Kick, &[], 80.0).features, SampleClass::Kick);
        let b = e.embed_audio(&sample("x", SampleClass::Kick, &[], 80.0).features, SampleClass::Kick);
        assert_eq!(a, b, "same features → same vector");
        let norm: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "unit norm, got {norm}");
    }

    #[test]
    fn empty_text_is_zero_vector() {
        let v = emb().embed_text("   .- _");
        assert!(v.iter().all(|&x| x == 0.0), "no tokens → no evidence");
    }

    #[test]
    fn cross_modal_vectors_share_a_space() {
        // The whole point of the shared bucket space: a text vector and an
        // audio vector have the same dimension and comparable directions.
        let e = emb();
        assert_eq!(e.embed_text("kick").len(), e.embed_audio(
            &sample("x", SampleClass::Kick, &[], 80.0).features, SampleClass::Kick
        ).len());
    }

    #[test]
    fn trigrams_graze_word_variants() {
        let e = emb();
        let kick = e.embed_text("kick");
        let kicks = e.embed_text("kicks");
        let dot = cosine(&kick, &kicks);
        assert!(dot > 0.1, "trigram overlap should correlate variants, got {dot}");
        let unrelated = cosine(&kick, &e.embed_text("pad"));
        assert!(dot > unrelated, "variants closer than unrelated words");
    }

    #[test]
    fn sample_vector_merges_lexical_and_audio() {
        let e = emb();
        let s = sample("kick.punch.01", SampleClass::Kick, &["punchy"], 80.0);
        let v = e.embed_sample(&s);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "merged vector renormalized");
        // Text matching the sample's own words beats text matching nothing.
        let hit = cosine(&v, &e.embed_text("kick punchy"));
        let miss = cosine(&v, &e.embed_text("rain field-recording"));
        assert!(hit > miss, "lexical half must answer text: {hit} vs {miss}");
        // Audio matching the sample's features beats unrelated features.
        let audio_hit = cosine(&v, &e.embed_audio(&s.features, SampleClass::Kick));
        let audio_miss = cosine(&v, &e.embed_audio(
            &sample("y", SampleClass::Hat, &[], 19_000.0).features, SampleClass::Hat,
        ));
        assert!(audio_hit > audio_miss, "audio half must answer by-example");
    }

    #[test]
    fn non_finite_features_contribute_nothing() {
        let e = emb();
        let clean = e.embed_audio(&sample("x", SampleClass::Kick, &[], 80.0).features, SampleClass::Kick);
        let bad = EngineeredFeatures {
            duration_s: f32::NAN,
            spectral_centroid_hz: f32::INFINITY,
            flatness: f32::NAN,
            pitch_hz: 0.0,
            transient_sharpness: f32::NAN,
            lufs: f32::NAN,
        };
        let v = e.embed_audio(&bad, SampleClass::Kick);
        assert_eq!(v.len(), clean.len());
        // Only the class bin fires; still finite and normalized.
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(norm.is_finite() && (norm - 1.0).abs() < 1e-5);
    }

    /// Cosine similarity, clamped like `query::cosine` (test-local copy —
    /// the production one is private to the query module).
    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        if na == 0.0 || nb == 0.0 {
            return 0.0;
        }
        (dot / (na * nb)).clamp(0.0, 1.0)
    }
}
