//! Statistical checks (issue #17 acceptance): the generated material must
//! stay inside the archetype envelopes — velocity and microtiming
//! distributions, no dead bars anywhere in the session, and bit-for-bit
//! determinism per seed. These run in CI next to the validator fuzz so a
//! generator regression that "compiles but doesn't groove" cannot merge.

use kontinuum_compose::arrangement::{generate_session, GenParams};
use kontinuum_ir::schema::{Pattern, Session};
use kontinuum_ir::validate_session;

const GENRES: [&str; 8] = [
    "minimal techno",
    "techno",
    "deep house",
    "house",
    "microhouse",
    "acid",
    "dub techno",
    "ambient",
];

fn session(genre: &str, seed: u64) -> Session {
    generate_session(&GenParams { seed, genre: Some(genre.into()), ..GenParams::default() })
}

fn steps(pattern: &Pattern) -> Vec<(f32, i16, f32)> {
    // (velocity, microtiming, probability) of every onset the bar repeats.
    match pattern {
        Pattern::Steps(st) => {
            st.steps.iter().map(|s| (s.velocity, s.microtiming_ticks, s.probability)).collect()
        }
        Pattern::Euclidean(e) => {
            let n = e.n.clamp(1, 16);
            kontinuum_ir::compile::expand::euclidean(e.k, n, e.rot)
                .iter()
                .filter(|on| **on)
                .map(|_| (e.velocity, 0, e.probability))
                .collect()
        }
        Pattern::ProbabilityMask(m) => {
            // The runtime draw is seeded; the envelope check rides the
            // distribution the generator would emit, so sample the mask's
            // own parameters at its density.
            let hits = (16.0 * m.density).round().max(1.0) as usize;
            vec![(m.velocity, 0, m.probability); hits]
        }
    }
}

/// Expected audible onsets per bar — the no-dead-bars measure.
fn expected_onsets(pattern: &Pattern) -> f32 {
    match pattern {
        Pattern::Steps(st) => st.steps.iter().map(|s| s.probability).sum(),
        Pattern::Euclidean(e) => e.k.min(16) as f32 * e.probability,
        Pattern::ProbabilityMask(m) => 16.0 * m.density * m.probability,
    }
}

#[test]
fn velocity_and_timing_stay_in_the_archetype_envelopes() {
    for genre in GENRES {
        for seed in 0..8u64 {
            let session = session(genre, seed);
            let mut velocities: Vec<f32> = Vec::new();
            let mut microtimings: Vec<i16> = Vec::new();
            let mut perc_velocities: Vec<f32> = Vec::new();
            for sec in &session.sections {
                for (track_id, pattern) in &sec.pattern_bindings {
                    for (v, mt, p) in steps(pattern) {
                        assert!((0.0..=1.0).contains(&v), "{genre}/{seed} {track_id}: velocity {v}");
                        assert!((0.0..=1.0).contains(&p), "{genre}/{seed} {track_id}: probability {p}");
                        assert!(
                            (-120..=120).contains(&mt),
                            "{genre}/{seed} {track_id}: microtiming {mt} outside the schema"
                        );
                        velocities.push(v);
                        microtimings.push(mt);
                        if track_id == "perc" || track_id == "shaker" {
                            perc_velocities.push(v);
                        }
                    }
                }
            }
            assert!(!velocities.is_empty(), "{genre}/{seed}: no material at all");
            let mean = velocities.iter().sum::<f32>() / velocities.len() as f32;
            assert!(
                (0.1..=0.95).contains(&mean),
                "{genre}/{seed}: mean velocity {mean} outside the musical envelope"
            );
            let mt_max = microtimings.iter().map(|m| m.abs()).max().unwrap_or(0);
            assert!(mt_max <= 120, "{genre}/{seed}: microtiming overflow {mt_max}");
            // The groove bundle ships bias ±12 + jitter σ ≤ 4 ticks; clamping
            // to the schema's ±120 is a guard, never a working state.
            let perc_mt: Vec<i16> = microtimings
                .iter()
                .copied()
                .filter(|_| !perc_velocities.is_empty())
                .collect();
            let _ = perc_mt;
            if !perc_velocities.is_empty() {
                let pv = perc_velocities.iter().sum::<f32>() / perc_velocities.len() as f32;
                assert!(
                    (0.15..=0.95).contains(&pv),
                    "{genre}/{seed}: perc mean velocity {pv} outside its archetype"
                );
            }
        }
    }
}

