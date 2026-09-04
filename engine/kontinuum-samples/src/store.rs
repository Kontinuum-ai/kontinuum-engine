//! SQLite sample catalog (#19 storage half): the durable home for catalog
//! rows and per-sample provenance. Embeddings are stored here as f16 blobs
//! (codec in [`crate::embedding`]) — they live in the database, not in the
//! `.kpack` container: the #20 build pipeline computes them after packing
//! and fills the catalog (see `pack.rs` for the container side).
//!
//! Split of concerns: [`crate::catalog`] owns the JSON document domain as
//! shipped inside packs; this module owns the row model — everything the
//! database adds on top (path, license, source note, pipeline version,
//! integrity hash, embedding blob).
//!
//! Schema is versioned with `PRAGMA user_version`; migrations are in-code
//! batches that each end by stamping their version, so a fresh database and
//! an already-migrated one converge on the same state. A database written
//! by a newer build is a typed error ([`StoreError::Version`]) — never a
//! silent downgrade.
//!
//! Provenance is mandatory at import: [`CatalogDb::import_json`] takes
//! `license` and `source_note` arguments with no defaults, because a sample
//! without an honest origin is not shippable (#6). Catalog JSON describes
//! synthesized material, so imported rows take the documented engine
//! defaults: `sample_rate` 48_000 and an empty `path` (paths only exist for
//! imported files).

use std::path::Path;

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

use crate::catalog::{
    parse_catalog, CatalogError, EngineeredFeatures, SampleCatalog, SampleClass,
    SAMPLE_PIPELINE_VERSION,
};
use crate::embedding::{embedding_from_blob, embedding_to_blob, StoredEmbedding};

/// Newest schema version this build understands.
const SCHEMA_VERSION: i64 = 2;

/// Migration batch v1: the initial `samples` table. Ends by stamping its
/// version so re-opening skips it.
const MIGRATE_V1: &str = "
CREATE TABLE samples (
    id                   TEXT PRIMARY KEY,
    path                 TEXT NOT NULL,
    duration_s           REAL NOT NULL,
    sample_rate          INTEGER NOT NULL,
    spectral_centroid_hz REAL NOT NULL,
    flatness             REAL NOT NULL,
    pitch_hz             REAL NOT NULL,
    transient_sharpness  REAL NOT NULL,
    lufs                 REAL NOT NULL,
    class                TEXT NOT NULL,
    pack                 TEXT NOT NULL,
    license              TEXT NOT NULL,
    source_note          TEXT NOT NULL,
    tags                 TEXT NOT NULL,
    embedding            BLOB,
    embedding_dim        INTEGER NOT NULL,
    pipeline_version     INTEGER NOT NULL,
    integrity_hash       TEXT NOT NULL
);
PRAGMA user_version = 1;
";

/// Migration batch v2 (#30 gap analysis): the query log. Every sample
/// search lands one row here (the query executor calls [`CatalogDb::log_query`]);
/// [`CatalogDb::worst_served_queries`] mines it for expansion-pack
/// curation — "what did the composer ask for that scored poorly". The
/// table starts empty; the data accumulates in production.
const MIGRATE_V2: &str = "
CREATE TABLE query_log (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_utc           TEXT NOT NULL,
    terms            TEXT NOT NULL,
    class            TEXT,
    result_count     INTEGER NOT NULL,
    top_score        REAL,
    top_sample_id    TEXT,
    pipeline_version INTEGER NOT NULL
);
CREATE INDEX idx_query_log_terms ON query_log(terms);
PRAGMA user_version = 2;
";

/// Sample rate stamped on rows imported from catalog JSON: the JSON domain
/// describes engine-synthesized material, rendered at the documented
/// engine default.
const SYNTH_SAMPLE_RATE: u32 = 48_000;

const SELECT_ALL: &str = "SELECT id, path, duration_s, sample_rate, spectral_centroid_hz, \
     flatness, pitch_hz, transient_sharpness, lufs, class, pack, license, source_note, tags, \
     embedding, embedding_dim, pipeline_version, integrity_hash \
     FROM samples ORDER BY id";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("catalog document rejected: {0}")]
    Catalog(#[from] CatalogError),
    #[error("json: {0}")]
    Json(String),
    #[error("catalog schema v{found} is newer than this build supports; regenerate the catalog")]
    Version { found: i64 },
    #[error("integrity mismatch for `{id}`")]
    Integrity { id: String },
}

