//! Bass archetype vocabulary (issue #17): the minimal-techno / microhouse
//! low-end patterns as named, parameterized archetypes — the bass sibling
//! of [`crate::groove`]'s hand-made vocabulary.
//!
//! Each archetype is a rhythm mask + pitch-contour + gate profile over one
//! bar. Selection is seeded and energy-weighted so #16's energy/density
//! curves (and later taste DNA, #21) steer the draw; a name pin selects a
//! specific archetype deterministically. All randomness enters through the
//! caller's [`kontinuum_clock::Rng`] and every step stays inside the
//! `kontinuum-ir` bounds (same contract as [`crate::pattern`]).

use kontinuum_clock::{PPQ, Rng, TICKS_PER_BAR};
use kontinuum_ir::compile::{expand::POOL_BASS, BLOCK_BARS};
use kontinuum_ir::schema::{bounds, DownbeatCollision, Step};

use crate::pattern::{apply_swing, euclidean, first_onset_rot, humanize};

const BAR_END_TICKS: i64 = TICKS_PER_BAR as i64 - 1;
/// Tick positions of the "and" of each beat (offbeat 8ths at PPQ 960).
const OFFBEAT_EIGHTHS: [i64; 4] = [240, 720, 1200, 1680];

/// Gate the compiler assumes for a bass step that does not name one
/// (`kontinuum_ir::compile::expand::default_gate_beats`).
const DEFAULT_GATE_BEATS: f32 = 1.0;

/// The named bass archetypes (#17 "Bass" checklist).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BassArchetype {
    /// Classic house/microhouse offbeat 8th stabs (the "and" of every beat).
    OffbeatEighths,
    /// Rolling 16ths with a velocity wave over the bar.
    RollingSixteenths,
    /// Sparse dub-sub: 1–2 long sub notes per bar.
    DubSub,
    /// Syncopated funk: a 16th syncopation grid — the pushed "a" of 1 and 2
    /// plus the "e" pickups, landed hard on 2 and 4 (issue #17).
    SyncopatedFunk,
    /// Acid line: syncopated 16ths, octave-bounce accents for the glide path.
    AcidSlide,
    /// Call-and-answer (#17): the bar's first half states the root with
    /// 2–4 hits, the second half answers on the fifth, sparser. Patterns
    /// are one repeated bar, so the conversation lives inside the bar.
    CallResponse,
}

pub const ALL: [BassArchetype; 6] =
    [BassArchetype::OffbeatEighths, BassArchetype::RollingSixteenths, BassArchetype::DubSub, BassArchetype::SyncopatedFunk, BassArchetype::AcidSlide, BassArchetype::CallResponse];

impl BassArchetype {
    pub fn name(self) -> &'static str {
        match self {
            BassArchetype::OffbeatEighths => "offbeat-eighths",
            BassArchetype::RollingSixteenths => "rolling-16ths",
            BassArchetype::DubSub => "dub-sub",
            BassArchetype::SyncopatedFunk => "syncopated-funk",
            BassArchetype::AcidSlide => "acid-slide",
            BassArchetype::CallResponse => "call-response",
        }
    }

    fn from_name(name: &str) -> Option<BassArchetype> {
        ALL.iter().copied().find(|a| a.name() == name)
    }
}

/// Seeded archetype pick. `Some(name)` pins the archetype (unknown names
/// fall through to the draw, mirroring [`crate::groove::pick`]); `None`
/// draws with energy-driven weights — quiet sections lean dub-sub, mid
/// sections offbeat-eighths, hot sections rolling 16ths and acid.
pub fn pick(name: Option<&str>, energy: f32, rng: &mut Rng) -> BassArchetype {
    if let Some(n) = name.and_then(BassArchetype::from_name) {
        return n;
    }
    // Sub-octave draw: the first half of the unit square favors the sparse
    // archetype, the second half the busy ones, so low-energy sections land
    // on dub-sub most of the time without hard-clipping the distribution.
    let roll = rng.next_f32();
    let candidates: [BassArchetype; 3] = if energy < 0.4 {
        [BassArchetype::DubSub, BassArchetype::OffbeatEighths, BassArchetype::OffbeatEighths]
    } else if energy < 0.7 {
        [BassArchetype::OffbeatEighths, BassArchetype::RollingSixteenths, BassArchetype::SyncopatedFunk]
    } else {
        [BassArchetype::RollingSixteenths, BassArchetype::AcidSlide, BassArchetype::SyncopatedFunk]
    };
    if roll < 0.7 {
        candidates[0]
    } else if roll < 0.9 {
        candidates[1]
    } else {
        candidates[2]
    }
}

