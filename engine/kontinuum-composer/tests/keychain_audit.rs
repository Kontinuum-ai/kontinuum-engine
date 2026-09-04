//! The #36 hard rule, grep-audited: API keys live in the Keychain only and
//! **never** appear in session files, exports, or logs. The audit puts a
//! distinctive key in the store, then serializes every artifact the Rust
//! side can produce — session JSON, composer settings, plan request and
//! response, wake report, telemetry, provider envelopes, a log buffer —
//! and asserts the key string surfaces in none of them. Positive control:
//! the key does ride the wire headers (and only there).

mod common;

use std::sync::{Arc, Mutex};

use kontinuum_compose::arrangement::{generate_session, GenParams};
use kontinuum_composer::{
    build_plan_request, run_wake, BackendConfig, BackendSelector, ComposerBackend,
    ComposerRole, ComposerSecrets, MemorySecretStore, OnDeviceHeuristicBackend, PlanResponse,
    RoleConfig, ScriptedBackend, SecretStore, Steering,
};
use kontinuum_ir::Session;

/// Distinctive enough that a substring match can't false-negative, random
/// enough that no fixture could contain it by accident.
const KEY: &str = "sk-live-AUDIT-9f3c1e77aa31b40d";

const PROVIDER: &str = "openai_compat";

fn store_with_key() -> MemorySecretStore {
    let store = MemorySecretStore::new();
    store.set(&ComposerSecrets::account_for(PROVIDER), KEY);
    store
}

fn session() -> Session {
    generate_session(&GenParams { seed: 31, target_bars: 32, ..Default::default() })
}

fn settings_config() -> BackendConfig {
    let mut cfg = BackendConfig::on_device_default(ComposerRole::QuickMoves.as_str());
    cfg.provider = PROVIDER.into();
    cfg.endpoint = "https://api.openai.com/v1/chat/completions".into();
    cfg.model = "qwen3.5-2b".into();
    cfg
}

