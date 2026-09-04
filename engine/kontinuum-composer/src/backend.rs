//! The provider-agnostic planning trait and the escalation ladder policy
//! (issues #41/#36). Contract: backends return [`PlanResponse`] — a batch of
//! IR diff JSON strings plus metadata — and are fallible; the orchestrator
//! never trusts a diff that hasn't been validated through `kontinuum-ir`.
//!
//! Ladder (issue #36 tiers): [`crate::OnDeviceHeuristicBackend`] is the
//! always-available T0 floor (airplane mode keeps playing); a T1 on-device
//! LLM rung (Apple Foundation Models over the Swift bridge, host-provided)
//! and a T2 remote BYOK rung sit above it. [`BackendSelector`] orders the
//! rungs T2 → T1 → T0 and owns the timeout/retry policy; on timeout or
//! transport error the caller falls through to the next rung, ending at the
//! infallible on-device floor.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// What the composer asks a backend to plan.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanRequest {
    pub style: String,
    pub prompt: String,
    pub bars_left_in_section: u32,
    pub progression: Vec<(u8, bool)>,
    pub taste_json: String,
    /// Creative Soul style card (issue #55): the blended identity fragments
    /// the active souls contribute to the plan — the pack content, unlike
    /// `style`, is prose the model reads. Empty when no souls are active.
    #[serde(default)]
    pub style_card: String,
    /// Live session snapshot the planner may target (empty on hosts that
    /// don't supply one; backends then fall back to legacy "intro" targeting).
    #[serde(default)]
    pub context: PlanContext,
    /// Validation errors from the previous attempt, fed back by the
    /// orchestrator's bounded repair loop (issue #22). Empty on first plan.
    #[serde(default)]
    pub repair_context: String,
}

/// Session-state summary a wake builds its request from (issue #22).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PlanContext {
    pub current_bar: u32,
    pub total_bars: u32,
    /// Track ids present in the session ("kick", "bass", "perc", "pad"...).
    pub tracks: Vec<String>,
    pub sections: Vec<SectionSummary>,
}

/// One section's write-relevant geometry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SectionSummary {
    pub id: String,
    pub start_bar: u32,
    pub bars: u32,
}

impl PlanContext {
    /// Summarizes the session at a playhead position.
    pub fn from_session(session: &kontinuum_ir::Session, current_bar: u32) -> Self {
        let starts = session.section_start_bars();
        PlanContext {
            current_bar,
            total_bars: session.total_bars() as u32,
            tracks: session.tracks.iter().map(|t| t.id.clone()).collect(),
            sections: session
                .sections
                .iter()
                .zip(starts)
                .map(|(sec, start_bar)| SectionSummary {
                    id: sec.id.clone(),
                    start_bar,
                    bars: sec.bars,
                })
                .collect(),
        }
    }

    /// First section starting at or after the playhead — the safe write
    /// target for future-anchored ops.
    pub fn future_section(&self) -> Option<&SectionSummary> {
        self.sections.iter().find(|s| s.start_bar >= self.current_bar)
    }

    pub fn has_track(&self, id: &str) -> bool {
        self.tracks.iter().any(|t| t == id)
    }
}

/// The composer output: IR diff JSON strings, validated by the engine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct PlanResponse {
    pub diffs: Vec<String>,
    pub notes: String,
    /// Which backend produced this plan (echoed into the composer report).
    #[serde(default)]
    pub backend_id: String,
    /// Upper-bound planning latency hint in milliseconds (0 = negligible,
    /// on-device).
    #[serde(default)]
    pub latency_hint_ms: u64,
}

/// Why a backend failed. The orchestrator treats every variant the same way:
/// fall through to the next ladder rung.
#[derive(Clone, Debug, PartialEq, Error)]
pub enum BackendError {
    #[error("backend timed out after {0}ms")]
    Timeout(u64),
    #[error("transport failure: {0}")]
    Transport(String),
    #[error("unusable response: {0}")]
    BadResponse(String),
}

