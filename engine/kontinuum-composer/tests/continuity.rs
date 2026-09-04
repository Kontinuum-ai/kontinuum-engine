//! Session-continuity invariant under steering, tested adversarially
//! (issue #22): diffs only touch the future. The gate enforces it in the
//! op applier (`apply_diff` → `InPast`); these tests attack it from the
//! steering surface with past-targeting ops, and prove the audible
//! guarantee at the block level — every bar before the next boundary stays
//! bit-identical after a steering move.

use kontinuum_compose::arrangement::{generate_session, GenParams};
use kontinuum_compose::engine::ArrangementEngine;
use kontinuum_composer::{
    apply_plan, plan_ops, run_steering, ComposerTelemetry, ContextInputs,
    PlannedOp, QuickChip, ScriptedBackend, ScriptedSteeringProvider,
    SteeringDirective, SteeringPlan, SteeringSource, SteeringVector, Tier,
};
use kontinuum_ir::schema::EuclideanPattern;
use kontinuum_ir::schema::{EuclideanTag, Pattern};
use kontinuum_ir::{IrDiff, Session};
use kontinuum_schedule::{BlockSource, CompiledBlock};

const SR: u32 = 48_000;

fn engine() -> ArrangementEngine {
    ArrangementEngine::new(session(), SR)
}

fn session() -> Session {
    generate_session(&GenParams { seed: 7, target_bars: 32, ..Default::default() })
}

fn past_targeting_op() -> IrDiff {
    IrDiff::ReplacePattern {
        section: "intro".into(),
        track: "kick".into(),
        pattern: Pattern::Euclidean(EuclideanPattern {
            generator: EuclideanTag::Euclidean,
            k: 1,
            n: 16,
            rot: 0,
            velocity: 0.9,
            probability: 1.0,
            repeats: 1,
            gate: None,
            pitch: None,
        }),
    }
}

fn plan_with(past_op: IrDiff, at_bar: u32) -> SteeringPlan {
    SteeringPlan {
        t0: vec![
            PlannedOp {
                landing_bar: at_bar + 4,
                diff: past_op,
            },
        ],
        composition: vec![],
    }
}

fn fingerprint(blocks: &[CompiledBlock]) -> String {
    blocks.iter().map(|b| format!("{b:?}")).collect::<Vec<_>>().join("|")
}

fn warm_blocks(engine: &mut ArrangementEngine, through_bar: u32) -> Vec<CompiledBlock> {
    let mut blocks = Vec::new();
    let mut bar = 0;
    while bar < through_bar {
        let b = engine.block_for_bars(bar, 4).expect("block").as_ref().clone();
        bar += b.bars;
        blocks.push(b);
    }
    blocks
}

#[test]
fn past_targeting_steering_op_is_rejected_and_drops_nothing() {
    let mut engine = engine();
    let at_bar = 20;
    let before = engine.current_session().clone();
    let mut telemetry = ComposerTelemetry::default();
    let outcome =
        apply_plan(&mut engine, &plan_with(past_targeting_op(), at_bar), at_bar, &mut telemetry, Tier::T0Deterministic);
    assert!(outcome.applied.is_empty(), "the past-targeting op never lands");
    assert_eq!(outcome.dropped, 1, "dropped and logged, not retried forever");
    assert_eq!(engine.current_session(), &before, "the session is untouched");
}

#[test]
fn steered_session_keeps_every_played_bar_bit_identical() {
    let mut engine = engine();
    let at_bar = 10;

    let heard = warm_blocks(&mut engine, at_bar);
    let heard_fp = fingerprint(&heard);

    // Energy steering: SetSectionEnergy never touches compiled blocks (the
    // compiler does not read curves), so the played past is bit-identical
    // for any grammar-drawn layout (#16). The mute/density pattern ops are
    // the #38 live-edit carve-out — they deliberately rewrite the block
    // straddling the playhead, played bars included, so they are not part
    // of this continuity contract.
    let directive = SteeringDirective {
        vector: SteeringVector::new(0.5, 0.0, 0.0, 0.0, 0.0, 0.0),
        intents: vec![],
        source: SteeringSource::QuickChip,
        notes: "adversarial".into(),
    };
    let mut telemetry = ComposerTelemetry::default();
    let plan = plan_ops(&directive, engine.current_session(), at_bar);
    let outcome =
        apply_plan(&mut engine, &plan, at_bar, &mut telemetry, Tier::T0Deterministic);
    assert!(!outcome.applied.is_empty(), "the steering actually moved something");

    let after = warm_blocks(&mut engine, at_bar);
    assert_eq!(fingerprint(&after), heard_fp, "bars 0..{at_bar} are bit-identical after steering");
}

#[test]
fn steering_audibility_lands_within_the_t0_window() {
    let mut engine = engine();
    let at_bar = 9;
    let heard = warm_blocks(&mut engine, at_bar);

    let mut telemetry = ComposerTelemetry::default();
    let plan = plan_ops(&SteeringDirective::chip(QuickChip::MoreEnergy), engine.current_session(), at_bar);
    let outcome =
        apply_plan(&mut engine, &plan, at_bar, &mut telemetry, Tier::T0Deterministic);
    assert!(!outcome.applied.is_empty());
    assert!(
        outcome.bars_to_audible <= 4,
        "T0 move audible in ≤ 4 bars, got {}",
        outcome.bars_to_audible
    );

    // The playhead's own 4-bar block (bars 8..12) stays identical too — the
    // cache boundary only touches blocks at or after the landing boundary.
    let after = warm_blocks(&mut engine, at_bar);
    for (before, after) in heard.iter().zip(after.iter()) {
        assert_eq!(
            format!("{before:?}"),
            format!("{after:?}"),
            "block at bar {} changed before the landing boundary",
            before.start_bar
        );
    }
}

#[test]
fn scripted_backend_trying_to_rewrite_history_cannot() {
    let mut engine = engine();
    let at_bar = 16;
    let before = engine.current_session().clone();

    let history_rewrite = vec![serde_json::to_string(&past_targeting_op()).unwrap()];
    let mut emitter = ScriptedBackend::new("rogue-t1", vec![history_rewrite]);
    let mut telemetry = ComposerTelemetry::default();
    let outcome = run_steering(
        &mut engine,
        &mut ScriptedSteeringProvider,
        &mut emitter,
        at_bar,
        "bump the energy",
        ContextInputs::default(),
        &mut telemetry,
    );
    assert!(outcome.applied.is_empty(), "nothing past-targeting survives the gate");
    assert_eq!(engine.current_session(), &before, "history is immutable");
    assert_eq!(telemetry.t1.invalid_after_retry, 1, "the batch is dropped after one retry");
    assert_eq!(outcome.repairs, 1, "one bounded repair round was spent first");
}