/// Generates one bar of the archetype as IR steps. `energy` (0..=1) drives
/// density and velocity, `swing` delays odd 16ths, both per the #16 curves.
pub fn pattern(archetype: BassArchetype, energy: f32, swing: f32, rng: &mut Rng) -> Vec<Step> {
    let root = if rng.chance(0.5) { 36.0 } else { 41.0 };
    let positions: Vec<i64> = match archetype {
        BassArchetype::OffbeatEighths => {
            let mut p: Vec<i64> = OFFBEAT_EIGHTHS.to_vec();
            if rng.chance(energy * 0.6) {
                p.insert(0, 0); // pickup stab on the downbeat when pushed
            }
            if rng.chance(energy * 0.6) {
                p.push(2160); // ghost at the last "and-of-4.5"
            }
            p
        }
        BassArchetype::RollingSixteenths => {
            let k = 8 + (energy * 4.0).round() as u32;
            let rot = first_onset_rot(&euclidean(k, 16, 0));
            onset_slots(euclidean(k, 16, rot))
        }
        BassArchetype::DubSub => {
            let mut p = vec![0];
            if rng.chance(0.6) {
                p.push(2880); // dotted-half echo on the "and" of 3
            }
            p
        }
        BassArchetype::SyncopatedFunk => {
            // The 16th syncopation grid: pickups on the "e"/"a" cells with
            // the money notes landing on 2 and 4. The exact pickup set is
            // seeded so the groove never loops identically two sessions
            // running.
            let mut p: Vec<i64> = vec![480, 1440];
            for &pickup in &[180i64, 420, 660, 1140, 1380, 1620, 2100] {
                if rng.chance(0.3 + 0.3 * energy) {
                    p.push(pickup);
                }
            }
            if rng.chance(0.5) {
                p.push(2160); // the "and" of 4 — the funk turnaround
            }
            p.sort_unstable();
            p
        }
        BassArchetype::AcidSlide => {
            // Syncopated 16th grid: offbeat 8ths plus the 16ths just before
            // each beat — the galloping acid pickup.
            let mut p: Vec<i64> = OFFBEAT_EIGHTHS.to_vec();
            p.extend([180, 660, 1140, 1620]);
            if rng.chance(0.5) {
                p.push(0);
            }
            p.sort_unstable();
            p
        }
        BassArchetype::CallResponse => {
            // Call: the downbeat plus 1–3 root hits in the first half.
            // Response: 1–2 fifth hits in the back half, sparser.
            let mut p: Vec<i64> = vec![0];
            for _ in 0..(1 + rng.below(3)) {
                p.push(240 * (1 + rng.below(7)) as i64);
            }
            p.push(1920 + 480 * rng.below(4) as i64);
            if rng.chance(0.4) {
                p.push(2160 + 240 * rng.below(6) as i64);
            }
            p.sort_unstable();
            p.dedup();
            p
        }
    };
    let mut swung = positions;
    apply_swing(&mut swung, swing, i64::from(PPQ));
    let mut steps: Vec<Step> = swung
        .into_iter()
        .map(|p| {
            let base = archetype.velocity(energy, p);
            let (t, v) = humanize(p, base, rng, 8, 0.04);
            let (pitch, gate) = archetype.note(root, energy, p, rng);
            Step {
                position: t.min(BAR_END_TICKS) as u32,
                velocity: v,
                probability: 1.0,
                microtiming_ticks: 0,
                ratchet: 1,
                pitch: Some(pitch),
                gate: Some(gate),
                accent: false,
            }
        })
        .collect();
    // Structural accent (issue #17): the bar's first hit carries the
    // compiler's ×1.2 boost on every repetition.
    if let Some(first) = steps.first_mut() {
        first.accent = true;
    }
    // Acid accents (issue #17): the octave-bounce hits are the line's
    // accents, so they carry the compiler boost too — the squelch answer
    // to the kick's thud.
    if archetype == BassArchetype::AcidSlide {
        for st in steps.iter_mut().skip(1) {
            if let Some(pitch) = st.pitch {
                st.accent |= pitch >= root + 12.0;
            }
        }
    }
    steps
}