/// What a backend advertises to the picker and the ladder (issue #36).
/// Purely descriptive — the validated-diff gate, not these flags, is the
/// trust boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Caps {
    /// Provider can take a JSON schema constraining the response shape
    /// (OpenAI json_schema, Gemini responseSchema, Anthropic tool input
    /// schemas). False = prompt-for-JSON + validator-repair.
    pub json_schema: bool,
    pub latency_class: LatencyClass,
    /// Works with no network (airplane-mode invariant, PLAN §2.4).
    pub offline: bool,
    pub cost_class: CostClass,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LatencyClass {
    /// No network round trip (heuristic, on-device LLM).
    OnDevice,
    /// A remote call bounds the wake — the ladder's timeout applies.
    Network,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostClass {
    /// No marginal cost per call.
    Free,
    /// BYOK metered tokens.
    Metered,
}

impl Caps {
    /// The on-device default: no schema constraint needed (the heuristic
    /// backend emits valid diffs by construction), instant, free, offline.
    pub fn on_device() -> Self {
        Caps {
            json_schema: false,
            latency_class: LatencyClass::OnDevice,
            offline: true,
            cost_class: CostClass::Free,
        }
    }

    /// A remote BYOK provider: network latency, metered, schema support
    /// depends on the wire format.
    pub fn remote(json_schema: bool) -> Self {
        Caps {
            json_schema,
            latency_class: LatencyClass::Network,
            offline: false,
            cost_class: CostClass::Metered,
        }
    }
}

pub trait ComposerBackend: Send {
    fn name(&self) -> &str;
    /// What the Settings picker and ladder show for this backend.
    fn capabilities(&self) -> Caps {
        Caps::on_device()
    }
    /// Applies the escalation policy's timeout budget. Default: no-op —
    /// on-device backends don't block, so they don't need one.
    fn set_timeout_ms(&mut self, _timeout_ms: u64) {}
    /// Plan a batch of IR diffs for the running session. Implementations must
    /// be fallible (network, parse) — the orchestrator falls through on
    /// error. Returned diffs are *candidates*: they cross the kontinuum-ir
    /// validate/apply gate before anything reaches the engine.
    fn plan(&mut self, request: &PlanRequest) -> Result<PlanResponse, BackendError>;
}

/// One rung of the escalation ladder: a backend plus its error-retry budget
/// (transport-level retries; content repairs are budgeted separately by the
/// orchestrator).
pub struct LadderRung<'a> {
    pub backend: &'a mut dyn ComposerBackend,
    pub attempts: u32,
}

/// Escalation policy (issue #41). The on-device heuristic rung is always
/// appended last — a session never stalls, whatever the cloud does.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BackendSelector {
    /// true: try on-device first and escalate to cloud only when it yields
    /// nothing applicable; false: cloud first, on-device as fallback.
    pub prefer_on_device: bool,
    /// Hard timeout handed to the cloud backend for every call.
    pub cloud_timeout_ms: u64,
    /// Extra cloud attempts on timeout/transport error before falling back.
    pub max_retries: u32,
}

impl Default for BackendSelector {
    fn default() -> Self {
        BackendSelector {
            prefer_on_device: true,
            cloud_timeout_ms: crate::ESCALATION_TIMEOUT_MS,
            max_retries: 1,
        }
    }
}

#[cfg(test)]
mod tier_tests {
    use super::*;
    use crate::scripted::ScriptedBackend;

    #[test]
    fn tiers_order_t2_t1_t0_and_budget_attempts() {
        let mut t2 = ScriptedBackend::new("t2-cloud", vec![]);
        let mut t1 = ScriptedBackend::new("t1-foundation-models", vec![]);
        let mut t0 = ScriptedBackend::new("heuristic", vec![]);
        let ladder = BackendSelector { max_retries: 2, ..Default::default() }.ladder_tiers(
            Some(&mut t2 as &mut dyn ComposerBackend),
            Some(&mut t1 as &mut dyn ComposerBackend),
            &mut t0 as &mut dyn ComposerBackend,
        );
        assert_eq!(ladder.len(), 3);
        assert_eq!(ladder[0].backend.name(), "t2-cloud");
        assert_eq!(ladder[0].attempts, 3, "T2 gets 1 + max_retries");
        assert_eq!(ladder[1].backend.name(), "t1-foundation-models");
        assert_eq!(ladder[1].attempts, 1, "on-device T1 never retries transport");
        assert_eq!(ladder[2].backend.name(), "heuristic", "T0 floor is last");
    }

