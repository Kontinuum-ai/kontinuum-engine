//! Render determinism — the #53 contract: recipe + seed always yields
//! bit-identical samples, and the seed meaningfully varies the render.

use kontinuum_samples::{recipe_hash, render_recipe, validate, RecipeError, SampleRecipe};

fn kit() -> SampleRecipe {
    serde_json::from_str(
        r#"{
        "version": 1, "seed": 7, "name": "dusty micro kit",
        "tail_ms": 400,
        "voices": [
            {"id": "kick", "instrument": {"kind": "kick", "tune_hz": 54.0, "decay_ms": 260.0, "click": 0.55},
             "chain": [{"type": "drive", "amount": 1.4, "mix": 0.5}]},
            {"id": "click", "instrument": {"kind": "hat", "decay_ms": 28.0, "tone": 0.85},
             "chain": [{"type": "highpass", "amount": 3000.0}]},
            {"id": "shaker", "instrument": {"kind": "shaker", "decay_ms": 70.0, "tone": 0.8}}
        ],
        "hits": [
            {"voice": "kick", "at_ms": 0.0, "pitch": 36, "velocity": 0.95},
            {"voice": "click", "at_ms": 125.0, "velocity": 0.5},
            {"voice": "shaker", "at_ms": 250.0, "pitch": 62, "velocity": 0.6},
            {"voice": "kick", "at_ms": 500.0, "pitch": 36, "velocity": 0.9},
            {"voice": "click", "at_ms": 625.0, "velocity": 0.45}
        ],
        "slice": {"mode": "transient", "max_slices": 8, "sensitivity": 0.5},
        "tags": ["microhouse", "dusty"]
    }"#,
    )
    .expect("parse")
}

#[test]
fn same_recipe_renders_bit_identically() {
    let a = render_recipe(&kit()).expect("render");
    let b = render_recipe(&kit()).expect("render");
    assert_eq!(a.pcm.len(), b.pcm.len());
    assert!(
        a.pcm.iter().zip(b.pcm.iter()).all(|(x, y)| x.to_bits() == y.to_bits()),
        "renders diverged"
    );
    assert_eq!(a.hash, b.hash);
    assert_eq!(a.slices, b.slices);
}

#[test]
fn seed_changes_the_humanized_render() {
    let mut other = kit();
    other.seed = 99;
    let a = render_recipe(&kit()).expect("render");
    let b = render_recipe(&other).expect("render");
    assert_ne!(
        a.pcm.iter().zip(b.pcm.iter()).filter(|(x, y)| x.to_bits() != y.to_bits()).count(),
        0,
        "seed must humanize the render"
    );
}

#[test]
fn renders_are_audible_and_bounded() {
    let out = render_recipe(&kit()).expect("render");
    let peak = out.pcm.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(peak > 0.1, "silent render: {peak}");
    assert!(out.pcm.iter().all(|s| s.is_finite()));
    assert!(peak <= 4.0, "runaway chain: {peak}");
}

#[test]
fn transient_slicing_finds_hits() {
    let out = render_recipe(&kit()).expect("render");
    assert_eq!(out.slices[0], 0);
    assert!(out.slices.len() >= 2, "onsets found: {:?}", out.slices);
    assert!(out.tags.contains(&"microhouse".to_string()));
}

