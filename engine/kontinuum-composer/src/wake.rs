//! What a composer wake does (issue #22): build a [`PlanRequest`] from live
//! session state, walk the backend ladder, and drive the **validated-diff
//! application loop** — every diff crosses `kontinuum-ir`'s apply/validate
//! gate, invalid plans get a bounded repair round (errors fed back into the
//! next request), and valid diffs are applied through the compose
//! [`ArrangementEngine`]'s diff API so its block cache stays coherent. No
//! backend failure can stall the session: the ladder always ends at the
//! infallible on-device heuristic. Cadence/steering scheduling lives in
//! [`crate::cadence`].

use kontinuum_compose::engine::ArrangementEngine;
use kontinuum_ir::Session;

use crate::backend::{LadderRung, PlanContext, PlanRequest};
use crate::orchestrator::{validate_diffs, ComposerReport};

/// Content-repair replans per backend response before the rung is abandoned.
pub const MAX_REPAIR_ROUNDS: u32 = 2;

/// The steering texts a wake plans from (user prompt, style, taste priors,
/// and the blended Creative Soul style card when souls are active).
#[derive(Clone, Copy, Debug, Default)]
pub struct Steering<'a> {
    pub style: &'a str,
    pub prompt: &'a str,
    /// Free-form taste priors until the preference crate is wired in.
    pub taste_json: &'a str,
    /// Blended soul style card (issue #55); empty when no souls are active.
    pub style_card: &'a str,
}

/// Builds the wake's [`PlanRequest`] from live session state plus steering
/// priors. The progression field stays empty until #46 exposes the drawn
/// progression on the session.
pub fn build_plan_request(session: &Session, current_bar: u32, steering: Steering<'_>) -> PlanRequest {
    let starts = session.section_start_bars();
    let bars_left_in_section = session
        .sections
        .iter()
        .zip(starts.iter())
        .find(|&(sec, start)| current_bar < start + sec.bars)
        .map(|(sec, start)| start + sec.bars - current_bar)
        .unwrap_or(0);
    PlanRequest {
        style: steering.style.into(),
        prompt: steering.prompt.into(),
        bars_left_in_section,
        progression: Vec::new(),
        taste_json: steering.taste_json.into(),
        style_card: steering.style_card.into(),
        context: PlanContext::from_session(session, current_bar),
        repair_context: String::new(),
    }
}

