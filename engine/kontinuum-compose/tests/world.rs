//! Sound-world end-to-end tests (issue #30): fixture loading, the
//! layering contract (world on top of genre), taste-weighted selection,
//! the section-boundary morph, and the no-world byte-identical regression.

use kontinuum_compose::taste::TasteProfile;
use kontinuum_compose::world::{self, MorphError, SoundWorld};
use kontinuum_compose::{generate_session, GenParams};
use kontinuum_ir::schema::InstrumentDef;
use kontinuum_ir::{validate_session, Session};

const DUST: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/worlds/dust.json"));
const CONCRETE: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/worlds/concrete.json"));
const FATHOM: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/worlds/fathom.json"));
const PULSE: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/worlds/pulse.json"));

fn load(text: &str) -> SoundWorld {
    world::load_json(text).expect("fixture world parses and validates")
}

fn shipped() -> Vec<SoundWorld> {
    vec![load(DUST), load(CONCRETE), load(FATHOM), load(PULSE)]
}

fn params(seed: u64, world: Option<SoundWorld>) -> GenParams {
    GenParams { seed, world, ..GenParams::default() }
}

fn bass_cutoff(s: &Session) -> f32 {
    match &s.tracks.iter().find(|t| t.id == "bass").expect("bass").instrument {
        InstrumentDef::Bass(b) => b.cutoff_hz,
        other => panic!("bass is not a bass: {other:?}"),
    }
}

#[test]
fn shipped_world_fixtures_parse_and_validate() {
    let worlds = shipped();
    assert_eq!(worlds.len(), 4);
    let mut ids: Vec<_> = worlds.iter().map(|w| w.id.0.clone()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 4, "world ids must be distinct: {ids:?}");
    for w in &worlds {
        assert!(!w.taste_tags.is_empty(), "{} must declare taste tags", w.id.0);
        assert!(!w.groove_affinities.is_empty(), "{} must declare groove affinities", w.id.0);
    }
}

#[test]
fn world_overrides_move_session_palette_params() {
    let with = generate_session(&params(7, Some(load(DUST))));
    let without = generate_session(&params(7, None));

    assert!((bass_cutoff(&with) - 320.0).abs() < 1e-4, "dust bass cutoff must land");
    assert!(
        (bass_cutoff(&with) - bass_cutoff(&without)).abs() > 1e-4,
        "world must move the palette vs no-world"
    );

    let perc = with.tracks.iter().find(|t| t.id == "perc").expect("perc");
    match &perc.instrument {
        InstrumentDef::Hat(h) => assert!((h.tone - 0.5).abs() < 1e-4, "dust perc tone must land"),
        other => panic!("perc is not a hat: {other:?}"),
    }
    let pad = with.tracks.iter().find(|t| t.id == "pad").expect("pad");
    assert!((pad.gain - 0.5).abs() < 1e-4, "dust pad mix gain must land: {}", pad.gain);

    assert_eq!(without.palette, None, "no-world sessions stay unstamped");
    assert_eq!(
        with.palette.as_ref().and_then(|p| p.get("world")).and_then(|w| w.as_str()),
        Some("dust")
    );

    validate_session(&with).expect("world-fed session validates");
}

#[test]
fn world_layers_on_top_of_the_genre_rig() {
    let p = GenParams {
        seed: 7,
        genre: Some("microhouse".into()),
        world: Some(load(DUST)),
        ..GenParams::default()
    };
    let s = generate_session(&p);
    assert!(
        (bass_cutoff(&s) - 320.0).abs() < 1e-4,
        "the world override beats genre staging on the fields it names"
    );
    // Fields (and tracks) the world does not name keep the genre staging:
    // the sparse rack's pluck keeps its mix level.
    let pluck = s.tracks.iter().find(|t| t.id == "pluck").expect("pluck");
    match &pluck.instrument {
        InstrumentDef::Pluck(_) => {}
        other => panic!("pluck is not a pluck: {other:?}"),
    }
    assert!((pluck.gain - 0.42).abs() < 1e-4, "genre staging survives where the world is silent");
}

#[test]
fn no_world_output_is_byte_identical() {
    let explicit_none = generate_session(&params(31, None));
    let default = generate_session(&GenParams { seed: 31, ..GenParams::default() });
    assert_eq!(
        serde_json::to_string(&explicit_none).unwrap(),
        serde_json::to_string(&default).unwrap(),
        "world: None must not move a hair"
    );
    let with = generate_session(&params(31, Some(load(DUST))));
    assert_ne!(
        serde_json::to_string(&with).unwrap(),
        serde_json::to_string(&default).unwrap(),
        "a world must change the session"
    );
}

