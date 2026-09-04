use kontinuum_ir::{compile_session, validate_session, Session};
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

#[test]
fn main_fixture_validates_and_compiles() {
    let raw = std::fs::read_to_string(fixtures_dir().join("loop-4track.ir.json")).unwrap();
    let session: Session = serde_json::from_str(&raw).unwrap();
    validate_session(&session).expect("main fixture must validate");
    let blocks = compile_session(&session, 48_000).expect("main fixture must compile");
    assert_eq!(blocks.len(), 4);
    assert!(blocks.windows(2).all(|w| w[0].start_bar < w[1].start_bar));
}

#[test]
fn adversarial_fixtures_all_rejected() {
    let dir = fixtures_dir().join("adversarial");
    let mut count = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        count += 1;
        let raw = std::fs::read_to_string(&path).unwrap();
        let rejected = match serde_json::from_str::<Session>(&raw) {
            Ok(session) => validate_session(&session).is_err(),
            Err(_) => true,
        };
        assert!(rejected, "fixture not rejected: {}", path.display());
    }
    assert!(count >= 20, "expected >=20 adversarial fixtures, found {count}");
}
