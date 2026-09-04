//! genpack (issue #19 curated library): render the checked-in recipe set
//! into the starter sample library — one 16-bit WAV per recipe plus a
//! `manifest.json` carrying provenance, engineered features, PCM hashes,
//! and choke-group assignments.
//!
//! Path taken (Nick's sourcing strategy, issue #19 comment): the library is
//! SELF-GENERATED — every one-shot is the engine's own synth voices driven
//! through `kontinuum_samples::render_recipe`, so licensing is trivially
//! clean and the whole set is reproducible from the recipes: recipe + seed
//! = bit-identical WAVs. Rebuild with:
//!
//! ```text
//! cargo run --release -p kontinuum-offline --bin genpack assets/samples/recipes assets/samples
//! ```
//!
//! Choke groups ride recipe tags: a `choke:N` tag assigns the entry to
//! group N (the hat pair shares group 1 so open/closed cut each other).
//! The class tag (kick/hat/perc/pad/texture) becomes the manifest class.

use std::path::Path;
use std::process::ExitCode;

use kontinuum_samples::catalog::SampleClass;
use kontinuum_samples::pack::{ManifestEntry, PackManifest};
use kontinuum_samples::{analyze_features, render_recipe, validate, SampleRecipe};

const PACK_NAME: &str = "kontinuum-starter-v1";
const LICENSE: &str = "CC0-1.0 (self-generated in-repo from assets/samples/recipes)";
const SOURCE: &str = "rendered by kontinuum-offline genpack via kontinuum_samples::render_recipe";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: genpack <recipes_dir> <out_dir>");
        return ExitCode::FAILURE;
    }
    match run(Path::new(&args[1]), Path::new(&args[2])) {
        Ok(count) => {
            println!("genpack: wrote {count} entries");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("genpack failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(recipes_dir: &Path, out_dir: &Path) -> Result<usize, String> {
    let mut recipe_paths: Vec<_> = std::fs::read_dir(recipes_dir)
        .map_err(|e| format!("read {}: {e}", recipes_dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    recipe_paths.sort();
    if recipe_paths.is_empty() {
        return Err(format!("no recipes in {}", recipes_dir.display()));
    }

    let mut entries = Vec::new();
    for path in &recipe_paths {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("bad stem: {}", path.display()))?
            .to_owned();
        let doc = std::fs::read_to_string(path)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        let recipe: SampleRecipe =
            serde_json::from_str(&doc).map_err(|e| format!("{}: {e}", path.display()))?;
        validate(&recipe).map_err(|e| format!("{}: {e}", path.display()))?;
        let rendered = render_recipe(&recipe).map_err(|e| format!("{}: {e}", path.display()))?;        let choke_group = recipe.tags.iter().find_map(|t| t.strip_prefix("choke:").and_then(|n| n.parse::<u8>().ok()));
        let class = recipe
            .tags
            .iter()
            .find_map(|t| t.parse::<ClassTag>().ok())
            .unwrap_or(ClassTag::Perc);

        let wav_path = out_dir.join(format!("{stem}.wav"));
        write_wav16(&wav_path, &rendered.pcm, rendered.sample_rate)
            .map_err(|e| format!("write {}: {e}", wav_path.display()))?;
        // Hash the DECODED file so the manifest verifies the checked-in
        // artifact bit-exactly, not the pre-quantization render.
        let decoded = read_wav_f32(&wav_path)
            .map_err(|e| format!("re-read {}: {e}", wav_path.display()))?;
        let features = analyze_features(&decoded, rendered.sample_rate);
        entries.push(ManifestEntry {
            id: stem,
            class: class.value(),
            sample_rate: rendered.sample_rate,
            frames: decoded.len() as u32,
            features,
            pcm_hash: format!("{:016x}", kontinuum_core::fnv1a64(&f32_le_bytes(&decoded))),
            choke_group,
        });
    }

    let manifest = PackManifest {
        version: 1,
        pack: PACK_NAME.to_owned(),
        license: LICENSE.to_owned(),
        source: SOURCE.to_owned(),
        samples: entries,
    };
    let manifest_path = out_dir.join("manifest.json");
    let body = serde_json::to_string_pretty(&manifest)
        .map_err(|e| format!("manifest: {e}"))?;
    std::fs::write(&manifest_path, body + "\n")
        .map_err(|e| format!("write {}: {e}", manifest_path.display()))?;
    Ok(manifest.samples.len())
}

/// Tag → `SampleClass` parse helper (genpack-local, keeps catalog types
/// out of the recipe vocabulary).
#[derive(Clone, Copy)]
enum ClassTag {
    Kick,
    Hat,
    Perc,
    Pad,
    Texture,
}
impl ClassTag {
    const fn value(self) -> SampleClass {
        match self {
            ClassTag::Kick => SampleClass::Kick,
            ClassTag::Hat => SampleClass::Hat,
            ClassTag::Perc => SampleClass::Perc,
            ClassTag::Pad => SampleClass::Pad,
            ClassTag::Texture => SampleClass::Texture,
        }
    }
}
impl std::str::FromStr for ClassTag {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "kick" => Ok(ClassTag::Kick),
            "hat" => Ok(ClassTag::Hat),
            "perc" => Ok(ClassTag::Perc),
            "pad" => Ok(ClassTag::Pad),
            "texture" => Ok(ClassTag::Texture),
            _ => Err(()),
        }
    }
}

fn write_wav16(path: &Path, pcm: &[f32], sample_rate: u32) -> Result<(), hound::Error> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for &s in pcm {
        let v = (s.clamp(-1.0, 1.0) * 32_767.0).round() as i16;
        writer.write_sample(v)?;
    }
    writer.finalize()
}

/// Decode a 16-bit int WAV to f32 with the same scale kpack readers use.
fn read_wav_f32(path: &Path) -> Result<Vec<f32>, hound::Error> {
    let mut reader = hound::WavReader::open(path)?;
    let mut out = Vec::new();
    for sample in reader.samples::<i16>() {
        out.push(sample? as f32 / 32_768.0);
    }
    Ok(out)
}

fn f32_le_bytes(pcm: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(pcm.len() * 4);
    for s in pcm {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    bytes
}
