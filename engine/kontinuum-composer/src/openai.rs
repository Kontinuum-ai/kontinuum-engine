//! OpenAI-compatible chat wire format (#36): wraps the plan request in a
//! chat-completions envelope with a structured-output hint, so one client
//! covers OpenAI, OpenRouter, Groq, Mistral, DeepSeek, Together, Ollama and
//! LM Studio (`base_url + key + model` per #36). Deterministic by policy:
//! temperature 0, and the system message pins the response contract — the
//! #22 validated-repair loop is the equalizer for whatever slips through.
//!
//! Parsing is tolerant: an OpenAI envelope is unwrapped to its assistant
//! message; anything that does not look like an envelope is treated as a raw
//! [`PlanResponse`] body so llama.cpp-server-style endpoints keep working
//! unchanged.

use serde_json::{json, Value};

use crate::backend::PlanRequest;

const SYSTEM_CONTRACT: &str = "You plan music for a generative techno engine. \
Respond with ONLY a JSON object: {\"diffs\": [\"<ir-diff json string>\", ...], \
\"notes\": \"<short plan rationale>\"}. Each diffs entry must be one IR diff \
op serialized as a JSON string (op-tagged, e.g. {\"op\": \
\"set_instrument_param\", \"track\": \"kick\", \"param\": \"decay_ms\", \
\"value\": 220.0}). Never emit audio, never emit prose outside the JSON.";

/// Body for an OpenAI-compatible chat-completions call. `json_object` mode
/// (rather than a full json_schema) keeps one wire shape across providers
/// whose schema support differs; the system contract carries the exact shape.
pub fn build_request_body(model: &str, request: &PlanRequest) -> Result<String, String> {
    let user_payload = serde_json::to_string(request)
        .map_err(|e| format!("plan request does not serialize: {e}"))?;
    let body = json!({
        "model": model,
        "temperature": 0.0,
        "response_format": { "type": "json_object" },
        "messages": [
            { "role": "system", "content": SYSTEM_CONTRACT },
            { "role": "user", "content": user_payload },
        ],
    });
    serde_json::to_string(&body).map_err(|e| format!("envelope does not serialize: {e}"))
}

/// Body variant for providers advertising `json_schema` support
/// ([`crate::Caps::json_schema`]): the strict plan schema constrains the
/// output instead of relying on the prompt alone. Servers that reject the
/// shape fall back to [`build_request_body`] — more repair rounds, never
/// broken audio (#36 risk note).
pub fn build_schema_request_body(model: &str, request: &PlanRequest) -> Result<String, String> {
    let user_payload = serde_json::to_string(request)
        .map_err(|e| format!("plan request does not serialize: {e}"))?;
    let body = json!({
        "model": model,
        "temperature": 0.0,
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "plan_response",
                "strict": true,
                "schema": crate::schema::plan_response_schema(),
            },
        },
        "messages": [
            { "role": "system", "content": SYSTEM_CONTRACT },
            { "role": "user", "content": user_payload },
        ],
    });
    serde_json::to_string(&body).map_err(|e| format!("envelope does not serialize: {e}"))
}

/// Extracts the raw response body: the assistant message content if the body
/// is an OpenAI envelope, else the body itself (raw PlanResponse endpoints).
pub fn extract_response(raw: &str) -> String {
    let value: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return raw.to_string(),
    };
    let is_envelope = value.get("choices").and_then(|c| c.get(0)).is_some();
    if !is_envelope {
        return raw.to_string();
    }
    value["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn envelope_is_deterministic_and_contract_pinned() {
        let a = build_request_body("qwen3.5-2b", &request()).unwrap();
        let b = build_request_body("qwen3.5-2b", &request()).unwrap();
        assert_eq!(a, b, "same request → byte-identical body");
        let v: Value = serde_json::from_str(&a).unwrap();
        assert_eq!(v["temperature"], 0.0, "deterministic decoding is policy");
        assert_eq!(v["response_format"]["type"], "json_object");
        let system = v["messages"][0]["content"].as_str().unwrap();
        assert!(system.contains("diffs") && system.contains("op"), "contract pins the shape");
        let user: Value = serde_json::from_str(v["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(user["prompt"], "darker", "plan request rides the user message");
    }

    #[test]
    fn openai_envelope_is_unwrapped_to_content() {
        let envelope = r#"{"choices":[{"message":{"content":"{\"diffs\":[\"{}\"],\"notes\":\"n\"}"}}]}"#;
        let inner = extract_response(envelope);
        let v: Value = serde_json::from_str(&inner).unwrap();
        assert_eq!(v["notes"], "n");
    }

    #[test]
    fn schema_mode_embodies_the_strict_plan_schema() {
        let body = build_schema_request_body("qwen3.5-2b", &request()).unwrap();
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["response_format"]["type"], "json_schema");
        assert_eq!(v["response_format"]["json_schema"]["strict"], true);
        assert_eq!(v["response_format"]["json_schema"]["schema"]["type"], "object");
        let plain = build_request_body("qwen3.5-2b", &request()).unwrap();
        let p: Value = serde_json::from_str(&plain).unwrap();
        assert_eq!(p["response_format"]["type"], "json_object");
    }

    #[test]
    fn raw_plan_response_passes_through_unchanged() {
        let raw = r#"{"diffs": [], "notes": "raw"}"#;
        assert_eq!(extract_response(raw), raw);
        assert_eq!(extract_response("not json at all"), "not json at all");
    }
}
