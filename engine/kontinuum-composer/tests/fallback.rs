//! The #36 fallback chain against real conditions: configured T2 → T1
//! on-device → T0 deterministic. The T2 rung is a live mock server that
//! answers slower than the configured timeout (real socket expiry, not a
//! stubbed error); degradation must be invisible — the wake still lands a
//! valid plan, the session stays playable, nothing surfaces to audio.

mod common;

use std::sync::Arc;
use std::time::Instant;

use kontinuum_compose::arrangement::{generate_session, GenParams};
use kontinuum_compose::engine::ArrangementEngine;
use kontinuum_composer::{
    build_plan_request, run_wake, BackendSelector, ComposerBackend, ScriptedBackend, Steering,
};
use kontinuum_ir::{validate_session, Session};

const GOOD_DIFF: &str =
    r#"{"op":"set_instrument_param","track":"kick","param":"decay_ms","value":220.0}"#;

fn session() -> Session {
    generate_session(&GenParams { seed: 23, target_bars: 32, ..Default::default() })
}

fn request(session: &Session) -> kontinuum_composer::PlanRequest {
    build_plan_request(session, 8, Steering {
        style: "techno",
        prompt: "darker",
        taste_json: "{}",
        style_card: "",
    })
}

/// Server that holds the connection longer than any reasonable timeout.
fn slow_server(hold_ms: u64) -> common::MockServer {
    common::MockServer::start(Arc::new(move |_req: &common::RecordedRequest| {
        std::thread::sleep(std::time::Duration::from_millis(hold_ms));
        (200, vec![], r#"{"diffs":[],"notes":"too late"}"#.to_string())
    }))
}

fn openai_backend(server: &common::MockServer, timeout_ms: u64) -> kontinuum_composer::HttpCloudBackend {
    kontinuum_composer::HttpCloudBackend::openai_chat(
        format!("{}/v1/chat/completions", server.base_url),
        timeout_ms,
        "qwen3.5-2b".into(),
        common::tcp_transport,
    )
}

#[test]
fn t2_timeout_degrades_to_t1_on_device_invisibly() {
    let slow = slow_server(400);
    let session = session();
    let mut t2 = openai_backend(&slow, 50);
    let mut t1 = ScriptedBackend::new("t1-foundation-models", vec![vec![GOOD_DIFF.to_string()]]);
    let mut t0 = kontinuum_composer::OnDeviceHeuristicBackend;

    let selector = BackendSelector { prefer_on_device: false, cloud_timeout_ms: 50, max_retries: 1 };
    let mut ladder = selector.ladder_tiers(
        Some(&mut t2 as &mut dyn ComposerBackend),
        Some(&mut t1 as &mut dyn ComposerBackend),
        &mut t0 as &mut dyn ComposerBackend,
    );
    let mut engine = ArrangementEngine::new(session.clone(), 48_000);
    let started = Instant::now();
    let report = run_wake(&mut engine, &mut ladder, &request(&session));
    let elapsed = started.elapsed();

    assert_eq!(report.backend, "t1-foundation-models", "T1 catches the timed-out T2");
    assert_eq!(report.applied, vec![GOOD_DIFF.to_string()]);
    assert_eq!(report.rejected, 0);
    assert!(
        slow.request_count() >= 1,
        "T2 really dialed out before the fall-through (exact retry budget is pinned by the stub tests)"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "degradation is bounded by the timeout budget, took {elapsed:?}"
    );
    // Invisible to audio: the session the engine holds is valid and alive.
    assert!(validate_session(&engine.current_session()).is_ok());
}

#[test]
fn t1_absent_or_failing_degrades_to_t0_and_the_session_still_lives() {
    let slow = slow_server(400);
    let session = session();

    // T2 times out, no T1 on this host (no Foundation Models session):
    // the ladder is T2 → T0.
    let mut t2 = openai_backend(&slow, 50);
    let mut t0 = kontinuum_composer::OnDeviceHeuristicBackend;
    let selector = BackendSelector { prefer_on_device: false, cloud_timeout_ms: 50, max_retries: 0 };
    let mut ladder = selector.ladder_tiers(
        Some(&mut t2 as &mut dyn ComposerBackend),
        None,
        &mut t0 as &mut dyn ComposerBackend,
    );
    let mut engine = ArrangementEngine::new(session.clone(), 48_000);
    let report = run_wake(&mut engine, &mut ladder, &request(&session));
    assert_eq!(report.backend, "heuristic", "T0 floor serves the wake");
    assert!(!report.applied.is_empty(), "session keeps evolving");

    // A failing T1 (host bridge error) is equally invisible.
    let mut t2 = openai_backend(&slow, 50);
    let mut t1_failing = FailingBridge;
    let mut t0 = kontinuum_composer::OnDeviceHeuristicBackend;
    let mut ladder = selector.ladder_tiers(
        Some(&mut t2 as &mut dyn ComposerBackend),
        Some(&mut t1_failing as &mut dyn ComposerBackend),
        &mut t0 as &mut dyn ComposerBackend,
    );
    let mut engine = ArrangementEngine::new(session.clone(), 48_000);
    let report = run_wake(&mut engine, &mut ladder, &request(&session));
    assert_eq!(report.backend, "heuristic");
    assert!(!report.applied.is_empty());
    assert!(validate_session(&engine.current_session()).is_ok());
}

/// A T1 bridge that errors like an unavailable Foundation Models session.
struct FailingBridge;

impl ComposerBackend for FailingBridge {
    fn name(&self) -> &str {
        "t1-foundation-models"
    }
    fn plan(
        &mut self,
        _: &kontinuum_composer::PlanRequest,
    ) -> Result<kontinuum_composer::PlanResponse, kontinuum_composer::BackendError> {
        Err(kontinuum_composer::BackendError::Transport(
            "Foundation Models session unavailable".into(),
        ))
    }
}

#[test]
fn degradation_repairs_never_touch_disabled_rungs_and_telemetry_stays_attributed() {
    // The T2 runs out of attempts; no repair round may leak into it after
    // the fall-through, and the report names only the serving backend.
    let slow = slow_server(400);
    let session = session();
    let mut t2 = openai_backend(&slow, 50);
    let mut t1 = ScriptedBackend::new("t1-foundation-models", vec![vec![GOOD_DIFF.to_string()]]);
    let mut t0 = kontinuum_composer::OnDeviceHeuristicBackend;
    let selector = BackendSelector { prefer_on_device: false, cloud_timeout_ms: 50, max_retries: 0 };
    let mut ladder = selector.ladder_tiers(
        Some(&mut t2 as &mut dyn ComposerBackend),
        Some(&mut t1 as &mut dyn ComposerBackend),
        &mut t0 as &mut dyn ComposerBackend,
    );
    let mut engine = ArrangementEngine::new(session.clone(), 48_000);
    let report = run_wake(&mut engine, &mut ladder, &request(&session));
    assert_eq!(report.backend, "t1-foundation-models");
    assert_eq!(report.repairs, 0, "a clean plan spends no repair rounds");
    assert_eq!(slow.request_count(), 1, "exhausted T2 is abandoned, not retried");

    let mut telemetry = kontinuum_composer::ComposerBackendTelemetry::default();
    telemetry.record_plan(
        &report.backend,
        kontinuum_composer::Tier::of_backend(&report.backend),
        report.applied.len() + report.rejected,
        report.rejected,
        report.repairs,
        12,
    );
    let rows = telemetry.rows(0.25);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].tier, kontinuum_composer::Tier::T1OnDevice);
    assert!(rows[0].invalid_ir_rate < f32::EPSILON);
}
