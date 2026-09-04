//! Custom patch IR fixtures + structural rules (issue #37): the committed
//! patch library validates + compiles clean, cycles follow the delay-only
//! feedback rule, caps and bounds produce the right error codes, patch params
//! ride `SetInstrumentParam` dotted paths, and compile is deterministic.

use kontinuum_ir::compile::{compile_patch, PatchCompileError};
use kontinuum_ir::schema::{InstrumentDef, Session};
use kontinuum_ir::{apply_diff, validate_session, CustomPatch, ErrorCatalog, IrDiff};

const MANIFEST: &str = env!("CARGO_MANIFEST_DIR");
const PATCHES: [&str; 3] = [
    "fixtures/patches/metallic_fm_perc.json",
    "fixtures/patches/resonant_bell.json",
    "fixtures/patches/acid_bass.json",
];

fn read(rel: &str) -> String {
    std::fs::read_to_string(format!("{MANIFEST}/{rel}")).expect("fixture file")
}

#[test]
fn patch_fixtures_validate_and_compile_clean() {
    for rel in PATCHES {
        let session: Session =
            serde_json::from_str(&read(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
        validate_session(&session).unwrap_or_else(|e| panic!("{rel}: {e:?}"));
        let InstrumentDef::Custom(custom) = &session.tracks[0].instrument else {
            panic!("{rel}: expected a custom instrument track");
        };
        let plan = compile_patch(custom).unwrap_or_else(|e| panic!("{rel}: {e}"));
        assert_eq!(plan.nodes.last().map(kontinuum_ir::patch::PatchNode::id), Some("out1"));
        assert!(!plan.delay_lines.is_empty() || rel.ends_with("acid_bass.json"));
        assert!(plan.edges.len() <= kontinuum_ir::schema::bounds::MAX_PATCH_EDGES);
    }
}

#[test]
fn patch_compile_is_deterministic_across_runs() {
    for rel in PATCHES {
        let session: Session = serde_json::from_str(&read(rel)).expect("session");
        let InstrumentDef::Custom(custom) = &session.tracks[0].instrument else {
            panic!("custom instrument");
        };
        let a = compile_patch(custom).expect("compile");
        let b = compile_patch(custom).expect("compile");
        assert_eq!(format!("{a:?}"), format!("{b:?}"), "{rel} must compile identically");
        let json = serde_json::to_string(custom).expect("serialize");
        let reparsed: CustomPatch = serde_json::from_str(&json).expect("reparse");
        assert_eq!(custom, &reparsed, "{rel} must round-trip through JSON");
    }
}

fn session_with_patch(patch_json: &str) -> Session {
    let mut doc = String::from(
        r#"{"version": 1, "seed": 1, "tempo_lane": [[0, 124.0]],
        "sections": [{"id": "a", "bars": 2, "energy_curve": [0.5],
            "pattern_bindings": {"p": {"generator": "euclidean", "k": 4, "n": 16}}}],
        "tracks": [{"id": "p", "role": "perc",
            "instrument": {"kind": "custom", "patch": "#,
    );
    doc.push_str(patch_json);
    doc.push_str("}}]}\n");
    serde_json::from_str(&doc).expect("session parses")
}

fn codes(session: &Session) -> Vec<&'static str> {
    validate_session(session)
        .expect_err("fixture must fail")
        .into_iter()
        .map(|e| e.code)
        .collect()
}

#[test]
fn raw_cycle_is_rejected_but_delay_cycle_passes() {
    let raw = r#"{"nodes": [
        {"id": "o1", "type": "osc"}, {"id": "f1", "type": "filter"},
        {"id": "x", "type": "out"}],
        "edges": [
            {"from": "o1", "to": "f1", "type": "audio"},
            {"from": "f1", "to": "o1", "type": "audio"},
            {"from": "f1", "to": "x", "type": "audio"}]}"#;
    let set: std::collections::BTreeSet<_> = codes(&session_with_patch(raw)).into_iter().collect();
    assert!(set.contains(&ErrorCatalog::E_PATCH_CYCLE), "{set:?}");

    let via_delay = r#"{"nodes": [
        {"id": "o1", "type": "osc"}, {"id": "f1", "type": "filter"},
        {"id": "d1", "type": "delay"}, {"id": "x", "type": "out"}],
        "edges": [
            {"from": "o1", "to": "f1", "type": "audio"},
            {"from": "f1", "to": "d1", "type": "audio"},
            {"from": "d1", "to": "f1", "type": "audio"},
            {"from": "d1", "to": "x", "type": "audio"}]}"#;
    let s = session_with_patch(via_delay);
    validate_session(&s).expect("delay-mediated feedback is legal");
}

