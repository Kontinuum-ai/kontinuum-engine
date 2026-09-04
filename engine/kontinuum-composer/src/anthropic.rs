//! Anthropic Messages API wire format (#36 adapter 2). Structured output
//! rides tool use: the plan request is the user message, and a single
//! `emit_plan` tool — whose `input_schema` is the shared plan schema — is
//! force-selected via `tool_choice`, so the model's answer *is* the tool
//! input. Parsing pulls the `tool_use` content block and re-serializes its
//! input as the PlanResponse JSON the caller expects. Deterministic by
//! policy: temperature 0.

use serde_json::{json, Value};

use crate::backend::PlanRequest;

const SYSTEM_CONTRACT: &str = "You plan music for a generative techno engine. \
Call the emit_plan tool exactly once. Each diffs entry must be one IR diff \
op serialized as a JSON string (op-tagged, e.g. {\"op\": \
\"set_instrument_param\", \"track\": \"kick\", \"param\": \"decay_ms\", \
\"value\": 220.0}). Never emit audio, never emit prose outside the tool call.";

/// Default Messages API endpoint for a model.
pub fn endpoint() -> String {
    "https://api.anthropic.com/v1/messages".into()
}

/// Body for an Anthropic Messages call: system contract, the serialized
/// plan request as the single user message, and a force-selected
/// `emit_plan` tool carrying the plan schema.
pub fn build_request_body(model: &str, request: &PlanRequest) -> Result<String, String> {
    let user_payload = serde_json::to_string(request)
        .map_err(|e| format!("plan request does not serialize: {e}"))?;
    let body = json!({
        "model": model,
        "max_tokens": 4_096,
        "temperature": 0.0,
        "system": SYSTEM_CONTRACT,
        "messages": [
            { "role": "user", "content": user_payload }
        ],
        "tools": [{
            "name": "emit_plan",
            "description": "Emit one batch of IR diff ops for the composer wake",
            "input_schema": crate::schema::plan_response_schema(),
        }],
        "tool_choice": { "type": "tool", "name": "emit_plan" },
    });
    serde_json::to_string(&body).map_err(|e| format!("envelope does not serialize: {e}"))
}

/// Extracts the PlanResponse JSON from a Messages response: the `input` of
/// the first `tool_use` block, re-serialized. Missing blocks yield `""`
/// (the caller's parse then surfaces a `BadResponse`).
pub fn extract_plan(raw: &str) -> String {
    let value: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    let block = value["content"]
        .as_array()
        .and_then(|blocks| {
            blocks.iter().find(|b| b["type"] == "tool_use" && b["name"] == "emit_plan")
        });
    match block {
        Some(b) => serde_json::to_string(&b["input"]).unwrap_or_default(),
        None => String::new(),
    }
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
    fn body_force_selects_the_emit_plan_tool() {
        let a = build_request_body("claude-sonnet-4-6", &request()).unwrap();
        let b = build_request_body("claude-sonnet-4-6", &request()).unwrap();
        assert_eq!(a, b, "same request → byte-identical body");
        let v: Value = serde_json::from_str(&a).unwrap();
        assert_eq!(v["model"], "claude-sonnet-4-6");
        assert_eq!(v["temperature"], 0.0);
        assert_eq!(v["tool_choice"], json!({"type": "tool", "name": "emit_plan"}));
        assert_eq!(v["tools"][0]["name"], "emit_plan");
        assert_eq!(v["tools"][0]["input_schema"]["type"], "object");
        let user = v["messages"][0]["content"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(user).unwrap();
        assert_eq!(parsed["prompt"], "darker", "plan request rides the user message");
    }

    #[test]
    fn tool_use_block_is_extracted_to_plan_json() {
        let raw = r#"{"content":[
            {"type":"text","text":"thinking…"},
            {"type":"tool_use","name":"emit_plan","input":{"diffs":["{\"op\":1}"],"notes":"n"}}
        ]}"#;
        let plan = extract_plan(raw);
        let v: Value = serde_json::from_str(&plan).unwrap();
        assert_eq!(v["notes"], "n");
        assert_eq!(v["diffs"][0], "{\"op\":1}");
    }

    #[test]
    fn missing_tool_block_is_an_empty_payload() {
        assert_eq!(extract_plan(r#"{"content":[{"type":"text","text":"no"}]}"#), "");
        assert_eq!(extract_plan("not json"), "");
    }
}
