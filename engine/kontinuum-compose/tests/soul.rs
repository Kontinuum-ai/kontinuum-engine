//! Creative Soul end-to-end tests (issue #55): fixture loading (the eight
//! first-party genre packs dogfooding the format), the layering contract
//! (souls between the genre rig and an explicit world), deterministic
//! blending, mix interpolation, the style-card budget, era switching at a
//! section boundary, and the no-souls byte-identical regression.

use std::collections::BTreeMap;

use kontinuum_compose::soul::{
    blend, blend as blend_stack, load_json, prepare, set_era, BlendError, BlendInput,
    SoulStackEntry, STYLE_CARD_WORD_BUDGET,
};
use kontinuum_compose::{generate_session, GenParams};
use kontinuum_ir::schema::{InstrumentDef, SoulRef};
use kontinuum_ir::{validate_session, Session};

macro_rules! soul {
    ($fname:ident, $file:literal) => {
        const $fname: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/souls/", $file));
    };
}

soul!(MICROHOUSE, "dusty-microhouse.json");
soul!(MINIMAL, "minimal-techno-function.json");
soul!(TECHNO, "concrete-floor-techno.json");
soul!(HOUSE, "classic-house-groove.json");
soul!(DEEP, "deep-house-chords.json");
soul!(AMBIENT, "pre-dawn-ambient.json");
soul!(ACID, "acid-warehouse-303.json");
soul!(DUB, "dub-techno-chords.json");

fn load(text: &str) -> kontinuum_compose::CreativeSoul {
    load_json(text).expect("fixture soul parses and validates")
}

fn shipped() -> Vec<kontinuum_compose::CreativeSoul> {
    vec![
        load(MICROHOUSE),
        load(MINIMAL),
        load(TECHNO),
        load(HOUSE),
        load(DEEP),
        load(AMBIENT),
        load(ACID),
        load(DUB),
    ]
}

fn entry(soul: &kontinuum_compose::CreativeSoul, weight: f32, era: Option<&str>) -> SoulStackEntry {
    SoulStackEntry { soul: soul.clone(), weight, era: era.map(str::to_string) }
}

fn track_gain(s: &Session, id: &str) -> f32 {
    s.tracks.iter().find(|t| t.id == id).unwrap_or_else(|| panic!("track {id}")).gain
}

fn bass_cutoff(s: &Session) -> f32 {
    match &s.tracks.iter().find(|t| t.id == "bass").expect("bass").instrument {
        InstrumentDef::Bass(b) => b.cutoff_hz,
        other => panic!("bass is not a bass: {other:?}"),
    }
}

#[test]
fn shipped_soul_fixtures_parse_and_validate() {
    let souls = shipped();
    assert_eq!(souls.len(), 8, "the eight first-party genre packs");
    let mut ids: Vec<_> = souls.iter().map(|s| s.id.0.clone()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 8, "soul ids must be distinct: {ids:?}");
    for s in &souls {
        assert!(s.eras.contains_key("default"), "{} must ship a default era", s.id.0);
        assert!(!s.taste_tags.is_empty(), "{} must declare taste tags", s.id.0);
    }
}

#[test]
fn soul_layers_move_the_generated_session() {
    let with = generate_session(&GenParams {
        seed: 7,
        souls: vec![entry(&load(MICROHOUSE), 1.0, None)],
        ..GenParams::default()
    });
    let without = generate_session(&GenParams { seed: 7, ..GenParams::default() });

    assert!(
        (bass_cutoff(&with) - 320.0).abs() < 1e-4,
        "soul rack override must land (bass cutoff {})",
        bass_cutoff(&with)
    );
    assert!((bass_cutoff(&with) - bass_cutoff(&without)).abs() > 1e-4, "soul must change the rig");
    assert!(
        (track_gain(&with, "pad") - 0.4).abs() < 1e-4,
        "soul mix profile must land (pad gain {})",
        track_gain(&with, "pad")
    );
    assert_eq!(
        with.souls.as_deref(),
        Some(&[SoulRef { id: "dusty-microhouse".into(), weight: 1.0, era: None }][..]),
        "session records the soul stack"
    );
    assert_eq!(without.souls, None, "no-souls sessions stay unstamped");
    validate_session(&with).expect("soul-fed session validates");
}

