//! The #23 end-to-end harness, on the synthetic corpus where the truth is
//! known by construction: render → analyze → segmentation-F1 gate → fit →
//! shipped artifacts → loaded by the REAL #16/#17 consumers with zero
//! code change → sampled arrangements match corpus statistics.
//!
//! This is the scaled-down stand-in for the issue's full validation
//! (which needs the purchased corpus + human annotations); every number
//! here proves pipeline shape, not musical truth.

use kontinuum_analysis::corpus::analyze_track;
use kontinuum_analysis::synthgen::{self, render, PRESETS};
use kontinuum_compose::arrangement::Kind;
use kontinuum_compose::groove::GrooveBank;
use kontinuum_compose::structure::StructureParams;
use kontinuum_compose::{generate_session, GenParams};
use kontinuum_corpus::{
    boundary_f1, fit_subgenre, sample_arrangement, write_artifacts, SEGMENTATION_F1_GATE,
};
use std::collections::BTreeMap;

const NOTE_NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

fn analyzed() -> Vec<(usize, kontinuum_analysis::TrackAnalysis)> {
    PRESETS
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let mono = render(p);
            let a = analyze_track(p.track_id, p.subgenre, &mono, synthgen::SYNTH_SAMPLE_RATE, p.bpm)
                .unwrap_or_else(|e| panic!("{} analyzes: {e}", p.id));
            (i, a)
        })
        .collect()
}

/// Tempo/key/segmentation/groove recovery per fixture track.
#[test]
fn detectors_recover_the_planted_truth() {
    let all = analyzed();
    let mut f1_sum = 0.0f64;
    for (i, a) in &all {
        let p = &PRESETS[*i];
        assert!(
            (f64::from(a.observation.bpm) - p.bpm).abs() <= 0.5,
            "{}: bpm {} vs planted {}",
            p.id,
            a.observation.bpm,
            p.bpm
        );
        assert_eq!(
            a.observation.key,
            format!("{} minor", NOTE_NAMES[p.root_pc as usize]),
            "{}: key {} vs planted {} minor",
            p.id,
            a.observation.key,
            NOTE_NAMES[p.root_pc as usize]
        );
        let truth = synthgen::planted_annotation(p);
        let scores = boundary_f1(&a.boundary_bars, &truth);
        assert!(
            scores.f1 >= SEGMENTATION_F1_GATE,
            "{}: segmentation F1 {} (gate {SEGMENTATION_F1_GATE})",
            p.id,
            scores.f1
        );
        f1_sum += scores.f1;

        // Per-track: the silence treatment (an actual near-silent bar —
        // the most objective plant) must be found.
        let detected_types: Vec<&str> =
            a.observation.transitions.iter().map(|t| t.kind.as_str()).collect();
        assert!(
            detected_types.contains(&"silence"),
            "{}: silence boundary not detected among {detected_types:?}",
            p.id
        );

        // Groove: straight families read ~0 swing, swung families clearly
        // positive.
        let g = a.observation.groove.as_ref().expect("fixture has groove");
        if p.swing_ticks == 0.0 {
            assert!(g.swing < 0.04, "{}: straight swing {}", p.id, g.swing);
        } else {
            assert!(
                f64::from(g.swing) >= f64::from(p.swing_ticks) / 120.0 - 0.04,
                "{}: swing {} vs planted {} ticks",
                p.id,
                g.swing,
                p.swing_ticks
            );
        }
    }

    // Leave headroom over the gate: fixture plants are strongly
    // separated, so the CORPUS MEAN must sit well above 0.7 even though
    // individual tracks wobble with the ±1-bar grid tolerance.
    let mean_f1 = f1_sum / all.len() as f64;
    assert!(mean_f1 >= 0.85, "corpus mean segmentation F1 {mean_f1} dropped below headroom");
}

/// Fit the analyzed corpus and emit the shipped artifacts.
fn fits_and_artifacts() -> (std::path::PathBuf, BTreeMap<String, kontinuum_corpus::SubgenreFit>) {
    let obs: Vec<_> = analyzed().into_iter().map(|(_, a)| a.observation).collect();
    let dir = std::env::temp_dir().join("kontinuum-corpus-pipeline-e2e");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let mut fits = BTreeMap::new();
    for subgenre in ["minimal-techno", "microhouse"] {
        let tracks: Vec<_> = obs.iter().filter(|o| o.subgenre == subgenre).cloned().collect();
        let fit = fit_subgenre(&tracks).expect("fit");
        write_artifacts(&fit, &dir).expect("write artifacts");
        fits.insert(subgenre.to_string(), fit);
    }
    (dir, fits)
}

