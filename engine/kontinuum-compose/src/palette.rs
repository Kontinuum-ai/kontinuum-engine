//! Track palette and per-track pattern builders for the arrangement engine.
//!
//! Since #88 the rig is per-genre: every style's rack (see
//! [`crate::genre::GenreSpec::rack`]) is a selection from the full 12-voice
//! palette, and this module materializes rack entries into session tracks
//! with their mixer/FX defaults attached. The legacy six-track rig is the
//! default (unnamed-genre) rack.
//!
//! Pattern builders map a section's energy onto density and velocity
//! (`0.4 + 0.5·energy`), baking swing and humanization into explicit steps via
//! the [`crate::pattern`] toolkit. Pitched material is written against a
//! placeholder root and transposed onto the section's chord by
//! [`crate::harmony::retune_section`], so motifs keep their shape.

use kontinuum_clock::{PPQ, Rng, TICKS_PER_BAR};
use kontinuum_ir::schema::{
    EuclideanPattern, EuclideanTag, DownbeatCollision, InsertDef, InsertKind, InstrumentDef,
    Pattern, ProbabilityMaskPattern, ProbabilityMaskTag, SampleSlot, SampleTag, Sends, Step,
    StepsPattern, Track,
};
use serde_json::json;

use crate::arrangement::{Kind, SectionPlan};
use crate::genre::{HatIdiom, RackEntry, Voice};
use crate::ghost;
use crate::pattern::{
    apply_swing, euclidean, first_onset_rot, humanize, humanize_gauss, jitter_sigma, ratchet_steps,
    VelContour,
};

const SLOT_TICKS: u64 = TICKS_PER_BAR / 16;
const BAR_END_TICKS: i64 = TICKS_PER_BAR as i64 - 1;
/// Ticks per beat and per eighth at the fixed 4/4 grid.
const BEAT_TICKS: u32 = PPQ;
const EIGHTH_TICKS: u32 = PPQ / 2;
/// Tick positions of the "and" of each beat — the offbeat-eighth line.
const OFFBEAT_EIGHTHS: [i64; 4] = [240, 720, 1200, 1680];

/// Materializes one rack entry into a session track: instrument (with the
/// sample query filled in for texture voices — the static rack tables cannot
/// hold a `String`), the kick's drive insert, and the rack's own mixer/FX
/// defaults.
pub(crate) fn voice_track(entry: &RackEntry) -> Track {
    let mut instrument = entry.inst.clone();
    if let InstrumentDef::Sample(slot @ SampleSlot { kind: SampleTag::Sample, .. }) = instrument {
        instrument = InstrumentDef::Sample(SampleSlot {
            query: Some(entry.sample_query.to_string()),
            ..slot
        });
    }
    // The kick keeps the drive insert the legacy rig shipped; every other
    // voice's colour lives in its instrument and sends.
    let inserts = match entry.voice {
        Voice::Kick => {
            vec![InsertDef { kind: InsertKind::Drive, params: json!({ "amount": 1.2 }), mix: 0.4 }]
        }
        _ => vec![],
    };
    Track {
        id: entry.id.into(),
        role: entry.role,
        instrument,
        inserts,
        sends: Sends { delay: entry.sends.delay, reverb: entry.sends.reverb },
        gain: entry.gain,
        pan: entry.pan,
        duck_depth: entry.duck,
    }
}

fn velocity_for(energy: f32) -> f32 {
    (0.4 + 0.5 * energy).clamp(0.0, 1.0)
}

/// Hat grid density scales with energy: `k = 3 + round(energy·7)`, capped at
/// 11 onsets over 16 slots. Used by the [`HatIdiom::Euclid`] styles.
pub fn hat_density(energy: f32) -> u32 {
    (3.0 + (energy * 7.0).round()).clamp(3.0, 11.0) as u32
}

