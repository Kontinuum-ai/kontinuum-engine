//! Fallback arrangement generator (#15): the built-in "safe arrangement"
//! that plays when the AI planner cannot. The music never stops.
// allow: SIZE_OK — ~150 of these lines are the built-in session palette, a
// pure data table mirroring fixtures/loop-4track.ir.json; the logic half is
// well under the ceiling.

use std::collections::BTreeMap;
use std::sync::Arc;

use kontinuum_clock::TempoLane;
use kontinuum_ir::compile::{compile_session, BLOCK_BARS};
use kontinuum_ir::schema::{
    AutomationLane, BassInstrument, BassTag, CurveKind, EuclideanPattern, EuclideanTag,
    HatInstrument, HatTag, InsertDef, InsertKind, InstrumentDef, KickInstrument, KickTag,
    PadInstrument, PadTag, Pattern, ProbabilityMaskPattern, ProbabilityMaskTag, Section, Sends,
    Session, Step, StepsPattern, Track, Transition, TransitionKind, Wave,
};
use kontinuum_ir::{IR_VERSION, TrackRole};
use kontinuum_schedule::{BlockSource, CompiledBlock, Event, TrackEvents};

/// Authored tempo of the built-in safe arrangement.
const FALLBACK_BPM: f64 = 124.0;
/// Session length in bars: two alternating 8-bar sections.
pub const FALLBACK_BARS: u32 = 16;

/// Endless source of the built-in safe arrangement. `block_for_bars` returns
/// `Some` for every `(start_bar, bars)` — ranges beyond the session wrap
/// modulo the session length, so the fallback loops forever.
pub struct FallbackSource {
    session: Session,
    lane: TempoLane,
    /// Full compiled session, built lazily on first request.
    blocks: Option<Vec<Arc<CompiledBlock>>>,
}

impl FallbackSource {
    /// Builds the built-in safe arrangement: 4-on-the-floor kick, offbeat
    /// bass, simple hat, sustained pad; two alternating 8-bar sections at
    /// moderate energy (palette of `fixtures/loop-4track.ir.json`).
    pub fn new(seed: u64, sample_rate: u32) -> Self {
        Self::from_session(builtin_session(seed), sample_rate)
    }

    /// Wraps an arbitrary session into the endless-fallback machinery.
    /// Callers guarantee the session passes `validate_session` (the built-in
    /// one does by construction; restored ones are validated by `restore`).
    pub(crate) fn from_session(session: Session, sample_rate: u32) -> Self {
        // Boundary clamp: sample_rate 0 would degenerate every frame mapping.
        let sample_rate = sample_rate.max(1);
        // Cannot fail: the built-in lane is an authored constant, and restored
        // sessions passed validate_session, which subsumes TempoLane's rules.
        let lane = TempoLane::new(sample_rate, &session.tempo_lane)
            .expect("fallback session tempo lane is valid");
        FallbackSource { session, lane, blocks: None }
    }

    /// The session this source serves (the built-in safe arrangement, or the
    /// restored session when driven by [`crate::restore::RestoredSource`]).
    pub fn session(&self) -> &Session {
        &self.session
    }

    fn ensure_compiled(&mut self) {
        if self.blocks.is_none() {
            let rate = self.lane.sample_rate();
            self.blocks = Some(compile_session(&self.session, rate).unwrap_or_default());
        }
    }

