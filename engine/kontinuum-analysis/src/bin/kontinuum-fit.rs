//! `kontinuum-fit` — one-shot parameter fitting (issue #75): fit our
//! voice's parameters to a reference hit and print the resulting
//! `InstrumentDef` IR. The reference audio is an input to this local
//! optimiser only; the artifact that ships is the parameter vector.
//!
//! usage:
//!   kontinuum-fit <target.wav> <kick|hat|clap> [--restarts N] [--out path] [--seed S]
//!
//! Any printed def is valid IR by construction: the fitter searches only
//! the bounds of the voices' `set_param` clamps (kick.rs / hat.rs /
//! hand.rs) and applies candidates through `set_param`, which clamps
//! again — there is no second validation pass to get out of sync.
//!
//! Reading the loss (weighted multi-resolution STFT + envelope distance,
//! see `analysis::fit::objective`): round-trip fits of our own voices
//! land ≲ 0.05. A loss ≥ 0.35 (≈ 7× that) means the target is outside
//! this voice's model — a plausible-looking fit that should NOT ship.
//! In between: inspect by ear before use.

use std::path::Path;
use std::process::ExitCode;

use kontinuum_analysis::fit::{fit, FitConfig, VoiceKind};

const USAGE: &str = "usage:\n  kontinuum-fit <target.wav> <kick|hat|clap> [--restarts N] [--out path] [--seed S]\n\nFits Kontinuum's own voice parameters to a reference one-shot and prints\nthe resulting InstrumentDef JSON (valid IR by construction — the search\nstays inside the voices' set_param clamps). The reference audio is only\nan input to this local optimiser; the shipped artifact is the parameter\nvector.";

struct Args {
    wav: String,
    kind: VoiceKind,
    restarts: usize,
    out: Option<String>,
    seed: u64,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut positional = Vec::new();
    let mut restarts = 8usize;
    let mut out = None;
    let mut seed = 0u64;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--restarts" => {
                i += 1;
                restarts = args.get(i).ok_or("--restarts needs a value")?
                    .parse().map_err(|_| "--restarts must be a positive integer".to_string())?;
            }
            "--out" => {
                i += 1;
                out = Some(args.get(i).ok_or("--out needs a path")?.clone());
            }
            "--seed" => {
                i += 1;
                seed = args.get(i).ok_or("--seed needs a value")?
                    .parse().map_err(|_| "--seed must be an integer".to_string())?;
            }
            other => positional.push(other.to_string()),
        }
        i += 1;
    }
    match positional.as_slice() {
        [wav, kind] => {
            let kind = VoiceKind::parse(kind).ok_or_else(|| {
                format!("unknown voice kind {kind:?} — expected kick, hat, or clap")
            })?;
            Ok(Args { wav: wav.clone(), kind, restarts: restarts.max(1), out, seed })
        }
        _ => Err(USAGE.to_string()),
    }
}

/// Mono-downmixed f32 samples + sample rate of `path`.
fn load_wav_mono(path: &str) -> Result<(Vec<f32>, u32), String> {
    let mut reader = hound::WavReader::open(Path::new(path)).map_err(|e| e.to_string())?;
    let spec = reader.spec();
    let channels = spec.channels as usize;
    let mut mono = Vec::new();
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for frame in reader.samples::<f32>() {
                mono.push(frame.map_err(|e| e.to_string())?);
            }
        }
        hound::SampleFormat::Int => {
            let bits = spec.bits_per_sample;
            let scale = 1.0f32 / 2f32.powi(bits as i32 - 1);
            for frame in reader.samples::<i32>() {
                mono.push(frame.map_err(|e| e.to_string())? as f32 * scale);
            }
        }
    }
    let downmixed = if channels <= 1 {
        mono
    } else {
        mono.chunks(channels)
            .map(|c| c.iter().sum::<f32>() / channels as f32)
            .collect()
    };
    Ok((downmixed, spec.sample_rate))
}

fn run(args: &Args) -> Result<(), String> {
    let (target, sample_rate) = load_wav_mono(&args.wav)?;
    // One-shots are short; compare over the whole file capped at 3 s so
    // pathological inputs cannot stall the optimiser.
    let frames = target.len().min(3 * sample_rate as usize);
    let cfg = FitConfig { restarts: args.restarts, seed: args.seed, sample_rate, frames };
    let result = fit(&target[..frames], args.kind, &cfg);
    let def = args.kind.to_instrument_def(&result.params);
    let json = serde_json::to_string_pretty(&def).map_err(|e| e.to_string())?;
    match &args.out {
        Some(path) => std::fs::write(Path::new(path), json + "\n").map_err(|e| e.to_string())?,
        None => println!("{json}"),
    }
    println!("// loss {:.6}  restarts {}  seed {}", result.loss, args.restarts, args.seed);
    println!(
        "// loss ≲ 0.05 is a round-trip-quality fit; ≥ 0.35 means the target is outside this voice's model"
    );
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if args.is_empty() {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    }
    match parse_args(&args).and_then(|a| run(&a)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("error: {msg}");
            ExitCode::FAILURE
        }
    }
}
