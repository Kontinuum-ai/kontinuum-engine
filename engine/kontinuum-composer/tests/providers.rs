//! The #36 provider adapters against real in-process mock servers (the #21
//! taste pattern): one OpenAI-compatible endpoint, the Anthropic Messages
//! API, and Gemini generateContent. Each adapter runs end to end — build
//! envelope → mock server over the bundled TCP transport → extraction →
//! validated-diff application through [`run_wake`] — plus the schema-mode
//! and auth-header contracts.

mod common;

use std::sync::Arc;

use kontinuum_compose::arrangement::{generate_session, GenParams};
use kontinuum_compose::engine::ArrangementEngine;
use kontinuum_composer::{
    build_plan_request, run_wake, BackendConfig, BackendSelector, Caps, ComposerBackend,
    HttpCloudBackend, PlanRequest, Steering,
};
use kontinuum_ir::Session;

const GOOD_DIFF: &str =
    r#"{"op":"set_instrument_param","track":"kick","param":"decay_ms","value":220.0}"#;
const KEY: &str = "sk-test-e2e-77aa31";

fn session() -> Session {
    generate_session(&GenParams { seed: 11, target_bars: 32, ..Default::default() })
}

fn plan_request(session: &Session) -> PlanRequest {
    build_plan_request(session, 8, Steering {
        style: "techno",
        prompt: "darker",
        taste_json: "{}",
        style_card: "",
    })
}

fn wake_with(
    session: &Session,
    t2: &mut dyn ComposerBackend,
) -> kontinuum_composer::ComposerReport {
    let mut t0 = kontinuum_composer::OnDeviceHeuristicBackend;
    let mut ladder = BackendSelector { prefer_on_device: false, ..Default::default() }
        .ladder_tiers(Some(t2), None, &mut t0);
    let mut engine = ArrangementEngine::new(session.clone(), 48_000);
    let request = plan_request(session);
    run_wake(&mut engine, &mut ladder, &request)
}

fn json_body(status_body: String) -> common::MockResponse {
    (200, vec![("Content-Type".into(), "application/json".into())], status_body)
}

// -- OpenAI-compatible -------------------------------------------------------

