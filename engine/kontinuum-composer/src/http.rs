//! HTTP escalation backend (issues #41/#36): POSTs the plan request to a
//! configurable endpoint in one of the provider wire formats — raw
//! PlanResponse JSON (llama.cpp server / Kontinuum server), OpenAI-compatible
//! chat, Anthropic Messages, or Gemini generateContent. Diff validation
//! happens in the orchestrator, never here.
//!
//! **No blocking I/O in this crate.** The transport is injected as a
//! [`TransportFn`] taking an explicit timeout; the host wires the real
//! stack — URLSession at the iOS FFI boundary, reqwest on the server. The
//! default transport reports "not wired" so an unconfigured cloud rung
//! fails fast into the ladder's on-device fallback. API keys ride the
//! request *headers* this seam carries — they never enter a request body,
//! a config file, or a log (issue #36: Keychain-only keys).

use std::time::Duration;

use crate::backend::{BackendError, ComposerBackend, Caps, PlanRequest, PlanResponse};

/// Everything a host transport needs for one call. `headers` carries auth
/// (`Authorization: Bearer …`, `x-api-key`, …) and content type; the
/// transport must NOT log them.
pub struct TransportRequest<'a> {
    pub method: &'a str,
    pub url: &'a str,
    pub headers: &'a [(String, String)],
    pub body: &'a str,
    pub timeout: Duration,
}

