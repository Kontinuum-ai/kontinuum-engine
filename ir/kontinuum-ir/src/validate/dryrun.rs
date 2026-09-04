//! L3 dry-run: compile the session and analyse the event stream — density,
//! unplanned silence, automation slew, and the CPU budget. Polyphony is
//! enforced inside the compiler (slot pool exhaustion) and surfaced here.

use std::collections::BTreeMap;
use std::sync::Arc;

use kontinuum_clock::TempoLane;
use kontinuum_schedule::{CompiledBlock, Event};

use crate::compile::{self, CompileError, CPU_BUDGET_UNITS};
use crate::schema::Session;
use crate::validate::{err, ErrorCatalog, ValidationError};

const DRYRUN_SAMPLE_RATE: u32 = 48_000;
/// Amplitude slew ceiling in dB per bar (issue #11: > 24 dB-equivalent/bar).
const MAX_SLEW_DB_PER_BAR: f64 = 24.0;
const SLEW_EPS: f64 = 1e-4;

pub(super) fn check(s: &Session, out: &mut Vec<ValidationError>) {
    let blocks = match compile::compile_session(s, DRYRUN_SAMPLE_RATE) {
        Ok(b) => b,
        Err(e) => {
            out.push(compile_error(&e));
            return;
        }
    };
    let Ok(lane) = TempoLane::new(DRYRUN_SAMPLE_RATE, &s.tempo_lane) else {
        // Structural checks already rejected bad lanes; unreachable here.
        return;
    };

    check_density_and_silence(s, &blocks, &lane, out);

    if let Ok((cpu, report)) = compile::worst_block_cost(s, &blocks, DRYRUN_SAMPLE_RATE) {
        if cpu > CPU_BUDGET_UNITS {
            let top: Vec<String> = report
                .iter()
                .filter(|(_, c)| *c > 0.0)
                .take(3)
                .map(|(id, c)| format!("{id} {c:.1}u"))
                .collect();
            let breakdown = if top.is_empty() {
                String::new()
            } else {
                format!("; worst tracks: {}", top.join(", "))
            };
            out.push(err(
                ErrorCatalog::E_CPU_BUDGET_EXCEEDED,
                "/blocks",
                format!("peak block cost {cpu:.1} units exceeds the {CPU_BUDGET_UNITS} budget{breakdown}"),
                "reduce concurrent voices: shorter gates, thinner pads, fewer tracks; for patch tracks swap in a cheaper custom patch",
            ));
        }
    }

    check_slew(s, out);
}

fn compile_error(e: &CompileError) -> ValidationError {
    match CompileErrorLike::from(e) {
        CompileErrorLike::PoolExhausted(track, bar) => err(
            ErrorCatalog::E_POLYPHONY_EXCEEDED,
            format!("/tracks/{track}"),
            format!("voice pool exhausted near bar {bar}: too many overlapping onsets"),
            "shorten gates, thin the pattern, or split hits across tracks",
        ),
        CompileErrorLike::Other(msg) => err(
            ErrorCatalog::E_COMPILE_FAILED,
            "/",
            format!("dry-run compile failed: {msg}"),
            "fix the structural issue reported and re-validate",
        ),
    }
}

enum CompileErrorLike {
    PoolExhausted(u8, u32),
    Other(String),
}

impl From<&CompileError> for CompileErrorLike {
    fn from(e: &CompileError) -> Self {
        match e {
            CompileError::VoicePoolExhausted { track, bar } => {
                CompileErrorLike::PoolExhausted(*track, *bar)
            }
            other => CompileErrorLike::Other(other.to_string()),
        }
    }
}

/// NoteOn counts per (track index, absolute bar) off the compiled blocks.
fn noteons_per_bar(
    s: &Session,
    blocks: &[Arc<CompiledBlock>],
    lane: &TempoLane,
) -> BTreeMap<(u8, u32), u32> {
    let mut counts: BTreeMap<(u8, u32), u32> = BTreeMap::new();
    let total = s.total_bars() as u32;
    for block in blocks {
        for te in &block.tracks {
            for (frame, e) in &te.events {
                if matches!(e, Event::NoteOn { .. }) {
                    let abs_frame = block.start_frame + u64::from(*frame);
                    let bar = (lane.bar_at_frame(abs_frame).floor() as u32).min(total.saturating_sub(1));
                    *counts.entry((te.track, bar)).or_default() += 1;
                }
            }
        }
    }
    counts
}

