//! `.kpack` v1 container (#19 build pipeline). Format — all integers
//! little-endian:
//!
//! ```text
//! offset  size  field
//! 0       4     magic `KPAK`
//! 4       2     format version (1)
//! 6       2     reserved (0)
//! 8       4     manifest length N
//! 12      N     manifest JSON (fixed version fields only — no timestamps,
//!               no absolute paths — so identical inputs build identical
//!               bytes forever)
//! 12+N    ..    entries, back to back until 8 bytes remain:
//!                 u32 id length · id UTF-8 · u32 pcm byte length · f32 LE PCM
//! end-8   8     FNV-1a u64 over every preceding byte (trailer)
//! ```
//!
//! Per-entry PCM hashes in the manifest let readers detect bit rot in the
//! audio; the trailer detects corruption anywhere in the container.
//!
//! Embeddings are deliberately NOT in the container: they live in the
//! SQLite catalog ([`crate::store`]) and are filled by the #20 build
//! pipeline after packing. The container is the audio; the catalog is the
//! knowledge.
//!
//! Ingestion (directory walk, WAV decode, −1 dBTP normalization) lives in
//! [`crate::ingest`]; feature extraction in [`crate::features`].

use serde::{Deserialize, Serialize};

use crate::catalog::SampleClass;
use crate::ingest::IngestedSample;

/// Container format version this module reads and writes.
pub const KPACK_VERSION: u16 = 1;

const MAGIC: &[u8; 4] = b"KPAK";
/// magic + version + reserved + manifest length.
const HEADER_LEN: usize = 12;
/// FNV-1a u64.
const TRAILER_LEN: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum PackError {
    #[error("wav decode failed: {0}")]
    Decode(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("bad container header: {0}")]
    Header(String),
    #[error("invalid manifest: {0}")]
    Manifest(String),
    #[error("integrity failure: {0}")]
    Integrity(String),
}

/// Pack provenance — mandatory for every build and import. No `Default`:
/// provenance must be written down, never implied.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackMeta {
    pub pack: String,
    pub license: String,
    pub source: String,
}

/// Container manifest: fixed version fields only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackManifest {
    pub version: u32,
    pub pack: String,
    pub license: String,
    pub source: String,
    pub samples: Vec<ManifestEntry>,
}

/// Manifest record for one entry; `pcm_hash` is FNV-1a (hex) over the
/// entry's f32-LE bytes. `choke_group` (issue #19) assigns the entry to a
/// choke group at attach time; `None` = no choke.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestEntry {
    pub id: String,
    pub class: SampleClass,
    pub sample_rate: u32,
    pub frames: u32,
    pub features: crate::catalog::EngineeredFeatures,
    pub pcm_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choke_group: Option<u8>,
}

/// One decoded entry: its manifest record plus the PCM that hashes to it.
#[derive(Clone, Debug, PartialEq)]
pub struct PackEntry {
    pub meta: ManifestEntry,
    pub pcm: Vec<f32>,
}

/// A loaded, fully verified container.
#[derive(Clone, Debug, PartialEq)]
pub struct Pack {
    pub manifest: PackManifest,
    pub entries: Vec<PackEntry>,
}

/// Builds the container: sorted manifest + entries + FNV-1a trailer. Same
/// samples + meta → identical bytes, always.
pub fn build_pack(samples: &[IngestedSample], meta: &PackMeta) -> Vec<u8> {
    let mut sorted: Vec<&IngestedSample> = samples.iter().collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));

    let manifest = PackManifest {
        version: u32::from(KPACK_VERSION),
        pack: meta.pack.clone(),
        license: meta.license.clone(),
        source: meta.source.clone(),
        samples: sorted
            .iter()
            .map(|s| ManifestEntry {
                id: s.id.clone(),
                class: s.class,
                sample_rate: s.sample_rate,
                frames: s.pcm.len() as u32,
                features: s.features,
                pcm_hash: fnv1a_hex(&pcm_le_bytes(&s.pcm)),
                choke_group: None,
            })
            .collect(),
    };
    let manifest_json = serde_json::to_string(&manifest).unwrap_or_default();
    let mut out = Vec::with_capacity(HEADER_LEN + manifest_json.len() + TRAILER_LEN);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&KPACK_VERSION.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(manifest_json.len() as u32).to_le_bytes());
    out.extend_from_slice(manifest_json.as_bytes());
    for s in &sorted {
        let id = s.id.as_bytes();
        let pcm = pcm_le_bytes(&s.pcm);
        out.extend_from_slice(&(id.len() as u32).to_le_bytes());
        out.extend_from_slice(id);
        out.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
        out.extend_from_slice(&pcm);
    }
    out.extend_from_slice(&fnv1a(&out).to_le_bytes());
    out
}