fn step_at(position: u32, velocity: f32, pitch: Option<f32>, gate: Option<f32>) -> Step {
    Step {
        position,
        velocity,
        probability: 1.0,
        microtiming_ticks: 0,
        ratchet: 1,
        pitch,
        gate,
        accent: false,
    }
}

fn onset_positions(k: u32, rot: i32) -> Vec<i64> {
    euclidean(k, 16, rot)
        .into_iter()
        .enumerate()
        .filter(|(_, on)| *on)
        .map(|(slot, _)| (slot as u64 * SLOT_TICKS) as i64)
        .collect()
}

fn grooved_steps(positions: Vec<i64>, energy: f32, swing: f32, rng: &mut Rng, timing: i64) -> Vec<Step> {
    let mut swung = positions;
    apply_swing(&mut swung, swing, i64::from(PPQ));
    swung
        .into_iter()
        .map(|p| {
            let (t, v) = humanize(p, velocity_for(energy), rng, timing, 0.05);
            step_at(t.min(BAR_END_TICKS) as u32, v, None, None)
        })
        .collect()
}

/// The velocity contour the session's groove implies (issue #17): pushed
/// grooves are off-accent, pulled grooves humanized, and a groove that
/// treats both grids alike is the machine look.
fn contour_for(groove: &crate::groove::ActiveGroove) -> VelContour {
    let gain = groove.offbeat_gain();
    if (gain - 1.0).abs() < 0.03 {
        VelContour::FlatMachine
    } else if gain > 1.0 {
        VelContour::OffAccent
    } else {
        VelContour::Humanized
    }
}

fn euclid(k: u32, n: u32, rot: i32, velocity: f32) -> Pattern {
    Pattern::Euclidean(EuclideanPattern {
        generator: EuclideanTag::Euclidean,
        k,
        n,
        rot,
        velocity,
        probability: 1.0,
        repeats: 1,
        gate: None,
        pitch: None,
    })
}

/// Structural accents (issue #17): steps exactly on a bar start carry the
/// compiler's ×1.2 velocity boost. Runs after all velocity shaping so the
/// boost lands on the final dynamics.
fn accent_downbeats(steps: &mut [Step]) {
    for st in steps.iter_mut() {
        st.accent |= st.position % TICKS_PER_BAR as u32 == 0;
    }
}

pub(crate) fn kick_pattern(plan: &SectionPlan, energy: f32, rng: &mut Rng) -> Pattern {
    if plan.kind == Kind::Breakdown && plan.sparse_kick {
        return euclid(1 + rng.below(2) as u32, 16, 0, velocity_for(energy));
    }
    if plan.fill {
        let vel = velocity_for(energy);
        // Kick drop (issue #17): the beat-3 hit vacates so the tail roll
        // lands into a vacuum.
        let quarters: Vec<u32> = [0u32, 960, 1920]
            .iter()
            .copied()
            .filter(|&t| !(t == 1920 && rng.chance(0.5)))
            .collect();
        let mut steps: Vec<Step> = quarters.iter().map(|&t| step_at(t, vel, None, None)).collect();
        steps.extend(ratchet_steps(2880, vel, 3));
        accent_downbeats(&mut steps);
        return Pattern::Steps(StepsPattern { steps, repeats: 1 });
    }
    euclid(4, 16, 0, (velocity_for(energy) + 0.05).min(0.95))
}

/// Backbeat: beats 2 and 4, the spine of every four-to-the-floor style.
/// Deliberately not humanized off the grid by much — a late clap reads as a
/// mistake where a late hat reads as feel.
pub(crate) fn backbeat_pattern(energy: f32, rng: &mut Rng) -> Pattern {
    let vel = (velocity_for(energy) + 0.08).min(0.95);
    let steps = [BEAT_TICKS, BEAT_TICKS * 3]
        .iter()
        .map(|&t| {
            let (pos, v) = humanize(i64::from(t), vel, rng, 4, 0.03);
            step_at(pos.min(BAR_END_TICKS) as u32, v, None, None)
        })
        .collect();
    Pattern::Steps(StepsPattern { steps, repeats: 1 })
}

