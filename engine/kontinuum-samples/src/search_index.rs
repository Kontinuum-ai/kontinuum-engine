//! On-device sample vector index (#20): a flat, brute-force cosine index
//! over the curated library — the honest v1. Hundreds to a few thousand
//! rows scan in well under a millisecond (a dot product per row), so the
//! ANN machinery the issue's bake-off measures (HNSW / IVF-PQ) is deferred,
//! not missing: `search_by_vector` is the seam. An upgrade swaps the scan
//! loop behind that signature; the persisted format is independent of the
//! scan strategy, so flat-file indexes stay readable across the swap until
//! an explicit format-version bump.
//!
//! Stored format — all integers little-endian, sibling of `pack.rs`:
//!
//! ```text
//! offset  size  field
//! 0       4     magic `KSID`
//! 4       2     format version (1)
//! 6       2     reserved (0)
//! 8       4     header JSON length N
//! 12      N     header JSON: embedder_id, embedder_version, dim
//!               (fixed fields only — no timestamps, no paths)
//! 12+N    ..    entries, back to back until 8 bytes remain:
//!                 u32 id len · id UTF-8 · u32 hash len · hash UTF-8 ·
//!                 u32 vector byte length · f32 LE vector
//! end-8   8     FNV-1a u64 over every preceding byte (trailer)
//! ```
//!
//! Determinism: entries are written id-ascending and parse rejects any
//! other order, so the same library + embedder always produces identical
//! bytes (content-hash addressable — the trailer is the document's content
//! hash). Per-entry `content_hash` pins the *source sample* the vector was
//! computed from (canonical id/features/class/pack/tags — the same recipe
//! and seed render bit-identical audio, so content addressing needs no
//! timestamps); a re-render that changes the sample changes the hash and
//! the index rebuild naturally.
//!
//! Invalidation: the header carries the embedder stamp (id + version +
//! dim). [`VectorIndex::open`] against a different stamp is a typed error —
//! an index built by another embedder is meaningless, never silently
//! reused. The app stores the file next to its catalog database (engine
//! crates stay path-agnostic, per the `CatalogDb::open(path)` convention).

use serde::{Deserialize, Serialize};

use crate::catalog::{SampleCatalog, SampleClass};
use crate::embedder::SampleEmbedder;

/// Index format version this module reads and writes.
pub const INDEX_FORMAT_VERSION: u16 = 1;

const MAGIC: &[u8; 4] = b"KSID";
/// magic + version + reserved + header length.
const HEADER_LEN: usize = 12;
/// FNV-1a u64.
const TRAILER_LEN: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("bad index header: {0}")]
    Header(String),
    #[error("invalid index header: {0}")]
    HeaderJson(String),
    #[error("entry decode failed: {0}")]
    Decode(String),
    #[error("index integrity failure: {0}")]
    Integrity(String),
    #[error(
        "index was built by embedder `{found_id}` v{found_version}, want `{want_id}` \
         v{want_version} — rebuild the index"
    )]
    EmbedderMismatch {
        found_id: String,
        found_version: u32,
        want_id: &'static str,
        want_version: u32,
    },
    #[error("index dim {found} does not match embedder dim {want}")]
    DimMismatch { found: usize, want: usize },
    #[error("sample `{0}` is not in the index")]
    UnknownSample(String),
}

/// One index row: the sample it answers for, the content hash of the source
/// sample it was computed from, and the embedding vector.
#[derive(Clone, Debug, PartialEq)]
pub struct IndexEntry {
    pub id: String,
    pub content_hash: String,
    pub vector: Vec<f32>,
}

/// One retrieval hit: sample identity and cosine similarity (0..=1).
#[derive(Clone, Debug, PartialEq)]
pub struct Scored {
    pub id: String,
    pub content_hash: String,
    pub score: f32,
}

/// Header stamped into the document (strict on parse — `deny_unknown_fields`,
/// like every catalog/recipe document in the crate).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexHeader {
    embedder_id: String,
    embedder_version: u32,
    dim: usize,
}

/// The built index: entries sorted by id, ready to search or serialize.
#[derive(Clone, Debug, PartialEq)]
pub struct VectorIndex {
    header: IndexHeader,
    entries: Vec<IndexEntry>,
}

/// Canonical content projection of a sample — the hash input for
/// [`content_hash_of`]. Field order is the canonical form; the derived
/// embedding is excluded (it is computed *from* this content).
#[derive(Serialize)]
struct CanonicalSample<'a> {
    id: &'a str,
    features: &'a crate::catalog::EngineeredFeatures,
    class: &'a SampleClass,
    pack: &'a str,
    tags: &'a [String],
}