/// Kick-interaction rule (issue #17): how the bass may sit against the
/// four-on-the-floor kick's positions (0, 960, 1920, 2880 and the swing/
/// humanize halo around them).
///
/// - [`DownbeatCollision::Avoid`] drops colliding onsets (shifting would
///   smear the idiom's grid); a bar that would empty out keeps its downbeat
///   instead — never a dead bar.
/// - [`DownbeatCollision::Allow`] stacks them on purpose (driving techno).
/// - [`DownbeatCollision::DuckOnly`] leaves placement alone; the kick
///   sidechain (the rack's duck depth) does the work.
pub fn apply_downbeat_collision(steps: &mut Vec<Step>, mode: DownbeatCollision) {
    if mode != DownbeatCollision::Avoid || steps.is_empty() {
        return;
    }
    const HALO_TICKS: i64 = 40;
    let collides = |pos: u32| {
        (0..4).any(|beat| {
            let kick = beat * 960;
            (i64::from(pos) - kick).abs() <= HALO_TICKS
        })
    };
    let keep_downbeat = steps[0].position == 0;
    steps.retain(|st| keep_downbeat && st.position == 0 || !collides(st.position));
    if steps.is_empty() {
        // The archetype was nothing but downbeats; keep one so the bar
        // still states the root.
        steps.push(Step {
            position: 0,
            velocity: 0.5,
            probability: 1.0,
            microtiming_ticks: 0,
            ratchet: 1,
            pitch: None,
            gate: Some(0.5),
            accent: false,
        });
    }
}

impl BassArchetype {
    /// Archetype velocity contour at position `p`: rolling 16ths wave,
    /// acid accents, flat stabs otherwise.
    fn velocity(self, energy: f32, p: i64) -> f32 {
        let base = 0.4 + 0.5 * energy;
        match self {
            BassArchetype::RollingSixteenths => {
                let slot = (p / 240) as f32;
                let wave = 0.8 + 0.2 * (slot * std::f32::consts::PI / 4.0).sin();
                (base * wave).clamp(0.0, 1.0)
            }
            BassArchetype::AcidSlide => {
                if p % 960 == 0 { (base + 0.1).min(1.0) } else { base * 0.85 }
            }
            BassArchetype::CallResponse => {
                if p < TICKS_PER_BAR as i64 / 2 { (base + 0.05).min(1.0) } else { base * 0.8 }
            }
            BassArchetype::SyncopatedFunk => {
                // The money notes (2 and 4) lead; pickups ghost behind.
                if p % 960 == 480 { base } else { base * 0.72 }
            }
            BassArchetype::OffbeatEighths | BassArchetype::DubSub => base,
        }
    }

