//! The composer pass (issue #43): wake, ask backends for IR diffs, validate
//! every diff against the live session, apply only valid ones. PLAN §2.2
//! rule 2 enforced here: invalid plans are rejected and counted; the session
//! keeps performing.
//!
//! [`validate_diffs`] is the single validation gate, shared with the
//! validated-diff wake loop in [`crate::wake`] (issue #22).

use crate::{ComposerBackend, PlanRequest};
use kontinuum_ir::{apply_diff, validate_session, IrDiff, Session, ValidationError};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ComposerReport {
    pub backend: String,
    pub applied: Vec<String>,
    pub rejected: usize,
    pub notes: String,
    /// Content-repair replans spent inside the wake loop (issue #22);
    /// 0 for a single pass.
    #[serde(default)]
    pub repairs: u32,
}

/// Outcome of gating one plan's diffs: the raw JSON + parsed op for every
/// diff that survived, the rejection count, and per-diff failure text
/// (machine-readable, fed back into repair requests).
pub(crate) struct GateResult {
    pub valid: Vec<(String, IrDiff)>,
    pub rejected: usize,
    pub problems: Vec<String>,
}

/// Validates each raw diff against `scratch`: parse → apply (future
/// anchoring, last-writer-wins) → full session validation. Valid ops
/// accumulate on the scratch so later diffs see earlier ones; a failing op
/// is rolled back before the next candidate is tried.
pub(crate) fn validate_diffs(scratch: &mut Session, at_bar: u32, diffs: &[String]) -> GateResult {
    let mut result = GateResult { valid: Vec::new(), rejected: 0, problems: Vec::new() };
    for raw in diffs {
        let snapshot = scratch.clone();
        let outcome = match serde_json::from_str::<IrDiff>(raw) {
            Err(e) => Err(format!("parse: {e}")),
            Ok(diff) => apply_diff(scratch, &diff, at_bar)
                .map_err(|e| format!("apply: {e}"))
                .and_then(|_| {
                    validate_session(scratch)
                        .map(|_| diff)
                        .map_err(|errs| format!("validate: {}", join_errors(&errs)))
                }),
        };
        match outcome {
            Ok(diff) => result.valid.push((raw.clone(), diff)),
            Err(reason) => {
                *scratch = snapshot;
                #[cfg(test)]
                eprintln!("DIFF REJECTED: {reason} :: {raw}");
                result.rejected += 1;
                result.problems.push(format!("{reason} [diff: {raw}]"));
            }
        }
    }
    result
}

