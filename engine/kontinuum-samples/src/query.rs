//! Hybrid sample query executor (#20 engine side): linear scan over the
//! catalog with hard filters, scored as
//!
//! ```text
//! score = FEATURE_W * feature_match + TERM_W * term_match
//!       + EMBED_W * cosine        (only when both rows are present)
//! ```
//!
//! `feature_match` averages the present components — a log-octave gaussian
//! on spectral centroid and a duration-in-range term (1.0 inside the range,
//! harmonic decay outside); with neither requested it is neutral 1.0.
//! `term_match` is the fraction of `text_terms` found as case-insensitive
//! substrings of id/pack/tags. The hand-engineered features
//! (`flatness`, `pitch_hz`, `transient_sharpness`, `lufs`) ride along for
//! the embedding pipeline; v0 scoring uses centroid + duration only.
//!
//! SQL pre-filter ∩ vector is deliberately over-engineered for v0: the
//! catalog is scanned linearly, which stays well under 1 ms at 10k rows
//! (a few float ops per row). When the best score is below
//! [`SIMILARITY_FLOOR`] the executor returns the palette default for the
//! query's class instead, with [`QueryResult::used_fallback`] set as the
//! caller-visible warning marker.
//!
//! Real audio embeddings (#20 build-pipeline scope) plug in through
//! [`AudioEmbedding`]; the executor is embedding-agnostic until then.

use serde::{Deserialize, Serialize};

use crate::catalog::{EngineeredFeatures, SampleCatalog, SampleClass};

/// Score weights (v0, documented): feature evidence dominates, term/tag
/// match refines, and an embedding row — when supplied — adds on top, so
/// embedding-backed scores may reach FEATURE_W + TERM_W + EMBED_W.
pub const FEATURE_W: f32 = 0.6;
pub const TERM_W: f32 = 0.4;
pub const EMBED_W: f32 = 0.5;

/// Below this score nothing in the catalog answers the query well enough;
/// the resolver falls back to the palette default for the query's class.
pub const SIMILARITY_FLOOR: f32 = 0.35;

/// Ranked candidates kept in a result.
pub const MAX_CANDIDATES: usize = 8;

/// Gaussian sigma, in octaves, for the centroid match term: one octave of
/// brightness error costs ~40% of the component.
const CENTROID_SIGMA_OCT: f32 = 1.0;

/// Seam for learned audio embeddings (#20 build pipeline): a CLAP (or any
/// model) query row plugs in behind this trait so the executor gains the
/// cosine term without code changes.
pub trait AudioEmbedding {
    fn vector(&self) -> &[f32];
}

/// What a slot asks the catalog for. Unset fields are neutral (they do not
/// influence ranking).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SampleQuery {
    /// Free text matched against id, pack, and tags (all terms must hit).
    pub text_terms: Vec<String>,
    /// Hard class filter; also selects the palette default on fallback.
    pub class: Option<SampleClass>,
    /// Hard duration filter in seconds, inclusive.
    pub duration_range: Option<(f32, f32)>,
    /// Desired brightness (spectral centroid, Hz).
    pub target_centroid_hz: Option<f32>,
}

/// One ranked hit.
#[derive(Clone, Debug, PartialEq)]
pub struct RankedSample<'a> {
    pub sample: &'a SampleCatalog,
    pub score: f32,
}

/// Executor output. `used_fallback` is the warning marker: when set, no
/// candidate cleared [`SIMILARITY_FLOOR`] and the candidates list is empty;
/// callers substitute the palette default for the query's class
/// ([`default_for`]).
#[derive(Clone, Debug, PartialEq)]
pub struct QueryResult<'a> {
    pub candidates: Vec<RankedSample<'a>>,
    pub used_fallback: bool,
}

/// Palette fallback table: role-based defaults used when a query cannot clear
/// the similarity floor. These ids are palette constants, stable across
/// releases so pins stay meaningful.
pub mod palette_defaults {
    pub const KICK: &str = "palette.kick.four-floor";
    pub const HAT: &str = "palette.hat.closed";
    pub const PERC: &str = "palette.perc.wood";
    pub const PAD: &str = "palette.pad.warm";
    pub const TEXTURE: &str = "palette.texture.grain";
}

/// Palette default for a class; an unclassified query falls back to the
/// perc slot (the palette's generic "misc hit" role).
pub fn default_for(class: Option<SampleClass>) -> &'static str {
    match class {
        Some(SampleClass::Kick) | None => palette_defaults::KICK,
        Some(SampleClass::Hat) => palette_defaults::HAT,
        Some(SampleClass::Perc) => palette_defaults::PERC,
        Some(SampleClass::Pad) => palette_defaults::PAD,
        Some(SampleClass::Texture) => palette_defaults::TEXTURE,
    }
}

/// Runs the query: filter, score, rank. Deterministic — ties break by id
/// ascending, so equal scores always order identically.
pub fn run_query<'a>(
    catalog: &'a [SampleCatalog],
    query: &SampleQuery,
    embedding: Option<&dyn AudioEmbedding>,
) -> QueryResult<'a> {
    let mut ranked: Vec<RankedSample<'a>> = catalog
        .iter()
        .filter(|s| query.class.map_or(true, |c| s.class == c))
        .filter(|s| {
            query.duration_range.map_or(true, |(lo, hi)| {
                s.features.duration_s >= lo && s.features.duration_s <= hi
            })
        })
        .map(|s| RankedSample { sample: s, score: score(s, query, embedding) })
        .collect();
    ranked.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.sample.id.cmp(&b.sample.id))
    });
    ranked.truncate(MAX_CANDIDATES);

    let under_floor = match ranked.first() {
        Some(best) => best.score < SIMILARITY_FLOOR,
        None => true,
    };
    if under_floor {
        QueryResult { candidates: Vec::new(), used_fallback: true }
    } else {
        QueryResult { candidates: ranked, used_fallback: false }
    }
}