#[test]
fn node_and_edge_caps_are_enforced() {
    let nodes: Vec<String> = (0..=kontinuum_ir::schema::bounds::MAX_PATCH_NODES)
        .map(|i| format!(r#"{{"id": "g{i}", "type": "gain", "level": 1.0}}"#))
        .chain(std::iter::once(r#"{"id": "x", "type": "out"}"#.to_string()))
        .collect();
    let patch = format!(r#"{{"nodes": [{}], "edges": []}}"#, nodes.join(","));
    let set: std::collections::BTreeSet<_> =
        codes(&session_with_patch(&patch)).into_iter().collect();
    assert!(set.contains(&ErrorCatalog::E_PATCH_TOO_MANY_NODES), "{set:?}");

    let edges: Vec<String> = (0..33)
        .map(|i| {
            format!(r#"{{"from": "o1", "to": "g{}", "type": "audio"}}"#, i % 2)
        })
        .collect();
    let patch = format!(
        r#"{{"nodes": [
            {{"id": "o1", "type": "osc"}}, {{"id": "g0", "type": "gain"}},
            {{"id": "g1", "type": "gain"}}, {{"id": "x", "type": "out"}}],
        "edges": [{}]}}"#,
        edges.join(",")
    );
    let set: std::collections::BTreeSet<_> =
        codes(&session_with_patch(&patch)).into_iter().collect();
    assert!(set.contains(&ErrorCatalog::E_PATCH_TOO_MANY_EDGES), "{set:?}");
}

#[test]
fn bounds_and_identity_violations_produce_catalog_codes() {
    let cases: [(&str, &'static str); 6] = [
        (
            r#"{"nodes": [{"id": "o1", "type": "filter", "cutoff_hz": 99999.0},
                {"id": "x", "type": "out"}],
                "edges": [{"from": "o1", "to": "x", "type": "audio"}]}"#,
            ErrorCatalog::E_PARAM_RANGE,
        ),
        (
            r#"{"nodes": [{"id": "o1", "type": "osc"}, {"id": "o1", "type": "osc"},
                {"id": "x", "type": "out"}],
                "edges": [{"from": "o1", "to": "x", "type": "audio"}]}"#,
            ErrorCatalog::E_PATCH_DUPLICATE_NODE_ID,
        ),
        (
            r#"{"nodes": [{"id": "o1", "type": "osc"}, {"id": "x", "type": "out"}],
                "edges": [{"from": "o1", "to": "ghost", "type": "audio"}]}"#,
            ErrorCatalog::E_PATCH_UNKNOWN_EDGE_NODE,
        ),
        (
            r#"{"nodes": [{"id": "o1", "type": "osc"}], "edges": []}"#,
            ErrorCatalog::E_PATCH_NO_OUT,
        ),
        (
            r#"{"nodes": [{"id": "o1", "type": "osc"}, {"id": "e2", "type": "env"},
                {"id": "x", "type": "out"}],
                "edges": [{"from": "o1", "to": "x", "type": "audio"}]}"#,
            ErrorCatalog::E_PATCH_DISCONNECTED,
        ),
        (
            r#"{"nodes": [{"id": "o1", "type": "osc"}, {"id": "x", "type": "out"}],
                "edges": [{"from": "o1", "to": "x", "type": "audio"},
                          {"from": "o1", "to": "x", "type": "audio"}]}"#,
            ErrorCatalog::E_PATCH_DUPLICATE_EDGE,
        ),
    ];
    for (patch, want) in cases {
        let set: std::collections::BTreeSet<_> =
            codes(&session_with_patch(patch)).into_iter().collect();
        assert!(set.contains(&want), "missing {want} for {patch}");
    }
}

