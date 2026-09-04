//! End-to-end critic contract tests (issue #25): a synthesized minimal-
//! techno-ish mix pushed through `CriticEngine` + `StemBoard`, verdict
//! flag behavior on planted good/bad mixes, the shipped targets fixture,
//! and the serde feed contract for #26/#15.

use kontinuum_analysis::{
    CriticEngine, CriticSnapshot, CriticTargets, CriticVerdict, StemBoard, StemId, BANDS,
};

const SR: u32 = 48_000;
const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/critic-targets.json");

fn sine(hz: f64, amp: f64, t: f64) -> f32 {
    (amp * (std::f64::consts::TAU * hz * t).sin()) as f32
}

/// One stem generator: kick = 2 Hz gated 60 Hz thump, bass = 55 Hz line
/// gated in sync with the kick (the planted kick↔bass mud case), perc =
/// 8 Hz 6 kHz ticks, pad = soft 220/277 Hz drone.
fn stem_signal(id: StemId, i: usize) -> f32 {
    let t = i as f64 / SR as f64;
    match id {
        StemId::Kick => {
            let tb = (t * 2.0).fract();
            if tb < 0.25 {
                (0.9 * (-tb * 30.0).exp() * (std::f64::consts::TAU * 60.0 * tb).sin()) as f32
            } else {
                0.0
            }
        }
        StemId::Bass => {
            let tb = (t * 2.0).fract();
            if tb < 0.5 {
                (0.5 * (-tb * 8.0).exp() * (std::f64::consts::TAU * 55.0 * tb).sin()) as f32
            } else {
                0.0
            }
        }
        StemId::Perc => {
            let on = (t * 8.0).fract() < 0.02;
            if on { sine(6000.0, 0.3, t) } else { 0.0 }
        }
        StemId::Pad => sine(220.0, 0.15, t) + sine(277.0, 0.1, t),
    }
}

/// Sum the stems into a master and feed both layers. `clip` crushes the
/// master to ±0.12 — the planted "over-limited" defect.
fn push_mix(seconds: f64, clip: bool) -> (CriticEngine, StemBoard) {
    let n = (SR as f64 * seconds) as usize;
    let mut master = vec![0.0f32; n];
    let mut stems = StemBoard::new(SR);
    for id in StemId::ALL {
        let block: Vec<f32> = (0..n).map(|i| stem_signal(id, i)).collect();
        stems.push_block(id, &block);
        for (m, s) in master.iter_mut().zip(block.iter()) {
            *m += s;
        }
    }
    if clip {
        for m in master.iter_mut() {
            *m = m.clamp(-0.12, 0.12);
        }
    }
    let mut engine = CriticEngine::new(SR);
    engine.push_block(&master);
    (engine, stems)
}

/// Targets derived from the measured clean snapshot: the floor/target of
/// every axis sits between the clean and the defective reading, so the
/// test pins the *decision boundary*, not magic synthetic numbers.
fn targets_around(s: &CriticSnapshot) -> CriticTargets {
    CriticTargets {
        version: 1,
        name: "test".into(),
        integrated_target_lufs: s.integrated_lufs,
        loudness_tolerance_lu: 2.5,
        crest_floor_db: s.crest_db - 2.0,
        crest_tolerance_db: 1.0,
        tilt_target_db_per_oct: s.tilt_db_per_oct,
        tilt_tolerance_db_per_oct: 2.0,
        sub_share_cap: s.sub_share + 0.1,
        sub_share_tolerance: 0.05,
    }
}

#[test]
fn good_mix_stays_quiet_on_every_axis() {
    let (engine, _) = push_mix(6.0, false);
    let s = engine.snapshot();
    let v = CriticVerdict::evaluate(&s, &targets_around(&s));
    assert_eq!(v.total(), 0.0, "clean mix must be on-target: {v:?}");
    assert!(!v.flags.any());
}

#[test]
fn clipped_mix_collapses_dynamics_and_the_verdict_notices() {
    let (clean_engine, _) = push_mix(6.0, false);
    let clean = clean_engine.snapshot();
    let (clipped_engine, _) = push_mix(6.0, true);
    let clipped = clipped_engine.snapshot();

    assert!(clipped.crest_db < clean.crest_db - 3.0,
        "clipping must crush crest: {} vs {}", clipped.crest_db, clean.crest_db);

    let mut targets = targets_around(&clean);
    targets.crest_floor_db = (clean.crest_db + clipped.crest_db) / 2.0;
    let v_bad = CriticVerdict::evaluate(&clipped, &targets);
    assert!(v_bad.flags.dynamics_collapsed, "clipped mix must flag: {:?}", v_bad.flags);
    assert!(v_bad.dynamics_score > 0.0);
    assert!(!CriticVerdict::evaluate(&clean, &targets).flags.dynamics_collapsed,
        "clean mix must stay quiet");
}

