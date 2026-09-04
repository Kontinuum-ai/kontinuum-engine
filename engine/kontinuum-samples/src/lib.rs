//! `kontinuum-samples` — sample recipes (issue #53): packs as validated JSON
//! + seed, rendered deterministically to PCM. Sessions reference recipe
//! hashes and re-derive samples on import; song files stay tiny while the
//! palette is effectively infinite.

pub mod catalog;
pub mod choke;
pub mod embedder;
pub mod expansion;
pub mod embedding;
pub mod expr;
pub mod features;
pub mod granular;
pub mod ingest;
pub mod loader;
pub mod pack;
pub mod query;
pub mod render;
pub mod schema;
pub mod search_index;
pub mod store;
pub mod stretch;

pub use catalog::{
    parse_catalog, CatalogError, CatalogFile, EngineeredFeatures, SampleCatalog, SampleClass,
    SAMPLE_PIPELINE_VERSION,
};
pub use choke::CHOKE_FADE_MS;
pub use embedder::{HashedBagEmbedder, SampleEmbedder};
pub use embedding::StoredEmbedding;
pub use expr::{HitExpression, HitSelection, VelocityCurve, select_hit};
pub use features::analyze_features;
pub use granular::{GrainSpec, GrainWindow, render_cloud};
pub use ingest::{ingest_dir, normalize_to_target_peak, IngestedSample, TARGET_DBTP};
pub use loader::PackLoader;
pub use pack::{
    build_pack, load_pack, ManifestEntry, Pack, PackEntry, PackError, PackManifest, PackMeta,
};
pub use query::{
    default_for, resolve_slot, run_query, AudioEmbedding, QueryResult, RankedSample, SamplePin,
    SampleQuery, EMBED_W, FEATURE_W, MAX_CANDIDATES, SIMILARITY_FLOOR, TERM_W,
};
pub use render::{is_silent, render_recipe};
pub use schema::{recipe_hash, validate, RecipeError, RenderedSample, SampleRecipe};
pub use search_index::{
    content_hash_of, IndexEntry, IndexError, Scored, VectorIndex, INDEX_FORMAT_VERSION,
};
pub use store::{CatalogDb, CatalogRow, StoreError};
pub use stretch::{StretchMode, stretch};
