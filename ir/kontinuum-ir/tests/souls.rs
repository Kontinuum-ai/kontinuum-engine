//! Session.souls (issue #55) validation tests: structural rules on the soul
//! stack, and backward compatibility — sessions without the field parse and
//! validate exactly as before.

use kontinuum_ir::{validate_session, ErrorCatalog, Session};

fn session_with_souls(souls: &str) -> Session {
    let json = format!(
        r#"{{
            "version": 1,
            "seed": 7,
            "tempo_lane": [[0, 124.0]],
            "sections": [{{"id": "a", "bars": 8, "energy_curve": [0.5],
                "pattern_bindings": {{"kick": {{"generator": "euclidean", "k": 4, "n": 16, "rot": 0, "velocity": 0.8}}}}}}],
            "tracks": [
                {{"id": "kick", "role": "kick", "instrument": {{"kind": "kick"}}}}
            ],
            "souls": {souls}
        }}"#
    );
    serde_json::from_str(&json).expect("session JSON parses")
}

fn codes(errors: &[kontinuum_ir::ValidationError]) -> Vec<&'static str> {
    errors.iter().map(|e| e.code).collect()
}

#[test]
fn valid_soul_stack_validates() {
    let s = session_with_souls(
        r#"[{"id": "detroit-909-minimalism", "weight": 0.6},
            {"id": "dub-techno-chills", "weight": 0.4, "era": "90s"}]"#,
    );
    assert_eq!(validate_session(&s), Ok(()));
}

#[test]
fn absence_of_souls_is_fully_backward_compatible() {
    let json = r#"{
        "version": 1, "seed": 7, "tempo_lane": [[0, 124.0]],
        "sections": [{"id": "a", "bars": 8, "energy_curve": [0.5],
            "pattern_bindings": {"kick": {"generator": "euclidean", "k": 4, "n": 16, "rot": 0, "velocity": 0.8}}}],
        "tracks": [{"id": "kick", "role": "kick", "instrument": {"kind": "kick"}}]
    }"#;
    let s: Session = serde_json::from_str(json).expect("pre-souls sessions parse");
    assert_eq!(s.souls, None);
    assert_eq!(validate_session(&s), Ok(()));
}

#[test]
fn structural_rules_fire_with_actionable_codes() {
    let zero = session_with_souls(r#"[{"id": "x", "weight": 0.0}]"#);
    assert!(codes(&validate_session(&zero).unwrap_err()).contains(&ErrorCatalog::E_SOUL_WEIGHT_RANGE));

    let over = session_with_souls(r#"[{"id": "x", "weight": 1.5}]"#);
    assert!(codes(&validate_session(&over).unwrap_err()).contains(&ErrorCatalog::E_SOUL_WEIGHT_RANGE));

    let empty_id = session_with_souls(r#"[{"id": "  ", "weight": 1.0}]"#);
    assert!(codes(&validate_session(&empty_id).unwrap_err()).contains(&ErrorCatalog::E_SOUL_EMPTY_ID));

    let blank_era = session_with_souls(r#"[{"id": "x", "weight": 1.0, "era": " "}]"#);
    assert!(codes(&validate_session(&blank_era).unwrap_err()).contains(&ErrorCatalog::E_SOUL_ERA_EMPTY));

    let dup = session_with_souls(r#"[{"id": "x", "weight": 1.0}, {"id": "x", "weight": 0.5}]"#);
    assert!(codes(&validate_session(&dup).unwrap_err()).contains(&ErrorCatalog::E_SOUL_DUPLICATE));

    // Same id with a *different* era is a distinct stack entry.
    let dup_era = session_with_souls(
        r#"[{"id": "x", "weight": 1.0}, {"id": "x", "weight": 0.5, "era": "90s"}]"#,
    );
    assert_eq!(validate_session(&dup_era), Ok(()));
}

#[test]
fn unknown_soul_field_is_a_parse_error() {
    let json = r#"{{
            "version": 1,
            "seed": 7,
            "tempo_lane": [[0, 124.0]],
            "sections": [{{"id": "a", "bars": 8, "energy_curve": [0.5],
                "pattern_bindings": {{"kick": {{"generator": "euclidean", "k": 4, "n": 16, "rot": 0, "velocity": 0.8}}}}}}],
            "tracks": [{{"id": "kick", "role": "kick", "instrument": {{"kind": "kick"}}}}],
            "souls": [{{"id": "x", "weight": 1.0, "wight": 2.0}}]
        }}"#
        .replace("{{", "{")
        .replace("}}", "}");
    assert!(
        serde_json::from_str::<Session>(&json).is_err(),
        "deny_unknown_fields must reject the typo field"
    );
}
