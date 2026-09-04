//! The JSON schema every provider wire constrains its output with (#36):
//! the [`crate::PlanResponse`] shape, expressed in the OpenAPI subset that
//! OpenAI `json_schema`, Anthropic tool `input_schema`, and Gemini
//! `responseSchema` all accept. The orchestrator's validator-repair loop
//! remains the equalizer — this constrains shape, not musical validity.

use serde_json::{json, Value};

/// Schema for one plan batch: `{"diffs": ["<ir-diff json>", ...], "notes":
/// "..."}`. `backend_id`/`latency_hint_ms` stay optional (the backend fills
/// them in) and are deliberately outside the constraint — providers that
/// echo extra keys still pass.
pub fn plan_response_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "diffs": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Each entry is one IR diff op serialized as a JSON string (op-tagged, e.g. {\"op\": \"set_instrument_param\", \"track\": \"kick\", \"param\": \"decay_ms\", \"value\": 220.0})"
            },
            "notes": { "type": "string", "description": "Short plan rationale" }
        },
        "required": ["diffs", "notes"],
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_pins_the_plan_shape() {
        let s = plan_response_schema();
        assert_eq!(s["type"], "object");
        assert_eq!(s["properties"]["diffs"]["type"], "array");
        assert_eq!(s["required"], json!(["diffs", "notes"]));
        let valid = json!({"diffs": ["{}"], "notes": "n"});
        assert!(valid.get("diffs").is_some());
    }
}
