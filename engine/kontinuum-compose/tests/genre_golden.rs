//! Per-genre golden fixtures (issue #87): every app genre has a committed
//! session snapshot at a pinned seed, and regeneration must reproduce it.
//!
//! Session JSON is fully deterministic (seeded integer RNG, IEEE arithmetic,
//! serde's deterministic float formatting), so this compare is portable —
//! unlike the render-path golden hash in `kontinuum-offline`, which is
//! host/toolchain-canonical. A change here means the style's identity moved:
//! update the fixture only with intent, and note what changed musically.

use kontinuum_ir::{validate_session, Session};

const GENRES: [&str; 8] = [
    "minimal-techno",
    "techno",
    "deep-house",
    "house",
    "microhouse",
    "acid",
    "dub-techno",
    "ambient",
];

const SEED: u64 = 42;
const BARS: u32 = 32;

fn fixture_path(genre: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/genres")
        .join(format!("{genre}.ir.json"))
}

fn regenerate(genre: &str) -> Session {
    kontinuum_compose::arrangement::generate_session(&kontinuum_compose::arrangement::GenParams {
        seed: SEED,
        target_bars: BARS,
        genre: Some(genre.replace('-', " ")),
        intensity: 0.75,
        ..kontinuum_compose::arrangement::GenParams::default()
    })
}

#[test]
fn every_genre_fixture_validates_and_regenerates_identically() {
    for genre in GENRES {
        let path = fixture_path(genre);
        let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{genre}: {e}"));
        let committed: Session = serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{genre}: {e}"));
        validate_session(&committed).unwrap_or_else(|e| panic!("{genre}: {e:?}"));

        let fresh = regenerate(genre);
        validate_session(&fresh).unwrap_or_else(|e| panic!("{genre}: {e:?}"));
        // Canonical round-trip compare: robust to formatting, exact on content.
        assert_eq!(
            serde_json::to_string(&committed).unwrap(),
            serde_json::to_string(&fresh).unwrap(),
            "{genre}: regenerated session drifted from its golden fixture — \
             if the style's identity changed deliberately, re-commit the fixture"
        );
    }
}

#[test]
fn every_genre_fixture_carries_its_identity() {
    for genre in GENRES {
        let raw = std::fs::read_to_string(fixture_path(genre)).unwrap();
        let session: Session = serde_json::from_str(&raw).unwrap();
        assert_eq!(session.seed, SEED);
        assert!(session.key.is_some(), "{genre}: no key");
        assert!(!session.tracks.is_empty(), "{genre}: no rack");
        if genre == "ambient" {
            assert!(
                !session.tracks.iter().any(|t| t.role == kontinuum_ir::TrackRole::Kick),
                "ambient must stay beatless"
            );
        }
    }
}