/// One durable catalog row: the JSON document's content fields plus the
/// storage-only additions (path, provenance, pipeline version, embedding,
/// integrity hash).
#[derive(Clone, Debug, PartialEq)]
pub struct CatalogRow {
    pub id: String,
    pub path: String,
    pub features: EngineeredFeatures,
    pub sample_rate: u32,
    pub class: SampleClass,
    pub pack: String,
    pub license: String,
    pub source_note: String,
    pub tags: Vec<String>,
    /// Decoded embedding (None = not yet embedded by the #20 pipeline).
    pub embedding: Option<Vec<f32>>,
    pub embedding_dim: u32,
    pub pipeline_version: u32,
    pub integrity_hash: String,
}

/// The integrity-hashed projection of a row's content: field order is the
/// canonical form, and `integrity_hash` itself is excluded (it hashes the
/// content, not itself).
#[derive(Serialize)]
struct CanonicalRow<'a> {
    id: &'a str,
    path: &'a str,
    sample_rate: u32,
    features: &'a EngineeredFeatures,
    class: &'a SampleClass,
    pack: &'a str,
    license: &'a str,
    source_note: &'a str,
    tags: &'a [String],
    embedding: Option<&'a [f32]>,
    embedding_dim: u32,
    pipeline_version: u32,
}

impl CatalogRow {
    /// Builder: stamps the integrity hash — FNV-1a (the `schema.rs::
    /// recipe_hash` style) over the canonical JSON of every content field,
    /// hex-encoded. Same content → same hash, always.
    pub fn with_integrity(mut self) -> Self {
        self.integrity_hash = integrity_hash(&self);
        self
    }

    /// View as the query-side [`SampleCatalog`]: storage-only fields are
    /// dropped; the f16 blob is already decoded into `embedding`.
    pub fn as_catalog(&self) -> SampleCatalog {
        SampleCatalog {
            id: self.id.clone(),
            features: self.features,
            class: self.class,
            pack: self.pack.clone(),
            tags: self.tags.clone(),
            embedding: self.embedding.clone(),
        }
    }
}

fn canonical(row: &CatalogRow) -> CanonicalRow<'_> {
    CanonicalRow {
        id: &row.id,
        path: &row.path,
        sample_rate: row.sample_rate,
        features: &row.features,
        class: &row.class,
        pack: &row.pack,
        license: &row.license,
        source_note: &row.source_note,
        tags: &row.tags,
        embedding: row.embedding.as_deref(),
        embedding_dim: row.embedding_dim,
        pipeline_version: row.pipeline_version,
    }
}