#[test]
fn taste_weighted_selection_picks_each_shipped_corner() {
    let worlds = shipped();
    let profile = |genres: &[&str], energy: f32, darkness: f32| TasteProfile {
        genres: genres.iter().map(|g| (*g).to_string()).collect(),
        energy,
        darkness,
        ..TasteProfile::default()
    };
    let pick = |p: &TasteProfile| world::select_world(&worlds, p).map(|w| w.id.0.clone());
    assert_eq!(pick(&profile(&["microhouse"], 0.7, 0.7)), Some("dust".into()));
    assert_eq!(pick(&profile(&["dub"], 0.7, 0.7)), Some("fathom".into()));
    assert_eq!(pick(&profile(&["techno"], 0.75, 0.95)), Some("concrete".into()));
    assert_eq!(pick(&profile(&["techno"], 0.9, 0.3)), Some("pulse".into()));

    // Deterministic: same profile, same pick — regardless of slice order.
    let p = profile(&["dub techno"], 0.7, 0.9);
    let forward = world::select_world(&worlds, &p).map(|w| w.id.0.clone());
    let mut reversed = worlds.clone();
    reversed.reverse();
    let backward = world::select_world(&reversed, &p).map(|w| w.id.0.clone());
    assert_eq!(forward, backward, "slice order must not change the pick");
}

#[test]
fn morph_lands_at_the_section_boundary_and_stays_continuous() {
    let fathom = load(FATHOM);
    let mut session = generate_session(&params(7, Some(load(CONCRETE))));
    let boundary = 1; // first dev section

    // World A's audible pad value: the boundary section's own lane endpoint
    // when one exists (presence walk), else the static track value.
    let static_gain = |s: &Session, id: &str| {
        s.tracks.iter().find(|t| t.id == id).expect("track").gain
    };
    let audible_from = session.sections[boundary]
        .automation
        .get("pad")
        .filter(|l| l.target_param == "gain")
        .and_then(|l| l.points.last())
        .map(|(_, v, _)| *v)
        .unwrap_or_else(|| static_gain(&session, "pad"));
    let pre_sections = session.sections.clone();

    world::morph_world(&mut session, &fathom, boundary).expect("morph");

    // Palette re-voicing lands: concrete's 56 Hz kick gives way to fathom.
    let kick_tune = match &session.tracks.iter().find(|t| t.id == "kick").expect("kick").instrument {
        InstrumentDef::Kick(k) => k.tune_hz,
        other => panic!("kick is not a kick: {other:?}"),
    };
    assert!((kick_tune - 45.0).abs() < 1e-4, "fathom kick tune must land after morph");

    // Crossfade lane in the boundary section: A's audible value → B's value.
    let sec = &session.sections[boundary];
    let lane = sec.automation.get("pad").expect("crossfade lane on pad");
    assert_eq!(lane.target_param, "gain");
    assert_eq!(lane.points.len(), 2);
    let (start, end) = (lane.points[0].1, lane.points[1].1);
    assert!(
        (start - audible_from).abs() < 1e-3,
        "lane starts where world A actually sounds: {start} vs {audible_from}"
    );
    assert!((end - 0.55).abs() < 1e-4, "lane ends at world B's value: {end}");
    assert!(
        (static_gain(&session, "pad") - end).abs() < 1e-4,
        "post-morph static value == lane end, so the curve lands on B and stays"
    );

    // Bounded slew: the crossfade stays far under the 24 dB/bar ceiling.
    let span_bars = f64::from(sec.bars.saturating_sub(1)).max(1.0);
    let delta_db = (20.0 * f64::from(end / start).max(1e-4).log10()).abs();
    assert!(delta_db / span_bars <= 24.0, "slew {delta_db} dB / {span_bars} bars exceeds the ceiling");

    // No collateral damage: every other section's automation is untouched.
    for (si, (before, after)) in pre_sections.iter().zip(&session.sections).enumerate() {
        if si == boundary {
            continue;
        }
        assert_eq!(before.automation, after.automation, "section {si} automation must be untouched");
    }

    assert_eq!(
        world::morph_world(&mut session, &fathom, 99),
        Err(MorphError::Section { index: 99, sections: session.sections.len() })
    );
}

#[test]
fn world_sessions_stay_deterministic_and_valid() {
    let p = params(19, Some(load(PULSE)));
    let a = generate_session(&p);
    let b = generate_session(&p);
    assert_eq!(serde_json::to_string(&a).unwrap(), serde_json::to_string(&b).unwrap());
    validate_session(&a).expect("pulse session validates");
    assert_eq!(a.total_bars(), 128);
}