    /// Builds the block covering `[start_bar, start_bar + bars)`: the request
    /// range is mapped onto the (cached) session blocks with wrap-around, and
    /// events are re-anchored to the request's block-relative frame origin.
    fn synthesize(
        &self,
        blocks: &[Arc<CompiledBlock>],
        start_bar: u32,
        bars: u32,
    ) -> Arc<CompiledBlock> {
        let start_frame = self.lane.frame_of_bar(f64::from(start_bar));
        let mut out =
            CompiledBlock { start_bar, bars, start_frame, tracks: Vec::new() };
        let total = self.session.total_bars();
        if blocks.is_empty() || total == 0 {
            return Arc::new(out);
        }
        let total = total as u32;

        let mut per_track: BTreeMap<u8, Vec<(u32, Event)>> = BTreeMap::new();
        let mut cursor = start_bar % total;
        let mut served = 0u64;
        while served < u64::from(bars) {
            // Blocks chain from bar 0 in BLOCK_BARS tiles (the last may be
            // short); `cursor < total` always lands inside that chain.
            let block = &blocks[(cursor / BLOCK_BARS) as usize];
            let seg_bars = (block.bars - (cursor - block.start_bar))
                .min((u64::from(bars) - served) as u32);
            let seg_frame_lo = self.lane.frame_of_bar(f64::from(cursor));
            let seg_frame_hi = self.lane.frame_of_bar(f64::from(cursor + seg_bars));
            // Offset of this segment's origin inside the synthesized block.
            let seg_offset = self
                .lane
                .frame_of_bar((u64::from(start_bar) + served) as f64)
                .saturating_sub(start_frame);
            for te in &block.tracks {
                let sink = per_track.entry(te.track).or_default();
                for (f, e) in &te.events {
                    let abs = block.start_frame + u64::from(*f);
                    if abs < seg_frame_lo || abs >= seg_frame_hi {
                        continue;
                    }
                    let rel = (abs - seg_frame_lo).saturating_add(seg_offset);
                    sink.push((u32::try_from(rel).unwrap_or(u32::MAX), *e));
                }
            }
            cursor = (cursor + seg_bars) % total;
            served += u64::from(seg_bars);
        }

        out.tracks = per_track
            .into_iter()
            .map(|(track, mut events)| {
                events.sort_by_key(|(f, _)| *f);
                TrackEvents { track, events }
            })
            .collect();
        Arc::new(out)
    }
}

impl BlockSource for FallbackSource {
    fn block_for_bars(&mut self, start_bar: u32, bars: u32) -> Option<Arc<CompiledBlock>> {
        self.ensure_compiled();
        let blocks = self.blocks.as_deref().unwrap_or(&[]);
        Some(self.synthesize(blocks, start_bar, bars))
    }
}

// -- Built-in safe arrangement ----------------------------------------------

fn step(position: u32, velocity: f32, pitch: f32, gate: f32) -> Step {
    Step {
        position,
        velocity,
        probability: 1.0,
        accent: false,
        microtiming_ticks: 0,
        ratchet: 1,
        pitch: Some(pitch),
        gate: Some(gate),
    }
}

fn euclid(k: u32, n: u32, velocity: f32, probability: f32) -> Pattern {
    Pattern::Euclidean(EuclideanPattern {
        generator: EuclideanTag::Euclidean,
        k,
        n,
        rot: 0,
        velocity,
        probability,
        repeats: 1,
        gate: None,
        pitch: None,
    })
}

fn hat_mask(density: f32) -> Pattern {
    Pattern::ProbabilityMask(ProbabilityMaskPattern {
        generator: ProbabilityMaskTag::ProbabilityMask,
        density,
        velocity: 0.5,
        probability: 0.85,
        repeats: 1,
        gate: None,
        pitch: None,
    })
}

fn bass_steps(steps: Vec<Step>) -> Pattern {
    Pattern::Steps(StepsPattern { steps, repeats: 1 })
}

fn pad_step(pitch: f32) -> Pattern {
    Pattern::Steps(StepsPattern { steps: vec![step(0, 0.5, pitch, 4.0)], repeats: 1 })
}

