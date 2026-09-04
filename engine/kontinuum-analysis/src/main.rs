//! `kontinuum-critic` — score a render against a reference profile, or gate
//! it against a stored ratchet baseline.
//!
//! usage:
//!   kontinuum-critic score <render.wav> <profile.json>
//!   kontinuum-critic gate <render.wav> <profile.json> <baseline.json>
//!
//! `score` prints the metric table and exits 0. `gate` additionally exits 1
//! when the distance regresses beyond the baseline's ratchet.

use std::path::Path;
use std::process::ExitCode;

use kontinuum_analysis::metrics::Metrics;
use kontinuum_analysis::{Baseline, QualityProfile, BANDS};

const USAGE: &str = "usage:\n  kontinuum-critic score <render.wav> <profile.json>\n  kontinuum-critic gate <render.wav> <profile.json> <baseline.json>";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [cmd, wav, profile] if cmd == "score" => run_score(wav, profile),
        [cmd, wav, profile, baseline] if cmd == "gate" => run_gate(wav, profile, baseline),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

fn load_metrics(wav: &str) -> Result<Metrics, String> {
    let mut reader = hound::WavReader::open(Path::new(wav)).map_err(|e| e.to_string())?;
    let spec = reader.spec();
    if spec.channels != 2 {
        return Err(format!("expected stereo, got {} channels", spec.channels));
    }
    let sr = spec.sample_rate;
    let mut left = Vec::new();
    let mut right = Vec::new();
    match spec.sample_format {
        hound::SampleFormat::Float => {
            let mut samples = reader.samples::<f32>();
            while let (Some(l), Some(r)) = (samples.next(), samples.next()) {
                left.push(l.map_err(|e| e.to_string())?);
                right.push(r.map_err(|e| e.to_string())?);
            }
        }
        hound::SampleFormat::Int => {
            let bits = spec.bits_per_sample;
            let mut samples = reader.samples::<i32>();
            let scale = |v: i32| -> f32 { v as f32 / 2f32.powi(bits as i32 - 1) };
            while let (Some(l), Some(r)) = (samples.next(), samples.next()) {
                left.push(scale(l.map_err(|e| e.to_string())?));
                right.push(scale(r.map_err(|e| e.to_string())?));
            }
        }
    }
    Ok(Metrics::analyze(&left, &right, sr))
}

fn print_metrics(m: &Metrics) {
    println!("  rms {:6.1} dBFS   peak {:6.1} dBFS   true peak {:6.1} dBFS   crest {:5.2} dB   short-term dyn {:6.2} dB",
        m.rms_dbfs, m.peak_dbfs, m.true_peak_dbfs, m.crest_db, m.short_term_dyn_db);
    println!("  centroid {:7.1} Hz   transients/sec {:5.2}   hit cv {:5.3}",
        m.centroid_hz, m.transients_per_sec, m.hit_cv);
    let bands: Vec<String> =
        BANDS.iter().enumerate().map(|(i, (name, _, _))| format!("{name}={:.1}%", m.band_shares[i] * 100.0)).collect();
    println!("  bands: {}", bands.join("  "));
}

fn run_score(wav: &str, profile_path: &str) -> ExitCode {
    let run = (|| {
        let m = load_metrics(wav)?;
        let p = QualityProfile::load(Path::new(profile_path))?;
        Ok::<_, String>((m, p))
    })();
    match run {
        Ok((m, p)) => {
            println!("== {wav} vs {}", p.name);
            print_metrics(&m);
            println!("  distance {:.3}", p.distance(&m));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_gate(wav: &str, profile_path: &str, baseline_path: &str) -> ExitCode {
    let run = (|| {
        let m = load_metrics(wav)?;
        let p = QualityProfile::load(Path::new(profile_path))?;
        let b = Baseline::load(Path::new(baseline_path))?;
        Ok::<_, String>((m, p, b))
    })();
    match run {
        Ok((m, p, b)) => {
            println!("== {wav} vs {} (ratchet {} + {})", p.name, b.distance, b.ratchet);
            print_metrics(&m);
            match b.passes(&m, &p) {
                Ok(d) => {
                    println!("  PASS  distance {d:.3} ≤ {} + {}", b.distance, b.ratchet);
                    ExitCode::SUCCESS
                }
                Err(d) => {
                    println!("  FAIL  distance {d:.3} > {} + {}", b.distance, b.ratchet);
                    ExitCode::FAILURE
                }
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
