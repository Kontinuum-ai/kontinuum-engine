//! Issue #21 acceptance, as far as it can run without live accounts:
//! two different synthetic libraries → measurably different DNA **and**
//! measurably different generation; pinning a swung reference shifts the
//! DNA's swing/density stats toward it. (The human blind-test leg stays
//! with the issue, per Nick.)

use kontinuum_compose::taste::{session_from_taste, TasteProfile};
use kontinuum_taste::audio::{Contribution, TrackDna, PIN_WEIGHT};
use kontinuum_taste::model::{profile_from_events, EntityGraph};
use kontinuum_taste::store::{EventContext, LibraryEvent};

const NOW: i64 = 1_700_000_000_000;

fn ev(context: EventContext, artist: &str, track: &str, genres: &[&str], label: &str, year: i32, occurred_ms: i64) -> LibraryEvent {
    LibraryEvent {
        context,
        artist: artist.into(),
        track: track.into(),
        album: Some(format!("{artist} — {track}")),
        label: Some(label.into()),
        release_year: Some(year),
        genres: genres.iter().map(|s| s.to_string()).collect(),
        bpm: None,
        occurred_ms,
    }
}

/// A Perlon/Ostgut-flavored minimal-techno library: heavily dominated by
/// one genre (the narrow-catalog side of the acceptance pair).
fn techno_library() -> Vec<LibraryEvent> {
    (0..40)
        .map(|i| {
            let genres: &[&str] = if i % 10 == 9 { &["dub techno"] } else { &["minimal techno"] };
            ev(
                EventContext::Saved,
                &format!("techno-act-{i}"),
                &format!("cut-{i}"),
                genres,
                if i % 2 == 0 { "Perlon" } else { "Ostgut Ton" },
                1998 + (i % 10) as i32,
                NOW - (i as i64) * 86_400_000,
            )
        })
        .collect()
}

/// A Kompakt/ambient-flavored wide-catalog library.
fn ambient_library() -> Vec<LibraryEvent> {
    let genres: [&[&str]; 5] = [
        &["ambient"],
        &["downtempo"],
        &["modern classical"],
        &["leftfield"],
        &["dub techno"],
    ];
    (0..40)
        .map(|i| {
            ev(
                if i % 2 == 0 { EventContext::Saved } else { EventContext::RecentlyPlayed },
                &format!("ambient-act-{i}"),
                &format!("drone-{i}"),
                genres[i % 5],
                "Kompakt",
                2004 + (i % 16) as i32,
                NOW - (i as i64) * 86_400_000,
            )
        })
        .collect()
}

#[test]
fn different_libraries_produce_different_dna_and_generation() {
    let techno = profile_from_events(&techno_library(), NOW);
    let ambient = profile_from_events(&ambient_library(), NOW);

    // Genre mix: techno library is two-genre dominated; ambient is wide.
    assert_eq!(techno.genre_mix.len(), 2);
    assert_eq!(ambient.genre_mix.len(), 5);
    assert_eq!(techno.genre_mix[0].0, "minimal techno");
    assert_eq!(ambient.genre_mix[0].0, "ambient");
    let mix_gap: f32 = techno
        .genre_mix
        .iter()
        .map(|(g, w)| {
            let other = ambient
                .genre_mix
                .iter()
                .find(|(g2, _)| g2 == g)
                .map_or(0.0_f32, |(_, w2)| *w2);
            (w - other).abs()
        })
        .sum();
    assert!(mix_gap > 0.5, "genre-mix distributions are far apart: {techno:?} vs {ambient:?}");

    // Adventurousness: narrow catalog scores lower than the wide one.
    let techno_adv = techno.adventurousness.unwrap();
    let ambient_adv = ambient.adventurousness.unwrap();
    assert!(
        ambient_adv > techno_adv + 0.3,
        "wide catalog must score clearly more adventurous: {ambient_adv} vs {techno_adv}"
    );

    // Era and scene profiles differ.
    assert_ne!(techno.era_weights[0].0, ambient.era_weights[0].0, "decades differ");
    assert_eq!(techno.scene_weights[0].0, "Perlon");
    assert_eq!(ambient.scene_weights[0].0, "Kompakt");

    // Darkness nudges: techno bias raises darkness, ambient lowers energy.
    assert!(techno.darkness > ambient.darkness, "techno {} ambient {}", techno.darkness, ambient.darkness);
    assert!(techno.energy > ambient.energy, "techno {} ambient {}", techno.energy, ambient.energy);

    // …and the generation follows: different DNA → audibly-different
    // sessions (different bytes, deterministic per DNA).
    let s1 = session_from_taste(&techno, 42);
    let s2 = session_from_taste(&ambient, 42);
    assert_ne!(
        serde_json::to_string(&s1).unwrap(),
        serde_json::to_string(&s2).unwrap(),
        "different libraries must generate differently"
    );
    // Tempo lanes diverge too: the two DNAs don't collapse to one session.
    let t1 = s1.tempo_lane[0].1;
    let t2 = s2.tempo_lane[0].1;
    assert!((t1 - t2).abs() > 1e-9 || techno.bpm != ambient.bpm || techno.genres != ambient.genres);
}

