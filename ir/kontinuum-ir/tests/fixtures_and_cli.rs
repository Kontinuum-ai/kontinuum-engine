//! Integration: golden fixture, adversarial corpus, and CLI surface.

use std::process::Command;

use kontinuum_ir::{compile_session_summary, validate_session, Session};

const MANIFEST: &str = env!("CARGO_MANIFEST_DIR");
const ADVERSARIAL: &str = "fixtures/adversarial";

fn fixture_path(rel: &str) -> String {
    format!("{MANIFEST}/fixtures/{rel}")
}

#[test]
fn golden_fixture_validates_and_compiles() {
    let text = std::fs::read_to_string(fixture_path("loop-4track.ir.json")).expect("fixture");
    let session: Session = serde_json::from_str(&text).expect("golden fixture parses");
    validate_session(&session).expect("golden fixture must validate clean");

    let summary = compile_session_summary(&session, 48_000).expect("golden fixture compiles");
    assert_eq!(summary.blocks, 4, "16 bars in 4-bar blocks");
    assert!(summary.events_total > 0);
    assert!(summary.events_total < 2000 * 4, "keep blocks lean");
    assert!(summary.cpu_estimate > 0.0 && summary.cpu_estimate < 100.0);
}

#[test]
fn every_adversarial_fixture_is_rejected_without_panicking() {
    let mut count = 0;
    for entry in std::fs::read_dir(format!("{MANIFEST}/{ADVERSARIAL}")).expect("corpus dir") {
        let path = entry.expect("entry").path();
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("json"));
        let text = std::fs::read_to_string(&path).expect("read fixture");
        let parsed: Result<Session, _> = serde_json::from_str(&text);
        match parsed {
            Err(_) => { /* parse-time rejection is sufficient */ }
            Ok(session) => {
                let verdict = validate_session(&session);
                assert!(
                    verdict.is_err(),
                    "{} must be rejected, got Ok",
                    path.display()
                );
                for e in verdict.expect_err("checked above") {
                    assert!(!e.code.is_empty());
                    assert!(!e.message.is_empty());
                    assert!(!e.suggested_fix.is_empty());
                }
            }
        }
        count += 1;
    }
    assert!(count >= 20, "expected the full adversarial corpus, got {count}");
}

fn cli(args: &[&str]) -> (Option<i32>, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_kontinuum-ir"))
        .args(args)
        .current_dir(MANIFEST)
        .output()
        .expect("cli runs");
    let code = out.status.code();
    (code, String::from_utf8_lossy(&out.stdout).into_owned())
}

#[test]
fn cli_validate_and_compile_smoke() {
    let (code, out) = cli(&["validate", "fixtures/loop-4track.ir.json"]);
    assert_eq!(code, Some(0));
    assert!(out.contains(r#""ok":true"#) || out.contains(r#""ok": true"#), "{out}");

    let (code, out) = cli(&["validate", "fixtures/adversarial/10_velocity_5.json"]);
    assert_eq!(code, Some(1));
    assert!(out.contains("E_VELOCITY_RANGE"), "{out}");

    let (code, out) = cli(&["compile", "fixtures/loop-4track.ir.json"]);
    assert_eq!(code, Some(0));
    let v: serde_json::Value = serde_json::from_str(&out).expect("JSON summary");
    assert_eq!(v["blocks"], 4);
    assert!(v["events_total"].as_u64().expect("events") > 0);
    assert!(v["cpu_estimate"].as_f64().expect("cpu") > 0.0);
}

#[test]
fn cli_diff_apply_schema_and_render() {
    let diff_path = std::env::temp_dir().join("kontinuum-ir-test-diff.json");
    std::fs::write(
        &diff_path,
        r#"{"op":"replace_pattern","section":"b_groove","track":"kick",
            "pattern":{"generator":"euclidean","k":8,"n":16,"rot":0}}"#,
    )
    .expect("write diff");
    let diff = diff_path.to_str().expect("utf8");

    let (code, out) = cli(&["diff-apply", "fixtures/loop-4track.ir.json", diff, "0"]);
    assert_eq!(code, Some(0), "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("JSON report");
    assert!(!v["applied"].as_array().expect("applied").is_empty());
    assert!(v["session"]["sections"].is_array());

    let (code, _) = cli(&["diff-apply", "fixtures/loop-4track.ir.json", diff, "99999999999"]);
    assert_eq!(code, Some(1), "invalid at_bar must fail cleanly");

    // Render delegates to the sibling kontinuum-offline binary: exits 0 when
    // it is present, 1 with an actionable message when it is not. The old
    // placeholder (exit 2) must stay gone.
    let (code, out) = cli(&["render", "fixtures/loop-4track.ir.json", "out.wav"]);
    assert_ne!(code, Some(2), "render placeholder returned: {out}");
    assert!(code == Some(0) || code == Some(1), "render exit {code:?}: {out}");

    let (code, out) = cli(&["schema"]);
    assert_eq!(code, Some(0));
    let v: serde_json::Value = serde_json::from_str(&out).expect("schema is JSON");
    assert_eq!(v["type"], "object");

    let (code, _) = cli(&["bogus-command"]);
    assert_eq!(code, Some(1));
}