/// Open hat on the offbeat eighths — the "and" of every beat. Any rack entry
/// whose hat voice is open (`open: true`) plays this line, whatever the
/// style's closed-hat idiom says.
///
/// The positions are the offbeat eighths, `EIGHTH_TICKS + BEAT_TICKS·k`. A
/// previous revision wrote `240 + 480·k`, which is a sixteenth early and runs
/// out after beat two, so the line that was supposed to be the most
/// recognisable thing in the mix covered half a bar off the grid.
///
/// Placement rules (issue #17): the offbeat default thins at low energy and
/// picks up variation slots when pushed — a sixteenth-late push after an
/// offbeat and a skipped offbeat keep the line from being a metronome.
pub(crate) fn open_hat_pattern(energy: f32, rng: &mut Rng) -> Pattern {
    let vel = (0.42 + 0.3 * energy).clamp(0.0, 0.85);
    let mut steps: Vec<Step> = Vec::new();
    for (i, &t) in OFFBEAT_EIGHTHS.iter().enumerate() {
        // The first offbeat anchors the bar; later ones may drop at low energy.
        if i > 0 && !rng.chance(0.7 + 0.3 * energy) {
            continue;
        }
        let (pos, v) = humanize(t, vel, rng, 6, 0.04);
        steps.push(step_at(pos.min(BAR_END_TICKS) as u32, v, None, Some(0.4)));
        // Variation slot: a sixteenth-late push, hot sections only.
        if energy > 0.6 && rng.chance(0.2) {
            let (p2, v2) = humanize(t + 120, vel * 0.7, rng, 4, 0.03);
            steps.push(step_at(p2.min(BAR_END_TICKS) as u32, v2, None, Some(0.2)));
        }
    }
    Pattern::Steps(StepsPattern { steps, repeats: 1 })
}

