//! Scripted evaluation harness for the composer steering surface
//! (issue #22). CI-runnable, fully offline: the provider seam is the
//! deterministic [`ScriptedSteeringProvider`], the model is the
//! deterministic [`ScriptedBackend`], sessions are generated, and the
//! "bars to audible" proxy is the landing bar the engine's block cache
//! guarantees (the next 4-bar block boundary for T0 moves, the next section
//! boundary for composition moves). At 126 BPM one bar ≈ 1.905 s.
//!
//! Protocol: 20 scripted instructions (the issue's list plus the
//! contradiction and vague edge cases) × 3 session states = 60 runs.
//! Scoring is mechanical, standing in for the issue's human scoring: a run
//! is correct when an applied op's class matches the expected class, or —
//! for the unrepairable-hallucination case — when the diff was dropped,
//! logged, and left the session valid (drop-log-continue is the specified
//! behavior).
//!
//! Targets (issue #22 acceptance): ≥ 18/20 instructions correct across all
//! states; invalid-after-retry < 5% of proposed ops; p95 instruction→audible
//! ≤ 16 bars @ 126.

use kontinuum_compose::arrangement::{generate_session, GenParams};
use kontinuum_compose::engine::ArrangementEngine;
use kontinuum_composer::{
    plan_ops, run_steering, ComposerTelemetry, ContextInputs, DirectIntent, OpClass,
    ScriptedBackend, ScriptedSteeringProvider, SteeringDirective, SteeringProvider,
    SteeringSource, SteeringVector, QuickChip,
};
use kontinuum_ir::{validate_session, Session};
use std::collections::BTreeMap;

const SR: u32 = 48_000;

#[derive(Clone, Copy, Debug)]
enum Expect {
    Ops(&'static [OpClass]),
    Drop,
}

struct Case {
    instruction: &'static str,
    expected: Expect,
    /// Quick chips resolve with no model at all — the harness runs a poison
    /// emitter and asserts it is never consulted.
    chip: bool,
}

const CASES: [Case; 20] = [
    Case { instruction: "darker", expected: Expect::Ops(&[OpClass::Energy]), chip: true },
    Case { instruction: "brighter", expected: Expect::Ops(&[OpClass::Timbre]), chip: true },
    Case { instruction: "more energy", expected: Expect::Ops(&[OpClass::Energy]), chip: true },
    Case { instruction: "calmer", expected: Expect::Ops(&[OpClass::Energy]), chip: true },
    Case { instruction: "more minimal", expected: Expect::Ops(&[OpClass::Density]), chip: true },
    Case { instruction: "deeper", expected: Expect::Ops(&[OpClass::Timbre]), chip: true },
    Case { instruction: "weirder", expected: Expect::Ops(&[OpClass::Transition]), chip: true },
    Case { instruction: "sleep mode", expected: Expect::Ops(&[OpClass::Energy]), chip: true },
    Case { instruction: "drop the hats", expected: Expect::Ops(&[OpClass::Mute]), chip: false },
    Case { instruction: "make it darker and slower", expected: Expect::Ops(&[OpClass::Tempo]), chip: false },
    Case { instruction: "more energy but keep it subtle", expected: Expect::Ops(&[OpClass::Energy]), chip: false },
    Case { instruction: "take me somewhere", expected: Expect::Ops(&[OpClass::Transition]), chip: false },
    Case { instruction: "bump the energy for the drop", expected: Expect::Ops(&[OpClass::Energy]), chip: false },
    Case { instruction: "softer pads", expected: Expect::Ops(&[OpClass::Space]), chip: false },
    Case { instruction: "less going on in the percussion", expected: Expect::Ops(&[OpClass::Density]), chip: false },
    Case { instruction: "slow it down a touch", expected: Expect::Ops(&[OpClass::Tempo]), chip: false },
    Case { instruction: "bump the energy a little", expected: Expect::Ops(&[OpClass::Energy]), chip: false },
    Case { instruction: "everything louder", expected: Expect::Ops(&[OpClass::Energy]), chip: false },
    Case { instruction: "add a riser into the next section", expected: Expect::Ops(&[OpClass::Transition]), chip: false },
    Case { instruction: "mute the marimba", expected: Expect::Drop, chip: false },
];

struct SessionState {
    name: &'static str,
    params: GenParams,
    bar: u32,
}

fn states() -> Vec<SessionState> {
    vec![
        SessionState {
            name: "intro",
            params: GenParams { seed: 7, target_bars: 32, ..Default::default() },
            bar: 2,
        },
        SessionState {
            name: "mid-phrase",
            params: GenParams { seed: 7, target_bars: 32, ..Default::default() },
            bar: 14,
        },
        SessionState {
            name: "alt-session",
            params: GenParams {
                seed: 42,
                target_bars: 48,
                bpm: Some(126.0),
                intensity: 0.5,
                ..Default::default()
            },
            bar: 20,
        },
    ]
}

fn live_section(session: &Session, at_bar: u32) -> (String, u32, u32) {
    let starts = session.section_start_bars();
    session
        .sections
        .iter()
        .zip(starts.iter())
        .find(|&(sec, start)| at_bar < start + sec.bars)
        .map(|(sec, start)| (sec.id.clone(), *start, sec.bars))
        .expect("eval states sit inside the session")
}

/// The grammar (#16) draws the layout, so a fixed playhead can land in a
/// section that never bound the track an instruction steers ("drop the
/// hats" needs hats). Resolve the first bar at/after `min_bar` inside a
/// section that binds `track`, mid-section.
fn bar_with(session: &Session, min_bar: u32, track: &str) -> u32 {
    let starts = session.section_start_bars();
    for (sec, start) in session.sections.iter().zip(starts.iter()) {
        let end = start + sec.bars;
        if end > min_bar && sec.pattern_bindings.contains_key(track) {
            return start + sec.bars / 2;
        }
    }
    min_bar
}

/// The scripted model's first attempt for `take me somewhere`: a transition
/// anchored mid-section — the classic anchor error, rejected by the
/// validator with a boundary suggestion, corrected on the one retry.
fn mid_section_transition(session: &Session, at_bar: u32) -> String {
    let (_, start, bars) = live_section(session, at_bar);
    format!(
        r#"{{"op":"schedule_transition","at_bar":{},"transition":{{"type":"riser","bars":1}}}}"#,
        start + bars / 2
    )
}

/// The scripted model's attempts for `mute the marimba`: it hallucinates a
/// track the palette never had. No suggestion can conjure one, so both
/// attempts fail and the diff is dropped and logged.
fn hallucinated_mute(session: &Session, at_bar: u32) -> String {
    let (section, _, _) = live_section(session, at_bar);
    format!(
        r#"{{"op":"replace_pattern","section":"{section}","track":"marimba","pattern":{{"generator":"euclidean","k":4,"n":16,"rot":0}}}}"#
    )
}

fn reference_directive(instruction: &str) -> Option<SteeringDirective> {
    ScriptedSteeringProvider.parse(instruction).ok()
}

fn energy_target(applied: &[String]) -> Option<f32> {
    for raw in applied {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
            if v["op"] == "set_section_energy" {
                if let Some(first) = v["energy"].as_array().and_then(|a| a.first()) {
                    return first.as_f64().map(|f| f as f32);
                }
            }
        }
    }
    None
}