#[test]
fn loudness_axis_separates_quiet_and_hot_mixes() {
    let (engine, _) = push_mix(6.0, false);
    let s = engine.snapshot();
    let targets = targets_around(&s);

    let mut quiet = s;
    quiet.integrated_lufs = s.integrated_lufs - 6.0;
    let mut hot = s;
    hot.integrated_lufs = s.integrated_lufs + 6.0;
    let v_quiet = CriticVerdict::evaluate(&quiet, &targets);
    let v_hot = CriticVerdict::evaluate(&hot, &targets);
    assert!(v_quiet.flags.loudness_shortfall && !v_quiet.flags.loudness_excess);
    assert!(v_hot.flags.loudness_excess && !v_hot.flags.loudness_shortfall);
}

#[test]
fn stem_board_reads_planted_collision_and_sane_stem_stats() {
    let (_, stems) = push_mix(8.0, false);
    let s = stems.snapshot();
    // The generated mix fires kick and bass together in the same 55–60 Hz
    // range by design, so the collision index must sit well above the
    // time-disjoint / separated-spectra ceilings (< 0.2) proven in the
    // stems unit tests.
    assert!(s.bass_kick_collision > 0.4, "planted low collision {}", s.bass_kick_collision);
    assert!(s.bass_kick_collision <= 1.0);
    for st in &s.stems {
        assert!(st.short_term_lufs.is_finite() && st.centroid_hz.is_finite());
        assert!(st.transients_per_sec.is_finite());
    }
    let kick = s.stems[StemId::Kick.index()];
    let pad = s.stems[StemId::Pad.index()];
    assert!(kick.transients_per_sec > pad.transients_per_sec,
        "kick {} must out-transient the pad {}", kick.transients_per_sec, pad.transients_per_sec);
    assert!(kick.centroid_hz < pad.centroid_hz,
        "kick centroid {} below pad centroid {}", kick.centroid_hz, pad.centroid_hz);
}

#[test]
fn snapshots_round_trip_through_serde_for_the_feed_contract() {
    let (engine, stems) = push_mix(6.0, false);
    let master = engine.snapshot();
    let board = stems.snapshot();

    let master_text = serde_json::to_string(&master).expect("master serializes");
    let board_text = serde_json::to_string(&board).expect("stems serialize");
    let master_back: CriticSnapshot = serde_json::from_str(&master_text).expect("parses");
    // serde_json's default f64 formatting is shortest-repr, not bit-exact;
    // the feed contract needs value equality within 1 ulp, not bit equality.
    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() <= a.abs().max(b.abs()) * 1e-12 + 1e-15
    }
    assert!(near(master_back.momentary_lufs, master.momentary_lufs));
    assert!(near(master_back.integrated_lufs, master.integrated_lufs));
    assert!(near(master_back.crest_db, master.crest_db));
    assert!(near(master_back.tilt_db_per_oct, master.tilt_db_per_oct));
    assert!(near(master_back.centroid_hz, master.centroid_hz));
    assert!(near(master_back.sub_share, master.sub_share));
    assert!(near(master_back.width_db, master.width_db));
    assert!(near(master_back.true_peak_dbfs, master.true_peak_dbfs));
    assert_eq!(master_back.seconds, master.seconds);
    // Feed contract: finite fields only — `null`/inf would poison #26/#15.
    assert!(!master_text.contains("null") && !board_text.contains("null"));
}

#[test]
fn shipped_fixture_parses_and_pins_schema_version_1() {
    let targets = CriticTargets::load(std::path::Path::new(FIXTURE)).expect("fixture parses");
    assert_eq!(targets.version, 1, "schema version consumers pin against");
    assert!(targets.loudness_tolerance_lu > 0.0 && targets.crest_tolerance_db > 0.0);
    assert!(targets.sub_share_cap > 0.0 && targets.sub_share_cap < 1.0);
    assert!(targets.tilt_tolerance_db_per_oct > 0.0);
}

#[test]
fn fixture_crest_floor_matches_the_genre_profile() {
    // The critic's dynamics floor must agree with the #52 minimal-techno
    // profile so offline gating (#52) and live verdicts (#25) cannot drift.
    let targets = CriticTargets::load(std::path::Path::new(FIXTURE)).expect("fixture parses");
    let profile_path = format!(
        "{}/../../fixtures/profiles/minimal-techno.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let profile: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&profile_path).expect("genre profile readable"),
    )
    .expect("profile JSON");
    let crest_min = profile["targets"]["crest_db"]["min"].as_f64().expect("crest min");
    assert_eq!(targets.crest_floor_db, crest_min, "floors must agree across crates");
}

#[test]
fn sub_band_matches_the_shared_band_plan() {
    let sub = BANDS.iter().find(|(n, _, _)| *n == "sub").expect("sub band");
    assert_eq!((sub.1, sub.2), (20.0, 60.0), "critic sub band == BANDS sub band");
}
