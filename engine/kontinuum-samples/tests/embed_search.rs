//! Embedding search contract (#20): the pluggable embedder + flat vector
//! index over the shipped curated library — determinism, embedder-version
//! invalidation, index roundtrip, and retrieval quality (text→samples and
//! by-example).

use kontinuum_samples::catalog::EngineeredFeatures;
use kontinuum_samples::{
    content_hash_of, parse_catalog, HashedBagEmbedder, IndexError, SampleCatalog, SampleClass,
    SampleEmbedder, VectorIndex,
};

fn fixture() -> Vec<SampleCatalog> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/catalog.json");
    let json = std::fs::read_to_string(path).expect("fixture");
    parse_catalog(&json).expect("valid catalog").samples
}

fn index() -> VectorIndex {
    VectorIndex::build(&HashedBagEmbedder, &fixture())
}

fn class_of(samples: &[SampleCatalog], id: &str) -> SampleClass {
    samples.iter().find(|s| s.id == id).expect("known id").class
}

/// A same-identity, bumped-version embedder: the "embedder was upgraded"
/// side of the invalidation contract.
struct UpgradedEmbedder;
impl SampleEmbedder for UpgradedEmbedder {
    fn id(&self) -> &'static str {
        HashedBagEmbedder.id()
    }
    fn version(&self) -> u32 {
        HashedBagEmbedder::VERSION + 1
    }
    fn dim(&self) -> usize {
        HashedBagEmbedder::DIM
    }
    fn embed_text(&self, text: &str) -> Vec<f32> {
        HashedBagEmbedder.embed_text(text)
    }
    fn embed_audio(&self, f: &EngineeredFeatures, class: SampleClass) -> Vec<f32> {
        HashedBagEmbedder.embed_audio(f, class)
    }
}

#[test]
fn text_query_kick_ranks_kicks_top3_on_the_curated_library() {
    let samples = fixture();
    let index = index();
    let hits = index.search_text(&HashedBagEmbedder, "kick", 3).expect("stamp matches");
    assert_eq!(hits.len(), 3);
    for hit in &hits {
        assert_eq!(
            class_of(&samples, &hit.id),
            SampleClass::Kick,
            "`{}` must be a kick",
            hit.id
        );
    }
    assert!(hits.windows(2).all(|w| w[0].score >= w[1].score), "ranked descending");
}

#[test]
fn descriptive_text_query_lands_in_the_right_class() {
    // The composer's phrasing from the issue brief: extra adjectives must
    // not knock retrieval out of the class entirely.
    let samples = fixture();
    let index = index();
    let hits = index
        .search_text(&HashedBagEmbedder, "dark analog kick", 5)
        .expect("stamp matches");
    assert!(!hits.is_empty());
    let kicks = hits.iter().filter(|h| class_of(&samples, &h.id) == SampleClass::Kick).count();
    assert!(kicks >= 3, "expected kicks to dominate, got {hits:?}");
}

#[test]
fn by_example_returns_near_neighbors_first() {
    let samples = fixture();
    let index = index();
    let hits = index.search_by_example("kick.punch.01", 5).expect("reference exists");
    assert_eq!(hits.len(), 5);
    assert_ne!(hits[0].id, "kick.punch.01", "reference excluded");
    let kicks = hits.iter().filter(|h| class_of(&samples, &h.id) == SampleClass::Kick).count();
    assert!(kicks >= 3, "kick neighbors must dominate, got {hits:?}");
    assert!(hits[0].score > hits.last().expect("non-empty").score, "genuinely ranked");
}

#[test]
fn by_example_unknown_sample_is_typed_error() {
    let err = index().search_by_example("no.such.sample", 5).unwrap_err();
    assert!(matches!(err, IndexError::UnknownSample(ref s) if s == "no.such.sample"));
}

#[test]
fn entries_key_off_sample_id_and_content_hash() {
    let mut samples = fixture();
    samples.sort_by(|a, b| a.id.cmp(&b.id));
    let index = index();
    assert_eq!(index.len(), samples.len());
    for (entry, sample) in index.entries().iter().zip(&samples) {
        assert_eq!(entry.id, sample.id, "entries id-ascending, one per sample");
        assert_eq!(entry.content_hash, content_hash_of(sample));
    }
}

#[test]
fn same_input_same_index_same_query() {
    let a = index();
    let b = index();
    assert_eq!(a.to_bytes(), b.to_bytes(), "index build is deterministic");
    let ha = a.search_text(&HashedBagEmbedder, "short woody percussion", 8).unwrap();
    let hb = b.search_text(&HashedBagEmbedder, "short woody percussion", 8).unwrap();
    assert_eq!(ha, hb);
}

#[test]
fn index_roundtrips_through_bytes() {
    let index = index();
    let decoded = VectorIndex::decode(&index.to_bytes()).expect("roundtrip");
    assert_eq!(decoded, index);
    // A query answered identically before and after the roundtrip.
    let direct = index.search_text(&HashedBagEmbedder, "hiss", 3).unwrap();
    let decoded_hits = decoded.search_text(&HashedBagEmbedder, "hiss", 3).unwrap();
    assert_eq!(direct, decoded_hits);
}

#[test]
fn embedder_upgrade_invalidates_the_index() {
    let bytes = index().to_bytes();
    // Old index + upgraded embedder → typed rebuild signal, never a silent
    // cross-version score.
    let err = VectorIndex::open(&UpgradedEmbedder, &bytes).unwrap_err();
    assert!(
        matches!(err, IndexError::EmbedderMismatch { found_version: 1, want_version: 2, .. }),
        "got {err:?}"
    );
    // Current embedder still opens it.
    assert!(VectorIndex::open(&HashedBagEmbedder, &bytes).is_ok());
}

#[test]
fn search_returns_scores_in_unit_range() {
    let index = index();
    for hit in index.search_text(&HashedBagEmbedder, "rumble sub", 20).unwrap() {
        assert!((0.0..=1.0).contains(&hit.score), "cosine clamped, got {}", hit.score);
    }
}
