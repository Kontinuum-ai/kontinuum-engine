//! `kontinuum-ir` CLI — manual `std::env::args` parsing (clap unavailable).
//!
//! Commands:
//!   validate   <file>                 exit 0 valid / 1 with error list
//!   compile    <file> [sample_rate]   JSON summary {blocks, events_total, cpu_estimate}
//!   diff-apply <session> <diff> <at_bar>   prints resulting session JSON
//!   render     <file> <out.wav>       delegates to kontinuum-offline
//!   schema                            prints the JSON Schema
//!
//! All failures print machine-readable JSON to stdout and exit 1.

use std::process::ExitCode;

use kontinuum_ir::{
    apply_diff, compile_session_summary, export_json_schema, validate_session, IrDiff, Session,
    ValidationError,
};

const USAGE: &str = "\
kontinuum-ir — musical IR toolchain (issue #11)

USAGE:
  kontinuum-ir validate <session.json>
  kontinuum-ir compile <session.json> [sample_rate]
  kontinuum-ir diff-apply <session.json> <diff.json> <at_bar>
  kontinuum-ir render <session.json> <out.wav>
  kontinuum-ir schema
";

const DEFAULT_SAMPLE_RATE: u32 = 48_000;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => code,
        Err(msg) => {
            println!("{msg}");
            ExitCode::from(1)
        }
    }
}

fn run(args: &[String]) -> Result<ExitCode, String> {
    match args.first().map(String::as_str) {
        Some("validate") => cmd_validate(args.get(1))?,
        Some("compile") => return cmd_compile(args.get(1), args.get(2)),
        Some("diff-apply") => return cmd_diff_apply(args.get(1), args.get(2), args.get(3)),
        Some("render") => return cmd_render(args.get(1), args.get(2)),
        Some("schema") => {
            println!(
                "{}",
                serde_json::to_string_pretty(&export_json_schema()).expect("schema serializes")
            );
            return Ok(ExitCode::SUCCESS);
        }
        _ => return Err(USAGE.to_string()),
    }
    Ok(ExitCode::SUCCESS)
}

fn read_json(path: Option<&String>, what: &str) -> Result<String, String> {
    let path = path.ok_or_else(|| format!("missing <{what}> argument\n\n{USAGE}"))?;
    std::fs::read_to_string(path).map_err(|e| format!("cannot read {what} `{path}`: {e}"))
}

fn parse_session(path: Option<&String>) -> Result<Session, String> {
    let text = read_json(path, "session file")?;
    serde_json::from_str(&text).map_err(|e| {
        format!(
            "{}",
            serde_json::json!({
                "ok": false,
                "errors": [{
                    "code": "E_PARSE",
                    "path": "$",
                    "message": e.to_string(),
                    "suggested_fix": "fix the JSON syntax/shape; run `kontinuum-ir schema` for the contract"
                }]
            })
        )
    })
}

fn cmd_validate(path: Option<&String>) -> Result<(), String> {
    let session = parse_session(path)?;
    match validate_session(&session) {
        Ok(()) => {
            println!(
                "{}",
                serde_json::json!({
                    "ok": true,
                    "sections": session.sections.len(),
                    "tracks": session.tracks.len(),
                    "bars": session.total_bars(),
                })
            );
            Ok(())
        }
        Err(errors) => Err(pretty_errors(&errors)),
    }
}

fn cmd_compile(path: Option<&String>, sample_rate: Option<&String>) -> Result<ExitCode, String> {
    let session = parse_session(path)?;
    if let Err(errors) = validate_session(&session) {
        return Err(pretty_errors(&errors));
    }
    let sr = match sample_rate {
        Some(arg) => arg
            .parse::<u32>()
            .map_err(|_| format!("invalid sample rate `{arg}`"))?,
        None => DEFAULT_SAMPLE_RATE,
    };
    match compile_session_summary(&session, sr) {
        Ok(summary) => {
            println!(
                "{}",
                serde_json::json!({
                    "ok": true,
                    "sample_rate": sr,
                    "blocks": summary.blocks,
                    "events_total": summary.events_total,
                    "cpu_estimate": summary.cpu_estimate,
                    "bars": session.total_bars(),
                })
            );
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => Err(serde_json::json!({
            "ok": false,
            "error": { "kind": "CompileError", "message": e.to_string() }
        })
        .to_string()),
    }
}

fn cmd_diff_apply(
    session_path: Option<&String>,
    diff_path: Option<&String>,
    at_bar: Option<&String>,
) -> Result<ExitCode, String> {
    let mut session = parse_session(session_path)?;
    if let Err(errors) = validate_session(&session) {
        return Err(pretty_errors(&errors));
    }
    let diff_text = read_json(diff_path, "diff file")?;
    let diff: IrDiff = serde_json::from_str(&diff_text)
        .map_err(|e| format!("invalid diff: {e}"))?;
    let at_bar: u32 = at_bar
        .and_then(|a| a.parse().ok())
        .ok_or_else(|| format!("missing or invalid <at_bar>\n\n{USAGE}"))?;
    match apply_diff(&mut session, &diff, at_bar) {
        Ok(report) => {
            let out = serde_json::json!({
                "applied": report.applied,
                "superseded": report.superseded,
                "session": session,
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&out).expect("session serializes")
            );
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => Err(serde_json::json!({
            "ok": false,
            "error": { "kind": "ApplyError", "message": e.to_string() }
        })
        .to_string()),
    }
}

fn pretty_errors(errors: &[ValidationError]) -> String {
    serde_json::json!({
        "ok": false,
        "errors": errors,
    })
    .to_string()
}

/// Delegates rendering to the sibling `kontinuum-offline` binary (a direct
/// dependency would be circular: offline → ir). Cargo places all workspace
/// binaries in one directory, so the sibling is found next to the current
/// executable, falling back to `$PATH`.
fn cmd_render(session: Option<&String>, out_wav: Option<&String>) -> Result<ExitCode, String> {
    let session = session.ok_or_else(|| format!("missing <session.json> argument\n\n{USAGE}"))?;
    let out_wav = out_wav.ok_or_else(|| format!("missing <out.wav> argument\n\n{USAGE}"))?;
    parse_session(Some(session))?;
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot locate current executable: {e}"))?
        .parent()
        .ok_or_else(|| "executable has no parent directory".to_string())?
        .join("kontinuum-offline");
    let status = std::process::Command::new(&exe)
        .arg("render")
        .arg(session)
        .arg(out_wav)
        .status()
        .map_err(|e| {
            format!(
                "cannot run the offline renderer at `{}` (build it first: `cargo build --release -p kontinuum-offline`): {e}",
                exe.display()
            )
        })?;
    if status.success() {
        Ok(ExitCode::SUCCESS)
    } else {
        Err(format!("offline renderer failed with {status}"))
    }
}
