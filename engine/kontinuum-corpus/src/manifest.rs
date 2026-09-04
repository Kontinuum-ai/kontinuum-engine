//! The corpus manifest (issue #23): `corpus/manifest.csv` is the single
//! source of truth for what is in the reference corpus. The pipeline
//! reads the manifest and nothing else — it never assumes audio is in the
//! repository, and every missing or undprocessable track is a typed
//! [`ManifestError`], never a silent skip.
//!
//! Format: one header row, then one row per track. Plain CSV — no
//! quoting; fields must not contain commas, quotes, or newlines
//! (validated). Blank lines are skipped.
//!
//! Columns (exact header, in order):
//! `track_id,artist,label,year,bpm,subgenre,why_included,file_path,file_hash,hash_algo,license_proof,synthetic,synth_spec`
//!
//! - `file_path` — audio location OUTSIDE the repo (private bucket
//!   mount / local purchase dir), absolute. Never committed for licensed
//!   tracks.
//! - `file_hash` + `hash_algo` — file integrity; the only supported
//!   algo is `fnv1a64` (hex, the same FNV-1a convention as the samples
//!   store's integrity hashes). This is an integrity check, NOT
//!   cryptographic provenance — purchase provenance lives in
//!   `license_proof` and the #6 memo.
//! - `license_proof` — the audit trail that a track is legally in the
//!   corpus (order id / receipt reference). Required for every row;
//!   synthetic rows carry the in-repo generator statement.
//! - `synthetic` — `true` for the in-repo fixture corpus; such rows are
//!   rendered deterministically by `kontinuum-analysis::synthgen` from
//!   `synth_spec` (a preset id) and need no file on disk.
//!
//! The checked-in manifest ships ONLY synthetic rows. Real rows are
//! appended when purchasing (#6 legal memo first) lands — see
//! `corpus/README.md` for the procedure.

use std::path::Path;

use serde::{Deserialize, Serialize};

pub const HEADER: &str = "track_id,artist,label,year,bpm,subgenre,why_included,\
file_path,file_hash,hash_algo,license_proof,synthetic,synth_spec";

pub const HASH_ALGO: &str = "fnv1a64";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ManifestRow {
    pub track_id: String,
    pub artist: String,
    pub label: String,
    pub year: u16,
    pub bpm: f32,
    pub subgenre: String,
    pub why_included: String,
    /// Empty for synthetic rows (they render from `synth_spec`).
    pub file_path: String,
    /// Empty for synthetic rows; `fnv1a64` hex of the file bytes for real
    /// tracks.
    pub file_hash: String,
    pub hash_algo: String,
    pub license_proof: String,
    pub synthetic: bool,
    /// Preset id for synthetic rows (`kontinuum_analysis::synthgen`).
    pub synth_spec: String,
}

#[derive(Clone, Debug)]
pub struct Manifest {
    pub tracks: Vec<ManifestRow>,
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("manifest is empty (need the header row)")]
    Empty,
    #[error("manifest header mismatch: want \"{want}\"")]
    Header { want: &'static str },
    #[error("manifest line {line}: {source}")]
    Row {
        line: usize,
        source: RowError,
    },
    #[error("manifest line {line}: field contains a comma, quote, or control character")]
    IllegalCharacter { line: usize },
    #[error("duplicate track_id '{0}'")]
    DuplicateTrackId(String),
    #[error("track {track_id}: file_path '{path}' must be absolute — licensed audio lives outside the repo")]
    AudioPathNotAbsolute { track_id: String, path: String },
    #[error("track {track_id}: audio unreadable: {detail}")]
    IoRow { track_id: String, detail: String },
    #[error("track {track_id}: file hash mismatch (manifest {want}, file {got}) — re-hash or re-purchase")]
    HashMismatch { track_id: String, want: String, got: String },
}