/// The authored safe arrangement. Values mirror `fixtures/loop-4track.ir.json`
/// (already a validated palette); section B stays at moderate energy.
fn builtin_session(seed: u64) -> Session {
    let mut a_binds = BTreeMap::new();
    a_binds.insert("kick".to_string(), euclid(4, 16, 0.85, 1.0));
    a_binds.insert("hat".to_string(), euclid(7, 16, 0.4, 0.9));
    a_binds.insert(
        "bass".to_string(),
        bass_steps(vec![
            step(480, 0.8, 36.0, 0.4),
            step(1440, 0.7, 36.0, 0.4),
            step(2400, 0.75, 41.0, 0.4),
            step(3360, 0.7, 39.0, 0.4),
        ]),
    );
    a_binds.insert("pad".to_string(), pad_step(55.0));

    let mut b_binds = BTreeMap::new();
    b_binds.insert("kick".to_string(), euclid(4, 16, 0.95, 1.0));
    b_binds.insert("hat".to_string(), hat_mask(0.5));
    b_binds.insert(
        "bass".to_string(),
        bass_steps(vec![
            step(480, 0.85, 36.0, 0.4),
            step(960, 0.6, 36.0, 0.3),
            step(1440, 0.8, 43.0, 0.4),
            step(2400, 0.8, 41.0, 0.4),
            step(2880, 0.6, 39.0, 0.3),
            step(3360, 0.75, 39.0, 0.4),
        ]),
    );
    b_binds.insert("pad".to_string(), pad_step(58.0));

    let mut b_automation = BTreeMap::new();
    b_automation.insert(
        "pad".to_string(),
        AutomationLane {
            target_param: "send_reverb".to_string(),
            points: vec![
                (0, 0.35, CurveKind::Linear),
                (4, 0.5, CurveKind::Linear),
                (7, 0.3, CurveKind::Smooth),
            ],
        },
    );

    let kick = Track {
        id: "kick".to_string(),
        role: TrackRole::Kick,
        instrument: InstrumentDef::Kick(KickInstrument {
            kind: KickTag::Kick,
            tune_hz: 47.0,
            decay_ms: 320.0,
            click: 0.4,
            drive: 0.3,
        }),
        inserts: vec![InsertDef {
            kind: InsertKind::Drive,
            params: serde_json::json!({ "amount": 1.2 }),
            mix: 0.4,
        }],
        sends: Sends { delay: 0.0, reverb: 0.05 },
        gain: 1.0,
        pan: 0.0,
        duck_depth: None,
    };
    let hat = Track {
        id: "hat".to_string(),
        role: TrackRole::Perc,
        instrument: InstrumentDef::Hat(HatInstrument {
            kind: HatTag::Hat,
            decay_ms: 60.0,
            tone: 0.6,
            open: false,
        }),
        inserts: vec![],
        sends: Sends { delay: 0.25, reverb: 0.1 },
        gain: 0.8,
        pan: 0.3,
        duck_depth: None,
    };
    let bass = Track {
        id: "bass".to_string(),
        role: TrackRole::Bass,
        instrument: InstrumentDef::Bass(BassInstrument {
            kind: BassTag::Bass,
            cutoff_hz: 900.0,
            resonance: 0.3,
            wave: Wave::Saw,
            glide_ms: 40.0,
        }),
        inserts: vec![],
        sends: Sends { delay: 0.0, reverb: 0.0 },
        gain: 0.9,
        pan: 0.0,
        duck_depth: None,
    };
    let pad = Track {
        id: "pad".to_string(),
        role: TrackRole::Pad,
        instrument: InstrumentDef::Pad(PadInstrument {
            kind: PadTag::Pad,
            attack_ms: 600.0,
            release_ms: 1200.0,
            detune_cents: 12.0,
            cutoff_hz: 2400.0,
        }),
        inserts: vec![],
        sends: Sends { delay: 0.1, reverb: 0.35 },
        gain: 0.7,
        pan: -0.2,
        duck_depth: None,
    };

    Session {
        version: IR_VERSION,
        seed,
        tempo_lane: vec![(0, FALLBACK_BPM)],
        key: Some("F minor".to_string()),
        souls: None,
        send_fx: None,
        pattern_engine: None,
        sections: vec![
            Section {
                id: "fb_a".to_string(),
                bars: 8,
                energy_curve: vec![0.4, 0.5, 0.55, 0.6, 0.6, 0.65, 0.7, 0.75],
                density_curve: Vec::new(),
                brightness_curve: Vec::new(),
                transition_in: Some(Transition {
                    kind: TransitionKind::FilterSweep,
                    bars: 2,
                    params: serde_json::json!({ "from_hz": 200, "to_hz": 18000 }),
                }),
                transition_out: None,
                pattern_bindings: a_binds,
                automation: BTreeMap::new(),
            },
            Section {
                id: "fb_b".to_string(),
                bars: 8,
                energy_curve: vec![0.75, 0.8, 0.85, 0.85, 0.85, 0.8, 0.75, 0.7],
                density_curve: Vec::new(),
                brightness_curve: Vec::new(),
                transition_in: None,
                transition_out: Some(Transition {
                    kind: TransitionKind::Riser,
                    bars: 2,
                    params: serde_json::json!({ "target": "noise" }),
                }),
                pattern_bindings: b_binds,
                automation: b_automation,
            },
        ],
        tracks: vec![kick, hat, bass, pad],
        palette: None,
        duck_release_ms: kontinuum_ir::DEFAULT_DUCK_RELEASE_MS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kontinuum_ir::{compile_session, validate_session};

    fn source(seed: u64) -> FallbackSource {
        FallbackSource::new(seed, 48_000)
    }

    fn track_dump(b: &CompiledBlock) -> String {
        format!("{:?}", b.tracks)
    }

    #[test]
    fn builtin_session_passes_validation() {
        let src = source(7);
        let verdict = validate_session(src.session());
        assert!(verdict.is_ok(), "built-in arrangement must validate: {:?}", verdict);
    }

    #[test]
    fn always_serves_some_for_arbitrary_ranges() {
        let mut src = source(7);
        for (start, bars) in
            [(0, 4), (2, 4), (7, 13), (100_000, 4), (123_456_789, 1), (u32::MAX - 3, 4), (0, 0)]
        {
            let block = src
                .block_for_bars(start, bars)
                .unwrap_or_else(|| panic!("no block for ({start}, {bars})"));
            assert_eq!((block.start_bar, block.bars), (start, bars));
        }
    }

    #[test]
    fn far_range_blocks_carry_music() {
        let mut src = source(7);
        let block = src.block_for_bars(100_000, 4).expect("block at bar 100000");
        assert!(block.total_events() > 0, "fallback must never serve silence");
        let kick_ons = block
            .tracks
            .iter()
            .filter(|t| t.track == 0)
            .flat_map(|t| &t.events)
            .filter(|(_, e)| matches!(e, Event::NoteOn { .. }))
            .count();
        assert!(kick_ons >= 16, "4-on-floor kick over 4 bars");
    }

    #[test]
    fn wraps_with_session_period() {
        let mut src = source(7);
        let base = src.block_for_bars(0, 4).expect("block 0");
        let wrapped = src.block_for_bars(FALLBACK_BARS, 4).expect("wrapped block");
        assert_eq!(track_dump(&base), track_dump(&wrapped), "bar N ≡ bar N + period");

        let off = src.block_for_bars(1, 4).expect("unaligned bar 1");
        let wrapped_off = src.block_for_bars(FALLBACK_BARS + 1, 4).expect("unaligned wrap");
        assert_eq!(track_dump(&off), track_dump(&wrapped_off), "unaligned bars loop too");
    }

    #[test]
    fn first_blocks_match_direct_compile() {
        let mut src = source(11);
        let compiled = compile_session(src.session(), 48_000).expect("compile");
        assert!(compiled.len() >= 4, "16-bar session compiles to 4 blocks");
        for b in &compiled {
            let got = src.block_for_bars(b.start_bar, b.bars).expect("block");
            assert_eq!(
                (got.start_bar, got.bars, got.start_frame),
                (b.start_bar, b.bars, b.start_frame)
            );
            assert_eq!(track_dump(&got), track_dump(b));
        }
    }
}