/// Closed hats. [`HatIdiom::Sixteenths`] is the engine room: eighths at low
/// energy opening out to sixteenths, accented on the offbeat so the grid has a
/// direction. [`HatIdiom::Euclid`] keeps the sparse seeded placement the
/// minimal styles are built on. `density` (the taste knob, 0..1) scales the
/// Euclid onset budget around its 0.6 neutral.
pub(crate) fn perc_pattern(
    plan: &SectionPlan,
    energy: f32,
    swing: f32,
    idiom: HatIdiom,
    density: f32,
    groove: &crate::groove::ActiveGroove,
    rng: &mut Rng,
) -> Pattern {
    let mut steps = match idiom {
        HatIdiom::Sixteenths => {
            let step_ticks = if energy > 0.45 { SLOT_TICKS } else { u64::from(EIGHTH_TICKS) };
            let count = TICKS_PER_BAR / step_ticks;
            let positions: Vec<i64> = (0..count).map(|i| (i * step_ticks) as i64).collect();
            let mut steps = grooved_steps(positions, energy, swing, rng, 8);
            for step in steps.iter_mut() {
                // On the beat: firm. Offbeat eighth: the accent. In between:
                // ghosted, so sixteenths read as motion rather than a buzz.
                // The #16 density curve rides the whole grid — a busy curve
                // pushes the tiers up, a sparse one pulls the line back.
                let on_beat = step.position % BEAT_TICKS == 0;
                let on_eighth = step.position % EIGHTH_TICKS == 0;
                let scale = if on_beat {
                    0.78
                } else if on_eighth {
                    1.0
                } else {
                    0.58
                };
                step.velocity = (step.velocity * scale * (0.8 + 0.4 * density)).clamp(0.0, 1.0);
            }
            steps
        }
        HatIdiom::Euclid => {
            if rng.chance(0.25) {
                let mask_density = ((0.25 + 0.45 * energy) * (0.75 + 0.5 * density)).clamp(0.0, 1.0);
                return Pattern::ProbabilityMask(ProbabilityMaskPattern {
                    generator: ProbabilityMaskTag::ProbabilityMask,
                    density: mask_density,
                    velocity: velocity_for(energy),
                    probability: 0.9,
                    repeats: 1,
                    gate: None,
                    pitch: None,
                });
            }
            let k = ((hat_density(energy) as f32 * (0.75 + 0.5 * density)).round() as u32).clamp(3, 11);
            let positions = onset_positions(k, first_onset_rot(&euclidean(k, 16, 0)));
            let mut swung = positions;
            apply_swing(&mut swung, swing, i64::from(PPQ));
            // Velocity contour + seeded gaussian jitter (issue #17): the
            // contour derives from the session's groove, the jitter σ from
            // the density curve (1–4 ticks @ 960).
            let contour = contour_for(groove);
            let sigma = jitter_sigma(density);
            swung
                .into_iter()
                .map(|p| {
                    let slot = (p as u64 / SLOT_TICKS).clamp(0, 15) as usize;
                    let base = velocity_for(energy) * contour.multiplier(slot);
                    let (t, v) = humanize_gauss(p, base, rng, sigma, 0.04);
                    step_at(t.min(BAR_END_TICKS) as u32, v, None, None)
                })
                .collect()
        }
    };
    ghost::ghost_pass(&mut steps, energy, density, rng);
    groove.apply(&mut steps, rng);
    if plan.fill {
        if let Some(last) = steps.pop() {
            let sub_hits = 2 + rng.below(4) as u8;
            steps.extend(crate::fill::roll_steps(last.position, last.velocity, sub_hits, rng));
            // Glitch repeat (issue #17): the tail's final hits stutter —
            // re-fired as machine-gun double-hits. Targeting a specific
            // audio slice per retrigger needs a per-step slice surface in
            // the IR; #19 owns the sample-voice internals, so the rhythm
            // level is what compiles today.
            if rng.chance(0.35) {
                let tail: Vec<Step> =
                    steps.iter().rev().take(2).map(|s| Step { ratchet: 2, ..*s }).collect();
                steps.extend(tail);
            }
        }
    }
    accent_downbeats(&mut steps);
    Pattern::Steps(StepsPattern { steps, repeats: 1 })
}

pub fn bass_pattern(
    archetype: Option<&str>,
    energy: f32,
    swing: f32,
    collision: DownbeatCollision,
    groove: Option<&crate::groove::ActiveGroove>,
    rng: &mut Rng,
) -> Pattern {
    let picked = crate::bass::pick(archetype, energy, rng);
    let mut steps = crate::bass::pattern(picked, energy, swing, rng);
    if let Some(g) = groove {
        // Per-track microtiming (issue #17): the low end rides half the
        // groove's push/pull — locked, but breathing with the top.
        g.apply_tilted(crate::groove::Tilt::Half, &mut steps, rng);
    }
    crate::bass::apply_downbeat_collision(&mut steps, collision);
    crate::bass::enforce_gate_pool(&mut steps);
    Pattern::Steps(StepsPattern { steps, repeats: 1 })
}

/// The 303 line (issue #87's acid idiom): the acid-slide archetype performed
/// by the acid voice — syncopated 16ths, octave-bounce accents for the glide
/// path. Rides the bass gate pool, so the #86 slot walk applies.
pub(crate) fn acid_pattern(
    energy: f32,
    swing: f32,
    groove: Option<&crate::groove::ActiveGroove>,
    rng: &mut Rng,
) -> Pattern {
    let mut steps = crate::bass::pattern(crate::bass::BassArchetype::AcidSlide, energy, swing, rng);
    if let Some(g) = groove {
        g.apply_tilted(crate::groove::Tilt::Half, &mut steps, rng);
    }
    crate::bass::enforce_gate_pool(&mut steps);
    Pattern::Steps(StepsPattern { steps, repeats: 1 })
}