/// Runs one composer wake: walks the ladder, and for the first rung that
/// yields at least one valid diff, applies those diffs through the
/// [`ArrangementEngine`] (which invalidates its future block cache). Diffs
/// anchor at the playhead carried by the request
/// ([`PlanContext::current_bar`]) — the same bar the planner saw. Invalid
/// plans get up to [`MAX_REPAIR_ROUNDS`] replans with the validation errors
/// fed back via [`PlanRequest::repair_context`]; exhausted rungs fall
/// through — the last rung is always the infallible heuristic floor, so the
/// session never stalls. `repairs` in the report counts every repair round
/// spent across all rungs.
pub fn run_wake(
    engine: &mut ArrangementEngine,
    ladder: &mut [LadderRung<'_>],
    request: &PlanRequest,
) -> ComposerReport {
    let at_bar = request.context.current_bar;
    let mut repairs_spent = 0u32;
    for rung in ladder.iter_mut() {
        // Transport-level retries: timeout / hard error burns the rung's
        // attempt budget on a clean request, then falls to the next rung.
        let mut response = None;
        for _ in 0..rung.attempts {
            match rung.backend.plan(request) {
                Ok(r) => {
                    response = Some(r);
                    break;
                }
                Err(_) => continue,
            }
        }
        let Some(mut plan) = response else { continue };

        // Validated-diff application loop with bounded content repair.
        let mut repair_request = request.clone();
        let mut repairs_used = 0u32;
        loop {
            let mut scratch = engine.current_session().clone();
            let gate = validate_diffs(&mut scratch, at_bar, &plan.diffs);
            if !gate.valid.is_empty() {
                let mut applied = Vec::new();
                let mut rejected = gate.rejected;
                for (raw, diff) in &gate.valid {
                    match engine.apply_diff(diff, at_bar) {
                        Ok(_) => applied.push(raw.clone()),
                        Err(_) => rejected += 1,
                    }
                }
                return ComposerReport {
                    backend: rung.backend.name().to_string(),
                    applied,
                    rejected,
                    notes: plan.notes,
                    repairs: repairs_spent + repairs_used,
                };
            }
            if repairs_used >= MAX_REPAIR_ROUNDS {
                repairs_spent += repairs_used;
                break; // this rung cannot produce anything valid: next rung
            }
            repairs_used += 1;
            repair_request.repair_context = gate.problems.join("; ");
            match rung.backend.plan(&repair_request) {
                Ok(p) => plan = p,
                Err(_) => {
                    repairs_spent += repairs_used;
                    break; // repair round itself failed: next rung
                }
            }
        }
    }
    ComposerReport { repairs: repairs_spent, ..ComposerReport::default() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendSelector, ComposerBackend};
    use crate::stub::StubLlmBackend;
    use crate::OnDeviceHeuristicBackend;
    use kontinuum_compose::arrangement::{generate_session, GenParams};

    fn session() -> Session {
        generate_session(&GenParams { seed: 7, target_bars: 32, ..Default::default() })
    }

    fn engine() -> ArrangementEngine {
        ArrangementEngine::new(session(), 48_000)
    }

    fn request(session: &Session, prompt: &str) -> PlanRequest {
        build_plan_request(session, 8, Steering { style: "techno", prompt, taste_json: "{}", style_card: "" })
    }

    const GOOD_DIFF: &str =
        r#"{"op":"set_instrument_param","track":"kick","param":"decay_ms","value":220.0}"#;
    const OUT_OF_RANGE_DIFF: &str =
        r#"{"op":"set_instrument_param","track":"kick","param":"decay_ms","value":99999.0}"#;

    // -- request building ----------------------------------------------------

    #[test]
    fn build_plan_request_reports_bars_left_and_context() {
        let session = session();
        let req = build_plan_request(&session, 10, Steering { style: "techno", prompt: "darker", taste_json: "{}", style_card: "" });
        assert!(req.bars_left_in_section >= 1, "bar 10 is inside the session");
        assert_eq!(req.context.current_bar, 10);
        assert_eq!(req.context.sections[0].id, "intro");
        assert_eq!(req.context.sections[0].start_bar, 0);
        // The grammar (#16) draws intro lengths (8..=12) instead of a
        // fixed 8 — assert the recorded length matches the session.
        assert_eq!(req.context.sections[0].bars, session.sections[0].bars);
        assert_eq!(req.context.sections.last().unwrap().id, "outro");
        for track in ["kick", "perc", "bass", "pad"] {
            assert!(req.context.has_track(track), "palette always has {track}");
        }
        let past = build_plan_request(&session, 999, Steering::default());
        assert_eq!(past.bars_left_in_section, 0, "past the end: nothing left");
        assert!(past.context.future_section().is_none());
    }

    // -- validated-diff application loop --------------------------------------

    #[test]
    fn wake_applies_cloud_diffs_to_the_engine() {
        let mut engine = engine();
        let before = engine.current_session().clone();
        let mut heuristic = OnDeviceHeuristicBackend;
        let mut cloud = StubLlmBackend::scripted(vec![vec![GOOD_DIFF.to_string()]]);
        let selector = BackendSelector { prefer_on_device: false, ..Default::default() };
        let mut ladder =
            selector.ladder(&mut heuristic, Some(&mut cloud as &mut dyn ComposerBackend));
        let req = request(engine.current_session(), "keep going");
        let report = run_wake(&mut engine, &mut ladder, &req);
        assert_eq!(report.backend, "stub-llm");
        assert_eq!(report.applied, vec![GOOD_DIFF.to_string()]);
        assert_eq!(report.rejected, 0);
        assert_eq!(report.repairs, 0);
        assert_ne!(engine.current_session(), &before, "engine state moved");
    }

    #[test]
    fn cloud_timeout_falls_back_to_on_device_and_session_continues() {
        let mut engine = engine();
        let mut heuristic = OnDeviceHeuristicBackend;
        let mut cloud = StubLlmBackend::timing_out();
        let selector = BackendSelector {
            prefer_on_device: false,
            max_retries: 1,
            ..Default::default()
        };
        let mut ladder =
            selector.ladder(&mut heuristic, Some(&mut cloud as &mut dyn ComposerBackend));
        let req = request(engine.current_session(), "darker");
        let report = run_wake(&mut engine, &mut ladder, &req);
        assert_eq!(report.backend, "heuristic", "timeout falls through to the floor");
        assert!(!report.applied.is_empty(), "the session keeps evolving");
        assert_eq!(cloud.calls(), 2, "attempts = 1 + max_retries before fallback");
    }

    #[test]
    fn cloud_hard_failure_falls_back_to_on_device() {
        let mut engine = engine();
        let mut heuristic = OnDeviceHeuristicBackend;
        let mut cloud = StubLlmBackend::hard_failing();
        let selector = BackendSelector {
            prefer_on_device: false,
            max_retries: 1,
            ..Default::default()
        };
        let mut ladder =
            selector.ladder(&mut heuristic, Some(&mut cloud as &mut dyn ComposerBackend));
        let req = request(engine.current_session(), "darker");
        let report = run_wake(&mut engine, &mut ladder, &req);
        assert_eq!(report.backend, "heuristic", "a hard backend error is not a stall");
        assert!(!report.applied.is_empty(), "the session keeps evolving");
        assert_eq!(cloud.calls(), 2, "hard errors retry like timeouts before fallback");
    }

    #[test]
    fn invalid_diff_repair_feeds_errors_back_then_applies_fix() {
        let mut engine = engine();
        let mut heuristic = OnDeviceHeuristicBackend;
        let mut cloud = StubLlmBackend::repairable(
            vec![OUT_OF_RANGE_DIFF.to_string()],
            vec![GOOD_DIFF.to_string()],
        );
        let selector = BackendSelector { prefer_on_device: false, ..Default::default() };
        let mut ladder =
            selector.ladder(&mut heuristic, Some(&mut cloud as &mut dyn ComposerBackend));
        let req = request(engine.current_session(), "keep going");
        let report = run_wake(&mut engine, &mut ladder, &req);
        assert_eq!(report.backend, "stub-llm");
        assert_eq!(report.applied, vec![GOOD_DIFF.to_string()]);
        assert_eq!(report.repairs, 1, "one bounded repair round");
        assert!(
            cloud.last_repair_context().contains("E_KICK_DECAY_RANGE"),
            "validation errors are fed back into the next request: {:?}",
            cloud.last_repair_context()
        );
    }

    #[test]
    fn unrepairable_backend_falls_through_to_heuristic() {
        let mut engine = engine();
        let mut heuristic = OnDeviceHeuristicBackend;
        let mut cloud = StubLlmBackend::always_invalid(vec![OUT_OF_RANGE_DIFF.to_string()]);
        let selector = BackendSelector { prefer_on_device: false, ..Default::default() };
        let mut ladder =
            selector.ladder(&mut heuristic, Some(&mut cloud as &mut dyn ComposerBackend));
        let req = request(engine.current_session(), "darker");
        let report = run_wake(&mut engine, &mut ladder, &req);
        assert_eq!(report.backend, "heuristic", "exhausted rung is abandoned");
        assert!(!report.applied.is_empty(), "heuristic floor keeps the session alive");
        assert_eq!(report.repairs, MAX_REPAIR_ROUNDS, "repair effort accumulates across rungs");
    }

    #[test]
    fn wake_is_deterministic_for_same_inputs() {
        let run = || {
            let mut engine = engine();
            let mut heuristic = OnDeviceHeuristicBackend;
            let mut cloud = StubLlmBackend::scripted(vec![vec![GOOD_DIFF.to_string()]]);
            let selector = BackendSelector { prefer_on_device: false, ..Default::default() };
            let mut ladder =
                selector.ladder(&mut heuristic, Some(&mut cloud as &mut dyn ComposerBackend));
            let req = request(engine.current_session(), "keep going");
            let report = run_wake(&mut engine, &mut ladder, &req);
            let after = engine.current_session().clone();
            (serde_json::to_string(&report).unwrap(), serde_json::to_string(&after).unwrap())
        };
        assert_eq!(run(), run(), "same seed and script reproduce the wake exactly");
    }

    #[test]
    fn empty_ladder_gives_empty_report_without_touching_the_engine() {
        let mut engine = engine();
        let before = engine.current_session().clone();
        let mut ladder: Vec<LadderRung> = Vec::new();
        let req = request(engine.current_session(), "darker");
        let report = run_wake(&mut engine, &mut ladder, &req);
        assert_eq!(report.backend, "");
        assert_eq!(engine.current_session(), &before);
    }
}