#[derive(Debug, thiserror::Error)]
pub enum RowError {
    #[error("want 13 columns, got {got}")]
    ColumnCount { got: usize },
    #[error("field '{field}' must not be empty")]
    EmptyField { field: &'static str },
    #[error("bad year: {0}")]
    Year(String),
    #[error("bad bpm (want 60..=200): {0}")]
    Bpm(String),
    #[error("bad boolean (want true/false): {0}")]
    Bool(String),
    #[error("synthetic rows need a synth_spec preset id")]
    MissingSynthSpec,
    #[error("unsupported hash_algo '{0}' (only '{1}')")]
    HashAlgo(String, &'static str),
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Manifest, ManifestError> {
        let mut lines = text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty());
        let Some((_, header)) = lines.next() else {
            return Err(ManifestError::Empty);
        };
        if header.trim() != HEADER {
            return Err(ManifestError::Header { want: HEADER });
        }
        let mut tracks = Vec::new();
        for (idx, line) in lines {
            let line_no = idx + 1;
            for ch in line.chars() {
                if matches!(ch, '"' | '\'' | '\t' | '\r') || ch.is_control() {
                    return Err(ManifestError::IllegalCharacter { line: line_no });
                }
            }
            let cols: Vec<&str> = line.split(',').map(str::trim).collect();
            let row = parse_row(&cols).map_err(|source| ManifestError::Row { line: line_no, source })?;
            if tracks.iter().any(|t: &ManifestRow| t.track_id == row.track_id) {
                return Err(ManifestError::DuplicateTrackId(row.track_id));
            }
            tracks.push(row);
        }
        Ok(Manifest { tracks })
    }

    /// Resolves a row's audio. Synthetic rows render in-process (the
    /// pipeline handles that) and touch no filesystem; real rows must be
    /// an absolute path outside the repo whose bytes hash to the
    /// manifest's `file_hash`.
    pub fn resolve_audio(&self, row: &ManifestRow) -> Result<Vec<u8>, ManifestError> {
        if row.synthetic {
            return Ok(Vec::new());
        }
        if !Path::new(&row.file_path).is_absolute() {
            return Err(ManifestError::AudioPathNotAbsolute {
                track_id: row.track_id.clone(),
                path: row.file_path.clone(),
            });
        }
        let bytes = std::fs::read(&row.file_path).map_err(|e| ManifestError::IoRow {
            track_id: row.track_id.clone(),
            detail: e.to_string(),
        })?;
        let got = fnv1a64_hex(&bytes);
        if got != row.file_hash {
            return Err(ManifestError::HashMismatch {
                track_id: row.track_id.clone(),
                want: row.file_hash.clone(),
                got,
            });
        }
        Ok(bytes)
    }
}

fn parse_row(cols: &[&str]) -> Result<ManifestRow, RowError> {
    if cols.len() != 13 {
        return Err(RowError::ColumnCount { got: cols.len() });
    }
    let field = |i: usize| cols[i];
    let non_empty = |i: usize, field_name: &'static str| -> Result<String, RowError> {
        let v = field(i);
        if v.is_empty() {
            Err(RowError::EmptyField { field: field_name })
        } else {
            Ok(v.to_string())
        }
    };
    let year = field(3)
        .parse::<u16>()
        .map_err(|_| RowError::Year(field(3).to_string()))?;
    let bpm = field(4)
        .parse::<f32>()
        .ok()
        .filter(|b| (60.0..=200.0).contains(b))
        .ok_or_else(|| RowError::Bpm(field(4).to_string()))?;
    let synthetic = match field(11) {
        "true" => true,
        "false" => false,
        other => return Err(RowError::Bool(other.to_string())),
    };
    let (file_path, file_hash, license_proof) = if synthetic {
        if field(12).is_empty() {
            return Err(RowError::MissingSynthSpec);
        }
        (String::new(), String::new(), non_empty(10, "license_proof")?)
    } else {
        let path = non_empty(7, "file_path")?;
        let hash = non_empty(8, "file_hash")?;
        let proof = non_empty(10, "license_proof")?;
        (path, hash, proof)
    };
    if !synthetic && field(9) != HASH_ALGO {
        return Err(RowError::HashAlgo(field(9).to_string(), HASH_ALGO));
    }
    Ok(ManifestRow {
        track_id: non_empty(0, "track_id")?,
        artist: non_empty(1, "artist")?,
        label: non_empty(2, "label")?,
        year,
        bpm,
        subgenre: non_empty(5, "subgenre")?,
        why_included: non_empty(6, "why_included")?,
        file_path,
        file_hash,
        hash_algo: field(9).to_string(),
        license_proof,
        synthetic,
        synth_spec: field(12).to_string(),
    })
}