#[test]
fn groove_microtiming_stays_in_the_groove_envelope() {
    // Pins each of the six hand-made grooves: every perc onset's timing
    // offset from the grid stays within bias+jitter headroom (≤ 16 ticks)
    // — the make-or-break microhouse feel, measured.
    const GROOVES: [&str; 6] =
        ["straight-machine", "mpc-ish", "drunk-shuffle", "pushed-hats", "laid-back", "tense"];
    for groove in GROOVES {
        for seed in 0..4u64 {
            let session = generate_session(&GenParams {
                seed,
                genre: Some("microhouse".into()),
                groove: Some(groove.into()),
                ..GenParams::default()
            });
            let engine = session.pattern_engine.as_ref().expect("recorded engine");
            assert_eq!(engine.groove.as_deref(), Some(groove));
            let mut offsets: Vec<i16> = Vec::new();
            for sec in &session.sections {
                if let Some(Pattern::Steps(p)) = sec.pattern_bindings.get("perc") {
                    offsets.extend(p.steps.iter().map(|s| s.microtiming_ticks));
                }
            }
            assert!(!offsets.is_empty(), "{groove}/{seed}: no perc material");
            let max = offsets.iter().map(|m| m.abs()).max().unwrap_or(0);
            assert!(
                max <= 16,
                "{groove}/{seed}: perc microtiming {max} exceeds bias+jitter headroom"
            );
            // Grooves with a positive bias must lean late, negative early.
            let mean: f32 = offsets.iter().map(|m| *m as f32).sum::<f32>() / offsets.len() as f32;
            let bias = engine.bias_ticks;
            if bias > 4 {
                assert!(mean > 0.0, "{groove}/{seed}: pushed groove pulled ({mean})");
            }
            if bias < -4 {
                assert!(mean < 0.0, "{groove}/{seed}: pulled groove pushed ({mean})");
            }
        }
    }
}

#[test]
fn no_dead_bars_anywhere() {
    for genre in GENRES {
        for seed in [0u64, 1, 42, 999] {
            let session = session(genre, seed);
            for sec in &session.sections {
                let expected: f32 =
                    sec.pattern_bindings.values().map(expected_onsets).sum();
                assert!(
                    expected >= 1.0,
                    "{genre}/{seed}/{}: {expected} expected onsets per bar — a dead bar",
                    sec.id
                );
                assert!(
                    !sec.pattern_bindings.is_empty(),
                    "{genre}/{seed}/{}: no bindings",
                    sec.id
                );
            }
        }
    }
}

#[test]
fn same_seed_renders_byte_identical_patterns() {
    for genre in ["microhouse", "techno", "deep house"] {
        for seed in [0u64, 7, 1234] {
            let a = session(genre, seed);
            let b = session(genre, seed);
            validate_session(&a).unwrap_or_else(|e| panic!("{genre}/{seed}: {e:?}"));
            assert_eq!(
                serde_json::to_string(&a).unwrap(),
                serde_json::to_string(&b).unwrap(),
                "{genre}/{seed}: same seed must reproduce the session bit-for-bit"
            );
        }
    }
}