/// Feature component: average of the requested components (neutral 1.0 when
/// none are requested).
fn feature_match(f: &EngineeredFeatures, query: &SampleQuery) -> f32 {
    let mut parts = Vec::new();
    if let Some(target) = query.target_centroid_hz {
        let octaves = (target / f.spectral_centroid_hz).log2();
        let closeness = (-octaves * octaves / (2.0 * CENTROID_SIGMA_OCT * CENTROID_SIGMA_OCT)).exp();
        parts.push(closeness);
    }
    if let Some((lo, hi)) = query.duration_range {
        let span = (hi - lo).max(1.0);
        let away = if f.duration_s < lo {
            lo - f.duration_s
        } else if f.duration_s > hi {
            f.duration_s - hi
        } else {
            0.0
        };
        parts.push(1.0 / (1.0 + away / span));
    }
    if parts.is_empty() { 1.0 } else { parts.iter().sum::<f32>() / parts.len() as f32 }
}

/// Term component: fraction of terms found in id/pack/tags (case-insensitive
/// substring); neutral 1.0 with no terms.
fn term_match(s: &SampleCatalog, query: &SampleQuery) -> f32 {
    if query.text_terms.is_empty() {
        return 1.0;
    }
    let mut haystack = format!("{} {}", s.id, s.pack).to_lowercase();
    for tag in &s.tags {
        haystack.push(' ');
        haystack.push_str(&tag.to_lowercase());
    }
    let hits = query
        .text_terms
        .iter()
        .filter(|t| haystack.contains(&t.to_lowercase()))
        .count();
    hits as f32 / query.text_terms.len() as f32
}

fn score(sample: &SampleCatalog, query: &SampleQuery, embedding: Option<&dyn AudioEmbedding>) -> f32 {
    let mut score = FEATURE_W * feature_match(&sample.features, query) + TERM_W * term_match(sample, query);
    if let (Some(q), Some(c)) = (embedding, sample.embedding.as_deref()) {
        score += EMBED_W * cosine(q.vector(), c);
    }
    // Boundary guard: untrusted catalog floats must not poison the ranking.
    if score.is_finite() { score } else { 0.0 }
}

/// Cosine similarity clamped to 0..=1 (negative correlation contributes
/// nothing); mismatched row lengths contribute nothing.
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    (dot / (na * nb)).clamp(0.0, 1.0)
}

/// What the IR stores for a sample slot. Resolution happens once; **re-renders
/// use pins and never re-query** — the catalog may grow or the scorer may be
/// retuned later, but a pinned session renders identically. Pins migrate only
/// across an explicit [`crate::catalog::SAMPLE_PIPELINE_VERSION`] bump.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SamplePin {
    pub id: String,
    pub pipeline_version: u32,
}

/// Resolves a query to a deterministic pin: the top-ranked candidate, or the
/// palette default when the similarity floor is not met. Same catalog + same
/// query + same embedding row → same pin, always.
pub fn resolve_slot(
    catalog: &[SampleCatalog],
    query: &SampleQuery,
    embedding: Option<&dyn AudioEmbedding>,
) -> SamplePin {
    let result = run_query(catalog, query, embedding);
    let id = result
        .candidates
        .first()
        .map(|r| r.sample.id.clone())
        .unwrap_or_else(|| default_for(query.class).to_string());
    SamplePin { id, pipeline_version: crate::catalog::SAMPLE_PIPELINE_VERSION }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, class: SampleClass, centroid: f32) -> SampleCatalog {
        SampleCatalog {
            id: id.into(),
            features: EngineeredFeatures {
                duration_s: 1.0,
                spectral_centroid_hz: centroid,
                flatness: 0.5,
                pitch_hz: 0.0,
                transient_sharpness: 0.5,
                lufs: -10.0,
            },
            class,
            pack: "p".into(),
            tags: vec![],
            embedding: None,
        }
    }

    #[test]
    fn class_and_duration_filters_are_hard() {
        let cat = vec![entry("a", SampleClass::Kick, 100.0), entry("b", SampleClass::Pad, 100.0)];
        let q = SampleQuery { class: Some(SampleClass::Kick), ..Default::default() };
        let r = run_query(&cat, &q, None);
        assert_eq!(r.candidates.len(), 1);
        assert_eq!(r.candidates[0].sample.id, "a");

        let q = SampleQuery {
            class: Some(SampleClass::Kick),
            duration_range: Some((0.0, 0.5)),
            ..Default::default()
        };
        assert!(run_query(&cat, &q, None).used_fallback, "1 s kick misses a 0..0.5 s window");
    }

    #[test]
    fn cosine_seam_contributes_when_rows_match() {
        struct Row(Vec<f32>);
        impl AudioEmbedding for Row {
            fn vector(&self) -> &[f32] {
                &self.0
            }
        }
        let mut a = entry("a", SampleClass::Kick, 100.0);
        a.embedding = Some(vec![1.0, 0.0]);
        let mut b = entry("b", SampleClass::Kick, 100.0);
        b.embedding = Some(vec![0.0, 1.0]);
        let cat = vec![a, b];
        // No embedding: everything ties at the neutral 1.0 and ids break the tie.
        let plain = run_query(&cat, &SampleQuery::default(), None);
        assert_eq!(plain.candidates[0].sample.id, "a");
        // Query row aligned with b's row lifts b to the top.
        let q_row = Row(vec![0.0, 1.0]);
        let with = run_query(&cat, &SampleQuery::default(), Some(&q_row));
        assert_eq!(with.candidates[0].sample.id, "b");
        assert!(with.candidates[0].score > with.candidates[1].score);
    }
}
