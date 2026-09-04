// Regeneration helper for the per-genre golden fixtures (#87 convention):
// run with `cargo test --test golden_regenerate -- --ignored --nocapture`
// AFTER verifying the drift is intentional; overwrites fixtures in place.
#[test]
#[ignore]
fn regenerate_genre_fixtures() {
    for genre in [
        "minimal-techno",
        "techno",
        "deep-house",
        "house",
        "microhouse",
        "acid",
        "dub-techno",
        "ambient",
    ] {
        let fresh = kontinuum_compose::arrangement::generate_session(
            &kontinuum_compose::arrangement::GenParams {
                seed: 42,
                target_bars: 32,
                genre: Some(genre.replace('-', " ")),
                intensity: 0.75,
                ..Default::default()
            },
        );
        kontinuum_ir::validate_session(&fresh).unwrap_or_else(|e| panic!("{genre}: {e:?}"));
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/genres")
            .join(format!("{genre}.ir.json"));
        std::fs::write(&path, serde_json::to_string_pretty(&fresh).unwrap()).expect("write");
        println!("regenerated {genre}");
    }
}
