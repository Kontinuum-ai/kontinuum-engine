//! Gemini generateContent wire format (#36 adapter 3). Structured output
//! rides `generationConfig.responseMimeType: "application/json"` plus
//! `responseSchema` (the shared plan schema); the plan request is the single
//! user part, the contract the system instruction. Parsing joins the text
//! parts of the first candidate. Deterministic by policy: temperature 0.

use serde_json::{json, Value};

use crate::backend::PlanRequest;

const SYSTEM_CONTRACT: &str = "You plan music for a generative techno engine. \
Respond with ONLY the JSON object the schema describes. Each diffs entry must \
be one IR diff op serialized as a JSON string (op-tagged, e.g. {\"op\": \
\"set_instrument_param\", \"track\": \"kick\", \"param\": \"decay_ms\", \
\"value\": 220.0}). Never emit audio, never emit prose outside the JSON.";

/// Default generateContent endpoint for a model.
pub fn endpoint(model: &str) -> String {
    format!("https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent")
}

/// Body for a Gemini generateContent call with a schema-constrained JSON
/// response. (The model id lives in the endpoint URL, not this body.)
pub fn build_request_body(request: &PlanRequest) -> Result<String, String> {
    let user_payload = serde_json::to_string(request)
        .map_err(|e| format!("plan request does not serialize: {e}"))?;
    let body = json!({
        "systemInstruction": { "parts": [{ "text": SYSTEM_CONTRACT }] },
        "contents": [
            { "role": "user", "parts": [{ "text": user_payload }] }
        ],
        "generationConfig": {
            "temperature": 0.0,
            "responseMimeType": "application/json",
            "responseSchema": crate::schema::plan_response_schema(),
        },
    });
    serde_json::to_string(&body).map_err(|e| format!("envelope does not serialize: {e}"))
}

/// Extracts the PlanResponse JSON from a generateContent response: the text
/// parts of the first candidate, joined. Missing candidates yield `""`.
pub fn extract_plan(raw: &str) -> String {
    let value: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    let texts = value["candidates"][0]["content"]["parts"].as_array().map(|parts| {
        parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("")
    });
    texts.unwrap_or_default().trim().to_string()
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
    fn body_pins_response_schema_and_determinism() {
        let a = build_request_body(&request()).unwrap();
        let b = build_request_body(&request()).unwrap();
        assert_eq!(a, b, "same request → byte-identical body");
        let v: Value = serde_json::from_str(&a).unwrap();
        assert_eq!(v["generationConfig"]["temperature"], 0.0);
        assert_eq!(v["generationConfig"]["responseMimeType"], "application/json");
        assert_eq!(v["generationConfig"]["responseSchema"]["type"], "object");
        let user = v["contents"][0]["parts"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(user).unwrap();
        assert_eq!(parsed["prompt"], "darker", "plan request rides the user part");
    }

    #[test]
    fn candidate_text_parts_are_joined() {
        let raw = r#"{"candidates":[{"content":{"parts":[
            {"text":"{\"diffs\":[\"{\\\"op\\\":1}\"],"},
            {"text":"\"notes\":\"n\"}"}
        ]}}]}"#;
        let plan = extract_plan(raw);
        let v: Value = serde_json::from_str(&plan).unwrap();
        assert_eq!(v["notes"], "n");
    }

    #[test]
    fn missing_candidate_is_an_empty_payload() {
        assert_eq!(extract_plan(r#"{"candidates":[]}"#), "");
        assert_eq!(extract_plan("not json"), "");
    }
}