/// FNV-1a hex over the sample's canonical content: same sample content →
/// same hash, always (the `store.rs` integrity-hash idiom, minus storage
/// fields the index never sees).
pub fn content_hash_of(sample: &SampleCatalog) -> String {
    let canonical = CanonicalSample {
        id: &sample.id,
        features: &sample.features,
        class: &sample.class,
        pack: &sample.pack,
        tags: &sample.tags,
    };
    // A validated catalog sample serializes cleanly; the fallback keeps the
    // hash total (same idiom as `store.rs::integrity_hash`).
    let json = serde_json::to_string(&canonical).unwrap_or_default();
    format!("{:016x}", kontinuum_core::fnv1a64(json.as_bytes()))
}

impl VectorIndex {
    /// Embeds every sample with `embedder` and assembles the index.
    /// Deterministic: entries are ordered by id, so the same catalog and
    /// embedder always produce the same index (and the same bytes).
    pub fn build(embedder: &dyn SampleEmbedder, samples: &[SampleCatalog]) -> VectorIndex {
        let mut rows: Vec<IndexEntry> = samples
            .iter()
            .map(|s| IndexEntry {
                id: s.id.clone(),
                content_hash: content_hash_of(s),
                vector: embedder.embed_sample(s),
            })
            .collect();
        rows.sort_by(|a, b| a.id.cmp(&b.id));
        VectorIndex {
            header: IndexHeader {
                embedder_id: embedder.id().to_string(),
                embedder_version: embedder.version(),
                dim: embedder.dim(),
            },
            entries: rows,
        }
    }

    /// The embedder stamp this index was built with.
    pub fn embedder_stamp(&self) -> (&str, u32, usize) {
        (&self.header.embedder_id, self.header.embedder_version, self.header.dim)
    }