    #[test]
    fn missing_llm_rungs_degrade_to_the_floor() {
        let mut t0 = ScriptedBackend::new("heuristic", vec![]);
        let ladder = BackendSelector::default().ladder_tiers(
            None,
            None,
            &mut t0 as &mut dyn ComposerBackend,
        );
        assert_eq!(ladder.len(), 1);
        assert_eq!(ladder[0].backend.name(), "heuristic");
    }

    #[test]
    fn t2_rung_receives_the_timeout_budget() {
        struct TimeoutRecorder {
            timeout_ms: u64,
        }
        impl ComposerBackend for TimeoutRecorder {
            fn name(&self) -> &str {
                "t2-recorder"
            }
            fn set_timeout_ms(&mut self, timeout_ms: u64) {
                self.timeout_ms = timeout_ms;
            }
            fn plan(&mut self, _: &PlanRequest) -> Result<PlanResponse, BackendError> {
                Err(BackendError::Transport("recorder".into()))
            }
        }
        let mut t2 = TimeoutRecorder { timeout_ms: 0 };
        let mut t0 = ScriptedBackend::new("heuristic", vec![]);
        let ladder = BackendSelector { cloud_timeout_ms: 4_242, ..Default::default() }
            .ladder_tiers(
                Some(&mut t2 as &mut dyn ComposerBackend),
                None,
                &mut t0 as &mut dyn ComposerBackend,
            );
        assert_eq!(ladder.len(), 2);
        assert_eq!(t2.timeout_ms, 4_242, "selector hands T2 its budget");
    }

    #[test]
    fn caps_defaults_and_remote_shape_serialize() {
        let caps = Caps::on_device();
        assert!(caps.offline && !caps.json_schema);
        let remote = Caps::remote(true);
        assert!(!remote.offline && remote.json_schema);
        assert_eq!(
            serde_json::to_string(&remote.latency_class).unwrap(),
            "\"network\""
        );
    }
}

impl BackendSelector {
    /// Orders the ladder rungs. `cloud` may be `None` (no BYOK credentials —
    /// the ladder degenerates to the on-device floor). Any backend can sit in
    /// the cloud rung (tests use stubs); the selector hands it the timeout
    /// budget through [`ComposerBackend::set_timeout_ms`].
    pub fn ladder<'a>(
        &self,
        on_device: &'a mut dyn ComposerBackend,
        cloud: Option<&'a mut dyn ComposerBackend>,
    ) -> Vec<LadderRung<'a>> {
        let on_device = LadderRung { backend: on_device, attempts: 1 };
        let cloud = cloud.map(|backend| {
            backend.set_timeout_ms(self.cloud_timeout_ms);
            LadderRung { backend, attempts: 1 + self.max_retries }
        });
        match (self.prefer_on_device, cloud) {
            (true, cloud) => {
                let mut rungs = vec![on_device];
                rungs.extend(cloud);
                rungs
            }
            (false, Some(cloud)) => vec![cloud, on_device],
            (false, None) => vec![on_device],
        }
    }

    /// The full #36 chain: configured T2 → T1 on-device → T0 deterministic.
    /// Either LLM rung may be absent (no BYOK key, no Foundation Models
    /// session on this host); the T0 floor is always present and last, so
    /// degradation is invisible — a wake never stalls, never touches audio.
    /// Rungs may come from different scopes; the ladder lives only as long
    /// as the shortest borrow.
    pub fn ladder_tiers<'a, 'b, 'c>(
        &self,
        t2: Option<&'a mut dyn ComposerBackend>,
        t1: Option<&'b mut dyn ComposerBackend>,
        t0: &'c mut dyn ComposerBackend,
    ) -> Vec<LadderRung<'c>>
    where
        'a: 'c,
        'b: 'c,
    {
        let t0 = LadderRung { backend: t0, attempts: 1 };
        let t2 = t2.map(|backend| {
            backend.set_timeout_ms(self.cloud_timeout_ms);
            LadderRung { backend: &mut *backend, attempts: 1 + self.max_retries }
        });
        let t1 = t1.map(|backend| LadderRung { backend: &mut *backend, attempts: 1 });
        let mut rungs = Vec::new();
        rungs.extend(t2);
        rungs.extend(t1);
        rungs.push(t0);
        rungs
    }
}