    /// Archetype pitch contour and gate (in beats): octave bounce for acid,
    /// root-only subs for dub, short stabs elsewhere.
    fn note(self, root: f32, energy: f32, p: i64, rng: &mut Rng) -> (f32, f32) {
        match self {
            BassArchetype::OffbeatEighths => {
                let pitch = if rng.chance(0.2) { root + 12.0 } else { root };
                (pitch, 0.25 + 0.1 * rng.next_f32())
            }
            BassArchetype::RollingSixteenths => {
                let pitch = if rng.chance(0.15) { root + 7.0 } else { root };
                (pitch, 0.15 + 0.05 * energy)
            }
            BassArchetype::DubSub => (root, 2.5 + 1.0 * rng.next_f32()),
            BassArchetype::AcidSlide => {
                // Octave bounce every other offbeat — the slide's target.
                let bar8 = (p / 480) % 2 == 1;
                let pitch = if bar8 { root + 12.0 } else if rng.chance(0.15) { root + 10.0 } else { root };
                (pitch, 0.12 + 0.06 * energy)
            }
            BassArchetype::SyncopatedFunk => {
                // Roots on the money notes, flat-seventh passing tones on the
                // pickups.
                let pitch = if rng.chance(0.2) { root + 10.0 } else { root };
                (pitch, 0.18 + 0.08 * energy)
            }
            BassArchetype::CallResponse => {
                // Call half states the root; the response answers on the fifth.
                let pitch = if p < TICKS_PER_BAR as i64 / 2 { root } else { root + 7.0 };
                (pitch, 0.2 + 0.08 * energy)
            }
        }
    }
}

fn onset_slots(grid: Vec<bool>) -> Vec<i64> {
    grid.into_iter()
        .enumerate()
        .filter(|(_, on)| *on)
        .map(|(slot, _)| slot as i64 * 240)
        .collect()
}

/// Holds the bar's gate ring inside the bass voice pool (issue #86): the
/// compiler hands the role [`POOL_BASS`] slots per [`BLOCK_BARS`]-bar block
/// and reuses a slot only once the previous note's gate has fully elapsed,
/// so a bar ringing across more than `POOL_BASS` overlapping onsets cannot
/// compile — the validator surfaces that as `E_POLYPHONY_EXCEEDED`. This
/// simulates the compiler's slot walk over a block of repeated bars and
/// shortens the earliest-finishing gate wherever an onset would land
/// without a free slot — the validator's own suggested_fix, applied before
/// the session exists so generation and validation agree by construction.
pub(crate) fn enforce_gate_pool(steps: &mut Vec<Step>) {
    for _ in 0..16 {
        if !block_needs_gate_repairs(steps) {
            return;
        }
    }
}

/// One slot-walk over `BLOCK_BARS` repetitions of the bar; a repair
/// shortens one canonical gate or drops one onset, which changes later
/// repetitions too, so the caller re-runs until a pass comes back clean
/// (every repair only ever shrinks the pattern).
fn block_needs_gate_repairs(steps: &mut Vec<Step>) -> bool {
    let pool = usize::from(POOL_BASS);
    let bar_ticks = TICKS_PER_BAR as i64;
    let floor_ticks = (bounds::GATE_BEATS.0 * PPQ as f32).ceil() as i64;
    // (onset, gate end, canonical step index)
    let mut spans: Vec<(i64, i64, usize)> = Vec::with_capacity(steps.len() * BLOCK_BARS as usize);
    for rep in 0..BLOCK_BARS as i64 {
        for (i, st) in steps.iter().enumerate() {
            let onset = rep * bar_ticks + i64::from(st.position);
            let gate = (st.gate.unwrap_or(DEFAULT_GATE_BEATS) * PPQ as f32) as i64;
            spans.push((onset, onset + gate.max(1), i));
        }
    }
    spans.sort_by_key(|&(onset, _, i)| (onset, i));

    // Per-slot gate end of the occupying span, and which span that is.
    let mut slot_end = [-1i64; POOL_BASS as usize];
    let mut slot_span = [0usize; POOL_BASS as usize];
    for (si, &(onset, end, _)) in spans.iter().enumerate() {
        let Some(slot) = (0..pool).find(|&s| slot_end[s] < onset) else {
            let repaired = end_the_earliest_ringer_before(
                steps,
                &spans,
                &slot_end,
                &slot_span,
                onset,
                floor_ticks,
            );
            if !repaired {
                // No gate can shrink out of the way: the bar is denser than
                // the pool can ring, so thin the pattern instead — drop the
                // onset that would have been stranded.
                let (_, _, stranded) = spans[si];
                steps.remove(stranded);
            }
            return true;
        };
        slot_end[slot] = end;
        slot_span[slot] = si;
    }
    false
}

