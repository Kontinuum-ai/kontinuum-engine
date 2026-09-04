//! Deterministic backends for the #36 provider seam (issues #22/#42).
//!
//! [`ScriptedBackend`] replays a fixed script of [`PlanResponse`] batches —
//! the stand-in for any real LLM in tests and the eval harness. Zero RNG,
//! zero network, zero time dependence: the same script against the same
//! session reproduces the wake bit-for-bit.
//!
//! [`BackendConfig`] is the JSON shape hosts use to select a provider
//! without recompiling. Where the real providers land (issue #36):
//! - **T1 on-device** (`provider: "foundation_models"` / `"gbnf"`): Apple
//!   Foundation Models guided generation, llama.cpp/GBNF fallback — the
//!   model session is owned by the Swift/FFI host, so that host implements
//!   [`ComposerBackend`] over the bridge and hands the ladder its own rung.
//!   `build_backend` reports [`ConfigError::HostProvided`] for these tiers
//!   rather than pretending to construct one.
//! - **T2 cloud BYOK** (`provider: "openai_compat"`): any OpenAI-compatible
//!   endpoint (OpenAI, OpenRouter, Groq, Ollama, LM Studio, …) behind the
//!   host-injected [`TransportFn`] — no network stack lives in this crate.
//! - **T0 deterministic** (`provider: "heuristic"`): the infallible ladder
//!   floor ([`crate::OnDeviceHeuristicBackend`]).
//!
//! Every backend's output crosses the same kontinuum-ir validate/apply gate
//! (PLAN §2.4: the model is a quality knob, never a trust boundary).

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::backend::{BackendError, ComposerBackend, PlanRequest, PlanResponse};
use crate::http::{HttpCloudBackend, TransportFn};

/// Deterministic scripted backend: replays diff batches in order; the last
/// batch repeats when the script is exhausted (a wake loop that repairs
/// once still sees a stable "model").
pub struct ScriptedBackend {
    name: String,
    script: VecDeque<PlanResponse>,
    calls: u32,
    last_request: Option<PlanRequest>,
}

impl ScriptedBackend {
    /// Scripts one response per entry; each entry is a batch of raw diff
    /// JSON strings for one `plan` call.
    pub fn new(name: impl Into<String>, batches: Vec<Vec<String>>) -> Self {
        let script = batches
            .into_iter()
            .map(|diffs| PlanResponse {
                diffs,
                notes: "scripted".into(),
                backend_id: String::new(),
                latency_hint_ms: 0,
            })
            .collect();
        ScriptedBackend { name: name.into(), script, calls: 0, last_request: None }
    }

    /// Scripts fully-formed responses (custom notes / latency hints).
    pub fn from_responses(responses: Vec<PlanResponse>) -> Self {
        ScriptedBackend {
            name: "scripted".into(),
            script: responses.into(),
            calls: 0,
            last_request: None,
        }
    }

    /// Number of `plan` calls served (tests assert against this to pin the
    /// repair-loop attempt counts).
    pub fn calls(&self) -> u32 {
        self.calls
    }

    /// The most recent request, repair context included.
    pub fn last_request(&self) -> Option<&PlanRequest> {
        self.last_request.as_ref()
    }
}

impl ComposerBackend for ScriptedBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn plan(&mut self, request: &PlanRequest) -> Result<PlanResponse, BackendError> {
        self.calls += 1;
        self.last_request = Some(request.clone());
        let idx = (self.calls as usize - 1).min(self.script.len().saturating_sub(1));
        let mut response = match self.script.get(idx) {
            Some(r) => r.clone(),
            None => PlanResponse::default(),
        };
        if response.backend_id.is_empty() {
            response.backend_id = self.name.clone();
        }
        Ok(response)
    }
}

/// Which ladder tier a configured backend serves (issue #22 telemetry splits
/// invalid-rate per tier; issue #3 names the tiers).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Deterministic shadow / heuristic floor — no model, never invalid.
    #[default]
    T0Deterministic,
    /// On-device LLM (Foundation Models / GBNF) — the default planner.
    T1OnDevice,
    /// Remote escalation (LAN or cloud BYOK), timeout-bounded.
    T2Cloud,
}

impl Tier {
    /// Tier for a backend id, for telemetry attribution.
    pub fn of_backend(name: &str) -> Tier {
        match name {
            "heuristic" | "scripted-chip" => Tier::T0Deterministic,
            n if n.contains("cloud") || n.contains("t2") => Tier::T2Cloud,
            n if n.contains("openai") || n.contains("anthropic") || n.contains("gemini") => {
                Tier::T2Cloud
            }
            _ => Tier::T1OnDevice,
        }
    }
}