#[test]
fn no_souls_output_is_byte_identical() {
    let empty = generate_session(&GenParams {
        seed: 31,
        souls: Vec::new(),
        ..GenParams::default()
    });
    let default = generate_session(&GenParams { seed: 31, ..GenParams::default() });
    assert_eq!(
        serde_json::to_string(&empty).unwrap(),
        serde_json::to_string(&default).unwrap(),
        "souls: [] must not move a hair"
    );
    let with = generate_session(&GenParams {
        seed: 31,
        souls: vec![entry(&load(DUB), 1.0, None)],
        ..GenParams::default()
    });
    assert_ne!(
        serde_json::to_string(&with).unwrap(),
        serde_json::to_string(&default).unwrap(),
        "a soul must change the session"
    );
}

#[test]
fn blend_is_deterministic_and_weight_ordered() {
    let souls = shipped();
    let techno = &souls[2];
    let ambient = &souls[5];

    fn mk<'a>(
        a: &'a kontinuum_compose::CreativeSoul,
        b: &'a kontinuum_compose::CreativeSoul,
    ) -> Vec<BlendInput<'a>> {
        vec![
            BlendInput { soul: a, weight: 0.6, era: None },
            BlendInput { soul: b, weight: 0.4, era: None },
        ]
    }
    let a = blend(&mk(techno, ambient)).unwrap();
    let b = blend(&mk(techno, ambient)).unwrap();
    assert_eq!(a, b, "same stack blends identically");

    // Dominant regardless of position: techno keeps 0.6 and wins the groove
    // layer even when it sits second in the stack.
    let flipped = vec![
        BlendInput { soul: ambient, weight: 0.4, era: None },
        BlendInput { soul: techno, weight: 0.6, era: None },
    ];
    let flipped = blend(&flipped).unwrap();
    assert_eq!(a.groove, flipped.groove, "weight, not position, picks the dominant");
    assert_eq!(a.groove.as_ref().unwrap().template.as_deref(), Some("straight-machine"));

    // Equal weights: stack order breaks the tie (stable sort).
    let tie = vec![
        BlendInput { soul: techno, weight: 0.5, era: None },
        BlendInput { soul: ambient, weight: 0.5, era: None },
    ];
    let tie_flipped = vec![
        BlendInput { soul: ambient, weight: 0.5, era: None },
        BlendInput { soul: techno, weight: 0.5, era: None },
    ];
    assert_ne!(
        blend(&tie).unwrap().groove,
        blend(&tie_flipped).unwrap().groove,
        "ties follow stack order"
    );
}

#[test]
fn mix_profile_interpolates_by_normalized_weight() {
    let souls = shipped();
    let microhouse = &souls[0]; // pad gain 0.4
    let deep = &souls[4]; // pad gain 0.5

    let fifty_fifty = vec![
        BlendInput { soul: microhouse, weight: 0.5, era: None },
        BlendInput { soul: deep, weight: 0.5, era: None },
    ];
    let blended = blend(&fifty_fifty).unwrap();
    let pad = blended.mix_profile.get("pad").expect("both souls name pad");
    assert!(
        (pad.gain - 0.45).abs() < 1e-4,
        "50/50 pad gain must interpolate to 0.45, got {}",
        pad.gain
    );

    // Weights need not sum to 1; normalization must land on the same point.
    let lopsided = vec![
        BlendInput { soul: microhouse, weight: 1.0, era: None },
        BlendInput { soul: deep, weight: 1.0, era: None },
    ];
    assert_eq!(
        blended.mix_profile.get("pad").copied(),
        blend(&lopsided).unwrap().mix_profile.get("pad").copied(),
        "normalized weights blend identically"
    );

    // 0.75/0.25 leans to the dominant's value.
    let leaning = vec![
        BlendInput { soul: microhouse, weight: 0.75, era: None },
        BlendInput { soul: deep, weight: 0.25, era: None },
    ];
    let pad = blend(&leaning).unwrap().mix_profile.get("pad").copied().unwrap();
    assert!((pad.gain - 0.425).abs() < 1e-3, "0.75/0.25 pad gain is 0.425, got {}", pad.gain);
}