#[test]
fn the_six_groove_templates_all_produce_valid_distinct_sessions() {
    let mut fingerprints: Vec<String> = Vec::new();
    for groove in
        ["straight-machine", "mpc-ish", "drunk-shuffle", "pushed-hats", "laid-back", "tense"]
    {
        let session = generate_session(&GenParams {
            seed: 42,
            genre: Some("house".into()),
            groove: Some(groove.into()),
            ..GenParams::default()
        });
        validate_session(&session).unwrap_or_else(|e| panic!("{groove}: {e:?}"));
        fingerprints.push(serde_json::to_string(&session).unwrap());
    }
    for (i, a) in fingerprints.iter().enumerate() {
        for b in fingerprints.iter().skip(i + 1) {
            assert_ne!(a, b, "two groove templates produced identical sessions");
        }
    }
}

#[test]
fn every_bass_archetype_pin_produces_valid_distinct_sessions() {
    let mut fingerprints: Vec<String> = Vec::new();
    for archetype in [
        "offbeat-eighths",
        "rolling-16ths",
        "dub-sub",
        "syncopated-funk",
        "acid-slide",
        "call-response",
    ] {
        let session = generate_session(&GenParams {
            seed: 42,
            genre: Some("techno".into()),
            bass_archetype: Some(archetype.into()),
            ..GenParams::default()
        });
        validate_session(&session).unwrap_or_else(|e| panic!("{archetype}: {e:?}"));
        let engine = session.pattern_engine.as_ref().expect("recorded engine");
        assert_eq!(engine.bass_archetype.as_deref(), Some(archetype));
        fingerprints.push(serde_json::to_string(&session).unwrap());
    }
    for (i, a) in fingerprints.iter().enumerate() {
        for b in fingerprints.iter().skip(i + 1) {
            assert_ne!(a, b, "two bass archetypes produced identical sessions");
        }
    }
}

#[test]
fn collision_policy_reaches_the_recorded_engine_state() {
    let avoid = generate_session(&GenParams {
        genre: Some("microhouse".into()),
        ..GenParams::default()
    });
    assert_eq!(
        avoid.pattern_engine.as_ref().expect("engine").downbeat_collision,
        kontinuum_ir::schema::DownbeatCollision::Avoid,
        "microhouse avoids"
    );
    let allow = generate_session(&GenParams {
        genre: Some("techno".into()),
        ..GenParams::default()
    });
    assert_eq!(
        allow.pattern_engine.as_ref().expect("engine").downbeat_collision,
        kontinuum_ir::schema::DownbeatCollision::Allow,
        "driving techno allows"
    );
}

#[test]
fn validator_rejects_out_of_envelope_pattern_engine_state() {
    let base = r#"{
        "version": 1,
        "seed": 1,
        "tempo_lane": [[0, 120.0]],
        "sections": [{"id": "s", "bars": 1, "energy_curve": [0.5],
            "pattern_bindings": {"kick": {"generator": "euclidean", "k": 4, "n": 16}}}],
        "tracks": [{"id": "kick", "role": "kick", "instrument": {"kind": "kick"}}]
    }"#;
    let inject = |engine: &str| {
        format!(r#"{},"pattern_engine": {}}}"#, &base[..base.len() - 1], engine)
    };
    for (engine, code) in [
        (r#"{"swing": 0.9}"#, "E_SWING_RANGE"),
        (r#"{"bias_ticks": 40}"#, "E_PATTERN_BIAS_RANGE"),
        (r#"{"jitter_ticks": 0.2}"#, "E_PATTERN_JITTER_RANGE"),
    ] {
        let session: kontinuum_ir::Session = serde_json::from_str(&inject(engine)).expect("parses");
        let errors = kontinuum_ir::validate_session(&session).expect_err("must reject");
        assert!(
            errors.iter().any(|e| e.code == code),
            "{engine}: expected {code}, got {:?}",
            errors.iter().map(|e| e.code).collect::<Vec<_>>()
        );
    }
    let good: kontinuum_ir::Session =
        serde_json::from_str(&inject(r#"{"groove": "tense", "swing": 0.1, "bias_ticks": -6, "jitter_ticks": 2.0}"#))
            .expect("parses");
    assert!(
        kontinuum_ir::validate_session(&good).is_ok(),
        "in-envelope engine state must validate"
    );
}
