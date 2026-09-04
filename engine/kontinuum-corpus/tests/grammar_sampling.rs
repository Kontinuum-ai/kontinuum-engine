//! The issue's statistical validation, scaled down: sample 100 arrangements
//! from the FITTED grammar (seeded, deterministic) and check that
//! section-length and section-order statistics land within tolerance of
//! the corpus statistics.
//!
//! Tolerances (fixed seed → a pass is stable, not a flake):
//! - per-kind sampled mean length within ±2.0 bars of the fitted mean:
//!   sampled per-kind σ ≤ ~4 bars over ≥ 100 draws → SE ≤ 0.4, so ±2.0 is
//!   ≈ 5 standard errors;
//! - per-kind successor distribution: for every kind with ≥ 50 sampled
//!   outgoing edges, the sampled argmax must equal the fitted argmax and
//!   |sampled freq − fitted p| ≤ 0.12 (SE ≈ 0.03 at n ≈ 200 → ≈ 4 SE).
//! They are wiring gates, not estimator-precision claims.

use std::collections::BTreeMap;

use kontinuum_corpus::{
    fit_subgenre, load_jsonl_file, sample_arrangement, SubgenreFit, TrackObservation,
};
use std::path::Path;

const SAMPLES: usize = 100;
const SEED: u64 = 7;

fn minimal_fit() -> SubgenreFit {
    let obs: Vec<TrackObservation> = load_jsonl_file(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/corpus-sample.jsonl"),
    )
    .expect("fixture must parse")
    .into_iter()
    .filter(|o| o.subgenre == "minimal-techno")
    .collect();
    fit_subgenre(&obs).expect("minimal fit")
}

fn tally(sections: &[(String, u32)]) -> BTreeMap<String, Vec<u32>> {
    let mut by_kind: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for (kind, bars) in sections {
        by_kind.entry(kind.clone()).or_default().push(*bars);
    }
    by_kind
}

#[test]
fn sampled_arrangements_have_valid_shape() {
    let fit = minimal_fit();
    for i in 0..SAMPLES {
        let s = sample_arrangement(&fit, SEED + i as u64);
        assert_eq!(s.sections[0].kind, "intro", "walks start at intro");
        assert_eq!(s.sections.last().unwrap().kind, "outro", "walks end at outro");
        assert!(s.sections.len() <= kontinuum_corpus::sample::MAX_SECTIONS);
        assert_eq!(s.transition_types.len(), s.sections.len() - 1);
        assert!(s.sections.iter().all(|sec| (1..=64).contains(&sec.bars)));
    }
}

#[test]
fn sampled_section_lengths_match_corpus_within_two_bars() {
    let fit = minimal_fit();
    let mut sections: Vec<(String, u32)> = Vec::new();
    for i in 0..SAMPLES {
        for sec in &sample_arrangement(&fit, SEED + i as u64).sections {
            sections.push((sec.kind.clone(), sec.bars));
        }
    }
    for (kind, params) in &fit.section_lengths {
        let drawn = &tally(&sections)[kind];
        assert!(drawn.len() >= 20, "{kind}: enough draws for a mean");
        let sampled_mean = drawn.iter().sum::<u32>() as f32 / drawn.len() as f32;
        assert!(
            (sampled_mean - params.mean_bars).abs() <= 2.0,
            "{kind}: sampled mean {sampled_mean} vs corpus mean {} (n={})",
            params.mean_bars,
            drawn.len()
        );
    }
}

#[test]
fn sampled_order_matches_fitted_transition_argmax() {
    let fit = minimal_fit();
    let mut successors: BTreeMap<String, BTreeMap<String, u32>> = BTreeMap::new();
    let mut type_counts: BTreeMap<String, u32> = BTreeMap::new();
    for i in 0..SAMPLES {
        let s = sample_arrangement(&fit, SEED + i as u64);
        for (w, kind) in s.sections.windows(2).zip(&s.transition_types) {
            *successors
                .entry(w[0].kind.clone())
                .or_default()
                .entry(w[1].kind.clone())
                .or_insert(0) += 1;
            *type_counts.entry(kind.clone()).or_insert(0) += 1;
        }
    }
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
        assert_eq!(
            sampled_best.0, fitted_best.0,
            "{kind}: sampled successor argmax must match the fitted matrix"
        );
        let sampled_p = sampled_best.1 as f32 / n as f32;
        assert!(
            (sampled_p - fitted_best.1).abs() <= 0.12,
            "{kind}→{}: sampled p {sampled_p} vs fitted {} over {n} edges",
            fitted_best.0,
            fitted_best.1
        );
    }
    // The planted conditional type: build→drop boundaries are filter sweeps.
    let build_drop_types: Vec<String> = (0..SAMPLES)
        .flat_map(|i| {
            let s = sample_arrangement(&fit, SEED + i as u64);
            s.sections
                .windows(2)
                .zip(&s.transition_types)
                .filter(|(w, _)| w[0].kind == "build" && w[1].kind == "drop")
                .map(|(_, t)| t.clone())
                .collect::<Vec<_>>()
        })
        .collect();
    let sweeps = build_drop_types.iter().filter(|t| *t == "filter_sweep").count();
    assert!(
        sweeps * 2 > build_drop_types.len(),
        "majority of build→drop boundaries must sample as filter_sweep ({sweeps}/{}>)",
        build_drop_types.len() / 2
    );
}
