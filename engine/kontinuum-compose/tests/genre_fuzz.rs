//! Genre fuzz gate (issue #86): every genre the app can tap, across many
//! seeds, must produce a session the engine's own validator accepts. This
//! drives the same path the macOS app does — a taste profile naming the
//! genre, `session_from_taste`, then `validate_session`, the exact sequence
//! `kontinuum_generate_session_from_taste` runs in the bridge — so a session
//! that would die on a user's tap cannot merge.

use kontinuum_compose::taste::{session_from_taste, TasteProfile};
use kontinuum_ir::validate_session;

/// The genre strip in `macos/Kontinuum/ContentView.swift`, in order. The two
/// lists must grow together: a chip the app can tap is a chip this gate
/// fuzzes.
const APP_GENRES: [&str; 8] = [
    "minimal techno",
    "techno",
    "deep house",
    "house",
    "microhouse",
    "acid",
    "dub techno",
    "ambient",
];

fn profile(genre: &str) -> TasteProfile {
    TasteProfile { genres: vec![genre.to_string()], ..TasteProfile::default() }
}

#[test]
fn every_app_genre_validates_across_64_seeds() {
    for genre in APP_GENRES {
        for seed in 0..64u64 {
            let session = session_from_taste(&profile(genre), seed);
            validate_session(&session).unwrap_or_else(|e| panic!("{genre} seed {seed}: {e:?}"));
        }
    }
}

#[test]
fn issue_86_seed_table_validates_for_the_house_family() {
    // The exact seeds the issue was measured on. The house family drew the
    // failures (deep house 5/10 at the time); the rest of the strip is
    // covered by the sweep above.
    for genre in ["house", "deep house", "microhouse"] {
        for seed in [1u64, 2, 3, 42, 99, 777, 1234, 5555, 31337, 424242] {
            let session = session_from_taste(&profile(genre), seed);
            validate_session(&session).unwrap_or_else(|e| panic!("{genre} seed {seed}: {e:?}"));
        }
    }
}
