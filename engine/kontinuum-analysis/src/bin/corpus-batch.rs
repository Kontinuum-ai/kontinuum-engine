//! `corpus-batch` — the versioned, rerunnable #23 batch pipeline
//! (local-Mac-shaped; the cloud run per PLAN §2.4 is this binary against
//! mounted bucket storage).
//!
//! usage:
//!   corpus-batch --manifest corpus/manifest.csv --out corpus/features \
//!                [--annotations corpus/annotations]
//!
//! Per manifest row: resolve audio (synthetic rows render in-process from
//! their `synth_spec`; real rows are read from `file_path` after the
//! manifest hash check), analyze, and collect the `TrackObservation`.
//! One bad track never hides another: every failure is reported with its
//! typed error and the run exits non-zero at the end.
//!
//! Outputs (never audio):
//!   {out}/observations-{subgenre}.jsonl   — fitter input
//!   {out}/segmentation-report.json        — F1 self-validation vs gate
//!   {out}/annotations/{track_id}.json     — synthetic ground truth
//!
//! Real tracks are graded against HUMAN annotation files (the issue's 20
//! hand annotations) expected at {annotations}/{track_id}.json; tracks
//! without one are reported with `passed: null` — ungraded, not passed.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use kontinuum_analysis::corpus::{analyze_track, decode_wav, PIPELINE_VERSION};
use kontinuum_analysis::synthgen;
use kontinuum_corpus::{
    boundary_f1, Manifest, ManifestRow, SegmentationAnnotation, SEGMENTATION_F1_GATE,
};

const USAGE: &str = "usage:\n  corpus-batch --manifest <manifest.csv> --out <features-dir> [--annotations <dir>]";

struct Args {
    manifest: String,
    out: String,
    annotations: Option<String>,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut parsed = Args { manifest: String::new(), out: String::new(), annotations: None };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--manifest" => {
                i += 1;
                parsed.manifest = args.get(i).ok_or("--manifest needs a path")?.clone();
            }
            "--out" => {
                i += 1;
                parsed.out = args.get(i).ok_or("--out needs a path")?.clone();
            }
            "--annotations" => {
                i += 1;
                parsed.annotations = Some(args.get(i).ok_or("--annotations needs a path")?.clone());
            }
            other => return Err(format!("unknown argument '{other}'\n\n{USAGE}")),
        }
        i += 1;
    }
    if parsed.manifest.is_empty() || parsed.out.is_empty() {
        return Err(USAGE.to_string());
    }
    Ok(parsed)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("corpus-batch: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &Args) -> Result<(), String> {
    let manifest_text =
        std::fs::read_to_string(&args.manifest).map_err(|e| format!("manifest unreadable: {e}"))?;
    let manifest = Manifest::parse(&manifest_text).map_err(|e| format!("manifest rejected: {e}"))?;
    if manifest.tracks.is_empty() {
        return Err("manifest has no rows".into());
    }
    println!("corpus-batch v{PIPELINE_VERSION}: {} manifest rows", manifest.tracks.len());

    std::fs::create_dir_all(&args.out).map_err(|e| format!("out dir: {e}"))?;
    let annotations_dir = args
        .annotations
        .clone()
        .unwrap_or_else(|| Path::new(&args.out).join("annotations").to_string_lossy().into_owned());

    let mut failures: Vec<(String, String)> = Vec::new();
    let mut observations: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut reports: Vec<TrackReport> = Vec::new();

    for row in &manifest.tracks {
        match process_track(&manifest, row, &annotations_dir) {
            Ok((json, report)) => {
                observations.entry(row.subgenre.clone()).or_default().push(json);
                reports.push(report);
            }
            Err(err) => failures.push((row.track_id.clone(), err)),
        }
    }

    for (subgenre, lines) in &observations {
        let path = Path::new(&args.out).join(format!("observations-{subgenre}.jsonl"));
        std::fs::write(&path, lines.join("\n") + "\n")
            .map_err(|e| format!("write {}: {e}", path.display()))?;
        println!("  wrote {} observations → {}", lines.len(), path.display());
    }

    let all_passed = reports.iter().all(|r| r.passed.unwrap_or(true)) && failures.is_empty();
    let report_path = Path::new(&args.out).join("segmentation-report.json");
    serde_json::to_writer_pretty(
        std::fs::File::create(&report_path).map_err(|e| format!("create report: {e}"))?,
        &serde_json::json!({
            "pipeline_version": PIPELINE_VERSION,
            "f1_gate": SEGMENTATION_F1_GATE,
            "all_passed": all_passed,
            "tracks": reports,
        }),
    )
    .map_err(|e| format!("write report: {e}"))?;
    println!("  report → {}", report_path.display());

    if !failures.is_empty() {
        println!("\nFAILED tracks ({}):", failures.len());
        for (track_id, err) in &failures {
            println!("  {track_id}: {err}");
        }
        return Err("batch had failing tracks".into());
    }
    if !all_passed {
        return Err("segmentation F1 gate failed — see segmentation-report.json".into());
    }
    println!("corpus-batch: OK");
    Ok(())
}