/// FNV-1a 64-bit, hex-encoded — the samples store's integrity-hash
/// convention (`kontinuum-samples` schema.rs recipe_hash style).
pub fn fnv1a64_hex(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_row(id: &str, subgenre: &str, spec: &str) -> String {
        format!(
            "{id},Kontinuum Fixture,fixture-lab,2026,128.0,{subgenre},\
synthetic reference track for pipeline self-test,,,fnv1a64,\
synthetic: generated in-repo by kontinuum-analysis synthgen; no third-party content,true,{spec}"
        )
    }

    fn manifest_with(rows: Vec<String>) -> String {
        let mut text = String::from(HEADER);
        text.push('\n');
        for r in rows {
            text.push_str(&r);
            text.push('\n');
        }
        text
    }

    #[test]
    fn synthetic_rows_parse() {
        let m = Manifest::parse(&manifest_with(vec![
            synthetic_row("syn-mt-a", "minimal-techno", "mt-a"),
            synthetic_row("syn-mh-a", "microhouse", "mh-a"),
        ]))
        .unwrap();
        assert_eq!(m.tracks.len(), 2);
        assert!(m.tracks[0].synthetic);
        assert_eq!(m.tracks[0].synth_spec, "mt-a");
        assert_eq!(m.tracks[0].bpm, 128.0);
        assert_eq!(m.tracks[1].subgenre, "microhouse");
    }

    #[test]
    fn header_and_shape_are_enforced() {
        assert!(matches!(Manifest::parse(""), Err(ManifestError::Empty)));
        assert!(matches!(
            Manifest::parse("track_id,artist\nx,y"),
            Err(ManifestError::Header { .. })
        ));
        let bad = format!("{HEADER}\nonly-one-column\n");
        assert!(matches!(
            Manifest::parse(&bad),
            Err(ManifestError::Row { line: 2, source: RowError::ColumnCount { got: 1 } })
        ));
    }

    #[test]
    fn real_rows_demand_provenance() {
        let hash = fnv1a64_hex(b"fake-bytes");
        let real = format!(
            "{HEADER}\n\
real-01,Real Artist,Real Label,2024,127.0,minimal-techno,era spread,\
/private/corpus-bucket/real-01.wav,\
{hash},fnv1a64,beatport-order-123,false,\n"
        );
        let m = Manifest::parse(&real).unwrap();
        assert!(!m.tracks[0].synthetic);

        // Missing hash → typed row error naming the empty field.
        let no_hash = real.replace(&format!("{hash},fnv1a64"), ",fnv1a64");
        assert!(matches!(
            Manifest::parse(&no_hash),
            Err(ManifestError::Row { source: RowError::EmptyField { field: "file_hash" }, .. })
        ));

        // Wrong algo → typed row error.
        let bad_algo = real.replace("fnv1a64,beatport", "md5,beatport");
        assert!(matches!(
            Manifest::parse(&bad_algo),
            Err(ManifestError::Row { source: RowError::HashAlgo(_, "fnv1a64"), .. })
        ));

        // Relative path → typed resolve error.
        let relative = real.replace("/private/corpus-bucket/real-01.wav", "audio/real-01.wav");
        let m = Manifest::parse(&relative).unwrap();
        assert!(matches!(
            m.resolve_audio(&m.tracks[0]),
            Err(ManifestError::AudioPathNotAbsolute { .. })
        ));

        // Missing file → typed resolve error naming the track.
        let m = Manifest::parse(&real).unwrap();
        assert!(matches!(
            m.resolve_audio(&m.tracks[0]),
            Err(ManifestError::IoRow { track_id, .. }) if track_id == "real-01"
        ));
    }

    #[test]
    fn resolve_audio_lets_synthetic_rows_skip_the_filesystem() {
        let m = Manifest::parse(&manifest_with(vec![synthetic_row("syn-mt-a", "minimal-techno", "mt-a")])).unwrap();
        assert_eq!(m.resolve_audio(&m.tracks[0]).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn duplicate_ids_and_illegal_characters_are_rejected() {
        let dup = manifest_with(vec![synthetic_row("syn-mt-a", "minimal-techno", "mt-a"), synthetic_row("syn-mt-a", "minimal-techno", "mt-b")]);
        assert!(matches!(
            Manifest::parse(&dup),
            Err(ManifestError::DuplicateTrackId(_))
        ));
        let quoted = format!("{HEADER}\n\"sneaky\",a,b,2024,128.0,s,w,,fnv1a64,p,true,mt-a\n");
        assert!(matches!(
            Manifest::parse(&quoted),
            Err(ManifestError::IllegalCharacter { line: 2 })
        ));
    }

    #[test]
    fn fnv1a_matches_the_known_vectors() {
        assert_eq!(fnv1a64_hex(b""), "cbf29ce484222325");
        assert_eq!(fnv1a64_hex(b"a"), "af63dc4c8601ec8c");
    }
}
