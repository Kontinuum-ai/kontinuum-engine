//! Section-grammar tests (issue #16): the hard constraints hold across
//! seeds and targets, and the learned-distribution mechanism is real —
//! a corpus-fit grammar file swaps in with zero code change and visibly
//! reshapes the plan (the #23 contract).

use kontinuum_compose::structure::StructureParams;
use kontinuum_compose::{generate_session, GenParams};
use kontinuum_ir::schema::Pattern;
use kontinuum_ir::validate_session;

const BASE: &str = include_str!("../fixtures/arrangement-grammar-base.json");
const FITTED: &str = include_str!("../fixtures/arrangement-params-fitted-test.json");

#[test]
fn grammar_constraints_hold_across_seeds_and_targets() {
    // Pin a late-breakdown family: twin_peak legitimately allows a
    // breakdown before the gate (that is the family's escape hatch).
    for seed in 0..12u64 {
        for target in [128u32, 312, 512] {
            let params = GenParams {
                seed,
                target_bars: target,
                arc: Some(kontinuum_compose::grammar::ArcFamily::SlowBurn),
                ..GenParams::default()
            };
            let s = generate_session(&params);
            validate_session(&s).unwrap_or_else(|e| panic!("seed {seed}/{target}: {e:?}"));

            let starts = s.section_start_bars();
            assert_eq!(s.sections.first().unwrap().id, "intro", "intro opens");
            assert_eq!(s.sections.last().unwrap().id, "outro", "outro closes");
            assert_eq!(
                s.sections[s.sections.len() - 2].id,
                "reintro",
                "reintro directly precedes the terminal outro"
            );
            assert!(
                s.sections.iter().any(|x| x.id.starts_with("dev_")),
                "seed {seed}/{target}: a motif source (groove_dev) exists before the reintro"
            );
            for (i, sec) in s.sections.iter().enumerate() {
                if sec.id.starts_with("break_") {
                    assert!(
                        starts[i] >= 64,
                        "seed {seed}/{target}: breakdown at bar {} before the gate",
                        starts[i]
                    );
                }
            }
        }
    }
}

#[test]
fn twin_peak_family_may_place_early_breakdowns() {
    // The escape hatch must actually exist somewhere in the family draws.
    let saw_early = (0..30u64).any(|seed| {
        let s = generate_session(&GenParams {
            seed,
            target_bars: 512,
            arc: Some(kontinuum_compose::grammar::ArcFamily::TwinPeak),
            ..GenParams::default()
        });
        let starts = s.section_start_bars();
        s.sections
            .iter()
            .zip(starts.iter())
            .any(|(sec, start)| sec.id.starts_with("break_") && *start < 64)
    });
    assert!(saw_early, "no seed placed a breakdown before bar 64 under twin_peak");
}

#[test]
fn reintro_plays_transformed_material_not_fresh_draws() {
    for seed in [3u64, 15, 64] {
        let s = generate_session(&GenParams { seed, target_bars: 312, ..GenParams::default() });
        let dev_patterns: Vec<(String, &Pattern)> = s
            .sections
            .iter()
            .filter(|x| x.id.starts_with("dev_"))
            .flat_map(|x| x.pattern_bindings.iter().map(|(t, p)| (t.clone(), p)))
            .collect();
        let reintro = s.sections.iter().find(|x| x.id == "reintro").unwrap();
        for (track, pattern) in &reintro.pattern_bindings {
            let same_as_some_dev =
                dev_patterns.iter().any(|(t, p)| t == track && **p == *pattern);
            assert!(
                !same_as_some_dev,
                "seed {seed} {track}: reintro replayed a dev figure verbatim — material must return changed"
            );
        }
    }
}

#[test]
fn corpus_fit_swaps_in_with_zero_code_change() {
    // Both files load through the same StructureParams loader; the base
    // grammar applies when no artifact is loaded.
    let base = StructureParams::load_json(BASE).expect("base parses");
    assert!(base.grammar.is_some(), "base carries its grammar block");
    let fitted = StructureParams::load_json(FITTED).expect("fitted parses");
    assert_eq!(fitted.corpus_size, 42);
    assert!(fitted.grammar.is_some());

    let with_base = GenParams {
        seed: 9,
        target_bars: 512,
        structure: Some(base),
        ..GenParams::default()
    };
    let with_fit = GenParams {
        seed: 9,
        target_bars: 512,
        structure: Some(fitted),
        ..GenParams::default()
    };
    let a = generate_session(&with_base);
    let b = generate_session(&with_fit);
    validate_session(&a).expect("base-driven session valid");
    validate_session(&b).expect("fit-driven session valid");
    assert_ne!(
        serde_json::to_string(&a).unwrap(),
        serde_json::to_string(&b).unwrap(),
        "the fitted file must visibly reshape the arrangement"
    );
    // The fit's longer dev windows (32..=48 vs the base 16..=32) show up
    // as a longer dev block for the same seed — the windows do not overlap.
    let dev_bars = |s: &kontinuum_ir::Session| {
        s.sections
            .iter()
            .filter(|x| x.id.starts_with("dev_"))
            .map(|x| x.bars)
            .sum::<u32>()
    };
    assert!(
        dev_bars(&b) > dev_bars(&a),
        "fit dev block {} must exceed base {} for the same seed",
        dev_bars(&b),
        dev_bars(&a)
    );
}

#[test]
fn pre_grammar_artifact_still_loads_and_falls_back() {
    // The #23 fixture predates the grammar field: it loads fine, exposes
    // no grammar, and the planner keeps the embedded base.
    let legacy = StructureParams::load_json(
        include_str!("../fixtures/arrangement-params-minimal-techno.json"),
    )
    .expect("legacy fixture parses");
    assert!(legacy.grammar.is_none());
    let s = generate_session(&GenParams {
        seed: 4,
        structure: Some(legacy),
        ..GenParams::default()
    });
    validate_session(&s).expect("legacy-artifact session valid");
}
