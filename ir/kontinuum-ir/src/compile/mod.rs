//! IR → `CompiledBlock` compiler (issue #11).
//!
//! Pure function: same session document + sample rate → identical block
//! contents. Sections chain back-to-back from bar 0 in 4-bar blocks. Events
//! carry block-relative frames; onsets claim a round-robin voice slot within
//! the role's pool and slots are only reused once their gate has expired, so
//! overlapping onsets spread across the pool and exhaustion is an error the
//! validator surfaces as `E_POLYPHONY_EXCEEDED`.

pub mod estimate;
pub mod expand;
pub mod patch;
pub mod slots;

use std::collections::HashMap;
use std::sync::Arc;

use kontinuum_clock::{stream, MusicalTime, TempoLane, TICKS_PER_BAR};
use kontinuum_schedule::{CompiledBlock, Event, TrackEvents, TrackId};

use crate::schema::{bounds, Session};
use expand::{default_gate_beats, expand_pattern, mask_rng, Onset};
use slots::{assign_slots, RawHit};

/// RNG stream selectors for the compile-time probability gate
/// (`kontinuum_clock::stream(seed, lane, purpose)`).
const LANE_PROBABILITY: u8 = 0xFD;
const PURPOSE_PROBABILITY: u16 = 0xA3;

/// Compile granularity (issue #11: block size = 4 bars).
pub const BLOCK_BARS: u32 = 4;
/// Per-block CPU budget in estimate units (validated, not enforced here).

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum CompileError {
    #[error("tempo lane invalid: {reason}")]
    Tempo { reason: &'static str },
    #[error("session has no sections (or a zero-bar section)")]
    NoSections,
    #[error("session exceeds the {0}-bar compile ceiling; split it into sessions")]
    SessionTooLarge(u64),
    #[error("too many tracks (max 255)")]
    TooManyTracks,
    #[error("voice pool exhausted for track {track} near bar {bar}; shorten gates or thin the pattern")]
    VoicePoolExhausted { track: u8, bar: u32 },
}

/// Aggregate stats for the CLI and supervision watchdog.
#[derive(Clone, Debug, PartialEq)]
pub struct CompileSummary {
    pub blocks: usize,
    pub events_total: usize,
    pub cpu_estimate: f64,
}

/// Compiles a validated session into chained 4-bar blocks. Input is expected
/// to pass `validate_session` first; cheap structural guards here only prevent
/// pathological input from looping or overflowing.
pub fn compile_session(
    session: &Session,
    sample_rate: u32,
) -> Result<Vec<Arc<CompiledBlock>>, CompileError> {
    if session.sections.is_empty() || session.sections.iter().any(|s| s.bars == 0) {
        return Err(CompileError::NoSections);
    }
    if session.tracks.len() > bounds::MAX_TRACKS {
        return Err(CompileError::TooManyTracks);
    }
    let total = session.total_bars();
    if total > bounds::MAX_SESSION_BARS {
        return Err(CompileError::SessionTooLarge(bounds::MAX_SESSION_BARS));
    }
    let total = total as u32;
    let lane = TempoLane::new(sample_rate, &session.tempo_lane)
        .map_err(|e| CompileError::Tempo { reason: e.reason })?;

    let spans: Vec<(u32, u32)> = session
        .section_start_bars()
        .into_iter()
        .zip(session.sections.iter().map(|s| s.bars))
        .map(|(start, bars)| (start, start + bars))
        .collect();

    // Phrase expansion cache keyed by (section, track, phrase index): mask RNG
    // streams are drawn once per phrase, independent of block boundaries.
    let mut cache: HashMap<(usize, usize, u32), Vec<Onset>> = HashMap::new();
    let mut gate_rng = stream(session.seed, LANE_PROBABILITY, PURPOSE_PROBABILITY);

    let mut blocks: Vec<Arc<CompiledBlock>> = Vec::with_capacity((total / BLOCK_BARS + 1) as usize);
    let mut b0 = 0u32;
    while b0 < total {
        let nb = BLOCK_BARS.min(total - b0);
        let end_bar = b0 + nb;
        let start_frame = lane.frame_of_bar(f64::from(b0));
        let end_frame = lane.frame_of_bar(f64::from(end_bar));

        let mut per_track: Vec<Vec<(u64, RawHit)>> =
            (0..session.tracks.len()).map(|_| Vec::new()).collect();
        let mut ramps: Vec<Vec<(u64, Event)>> =
            (0..session.tracks.len()).map(|_| Vec::new()).collect();

        for (si, section) in session.sections.iter().enumerate() {
            let (sec_start, sec_end) = spans[si];
            let lo = b0.max(sec_start);
            let hi = end_bar.min(sec_end);
            if lo >= hi {
                continue;
            }
            for (ti, track) in session.tracks.iter().enumerate() {
                let Some(pattern) = section.pattern_bindings.get(&track.id) else {
                    continue;
                };
                let repeats = pattern.repeats().max(1);
                let p_lo = (lo - sec_start) / repeats;
                let p_hi = (hi - 1 - sec_start) / repeats;
                for p in p_lo..=p_hi {
                    let key = (si, ti, p);
                    let onsets = match cache.get(&key) {
                        Some(v) => v,
                        None => {
                            let mut rng = mask_rng(session.seed, &section.id, ti as u8, p);
                            cache.entry(key).or_insert_with(|| {
                                expand_pattern(pattern, &mut rng)
                                    .into_iter()
                                    .filter(|o| gate_rng.chance(o.probability))
                                    .collect()
                            })
                        }
                    };
                    let sink = &mut per_track[ti];
                    for o in onsets {
                        let bar_in = (o.pos_ticks / TICKS_PER_BAR) as u32;
                        let abs_bar = sec_start + p * repeats + bar_in;
                        if abs_bar < lo || abs_bar >= hi {
                            continue;
                        }
                    let tick_abs =
                        abs_bar as u64 * TICKS_PER_BAR + o.pos_ticks % TICKS_PER_BAR;
                    let bar_f = tick_abs as f64 / TICKS_PER_BAR as f64;
                    // All durations derive from the lane's own frame mapping
                    // (finite differences of time_at_bar), so microtiming and
                    // gates stay consistent with NoteOn frames under any bar
                    // length convention.
                    let sec_per_tick =
                        (lane.time_at_bar(bar_f + 1.0) - lane.time_at_bar(bar_f)) / TICKS_PER_BAR as f64;
                    let frame_f = lane.frame_of(MusicalTime::from_ticks(tick_abs)) as f64
                        + f64::from(o.microtiming_ticks) * sec_per_tick * f64::from(sample_rate);
                    let frame = frame_f.max(0.0).round() as u64;
                    let gate_beats = o.gate_beats.unwrap_or_else(|| default_gate_beats(track.role));
                    let gate_frames = lane
                        .frame_of_bar(bar_f + f64::from(gate_beats) / 4.0)
                        .saturating_sub(lane.frame_of_bar(bar_f))
                        .max(1);
                    sink.push((
                        frame,
                        RawHit {
                            seq: sink.len(),
                            velocity: o.velocity,
                            pitch: o.pitch.unwrap_or(60.0),
                            micro: o.microtiming_ticks,
                            gate_frames,
                        },
                    ));
                    }
                }
            }
        }

        // Automation lanes → ParamRamp events.
        for (si, section) in session.sections.iter().enumerate() {
            let (sec_start, _) = spans[si];
            for (track_id_str, lane_def) in &section.automation {
                let Some(ti) = session.tracks.iter().position(|t| &t.id == track_id_str) else {
                    continue;
                };
                let Some(param) = resolve_param(ti as u8, &lane_def.target_param) else {
                    continue;
                };
                for (pi, (bar_off, value, curve)) in lane_def.points.iter().enumerate() {
                    let abs_bar = sec_start + bar_off;
                    let frame = lane.frame_of_bar(f64::from(abs_bar));
                    if frame < start_frame || frame >= end_frame {
                        continue;
                    }
                    let next_bar = lane_def
                        .points
                        .get(pi + 1)
                        .map(|(nb, _, _)| sec_start + *nb)
                        .unwrap_or(abs_bar + 1);
                    let next_frame = lane.frame_of_bar(f64::from(next_bar));
                    let duration_frames = next_frame.saturating_sub(frame).min(u32::MAX as u64) as u32;
                    ramps[ti].push((
                        frame,
                        Event::ParamRamp {
                            param,
                            target: *value,
                            duration_frames: duration_frames.max(1),
                            curve: curve.to_ramp(),
                        },
                    ));
                }
            }
        }

        let mut tracks: Vec<TrackEvents> = Vec::new();
        for (ti, track) in session.tracks.iter().enumerate() {
            let track_id: TrackId = ti as u8;
            let mut hits = std::mem::take(&mut per_track[ti]);
            let block_ramps = std::mem::take(&mut ramps[ti]);
            if hits.is_empty() && block_ramps.is_empty() {
                continue;
            }
            hits.sort_by_key(|(f, h)| (*f, h.seq));
            let mut events =
                assign_slots(track.role, track_id, hits, start_frame, end_frame, &lane)?;
            events.extend(
                block_ramps
                    .into_iter()
                    .filter(|(f, _)| *f >= start_frame && *f < end_frame)
                    .map(|(f, e)| ((f - start_frame) as u32, e)),
            );
            events.sort_by_key(|(f, _)| *f);
            if !events.is_empty() {
                tracks.push(TrackEvents { track: track_id, events });
            }
        }

        blocks.push(Arc::new(CompiledBlock {
            start_bar: b0,
            bars: nb,
            start_frame,
            tracks,
        }));
        b0 = end_bar;
    }
    Ok(blocks)
}

/// Compile + aggregate stats in one call (CLI / supervision).
pub fn compile_session_summary(
    session: &Session,
    sample_rate: u32,
) -> Result<CompileSummary, CompileError> {
    let blocks = compile_session(session, sample_rate)?;
    let cpu_estimate = estimate_peak_cpu(session, &blocks, sample_rate)?;
    Ok(CompileSummary {
        blocks: blocks.len(),
        events_total: blocks.iter().map(|b| b.total_events()).sum(),
        cpu_estimate,
    })
}

// Re-exports for downstream crates and tests.
pub use expand::{
    is_sustained, onsets_per_bar, pool_for_role, resolve_param, role_cost, PARAM_INSERT0,
    PARAM_INSERT1, PARAM_SEND_DELAY, PARAM_SEND_REVERB, PARAM_TRACK_GAIN, PARAM_TRACK_PAN,
};
pub use estimate::{
    estimate_peak_cpu, node_cost, patch_cost, worst_block_cost, CPU_BUDGET_UNITS,
};
pub use patch::{compile_patch, CompiledPatch, DelayLineSpec, PatchCompileError};

#[cfg(test)]
mod tests {
    use super::*;
    use kontinuum_schedule::RampCurve;
    use crate::schema::*;

    fn minimal(track_json: &str, pattern_json: &str) -> Session {
        let doc = format!(
            r#"{{
                "version": 1, "seed": 1,
                "tempo_lane": [[0, 120.0]],
                "sections": [{{"id": "a", "bars": 4, "energy_curve": [0.5],
                    "pattern_bindings": {{"k": {pattern_json}}}}}],
                "tracks": [{track_json}]
            }}"#
        );
        serde_json::from_str(&doc).expect("session")
    }

    fn kick_track() -> String {
        r#"{"id": "k", "role": "kick", "instrument": {"kind": "kick"}}"#.to_string()
    }

    #[test]
    fn compiles_four_on_floor_into_one_block() {
        let s = minimal(&kick_track(), r#"{"generator":"euclidean","k":4,"n":16}"#);
        let blocks = compile_session(&s, 48_000).expect("compile");
        assert_eq!(blocks.len(), 1);
        let b = &blocks[0];
        assert_eq!((b.start_bar, b.bars), (0, 4));
        assert_eq!(b.start_frame, 0);
        let ons = b.tracks[0]
            .events
            .iter()
            .filter(|(_, e)| matches!(e, Event::NoteOn { .. }))
            .count();
        assert_eq!(ons, 16, "4 onsets x 4 bars");
        assert_eq!(b.total_events(), 16, "one-shot drums emit no NoteOff");
    }

    #[test]
    fn events_are_block_relative_and_sorted() {
        let s = minimal(&kick_track(), r#"{"generator":"euclidean","k":16,"n":16}"#);
        let blocks = compile_session(&s, 48_000).expect("compile");
        for b in &blocks {
            for te in &b.tracks {
                assert!(!te.events.is_empty());
                for w in te.events.windows(2) {
                    assert!(w[0].0 <= w[1].0, "events must be frame-sorted");
                }
                assert!(te.events.iter().all(|(f, _)| (*f as u64) < 1 << 32));
            }
        }
    }

    #[test]
    fn sustained_roles_get_noteoffs() {
        let bass = r#"{"id":"k","role":"bass","instrument":{"kind":"bass"}}"#;
        let s = minimal(bass, r#"{"steps":[{"position":0,"pitch":36.0,"gate":2.0}]}"#);
        let blocks = compile_session(&s, 48_000).expect("compile");
        let evs = &blocks[0].tracks[0].events;
        assert!(matches!(evs[0].1, Event::NoteOn { voice: 0, .. }));
        assert!(
            evs.iter().any(|(_, e)| matches!(e, Event::NoteOff { voice: 0 })),
            "bass must be released"
        );
    }

    #[test]
    fn polyphony_overflow_is_an_error() {
        let bass = r#"{"id":"k","role":"bass","instrument":{"kind":"bass"}}"#;
        // 8 sustained overlapping onsets per bar vs pool of 4.
        let steps: Vec<String> = (0..8)
            .map(|i| format!(r#"{{"position":{},"gate":16.0,"pitch":36.0}}"#, i * 60))
            .collect();
        let s = minimal(bass, &format!(r#"{{"steps":[{}]}}"#, steps.join(",")));
        let err = compile_session(&s, 48_000).expect_err("pool exhausted");
        assert!(matches!(err, CompileError::VoicePoolExhausted { track: 0, .. }));
    }

    #[test]
    fn slot_reuse_across_pool() {
        // Kick pool is 8: 16 onsets per bar at 16th spacing with the default
        // half-beat gate overlap 2 slots' worth — must stay within the pool.
        let s = minimal(&kick_track(), r#"{"generator":"euclidean","k":16,"n":16}"#);
        let blocks = compile_session(&s, 48_000).expect("compile");
        let voices: Vec<u8> = blocks[0].tracks[0]
            .events
            .iter()
            .filter_map(|(_, e)| e.voice_slot())
            .collect();
        assert!(voices.iter().all(|v| *v < expand::POOL_KICK));
        assert!(voices.contains(&0) && voices.contains(&1), "round-robin walks the pool");
    }

    #[test]
    fn param_ramps_land_in_blocks() {
        let doc = r#"{
            "version": 1, "seed": 1, "tempo_lane": [[0, 120.0]],
            "sections": [{"id": "a", "bars": 8, "energy_curve": [0.5],
                "pattern_bindings": {"k": {"generator": "euclidean", "k": 4, "n": 16}},
                "automation": {"k": {"target_param": "gain",
                    "points": [[0, 0.5, "linear"], [4, 1.0, "exp"]]}}}],
            "tracks": [{"id": "k", "role": "kick", "instrument": {"kind": "kick"}}]
        }"#;
        let s: Session = serde_json::from_str(doc).expect("session");
        let blocks = compile_session(&s, 48_000).expect("compile");
        let ramp = blocks[0].tracks[0]
            .events
            .iter()
            .find_map(|(_, e)| match e {
                Event::ParamRamp { param, target, curve, .. } => Some((*param, *target, *curve)),
                _ => None,
            })
            .expect("ramp in block 0");
        assert_eq!(ramp.0, resolve_param(0, "gain").expect("gain"));
        assert_eq!(ramp.1, 0.5);
        assert_eq!(ramp.2, RampCurve::Linear);
        // Second point lives in block 1.
        let ramp2 = blocks[1].tracks[0]
            .events
            .iter()
            .find_map(|(_, e)| match e {
                Event::ParamRamp { target, curve, .. } => Some((*target, *curve)),
                _ => None,
            })
            .expect("ramp in block 1");
        assert_eq!(ramp2, (1.0, RampCurve::Exponential));
    }

    #[test]
    fn microtiming_shifts_frames() {
        let early = r#"{"steps":[{"position":480,"microtiming_ticks":-120}]}"#;
        let late = r#"{"steps":[{"position":480,"microtiming_ticks":120}]}"#;
        let s1 = minimal(&kick_track(), early);
        let s2 = minimal(&kick_track(), late);
        let b1 = compile_session(&s1, 48_000).expect("compile")[0].clone();
        let b2 = compile_session(&s2, 48_000).expect("compile")[0].clone();
        let f1 = b1.tracks[0].events[0].0;
        let f2 = b2.tracks[0].events[0].0;
        assert!(f1 < f2, "negative micro lands earlier: {f1} vs {f2}");
    }

    #[test]
    fn compile_is_deterministic() {
        let s = minimal(&kick_track(), r#"{"generator":"euclidean","k":7,"n":16}"#);
        let a = compile_session(&s, 48_000).expect("compile");
        let b = compile_session(&s, 48_000).expect("compile");
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
    }

    fn fixture_session() -> Session {
        let manifest = env!("CARGO_MANIFEST_DIR");
        let raw = std::fs::read_to_string(format!("{manifest}/fixtures/loop-4track.ir.json"))
            .expect("golden fixture");
        serde_json::from_str(&raw).expect("golden fixture parses")
    }

    fn normalize_probability(session: &mut Session) {
        for section in &mut session.sections {
            for pattern in section.pattern_bindings.values_mut() {
                match pattern {
                    Pattern::Steps(p) => {
                        for s in &mut p.steps {
                            s.probability = 1.0;
                        }
                    }
                    Pattern::Euclidean(p) => p.probability = 1.0,
                    Pattern::ProbabilityMask(p) => p.probability = 1.0,
                }
            }
        }
    }

    fn events_total(blocks: &[Arc<CompiledBlock>]) -> usize {
        blocks.iter().map(|b| b.total_events()).sum()
    }

    #[test]
    fn probability_one_compiles_byte_identically_to_ungated_baseline() {
        // Digest pinned against the pre-gate compiler: an all-1.0 session
        // (the golden fixture with its authored probabilities normalized) must
        // reproduce the exact pre-change block contents.
        let mut s = fixture_session();
        normalize_probability(&mut s);
        let blocks = compile_session(&s, 48_000).expect("compile");
        assert_eq!(events_total(&blocks), 360, "no onset may appear or vanish");
        let digest = expand::fnv1a(format!("{blocks:?}").as_bytes());
        assert_eq!(digest, 0x9570_b286_23b9_bf59, "byte-identical to baseline");
    }

    #[test]
    fn probability_gate_is_deterministic_and_zero_probability_emits_nothing() {
        let zero = minimal(
            &kick_track(),
            r#"{"steps":[{"position":0,"probability":0.0},{"position":480,"probability":0.0}]}"#,
        );
        let blocks = compile_session(&zero, 48_000).expect("compile");
        assert_eq!(events_total(&blocks), 0, "p=0.0 steps emit zero onsets");

        let mixed = minimal(
            &kick_track(),
            r#"{"steps":[{"position":0,"probability":0.3},
                         {"position":480,"probability":0.6},
                         {"position":960,"probability":0.9}]}"#,
        );
        let a = compile_session(&mixed, 48_000).expect("compile");
        let b = compile_session(&mixed, 48_000).expect("compile");
        assert_eq!(format!("{a:?}"), format!("{b:?}"), "same session = same gate decisions");
        // Ungated this pattern is 12 onsets (3 steps x 4 phrase repeats); the
        // gate must actually drop some, else the pass is vacuous.
        assert!(events_total(&a) < 12, "gate dropped nothing: {}", events_total(&a));
    }

    #[test]
    fn gated_fixture_compiles_identically_across_runs() {
        let s = fixture_session();
        let a = compile_session(&s, 48_000).expect("compile");
        let b = compile_session(&s, 48_000).expect("compile");
        assert_eq!(a.len(), 4, "block count unchanged by the gate");
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
    }

    #[test]
    fn guards_reject_pathological_input() {
        let mut s = minimal(&kick_track(), r#"{"generator":"euclidean","k":4,"n":16}"#);
        s.sections.clear();
        assert!(matches!(
            compile_session(&s, 48_000),
            Err(CompileError::NoSections)
        ));
        let mut s = minimal(&kick_track(), r#"{"generator":"euclidean","k":4,"n":16}"#);
        s.sections[0].bars = 100_000_000;
        assert!(matches!(
            compile_session(&s, 48_000),
            Err(CompileError::SessionTooLarge(_))
        ));
    }
}