#[test]
fn openai_compat_mock_server_plans_end_to_end() {
    let server = common::MockServer::start(Arc::new(|req: &common::RecordedRequest| {
        assert_eq!(req.path, "/v1/chat/completions");
        assert_eq!(req.header("authorization"), Some(format!("Bearer {KEY}")));
        let body: serde_json::Value = serde_json::from_str(&req.body).unwrap();
        assert_eq!(body["model"], "qwen3.5-2b");
        assert_eq!(body["temperature"], 0.0);
        json_body(r#"{"choices":[{"message":{"content":"{\"diffs\":[\"{\\\"op\\\":\\\"set_instrument_param\\\",\\\"track\\\":\\\"kick\\\",\\\"param\\\":\\\"decay_ms\\\",\\\"value\\\":220.0}\"],\"notes\":\"mock openai\"}"}}]}"#.to_string())
    }));
    let endpoint = format!("{}/v1/chat/completions", server.base_url);
    let mut t2 = HttpCloudBackend::openai_chat(endpoint, 8_000, "qwen3.5-2b".into(), common::tcp_transport)
        .with_key(Some(KEY));
    assert_eq!(t2.capabilities(), Caps::remote(false), "json_object wire: no strict schema cap");
    let report = wake_with(&session(), &mut t2);
    assert_eq!(report.backend, "http-cloud");
    assert_eq!(report.applied, vec![GOOD_DIFF.to_string()]);
    assert_eq!(server.request_count(), 1, "first plan lands: no repair round");
}

#[test]
fn openai_schema_mode_constrains_the_response_for_capable_providers() {
    let server = common::MockServer::start(Arc::new(|req: &common::RecordedRequest| {
        let body: serde_json::Value = serde_json::from_str(&req.body).unwrap();
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(body["response_format"]["json_schema"]["strict"], true);
        assert_eq!(body["response_format"]["json_schema"]["schema"]["type"], "object");
        json_body(r#"{"choices":[{"message":{"content":"{\"diffs\":[\"{\\\"op\\\":\\\"set_instrument_param\\\",\\\"track\\\":\\\"kick\\\",\\\"param\\\":\\\"decay_ms\\\",\\\"value\\\":220.0}\"],\"notes\":\"strict\"}"}}]}"#.to_string())
    }));
    let endpoint = format!("{}/v1/chat/completions", server.base_url);
    let mut t2 = HttpCloudBackend::openai_chat(endpoint, 8_000, "qwen3.5-2b".into(), common::tcp_transport)
        .with_key(Some(KEY))
        .with_strict_schema(true);
    assert_eq!(t2.capabilities(), Caps::remote(true));
    let report = wake_with(&session(), &mut t2);
    assert_eq!(report.applied, vec![GOOD_DIFF.to_string()]);
    assert_eq!(server.request_count(), 1);
}

// -- Anthropic ---------------------------------------------------------------

#[test]
fn anthropic_mock_server_plans_end_to_end_via_tool_use() {
    let server = common::MockServer::start(Arc::new(|req: &common::RecordedRequest| {
        assert_eq!(req.path, "/v1/messages");
        assert_eq!(req.header("x-api-key"), Some(KEY.to_string()));
        assert_eq!(req.header("anthropic-version"), Some("2023-06-01".to_string()));
        assert!(!req.body.contains(KEY), "the key never enters a request body");
        let body: serde_json::Value = serde_json::from_str(&req.body).unwrap();
        assert_eq!(body["tool_choice"], serde_json::json!({"type": "tool", "name": "emit_plan"}));
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
        json_body(r#"{"content":[{"type":"tool_use","name":"emit_plan","input":{"diffs":["{\"op\":\"set_instrument_param\",\"track\":\"kick\",\"param\":\"decay_ms\",\"value\":220.0}"],"notes":"mock anthropic"}}],"stop_reason":"tool_use"}"#.to_string())
    }));
    let endpoint = format!("{}/v1/messages", server.base_url);
    let mut t2 = HttpCloudBackend::anthropic(endpoint, 8_000, "claude-sonnet-4-6".into(), common::tcp_transport)
        .with_key(Some(KEY));
    assert!(t2.capabilities().json_schema, "tool input_schema is schema capability");
    let report = wake_with(&session(), &mut t2);
    assert_eq!(report.backend, "http-cloud");
    assert_eq!(report.applied, vec![GOOD_DIFF.to_string()]);
    assert_eq!(server.request_count(), 1);
}

// -- Gemini ------------------------------------------------------------------

#[test]
fn gemini_mock_server_plans_end_to_end_via_response_schema() {
    let server = common::MockServer::start(Arc::new(|req: &common::RecordedRequest| {
        assert!(req.path.starts_with("/v1beta/models/gemini-2.5-flash:generateContent"));
        assert_eq!(req.header("x-goog-api-key"), Some(KEY.to_string()));
        assert!(!req.body.contains(KEY), "the key never enters a request body");
        let body: serde_json::Value = serde_json::from_str(&req.body).unwrap();
        assert_eq!(body["generationConfig"]["responseMimeType"], "application/json");
        assert_eq!(body["generationConfig"]["responseSchema"]["type"], "object");
        json_body(r#"{"candidates":[{"content":{"parts":[{"text":"{\"diffs\":[\"{\\\"op\\\":\\\"set_instrument_param\\\",\\\"track\\\":\\\"kick\\\",\\\"param\\\":\\\"decay_ms\\\",\\\"value\\\":220.0}\"],\"notes\":\"mock gemini\"}"}]}}]}"#.to_string())
    }));
    let endpoint = format!("{}/v1beta/models/gemini-2.5-flash:generateContent", server.base_url);
    let mut t2 = HttpCloudBackend::gemini(endpoint, 8_000, "gemini-2.5-flash".into(), common::tcp_transport)
        .with_key(Some(KEY));
    assert!(t2.capabilities().json_schema);
    let report = wake_with(&session(), &mut t2);
    assert_eq!(report.backend, "http-cloud");
    assert_eq!(report.applied, vec![GOOD_DIFF.to_string()]);
    assert_eq!(server.request_count(), 1);
}

// -- Config-driven adapters (settings shape) ---------------------------------

#[test]
fn backend_config_builds_each_mock_provider_and_the_wake_runs() {
    for (provider, path, reply) in [
        (
            "openai_compat",
            "/v1/chat/completions",
            r#"{"choices":[{"message":{"content":"{\"diffs\":[\"{\\\"op\\\":\\\"set_instrument_param\\\",\\\"track\\\":\\\"kick\\\",\\\"param\\\":\\\"decay_ms\\\",\\\"value\\\":220.0}\"],\"notes\":\"cfg\"}"}}]}"#,
        ),
        (
            "anthropic",
            "/v1/messages",
            r#"{"content":[{"type":"tool_use","name":"emit_plan","input":{"diffs":["{\"op\":\"set_instrument_param\",\"track\":\"kick\",\"param\":\"decay_ms\",\"value\":220.0}"],"notes":"cfg"}}]}"#,
        ),
        (
            "gemini",
            "/v1beta/models/gemini-2.5-flash:generateContent",
            r#"{"candidates":[{"content":{"parts":[{"text":"{\"diffs\":[\"{\\\"op\\\":\\\"set_instrument_param\\\",\\\"track\\\":\\\"kick\\\",\\\"param\\\":\\\"decay_ms\\\",\\\"value\\\":220.0}\"],\"notes\":\"cfg\"}"}]}}]}"#,
        ),
    ] {
        let server = common::MockServer::start(Arc::new(move |_req: &common::RecordedRequest| {
            json_body(reply.to_string())
        }));
        let mut cfg = BackendConfig::on_device_default("quick_moves");
        cfg.provider = provider.into();
        cfg.endpoint = format!("{}{path}", server.base_url);
        cfg.model = "gemini-2.5-flash".into();
        let mut backend = cfg.build(common::tcp_transport, Some(KEY), vec![]).unwrap();
        let report = wake_with(&session(), backend.as_mut());
        assert_eq!(report.backend, "http-cloud", "{provider} wake lands through the config");
        assert!(!report.applied.is_empty(), "{provider} plan applied");
    }
}