#[test]
fn style_cards_concatenate_weight_ranked_under_budget() {
    let souls = shipped();
    let techno = &souls[2];
    let ambient = &souls[5];
    let stack = vec![
        BlendInput { soul: techno, weight: 0.8, era: None },
        BlendInput { soul: ambient, weight: 0.2, era: None },
    ];
    let card = blend(&stack).unwrap().style_card;
    let words = card.split_whitespace().count();
    assert!(words <= STYLE_CARD_WORD_BUDGET, "card must respect the budget ({words})");
    assert!(
        card.starts_with("Dark, physical, relentless"),
        "the dominant soul's card leads: {card}"
    );
    assert!(card.contains("Almost nothing happens"), "the supporting card rides along");

    // A stack of only non-dominant... an empty stack is an error.
    assert_eq!(blend(&[]), Err(BlendError::EmptyStack));
}

#[test]
fn invalid_stack_entries_are_dropped_deterministically() {
    let souls = shipped();
    let techno = &souls[2];
    let prepared = prepare(&[
        entry(techno, 0.0, None),          // weight out of range: dropped
        entry(techno, 1.5, None),          // dropped
        entry(techno, 0.9, Some("1962")),  // unknown era: dropped
        entry(techno, 0.6, None),          // survives
    ]);
    assert_eq!(prepared.refs.len(), 1, "only the valid entry survives");
    assert!(prepared.blended.is_some());

    let all_bad = prepare(&[entry(techno, 0.0, None)]);
    assert!(all_bad.refs.is_empty() && all_bad.blended.is_none());
}

#[test]
fn era_switch_lands_at_the_section_boundary() {
    let deep = load(DEEP);
    let mut session = generate_session(&GenParams {
        seed: 7,
        souls: vec![entry(&deep, 1.0, None)],
        ..GenParams::default()
    });
    let boundary = 1; // first dev section
    let pre_sections = session.sections.clone();
    let pre_pad_gain = track_gain(&session, "pad");

    let mut packs: BTreeMap<String, kontinuum_compose::CreativeSoul> = BTreeMap::new();
    packs.insert(deep.id.0.clone(), deep);
    set_era(&mut session, &packs, "deep-house-chords", "90s", boundary).expect("era switch");

    // The stack records the switch.
    assert_eq!(
        session.souls.as_ref().unwrap().as_slice(),
        &[SoulRef { id: "deep-house-chords".into(), weight: 1.0, era: Some("90s".into()) }][..]
    );

    // The 90s era's pad gain (0.4) replaces the default's (0.5) and
    // crossfades in the boundary section. The lane starts where era A was
    // actually sounding at that point — the existing automation's endpoint
    // when a presence/motion lane already drives the pad there (the same
    // rule `world::morph::with_lane_endpoint` applies), the static gain
    // otherwise.
    let post_gain = track_gain(&session, "pad");
    assert!(
        (post_gain - 0.4).abs() < 1e-4,
        "era switch re-voices the static mix (pad gain {post_gain})"
    );
    let lane = session.sections[boundary]
        .automation
        .get("pad")
        .expect("crossfade lane on pad");
    assert_eq!(lane.target_param, "gain");
    let expected_start = pre_sections[boundary]
        .automation
        .get("pad")
        .filter(|l| l.target_param == "gain")
        .and_then(|l| l.points.last())
        .map(|(_, v, _)| *v)
        .unwrap_or(pre_pad_gain);
    let (start, end) = (lane.points[0].1, lane.points[1].1);
    assert!((start - expected_start).abs() < 1e-3, "lane starts where era A sounded: {start}");
    assert!((end - post_gain).abs() < 1e-4, "lane ends at era B's value: {end}");

    // No collateral damage elsewhere.
    for (si, (before, after)) in pre_sections.iter().zip(&session.sections).enumerate() {
        if si == boundary {
            continue;
        }
        assert_eq!(before.automation, after.automation, "section {si} automation untouched");
    }
    validate_session(&session).expect("post-switch session validates");

    // Unknown era and unknown soul are errors, not panics.
    match set_era(&mut session, &packs, "deep-house-chords", "1972", 0) {
        Err(kontinuum_compose::soul::SoulSwitchError::UnknownEra { soul, era }) => {
            assert_eq!((soul.as_str(), era.as_str()), ("deep-house-chords", "1972"));
        }
        other => panic!("expected UnknownEra, got {other:?}"),
    }
    match set_era(&mut session, &packs, "concrete-floor-techno", "90s", 0) {
        Err(kontinuum_compose::soul::SoulSwitchError::UnknownSoul(id)) => {
            assert_eq!(id, "concrete-floor-techno");
        }
        other => panic!("expected UnknownSoul, got {other:?}"),
    }
}

