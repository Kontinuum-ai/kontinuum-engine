//! The models.dev model catalog (issue #36): the same `api.json` source
//! opencode uses, fetched through the injected transport and cached
//! verbatim for offline use — the provider/model picker stays current
//! without an app update.
//!
//! **Licensing gate (#6):** models.dev publishes `api.json` as open data
//! (MIT-licensed repo); the catalog is factual provider/model metadata
//! (ids, context windows, pricing), and this crate ships no models.dev
//! client crate — one GET through the host transport. Re-check both points
//! with #6 before the catalog ships in a store build.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::backend::BackendError;
use crate::http::TransportRequest;

pub const API_JSON_URL: &str = "https://models.dev/api.json";

/// Why the catalog could not be (re)loaded.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CatalogError {
    #[error("catalog fetch timed out after {0}ms")]
    Timeout(u64),
    #[error("catalog fetch failed: {0}")]
    Transport(String),
    #[error("catalog is not valid JSON: {0}")]
    Parse(String),
    #[error("no catalog available and the fetch failed (offline, empty cache)")]
    NoData,
}

/// The catalog: raw `api.json` plus a lazy typed view. `raw` doubles as the
/// offline cache — keep it around (hosts persist it) and [`ModelCatalog::refresh`]
/// only replaces it when a fetch *and* parse succeed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelCatalog {
    raw: String,
}

impl ModelCatalog {
    pub fn from_raw(raw: impl Into<String>) -> Result<Self, CatalogError> {
        let raw = raw.into();
        Self::validate(&raw)?;
        Ok(ModelCatalog { raw })
    }

    /// Cached raw catalog (the offline path: hosts hand this back after a
    /// restart when the fetch fails).
    pub fn raw(&self) -> &str {
        &self.raw
    }

    fn validate(raw: &str) -> Result<(), CatalogError> {
        let v: Value =
            serde_json::from_str(raw).map_err(|e| CatalogError::Parse(e.to_string()))?;
        if v.as_object().is_none() {
            return Err(CatalogError::Parse("top level is not an object".into()));
        }
        Ok(())
    }

    /// Fetches a fresh `api.json` through the injected transport. On
    /// success the cache is replaced; any failure leaves the existing cache
    /// untouched and reports the typed error.
    pub fn refresh(&mut self, transport: crate::http::TransportFn, timeout_ms: u64) -> Result<(), CatalogError> {
        let req = TransportRequest {
            method: "GET",
            url: API_JSON_URL,
            headers: &[],
            body: "",
            timeout: std::time::Duration::from_millis(timeout_ms),
        };
        let raw = (transport)(&req).map_err(|e| match e {
            BackendError::Timeout(ms) => CatalogError::Timeout(ms),
            BackendError::Transport(m) => CatalogError::Transport(m),
            BackendError::BadResponse(m) => CatalogError::Transport(m),
        })?;
        Self::validate(&raw)?;
        self.raw = raw;
        Ok(())
    }

    fn document(&self) -> Result<Value, CatalogError> {
        let v: Value =
            serde_json::from_str(&self.raw).map_err(|e| CatalogError::Parse(e.to_string()))?;
        Ok(v)
    }