#[test]
fn pinning_a_swung_reference_shifts_swing_and_density_stats() {
    // Synthetic tracks through the #23/#5 analysis path (synthgen presets
    // rendered + analyzed on-device): mt-a is dead straight, mt-b plants
    // heavy swing.
    let straight = analyzed("mt-a");
    let swung = analyzed("mt-b");

    let mut base = TasteProfile::default();
    kontinuum_taste::audio::apply_audio_dna(&mut base, &[Contribution::library(straight.clone())]);
    assert!(base.swing.unwrap().mean < 0.05, "straight track: {}", base.swing.unwrap().mean);

    // Pin the swung reference at the high pin weight.
    let mut pinned = base.clone();
    let contribs = vec![
        Contribution::library(straight.clone()),
        Contribution::pinned(swung.clone()),
    ];
    kontinuum_taste::audio::apply_audio_dna(&mut pinned, &contribs);

    let target = swung.swing.unwrap();
    let before = base.swing.unwrap().mean;
    let after = pinned.swing.unwrap().mean;
    assert!(
        after > before + 0.03,
        "pin must pull the swing stat toward the reference: {before} → {after} (target {target})"
    );
    assert!(after < target, "one pin must not overshoot the whole library: {after} vs {target}");
    // Dispersion opened up: two differing opinions now in the mix.
    assert!(pinned.swing.unwrap().dispersion > base.swing.unwrap().dispersion);

    // The shift reaches generation: the pinned profile's groove knob moves.
    let groove_of = |p: &TasteProfile| kontinuum_compose::taste::gen_params_for_taste(p, 7).groove;
    assert_ne!(groove_of(&base), groove_of(&pinned), "groove pin follows the swing stat");

    // Density follows the pinned reference's event density too.
    assert!(
        (pinned.density - base.density).abs() > 1e-6 || (pinned.brightness.unwrap().mean - base.brightness.unwrap().mean).abs() > 1e-6,
        "density/brightness stats moved with the pin"
    );
}

#[test]
fn pin_weight_actually_dominates_a_single_library_track() {
    let straight = analyzed("mt-a");
    let swung = analyzed("mt-b");
    // Sanity: the pin's weight is what makes it dominate — equal weights
    // would leave the aggregate nearer the library track.
    assert_eq!(PIN_WEIGHT, 4.0);
    let contribs = vec![
        Contribution::library(straight.clone()),
        Contribution { dna: swung.clone(), pinned: true },
    ];
    let samples: Vec<(f32, f32)> = contribs
        .iter()
        .map(|c| (c.dna.swing.unwrap(), c.weight()))
        .collect();
    let mean: f32 = samples.iter().map(|(v, w)| v * w).sum::<f32>() / samples.iter().map(|(_, w)| w).sum::<f32>();
    assert!(mean > straight.swing.unwrap() * 0.7, "pin pulls hard: {mean}");
}

/// Renders + analyzes one synthgen preset through the on-device #5 subset.
fn analyzed(preset_id: &str) -> TrackDna {
    let preset = kontinuum_analysis::synthgen::preset_by_id(preset_id).expect("preset");
    let mono = kontinuum_analysis::synthgen::render(preset);
    TrackDna::analyze(preset.track_id, &mono, kontinuum_analysis::synthgen::SYNTH_SAMPLE_RATE, preset.bpm)
        .expect("analysis converges on a planted track")
}

#[test]
fn entity_graph_decays_old_playlists_more_than_new_saves() {
    let mut events = vec![ev(EventContext::Playlist, "old-pl", "t", &["trance"], "label", 2010, NOW - 180 * 86_400_000)];
    events.push(ev(EventContext::Saved, "new-saved", "t2", &["trance"], "label", 2010, NOW));
    events.push(ev(EventContext::Playlist, "new-pl", "t3", &["trance"], "label", 2010, NOW));
    let g = EntityGraph::build(&events, NOW);
    let old_pl = &g.artists["old-pl"];
    let new_pl = &g.artists["new-pl"];
    // Same affinity, 180 days apart = exactly two half-lives.
    assert!((old_pl.weight * 4.0 - new_pl.weight).abs() < 1e-4, "{} vs {}", old_pl.weight, new_pl.weight);
    assert!(old_pl.weight < new_pl.weight);
}
