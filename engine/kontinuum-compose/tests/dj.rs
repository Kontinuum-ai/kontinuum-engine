//! Live DJ facade tests (issue #38 steps 1–3): landing rules, one-shot
//! quantization and audibility, loop extension, tempo/key wiring,
//! determinism.

use kontinuum_clock::TempoLane;
use kontinuum_compose::{
    generate_session, landing_bar, quantized_bar, ArrangementEngine, DjDeck, GenParams,
    LiveMoveKind, LoopLength, OneShot, Quantize,
};
use kontinuum_ir::schema::{EuclideanPattern, EuclideanTag, MusicalKey, Pattern};
use kontinuum_ir::{ApplyError, Session};
use kontinuum_schedule::{BlockSource, CompiledBlock, Event};

const SR: u32 = 48_000;

fn engine(seed: u64) -> ArrangementEngine {
    let params = GenParams { seed, target_bars: 32, ..GenParams::default() };
    ArrangementEngine::new(generate_session(&params), SR)
}

fn kick(k: u32) -> Pattern {
    Pattern::Euclidean(EuclideanPattern {
        generator: EuclideanTag::Euclidean,
        k,
        n: 16,
        rot: 0,
        velocity: 0.9,
        probability: 1.0,
        repeats: 1,
        gate: None,
        pitch: None,
    })
}

fn kick_index(s: &Session) -> u8 {
    s.tracks.iter().position(|t| t.id == "kick").expect("kick track") as u8
}

fn noteon_count(block: &CompiledBlock, track: u8) -> usize {
    block
        .tracks
        .iter()
        .find(|t| t.track == track)
        .map(|t| t.events.iter().filter(|(_, e)| matches!(e, Event::NoteOn { .. })).count())
        .unwrap_or(0)
}

fn shot(track: &str, k: u32, quantize: Quantize) -> OneShot {
    OneShot { track: track.into(), pattern: kick(k), quantize }
}

#[test]
fn landing_rule_lands_at_section_boundaries_only() {
    let starts = [0u32, 8, 16, 24];
    assert_eq!(
        landing_bar(LiveMoveKind::Tempo, 9, &starts),
        Some(16),
        "mid-bar request lands at the next boundary"
    );
    assert_eq!(landing_bar(LiveMoveKind::Tempo, 8, &starts), Some(8), "on-boundary lands now");
    assert_eq!(landing_bar(LiveMoveKind::Tempo, 25, &starts), None);
    assert_eq!(
        landing_bar(LiveMoveKind::Loop, 9, &starts),
        Some(16),
        "loop lands at the playing section's end"
    );
    assert_eq!(landing_bar(LiveMoveKind::Loop, 8, &starts), Some(16));
    assert_eq!(landing_bar(LiveMoveKind::Key, 9, &starts), None, "key moves are session-level");
}

#[test]
fn one_shot_quantizes_to_bar_and_phrase() {
    assert_eq!(quantized_bar(Quantize::Bar, 5), 6);
    assert_eq!(quantized_bar(Quantize::Bar, 8), 9);
    assert_eq!(quantized_bar(Quantize::Phrase, 0), 8);
    assert_eq!(quantized_bar(Quantize::Phrase, 5), 8);
    assert_eq!(quantized_bar(Quantize::Phrase, 8), 16);
}

#[test]
fn djdeck_one_shot_lands_on_the_quantized_bar() {
    let mut engine = engine(7);
    let mut deck = DjDeck::new();
    let armed = deck
        .arm_one_shot(shot("kick", 16, Quantize::Bar), 5, engine.current_session())
        .expect("arm");
    assert_eq!(armed.landing_bar, 6);

    assert!(deck.tick(5, &mut engine).is_empty(), "not due before the landing bar");
    let mut landed = deck.tick(6, &mut engine);
    assert_eq!(landed.len(), 1);
    let landed = landed.pop().expect("one result").expect("fires at the landing bar");
    assert_eq!(landed.landing_bar, 6);
    assert_eq!(landed.section, engine.current_session().sections[0].id);

    assert!(
        matches!(
            engine.current_session().sections[0].pattern_bindings.get("kick"),
            Some(Pattern::Euclidean(ep)) if ep.k == 16
        ),
        "one-shot present in the pattern bindings"
    );
    let block = engine.block_for_bars(4, 4).expect("block covering the landing bar");
    assert_eq!(
        noteon_count(&block, kick_index(engine.current_session())),
        64,
        "16 onsets x 4 bars audible in the compiled output"
    );
}

