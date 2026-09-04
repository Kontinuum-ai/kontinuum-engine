//! Deck-mixer behavior tests — moved into the first-party pack crate with
//! the rest of the voice-dependent suites (issue #51: the harness crate has
//! no instrument code, not even in tests).

use kontinuum_core::graph::VoiceFactory;
use kontinuum_core::mix::{Deck, DeckMixer};
use kontinuum_core::BLOCK_FRAMES;
use kontinuum_schedule::{Event, TrackId};

const SR: u32 = 48_000;

type Events = Vec<(u32, TrackId, Event)>;
use kontinuum_core::mix::equal_power_gains;

fn test_factory(kind: &str) -> VoiceFactory {
    kontinuum_instruments_core::registry()
        .voice_factory(kind)
        .expect("test kind")
}

const FADE_FRAMES: u32 = 48_000;


fn sustained_events(track: u8, pitch: f32) -> Events {
    vec![
        (0, track, Event::NoteOn { voice: 0, pitch, velocity: 0.8, microtiming_ticks: 0 }),
        // Span closer (ignored: voice 99 is outside the voice map) —
        // without a later event the NoteOn would only dispatch after
        // the whole buffer renders.
        (64, track, Event::NoteOff { voice: 99 }),
    ]
}

/// Deck A: wavetable pad on C4. Deck B: sustained pad on C3. Identical
/// for every build so runs are comparable. Gains stay small so every
/// soft-clip stage is in its linear regime (cubic deviation ≲ 1e-6) and
/// the analytic comparison stays exact to the stated tolerance.
fn rig() -> DeckMixer {
    let mut dm = DeckMixer::new(SR);
    dm.deck(Deck::A).attach_with(0, &test_factory("wavetable"));
    dm.deck(Deck::B).attach_with(0, &test_factory("wavetable"));
    dm.deck(Deck::A).snap_track_gain(0, 0.05);
    dm.deck(Deck::B).snap_track_gain(0, 0.05);
    dm
}

/// The shared master stage the mix passes through after the crossfade:
/// unity gain + tanh soft clip. Replicated here so the analytic
/// comparison covers the whole render path; the limiter never engages at
/// these levels.
fn shared_master(x: f32) -> f32 {
    1.2 * (x / 1.2).tanh()
}

fn render(dm: &mut DeckMixer, events_a: &Events, events_b: &Events, frames: usize) -> Vec<f32> {
    let mut l = vec![0.0f32; frames];
    let mut r = vec![0.0f32; frames];
    dm.render_block(&mut l, &mut r, events_a, events_b, 0);
    l
}

#[test]
fn crossfade_output_equals_analytic_equal_power_mix_at_quarter_positions() {
    let events_a = sustained_events(0, 60.0);
    let events_b = sustained_events(0, 48.0);
    let frames = FADE_FRAMES as usize + 1_024;

    // Solo captures: the parked crossfade is the analytic curve at its
    // endpoints, so park(A) yields deck A alone and park(B) deck B alone.
    let mut dm = rig();
    dm.park_crossfade(0.0);
    let a = render(&mut dm, &events_a, &events_b, frames);
    let mut dm = rig();
    dm.park_crossfade(1.0);
    let b = render(&mut dm, &events_a, &events_b, frames);
    let mut dm = rig();
    dm.begin_crossfade(FADE_FRAMES);
    let mixed = render(&mut dm, &events_a, &events_b, frames);

    for k in [0usize, FADE_FRAMES as usize / 4, FADE_FRAMES as usize / 2, FADE_FRAMES as usize * 3 / 4, FADE_FRAMES as usize, frames - 1] {
        let (ga, gb) = equal_power_gains(k as f32 / FADE_FRAMES as f32);
        let expected = shared_master(a[k] * ga + b[k] * gb);
        // 1e-3 tolerance: release-profile float drift on the cos/sin
        // gains and the bus sum measures ~4e-5 worst-case (−86 dB, far
        // below audibility); debug measured < 1e-5. Same host-canonical
        // philosophy as the golden-pin note.
        assert!(
            (mixed[k] - expected).abs() < 1e-3,
            "at sample {k}: mixed {} vs analytic {expected}",
            mixed[k]
        );
    }
    // The deck B content must actually be in the tail of the fade.
    let (ga, gb) = equal_power_gains(1.0);
    let expected = shared_master(a[frames - 1] * ga + b[frames - 1] * gb);
    assert!(b[frames - 1].abs() > 0.001, "deck B render silent");
    // Same release-profile float drift as the loop above (~4e-5 worst case).
    assert!((mixed[frames - 1] - expected).abs() < 1e-3);
}