/// Validation errors as one compact, machine-actionable string.
fn join_errors(errors: &[ValidationError]) -> String {
    errors
        .iter()
        .map(|e| format!("{}: {} (fix: {})", e.code, e.message, e.suggested_fix))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Runs one composer pass: sessions in JSON, diffs out — validated against
/// the real session (L1+L2+L3 via apply_diff semantics) before acceptance.
/// `at_bar` anchors diffs to the next musical boundary (the engine's diff
/// pipeline re-anchors onto the boundary itself). Backends are tried in
/// order; the first one producing at least one valid diff wins.
pub fn run_composer_pass(
    session: &Session,
    at_bar: u32,
    backends: &mut [&mut dyn ComposerBackend],
    request: &PlanRequest,
) -> ComposerReport {
    for backend in backends.iter_mut() {
        let response = match backend.plan(request) {
            Ok(r) => r,
            Err(_) => continue, // fall through to the next backend
        };
        let mut scratch = session.clone();
        let gate = validate_diffs(&mut scratch, at_bar, &response.diffs);
        if !gate.valid.is_empty() {
            return ComposerReport {
                backend: backend.name().to_string(),
                applied: gate.valid.into_iter().map(|(raw, _)| raw).collect(),
                rejected: gate.rejected,
                notes: response.notes,
                repairs: 0,
            };
        }
        // A backend that produced only rejected diffs: try the next one.
    }
    ComposerReport::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendError, PlanContext, PlanResponse};
    use crate::OnDeviceHeuristicBackend;

    fn session_json() -> String {
        let params = kontinuum_compose::arrangement::GenParams {
            seed: 11,
            target_bars: 16,
            bpm: Some(124.0),
            intensity: 0.7,
            genre: Some("techno".into()),
            ..Default::default()
        };
        let session = kontinuum_compose::arrangement::generate_session(&params);
        serde_json::to_string(&session).unwrap()
    }

    fn request() -> PlanRequest {
        PlanRequest {
            style: "techno".into(),
            prompt: "darker".into(),
            bars_left_in_section: 4,
            progression: vec![(29, true)],
            taste_json: "{}".into(),
            style_card: String::new(),
            context: PlanContext::default(),
            repair_context: String::new(),
        }
    }

    struct GarbageBackend;
    impl ComposerBackend for GarbageBackend {
        fn name(&self) -> &str { "garbage" }
        fn plan(&mut self, _r: &PlanRequest) -> Result<PlanResponse, BackendError> {
            Ok(PlanResponse {
                diffs: vec![
                    "{ not a diff }".to_string(),
                    r#"{"op":"set_instrument_param","track":"kick","param":"tune_hz","value":99.0}"#.to_string(),
                ],
                notes: "misbehaving".into(),
                ..Default::default()
            })
        }
    }

    struct AllInvalidBackend;
    impl ComposerBackend for AllInvalidBackend {
        fn name(&self) -> &str { "all-invalid" }
        fn plan(&mut self, _r: &PlanRequest) -> Result<PlanResponse, BackendError> {
            Ok(PlanResponse {
                diffs: vec![
                    "{ not a diff }".to_string(),
                    r#"{"op":"set_instrument_param","track":"kick","param":"tune_hz","value":999.0}"#.to_string(),
                ],
                notes: "half-broken output".into(),
                ..Default::default()
            })
        }
    }

    #[test]
    fn pass_applies_valid_diffs_and_counts_rejections() {
        // One valid diff among garbage: the valid one is applied, the rest
        // counted as rejected — a half-good plan is still usable.
        let session: Session = serde_json::from_str(&session_json()).unwrap();
        let mut backends: [&mut dyn ComposerBackend; 1] = [&mut GarbageBackend];
        let report = run_composer_pass(&session, 4, &mut backends, &request());
        assert_eq!(report.backend, "garbage");
        assert_eq!(report.applied.len(), 1, "valid tune_hz diff must apply");
        assert_eq!(report.rejected, 1, "the unparseable diff is counted");
        assert_eq!(report.repairs, 0, "single pass, no repair loop");
    }

    #[test]
    fn all_invalid_backend_falls_through_to_heuristic() {
        let session: Session = serde_json::from_str(&session_json()).unwrap();
        let mut invalid = AllInvalidBackend;
        let mut heuristic = OnDeviceHeuristicBackend;
        let mut backends: [&mut dyn ComposerBackend; 2] = [&mut invalid, &mut heuristic];
        let report = run_composer_pass(&session, 4, &mut backends, &request());
        assert_eq!(report.backend, "heuristic", "all-invalid backend is skipped");
        assert!(report.rejected >= 1, "its invalid diffs are counted");
    }

    #[test]
    fn failing_backend_falls_through_to_next() {
        let session: Session = serde_json::from_str(&session_json()).unwrap();
        let mut broken = BrokenBackend;
        let mut heuristic = OnDeviceHeuristicBackend;
        let mut backends: [&mut dyn ComposerBackend; 2] = [&mut broken, &mut heuristic];
        let report = run_composer_pass(&session, 4, &mut backends, &request());
        assert_eq!(report.backend, "heuristic", "timeout errors are skipped like any other");
    }

    #[test]
    fn empty_backends_give_empty_report() {
        let session: Session = serde_json::from_str(&session_json()).unwrap();
        let mut backends: [&mut dyn ComposerBackend; 0] = [];
        let report = run_composer_pass(&session, 4, &mut backends, &request());
        assert!(report.applied.is_empty());
        assert_eq!(report.backend, "");
    }

    struct BrokenBackend;
    impl ComposerBackend for BrokenBackend {
        fn name(&self) -> &str { "broken" }
        fn plan(&mut self, _r: &PlanRequest) -> Result<PlanResponse, BackendError> {
            Err(BackendError::Timeout(8_000))
        }
    }
}
