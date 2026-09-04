//! World regression renders (issue #30): every shipped sound world drives
//! generation (fixed genre/seed through [`GenParams::world`], the same hook
//! the SessionDirector taste selection feeds), renders offline, and must
//! pass its critic-gate profile — a `fixtures/profiles/world-{id}.json`
//! quality profile plus a ratchet baseline (`world-{id}.baseline.json`),
//! following the #117 premium-ratchet conventions: bounds are anchored to
//! the first measured render and the ratchet exists to absorb cross-host
//! float drift (libm/LLVM; see golden.rs), not to license misses.
//!
//! The fixture set also renders `sound-roster-v2.ir.json`, the reference
//! session exercising every #30 addition (wavetable / FM-perc / texture
//! tracks, chorus / phaser / freq-shifter / transient inserts, tape delay +
//! fdn8 buses): bit-deterministic, finite, audible.

use std::path::Path;

use kontinuum_analysis::profile::{Baseline, QualityProfile};
use kontinuum_analysis::Metrics;
use kontinuum_compose::arrangement::{generate_session, GenParams};
use kontinuum_compose::world;
use kontinuum_ir::{validate_session, Session};
use kontinuum_offline::{render_session, RenderOutput, DEFAULT_SAMPLE_RATE};

const WORLDS: [&str; 4] = ["dust", "concrete", "fathom", "pulse"];

fn world_fixture(id: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/../../engine/kontinuum-compose/fixtures/worlds/{id}.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap_or_else(|e| panic!("{id}: {e}"))
}

fn profile_path(id: &str) -> String {
    format!("{}/../../fixtures/profiles/world-{id}.json", env!("CARGO_MANIFEST_DIR"))
}

fn baseline_path(id: &str) -> String {
    format!("{}/../../fixtures/profiles/world-{id}.baseline.json", env!("CARGO_MANIFEST_DIR"))
}

fn generate_world_session(id: &str) -> Session {
    let w = world::load_json(&world_fixture(id)).expect("world fixture");
    let params = GenParams {
        seed: 31,
        target_bars: 32,
        genre: Some(match id {
            "pulse" => "techno".into(),
            "concrete" => "minimal techno".into(),
            "fathom" => "dub techno".into(),
            _ => "microhouse".into(),
        }),
        world: Some(w),
        ..GenParams::default()
    };
    let session = generate_session(&params);
    validate_session(&session).expect("world session must validate");
    session
}

fn roster_fixture() -> Session {
    let path = format!("{}/../../fixtures/genres/sound-roster-v2.ir.json", env!("CARGO_MANIFEST_DIR"));
    kontinuum_offline::parse_session(Path::new(&path)).expect("roster fixture parses")
}

fn render(session: &Session) -> RenderOutput {
    render_session(session, DEFAULT_SAMPLE_RATE).expect("renders")
}

fn assert_audible_and_finite(out: &RenderOutput, name: &str) {
    assert!(out.left.iter().chain(&out.right).all(|s| s.is_finite()), "{name}: non-finite");
    let peak = out.left.iter().chain(&out.right).fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(peak > 0.01, "{name}: silent render, peak {peak}");
}

#[test]
fn world_renders_are_bit_deterministic() {
    for id in WORLDS {
        let a = render(&generate_world_session(id));
        let b = render(&generate_world_session(id));
        assert_eq!(a.left, b.left, "{id}: not deterministic");
        assert_eq!(a.right, b.right, "{id}: not deterministic");
        assert_audible_and_finite(&a, id);
    }
}

#[test]
fn sound_roster_v2_fixture_renders_deterministically_and_audibly() {
    let a = render(&roster_fixture());
    let b = render(&roster_fixture());
    assert_eq!(a.left, b.left);
    assert_eq!(a.right, b.right);
    assert_audible_and_finite(&a, "sound-roster-v2");
}

#[test]
fn world_renders_pass_their_critic_gates() {
    for id in WORLDS {
        let out = render(&generate_world_session(id));
        let metrics = Metrics::analyze(&out.left, &out.right, out.sample_rate);
        let profile = QualityProfile::load(Path::new(&profile_path(id)))
            .unwrap_or_else(|e| panic!("{id} profile: {e}"));
        let baseline = Baseline::load(Path::new(&baseline_path(id)))
            .unwrap_or_else(|e| panic!("{id} baseline: {e}"));
        match baseline.passes(&metrics, &profile) {
            Ok(distance) => {
                println!("{id}: distance {distance:.3} within ratchet");
            }
            Err(distance) => panic!(
                "{id}: critic gate regression — distance {distance:.3} exceeds baseline {} + ratchet {}. \
                 Re-anchor only with a documented, deliberate sound change.",
                baseline.distance, baseline.ratchet
            ),
        }
    }
}