#[test]
fn signal_type_and_mod_target_rules() {
    // mod edge sourced from an oscillator: rate/type violation.
    let bad_source = r#"{"nodes": [
        {"id": "o1", "type": "osc"}, {"id": "g1", "type": "gain"},
        {"id": "x", "type": "out"}],
        "edges": [
            {"from": "o1", "to": "g1", "type": "audio"},
            {"from": "g1", "to": "x", "type": "audio"},
            {"from": "o1", "to": "g1", "type": "mod", "param": "level"}]}"#;
    let set: std::collections::BTreeSet<_> =
        codes(&session_with_patch(bad_source)).into_iter().collect();
    assert!(set.contains(&ErrorCatalog::E_PATCH_SIGNAL_TYPE), "{set:?}");

    // mod edge naming a param the target does not modulate.
    let bad_target = r#"{"nodes": [
        {"id": "o1", "type": "osc"}, {"id": "e1", "type": "env"},
        {"id": "x", "type": "out"}],
        "edges": [
            {"from": "o1", "to": "x", "type": "audio"},
            {"from": "e1", "to": "x", "type": "mod", "param": "level"}]}"#;
    let set: std::collections::BTreeSet<_> =
        codes(&session_with_patch(bad_target)).into_iter().collect();
    assert!(set.contains(&ErrorCatalog::E_PATCH_UNKNOWN_MOD_TARGET), "{set:?}");
}

#[test]
fn patch_params_ride_set_instrument_param_dotted_paths() {
    let session: Session = serde_json::from_str(&read(PATCHES[2])).expect("acid fixture");
    let d = IrDiff::SetInstrumentParam {
        track: "acid".into(),
        param: "patch.flt1.cutoff_hz".into(),
        value: 800.0,
    };
    let mut s = session;
    let r = apply_diff(&mut s, &d, 0).expect("patch param applies");
    assert_eq!(r.superseded, vec!["acid.patch.flt1.cutoff_hz=420".to_string()]);
    let InstrumentDef::Custom(custom) = &s.tracks[0].instrument else {
        panic!("custom instrument");
    };
    let plan_a = compile_patch(custom).expect("compile after diff");
    let cutoff = plan_a.nodes.iter().find_map(|n| match n {
        kontinuum_ir::patch::PatchNode::Filter(f) if f.id == "flt1" => Some(f.cutoff_hz),
        _ => None,
    });
    assert_eq!(cutoff, Some(800.0));
    // Validation still passes after the mutation.
    validate_session(&s).expect("mutated session stays valid");
}

#[test]
fn exec_order_places_out_last_and_respects_routing() {
    let session: Session = serde_json::from_str(&read(PATCHES[1])).expect("bell fixture");
    let InstrumentDef::Custom(custom) = &session.tracks[0].instrument else {
        panic!("custom instrument");
    };
    let plan = compile_patch(custom).expect("compile");
    let pos = |id: &str| {
        plan.nodes
            .iter()
            .position(|n| kontinuum_ir::patch::PatchNode::id(n) == id)
            .expect("node present")
    };
    assert_eq!(kontinuum_ir::patch::PatchNode::id(plan.nodes.last().expect("nodes")), "out1");
    assert!(pos("carrier") < pos("g1"));
    assert!(pos("g1") < pos("flt1"));
    assert!(pos("flt1") < pos("dly1"));
    assert_eq!(plan.delay_lines.len(), 1, "bell has one feedback delay line");
    // Cycle rejection through the compile surface too.
    let raw = r#"{"kind": "custom", "patch": {"nodes": [
        {"id": "o1", "type": "osc"}, {"id": "g1", "type": "gain"}, {"id": "x", "type": "out"}],
        "edges": [{"from": "o1", "to": "g1", "type": "audio"},
                  {"from": "g1", "to": "o1", "type": "audio"},
                  {"from": "g1", "to": "x", "type": "audio"}]}}"#;
    let p: CustomPatch = serde_json::from_str(raw).expect("patch");
    assert_eq!(
        compile_patch(&p),
        Err(PatchCompileError::Cycle { path: vec!["o1".into(), "g1".into()] })
    );
}