/// Parses and fully verifies a container: header, trailer, manifest, entry
/// framing and ordering, and every per-entry PCM hash.
pub fn load_pack(bytes: &[u8]) -> Result<Pack, PackError> {
    if bytes.len() < HEADER_LEN + TRAILER_LEN {
        return Err(PackError::Header("shorter than header + trailer".into()));
    }
    if &bytes[..4] != MAGIC {
        return Err(PackError::Header(format!("bad magic {:?}", &bytes[..4])));
    }
    let version = u16_le(bytes, 4);
    if version != KPACK_VERSION {
        return Err(PackError::Header(format!(
            "container version {version} unsupported (want {KPACK_VERSION})"
        )));
    }
    let manifest_len = u32_le(bytes, 8) as usize;
    let manifest_end = HEADER_LEN + manifest_len;
    if manifest_end + TRAILER_LEN > bytes.len() {
        return Err(PackError::Header("manifest extends past the container".into()));
    }
    let body_end = bytes.len() - TRAILER_LEN;
    if u64_le(bytes, body_end) != fnv1a(&bytes[..body_end]) {
        return Err(PackError::Integrity("trailer hash mismatch — container corrupted".into()));
    }
    let manifest: PackManifest = serde_json::from_slice(&bytes[HEADER_LEN..manifest_end])
        .map_err(|e| PackError::Manifest(e.to_string()))?;
    if manifest.version != u32::from(KPACK_VERSION) {
        return Err(PackError::Manifest(format!(
            "manifest version {} unsupported (want {})",
            manifest.version, KPACK_VERSION
        )));
    }

    let mut entries = Vec::with_capacity(manifest.samples.len());
    let mut at = manifest_end;
    for expected in &manifest.samples {
        entries.push(parse_entry(bytes, &mut at, body_end, expected)?);
    }
    if at != body_end {
        return Err(PackError::Integrity(format!(
            "container carries {} trailing bytes beyond the {} manifest entries",
            body_end - at,
            manifest.samples.len()
        )));
    }
    Ok(Pack { manifest, entries })
}

fn parse_entry(
    bytes: &[u8],
    at: &mut usize,
    body_end: usize,
    expected: &ManifestEntry,
) -> Result<PackEntry, PackError> {
    let need = |end: usize| -> Result<(), PackError> {
        if end > body_end {
            Err(PackError::Decode("entry data overruns the container".into()))
        } else {
            Ok(())
        }
    };
    need(*at + 4)?;
    let id_len = u32_le(bytes, *at) as usize;
    *at += 4;
    need(*at + id_len)?;
    let id = std::str::from_utf8(&bytes[*at..*at + id_len])
        .map_err(|_| PackError::Decode("entry id is not UTF-8".into()))?
        .to_string();
    *at += id_len;
    need(*at + 4)?;
    let pcm_len = u32_le(bytes, *at) as usize;
    *at += 4;
    need(*at + pcm_len)?;
    if pcm_len % 4 != 0 {
        return Err(PackError::Decode(format!("pcm length {pcm_len} is not whole f32s")));
    }
    let pcm_start = *at;
    *at += pcm_len;
    let pcm: Vec<f32> = bytes[pcm_start..*at]
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();

    if id != expected.id {
        return Err(PackError::Integrity(format!(
            "entry id `{id}` does not match manifest `{}`",
            expected.id
        )));
    }
    if fnv1a_hex(&bytes[pcm_start..*at]) != expected.pcm_hash {
        return Err(PackError::Integrity(format!("pcm hash mismatch for `{}`", expected.id)));
    }
    if pcm.len() != expected.frames as usize {
        return Err(PackError::Integrity(format!(
            "entry `{}` carries {} frames, manifest says {}",
            expected.id,
            pcm.len(),
            expected.frames
        )));
    }
    Ok(PackEntry { meta: expected.clone(), pcm })
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

fn pcm_le_bytes(pcm: &[f32]) -> Vec<u8> {
    pcm.iter().flat_map(|s| s.to_le_bytes()).collect()
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn fnv1a_hex(bytes: &[u8]) -> String {
    format!("{:016x}", fnv1a(bytes))
}
