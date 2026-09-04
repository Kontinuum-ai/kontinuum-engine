//! kontinuum-corpus — reference-corpus distribution fitting (issue #23).
//!
//! # STATUS: PIPELINE + FITTERS + EMITTERS ONLY — EVERY NUMBER THIS CRATE
//! CAN CURRENTLY SHIP IS A PLACEHOLDER.
//!
//! The real corpus (100–300 purchased tracks, legal memo per #6) does not
//! exist yet. Until it is bought, analyzed (#5) and fitted, all artifacts
//! are validated against the SYNTHETIC fixture
//! `fixtures/corpus-sample.jsonl`, which has planted structure: they prove
//! the pipeline shape, not musical truth. Consumers must gate on
//! `corpus_size` and treat fitted values as shape-proven,
//! content-meaningless until the real corpus lands.
//!
//! # Pipeline contract
//!
//! The #5 analysis toolchain (its hardening is out of scope here) writes
//! one [`schema::TrackObservation`] per analyzed track as JSONL into
//! `corpus/features/`. This crate owns the record schema, the
//! deterministic fitters, and the versioned artifacts:
//!
//! - `arrangement-params-{subgenre}.json` — per-kind section-length
//!   params, Laplace-smoothed section-kind transition matrix,
//!   transition-type tables, energy-arc centroids. Loaded by the #16
//!   planner (which currently hand-seeds this structure in
//!   `kontinuum-compose`); detector labels are free strings, the
//!   detected→grammar mapping stays with the planner.
//! - `groove-templates-{subgenre}.json` — named groove templates
//!   (`t0`..`tk-1`) for the #17 groove layer.
//!
//! # Documented deviations from the issue text
//!
//! - The issue names `.toml` (arrangement) and `.bin` (groove) outputs.
//!   Both ship as JSON: the workspace forbids new dependencies and `toml`
//!   would be one; JSON is serde-native for consumers and review-diffable.
//!   `artifact_version` replaces the binary format's magic number.
//! - The issue's full validation (100 sampled arrangements within CI of
//!   corpus stats, perceptual A/B, legal memo) runs once the real corpus
//!   exists; the scaled-down seeded-sampling check lives in
//!   `tests/grammar_sampling.rs`.
//!
//! # Determinism
//!
//! All fitting is closed-form or fixed-iteration (Laplace counts, linear
//! interpolation, farthest-first + 24 Lloyd iterations — no convergence
//! tolerances, no randomness). Conventions are documented in [`stats`],
//! [`arcs`], [`groove_fit`], and [`fit`]. Two fits of the same input are
//! byte-identical (asserted in `tests/corpus_validation.rs`).

pub mod arcs;
pub mod artifact;
pub mod eval;
pub mod fit;
pub mod groove_fit;
pub mod manifest;
pub mod sample;
pub mod schema;
pub mod stats;

#[derive(Debug, thiserror::Error)]
pub enum CorpusError {
    #[error("cannot fit an empty observation set")]
    EmptyCorpus,
    #[error("observations mix subgenres (expected only '{0}')")]
    MixedSubgenres(String),
    #[error("artifact schema version {found} unsupported (want {want})")]
    Version { found: u32, want: u32 },
    #[error("observation file unreadable: {0}")]
    Io(#[from] std::io::Error),
    #[error("observation JSONL parse failed at line {line}: {source}")]
    Json {
        line: usize,
        source: serde_json::Error,
    },
    #[error("artifact JSON parse failed: {0}")]
    ArtifactJson(#[from] serde_json::Error),
}

pub use arcs::{track_arc, ArcCluster};
pub use artifact::{
    emit, emit_groove, load_arrangement, load_groove, write_artifacts, ArcFamilySpec,
    ArrangementParamsArtifact, CurveWindows, GrammarBlock, GrammarConstraints,
    GrooveTemplatesArtifact, GRAMMAR_VERSION, LengthWindow, RecipeSpec, ARTIFACT_VERSION,
};
pub use eval::{
    boundary_f1, AnnotatedSection, BoundaryScores, SegmentationAnnotation,
    SEGMENTATION_F1_GATE,
};
pub use fit::{fit_subgenre, LengthParams, SubgenreFit};
pub use groove_fit::{groove_feature, GrooveTemplate};
pub use manifest::{fnv1a64_hex, Manifest, ManifestError, ManifestRow, HASH_ALGO, HEADER};
pub use sample::{sample_arrangement, SampledArrangement, SampledSection};
pub use schema::{
    load_jsonl, load_jsonl_file, GrooveObservation, SectionObservation, TrackObservation,
    TransitionObservation,
};