/// EP chords: soft electric-piano hits on the offbeat eighths — the deep-house
/// colour. Three voices per hit (a triad; [`crate::harmony::retune_section`]
/// assigns the chord tones), gates under a beat so consecutive hits never
/// overlap the pad voice pool.
pub(crate) fn ep_pattern(energy: f32, restrained: bool, rng: &mut Rng) -> Pattern {
    let base = if restrained { 0.22 + 0.16 * energy } else { 0.3 + 0.28 * energy };
    let gate = if restrained { 0.5 } else { 0.9 };
    let mut steps: Vec<Step> = Vec::new();
    for (i, &t) in OFFBEAT_EIGHTHS.iter().enumerate() {
        let hit_chance = if i == 0 { 0.9 } else { 0.25 + 0.45 * energy };
        if !rng.chance(hit_chance) && i > 0 {
            continue;
        }
        let trim = 1.0 - 0.1 * i as f32;
        for v in 0..3 {
            let (pos, vel) = humanize(t, (base * trim).clamp(0.0, 1.0), rng, 10, 0.04);
            steps.push(step_at(pos.min(BAR_END_TICKS) as u32, vel, Some(60.0 + v as f32), Some(gate)));
        }
    }
    if let Some(first) = steps.first_mut() {
        first.accent = true;
    }
    Pattern::Steps(StepsPattern { steps, repeats: 1 })
}

/// Chord stabs: one or two clipped chord hits per bar. `ring` is the dub
/// techno reading — longer gates and a softer attack, because the rack's long
/// delay send (not the note) carries the sustain. Restrained styles pull the
/// level down so the stab stays a colour, not a lead.
///
/// Rhythm modes (issue #17): the default one-two is the `OffStab` reading;
/// `gated` styles (the driving ones — the pump styles keep their classic
/// stab so the sidechain swing stays deep) may draw the `Gated16th` mode
/// when the section runs hot: clipped 16ths on the beat grid, the sidechain
/// doing the sustain.
pub(crate) fn stab_pattern(
    energy: f32,
    ring: bool,
    restrained: bool,
    gated: bool,
    rng: &mut Rng,
) -> Pattern {
    if gated && energy > 0.55 && rng.chance(0.4) {
        return stab_gated_sixteenths(energy, restrained, rng);
    }
    let mut vel = (0.34 + 0.3 * energy).clamp(0.0, 0.85) * if ring { 0.8 } else { 1.0 };
    if restrained {
        vel *= 0.72;
    }
    let gate = if ring { 0.8 } else { 0.35 };
    let mut steps: Vec<Step> = Vec::new();
    if rng.chance(0.9) {
        for v in 0..3 {
            steps.push(step_at(0, (vel - 0.04 * v as f32).clamp(0.0, 1.0), Some(60.0 + v as f32), Some(gate)));
        }
    }
    if rng.chance(0.35 + 0.3 * energy) {
        let (pos, v) = humanize(1200, vel, rng, 8, 0.03);
        for voice in 0..3 {
            steps.push(step_at(pos.min(BAR_END_TICKS) as u32, v, Some(60.0 + voice as f32), Some(gate)));
        }
    }
    if steps.is_empty() {
        // Both draws failed; the downbeat hit is the floor — an empty
        // pattern compiles to no NoteOns and the section reads as silence.
        for voice in 0..3 {
            steps.push(step_at(0, vel, Some(60.0 + voice as f32), Some(gate)));
        }
    }
    if let Some(first) = steps.first_mut() {
        first.accent = true;
    }
    Pattern::Steps(StepsPattern { steps, repeats: 1 })
}