fn integrity_hash(row: &CatalogRow) -> String {
    // A row built through this module cannot carry non-finite features, so
    // serialization cannot fail; `unwrap_or_default` keeps the hash total
    // (same idiom as `schema.rs::recipe_hash`).
    format!("{:016x}", fnv1a(serde_json::to_string(&canonical(row)).unwrap_or_default().as_bytes()))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Opened catalog database.
pub struct CatalogDb {
    conn: Connection,
}

impl CatalogDb {
    /// Opens (creating and migrating if needed) the catalog at `path`.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        Self::init(Connection::open(path)?)
    }

    /// In-memory catalog (tests, ephemeral tooling).
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, StoreError> {
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(StoreError::Version { found: version });
        }
        if version < 1 {
            conn.execute_batch(MIGRATE_V1)?;
        }
        if version < 2 {
            conn.execute_batch(MIGRATE_V2)?;
        }
        Ok(CatalogDb { conn })
    }

    /// Inserts or replaces a row by id (idempotent re-import). The row must
    /// already carry a [`CatalogRow::with_integrity`] stamp; `verify_integrity`
    /// is what judges it, so a tampered hash is stored as told and then
    /// reported — not silently rewritten.
    pub fn insert(&self, row: &CatalogRow) -> Result<(), StoreError> {
        let tags = serde_json::to_string(&row.tags).map_err(|e| StoreError::Json(e.to_string()))?;
        let class = class_to_text(&row.class)?;
        let blob = row.embedding.as_deref().map(embedding_to_blob);
        self.conn.execute(
            "INSERT OR REPLACE INTO samples (id, path, duration_s, sample_rate, \
             spectral_centroid_hz, flatness, pitch_hz, transient_sharpness, lufs, class, \
             pack, license, source_note, tags, embedding, embedding_dim, pipeline_version, \
             integrity_hash) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, \
             ?14, ?15, ?16, ?17, ?18)",
            rusqlite::params![
                row.id, row.path, row.features.duration_s, row.sample_rate,
                row.features.spectral_centroid_hz, row.features.flatness, row.features.pitch_hz,
                row.features.transient_sharpness, row.features.lufs, class, row.pack, row.license,
                row.source_note, tags, blob, row.embedding_dim, row.pipeline_version,
                row.integrity_hash,
            ],
        )?;
        Ok(())
    }

    /// All rows, id-ascending (deterministic order, always).
    pub fn rows(&self) -> Result<Vec<CatalogRow>, StoreError> {
        let mut stmt = self.conn.prepare(SELECT_ALL)?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            out.push(row_from(r)?);
        }
        Ok(out)
    }

    /// Decoded embedding for `id`, if present. The storage-only seam the
    /// query executor scores through ([`AudioEmbedding`]) — no inference
    /// happens here (#20).
    pub fn embedding_row(&self, id: &str) -> Result<Option<StoredEmbedding>, StoreError> {
        let blob: Option<Vec<u8>> = self
            .conn
            .query_row("SELECT embedding FROM samples WHERE id = ?1", [id], |r| r.get(0))
            .optional()?;
        Ok(blob
            .as_deref()
            .and_then(embedding_from_blob)
            .map(|vector| StoredEmbedding::from(vector)))
    }

    /// Imports a validated catalog document ([`parse_catalog`]): every
    /// sample becomes a row stamped with the mandatory provenance.
    /// Synthesized rows take the documented defaults — `sample_rate`
    /// 48_000, empty `path` — and document embeddings are stored in the
    /// f16 blob format (quantized). Re-importing the same document is
    /// idempotent and hash-stable. Returns the row count written.
    pub fn import_json(&self, json: &str, license: &str, source_note: &str) -> Result<usize, StoreError> {
        let doc = parse_catalog(json)?;
        for s in &doc.samples {
            self.insert(&CatalogRow {
                id: s.id.clone(),
                path: String::new(),
                features: s.features,
                sample_rate: SYNTH_SAMPLE_RATE,
                class: s.class,
                pack: s.pack.clone(),
                license: license.to_string(),
                source_note: source_note.to_string(),
                tags: s.tags.clone(),
                embedding: s.embedding.clone(),
                embedding_dim: s.embedding.as_ref().map_or(0, |v| v.len() as u32),
                pipeline_version: SAMPLE_PIPELINE_VERSION,
                integrity_hash: String::new(),
            }
            .with_integrity())?;
        }
        Ok(doc.samples.len())
    }

    /// Recomputes every row's integrity hash. Returns the number of rows
    /// verified; the first mismatch aborts with [`StoreError::Integrity`].
    pub fn verify_integrity(&self) -> Result<usize, StoreError> {
        let rows = self.rows()?;
        for row in &rows {
            if row.integrity_hash != integrity_hash(row) {
                return Err(StoreError::Integrity { id: row.id.clone() });
            }
        }
        Ok(rows.len())
    }

    /// The query-side view ([`SampleCatalog`]) of every stored row.
    pub fn catalog(&self) -> Result<Vec<SampleCatalog>, StoreError> {
        Ok(self.rows()?.iter().map(CatalogRow::as_catalog).collect())
    }

    /// Records one sample search (issue #30 gap analysis): called by the
    /// query executor after scoring. Failures here must never break a
    /// search — callers may ignore the Result.
    pub fn log_query(&self, entry: &QueryLogEntry) -> Result<(), StoreError> {
        let class = entry.class.as_ref().map(class_to_text).transpose()?;
        self.conn.execute(
            "INSERT INTO query_log (ts_utc, terms, class, result_count, top_score, \
             top_sample_id, pipeline_version) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                entry.ts_utc,
                entry.terms,
                class,
                entry.result_count,
                entry.top_score,
                entry.top_sample_id,
                SAMPLE_PIPELINE_VERSION,
            ],
        )?;
        Ok(())
    }

    /// The gap-analysis mining query (#30): repeated searches grouped by
    /// terms (and class), worst-served first — average top score ascending,
    /// so the head of the list is what the composer asked for and the
    /// library failed to serve. This is the pack-curation interface; the
    /// data accumulates at runtime.
    pub fn worst_served_queries(&self, min_asks: i64, limit: u32) -> Result<Vec<GapCandidate>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT terms, class, COUNT(*) AS asks, AVG(top_score) AS avg_top \
             FROM query_log WHERE result_count = 0 OR top_score IS NOT NULL \
             GROUP BY terms, class HAVING asks >= ?1 \
             ORDER BY avg_top ASC, asks DESC LIMIT ?2",
        )?;
        let mut out = Vec::new();
        let mut rows = stmt.query(rusqlite::params![min_asks, limit])?;
        while let Some(r) = rows.next()? {
            out.push(GapCandidate {
                terms: r.get("terms")?,
                class: r.get::<_, Option<String>>("class")?,
                asks: r.get::<_, i64>("asks")?,
                avg_top_score: r.get("avg_top")?,
            });
        }
        Ok(out)
    }

    /// Total logged queries (the "leave the data" counter for dashboards).
    pub fn query_log_len(&self) -> Result<u64, StoreError> {
        self.conn
            .query_row("SELECT COUNT(*) FROM query_log", [], |r| r.get(0))
            .map_err(Into::into)
    }

    /// Runs a sample query against this catalog and logs the outcome —
    /// the wired path the composer's searches take, so the gap analysis
    /// has data to mine. A logging failure does not fail the search.
    /// Returns an owned summary (the borrowed [`crate::query::QueryResult`]
    /// would not outlive the temporary catalog view).
    pub fn run_and_log_query(
        &self,
        query: &crate::query::SampleQuery,
        ts_utc: &str,
    ) -> Result<QueryOutcome, StoreError> {
        let catalog = self.catalog()?;
        let result = crate::query::run_query(&catalog, query, None);
        let top = result.candidates.first();
        let outcome = QueryOutcome {
            used_fallback: result.used_fallback,
            candidates: result
                .candidates
                .iter()
                .map(|c| (c.sample.id.clone(), c.score))
                .collect(),
        };
        let _ = self.log_query(&QueryLogEntry {
            ts_utc: ts_utc.to_string(),
            terms: query.text_terms.join(" "),
            class: query.class,
            result_count: if result.used_fallback { 0 } else { outcome.candidates.len() as u32 },
            top_score: top.map(|c| c.score),
            top_sample_id: top.map(|c| c.sample.id.clone()),
        });
        Ok(outcome)
    }
}