/// Host-injected HTTP transport. The timeout is part of the contract —
/// implementations must give up by its expiry and return
/// [`BackendError::Timeout`].
pub type TransportFn = fn(&TransportRequest<'_>) -> Result<String, BackendError>;

fn unwired_transport(_req: &TransportRequest<'_>) -> Result<String, BackendError> {
    Err(BackendError::Transport(
        "no transport wired: hosts inject URLSession (iOS FFI) or reqwest (server)".into(),
    ))
}

/// Optional BYOK cloud rung of the escalation ladder.
pub struct HttpCloudBackend {
    pub endpoint: String,
    pub timeout_ms: u64,
    transport: TransportFn,
    /// BYOK secret, resolved from the Keychain by the host per session.
    /// Empty for endpoints that need none (LAN llama.cpp, Ollama).
    pub key: String,
    /// Wire format: `Raw` posts the PlanRequest body directly
    /// (llama.cpp-server / Kontinuum server); the provider envelopes wrap it
    /// (#36: one client, many providers via `base_url + key + model`).
    pub wire: WireFormat,
    /// OpenAI wire only: send the strict json_schema response format
    /// instead of prompt-for-JSON. Anthropic/Gemini are schema-constrained
    /// by construction; `Raw` has no schema. Drives [`Caps::json_schema`].
    pub strict_schema: bool,
    /// Model id sent in the provider envelope (ignored in `Raw`).
    pub model: String,
}

/// Back-compat alias: the HTTP backend under its original name.
pub type HttpBackend = HttpCloudBackend;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WireFormat {
    #[default]
    Raw,
    OpenAiChat,
    Anthropic,
    Gemini,
}

impl WireFormat {
    fn auth_headers(self, key: &str) -> Vec<(String, String)> {
        let mut headers = vec![("Content-Type".to_string(), "application/json".to_string())];
        match self {
            WireFormat::Raw => {}
            WireFormat::OpenAiChat => {
                if !key.is_empty() {
                    headers.push(("Authorization".to_string(), format!("Bearer {key}")));
                }
            }
            WireFormat::Anthropic => {
                if !key.is_empty() {
                    headers.push(("x-api-key".to_string(), key.to_string()));
                }
                headers.push(("anthropic-version".to_string(), "2023-06-01".to_string()));
            }
            WireFormat::Gemini => {
                if !key.is_empty() {
                    headers.push(("x-goog-api-key".to_string(), key.to_string()));
                }
            }
        }
        headers
    }
}

impl HttpCloudBackend {
    pub fn new(endpoint: String, timeout_ms: u64) -> Self {
        HttpCloudBackend::with_transport(endpoint, timeout_ms, unwired_transport)
    }

    /// Builds a cloud backend around a host-supplied transport.
    pub fn with_transport(endpoint: String, timeout_ms: u64, transport: TransportFn) -> Self {
        HttpCloudBackend {
            endpoint,
            timeout_ms,
            transport,
            key: String::new(),
            wire: WireFormat::Raw,
            strict_schema: false,
            model: String::new(),
        }
    }

    /// OpenAI-compatible chat endpoint (#36): `endpoint` is the full URL
    /// (e.g. `https://api.openai.com/v1/chat/completions`), `model` the
    /// provider's model id.
    pub fn openai_chat(endpoint: String, timeout_ms: u64, model: String, transport: TransportFn) -> Self {
        HttpCloudBackend { wire: WireFormat::OpenAiChat, model, ..HttpCloudBackend::with_transport(endpoint, timeout_ms, transport) }
    }

    /// Anthropic Messages endpoint (issue #36 adapter 2), tool-use for
    /// structured output.
    pub fn anthropic(endpoint: String, timeout_ms: u64, model: String, transport: TransportFn) -> Self {
        HttpCloudBackend { wire: WireFormat::Anthropic, model, ..HttpCloudBackend::with_transport(endpoint, timeout_ms, transport) }
    }

    /// Gemini generateContent endpoint (issue #36 adapter 3), responseSchema
    /// for structured output.
    pub fn gemini(endpoint: String, timeout_ms: u64, model: String, transport: TransportFn) -> Self {
        HttpCloudBackend { wire: WireFormat::Gemini, model, ..HttpCloudBackend::with_transport(endpoint, timeout_ms, transport) }
    }

    /// Attaches the BYOK secret (resolved from the keystore by the caller).
    pub fn with_key(mut self, key: Option<&str>) -> Self {
        self.key = key.unwrap_or_default().to_string();
        self
    }

    /// OpenAI wire only: request the strict json_schema response format
    /// (providers advertising schema support, e.g. via the catalog's
    /// `tool_call` flag).
    pub fn with_strict_schema(mut self, strict: bool) -> Self {
        self.strict_schema = strict;
        self
    }

    fn build_body(&self, request: &PlanRequest) -> Result<String, BackendError> {
        let build = match self.wire {
            WireFormat::Raw => {
                return serde_json::to_string(request).map_err(|e| {
                    BackendError::BadResponse(format!("plan request does not serialize: {e}"))
                })
            }
            WireFormat::OpenAiChat if self.strict_schema => {
                crate::openai::build_schema_request_body(&self.model, request)
            }
            WireFormat::OpenAiChat => crate::openai::build_request_body(&self.model, request),
            WireFormat::Anthropic => crate::anthropic::build_request_body(&self.model, request),
            WireFormat::Gemini => crate::gemini::build_request_body(request),
        };
        build.map_err(BackendError::BadResponse)
    }

    fn extract_payload(&self, raw: &str) -> String {
        match self.wire {
            WireFormat::Raw => raw.to_string(),
            WireFormat::OpenAiChat => crate::openai::extract_response(raw),
            WireFormat::Anthropic => crate::anthropic::extract_plan(raw),
            WireFormat::Gemini => crate::gemini::extract_plan(raw),
        }
    }
}

impl ComposerBackend for HttpCloudBackend {
    fn name(&self) -> &str {
        "http-cloud"
    }

    fn capabilities(&self) -> Caps {
        let json_schema = match self.wire {
            WireFormat::Raw => false,
            WireFormat::OpenAiChat => self.strict_schema,
            WireFormat::Anthropic | WireFormat::Gemini => true,
        };
        Caps::remote(json_schema)
    }

    fn set_timeout_ms(&mut self, timeout_ms: u64) {
        self.timeout_ms = timeout_ms;
    }

    fn plan(&mut self, request: &PlanRequest) -> Result<PlanResponse, BackendError> {
        let body = self.build_body(request)?;
        let headers = self.wire.auth_headers(&self.key);
        let req = TransportRequest {
            method: "POST",
            url: &self.endpoint,
            headers: &headers,
            body: &body,
            timeout: Duration::from_millis(self.timeout_ms),
        };
        let raw = (self.transport)(&req)?;
        let payload = self.extract_payload(&raw);
        let mut response: PlanResponse = serde_json::from_str(&payload).map_err(|e| {
            BackendError::BadResponse(format!("plan response is not PlanResponse JSON: {e}"))
        })?;
        if response.backend_id.is_empty() {
            response.backend_id = self.name().to_string();
        }
        if response.latency_hint_ms == 0 {
            response.latency_hint_ms = self.timeout_ms;
        }
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK_BODY: &str = r#"{"diffs": ["{\"op\":\"set_instrument_param\",\"track\":\"kick\",\"param\":\"decay_ms\",\"value\":220.0}"], "notes": "cloud plan"}"#;

    fn ok_transport(_req: &TransportRequest<'_>) -> Result<String, BackendError> {
        Ok(OK_BODY.to_string())
    }

    fn timeout_transport(_req: &TransportRequest<'_>) -> Result<String, BackendError> {
        Err(BackendError::Timeout(8_000))
    }

    fn openai_envelope_transport(_req: &TransportRequest<'_>) -> Result<String, BackendError> {
        Ok(r#"{"choices":[{"message":{"content":"{\"diffs\":[\"{\\\"op\\\":\\\"set_section_energy\\\",\\\"id\\\":\\\"intro\\\",\\\"energy\\\":[0.5]}\"],\"notes\":\"cloud via openai wire\"}"}}]}"#.to_string())
    }

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
    fn parses_cloud_plan_and_fills_metadata() {
        let mut b = HttpCloudBackend::with_transport("http://cloud".into(), 8_000, ok_transport);
        let r = b.plan(&request()).unwrap();
        assert_eq!(r.backend_id, "http-cloud", "empty backend_id is filled in");
        assert_eq!(r.latency_hint_ms, 8_000, "zero latency hint falls back to the timeout");
        assert!(r.diffs[0].contains("set_instrument_param"));
    }

    #[test]
    fn timeout_surfaces_as_timeout_error() {
        let mut b = HttpCloudBackend::with_transport("http://cloud".into(), 8_000, timeout_transport);
        assert_eq!(b.plan(&request()), Err(BackendError::Timeout(8_000)));
    }

    #[test]
    fn default_transport_is_unwired() {
        let mut b = HttpCloudBackend::new("http://cloud".into(), 8_000);
        assert!(matches!(b.plan(&request()), Err(BackendError::Transport(_))));
    }

    #[test]
    fn openai_chat_wire_wraps_and_unwraps() {
        let mut b = HttpCloudBackend::openai_chat(
            "https://api.example.com/v1/chat/completions".into(),
            8_000,
            "qwen3.5-2b".into(),
            openai_envelope_transport,
        );
        let r = b.plan(&request()).unwrap();
        assert_eq!(r.backend_id, "http-cloud");
        assert_eq!(r.notes, "cloud via openai wire");
        assert!(r.diffs[0].contains("set_section_energy"));
    }

    #[test]
    fn openai_body_carries_model_and_contract() {
        let body = crate::openai::build_request_body("qwen3.5-2b", &request()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["model"], "qwen3.5-2b");
    }

    #[test]
    fn wire_headers_carry_keys_never_bodies() {
        // #36 hard rule: the key rides headers; the body never contains it.
        let key = "sk-test-secret-0f3a9";
        let openai = WireFormat::OpenAiChat.auth_headers(key);
        assert!(openai.contains(&("Authorization".to_string(), format!("Bearer {key}"))));

        let anthropic = WireFormat::Anthropic.auth_headers(key);
        assert!(anthropic.contains(&("x-api-key".to_string(), key.to_string())));
        assert!(anthropic.contains(&("anthropic-version".to_string(), "2023-06-01".to_string())));

        let gemini = WireFormat::Gemini.auth_headers(key);
        assert!(gemini.contains(&("x-goog-api-key".to_string(), key.to_string())));

        assert!(WireFormat::Raw.auth_headers(key).len() == 1, "raw wire carries only content type");
    }

    #[test]
    fn anthropic_and_gemini_adapters_round_trip_through_the_transport() {
        let mut a = HttpCloudBackend::anthropic(
            "https://api.anthropic.com/v1/messages".into(),
            8_000,
            "claude-sonnet-4-6".into(),
            anthropic_tool_use_transport,
        );
        let r = a.plan(&request()).unwrap();
        assert_eq!(r.notes, "via anthropic tool use");
        assert!(r.diffs[0].contains("set_instrument_param"));

        let mut g = HttpCloudBackend::gemini(
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent".into(),
            8_000,
            "gemini-2.5-flash".into(),
            gemini_response_transport,
        );
        let r = g.plan(&request()).unwrap();
        assert_eq!(r.notes, "via gemini responseSchema");
        assert!(r.diffs[0].contains("set_section_energy"));
    }

    fn anthropic_tool_use_transport(_req: &TransportRequest<'_>) -> Result<String, BackendError> {
        Ok(r#"{"id":"msg_1","content":[{"type":"tool_use","id":"tu_1","name":"emit_plan","input":{"diffs":["{\"op\":\"set_instrument_param\",\"track\":\"kick\",\"param\":\"decay_ms\",\"value\":220.0}"],"notes":"via anthropic tool use"}}],"stop_reason":"tool_use"}"#.to_string())
    }

    fn gemini_response_transport(_req: &TransportRequest<'_>) -> Result<String, BackendError> {
        Ok(r#"{"candidates":[{"content":{"parts":[{"text":"{\"diffs\":[\"{\\\"op\\\":\\\"set_section_energy\\\",\\\"id\\\":\\\"intro\\\",\\\"energy\\\":[0.5,0.6]}\"],\"notes\":\"via gemini responseSchema\"}"}]}}]}"#.to_string())
    }
}