/// The gated-16th chord rhythm (issue #17): clipped triad hits marching the
/// 16th grid, dropping density as the bar breathes. Short gates keep the
/// voices out of each other; the kick sidechain supplies the pumping shape.
fn stab_gated_sixteenths(energy: f32, restrained: bool, rng: &mut Rng) -> Pattern {
    let vel = (0.3 + 0.25 * energy) * if restrained { 0.72 } else { 1.0 };
    let mut steps: Vec<Step> = Vec::new();
    for slot in 0..16u32 {
        // Downbeats always fire; off-grid 16ths thin with energy.
        let keep = slot % 4 == 0 || rng.chance(0.25 + 0.45 * energy);
        if !keep {
            continue;
        }
        let pos = (slot as i64 * SLOT_TICKS as i64).min(BAR_END_TICKS) as u32;
        for voice in 0..3u32 {
            steps.push(step_at(
                pos,
                (vel - 0.04 * voice as f32).clamp(0.0, 1.0),
                Some(60.0 + voice as f32),
                Some(0.12),
            ));
        }
    }
    if let Some(first) = steps.first_mut() {
        first.accent = true;
    }
    Pattern::Steps(StepsPattern { steps, repeats: 1 })
}

/// Micro-pluck (microhouse) and sparse pluck (ambient): a handful of
/// single-note hits on a seeded euclid grid, gates short enough to stay out
/// of the pad pool. `density` scales the onset budget around its 0.6 neutral;
/// restrained styles (the sparse ones, issue #52 WS2) cap the velocity so the
/// micro-detail stays felt-not-heard.
pub(crate) fn pluck_pattern(
    energy: f32,
    swing: f32,
    density: f32,
    restrained: bool,
    rng: &mut Rng,
) -> Pattern {
    let budget = (2.0 + 3.0 * energy) * (0.75 + 0.5 * density);
    let k = (budget.round() as u32).clamp(2, 6);
    let vel = if restrained { 0.28 + 0.18 * energy } else { velocity_for(energy) };
    let mut steps = grooved_steps(onset_positions(k, first_onset_rot(&euclidean(k, 16, 0))), energy, swing, rng, 10);
    for (i, st) in steps.iter_mut().enumerate() {
        st.pitch = Some(48.0 + 0.5 * i as f32);
        st.gate = Some(0.2);
        st.velocity = st.velocity.min(vel);
    }
    accent_downbeats(&mut steps);
    Pattern::Steps(StepsPattern { steps, repeats: 1 })
}

/// Beatless texture: one long wash per bar — the sample slot rings across the
/// whole bar while everything else stays quiet.
pub(crate) fn texture_pattern(energy: f32, brightness: f32, rng: &mut Rng) -> Pattern {
    let vel = ((0.24 + 0.2 * energy) * (0.75 + 0.5 * brightness)).clamp(0.0, 0.6);
    let (t, v) = humanize(0, vel, rng, 0, 0.02);
    let steps = vec![step_at(t.min(BAR_END_TICKS) as u32, v, None, Some(8.0))];
    Pattern::Steps(StepsPattern { steps, repeats: 1 })
}

/// Pads carry a chord, not a note.
///
/// One step per voice, all at position 0; [`crate::harmony::retune_section`]
/// then assigns each step its tone from the section's chord voicing. The step
/// count picks the colour: three voices is a triad, four a seventh. A previous
/// revision emitted a single step, so the whole harmonic layer of every
/// generated track was one sustained pitch.
pub fn pad_pattern(energy: f32, restrained: bool, rng: &mut Rng) -> Pattern {
    let (velocity, gate) = if restrained {
        (0.26 + 0.16 * energy, 2.0)
    } else {
        (0.35 + 0.25 * energy, 4.0)
    };
    let voices = if restrained { 3 } else { 4 };
    // Voices fan out slightly in level so the chord has an inside.
    let steps = (0..voices)
        .map(|i| {
            let trim = 1.0 - 0.08 * i as f32;
            // Placeholder pitches; `harmony::retune_section` replaces each with
            // its tone from the section's chord voicing.
            step_at(0, (velocity * trim).clamp(0.0, 1.0), Some(60.0 + i as f32), Some(gate))
        })
        .collect();
    let _ = rng;
    Pattern::Steps(StepsPattern { steps, repeats: 1 })
}