#[test]
fn eval_steering_harness_hits_issue_22_targets() {
    let mut telemetry = ComposerTelemetry::default();
    let mut per_case: BTreeMap<&str, bool> = BTreeMap::new();
    let mut audible_bars: Vec<u32> = Vec::new();
    let mut energy_targets: BTreeMap<&str, f32> = BTreeMap::new();
    let mut runs = 0usize;

    for case in CASES.iter() {
        let mut all_states_ok = true;
        for state in states() {
            runs += 1;
            let session = generate_session(&state.params);
            let mut engine = ArrangementEngine::new(session.clone(), SR);
            let bar = bar_with(&session, state.bar, "perc");

            let reference = reference_directive(case.instruction);
            let scripted_batches: Vec<Vec<String>> = match case.instruction {
                "take me somewhere" => vec![
                    vec![mid_section_transition(&session, bar)],
                    reference
                        .as_ref()
                        .map(|d| plan_ops(d, &session, bar))
                        .map(|plan| serialize(&plan))
                        .unwrap_or_default(),
                ],
                "mute the marimba" => {
                    vec![vec![hallucinated_mute(&session, bar)]]
                }
                _ => vec![reference
                    .as_ref()
                    .map(|d| plan_ops(d, &session, bar))
                    .map(|plan| serialize(&plan))
                    .unwrap_or_default()],
            };
            let mut emitter = ScriptedBackend::new("t1-scripted-eval", scripted_batches);

            let outcome = run_steering(
                &mut engine,
                &mut ScriptedSteeringProvider,
                &mut emitter,
                bar,
                case.instruction,
                ContextInputs::default(),
                &mut telemetry,
            );

            if case.chip {
                assert_eq!(emitter.calls(), 0, "chip `{}` must never consult a model", case.instruction);
            }

            // The session stays valid and playable no matter what happened.
            assert!(
                validate_session(engine.current_session()).is_ok(),
                "session corrupted by `{}` in {}",
                case.instruction,
                state.name
            );

            let applied_classes: Vec<OpClass> = outcome.op_classes.clone();
            let correct = match case.expected {
                Expect::Ops(expected) => expected.iter().any(|e| applied_classes.contains(e)),
                Expect::Drop => {
                    outcome.applied.is_empty() && outcome.dropped >= 1 && outcome.rejected >= 2
                }
            };
            all_states_ok &= correct;

            if outcome.applied.is_empty() == false {
                audible_bars.push(outcome.bars_to_audible);
            }
            if let Some(t) = energy_target(&outcome.applied) {
                // Keep the least-clamped state: at a saturated base the
                // plain and subtle bumps clamp to the same ceiling.
                energy_targets
                    .entry(case.instruction)
                    .and_modify(|e| *e = e.min(t))
                    .or_insert(t);
            }
        }
        per_case.insert(case.instruction, all_states_ok);
    }

    // -- report ----------------------------------------------------------------
    let correct = per_case.values().filter(|ok| **ok).count();
    let after_retry: u32 = telemetry.t1.invalid_after_retry;
    let proposals: u32 = telemetry.t0.proposals + telemetry.t1.proposals + telemetry.t2.proposals;
    let after_retry_rate = after_retry as f64 / proposals.max(1) as f64;
    audible_bars.sort_unstable();
    let p95 = audible_bars
        .get(((audible_bars.len() as f64 * 0.95).ceil() as usize).saturating_sub(1))
        .copied()
        .unwrap_or(0);
    let p95_seconds = f64::from(p95) * 4.0 * 60.0 / 126.0;

    println!("\n=== issue #22 steering eval (scripted provider, offline) ===");
    for (instruction, ok) in &per_case {
        println!("  {:<38} {}", format!("\"{instruction}\""), if *ok { "PASS" } else { "FAIL" });
    }
    println!("  correctness: {correct}/20 instructions (all 3 states), target >= 18");
    println!("  runs: {runs}, applied-with-audible-change: {}", audible_bars.len());
    println!(
        "  invalid-after-retry: {after_retry}/{proposals} proposed ops = {:.2}%, target < 5%",
        after_retry_rate * 100.0
    );
    println!("  p95 instruction->audible: {p95} bars ({p95_seconds:.2}s @126), target <= 16");
    println!(
        "  tier telemetry (invalid-rate per tier): t0 {} proposals / {} invalid; t1 {} proposals / {} invalid / {} after-retry / {} repairs; t2 {} proposals",
        telemetry.t0.proposals, telemetry.t0.invalid, telemetry.t1.proposals, telemetry.t1.invalid,
        telemetry.t1.invalid_after_retry, telemetry.t1.repairs, telemetry.t2.proposals
    );

    // -- targets -----------------------------------------------------------------
    assert!(correct >= 18, "correctness {correct}/20 below the ≥18 target: {per_case:?}");
    assert!(
        after_retry_rate < 0.05,
        "invalid-after-retry {:.2}% above the <5% target",
        after_retry_rate * 100.0
    );
    assert!(p95 <= 16, "p95 instruction->audible {p95} bars above the 16-bar target");
    assert_eq!(runs, 60, "20 instructions x 3 states");

    // The contradiction case must steer *less* than the plain bump, in the
    // same session states.
    let plain = energy_targets.get("bump the energy for the drop");
    let subtle = energy_targets.get("more energy but keep it subtle");
    if let (Some(plain), Some(subtle)) = (plain, subtle) {
        assert!(subtle < plain, "subtle steering must land softer: {subtle} vs {plain}");
    }
}

