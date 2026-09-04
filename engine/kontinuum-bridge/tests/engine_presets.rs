//! Engine realizations for the instrument library (issue #97): every preset
//! in `fixtures/engine-presets.json` must resolve to a valid IR instrument
//! (voice params in the `SetInstrumentParam` vocabulary, patch graphs
//! through the #37 compiler + validator), render audibly, and stay
//! semantically pinned to the `engine` objects the bundled
//! InstrumentsCatalog.json carries for the same ids.

use std::ffi::{CStr, CString};

use kontinuum_bridge::ffi::{
    kontinuum_engine_export_session, kontinuum_engine_free, kontinuum_engine_new,
    kontinuum_engine_play, kontinuum_engine_render, kontinuum_engine_set_track_instrument,
    kontinuum_engine_stop, kontinuum_last_error,
};
use kontinuum_ir::{compile_session, validate_session, InstrumentDef, Session};
use serde_json::Value;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/engine-presets.json");
const CATALOG: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/instruments-catalog.json");
const FRAMES: usize = 512;

fn presets() -> Value {
    let raw = std::fs::read_to_string(FIXTURE).expect("read engine-presets fixture");
    let parsed: Value = serde_json::from_str(&raw).expect("parse engine-presets fixture");
    parsed["presets"].clone()
}

/// A voice preset's params merged over `{"kind": …}` — the exact JSON the
/// bridge FFI consumes and the session carries.
fn voice_instrument_json(preset: &Value) -> String {
    let mut def = preset["params"].as_object().expect("params object").clone();
    def.insert("kind".into(), preset["voice"].clone());
    Value::Object(def).to_string()
}

fn patch_instrument_json(patch: &Value) -> String {
    serde_json::json!({ "kind": "custom", "patch": patch }).to_string()
}

/// Instrument JSON -> IR def, so `deny_unknown_fields` catches params a kind
/// does not own and the validator catches out-of-range values.
fn parse_instrument(json: &str) -> InstrumentDef {
    serde_json::from_str(json).expect("instrument must parse into InstrumentDef")
}

/// Session role matching a preset class, so validation sees a coherent doc.
fn role_for(preset: &Value) -> &'static str {
    match preset["class"].as_str().expect("class") {
        "drum" => {
            if preset["voice"] == "kick" {
                "kick"
            } else {
                "perc"
            }
        }
        "bass" => "bass",
        _ => "pad",
    }
}

fn session_with(instrument_json: &str, role: &str) -> String {
    format!(
        r#"{{
            "version": 1, "seed": 3,
            "tempo_lane": [[0, 120.0]],
            "sections": [{{"id": "a", "bars": 4, "energy_curve": [0.8],
                "pattern_bindings": {{"x": {{"generator": "euclidean", "k": 4, "n": 16}}}}}}],
            "tracks": [{{"id": "x", "role": "{role}", "instrument": {instrument_json}}}]
        }}"#
    )
}

#[test]
fn every_voice_preset_uses_the_kind_vocabulary_and_validates() {
    for (id, preset) in presets().as_object().expect("presets object") {
        let Some(voice) = preset.get("voice") else { continue };
        let json = voice_instrument_json(preset);
        parse_instrument(&json); // panics with a clear message on unknown fields
        assert!(voice.is_string());
        let session: Session =
            serde_json::from_str(&session_with(&json, role_for(preset))).expect("session");
        validate_session(&session).unwrap_or_else(|e| panic!("{id} must validate: {e:?}"));
    }
}

#[test]
fn every_patch_preset_compiles_and_validates() {
    use kontinuum_ir::compile::compile_patch;
    for (id, preset) in presets().as_object().expect("presets object") {
        let Some(patch) = preset.get("patch") else { continue };
        let inst = parse_instrument(&patch_instrument_json(&patch));
        let InstrumentDef::Custom(custom) = inst else {
            panic!("{id} must be a custom patch");
        };
        compile_patch(&custom).unwrap_or_else(|e| panic!("{id} must compile: {e}"));
        let session: Session =
            serde_json::from_str(&session_with(&patch_instrument_json(&patch), role_for(&preset)))
                .expect("session");
        validate_session(&session).unwrap_or_else(|e| panic!("{id} must validate: {e:?}"));
    }
}

#[test]
fn catalog_engine_objects_stay_pinned_to_the_fixture() {
    let fixture = presets();
    let raw = std::fs::read_to_string(CATALOG).expect("read bundled catalog");
    let catalog: Value = serde_json::from_str(&raw).expect("parse catalog");
    let entries = catalog["instruments"].as_array().expect("instruments array");
    assert_eq!(entries.len(), 42, "the catalog stays at 42 entries");

    let mut realized = 0;
    for entry in entries {
        let Some(engine) = entry.get("engine") else { continue };
        let id = entry["id"].as_str().expect("id");
        assert_eq!(
            engine,
            &fixture[id],
            "catalog engine object for {id} drifted from fixtures/engine-presets.json"
        );
        realized += 1;
    }
    assert_eq!(realized, 30, "all 30 library entries carry engine realizations");
}