fn check_density_and_silence(
    s: &Session,
    blocks: &[Arc<CompiledBlock>],
    lane: &TempoLane,
    out: &mut Vec<ValidationError>,
) {
    let counts = noteons_per_bar(s, blocks, lane);
    for ((track, bar), n) in &counts {
        if *n > crate::validate::bounds::MAX_ONSETS_PER_BAR as u32 {
            let track_id = s
                .tracks
                .get(*track as usize)
                .map(|t| t.id.as_str())
                .unwrap_or("?");
            out.push(err(
                ErrorCatalog::E_DENSITY_TOO_HIGH,
                format!("/sections/…/pattern_bindings/{track_id}"),
                format!("track `{track_id}` fires {n} onsets in bar {bar}; ceiling is 256/bar"),
                "thin the pattern below 256 onsets per bar",
            ));
        }
    }
    let starts = s.section_start_bars();
    for (si, sec) in s.sections.iter().enumerate() {
        let lo = starts[si];
        let hi = lo + sec.bars;
        let any_note = counts.keys().any(|(_, bar)| *bar >= lo && *bar < hi);
        if !any_note {
            out.push(err(
                ErrorCatalog::E_UNPLANNED_SILENCE,
                format!("/sections/{si}"),
                format!(
                    "section `{}` (bars {lo}..{hi}) produces no NoteOn events on any track",
                    sec.id
                ),
                "bind at least one pattern to a track, or mark intent with a transition",
            ));
        }
    }
}

fn check_slew(s: &Session, out: &mut Vec<ValidationError>) {
    for (si, sec) in s.sections.iter().enumerate() {
        for (tid, lane) in &sec.automation {
            for (pi, w) in lane.points.windows(2).enumerate() {
                let (b1, v1, _) = &w[0];
                let (b2, v2, _) = &w[1];
                let bars = b2.saturating_sub(*b1).max(1) as f64;
                let ratio = (f64::from(*v2) + SLEW_EPS) / (f64::from(*v1) + SLEW_EPS);
                let db = 20.0 * ratio.abs().max(SLEW_EPS).log10();
                if db.abs() / bars > MAX_SLEW_DB_PER_BAR {
                    out.push(err(
                        ErrorCatalog::E_SLEW_TOO_FAST,
                        format!("/sections/{si}/automation/{tid}/points/{pi}"),
                        format!(
                            "param moves {:.1} dB in {bars} bar(s) (ceiling {MAX_SLEW_DB_PER_BAR} dB/bar)",
                            db.abs()
                        ),
                        "spread the move over more bars or shrink the value change",
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Session;
    use crate::validate::structural::tests::base_session_json;

    fn codes(json: &str) -> Vec<&'static str> {
        let s: Session = serde_json::from_str(json).expect("fixture parses");
        crate::validate::validate_session(&s)
            .expect_err("fixture must fail")
            .into_iter()
            .map(|e| e.code)
            .collect()
    }

    #[test]
    fn silence_only_section_is_rejected() {
        let json = base_session_json().replace(
            r#""pattern_bindings": {"k": {"generator": "euclidean", "k": 4, "n": 16}}"#,
            r#""pattern_bindings": {}"#,
        );
        assert!(codes(&json).contains(&ErrorCatalog::E_UNPLANNED_SILENCE));
    }

    #[test]
    fn slew_violation_is_rejected() {
        let json = base_session_json().replace(
            r#""energy_curve": [0.5],"#,
            r#""energy_curve": [0.5], "automation": {"k": {"target_param": "gain",
                "points": [[0, 0.001, "linear"], [1, 1.0, "linear"]]}},"#,
        );
        assert!(codes(&json).contains(&ErrorCatalog::E_SLEW_TOO_FAST));
    }

    #[test]
    fn gentle_automation_passes() {
        let json = base_session_json().replace(
            r#""energy_curve": [0.5],"#,
            r#""energy_curve": [0.5], "automation": {"k": {"target_param": "gain",
                "points": [[0, 0.6, "linear"], [3, 1.0, "linear"]]}},
            "#,
        );
        let s: Session = serde_json::from_str(&json).expect("parses");
        crate::validate::validate_session(&s).expect("gentle slew is fine");
    }

    #[test]
    fn polyphony_flood_is_rejected() {
        let steps: Vec<String> = (0..8)
            .map(|i| format!(r#"{{"position":{},"gate":16.0,"pitch":36.0}}"#, i * 60))
            .collect();
        let json = base_session_json()
            .replace(r#"{"id": "k", "role": "kick""#, r#"{"id": "k", "role": "bass""#)
            .replace(r#""instrument": {"kind": "kick"}}"#, r#""instrument": {"kind": "bass"}}"#)
            .replace(
                r#"{"generator": "euclidean", "k": 4, "n": 16}"#,
                &format!(r#"{{"steps":[{}]}}"#, steps.join(",")),
            );
        assert!(codes(&json).contains(&ErrorCatalog::E_POLYPHONY_EXCEEDED));
    }
}