/// THE acceptance proof: artifacts written by the pipeline load through
/// the real #16/#17 consumers with zero code change and drive generation.
#[test]
fn artifacts_load_through_the_real_consumers() {
    let (dir, fits) = fits_and_artifacts();

    for subgenre in ["minimal-techno", "microhouse"] {
        let a_path = dir.join(format!("arrangement-params-{subgenre}.json"));
        let g_path = dir.join(format!("groove-templates-{subgenre}.json"));
        let a_text = std::fs::read_to_string(&a_path).expect("arrangement artifact on disk");
        let g_text = std::fs::read_to_string(&g_path).expect("groove artifact on disk");

        // Zero-code-change load path (the same calls #16/#17 use).
        let structure = StructureParams::load_json(&a_text).expect("loads");
        let bank = GrooveBank::load_json(&g_text).expect("loads");
        assert_eq!(structure.subgenre, subgenre);
        assert_eq!(bank.subgenre, subgenre);
        assert!(!structure.is_empty());
        assert!(!bank.is_empty());

        // Structure reflects the plants: full-energy Dev sections are
        // 12–18 bars in the fixtures; Breakdown is 8 (± the detector's
        // 1-bar boundary tolerance).
        let dev = structure.median_bars(Kind::Dev).expect("Dev mapped");
        assert!((8..=20).contains(&dev), "{subgenre}: Dev median {dev}");
        assert_eq!(structure.median_bars(Kind::Breakdown), Some(8), "{subgenre}");
        assert!(structure.dev_energy(0, 3).is_some());

        // A fitted template applies through the shared timing path.
        let mut rng = kontinuum_clock::stream(3, 1, 1);
        let g = bank.pick(None, 0.7, &mut rng).expect("templates exist");
        let mut steps = [0, 480, 960, 1440]
            .iter()
            .map(|&position| kontinuum_ir::schema::Step {
                position,
                velocity: 0.8,
                probability: 1.0,
                microtiming_ticks: 0,
                ratchet: 1,
                pitch: None,
                gate: None,
                accent: false,
            })
            .collect::<Vec<_>>();
        g.apply(&mut steps, &mut kontinuum_clock::stream(3, 1, 2));

        // And the artifacts drive a real session: valid, deterministic,
        // and different from the hand-seeded plan.
        let params = GenParams {
            seed: 31,
            structure: Some(structure),
            groove_bank: Some(bank),
            ..GenParams::default()
        };
        let with = generate_session(&params);
        let with_again = generate_session(&params);
        assert_eq!(
            serde_json::to_string(&with).unwrap(),
            serde_json::to_string(&with_again).unwrap(),
            "{subgenre}: artifact-fed generation is deterministic"
        );
        kontinuum_ir::validate_session(&with).expect("{subgenre} session validates");
        let plain = generate_session(&GenParams { seed: 31, ..GenParams::default() });
        assert_ne!(
            serde_json::to_string(&with).unwrap(),
            serde_json::to_string(&plain).unwrap(),
            "{subgenre}: fitted artifacts must move the plan"
        );
        let _ = &fits[subgenre];
    }
}

/// The issue's statistical validation, on the synthetic corpus: 100
/// sampled arrangements land within tolerance of the fitted (= corpus)
/// statistics.
#[test]
fn sampled_arrangements_match_the_corpus_statistics() {
    let (_, fits) = fits_and_artifacts();
    let fit = &fits["minimal-techno"];
    const SAMPLES: usize = 100;

    let mut by_kind: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    let mut successors: BTreeMap<String, BTreeMap<String, u32>> = BTreeMap::new();
    let mut build_drop_types: BTreeMap<String, u32> = BTreeMap::new();
    for i in 0..SAMPLES {
        let s = sample_arrangement(fit, 11 + i as u64);
        assert_eq!(s.sections.first().unwrap().kind, "intro");
        // Walks end at outro unless the 32-section clamp fires first —
        // with only 3 fixture tracks the outro row is thin, so a few
        // capped walks are expected and honest.
        assert!(
            s.sections.last().unwrap().kind == "outro"
                || s.sections.len() == kontinuum_corpus::sample::MAX_SECTIONS,
            "walk neither ends at outro nor at the clamp"
        );
        for sec in &s.sections {
            by_kind.entry(sec.kind.clone()).or_default().push(sec.bars);
        }
        for (w, t) in s.sections.windows(2).zip(&s.transition_types) {
            *successors
                .entry(w[0].kind.clone())
                .or_default()
                .entry(w[1].kind.clone())
                .or_insert(0) += 1;
            if w[0].kind == "build" && w[1].kind == "drop" {
                *build_drop_types.entry(t.clone()).or_insert(0) += 1;
            }
        }
    }
    // Section lengths within ±2 bars of the corpus mean (≈5 SE at this n).
    for (kind, params) in &fit.section_lengths {
        let drawn = &by_kind[kind];
        assert!(drawn.len() >= 20, "{kind}: enough draws");
        let mean = drawn.iter().sum::<u32>() as f32 / drawn.len() as f32;
        assert!(
            (mean - params.mean_bars).abs() <= 2.0,
            "{kind}: sampled mean {mean} vs corpus {}",
            params.mean_bars
        );
    }
    // Order: sampled successor argmax must equal the fitted matrix argmax.
    for (kind, row) in &fit.transition_matrix {
        let Some(sampled) = successors.get(kind) else { continue };
        let n: u32 = sampled.values().sum();
        if n < 50 {
            continue;
        }
        let fitted_best = row.iter().fold(("", 0.0f32), |(k, p), (k2, p2)| {
            if *p2 > p { (k2.as_str(), *p2) } else { (k, p) }
        });
        let sampled_best = sampled.iter().fold(("", 0u32), |(k, c), (k2, c2)| {
            if *c2 > c { (k2.as_str(), *c2) } else { (k, c) }
        });
        assert_eq!(sampled_best.0, fitted_best.0, "{kind}: order argmax");
    }
    // Transition-type vocabulary survives the whole chain: every
    // boundary treatment the fixtures plant is present in the fitted
    // conditional tables. (The sampled-majority-per-type property is
    // corpus-mass-limited here — 6 tracks per subgenre — and is asserted
    // against the denser fit-level fixture in kontinuum-corpus's
    // grammar_sampling tests.)
    for t in ["silence", "filter_sweep", "fill", "hard_cut"] {
        let present = fit
            .transition_types
            .values()
            .any(|row| row.contains_key(t));
        assert!(present, "type {t} missing from the fitted tables");
    }
}