/// The style's rack (#88), materialized into session tracks. The rack owns
/// its mixer/FX identity — deep house's soft kick, dub techno's long delay
/// send, ambient's kickless texture bed.
pub fn tracks_for_genre(genre: Option<&str>) -> Vec<Track> {
    crate::genre::spec_for(genre).rack.iter().map(voice_track).collect()
}

/// The default (unnamed-genre) rig: the legacy six tracks.
pub fn tracks() -> Vec<Track> {
    tracks_for_genre(None)
}

#[cfg(test)]
mod tests {
    use kontinuum_clock::Rng;

    use super::*;

    fn plan(fill: bool) -> SectionPlan {
        SectionPlan {
            id: "dev_0".into(),
            kind: crate::arrangement::Kind::Dev,
            bars: 8,
            energy: (0.5, 0.6),
            density: (0.5, 0.6),
            brightness: (0.5, 0.6),
            fill,
            sparse_kick: false,
            stripped: false,
        }
    }

    #[test]
    fn kick_fill_downbeats_carry_the_accent() {
        let pattern = kick_pattern(&plan(true), 0.6, &mut Rng::from_seed(3));
        let Pattern::Steps(steps) = pattern else { panic!("fill kick is a step pattern") };
        assert!(steps.steps.iter().filter(|s| s.position == 0).all(|s| s.accent));
        assert!(steps.steps.iter().filter(|s| s.position != 0).all(|s| !s.accent));
    }

    #[test]
    fn perc_ghosts_are_never_accented() {
        for seed in 0..40u64 {
            let groove =
                crate::groove::ActiveGroove::from_static(crate::groove::pick(None, &mut Rng::from_seed(seed)));
            let pattern = perc_pattern(
                &plan(seed % 2 == 0),
                0.6,
                0.1,
                crate::genre::HatIdiom::Euclid,
                0.6,
                &groove,
                &mut Rng::from_seed(seed + 100),
            );
            let Pattern::Steps(steps) = pattern else { continue };
            for st in &steps.steps {
                if st.probability < 1.0 {
                    assert!(!st.accent, "seed {seed}: ghost accented");
                    assert_eq!(st.ratchet, 1);
                }
                if st.position == 0 {
                    assert!(st.accent, "seed {seed}: perc downbeat unaccented");
                }
            }
        }
    }

    /// Every rack materializes into a valid, fully-specified track list.
    #[test]
    fn every_rack_materializes() {
        for genre in [
            None,
            Some("minimal techno"),
            Some("techno"),
            Some("deep house"),
            Some("house"),
            Some("microhouse"),
            Some("acid"),
            Some("dub techno"),
            Some("ambient"),
        ] {
            let tracks = tracks_for_genre(genre);
            assert!(!tracks.is_empty());
            for t in &tracks {
                assert_eq!(t.inserts.len(), if t.role == kontinuum_ir::TrackRole::Kick { 1 } else { 0 });
                assert!((0.0..=2.0).contains(&t.gain));
            }
        }
    }

    /// The dub techno rack ships its delay send; ambient ships none.
    #[test]
    fn rack_mixer_identity_is_materialized() {
        let dub = tracks_for_genre(Some("dub techno"));
        let stab = dub.iter().find(|t| t.id == "stab").expect("dub stab");
        assert!(stab.sends.delay >= 0.5, "the long delay send is the style's identity");
        let ambient = tracks_for_genre(Some("ambient"));
        assert!(ambient.iter().all(|t| t.role != kontinuum_ir::TrackRole::Kick));
        let texture = ambient.iter().find(|t| t.id == "texture").expect("texture");
        let InstrumentDef::Sample(slot) = &texture.instrument else {
            panic!("texture materializes as a sample slot");
        };
        assert_eq!(slot.query.as_deref(), Some("atmospheric texture"));
    }
}