#[test]
fn chain_changes_the_sound() {
    let mut driven = kit();
    driven.voices[0].chain.push(serde_json::from_str(r#"{"type": "drive", "amount": 3.0, "mix": 1.0}"#).unwrap());
    assert!(validate(&driven).is_ok());
    let a = render_recipe(&kit()).expect("render");
    let b = render_recipe(&driven).expect("render");
    assert_ne!(a.hash, b.hash, "chain participates in the pack identity");
    assert_ne!(a.pcm, b.pcm);
}

#[test]
fn fixed_ms_slicing_yields_grid() {
    let mut r = kit();
    r.slice = serde_json::from_str(
        r#"{"mode": "fixed_ms", "interval_ms": 250.0}"#,
    )
    .unwrap();
    let out = render_recipe(&r).expect("render");
    assert!(out.slices.len() >= 4, "grid slices: {:?}", out.slices);
}

#[test]
fn invalid_documents_are_rejected_before_render() {
    let mut bad = kit();
    bad.hits[0].voice = "ghost".into();
    assert!(matches!(render_recipe(&bad), Err(RecipeError::UnknownVoice(_))));
}

#[test]
fn hash_is_stable_and_content_addressed() {
    assert_eq!(recipe_hash(&kit()), recipe_hash(&kit()));
}

#[test]
fn committed_pack_fixture_renders_clean() {
    let fixture: SampleRecipe = serde_json::from_str(
        &std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/recipes/dusty-micro-kit.json"
        ))
        .expect("fixture"),
    )
    .expect("parse");
    assert!(validate(&fixture).is_ok());
    let out = render_recipe(&fixture).expect("render");
    assert!(!out.pcm.is_empty());
    assert!(out.slices.len() >= 2);
}

// --- issue #19 v1 features: choke, expression, texture ---

/// Hat pair in choke group 1 plus a granular texture bed over a pad voice
/// and fully-loaded expression on the hats.
fn v1_kit() -> SampleRecipe {
    serde_json::from_str(
        r#"{
        "version": 1, "seed": 11, "name": "v1 kit",
        "tail_ms": 300,
        "voices": [
            {"id": "closed", "instrument": {"kind": "hat", "decay_ms": 40.0, "tone": 0.8}},
            {"id": "wash", "instrument": {"kind": "hat", "decay_ms": 900.0, "open": true}},
            {"id": "pad", "instrument": {"kind": "pad", "attack_ms": 40.0, "release_ms": 400.0, "detune_cents": 12.0, "cutoff_hz": 1200.0}}
        ],
        "hits": [
            {"voice": "wash", "at_ms": 0.0, "velocity": 0.9, "choke_group": 1},
            {"voice": "closed", "at_ms": 250.0, "velocity": 0.8, "choke_group": 1,
             "expression": {"curve": "exponential", "velocity_layers": 4, "round_robin": 4,
                            "alternate_probability": 0.25, "microtiming_ms": 8.0,
                            "humanize_gain_db": 1.5, "humanize_pitch_cents": 12.0}},
            {"voice": "closed", "at_ms": 500.0, "velocity": 0.8, "choke_group": 1,
             "expression": {"curve": "exponential", "velocity_layers": 4, "round_robin": 4,
                            "alternate_probability": 0.25, "microtiming_ms": 8.0,
                            "humanize_gain_db": 1.5, "humanize_pitch_cents": 12.0}}
        ],
        "texture": {"source_voice": "pad", "grain_ms": 80.0, "density": 20.0,
                    "spray_ms": 50.0, "pitch_jitter_cents": 25.0, "window": "hann",
                    "level": 0.3},
        "tags": ["v1"]
    }"#,
    )
    .expect("parse")
}

#[test]
fn v1_features_render_bit_identically() {
    let a = render_recipe(&v1_kit()).expect("render");
    let b = render_recipe(&v1_kit()).expect("render");
    assert_eq!(a.pcm.len(), b.pcm.len());
    assert!(
        a.pcm.iter().zip(b.pcm.iter()).all(|(x, y)| x.to_bits() == y.to_bits()),
        "choke + expression + texture renders diverged"
    );
    assert_eq!(a.slices, b.slices);
    assert_eq!(a.hash, b.hash);
}