#[test]
fn keys_survive_nowhere_outside_the_keystore() {
    let store = store_with_key();
    let account = ComposerSecrets::account_for(PROVIDER);

    // Positive control: the keystore holds it.
    assert_eq!(store.get(&account).as_deref(), Some(KEY));

    let session = session();
    let request = build_plan_request(&session, 8, Steering {
        style: "techno",
        prompt: "darker",
        taste_json: "{}",
        style_card: "",
    });
    let response = PlanResponse {
        diffs: vec![r#"{"op":"set_section_energy","id":"intro","energy":[0.5]}"#.into()],
        notes: "planned".into(),
        backend_id: "t1-foundation-models".into(),
        latency_hint_ms: 0,
    };
    let role_config = RoleConfig::default();
    let cfg = settings_config();
    let built = cfg
        .build(
            |_req| Ok(r#"{"diffs":[],"notes":"x"}"#.to_string()),
            store.get(&account).as_deref(),
            vec![],
        )
        .unwrap();

    let mut telemetry = kontinuum_composer::ComposerBackendTelemetry::default();
    telemetry.record_plan("http-cloud", kontinuum_composer::Tier::T2Cloud, 2, 0, 0, 90);
    telemetry.record_tokens(
        "http-cloud",
        kontinuum_composer::Tier::T2Cloud,
        1_000,
        100,
        &kontinuum_composer::ModelInfo {
            id: "qwen3.5-2b".into(),
            name: "Qwen".into(),
            context_window: 32_768,
            input_cost_per_mtok: Some(0.1),
            output_cost_per_mtok: Some(0.2),
            tool_call: true,
        },
    );

    // Every artifact the Rust side can emit, serialized the way hosts
    // persist them (session file, settings file, report/log lines).
    let session_file = serde_json::to_string(&session).unwrap();
    let settings_file = serde_json::to_string(&role_config).unwrap() + &serde_json::to_string(&cfg).unwrap();
    let request_line = serde_json::to_string(&request).unwrap();
    let response_line = serde_json::to_string(&response).unwrap();
    let telemetry_line = serde_json::to_string(&telemetry.rows(0.5)).unwrap();
    let caps_line = format!("{:?}", built.capabilities());

    for (what, artifact) in [
        ("session file", session_file.as_str()),
        ("settings file", settings_file.as_str()),
        ("plan request", request_line.as_str()),
        ("plan response", response_line.as_str()),
        ("telemetry rows", telemetry_line.as_str()),
        ("capabilities log", caps_line.as_str()),
    ] {
        assert!(
            !artifact.contains(KEY),
            "{what} must never contain the API key"
        );
    }

    // The built backend is usable, and its name stays clean for reports.
    assert_eq!(built.name(), "http-cloud");
    let mut built = built;
    let report = plan_through(&session, &mut *built);
    let report_line = serde_json::to_string(&report).unwrap();
    assert!(!report_line.contains(KEY), "wake report must never contain the API key");
}

fn plan_through(
    session: &kontinuum_ir::Session,
    backend: &mut dyn ComposerBackend,
) -> kontinuum_composer::ComposerReport {
    let request = build_plan_request(session, 8, Steering {
        style: "techno",
        prompt: "darker",
        taste_json: "{}",
        style_card: "",
    });
    let mut t0 = OnDeviceHeuristicBackend;
    let mut ladder = BackendSelector { prefer_on_device: false, ..Default::default() }
        .ladder_tiers(Some(backend), None, &mut t0);
    let mut engine = kontinuum_compose::engine::ArrangementEngine::new(session.clone(), 48_000);
    run_wake(&mut engine, &mut ladder, &request)
}

#[test]
fn headers_carry_the_key_bodies_and_logs_never_do() {
    let seen_bodies = Arc::new(Mutex::new(Vec::new()));
    let seen_headers = Arc::new(Mutex::new(Vec::new()));
    let bodies = seen_bodies.clone();
    let headers_log = seen_headers.clone();
    let server = common::MockServer::start(Arc::new(move |req: &common::RecordedRequest| {
        bodies.lock().unwrap().push(req.body.clone());
        headers_log.lock().unwrap().push(req.header("authorization").unwrap_or_default());
        (200, vec![], r#"{"choices":[{"message":{"content":"{\"diffs\":[\"{\\\"op\\\":\\\"set_instrument_param\\\",\\\"track\\\":\\\"kick\\\",\\\"param\\\":\\\"decay_ms\\\",\\\"value\\\":220.0}\"],\"notes\":\"ok\"}"}}]}"#.to_string())
    }));

    let store = store_with_key();
    let mut cfg = settings_config();
    cfg.endpoint = format!("{}/v1/chat/completions", server.base_url);
    let mut backend = cfg
        .build(common::tcp_transport, store.get(&ComposerSecrets::account_for(PROVIDER)).as_deref(), vec![])
        .unwrap();
    let report = plan_through(&session(), &mut *backend);
    assert_eq!(report.backend, "http-cloud");

    let bodies = seen_bodies.lock().unwrap();
    let headers = seen_headers.lock().unwrap();
    assert_eq!(headers.len(), bodies.len());
    for (header, body) in headers.iter().zip(bodies.iter()) {
        assert_eq!(header, &format!("Bearer {KEY}"), "auth rides the header");
        assert!(!body.contains(KEY), "request bodies never carry the key");
    }
    assert!(!headers.is_empty(), "the audit observed at least one call");
}

#[test]
fn log_buffer_accumulating_every_surface_stays_key_free() {
    // One buffer a naive host might build by formatting everything.
    let store = store_with_key();
    let session = session();
    let request = build_plan_request(&session, 4, Steering {
        style: "techno",
        prompt: "keep going",
        taste_json: "{}",
        style_card: "",
    });
    let scripted = ScriptedBackend::new("t1-foundation-models", vec![vec![
        r#"{"op":"set_instrument_param","track":"kick","param":"decay_ms","value":220.0}"#.into(),
    ]]);
    let mut scripted = scripted;
    let response = scripted.plan(&request).unwrap();

    let key_in_store = store.get(&ComposerSecrets::account_for(PROVIDER)).unwrap();
    assert_eq!(key_in_store, KEY);

    let log = format!(
        "session={} config={} request={} response={} telemetry={}",
        serde_json::to_string(&session).unwrap(),
        serde_json::to_string(&RoleConfig::default()).unwrap(),
        serde_json::to_string(&request).unwrap(),
        serde_json::to_string(&response).unwrap(),
        serde_json::to_string(
            &kontinuum_composer::ComposerBackendTelemetry::default().rows(1.0)
        )
        .unwrap(),
    );
    assert!(!log.contains(KEY), "the combined log buffer must never contain the API key");
    assert!(!log.contains(&key_in_store), "even the store-resolved value stays out");
}
