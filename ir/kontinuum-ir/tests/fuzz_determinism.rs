//! Integration: compiler determinism (double-compile equality + 10 random
//! valid sessions) and the robustness fuzz (2000 mutations, never panic).

use kontinuum_clock::Rng;
use kontinuum_ir::{compile_session, validate_session, Session};

const MANIFEST: &str = env!("CARGO_MANIFEST_DIR");

/// Builds a random *valid* session document; everything is inside the schema
/// bounds by construction.
fn random_valid_session(rng: &mut Rng) -> Session {
    let bpm = 90.0 + rng.next_f32() * 50.0;
    let track_specs: [(&str, &str, &str); 5] = [
        ("kick", "kick", r#"{"kind":"kick","tune_hz":48.0,"decay_ms":300.0}"#),
        ("hat", "perc", r#"{"kind":"hat","decay_ms":60.0,"tone":0.6}"#),
        ("bass", "bass", r#"{"kind":"bass","cutoff_hz":800.0,"resonance":0.2}"#),
        ("pad", "pad", r#"{"kind":"pad","attack_ms":500.0,"release_ms":900.0}"#),
        ("perc2", "perc", r#"{"kind":"hat","decay_ms":120.0,"tone":0.4}"#),
    ];
    let n_tracks = 1 + (rng.below(4) as usize);
    let mut sections = String::new();
    let n_sections = 1 + rng.below(3) as usize;
    for si in 0..n_sections {
        let bars = 1 + rng.below(8) as u32;
        let mut bindings = String::new();
        for (tid, _, _) in track_specs.iter().take(n_tracks) {
            let pattern = if rng.chance(0.5) {
                let k = 1 + rng.below(8) as u32;
                let n = if rng.chance(0.5) { 8 } else { 16 };
                format!(r#""{tid}": {{"generator":"euclidean","k":{k},"n":{n},"rot":0,"velocity":0.7}}"#)
            } else {
                let n_steps = 1 + rng.below(8) as u32;
                let steps: Vec<String> = (0..n_steps)
                    .map(|j| {
                        format!(
                            r#"{{"position":{},"velocity":0.6,"probability":1.0}}"#,
                            j * 480
                        )
                    })
                    .collect();
                format!(r#""{tid}": {{"steps":[{}]}}"#, steps.join(","))
            };
            if !bindings.is_empty() {
                bindings.push(',');
            }
            bindings.push_str(&pattern);
        }
        if !sections.is_empty() {
            sections.push(',');
        }
        sections.push_str(&format!(
            r#"{{"id":"s{si}","bars":{bars},"energy_curve":[0.5],"pattern_bindings":{{{bindings}}}}}"#
        ));
    }
    let tracks: Vec<String> = track_specs
        .iter()
        .take(n_tracks)
        .map(|(tid, role, inst)| {
            format!(r#"{{"id":"{tid}","role":"{role}","instrument":{inst}}}"#)
        })
        .collect();
    let doc = format!(
        r#"{{"version":1,"seed":{},"tempo_lane":[[0,{bpm}]],"sections":[{sections}],"tracks":[{}]}}"#,
        rng.next_u64(),
        tracks.join(",")
    );
    serde_json::from_str(&doc).expect("generator emits valid JSON")
}

#[test]
fn compile_is_deterministic_across_random_valid_sessions() {
    for seed in 0..10u64 {
        let session = random_valid_session(&mut Rng::from_seed(seed));
        validate_session(&session)
            .unwrap_or_else(|e| panic!("generated session {seed} must validate: {e:?}"));
        let a = compile_session(&session, 48_000).expect("compiles");
        let b = compile_session(&session, 48_000).expect("compiles");
        assert_eq!(
            format!("{a:?}"),
            format!("{b:?}"),
            "session {seed}: same input must give identical blocks"
        );
    }
}

#[test]
fn fuzz_2000_mutations_of_golden_fixture_never_panic() {
    let path = format!("{MANIFEST}/fixtures/loop-4track.ir.json");
    let original = std::fs::read(&path).expect("fixture");
    let mut rng = Rng::from_seed(0xC0FFEE);
    let mut parse_ok = 0usize;
    for round in 0..2000u32 {
        let mut bytes = original.clone();
        let mutations = 1 + rng.below(4);
        for _ in 0..mutations {
            if bytes.is_empty() {
                // Fully truncated: start the next mutant from the original.
                bytes = original.clone();
                continue;
            }
            match rng.below(3) {
                0 => {
                    // Byte flip.
                    let i = rng.below(bytes.len() as u64) as usize;
                    bytes[i] ^= 1 + rng.below(255) as u8;
                }
                1 => {
                    // Truncation.
                    let cut = rng.below(bytes.len() as u64) as usize;
                    bytes.truncate(cut);
                }
                _ => {
                    // Junk injection.
                    let junk = [b'{', b'}', b'[', b']', b'"', b'\\', b':', b',', b'n', b'a', b'z', b'X', 0x00, 0xff];
                    let i = rng.below(bytes.len() as u64) as usize;
                    bytes[i] = junk[rng.below(junk.len() as u64) as usize];
                }
            }
        }
        let parsed: Result<Session, _> = serde_json::from_slice(&bytes);
        if let Ok(session) = parsed {
            parse_ok += 1;
            // Must not panic; either verdict is acceptable for a mutant.
            let _ = validate_session(&session);
        }
        assert!(
            bytes.len() <= original.len(),
            "mutation {round}: truncation must never grow the input"
        );
    }
    assert!(parse_ok > 0, "fuzz should still parse some mutants");
}