#[test]
fn choke_kills_the_open_hat_within_10ms() {
    let choked = v1_kit();
    // Same document, but the closed hat triggers group 2 instead: the wash
    // rings on, everything else renders identically (same seed, same indices).
    let mut ringing = v1_kit();
    ringing.hits[1].choke_group = Some(2);

    let with = render_recipe(&choked).expect("render");
    let without = render_recipe(&ringing).expect("render");

    let at = |ms: f32| (ms / 1000.0 * 48_000.0).round() as usize;
    let fade_end = at(250.0 + 6.0 + 10.0) + 1;
    // Before the trigger both renders are bit-equal: the choke starts at
    // the trigger frame, not before.
    assert!(
        with.pcm[..at(244.0)].iter().zip(&without.pcm[..at(244.0)]).all(|(x, y)| x.to_bits() == y.to_bits()),
        "pre-trigger renders must be identical"
    );
    // After the fade the wash is gone from the choked render while the
    // un-choked kit still carries it: every post-fade frame loses energy.
    let window_end = at(490.0);
    let energy = |x: &[f32]| x.iter().map(|s| s * s).sum::<f32>();
    assert!(
        energy(&with.pcm[fade_end..window_end]) < energy(&without.pcm[fade_end..window_end]) * 0.5,
        "the open hat must be choked to silence within the fade window"
    );
    assert!(
        with.pcm[at(244.0)..fade_end].iter().zip(&without.pcm[at(244.0)..fade_end]).any(|(x, y)| x.to_bits() != y.to_bits()),
        "the fast fade must be audible inside the choke window"
    );
}

#[test]
fn expression_round_robin_varies_repeated_hits() {
    let mut cycled = v1_kit();
    cycled.tail_ms = Some(300.0);
    cycled.hits[1].at_ms = 125.0;
    cycled.hits[2].at_ms = 250.0;
    let mut uncycled = cycled.clone();
    for hit in uncycled.hits.iter_mut() {
        if let Some(expr) = &mut hit.expression {
            expr.round_robin = None;
        }
    }
    let with_rr = render_recipe(&cycled).expect("render");
    let without_rr = render_recipe(&uncycled).expect("render");

    let at = |ms: f32| (ms / 1000.0 * 48_000.0).round() as usize;
    // Hit 0 draws take 0 in both renders; hit 1 cycles to take 1 only when
    // round-robin is on, so their audio must diverge.
    assert!(
        with_rr.pcm[..at(110.0)].iter().zip(&without_rr.pcm[..at(110.0)]).all(|(x, y)| x.to_bits() == y.to_bits()),
        "take-0 step must render identically with and without round-robin"
    );
    assert_ne!(
        with_rr.pcm[at(131.0)..at(300.0)],
        without_rr.pcm[at(131.0)..at(300.0)],
        "round-robin must change the second step's take"
    );
}

#[test]
fn texture_bed_changes_the_mix_and_validates_its_source() {
    let mut with_texture = v1_kit();
    let baseline = {
        with_texture.texture = None;
        render_recipe(&with_texture).expect("render")
    };
    with_texture.texture = serde_json::from_str(
        r#"{"source_voice": "pad", "grain_ms": 80.0, "density": 20.0, "level": 0.3}"#,
    )
    .ok();
    let layered = render_recipe(&with_texture).expect("render");
    assert_ne!(baseline.pcm, layered.pcm, "texture bed must be audible");

    let mut ghost = v1_kit();
    ghost.texture.as_mut().expect("texture").source_voice = "ghost".into();
    assert!(matches!(validate(&ghost), Err(RecipeError::UnknownVoice(v)) if v == "ghost"));
}

#[test]
fn expression_and_choke_bounds_are_validated() {
    let mut bad = v1_kit();
    bad.hits[1].choke_group = Some(0);
    assert!(matches!(validate(&bad), Err(RecipeError::OutOfBounds { field: "choke_group", .. })));

    let mut bad = v1_kit();
    bad.hits[1].expression.as_mut().expect("expr").round_robin = Some(9);
    assert!(matches!(validate(&bad), Err(RecipeError::OutOfBounds { field: "round_robin", .. })));

    let mut bad = v1_kit();
    bad.hits[1].expression.as_mut().expect("expr").microtiming_ms = Some(99.0);
    assert!(validate(&bad).is_err());

    let mut bad = v1_kit();
    bad.texture.as_mut().expect("texture").grain_ms = 10.0;
    assert!(validate(&bad).is_err());

    assert!(validate(&v1_kit()).is_ok());
}