    /// All entries, id-ascending.
    pub fn entries(&self) -> &[IndexEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Serializes to the deterministic document form (module docs). Same
    /// index → identical bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let header_json = serde_json::to_string(&self.header).unwrap_or_default();
        let mut out = Vec::with_capacity(HEADER_LEN + header_json.len() + TRAILER_LEN);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&INDEX_FORMAT_VERSION.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(header_json.len() as u32).to_le_bytes());
        out.extend_from_slice(header_json.as_bytes());
        for e in &self.entries {
            let id = e.id.as_bytes();
            out.extend_from_slice(&(id.len() as u32).to_le_bytes());
            out.extend_from_slice(id);
            let hash = e.content_hash.as_bytes();
            out.extend_from_slice(&(hash.len() as u32).to_le_bytes());
            out.extend_from_slice(hash);
            out.extend_from_slice(&((e.vector.len() * 4) as u32).to_le_bytes());
            for s in &e.vector {
                out.extend_from_slice(&s.to_le_bytes());
            }
        }
        out.extend_from_slice(&kontinuum_core::fnv1a64(&out).to_le_bytes());
        out
    }

    /// Parses and fully verifies a document: header, trailer, strict JSON,
    /// entry framing, id ordering, and vector-length consistency.
    pub fn decode(bytes: &[u8]) -> Result<VectorIndex, IndexError> {
        if bytes.len() < HEADER_LEN + TRAILER_LEN {
            return Err(IndexError::Header("shorter than header + trailer".into()));
        }
        if &bytes[..4] != MAGIC {
            return Err(IndexError::Header(format!("bad magic {:?}", &bytes[..4])));
        }
        let version = u16_le(bytes, 4);
        if version != INDEX_FORMAT_VERSION {
            return Err(IndexError::Header(format!(
                "index format version {version} unsupported (want {INDEX_FORMAT_VERSION})"
            )));
        }
        let header_len = u32_le(bytes, 8) as usize;
        let header_end = HEADER_LEN + header_len;
        if header_end + TRAILER_LEN > bytes.len() {
            return Err(IndexError::Header("header extends past the document".into()));
        }
        let body_end = bytes.len() - TRAILER_LEN;
        if u64_le(bytes, body_end) != kontinuum_core::fnv1a64(&bytes[..body_end]) {
            return Err(IndexError::Integrity(
                "trailer hash mismatch — index corrupted".into(),
            ));
        }
        let header: IndexHeader = serde_json::from_slice(&bytes[HEADER_LEN..header_end])
            .map_err(|e| IndexError::HeaderJson(e.to_string()))?;

        let mut entries = Vec::new();
        let mut at = header_end;
        let mut prev_id = String::new();
        while at < body_end {
            let id = read_len_str(bytes, &mut at, body_end)?;
            if id <= prev_id {
                return Err(IndexError::Decode(format!(
                    "entries must be strictly id-ascending (`{id}` after `{prev_id}`)"
                )));
            }
            prev_id = id.clone();
            let content_hash = read_len_str(bytes, &mut at, body_end)?;
            let vec_bytes = read_len_slice(bytes, &mut at, body_end)?;
            if vec_bytes.is_empty() || vec_bytes.len() % 4 != 0 {
                return Err(IndexError::Decode(format!(
                    "entry `{id}` vector length {} is not whole f32s",
                    vec_bytes.len()
                )));
            }
            if vec_bytes.len() != header.dim * 4 {
                return Err(IndexError::Decode(format!(
                    "entry `{id}` carries {} floats, header dim is {}",
                    vec_bytes.len() / 4,
                    header.dim
                )));
            }
            entries.push(IndexEntry {
                id,
                content_hash,
                vector: vec_bytes
                    .chunks_exact(4)
                    .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                    .collect(),
            });
        }
        Ok(VectorIndex { header, entries })
    }

    /// Decode + embedder-stamp check: the invalidation gate. An index built
    /// by any other embedder id, version, or dimension is rejected as a
    /// typed error so callers rebuild instead of scoring across vector
    /// spaces (pinned by test).
    pub fn open(embedder: &dyn SampleEmbedder, bytes: &[u8]) -> Result<VectorIndex, IndexError> {
        let index = VectorIndex::decode(bytes)?;
        let (found_id, found_version, found_dim) = index.embedder_stamp();
        if found_id != embedder.id() || found_version != embedder.version() {
            return Err(IndexError::EmbedderMismatch {
                found_id: found_id.to_string(),
                found_version,
                want_id: embedder.id(),
                want_version: embedder.version(),
            });
        }
        if found_dim != embedder.dim() {
            return Err(IndexError::DimMismatch {
                found: found_dim,
                want: embedder.dim(),
            });
        }
        Ok(index)
    }

    /// Core scan: cosine similarity of `query` against every row, top-k,
    /// descending, ties broken by id ascending (same ordering discipline as
    /// `query::run_query`). Length-mismatched or zero queries score nothing.
    pub fn search_by_vector(&self, query: &[f32], k: usize) -> Vec<Scored> {
        let mut ranked: Vec<Scored> = self
            .entries
            .iter()
            .map(|e| Scored {
                id: e.id.clone(),
                content_hash: e.content_hash.clone(),
                score: cosine(query, &e.vector),
            })
            .collect();
        ranked.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });
        ranked.truncate(k);
        ranked
    }

    /// Text→samples: embeds `text` with `embedder` and scans.
    pub fn search_text(
        &self,
        embedder: &dyn SampleEmbedder,
        text: &str,
        k: usize,
    ) -> Result<Vec<Scored>, IndexError> {
        self.check_stamp(embedder)?;
        Ok(self.search_by_vector(&embedder.embed_text(text), k))
    }

    /// By-example: the stored rows nearest the named sample, reference
    /// excluded. The planner's "find me hats like this" path.
    pub fn search_by_example(&self, id: &str, k: usize) -> Result<Vec<Scored>, IndexError> {
        let reference = self
            .entries
            .iter()
            .find(|e| e.id == id)
            .ok_or_else(|| IndexError::UnknownSample(id.to_string()))?;
        Ok(self
            .search_by_vector(&reference.vector, k + 1)
            .into_iter()
            .filter(|s| s.id != id)
            .take(k)
            .collect())
    }

    fn check_stamp(&self, embedder: &dyn SampleEmbedder) -> Result<(), IndexError> {
        let (found_id, found_version, found_dim) = self.embedder_stamp();
        if found_id != embedder.id() || found_version != embedder.version() {
            return Err(IndexError::EmbedderMismatch {
                found_id: found_id.to_string(),
                found_version,
                want_id: embedder.id(),
                want_version: embedder.version(),
            });
        }
        if found_dim != embedder.dim() {
            return Err(IndexError::DimMismatch {
                found: found_dim,
                want: embedder.dim(),
            });
        }
        Ok(())
    }
}

fn read_len_str(bytes: &[u8], at: &mut usize, end: usize) -> Result<String, IndexError> {
    let slice = read_len_slice(bytes, at, end)?;
    std::str::from_utf8(slice)
        .map(|s| s.to_string())
        .map_err(|_| IndexError::Decode("length-prefixed field is not UTF-8".into()))
}