/// JSON-configured backend selection (issue #36 "settings → composer"
/// shape). Hosts persist this; `build_backend` turns it into a rung.
///
/// The config deliberately carries **no API key**: keys live in the
/// Keychain ([`crate::secrets`]) and are handed to [`BackendConfig::build`]
/// per session, so serializing settings can never leak one (grep-audited in
/// `tests/keychain_audit.rs`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BackendConfig {
    /// Per-role assignment: "quick_moves" (frequent 8-bar diffs) or
    /// "deep_planning" (arrangement rewrites). Default backend for both:
    /// on-device.
    pub role: String,
    /// Provider kind: "heuristic" | "openai_compat" | "anthropic" |
    /// "gemini" | "foundation_models" | "gbnf" | "scripted".
    pub provider: String,
    /// Full endpoint URL. Empty picks the provider default
    /// (anthropic/gemini); openai_compat requires it (arbitrary hosts).
    #[serde(default)]
    pub endpoint: String,
    /// Provider model id (openai_compat/anthropic/gemini).
    #[serde(default)]
    pub model: String,
    /// OpenAI wire only: provider advertises json_schema response format
    /// (picker drives this from the catalog's tool_call flag).
    #[serde(default)]
    pub json_schema: bool,
    /// Hard timeout handed to remote providers (issue #22: T2 calls time out
    /// and degrade invisibly to T1/T0).
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

fn default_timeout_ms() -> u64 {
    crate::ESCALATION_TIMEOUT_MS
}

/// Why a [`BackendConfig`] could not become a backend.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("unknown provider `{0}` (use heuristic | openai_compat | anthropic | gemini | foundation_models | gbnf | scripted)")]
    UnknownProvider(String),
    /// T1 providers are constructed by the app host over the bridge (the
    /// model session lives on the Swift side); this crate never owns one.
    #[error("provider `{0}` is host-provided: the Swift/FFI layer implements ComposerBackend over the model session")]
    HostProvided(String),
    #[error("openai_compat requires a non-empty endpoint and model")]
    MissingEndpoint,
}

impl BackendConfig {
    /// The default, zero-configuration shape: on-device heuristic for a
    /// role (airplane mode keeps working, PLAN §2.4).
    pub fn on_device_default(role: &str) -> Self {
        BackendConfig {
            role: role.into(),
            provider: "heuristic".into(),
            endpoint: String::new(),
            model: String::new(),
            json_schema: false,
            timeout_ms: default_timeout_ms(),
        }
    }

    /// Builds the backend this config names. `key` is the BYOK secret
    /// resolved from the keystore by the caller (`None` for keyless LAN
    /// endpoints); `script` is the scripted provider's batch list (used by
    /// tests and the eval harness; a real deployment never ships one).
    ///
    /// `foundation_models` / `gbnf` return [`ConfigError::HostProvided`]:
    /// see the module docs for where T1 lands.
    pub fn build(
        &self,
        transport: TransportFn,
        key: Option<&str>,
        script: Vec<Vec<String>>,
    ) -> Result<Box<dyn ComposerBackend>, ConfigError> {
        match self.provider.as_str() {
            "heuristic" => Ok(Box::new(crate::OnDeviceHeuristicBackend)),
            "openai_compat" => {
                if self.endpoint.is_empty() || self.model.is_empty() {
                    return Err(ConfigError::MissingEndpoint);
                }
                Ok(Box::new(HttpCloudBackend::openai_chat(
                    self.endpoint.clone(),
                    self.timeout_ms,
                    self.model.clone(),
                    transport,
                )
                .with_key(key)
                .with_strict_schema(self.json_schema)))
            }
            "anthropic" => Ok(Box::new(HttpCloudBackend::anthropic(
                self.defaulted_endpoint(crate::anthropic::endpoint()),
                self.timeout_ms,
                self.model.clone(),
                transport,
            )
            .with_key(key))),
            "gemini" => Ok(Box::new(HttpCloudBackend::gemini(
                self.defaulted_endpoint(crate::gemini::endpoint(&self.model)),
                self.timeout_ms,
                self.model.clone(),
                transport,
            )
            .with_key(key))),
            "scripted" => Ok(Box::new(ScriptedBackend::new("scripted", script))),
            other => Err(ConfigError::HostProvided(other.to_string())),
        }
    }

