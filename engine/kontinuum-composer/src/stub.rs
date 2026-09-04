//! Test double for cloud LLM backends (crate-internal, `#[cfg(test)]` only).
//! Deterministic by construction: scripted diff batches replay in order and
//! the last batch repeats; failure modes are fixed responses, never time- or
//! RNG-dependent.

use std::collections::VecDeque;

use crate::backend::{BackendError, ComposerBackend, PlanRequest, PlanResponse};

enum StubMode {
    /// Diff batches served in order; the last repeats when exhausted.
    Scripted(VecDeque<PlanResponse>),
    Timeout,
    HardError,
    /// Always answers with diffs that fail the validation gate.
    AlwaysInvalid(Vec<String>),
    /// First call answers `bad`, every later call `good` — models an LLM
    /// that self-corrects when given the validation errors.
    Repairable { bad: Vec<String>, good: Vec<String> },
}

pub(crate) struct StubLlmBackend {
    mode: StubMode,
    calls: u32,
    last_repair_context: String,
}

impl StubLlmBackend {
    pub(crate) fn scripted(batches: Vec<Vec<String>>) -> Self {
        let script =
            batches.into_iter().map(|diffs| response(diffs, "stub: scripted")).collect();
        StubLlmBackend { mode: StubMode::Scripted(script), calls: 0, last_repair_context: String::new() }
    }

    pub(crate) fn timing_out() -> Self {
        StubLlmBackend { mode: StubMode::Timeout, calls: 0, last_repair_context: String::new() }
    }

    pub(crate) fn hard_failing() -> Self {
        StubLlmBackend { mode: StubMode::HardError, calls: 0, last_repair_context: String::new() }
    }

    pub(crate) fn always_invalid(diffs: Vec<String>) -> Self {
        StubLlmBackend {
            mode: StubMode::AlwaysInvalid(diffs),
            calls: 0,
            last_repair_context: String::new(),
        }
    }

    pub(crate) fn repairable(bad: Vec<String>, good: Vec<String>) -> Self {
        StubLlmBackend {
            mode: StubMode::Repairable { bad, good },
            calls: 0,
            last_repair_context: String::new(),
        }
    }

    pub(crate) fn calls(&self) -> u32 {
        self.calls
    }

    pub(crate) fn last_repair_context(&self) -> &str {
        &self.last_repair_context
    }
}

impl ComposerBackend for StubLlmBackend {
    fn name(&self) -> &str {
        "stub-llm"
    }

    fn plan(&mut self, request: &PlanRequest) -> Result<PlanResponse, BackendError> {
        self.calls += 1;
        self.last_repair_context = request.repair_context.clone();
        match &mut self.mode {
            StubMode::Timeout => Err(BackendError::Timeout(8_000)),
            StubMode::HardError => Err(BackendError::Transport("stub hard failure".into())),
            StubMode::Scripted(script) => {
                if script.is_empty() {
                    return Ok(response(Vec::new(), "stub: empty script"));
                }
                let idx = (self.calls as usize - 1).min(script.len() - 1);
                Ok(script[idx].clone())
            }
            StubMode::AlwaysInvalid(diffs) => Ok(response(diffs.clone(), "stub: always invalid")),
            StubMode::Repairable { bad, good } => {
                let diffs = if self.calls == 1 { bad.clone() } else { good.clone() };
                Ok(response(diffs, "stub: repairable"))
            }
        }
    }
}

fn response(diffs: Vec<String>, notes: &str) -> PlanResponse {
    PlanResponse {
        diffs,
        notes: notes.into(),
        backend_id: "stub-llm".into(),
        latency_hint_ms: 1,
    }
}