#[test]
fn preset_tracks_render_finite_nonsilent_audio() {
    // One param preset (808-kick) + one patch preset (808-cowbell), the two
    // realization shapes, playing together through the live engine.
    let fixture = presets();
    let kick = voice_instrument_json(&fixture["808-kick"]);
    let cowbell = patch_instrument_json(&fixture["808-cowbell"]["patch"]);
    let session = format!(
        r#"{{
            "version": 1, "seed": 9,
            "tempo_lane": [[0, 120.0]],
            "sections": [{{"id": "a", "bars": 4, "energy_curve": [0.8],
                "pattern_bindings": {{
                    "k": {{"generator": "euclidean", "k": 4, "n": 16}},
                    "c": {{"generator": "euclidean", "k": 4, "n": 16}}
                }}}}],
            "tracks": [
                {{"id": "k", "role": "kick", "instrument": {kick}}},
                {{"id": "c", "role": "perc", "instrument": {cowbell}}}
            ]
        }}"#
    );
    let session = CString::new(session).unwrap();
    // SAFETY: valid C string; the engine is freed below.
    let engine = unsafe { kontinuum_engine_new(48_000, session.as_ptr()) };
    assert!(!engine.is_null(), "{}", last_error_message());

    // SAFETY: live engine pointer.
    assert_eq!(unsafe { kontinuum_engine_play(engine) }, 0);
    let mut l = vec![0.0f32; FRAMES];
    let mut r = vec![0.0f32; FRAMES];
    let mut peak = 0.0f32;
    for _ in 0..384 {
        // SAFETY: buffers outlive the call with FRAMES writable f32s each.
        unsafe { kontinuum_engine_render(engine, l.as_mut_ptr(), r.as_mut_ptr(), FRAMES as u32) };
        peak = peak.max(l.iter().chain(&r).fold(0.0f32, |m, s| m.max(s.abs())));
        assert!(l.iter().chain(&r).all(|s| s.is_finite()), "patch output must stay finite");
    }
    assert!(peak > 0.01, "both preset realizations must be audible, peak {peak}");

    // SAFETY: live engine pointer.
    assert_eq!(unsafe { kontinuum_engine_stop(engine) }, 0);
    // SAFETY: single free of the owned pointer.
    unsafe { kontinuum_engine_free(engine) };
}

#[test]
fn set_track_instrument_roundtrip_through_the_c_abi() {
    let base = CString::new(
        r#"{
            "version": 1, "seed": 5,
            "tempo_lane": [[0, 120.0]],
            "sections": [{"id": "a", "bars": 4, "energy_curve": [0.5],
                "pattern_bindings": {"k": {"generator": "euclidean", "k": 4, "n": 16}}}],
            "tracks": [{"id": "k", "role": "kick", "instrument": {"kind": "kick"}}]
        }"#,
    )
    .unwrap();
    // SAFETY: valid C string.
    let engine = unsafe { kontinuum_engine_new(48_000, base.as_ptr()) };
    assert!(!engine.is_null());

    let fixture = presets();
    let cowbell = CString::new(patch_instrument_json(&fixture["808-cowbell"]["patch"])).unwrap();
    let track = CString::new("k").unwrap();
    // SAFETY: live engine + valid C strings.
    assert_eq!(
        unsafe { kontinuum_engine_set_track_instrument(engine, track.as_ptr(), cowbell.as_ptr()) },
        0,
        "{}",
        last_error_message()
    );
    // SAFETY: live engine pointer.
    let exported = unsafe { kontinuum_engine_export_session(engine) };
    assert!(!exported.is_null());
    // SAFETY: bridge-owned string until the next call.
    let json = unsafe { CStr::from_ptr(exported) }.to_string_lossy().into_owned();
    // SAFETY: bridge-owned string.
    unsafe { kontinuum_bridge::ffi::kontinuum_string_free(exported) };
    assert!(json.contains("custom"), "export must carry the swapped patch: {json}");
    assert!(json.contains("band_pass"), "export must carry the patch graph: {json}");

    // Invalid instrument JSON -> 3 + last_error.
    let bad = CString::new(r#"{"kind": "kick", "wood": 1.0}"#).unwrap();
    // SAFETY: live engine + valid C strings.
    let code = unsafe { kontinuum_engine_set_track_instrument(engine, track.as_ptr(), bad.as_ptr()) };
    assert_eq!(code, 3);
    assert!(
        last_error_message().to_lowercase().contains("parse")
            || last_error_message().to_lowercase().contains("unknown field"),
        "unexpected error: {}",
        last_error_message()
    );

    // Out-of-range param -> 3 (validation).
    let wild = CString::new(r#"{"kind": "kick", "tune_hz": 9000.0}"#).unwrap();
    // SAFETY: live engine + valid C strings.
    assert_eq!(
        unsafe { kontinuum_engine_set_track_instrument(engine, track.as_ptr(), wild.as_ptr()) },
        3
    );

    // Unknown track -> 2.
    let other = CString::new("nope").unwrap();
    // SAFETY: live engine + valid C strings.
    assert_eq!(
        unsafe { kontinuum_engine_set_track_instrument(engine, other.as_ptr(), cowbell.as_ptr()) },
        2
    );

    // Null engine -> 1.
    // SAFETY: null engine exercises the null path.
    unsafe {
        assert_eq!(
            kontinuum_engine_set_track_instrument(
                std::ptr::null_mut(),
                track.as_ptr(),
                cowbell.as_ptr()
            ),
            1
        );
    }

    // SAFETY: single free of the owned pointer.
    unsafe { kontinuum_engine_free(engine) };
}

fn last_error_message() -> String {
    // SAFETY: the bridge guarantees the pointer is a valid C string until the
    // next bridge call on this thread.
    let p = kontinuum_last_error();
    if p.is_null() {
        String::new()
    } else {
        // SAFETY: see above.
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

#[test]
fn every_preset_session_compiles_to_blocks() {
    // Validation passes are cheap; this also runs the compile pass so a
    // preset that only validates but never compiles cannot slip through.
    for (id, preset) in presets().as_object().expect("presets object") {
        let json = if let Some(patch) = preset.get("patch") {
            patch_instrument_json(&patch)
        } else {
            voice_instrument_json(&preset)
        };
        let raw = session_with(&json, role_for(&preset));
        let session: Session = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("{id} session parse: {e}"));
        compile_session(&session, 48_000).unwrap_or_else(|e| panic!("{id} must compile: {e}"));
    }
}