    fn defaulted_endpoint(&self, default: String) -> String {
        if self.endpoint.is_empty() { default } else { self.endpoint.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD_DIFF: &str =
        r#"{"op":"set_instrument_param","track":"kick","param":"decay_ms","value":220.0}"#;

    fn request() -> PlanRequest {
        PlanRequest {
            style: "techno".into(),
            prompt: "darker".into(),
            bars_left_in_section: 4,
            progression: vec![],
            taste_json: "{}".into(),
            style_card: String::new(),
            context: crate::PlanContext::default(),
            repair_context: String::new(),
        }
    }

    #[test]
    fn scripted_backend_replays_then_repeats_the_last_batch() {
        let mut b = ScriptedBackend::new(
            "eval",
            vec![vec!["{ \"first\": true }".into()], vec![GOOD_DIFF.into()]],
        );
        let r1 = b.plan(&request()).unwrap();
        assert_eq!(r1.diffs, vec!["{ \"first\": true }".to_string()]);
        assert_eq!(r1.backend_id, "eval", "backend id filled from the backend name");
        let r2 = b.plan(&request()).unwrap();
        assert_eq!(r2.diffs, vec![GOOD_DIFF.to_string()]);
        let r3 = b.plan(&request()).unwrap();
        assert_eq!(r3.diffs, vec![GOOD_DIFF.to_string()], "last batch repeats");
        assert_eq!(b.calls(), 3);
    }

    #[test]
    fn scripted_backend_records_repair_context() {
        let mut b = ScriptedBackend::new("eval", vec![vec![GOOD_DIFF.into()]]);
        let mut req = request();
        req.repair_context = "E_KICK_DECAY_RANGE: value 99999 out of range".into();
        b.plan(&req).unwrap();
        assert_eq!(
            b.last_request().unwrap().repair_context,
            "E_KICK_DECAY_RANGE: value 99999 out of range"
        );
    }

    #[test]
    fn scripted_backend_is_deterministic_across_instances() {
        let run = || {
            let mut b = ScriptedBackend::new("eval", vec![vec![GOOD_DIFF.into()]]);
            let r = b.plan(&request()).unwrap();
            serde_json::to_string(&r).unwrap()
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn empty_script_serves_empty_plans() {
        let mut b = ScriptedBackend::new("eval", vec![]);
        let r = b.plan(&request()).unwrap();
        assert!(r.diffs.is_empty());
    }

    #[test]
    fn tier_attribution_covers_the_ladder_names() {
        assert_eq!(Tier::of_backend("heuristic"), Tier::T0Deterministic);
        assert_eq!(Tier::of_backend("t1-foundation-models"), Tier::T1OnDevice);
        assert_eq!(Tier::of_backend("http-cloud"), Tier::T2Cloud);
    }

    #[test]
    fn default_config_builds_the_heuristic_floor() {
        let cfg = BackendConfig::on_device_default("quick_moves");
        assert_eq!(cfg.role, "quick_moves");
        let built = cfg.build(|_| unreachable!("heuristic never dials out"), None, vec![]);
        assert_eq!(built.unwrap().name(), "heuristic");
    }

    #[test]
    fn openai_compat_config_builds_a_cloud_rung() {
        let cfg = BackendConfig {
            role: "deep_planning".into(),
            provider: "openai_compat".into(),
            endpoint: "https://api.example.com/v1/chat/completions".into(),
            model: "qwen3.5-2b".into(),
            json_schema: false,
            timeout_ms: 10_000,
        };
        let built = cfg
            .build(
                |_| Ok(r#"{"diffs": [], "notes": "ok"}"#.to_string()),
                None,
                vec![],
            )
            .unwrap();
        assert_eq!(built.name(), "http-cloud");
    }

    #[test]
    fn anthropic_and_gemini_configs_default_their_endpoints_and_take_keys() {
        let key = "sk-ant-test-9c1f";
        let mut cfg = BackendConfig::on_device_default("deep_planning");
        cfg.provider = "anthropic".into();
        cfg.model = "claude-sonnet-4-6".into();
        let transport: TransportFn = |req: &crate::http::TransportRequest<'_>| {
            let auth = req.headers.iter().find(|(k, _)| k == "x-api-key");
            assert_eq!(auth.map(|(_, v)| v.as_str()), Some("sk-ant-test-9c1f"), "key rides the header");
            assert!(!req.body.contains("sk-ant-test-9c1f"), "key never enters a request body");
            Ok(r#"{"content":[{"type":"tool_use","name":"emit_plan","input":{"diffs":[],"notes":"ok"}}]}"#.to_string())
        };
        let mut backend = cfg.build(transport, Some(key), vec![]).unwrap();
        assert!(backend.plan(&request()).is_ok());

        let mut cfg = BackendConfig::on_device_default("quick_moves");
        cfg.provider = "gemini".into();
        cfg.model = "gemini-2.5-flash".into();
        let backend = cfg.build(|_| unreachable!(), None, vec![]).unwrap();
        let _ = backend;
    }

    #[test]
    fn openai_compat_without_endpoint_fails_fast() {
        let cfg = BackendConfig {
            role: "deep_planning".into(),
            provider: "openai_compat".into(),
            endpoint: String::new(),
            model: String::new(),
            json_schema: false,
            timeout_ms: 10_000,
        };
        assert!(matches!(
            cfg.build(|_| unreachable!(), None, vec![]),
            Err(ConfigError::MissingEndpoint)
        ));
    }

    #[test]
    fn on_device_llm_providers_are_host_provided() {
        // T1 landing point: the Swift/FFI host owns the Foundation Models /
        // GBNF session and implements ComposerBackend over the bridge.
        for provider in ["foundation_models", "gbnf"] {
            let cfg = BackendConfig {
                role: "quick_moves".into(),
                provider: provider.into(),
                endpoint: String::new(),
                model: String::new(),
                json_schema: false,
                timeout_ms: 10_000,
            };
            assert!(
                matches!(cfg.build(|_| unreachable!(), None, vec![]), Err(ConfigError::HostProvided(_))),
                "{provider} must not pretend to construct here"
            );
        }
    }

    #[test]
    fn config_serializes_roundtrip() {
        let cfg = BackendConfig::on_device_default("quick_moves");
        let json = serde_json::to_string(&cfg).unwrap();
        let back: BackendConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cfg);
    }
}
