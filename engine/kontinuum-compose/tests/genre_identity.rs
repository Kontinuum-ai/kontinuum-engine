//! Genre identity acceptance gate (issue #87): the eight app genres must be
//! structurally distinguishable at the same seed — no two may ever produce
//! byte-identical sessions, and every pair must differ in more than one
//! structural dimension. On main @662170f acid and ambient produced the same
//! bytes and techno/dub-techno collapsed the same way; this gate makes that
//! regression class unmergeable.

use kontinuum_compose::arrangement::{generate_session, GenParams};
use kontinuum_ir::schema::{Pattern, Session};
use kontinuum_ir::validate_session;

/// The genre strip in `shared/Kontinuum/ContentView.swift`, in order. The two
/// lists must grow together.
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

/// Seeds the pairwise gate sweeps. Same seed, different genre: the exact
/// comparison under which the old specs collapsed.
const SEEDS: [u64; 8] = [42, 7, 1, 99, 1234, 31337, 5555, 777];

fn session(genre: &str, seed: u64) -> Session {
    let s = generate_session(&GenParams {
        seed,
        genre: Some(genre.into()),
        ..GenParams::default()
    });
    validate_session(&s).unwrap_or_else(|e| panic!("{genre} seed {seed}: {e:?}"));
    s
}

/// Structural fingerprint: the named, human-readable dimensions two sessions
/// can differ in — tempo, key, rack, and per-track event volume across the
/// arrangement. The pairwise distance is the size of the symmetric
/// difference of these components.
fn fingerprint(s: &Session) -> Vec<String> {
    let mut parts = vec![
        format!("tempo={:.1}", s.tempo_lane[0].1),
        format!("key={}", s.key.as_deref().unwrap_or("")),
        format!(
            "rack={}",
            s.tracks
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>()
                .join(",")
        ),
    ];
    let mut onsets: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for sec in &s.sections {
        for (id, pat) in &sec.pattern_bindings {
            let n = match pat {
                Pattern::Steps(st) => st.steps.len() * st.repeats.max(1) as usize,
                Pattern::Euclidean(e) => e.k as usize * e.repeats.max(1) as usize,
                Pattern::ProbabilityMask(m) => {
                    (m.density * 16.0).round() as usize * m.repeats.max(1) as usize
                }
            };
            *onsets.entry(id.as_str()).or_default() += n;
        }
    }
    for (id, n) in onsets {
        parts.push(format!("onsets/{id}={n}"));
    }
    parts
}

fn distance(a: &[String], b: &[String]) -> usize {
    let mut count: std::collections::BTreeMap<&String, i64> = std::collections::BTreeMap::new();
    for p in a {
        *count.entry(p).or_default() += 1;
    }
    for p in b {
        *count.entry(p).or_default() -= 1;
    }
    count.values().map(|v| v.unsigned_abs() as usize).sum()
}

#[test]
fn no_two_genres_are_byte_identical_at_the_same_seed() {
    // Issue #87's smoking gun, as a permanent gate: acid vs ambient was the
    // measured 5f0905f6… collision.
    for seed in SEEDS {
        for (i, a) in APP_GENRES.iter().enumerate() {
            for b in APP_GENRES.iter().skip(i + 1) {
                let ja = serde_json::to_string(&session(a, seed)).unwrap();
                let jb = serde_json::to_string(&session(b, seed)).unwrap();
                assert_ne!(ja, jb, "{a:?} and {b:?} are byte-identical at seed {seed}");
            }
        }
    }
}

/// Structural difference floor: two genres that differed only in, say, one
/// onset count would pass a byte-compare while sounding like the same
/// record. Every pair must differ in at least MIN_DISTANCE structural
/// components (rack + tempo alone usually exceed this).
#[test]
fn every_genre_pair_exceeds_the_structural_distance_floor() {
    const MIN_DISTANCE: usize = 3;
    for seed in SEEDS {
        for (i, a) in APP_GENRES.iter().enumerate() {
            for b in APP_GENRES.iter().skip(i + 1) {
                let (fa, fb) = (fingerprint(&session(a, seed)), fingerprint(&session(b, seed)));
                let d = distance(&fa, &fb);
                assert!(
                    d >= MIN_DISTANCE,
                    "{a:?} vs {b:?} at seed {seed}: structural distance {d} < {MIN_DISTANCE} \
                     ({fa:?} vs {fb:?})"
                );
            }
        }
    }
}

/// The spec's key tendencies and rack actually steer generation: every
/// session sits in one of its style's keys, and the beatless style never
/// grows a kick wherever the seed lands.
#[test]
fn key_tendencies_and_racks_steer_generation() {
    for seed in SEEDS {
        let expected: [(&str, [&str; 3]); 8] = [
            ("minimal techno", ["F minor", "G minor", "—"]),
            ("techno", ["G minor", "A minor", "F minor"]),
            ("deep house", ["D minor", "A minor", "G minor"]),
            ("house", ["A minor", "C minor", "F minor"]),
            ("microhouse", ["F minor", "E minor", "G minor"]),
            ("acid", ["F minor", "G minor", "C minor"]),
            ("dub techno", ["C minor", "G minor", "F minor"]),
            ("ambient", ["A minor", "D minor", "E minor"]),
        ];
        for (genre, keys) in expected {
            let s = session(genre, seed);
            let key = s.key.as_deref().expect("genre session carries its key");
            assert!(
                keys.contains(&key),
                "{genre} seed {seed}: key {key:?} outside the style's tendencies {keys:?}"
            );
        }
        // Ambient is beatless — no kick track, no kick bindings, ever.
        let ambient = session("ambient", seed);
        assert!(!ambient.tracks.iter().any(|t| t.role == kontinuum_ir::TrackRole::Kick));
        assert!(
            ambient
                .sections
                .iter()
                .flat_map(|s| s.pattern_bindings.keys())
                .all(|id| id != "kick"),
            "ambient bound a kick at seed {seed}"
        );
    }
}