#[test]
fn djdeck_phrase_quantize_lands_on_the_phrase_grid() {
    let mut engine = engine(8);
    let mut deck = DjDeck::new();
    let armed = deck
        .arm_one_shot(shot("kick", 16, Quantize::Phrase), 5, engine.current_session())
        .expect("arm");
    // Phrase quantize targets the fixed 8-bar grid, not section bounds —
    // the grammar (#16) may draw a longer intro, so resolve the section
    // that actually contains the landing bar.
    assert_eq!(armed.landing_bar, 8);
    let mut landed = deck.tick(8, &mut engine);
    let landed = landed.pop().expect("one result").expect("fires");
    assert_eq!(landed.landing_bar, 8);
    let session = engine.current_session();
    let starts = session.section_start_bars();
    let containing = starts
        .iter()
        .rposition(|&b| b <= 8)
        .map(|i| session.sections[i].id.clone())
        .expect("a section contains bar 8");
    assert_eq!(landed.section, containing);
}

#[test]
fn djdeck_set_tempo_lands_at_boundary_and_slews() {
    let mut engine = engine(9);
    let starts = engine.current_session().section_start_bars();
    let mut deck = DjDeck::new();
    let landed = deck.set_tempo(132.0, 5, &mut engine).expect("tempo move");
    assert_eq!(landed.landing_bar, starts[1], "lands at the next section boundary");
    assert_eq!(engine.current_session().tempo_lane.last(), Some(&(starts[1], 132.0)));

    let lane = TempoLane::new(SR, &engine.current_session().tempo_lane).expect("lane");
    // The starting tempo belongs to the style (the genre spec's), not a
    // hardcoded constant — read it back from the lane the session shipped.
    let base = lane.bpm_at_bar(0.0);
    assert_eq!(lane.bpm_at_bar(f64::from(starts[1])), 132.0);
    let mid = lane.bpm_at_bar(f64::from(starts[1]) / 2.0);
    assert!(base < mid && mid < 132.0, "tempo slews into the landing: {mid}");

    assert!(matches!(
        deck.set_tempo(300.0, 9, &mut engine),
        Err(ApplyError::Invalid(_))
    ));
    assert_eq!(engine.current_session().tempo_lane.len(), 2, "rejected move leaves the lane");
}

#[test]
fn djdeck_set_key_updates_hint() {
    let mut engine = engine(3);
    let mut deck = DjDeck::new();
    let landed = deck.set_key(MusicalKey::FMinor, 7, &mut engine).expect("key move");
    assert_eq!(landed.landing_bar, 7, "key moves land immediately");
    assert_eq!(engine.current_session().key.as_deref(), Some("F minor"));
}

#[test]
fn djdeck_loop_extends_playing_section() {
    let mut engine = engine(13);
    let starts = engine.current_session().section_start_bars();
    let playing = starts[1] + 1;
    let dev_bars = engine.current_session().sections[1].bars;
    let total = engine.current_session().total_bars();
    let mut deck = DjDeck::new();

    let looped = deck.loop_current_section(LoopLength::Half, playing, &mut engine).expect("loop");
    assert_eq!(looped.section, engine.current_session().sections[1].id);
    assert_eq!(looped.extra_bars, dev_bars / 2);
    assert_eq!(
        looped.landing_bar,
        starts[1] + dev_bars,
        "the loop becomes audible at the section's end"
    );
    assert_eq!(engine.current_session().sections[1].bars, dev_bars + looped.extra_bars);
    assert_eq!(engine.current_session().total_bars(), total + u64::from(looped.extra_bars));

    let block = engine
        .block_for_bars(looped.landing_bar, 4)
        .expect("appended bars compile into blocks");
    assert!(
        noteon_count(&block, kick_index(engine.current_session())) > 0,
        "the looped section's groove continues past the old end"
    );
}

#[test]
fn djdeck_arm_rejects_landing_past_session() {
    let mut engine = engine(5);
    let total = engine.current_session().total_bars() as u32;
    let mut deck = DjDeck::new();
    let armed = deck.arm_one_shot(shot("kick", 4, Quantize::Phrase), total - 1, engine.current_session());
    assert!(matches!(armed, Err(ApplyError::Invalid(_))));
    assert!(deck.tick(total - 1, &mut engine).is_empty(), "nothing was armed");
}

#[test]
fn djdeck_moves_are_deterministic() {
    fn perform(engine: &mut ArrangementEngine) -> String {
        let mut deck = DjDeck::new();
        deck.set_tempo(130.0, 5, engine).expect("tempo");
        deck.set_key(MusicalKey::AMinor, 5, engine).expect("key");
        deck.arm_one_shot(shot("kick", 16, Quantize::Bar), 9, engine.current_session())
            .expect("arm");
        for bar in 10..14 {
            for shot in deck.tick(bar, engine) {
                shot.expect("fires");
            }
        }
        deck.loop_current_section(LoopLength::Full, 11, engine).expect("loop");
        let total = engine.current_session().total_bars() as u32;
        let mut blocks = Vec::new();
        let mut bar = 0;
        while bar < total {
            let b = engine.block_for_bars(bar, 4).expect("block");
            bar += b.bars;
            blocks.push(format!("{b:?}"));
        }
        format!("{:?}\n{blocks:?}", engine.current_session())
    }

    assert_eq!(
        perform(&mut engine(21)),
        perform(&mut engine(21)),
        "same seed and same deck moves produce identical session and blocks"
    );
}