    /// Provider ids and display names, ordered by id for a stable picker.
    pub fn providers(&self) -> Result<Vec<ProviderSummary>, CatalogError> {
        let doc = self.document()?;
        let mut out: Vec<ProviderSummary> = doc
            .as_object()
            .map(|m| {
                m.iter()
                    .filter_map(|(id, p)| {
                        Some(ProviderSummary {
                            id: id.clone(),
                            name: p["name"].as_str()?.to_string(),
                            api: p["api"].as_str().map(str::to_string),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    /// One provider's models with picker-relevant fields: context window,
    /// per-million-token pricing, tool-call support (the structured-output
    /// proxy for json_schema capability).
    pub fn models(&self, provider_id: &str) -> Result<Vec<ModelInfo>, CatalogError> {
        let doc = self.document()?;
        let entry = &doc[provider_id];
        let mut out: Vec<ModelInfo> = entry["models"]
            .as_object()
            .map(|m| {
                m.iter()
                    .filter_map(|(id, model)| {
                        Some(ModelInfo {
                            id: id.clone(),
                            name: model["name"].as_str().unwrap_or(id).to_string(),
                            context_window: model["limit"]["context"].as_u64()?,
                            input_cost_per_mtok: model["cost"]["input"].as_f64(),
                            output_cost_per_mtok: model["cost"]["output"].as_f64(),
                            tool_call: model["tool_call"].as_bool().unwrap_or(false),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderSummary {
    pub id: String,
    pub name: String,
    /// API base URL (e.g. `https://api.openai.com/v1`); None = local/custom.
    pub api: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub context_window: u64,
    /// USD per million input tokens (models.dev `cost.input`).
    pub input_cost_per_mtok: Option<f64>,
    /// USD per million output tokens (models.dev `cost.output`).
    pub output_cost_per_mtok: Option<f64>,
    /// Provider supports tool calls — the capability proxy for
    /// schema-constrained output on this wire.
    pub tool_call: bool,
}

impl ModelInfo {
    /// USD cost for one hour of session planning at the given token rates
    /// (feeds the telemetry `$ / session-hour` column; `None` = free/unknown
    /// pricing, e.g. local Ollama).
    pub fn cost_per_session_hour(&self, input_tokens_per_hour: u64, output_tokens_per_hour: u64) -> Option<f64> {
        let input = self.input_cost_per_mtok? * (input_tokens_per_hour as f64 / 1_000_000.0);
        let output = self.output_cost_per_mtok? * (output_tokens_per_hour as f64 / 1_000_000.0);
        Some(input + output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "openai": {
            "id": "openai", "name": "OpenAI", "api": "https://api.openai.com/v1",
            "models": {
                "gpt-5.2": {
                    "id": "gpt-5.2", "name": "GPT-5.2", "tool_call": true,
                    "limit": { "context": 400000, "output": 128000 },
                    "cost": { "input": 1.25, "output": 10.0 }
                },
                "gpt-5.2-mini": {
                    "id": "gpt-5.2-mini", "name": "GPT-5.2 mini", "tool_call": true,
                    "limit": { "context": 400000, "output": 128000 },
                    "cost": { "input": 0.25, "output": 2.0 }
                }
            }
        },
        "ollama": {
            "id": "ollama", "name": "Ollama", "api": null,
            "models": {
                "qwen3.5-2b": {
                    "id": "qwen3.5-2b", "name": "Qwen 3.5 2B", "tool_call": true,
                    "limit": { "context": 32768 },
                    "cost": {}
                }
            }
        }
    }"#;

    fn catalog() -> ModelCatalog {
        ModelCatalog::from_raw(SAMPLE).unwrap()
    }

    #[test]
    fn providers_are_listed_stably_with_api_urls() {
        let providers = catalog().providers().unwrap();
        let ids: Vec<&str> = providers.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["ollama", "openai"]);
        let openai = providers.iter().find(|p| p.id == "openai").unwrap();
        assert_eq!(openai.name, "OpenAI");
        assert_eq!(openai.api.as_deref(), Some("https://api.openai.com/v1"));
    }

    #[test]
    fn models_carry_context_pricing_and_capability() {
        let models = catalog().models("openai").unwrap();
        let mini = models.iter().find(|m| m.id == "gpt-5.2-mini").unwrap();
        assert_eq!(mini.context_window, 400_000);
        assert_eq!(mini.input_cost_per_mtok, Some(0.25));
        assert!(mini.tool_call);
        let local = catalog().models("ollama").unwrap();
        let qwen = local.first().unwrap();
        assert_eq!(qwen.input_cost_per_mtok, None, "local models price at nothing");
        assert_eq!(qwen.cost_per_session_hour(1_000_000, 1_000_000), None);
    }

    #[test]
    fn session_hour_pricing_sums_input_and_output() {
        let gpt = catalog()
            .models("openai")
            .unwrap()
            .into_iter()
            .find(|m| m.id == "gpt-5.2")
            .unwrap();
        let cost = gpt.cost_per_session_hour(2_000_000, 500_000).unwrap();
        assert!((cost - (2.5 + 5.0)).abs() < 1e-9);
    }

    #[test]
    fn refresh_replaces_cache_only_on_success() {
        let mut catalog = catalog();
        let ok: crate::http::TransportFn = |_| {
            Ok(r#"{"zai":{"id":"zai","name":"Z.ai","models":{}}}"#.to_string())
        };
        catalog.refresh(ok, 1_000).unwrap();
        assert_eq!(catalog.providers().unwrap().len(), 1);

        let failing: crate::http::TransportFn =
            |_| Err(BackendError::Timeout(5_000));
        assert_eq!(
            catalog.refresh(failing, 5_000),
            Err(CatalogError::Timeout(5_000))
        );
        assert_eq!(
            catalog.providers().unwrap()[0].id,
            "zai",
            "failed fetch leaves the cache intact"
        );

        let garbage: crate::http::TransportFn = |_| Ok("not json".to_string());
        assert!(matches!(
            catalog.refresh(garbage, 1_000),
            Err(CatalogError::Parse(_))
        ));

        let mut empty = ModelCatalog::default();
        assert_eq!(empty.refresh(failing, 5_000), Err(CatalogError::Timeout(5_000)));
        assert!(matches!(empty.providers(), Err(CatalogError::Parse(_))));
    }

    #[test]
    fn from_raw_rejects_garbage() {
        assert!(matches!(
            ModelCatalog::from_raw("nope"),
            Err(CatalogError::Parse(_))
        ));
        assert!(matches!(
            ModelCatalog::from_raw("[1,2]"),
            Err(CatalogError::Parse(_))
        ));
    }
}
