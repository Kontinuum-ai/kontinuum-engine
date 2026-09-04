//! The #102 export seam across the C ABI: a host hands over a session
//! document and a spec, and gets files plus a readable report.

use std::ffi::{CStr, CString};

use kontinuum_bridge::ffi::{kontinuum_export_masters, kontinuum_last_error, kontinuum_string_free};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/loop-4track.ir.json");

fn session_json() -> String {
    std::fs::read_to_string(FIXTURE).expect("fixture")
}

/// Calls the ABI the way a host does and returns the report JSON, or the
/// thread-local error text.
fn export(session: &str, spec: &str) -> Result<String, String> {
    let c_session = CString::new(session).unwrap();
    let c_spec = CString::new(spec).unwrap();
    // SAFETY: both pointers are valid NUL-terminated strings for the call.
    let raw = unsafe { kontinuum_export_masters(c_session.as_ptr(), c_spec.as_ptr()) };
    if raw.is_null() {
        let err = kontinuum_last_error();
        let msg = if err.is_null() {
            "null error".to_string()
        } else {
            // SAFETY: bridge-owned buffer, valid until the next bridge call.
            unsafe { CStr::from_ptr(err) }.to_string_lossy().into_owned()
        };
        return Err(msg);
    }
    // SAFETY: raw came from CString::into_raw in the bridge.
    let json = unsafe { CStr::from_ptr(raw) }.to_string_lossy().into_owned();
    // SAFETY: same pointer, freed exactly once.
    unsafe { kontinuum_string_free(raw) };
    Ok(json)
}

fn out_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir()
        .join("kontinuum-export-ffi")
        .join(format!("{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn exports_a_press_kit_mp3_through_the_c_abi() {
    let dir = out_dir("mp3");
    let spec = format!(
        r#"{{"artist":"Kontinuum","title":"Night Shift","year":2026,"month":9,"day":2,
             "outDir":{:?},"presets":["pressKitMp3"]}}"#,
        dir.to_string_lossy()
    );
    let report = export(&session_json(), &spec).expect("export");

    let parsed: serde_json::Value = serde_json::from_str(&report).expect("report json");
    let files = parsed["files"].as_array().expect("files array");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0]["encoding"], "mp3_320");
    assert_eq!(files[0]["master"], "premium");
    assert_eq!(files[0]["cut"], "fullMix");
    assert!(parsed["seed"].as_u64().is_some(), "seed missing from report");
    // Hex, not a number: a JSON float would lose the low bits of a u64.
    let hash = files[0]["contentHash"].as_str().expect("hash string");
    assert_eq!(hash.len(), 16, "{hash}");
    assert!(hash.chars().all(|c| c.is_ascii_hexdigit()), "{hash}");

    let path = files[0]["path"].as_str().expect("path");
    let bytes = std::fs::read(path).expect("file on disk");
    assert_eq!(bytes[0], 0xFF, "exported file is not an MP3 stream");
    assert_eq!(bytes.len() as u64, files[0]["bytes"].as_u64().unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_bad_spec_reports_why_instead_of_crossing_the_boundary() {
    let dir = out_dir("bad-spec");
    let spec = format!(
        r#"{{"artist":"K","title":"T","year":2026,"month":9,"day":2,
             "outDir":{:?},"sampleRate":"not a number"}}"#,
        dir.to_string_lossy()
    );
    let err = export(&session_json(), &spec).expect_err("must be rejected");
    assert!(err.contains("spec_json"), "{err}");
    assert!(!dir.exists(), "a rejected spec must not create the directory");
}

#[test]
fn a_bad_session_reports_why() {
    let dir = out_dir("bad-session");
    let spec = format!(
        r#"{{"artist":"K","title":"T","year":2026,"month":9,"day":2,"outDir":{:?}}}"#,
        dir.to_string_lossy()
    );
    let err = export("{ not a session }", &spec).expect_err("must be rejected");
    assert!(err.contains("session_json"), "{err}");
}

#[test]
fn null_pointers_return_null_and_set_an_error() {
    // SAFETY: the contract explicitly accepts null.
    let raw = unsafe { kontinuum_export_masters(std::ptr::null(), std::ptr::null()) };
    assert!(raw.is_null());
    let err = kontinuum_last_error();
    assert!(!err.is_null());
    // SAFETY: bridge-owned buffer.
    let msg = unsafe { CStr::from_ptr(err) }.to_string_lossy().into_owned();
    assert!(msg.contains("null"), "{msg}");
}
