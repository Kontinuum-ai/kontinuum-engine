//! Query executor contract (#20 engine side): deterministic ranking, the
//! similarity-floor fallback, and pin determinism over the shipped fixture.

use kontinuum_samples::{default_for, parse_catalog, resolve_slot, run_query, SampleClass, SampleQuery};

fn fixture() -> Vec<kontinuum_samples::SampleCatalog> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/catalog.json");
    let json = std::fs::read_to_string(path).expect("fixture");
    parse_catalog(&json).expect("valid catalog").samples
}

#[test]
fn fixture_loads_and_deterministically_ranks() {
    let cat = fixture();
    let q = SampleQuery {
        text_terms: vec!["punchy".into()],
        class: Some(SampleClass::Kick),
        target_centroid_hz: Some(100.0),
        ..Default::default()
    };
    let a = run_query(&cat, &q, None);
    let b = run_query(&cat, &q, None);
    assert_eq!(a, b, "same catalog + query → identical ranking");
    assert!(!a.candidates.is_empty());
    assert_eq!(a.candidates[0].sample.id, "kick.punch.01", "term + centroid point at it");
    assert!(a.candidates.windows(2).all(|w| w[0].score >= w[1].score), "ranked descending");
}

#[test]
fn unfiltered_query_covers_all_classes_with_term_scoring() {
    let cat = fixture();
    let q = SampleQuery { text_terms: vec!["granular".into()], ..Default::default() };
    let r = run_query(&cat, &q, None);
    assert_eq!(r.candidates[0].sample.id, "tex.grain.01");
}

#[test]
fn similarity_floor_falls_back_to_palette_default() {
    let cat = fixture();
    // Impossible brightness for a kick (15 kHz) + a term nothing carries:
    // every score collapses to ~0, far below the floor.
    let q = SampleQuery {
        text_terms: vec!["zzz-no-such-tag".into()],
        class: Some(SampleClass::Kick),
        target_centroid_hz: Some(15_000.0),
        ..Default::default()
    };
    let r = run_query(&cat, &q, None);
    assert!(r.used_fallback, "warning marker set");
    assert!(r.candidates.is_empty());
    let pin = resolve_slot(&cat, &q, None);
    assert_eq!(pin.id, default_for(Some(SampleClass::Kick)));
    assert_eq!(pin.pipeline_version, 1);
}

#[test]
fn underspecified_query_still_clears_the_floor() {
    let cat = fixture();
    // Neutral query: every component is 1.0 → score 1.0 ≥ floor, no fallback.
    let r = run_query(&cat, &SampleQuery::default(), None);
    assert!(!r.used_fallback);
    assert_eq!(r.candidates.len(), kontinuum_samples::MAX_CANDIDATES);
}

#[test]
fn pins_are_deterministic_and_stable_across_re_resolution() {
    let cat = fixture();
    let q = SampleQuery {
        text_terms: vec!["woody".into()],
        class: Some(SampleClass::Perc),
        ..Default::default()
    };
    let a = resolve_slot(&cat, &q, None);
    let b = resolve_slot(&cat, &q, None);
    assert_eq!(a, b, "same catalog + query → same pin");
    assert_eq!(a.id, "perc.wood.01");
    assert_eq!(a.pipeline_version, 1);

    // An empty catalog cannot satisfy anything: the role default pins.
    let empty_pin = resolve_slot(&[], &q, None);
    assert_eq!(empty_pin.id, default_for(Some(SampleClass::Perc)));

    // The unclassified role default is whatever wins the id tie-break.
    let any = resolve_slot(&cat, &SampleQuery::default(), None);
    assert_eq!(any.id, "hat.closed.01", "neutral scores tie-break by id ascending");
}

#[test]
fn different_queries_pin_differently() {
    let cat = fixture();
    let bright = SampleQuery {
        class: Some(SampleClass::Hat),
        target_centroid_hz: Some(11_000.0),
        ..Default::default()
    };
    let dark = SampleQuery {
        class: Some(SampleClass::Hat),
        target_centroid_hz: Some(6_500.0),
        ..Default::default()
    };
    assert_ne!(resolve_slot(&cat, &bright, None), resolve_slot(&cat, &dark, None));
}