/// Ends the earliest-finishing ringing note one tick before `onset`, within
/// the schema's gate floor. Returns false when every ringer is already at
/// the floor or sits too close to the onset to free a slot in time.
fn end_the_earliest_ringer_before(
    steps: &mut [Step],
    spans: &[(i64, i64, usize)],
    slot_end: &[i64; POOL_BASS as usize],
    slot_span: &[usize; POOL_BASS as usize],
    onset: i64,
    floor_ticks: i64,
) -> bool {
    let mut order: Vec<usize> = (0..slot_end.len()).collect();
    order.sort_by_key(|&s| slot_end[s]);
    for s in order {
        let (_, span_onset, span_step) = spans[slot_span[s]];
        let room = onset - 1 - span_onset;
        if room < floor_ticks {
            continue;
        }
        steps[span_step].gate = Some(room as f32 / PPQ as f32);
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_in_vocabulary(steps: &[Step]) {
        assert!(!steps.is_empty());
        for st in steps {
            assert!(st.position < TICKS_PER_BAR as u32, "position inside the bar");
            assert!((0.0..=1.0).contains(&st.velocity));
            assert!((0.0..=1.0).contains(&st.probability));
            assert!(st.pitch.is_some(), "bass steps carry pitch");
            assert!(st.gate.unwrap_or(0.0) > 0.0);
        }
    }

    /// Peak concurrent gate spans over repeated bars, counted the way the
    /// compiler assigns slots: a slot frees only once its gate end is
    /// strictly before the new onset.
    fn peak_overlap(steps: &[Step]) -> usize {
        let bar = TICKS_PER_BAR as i64;
        let mut spans: Vec<(i64, i64)> = Vec::new();
        for rep in 0..2 {
            for st in steps {
                let onset = rep * bar + i64::from(st.position);
                let gate = (st.gate.unwrap_or(DEFAULT_GATE_BEATS) * PPQ as f32) as i64;
                spans.push((onset, onset + gate.max(1)));
            }
        }
        spans
            .iter()
            .map(|&(onset, _)| {
                spans.iter().filter(|&&(s, e)| s <= onset && onset < e).count()
            })
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn deep_style_gate_ring_never_exceeds_the_voice_pool() {
        // issue #86: the deep-house 0.8-beat ring put five overlapping notes
        // on the 4-slot bass pool (acid-slide flams), which the validator
        // rejects as E_POLYPHONY_EXCEEDED. The pool pass must hold every
        // archetype inside the budget under that override.
        for a in ALL {
            for energy in [0.1f32, 0.5, 0.95] {
                for seed in 0..32u64 {
                    let mut rng = Rng::from_seed(seed);
                    let mut steps = pattern(a, energy, 0.12, &mut rng);
                    for st in &mut steps {
                        st.gate = Some(0.8);
                    }
                    enforce_gate_pool(&mut steps);
                    assert!(
                        peak_overlap(&steps) <= usize::from(POOL_BASS),
                        "{} energy {energy} seed {seed}: {:?}",
                        a.name(),
                        steps.iter().map(|s| (s.position, s.gate)).collect::<Vec<_>>(),
                    );
                }
            }
        }
    }

    #[test]
    fn pool_pass_leaves_safe_patterns_untouched() {
        // Default archetype gates never crowd the pool, so the pass must be
        // a no-op there — sessions that already validated stay byte-identical.
        for a in ALL {
            for energy in [0.1f32, 0.5, 0.95] {
                let mut x = Rng::from_seed(4242);
                let mut y = Rng::from_seed(4242);
                let before = pattern(a, energy, 0.1, &mut x);
                let mut after = pattern(a, energy, 0.1, &mut y);
                enforce_gate_pool(&mut after);
                assert_eq!(before, after, "{} was altered", a.name());
            }
        }
    }

    #[test]
    fn every_archetype_is_deterministic_and_in_bounds() {
        for a in ALL {
            for energy in [0.1f32, 0.5, 0.95] {
                let mut x = Rng::from_seed(4242);
                let mut y = Rng::from_seed(4242);
                let a_steps = pattern(a, energy, 0.1, &mut x);
                let b_steps = pattern(a, energy, 0.1, &mut y);
                assert_eq!(a_steps, b_steps, "{} non-deterministic", a.name());
                assert_in_vocabulary(&a_steps);
            }
        }
    }

    #[test]
    fn archetypes_are_distinct_vocabulary() {
        let mut x = Rng::from_seed(7);
        let shapes: Vec<Vec<i64>> =
            ALL.iter().map(|&a| pattern(a, 0.6, 0.0, &mut x).iter().map(|s| s.position as i64).collect()).collect();
        for (i, s) in shapes.iter().enumerate() {
            for t in shapes.iter().skip(i + 1) {
                assert_ne!(s, t, "{} shares a rhythm shape", ALL[i].name());
            }
        }
        // Dub-sub is the long-note archetype; everything else is short.
        let dub = pattern(BassArchetype::DubSub, 0.5, 0.0, &mut Rng::from_seed(1));
        assert!(dub.iter().all(|s| s.gate.unwrap_or(0.0) > 2.0), "dub-sub rings");
        let stab = pattern(BassArchetype::OffbeatEighths, 0.5, 0.0, &mut Rng::from_seed(1));
        assert!(stab.iter().all(|s| s.gate.unwrap_or(9.0) < 1.0), "stabs are short");
    }

    #[test]
    fn offbeat_eighths_hit_the_and_of_beats() {
        let steps = pattern(BassArchetype::OffbeatEighths, 0.3, 0.0, &mut Rng::from_seed(2));
        let on_offbeats: Vec<i64> =
            steps.iter().map(|s| s.position as i64).filter(|p| (p / 480) % 2 == 1).collect();
        assert!(!on_offbeats.is_empty(), "offbeat stabs present");
        for p in OFFBEAT_EIGHTHS {
            assert!(
                steps.iter().any(|s| (s.position as i64 - p).abs() <= 16),
                "missing offbeat {p}"
            );
        }
    }

    #[test]
    fn rolling_sixteenths_density_rises_with_energy() {
        let low = pattern(BassArchetype::RollingSixteenths, 0.1, 0.0, &mut Rng::from_seed(3)).len();
        let high = pattern(BassArchetype::RollingSixteenths, 0.95, 0.0, &mut Rng::from_seed(3)).len();
        assert!(high > low, "energy must open the roll ({low} → {high})");
    }

    #[test]
    fn name_pin_selects_and_unknown_falls_through() {
        let mut rng = Rng::from_seed(5);
        assert_eq!(pick(Some("dub-sub"), 0.9, &mut rng), BassArchetype::DubSub);
        assert_eq!(pick(Some("acid-slide"), 0.1, &mut rng), BassArchetype::AcidSlide);
        let drawn = pick(None, 0.9, &mut rng);
        assert!(ALL.contains(&drawn));
    }

    #[test]
    fn low_energy_leans_dub_high_energy_leans_busy() {
        let mut quiet = Rng::from_seed(11);
        let mut hot = Rng::from_seed(11);
        let quiet_draws: Vec<BassArchetype> = (0..20).map(|_| pick(None, 0.1, &mut quiet)).collect();
        let hot_draws: Vec<BassArchetype> = (0..20).map(|_| pick(None, 0.9, &mut hot)).collect();
        assert!(quiet_draws.iter().all(|&a| a != BassArchetype::AcidSlide), "quiet never acid");
        assert!(hot_draws.iter().all(|&a| a != BassArchetype::DubSub), "hot never dub");
        assert!(
            hot_draws.contains(&BassArchetype::SyncopatedFunk),
            "the funk archetype is reachable in the hot tier"
        );
    }

    #[test]
    fn syncopated_funk_lands_the_money_notes() {
        for seed in 0..24u64 {
            let steps = pattern(BassArchetype::SyncopatedFunk, 0.6, 0.0, &mut Rng::from_seed(seed));
            assert_in_vocabulary(&steps);
            for beat in [480i64, 1440] {
                assert!(
                    steps.iter().any(|s| (s.position as i64 - beat).abs() <= 16),
                    "seed {seed}: missing money note {beat}"
                );
            }
            // Pickups ghost behind the money notes: compare per-hit means,
            // bucketing within the humanize halo of each money note (the
            // final positions are jittered off the exact grid).
            let is_money = |pos: u32| {
                [480i64, 1440].iter().any(|&m| (i64::from(pos) - m).abs() <= 16)
            };
            let mut money = Vec::new();
            let mut pickups = Vec::new();
            for s in &steps {
                if is_money(s.position) { money.push(s.velocity) } else { pickups.push(s.velocity) }
            }
            let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len().max(1) as f32;
            assert!(mean(&money) > mean(&pickups), "seed {seed}: pickups must not dominate");
        }
    }

    #[test]
    fn avoid_keeps_the_bass_off_the_kick_but_never_silences_the_bar() {
        let mut steps = vec![
            step_at(0),
            step_at(480),
            step_at(960),
            step_at(1440),
            step_at(1920),
            step_at(2400),
            step_at(2880),
            step_at(3360),
        ];
        apply_downbeat_collision(&mut steps, DownbeatCollision::Avoid);
        assert_eq!(steps[0].position, 0, "the downbeat survives");
        assert!(
            steps.iter().all(|s| s.position % 960 != 0 || s.position == 0),
            "collisions cleared: {:?}",
            steps.iter().map(|s| s.position).collect::<Vec<_>>()
        );
        assert!(steps.len() > 1, "not a dead bar");
        // Allow and duck-only are no-ops.
        let mut untouched = vec![step_at(0), step_at(960)];
        let before = untouched.clone();
        apply_downbeat_collision(&mut untouched, DownbeatCollision::Allow);
        assert_eq!(untouched, before);
        apply_downbeat_collision(&mut untouched, DownbeatCollision::DuckOnly);
        assert_eq!(untouched, before);
    }

    #[test]
    fn avoid_on_an_all_downbeats_bar_keeps_one_hit() {
        let mut steps = vec![step_at(0), step_at(960)];
        apply_downbeat_collision(&mut steps, DownbeatCollision::Avoid);
        assert_eq!(steps.len(), 1, "the downbeat statement survives");
        assert_eq!(steps[0].position, 0);
    }

    fn step_at(position: u32) -> Step {
        Step {
            position,
            velocity: 0.6,
            probability: 1.0,
            microtiming_ticks: 0,
            ratchet: 1,
            pitch: Some(36.0),
            gate: Some(0.4),
            accent: false,
        }
    }

    #[test]
    fn acid_octave_hits_carry_the_accent() {
        for seed in 0..16u64 {
            let steps = pattern(BassArchetype::AcidSlide, 0.7, 0.0, &mut Rng::from_seed(seed));
            let Some(floor) = steps.iter().filter_map(|s| s.pitch).fold(None::<f32>, |acc, p| {
                Some(acc.map_or(p, |a| a.min(p)))
            }) else {
                continue;
            };
            let octaves: Vec<&Step> =
                steps.iter().skip(1).filter(|s| s.pitch.map_or(false, |p| p >= floor + 12.0)).collect();
            if octaves.is_empty() {
                continue;
            }
            assert!(
                octaves.iter().all(|s| s.accent),
                "seed {seed}: octave bounce is accented"
            );
        }
    }
}
