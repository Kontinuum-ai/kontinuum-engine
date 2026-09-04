use kontinuum_compose::arrangement::{generate_session, GenParams};

const GENRES: [&str; 8] = [
    "minimal-techno", "techno", "deep-house", "house",
    "microhouse", "acid", "dub-techno", "ambient",
];

fn main() {
    let root = std::env::args().nth(1).expect("repo root arg");
    for genre in GENRES {
        let session = generate_session(&GenParams {
            seed: 42,
            target_bars: 32,
            genre: Some(genre.replace('-', " ")),
            intensity: 0.75,
            ..GenParams::default()
        });
        kontinuum_ir::validate_session(&session).expect("generated session must validate");
        let path = format!("{root}/fixtures/genres/{genre}.ir.json");
        std::fs::write(&path, serde_json::to_string_pretty(&session).unwrap()).unwrap();
        println!("regenerated {genre}");
    }
}