fn read_len_slice<'a>(
    bytes: &'a [u8],
    at: &mut usize,
    end: usize,
) -> Result<&'a [u8], IndexError> {
    if *at + 4 > end {
        return Err(IndexError::Decode("length prefix overruns the document".into()));
    }
    let len = u32_le(bytes, *at) as usize;
    *at += 4;
    if *at + len > end {
        return Err(IndexError::Decode("field data overruns the document".into()));
    }
    let out = &bytes[*at..*at + len];
    *at += len;
    Ok(out)
}

fn u16_le(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn u64_le(b: &[u8], at: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(a)
}

/// Cosine similarity clamped to 0..=1 (negative correlation contributes
/// nothing); mismatched or zero vectors contribute nothing — the same
/// contract as `query::cosine`.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::EngineeredFeatures;
    use crate::embedder::HashedBagEmbedder;

    fn sample(id: &str, class: SampleClass, centroid: f32) -> SampleCatalog {
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
            tags: vec![],
            embedding: None,
        }
    }

    /// A second embedder identity, for stamp-mismatch tests.
    struct FakeEmbedderV2;
    impl SampleEmbedder for FakeEmbedderV2 {
        fn id(&self) -> &'static str {
            "hashed-bag"
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
        fn embed_audio(
            &self,
            f: &EngineeredFeatures,
            class: SampleClass,
        ) -> Vec<f32> {
            HashedBagEmbedder.embed_audio(f, class)
        }
    }

    #[test]
    fn build_is_deterministic_byte_for_byte() {
        let samples = vec![
            sample("b.hat", SampleClass::Hat, 19_000.0),
            sample("a.kick", SampleClass::Kick, 80.0),
        ];
        let a = VectorIndex::build(&HashedBagEmbedder, &samples).to_bytes();
        let b = VectorIndex::build(&HashedBagEmbedder, &samples).to_bytes();
        assert_eq!(a, b, "same catalog + embedder → identical bytes");
        // Order of the input must not matter.
        let reversed = VectorIndex::build(&HashedBagEmbedder, &samples.iter().rev().cloned().collect::<Vec<_>>());
        assert_eq!(reversed.to_bytes(), a, "build normalizes entry order");
    }

    #[test]
    fn roundtrip_preserves_entries() {
        let samples = vec![
            sample("a.kick", SampleClass::Kick, 80.0),
            sample("b.hat", SampleClass::Hat, 19_000.0),
        ];
        let index = VectorIndex::build(&HashedBagEmbedder, &samples);
        let decoded = VectorIndex::decode(&index.to_bytes()).expect("roundtrip");
        assert_eq!(decoded, index);
    }

    #[test]
    fn open_rejects_foreign_embedder_stamp() {
        let index = VectorIndex::build(&HashedBagEmbedder, &[sample("a.kick", SampleClass::Kick, 80.0)]);
        let bytes = index.to_bytes();
        // Same identity, newer version → the index is invalid, typed error.
        let err = VectorIndex::open(&FakeEmbedderV2, &bytes).unwrap_err();
        assert!(
            matches!(err, IndexError::EmbedderMismatch { ref found_version, want_version: 2, .. }
                if *found_version == 1),
            "got {err:?}"
        );
    }

    #[test]
    fn corrupted_bytes_fail_integrity() {
        let index = VectorIndex::build(&HashedBagEmbedder, &[sample("a.kick", SampleClass::Kick, 80.0)]);
        let mut bytes = index.to_bytes();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        assert!(matches!(VectorIndex::decode(&bytes), Err(IndexError::Integrity(_))));
    }

    #[test]
    fn unordered_entries_rejected_on_decode() {
        // Hand-assemble a document whose entries descend: parse must refuse
        // rather than preserve an order-dependent index.
        let index = VectorIndex::build(
            &HashedBagEmbedder,
            &[sample("a.kick", SampleClass::Kick, 80.0), sample("b.hat", SampleClass::Hat, 19_000.0)],
        );
        let mut swapped = index.clone();
        swapped.entries.reverse();
        let bytes = swapped.to_bytes();
        assert!(matches!(VectorIndex::decode(&bytes), Err(IndexError::Decode(_))));
    }

    #[test]
    fn content_hash_tracks_sample_content() {
        let mut a = sample("x", SampleClass::Kick, 80.0);
        let h0 = content_hash_of(&a);
        a.tags.push("punchy".into());
        assert_ne!(content_hash_of(&a), h0, "content change → hash change");
        assert_eq!(content_hash_of(&a), content_hash_of(&a), "stable");
    }
}