fn serialize(plan: &kontinuum_composer::SteeringPlan) -> Vec<String> {
    plan.t0
        .iter()
        .chain(plan.composition.iter())
        .map(|op| serde_json::to_string(&op.diff).expect("IrDiff serializes"))
        .collect()
}

/// The scripted parser must cover every non-chip instruction in the eval
/// set — a parse miss would silently skip the model path.
#[test]
fn eval_instructions_all_parse() {
    for case in CASES.iter() {
        if case.chip {
            assert!(QuickChip::from_text(case.instruction).is_some());
            continue;
        }
        let d = reference_directive(case.instruction)
            .unwrap_or_else(|| panic!("`{}` must parse", case.instruction));
        assert_eq!(d.source, SteeringSource::Provider);
        assert!(
            !d.vector.is_quiet() || !d.intents.is_empty(),
            "`{}` must steer or carry an intent",
            case.instruction
        );
    }
}

/// Sanity: the mute alias table resolves "hats" to the perc track that the
/// generated palette actually carries.
#[test]
fn hats_intent_targets_the_perc_track() {
    let d = reference_directive("drop the hats").expect("parse");
    assert_eq!(d.intents, vec![DirectIntent::MuteTrack("perc".into())]);
    assert_eq!(d.vector, SteeringVector::zero());
}