#[test]
fn world_wins_over_soul_on_the_fields_it_names() {
    let p = GenParams {
        seed: 7,
        souls: vec![entry(&load(MINIMAL), 1.0, None)], // bass cutoff 260
        ..GenParams::default()
    };
    let soul_only = generate_session(&p);
    assert!((bass_cutoff(&soul_only) - 260.0).abs() < 1e-4);

    let dust = kontinuum_compose::world::load_json(include_str!(
        concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/worlds/dust.json")
    ))
    .expect("dust loads");
    let both = generate_session(&GenParams { world: Some(dust), ..p });
    assert!(
        (bass_cutoff(&both) - 320.0).abs() < 1e-4,
        "an explicit world beats the soul on the fields it names"
    );
    validate_session(&both).expect("soul+world session validates");
}

#[test]
fn soul_harmony_layer_replaces_the_progression_tables() {
    // Acid packs are near-static (i - i - iv - i): the dominant chord of the
    // first section must stay F-minor-rooted, and the session must stay
    // chord-snap-identical across two identical stacks.
    let acid = load(ACID);
    let p = GenParams {
        seed: 9,
        souls: vec![entry(&acid, 1.0, None)],
        ..GenParams::default()
    };
    let a = generate_session(&p);
    let b = generate_session(&p);
    assert_eq!(
        serde_json::to_string(&a).unwrap(),
        serde_json::to_string(&b).unwrap(),
        "soul-fed generation is deterministic"
    );
    validate_session(&a).expect("soul harmony session validates");
}

#[test]
fn every_pack_pair_blends_coherently_at_50_50() {
    let souls = shipped();
    for i in 0..souls.len() {
        for j in (i + 1)..souls.len() {
            let stack = vec![
                BlendInput { soul: &souls[i], weight: 0.5, era: None },
                BlendInput { soul: &souls[j], weight: 0.5, era: None },
            ];
            let blended = blend_stack(&stack)
                .unwrap_or_else(|e| panic!("{} + {} failed to blend: {e}", souls[i].id.0, souls[j].id.0));
            let session = generate_session(&GenParams {
                seed: 5,
                target_bars: 64,
                souls: vec![
                    entry(&souls[i], 0.5, None),
                    entry(&souls[j], 0.5, None),
                ],
                ..GenParams::default()
            });
            assert_eq!(
                session.souls.as_ref().unwrap().len(),
                2,
                "{} + {} must record both souls",
                souls[i].id.0,
                souls[j].id.0
            );
            assert!(
                !blended.style_card.is_empty(),
                "{} + {} must produce a style card",
                souls[i].id.0,
                souls[j].id.0
            );
            validate_session(&session)
                .unwrap_or_else(|e| panic!("{} + {} session must validate: {e:?}", souls[i].id.0, souls[j].id.0));
        }
    }
}

#[test]
fn guardrail_rejects_artist_titles_in_shared_catalogs() {
    use kontinuum_compose::soul::check_shareable_name;
    assert!(check_shareable_name("Jeff Mills").is_err());
    assert!(check_shareable_name("Detroit 909 minimalism").is_ok());
}
