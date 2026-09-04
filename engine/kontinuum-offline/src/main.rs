//! `kontinuum-offline` CLI.
//!
//! - `render [--premium] <in.ir.json> <out.wav>`
//! - `hash [--premium] <in.ir.json>`
//! - `render-ab [--premium] <in.ir.json> <out-dir>`
//!
//! `--premium` selects the premium export chain (#28): linear-phase
//! master EQ, ×8 oversampled limiting, loudness normalization to the
//! targets file, TPDF dither to 16-bit. Exports/bookmarks (#31 share
//! feature) render through this path. `--targets <file>` overrides the
//! mastering targets (default: the shipped hypothesis profile).

use std::path::Path;
use std::process::ExitCode;

use kontinuum_mastering::targets::MasteringTargets;
use kontinuum_offline::{
    parse_session, premium_render, premium_render_to_wav, render_ab, render_session,
    render_to_wav, write_ab, DEFAULT_SAMPLE_RATE,
};

const USAGE: &str = "usage:\n  kontinuum-offline render [--premium] [--targets <f>] <session.ir.json> <out.wav>\n  kontinuum-offline hash [--premium] [--targets <f>] <session.ir.json>\n  kontinuum-offline render-ab [--premium] [--targets <f>] <session.ir.json> <out-dir>\n\n--premium: export chain #28 (linear-phase EQ, x8 oversampled limiter,\nloudness normalize, TPDF dither to 16-bit) — the path exports and\nbookmarks (#31) use. Default: plain render / mix hash.";

struct Opts {
    premium: bool,
    targets: Option<String>,
}

/// Split `--premium` / `--targets <f>` flags from positional arguments.
fn parse_opts(args: &[String]) -> Option<(Opts, Vec<String>)> {
    let mut opts = Opts { premium: false, targets: None };
    let mut pos = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--premium" => opts.premium = true,
            "--targets" => {
                i += 1;
                match args.get(i) {
                    Some(v) => opts.targets = Some(v.clone()),
                    None => return None,
                }
            }
            flag if flag.starts_with('-') => return None,
            _ => pos.push(args[i].clone()),
        }
        i += 1;
    }
    Some((opts, pos))
}

fn load_targets(opts: &Opts) -> Result<MasteringTargets, String> {
    match &opts.targets {
        Some(path) => MasteringTargets::load(Path::new(path)).map_err(|e| e.to_string()),
        None => Ok(MasteringTargets::hypothesis()),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((opts, pos)) = parse_opts(&args) else {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    };
    let targets = match load_targets(&opts) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    match pos.as_slice() {
        [cmd, input, output] if cmd == "render" => run_render(input, output, &targets, opts.premium),
        [cmd, input] if cmd == "hash" => run_hash(input, &targets, opts.premium),
        [cmd, input, out_dir] if cmd == "render-ab" => {
            run_ab(input, out_dir, &targets, opts.premium)
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

fn run_render(input: &str, output: &str, targets: &MasteringTargets, premium: bool) -> ExitCode {
    let result = if premium {
        premium_render_to_wav(Path::new(input), Path::new(output), targets)
    } else {
        render_to_wav(Path::new(input), Path::new(output))
    };
    match result {
        Ok(()) => {
            println!("wrote {output}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_hash(input: &str, targets: &MasteringTargets, premium: bool) -> ExitCode {
    let hashed = parse_session(Path::new(input)).and_then(|s| {
        if premium {
            premium_render(&s, DEFAULT_SAMPLE_RATE, targets).map(|r| r.content_hash())
        } else {
            render_session(&s, DEFAULT_SAMPLE_RATE).map(|out| out.fnv_hash())
        }
    });
    match hashed {
        Ok(hash) => {
            println!("{hash:016x}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_ab(input: &str, out_dir: &str, targets: &MasteringTargets, premium: bool) -> ExitCode {
    let rendered = parse_session(Path::new(input))
        .and_then(|s| render_ab(&s, DEFAULT_SAMPLE_RATE, targets, premium))
        .and_then(|pair| {
            println!(
                "mix {:.2} LUFS ({}) | master {:.2} LUFS ({}) | matched to {:.2} LUFS",
                pair.manifest.files.mix.integrated_lufs,
                pair.manifest.files.mix.fnv_hash,
                pair.manifest.files.master.integrated_lufs,
                pair.manifest.files.master.fnv_hash,
                pair.manifest.match_target_lufs
            );
            write_ab(Path::new(out_dir), &pair)
        });
    match rendered {
        Ok(()) => {
            println!("wrote {out_dir}/{{mix.wav, master.wav, manifest.json}}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
