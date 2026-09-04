//! Generate a session JSON for a style.
//!
//! usage: gen_genre <genre> <out.json> [seed] [bars] [bpm]
//!
//! Tempo is omitted by default so the genre's own BPM applies; pass one only
//! to override it.
use kontinuum_compose::arrangement::GenParams;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let genre = args.get(1).cloned().unwrap_or_else(|| "techno".into());
    let out = args.get(2).cloned().unwrap_or_else(|| "session.json".into());
    let seed: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(7);
    let bars: u32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(32);
    let bpm: Option<f64> = args.get(5).and_then(|s| s.parse().ok());
    let params = GenParams {
        seed,
        target_bars: bars,
        bpm,
        intensity: 0.75,
        genre: Some(genre.clone()),
        ..GenParams::default()
    };
    let session = kontinuum_compose::arrangement::generate_session(&params);
    kontinuum_ir::validate_session(&session).expect("generated session must validate");
    std::fs::write(&out, serde_json::to_string_pretty(&session).unwrap()).unwrap();
    let tempo = session.tempo_lane.first().map(|(_, b)| *b).unwrap_or_default();
    eprintln!("wrote {out} ({} bars, {genre}, {tempo:.0} BPM)", session.total_bars());
}