/// One track's validation outcome for the report.
#[derive(serde::Serialize)]
struct TrackReport {
    track_id: String,
    detected_boundaries: Vec<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    truth_boundaries: Vec<u32>,
    precision: Option<f64>,
    recall: Option<f64>,
    f1: Option<f64>,
    /// `None` = no ground truth available (ungraded, not passed).
    passed: Option<bool>,
}

/// Processes one manifest row into (observation JSONL line, report entry).
fn process_track(
    manifest: &Manifest,
    row: &ManifestRow,
    annotations_dir: &str,
) -> Result<(String, TrackReport), String> {
    let (mono, sr) = track_audio(manifest, row)?;
    let analysis = analyze_track(&row.track_id, &row.subgenre, &mono, sr, f64::from(row.bpm))
        .map_err(|e| e.to_string())?;
    let json = serde_json::to_string(&analysis.observation)
        .map_err(|e| format!("observation serialize: {e}"))?;

    let truth: Option<SegmentationAnnotation> = if row.synthetic {
        let preset = synthgen::preset_by_id(&row.synth_spec)
            .ok_or_else(|| format!("unknown synth_spec '{}'", row.synth_spec))?;
        Some(synthgen::planted_annotation(preset))
    } else {
        std::fs::read_to_string(Path::new(annotations_dir).join(format!("{}.json", row.track_id)))
            .ok()
            .and_then(|text| SegmentationAnnotation::from_json(&text).ok())
    };

    let report = match &truth {
        Some(truth) => {
            let s = boundary_f1(&analysis.boundary_bars, truth);
            TrackReport {
                track_id: row.track_id.clone(),
                detected_boundaries: analysis.boundary_bars.clone(),
                truth_boundaries: truth.sections.iter().skip(1).map(|s| s.start_bar).collect(),
                precision: Some(s.precision),
                recall: Some(s.recall),
                f1: Some(s.f1),
                passed: Some(s.f1 >= SEGMENTATION_F1_GATE),
            }
        }
        None => TrackReport {
            track_id: row.track_id.clone(),
            detected_boundaries: analysis.boundary_bars.clone(),
            truth_boundaries: Vec::new(),
            precision: None,
            recall: None,
            f1: None,
            passed: None,
        },
    };
    Ok((json, report))
}

fn track_audio(
    manifest: &Manifest,
    row: &ManifestRow,
) -> Result<(Vec<f32>, u32), String> {
    if row.synthetic {
        let preset = synthgen::preset_by_id(&row.synth_spec).ok_or_else(|| {
            format!("track {}: unknown synth_spec '{}'", row.track_id, row.synth_spec)
        })?;
        return Ok((synthgen::render(preset), synthgen::SYNTH_SAMPLE_RATE));
    }
    let bytes = manifest
        .resolve_audio(row)
        .map_err(|e| format!("track {}: {e}", row.track_id))?;
    decode_wav(&bytes).map_err(|e| format!("track {}: {e}", row.track_id))
}
