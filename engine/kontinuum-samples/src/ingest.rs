//! Pack ingestion (#19 build pipeline): walk a pack directory, decode to
//! mono, normalize, and hand analyzed samples to the pack builder.
//!
//! Ingest convention: a pack directory contains ONLY class subdirectories
//! (`kick`, `hat`, `perc`, `pad`, `texture`) of WAV files, iterated in
//! sorted order for determinism. Unknown subdirectories are hard errors;
//! non-WAV files are ignored (DAW droppings, `.DS_Store`, license.txt).
//! Entry ids are `{class-dir-name}.{file stem}`.
//!
//! Provenance ([`crate::pack::PackMeta`]) applies when ingested samples are
//! packed and catalogued — decoding is provenance-free.

use std::path::{Path, PathBuf};

use crate::catalog::SampleClass;
use crate::features::analyze_features;

/// Normalization target for ingested material: −1 dBTP, applied as a single
/// scalar gain (dynamics preserved — no limiting, sample-peak only).
pub const TARGET_DBTP: f32 = -1.0;

/// One ingested, normalized, analyzed sample: everything
/// [`crate::pack::build_pack`] and the catalog row need.
#[derive(Clone, Debug, PartialEq)]
pub struct IngestedSample {
    pub id: String,
    pub class: SampleClass,
    pub sample_rate: u32,
    pub features: crate::catalog::EngineeredFeatures,
    pub pcm: Vec<f32>,
}

/// Scales `pcm` by one scalar so the sample peak hits `target_peak` (linear;
/// pass `10f32.powf(TARGET_DBTP / 20.0)` for the pipeline's −1 dBTP). Peak
/// ratios between samples are preserved, so dynamics survive untouched;
/// digital silence stays silent (gain 1.0 — there is nothing to lift and
/// scaling it would be a division by zero).
pub fn normalize_to_target_peak(pcm: &mut [f32], target_peak: f32) {
    let peak = pcm.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak == 0.0 || peak == target_peak {
        return;
    }
    let gain = target_peak / peak;
    for s in pcm.iter_mut() {
        *s *= gain;
    }
}

/// Ingests a pack directory: class subdirectories of WAVs in sorted order,
/// unknown subdirectories rejected, non-WAV files ignored. Every sample is
/// decoded to mono, normalized to −1 dBTP, and analyzed.
pub fn ingest_dir(dir: &Path) -> Result<Vec<IngestedSample>, crate::pack::PackError> {
    let mut out = Vec::new();
    for (name, class, path) in class_dirs(dir)? {
        let mut wavs: Vec<PathBuf> = std::fs::read_dir(&path)?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.extension().map_or(false, |x| x.eq_ignore_ascii_case("wav"))
            })
            .collect();
        wavs.sort();
        for wav in wavs {
            let (mut pcm, sample_rate) = decode_wav_mono(&wav)?;
            normalize_to_target_peak(&mut pcm, 10f32.powf(TARGET_DBTP / 20.0));
            let stem = wav
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            out.push(IngestedSample {
                id: format!("{name}.{stem}"),
                class,
                sample_rate,
                features: analyze_features(&pcm, sample_rate),
                pcm,
            });
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// Sorted class subdirectories; anything else is a hard error.
fn class_dirs(dir: &Path) -> Result<Vec<(String, SampleClass, PathBuf)>, crate::pack::PackError> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|e| e.path())
        .collect();
    entries.sort();
    let mut dirs = Vec::new();
    for path in entries {
        if !path.is_dir() {
            continue; // stray top-level files are ignored
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let class = match name.as_str() {
            "kick" => SampleClass::Kick,
            "hat" => SampleClass::Hat,
            "perc" => SampleClass::Perc,
            "pad" => SampleClass::Pad,
            "texture" => SampleClass::Texture,
            other => {
                return Err(crate::pack::PackError::Manifest(format!(
                    "unexpected pack subdirectory `{other}`; pack directories contain \
                     only kick/hat/perc/pad/texture"
                )))
            }
        };
        dirs.push((name, class, path));
    }
    Ok(dirs)
}

/// hound decode to mono f32 (int WAVs normalize via i32, matching
/// `kontinuum-compose::reference`), averaging interleaved channels.
fn decode_wav_mono(path: &Path) -> Result<(Vec<f32>, u32), crate::pack::PackError> {
    let reader =
        hound::WavReader::open(path).map_err(|e| crate::pack::PackError::Decode(e.to_string()))?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    let mut interleaved = Vec::new();
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for s in reader.into_samples::<f32>() {
                interleaved
                    .push(s.map_err(|e| crate::pack::PackError::Decode(e.to_string()))?);
            }
        }
        _ => {
            for s in reader.into_samples::<i32>() {
                // IntoSamples<i32> normalizes any int depth to i32.
                let s = s.map_err(|e| crate::pack::PackError::Decode(e.to_string()))?;
                interleaved.push(s as f32 / 2_147_483_648.0);
            }
        }
    }
    let mono = interleaved
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
        .collect();
    Ok((mono, spec.sample_rate))
}
