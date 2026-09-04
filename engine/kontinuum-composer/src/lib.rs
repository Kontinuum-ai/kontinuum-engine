//! `kontinuum-composer` — the composer's planning layer, backend-agnostic
//! (issues #41/#42/#22). PLAN §2.4 rules encoded here:
//! - on-device heuristic backend always available (airplane mode keeps working)
//! - cloud/LLM escalation is optional BYOK, with a timeout and retries,
//!   and its output lands as **validated IR diffs** — never audio, never
//!   unvalidated text
//! - the escalation ladder ([`BackendSelector`]) ends at the heuristic floor,
//!   so no backend failure can stall a session
//! - the wake loop ([`cadence`]/[`wake`], issue #22) plans every 16–64 bars
//!   and on steering events, and only diffs that survive the kontinuum-ir
//!   apply/validate gate reach the compose engine

pub mod anthropic;
pub mod backend;
pub mod cadence;
pub mod catalog;
pub mod context;
pub mod gemini;
pub mod heuristic;
pub mod http;
pub mod openai;
pub mod orchestrator;
pub mod roles;
pub mod schema;
pub mod scripted;
pub mod secrets;
pub mod steering;
pub mod telemetry;
pub mod wake;

#[cfg(test)]
mod stub;

pub use anthropic as anthropic_wire;
pub use backend::{
    BackendError, BackendSelector, Caps, ComposerBackend, CostClass, LadderRung, LatencyClass,
    PlanContext, PlanRequest, PlanResponse, SectionSummary,
};
pub use catalog::{CatalogError, ModelCatalog, ModelInfo, ProviderSummary, API_JSON_URL};
pub use cadence::{ComposerScheduler, WakeConfig, WakePolicy, WakeReason, MAX_WAKE_BARS, MIN_WAKE_BARS, MIN_LOOKAHEAD_MARGIN_BARS};
pub use context::{
    estimate_tokens, within_budget, ComposerContext, ContextInputs, PatternDigest, SectionPosition,
    CONTEXT_FORMAT_VERSION, TOKEN_BUDGET,
};
pub use gemini as gemini_wire;
pub use heuristic::{HeuristicBackend, OnDeviceHeuristicBackend};
pub use http::{HttpBackend, HttpCloudBackend, TransportFn, TransportRequest, WireFormat};
pub use orchestrator::{run_composer_pass, ComposerReport};
pub use roles::{ComposerRole, RoleConfig};
pub use scripted::{BackendConfig, ConfigError, ScriptedBackend, Tier};
pub use secrets::{ComposerSecrets, MemorySecretStore, SecretStore};
pub use steering::{
    apply_plan, classify, plan_ops, run_steering, ComposerTelemetry, DirectIntent, OpClass,
    PlannedOp, QuickChip, ScriptedSteeringProvider, SteeringDirective, SteeringError,
    SteeringPlan, SteeringProvider, SteeringSource, SteeringVector, TierStats, COMPOSITION_MAX_BARS,
    STEERING_REPAIR_ROUNDS, T0_MAX_BARS,
};
pub use telemetry::{BackendTelemetry, BackendTelemetryRow, ComposerBackendTelemetry};
pub use wake::{build_plan_request, run_wake, Steering, MAX_REPAIR_ROUNDS};

/// Timeout for escalation backends, in milliseconds.
pub const ESCALATION_TIMEOUT_MS: u64 = 8_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_request_serializes() {
        let r = PlanRequest {
            style: "deep house".into(),
            prompt: "darker, more hypnotic".into(),
            bars_left_in_section: 6,
            progression: vec![(29, true), (37, false)],
            taste_json: "{}".into(),
            style_card: String::new(),
            context: PlanContext::default(),
            repair_context: String::new(),
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: PlanRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.style, "deep house");
    }

    #[test]
    fn plan_request_deserializes_without_new_fields() {
        // Hosts from before PlanContext/repair_context existed still parse.
        let legacy = r#"{"style":"techno","prompt":"darker","bars_left_in_section":4,
            "progression":[[29,true]],"taste_json":"{}"}"#;
        let back: PlanRequest = serde_json::from_str(legacy).unwrap();
        assert_eq!(back.context, PlanContext::default());
        assert!(back.repair_context.is_empty());
    }

    #[test]
    fn selector_puts_the_floor_last_either_way() {
        let mut on_device = OnDeviceHeuristicBackend;
        let mut cloud = HttpCloudBackend::new("http://cloud".into(), 1_000);
        let prefer = BackendSelector::default();
        let ladder = prefer.ladder(&mut on_device, Some(&mut cloud as &mut dyn ComposerBackend));
        assert_eq!(ladder.len(), 2);
        assert_eq!(ladder[0].backend.name(), "heuristic", "on-device first when preferred");
        assert_eq!(ladder[1].backend.name(), "http-cloud");

        let escalate = BackendSelector { prefer_on_device: false, ..Default::default() };
        let mut on_device = OnDeviceHeuristicBackend;
        let mut cloud = HttpCloudBackend::new("http://cloud".into(), 1_000);
        let ladder = escalate.ladder(&mut on_device, Some(&mut cloud as &mut dyn ComposerBackend));
        assert_eq!(ladder[0].backend.name(), "http-cloud", "cloud escalates first");
        assert_eq!(ladder.last().unwrap().backend.name(), "heuristic", "floor is always last");
        assert_eq!(ladder[0].attempts, 2, "attempts = 1 + max_retries");
    }

    #[test]
    fn selector_without_cloud_is_just_the_floor() {
        let mut on_device = OnDeviceHeuristicBackend;
        let ladder = BackendSelector::default().ladder(&mut on_device, None);
        assert_eq!(ladder.len(), 1);
        assert_eq!(ladder[0].backend.name(), "heuristic");
    }
}