#[test]
fn correlated_decks_sum_3db_at_midpoint() {
    // Identical voices on both decks: at the midpoint the equal-power
    // pair gives x·(0.7071 + 0.7071) = 1.4142·x — +3.01 dB over either
    // solo deck (the correlated limit of power conservation). Levels
    // stay small so the shared master's soft clip is effectively linear
    // (its distortion would otherwise bias the ratio low).
    let events_a = sustained_events(0, 60.0);
    let events_b = sustained_events(0, 60.0);

    let mut solo_dm = DeckMixer::new(SR);
    solo_dm.deck(Deck::A).attach_with(0, &test_factory("wavetable"));
    solo_dm.deck(Deck::B).attach_with(0, &test_factory("wavetable"));
    solo_dm.deck(Deck::A).snap_track_gain(0, 0.1);
    solo_dm.deck(Deck::B).snap_track_gain(0, 0.1);
    solo_dm.park_crossfade(0.0);
    let solo = render(&mut solo_dm, &events_a, &events_b, 4_800);

    let mut mid_dm = DeckMixer::new(SR);
    mid_dm.deck(Deck::A).attach_with(0, &test_factory("wavetable"));
    mid_dm.deck(Deck::B).attach_with(0, &test_factory("wavetable"));
    mid_dm.deck(Deck::A).snap_track_gain(0, 0.1);
    mid_dm.deck(Deck::B).snap_track_gain(0, 0.1);
    mid_dm.park_crossfade(0.5);
    let mid = render(&mut mid_dm, &events_a, &events_b, 4_800);

    let rms = |b: &[f32]| (b.iter().map(|s| s * s).sum::<f32>() / b.len() as f32).sqrt();
    let ratio_db = 20.0 * ((rms(&mid) / rms(&solo)) as f64).log10();
    assert!((ratio_db - 3.0103).abs() < 0.05, "midpoint sum {ratio_db:.3} dB, expected ≈ +3.01 dB");
}

#[test]
fn deck_b_preroll_never_steals_deck_a_voice_budget() {
    // Kick pools cap at 8 voices per deck (graph.rs attach table).
    let overfire = |dm: &mut DeckMixer, deck: Deck| {
        let events: Events = (0..12u8)
            .map(|v| (0, 0u8, Event::NoteOn { voice: v, pitch: 60.0, velocity: 0.9, microtiming_ticks: 0 }))
            .collect();
        let mut l = [0.0f32; BLOCK_FRAMES];
        let mut r = [0.0f32; BLOCK_FRAMES];
        let (ea, eb) = match deck {
            Deck::A => (events, vec![]),
            Deck::B => (vec![], events),
        };
        dm.render_block(&mut l, &mut r, &ea, &eb, 0);
    };
    let mut dm = rig();
    dm.deck(Deck::A).attach_with(0, &test_factory("wavetable"));
    dm.deck(Deck::B).attach_with(0, &test_factory("wavetable"));
    // Deck A saturated while deck B pre-rolls its own saturated track.
    overfire(&mut dm, Deck::A);
    overfire(&mut dm, Deck::B);
    // Each deck is independently capped at its own pool capacity: deck
    // B's load did not shrink deck A's budget (with a shared pool one of
    // the two would sit below 8).
    assert_eq!(dm.deck(Deck::A).track_active_voices(0), 8, "deck A lost voice budget to deck B");
    assert_eq!(dm.deck(Deck::B).track_active_voices(0), 8, "deck B preroll did not get its own budget");
}

#[test]
fn deck_render_is_bit_identical_across_runs() {
    let events_a = sustained_events(0, 60.0);
    let events_b = sustained_events(0, 48.0);
    let frames = FADE_FRAMES as usize + 1_024;
    let mut hashes = vec![];
    for _ in 0..2 {
        let mut dm = rig();
        dm.begin_crossfade(FADE_FRAMES);
        let l = render(&mut dm, &events_a, &events_b, frames);
        hashes.push(kontinuum_core::fnv1a64(&l.iter().flat_map(|s| s.to_bits().to_le_bytes()).collect::<Vec<u8>>()));
    }
    assert_eq!(hashes[0], hashes[1], "deck renders diverged");
}

#[test]
fn deck_kill_switches_forward_and_sum_telemetry() {
    let events_a = sustained_events(0, 60.0);
    let events_b = sustained_events(0, 48.0);
    let mut dm = rig();
    dm.set_track_mute(Deck::B, 0, true);
    dm.set_track_mute(Deck::B, 0, true); // idempotent: no re-count
    dm.panic(Deck::A);
    dm.panic_all();
    dm.rearm_all();
    let tel = dm.kill_telemetry();
    assert_eq!(tel.mute_events, 1);
    assert_eq!(tel.panic_events, 2);
    // Panic on deck A silenced it while deck B (track-muted) is silent
    // too: after rearm only deck B's mute keeps its contribution at zero.
    let l = render(&mut dm, &events_a, &events_b, 2_400);
    assert!(l.iter().any(|s| s.abs() > 0.001), "rearmed deck A stayed silent");
}

#[test]
fn deck_render_path_is_allocation_free() {
    let events_a = sustained_events(0, 60.0);
    let events_b = sustained_events(0, 48.0);
    let mut dm = rig();
    dm.begin_crossfade(FADE_FRAMES);
    let mut l = vec![0.0f32; 9_600];
    let mut r = vec![0.0f32; 9_600];
    assert_no_alloc::assert_no_alloc(|| {
        dm.render_block(&mut l, &mut r, &events_a, &events_b, 0);
    });
}