/// Owned summary of a logged query run.
#[derive(Clone, Debug, PartialEq)]
pub struct QueryOutcome {
    pub used_fallback: bool,
    /// (sample id, score), best first.
    pub candidates: Vec<(String, f32)>,
}

/// One logged sample search.
#[derive(Clone, Debug, PartialEq)]
pub struct QueryLogEntry {
    /// RFC 3339 UTC timestamp, supplied by the caller (no clock dependency
    /// in this crate).
    pub ts_utc: String,
    /// The search text as typed/issued.
    pub terms: String,
    /// Hard class filter, if any.
    pub class: Option<SampleClass>,
    pub result_count: u32,
    /// Best cosine+feature score returned; None when nothing matched.
    pub top_score: Option<f32>,
    pub top_sample_id: Option<String>,
}

/// One mined gap: a repeated query the library serves poorly.
#[derive(Clone, Debug, PartialEq)]
pub struct GapCandidate {
    pub terms: String,
    pub class: Option<String>,
    pub asks: i64,
    pub avg_top_score: f64,
}

/// SampleClass reuses its serde spelling — one vocabulary, no parallel
/// match table to drift.
fn class_to_text(class: &SampleClass) -> Result<String, StoreError> {
    serde_json::to_string(class).map_err(|e| StoreError::Json(e.to_string()))
}

fn row_from(r: &rusqlite::Row<'_>) -> Result<CatalogRow, StoreError> {
    let class: String = r.get("class")?;
    let tags: String = r.get("tags")?;
    let blob: Option<Vec<u8>> = r.get("embedding")?;
    Ok(CatalogRow {
        id: r.get("id")?,
        path: r.get("path")?,
        features: EngineeredFeatures {
            duration_s: r.get("duration_s")?,
            spectral_centroid_hz: r.get("spectral_centroid_hz")?,
            flatness: r.get("flatness")?,
            pitch_hz: r.get("pitch_hz")?,
            transient_sharpness: r.get("transient_sharpness")?,
            lufs: r.get("lufs")?,
        },
        sample_rate: r.get("sample_rate")?,
        class: serde_json::from_str(&class).map_err(|_| column_corrupt("class"))?,
        pack: r.get("pack")?,
        license: r.get("license")?,
        source_note: r.get("source_note")?,
        tags: serde_json::from_str(&tags).map_err(|_| column_corrupt("tags"))?,
        embedding: blob.as_deref().and_then(embedding_from_blob),
        embedding_dim: r.get("embedding_dim")?,
        pipeline_version: r.get("pipeline_version")?,
        integrity_hash: r.get("integrity_hash")?,
    })
}

/// A text column that must be module-written JSON but isn't — surfaces as a
/// typed sqlite error instead of a silent fallback.
fn column_corrupt(column: &'static str) -> rusqlite::Error {
    rusqlite::Error::InvalidColumnType(0, column.to_string(), rusqlite::types::Type::Text)
}
