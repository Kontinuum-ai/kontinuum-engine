//! Voice-slot assignment: round-robin over the role's pool. A slot is
//! reusable once its gate end is strictly before the new onset; overlapping
//! onsets walk to the next slot; an exhausted pool is the polyphony error the
//! validator surfaces as `E_POLYPHONY_EXCEEDED`.

use kontinuum_clock::TempoLane;
use kontinuum_schedule::{Event, TrackId};

use crate::compile::{pool_for_role, CompileError};
use crate::TrackRole;

/// One raw onset awaiting a voice slot.
pub(super) struct RawHit {
    pub seq: usize,
    pub velocity: f32,
    pub pitch: f32,
    pub micro: i16,
    pub gate_frames: u64,
}

pub(super) fn assign_slots(
    role: TrackRole,
    track_id: TrackId,
    hits: Vec<(u64, RawHit)>,
    start_frame: u64,
    end_frame: u64,
    lane: &TempoLane,
) -> Result<Vec<(u32, Event)>, CompileError> {
    let pool = pool_for_role(role) as usize;
    let mut slots: Vec<Option<u64>> = vec![None; pool];
    let sustained = crate::compile::is_sustained(role);
    let mut events: Vec<(u64, Event)> = Vec::with_capacity(hits.len() + 8);
    let last_frame = end_frame - 1;

    for (i, (frame, hit)) in hits.into_iter().enumerate() {
        let start = i % pool;
        let mut chosen: Option<usize> = None;
        for j in 0..pool {
            let s = (start + j) % pool;
            if slots[s].is_none_or(|off| off < frame) {
                chosen = Some(s);
                break;
            }
        }
        let Some(slot) = chosen else {
            let bar = lane.bar_at_frame(frame).floor() as u32;
            return Err(CompileError::VoicePoolExhausted { track: track_id, bar });
        };
        let off = frame.saturating_add(hit.gate_frames);
        slots[slot] = Some(off);
        events.push((
            frame,
            Event::NoteOn {
                voice: slot as u8,
                pitch: hit.pitch,
                velocity: hit.velocity,
                microtiming_ticks: hit.micro,
            },
        ));
        if sustained {
            // Clamp the release into the block so the voice is explicitly
            // freed; strays beyond the boundary are clamped, not dropped.
            let rel = off.min(last_frame).max(frame);
            events.push((rel, Event::NoteOff { voice: slot as u8 }));
        }
    }

    // Frame window filter (block-relative), stable sort keeps
    // onset-before-release ordering at equal frames.
    let mut out: Vec<(u32, Event)> = events
        .into_iter()
        .filter(|(f, _)| *f >= start_frame && *f < end_frame)
        .map(|(f, e)| ((f - start_frame) as u32, e))
        .collect();
    out.sort_by_key(|(f, _)| *f);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(seq: usize, frame: u64, gate: u64) -> (u64, RawHit) {
        (frame, RawHit { seq, velocity: 0.8, pitch: 60.0, micro: 0, gate_frames: gate })
    }

    #[test]
    fn round_robin_walks_pool_then_reuses_expired_slots() {
        // Bass pool = 4: five onsets cycle 0,1,2,3 then return to slot 0
        // whose gate has expired by onset 5's frame.
        let hits: Vec<(u64, RawHit)> =
            (0..5).map(|i| hit(i, i as u64 * 100, 100)).collect();
        let lane = TempoLane::constant(48_000, 120.0).expect("lane");
        let out = assign_slots(TrackRole::Bass, 0, hits, 0, 100_000, &lane).expect("fits");
        let voices: Vec<u8> = out
            .iter()
            .filter_map(|(_, e)| match e {
                Event::NoteOn { voice, .. } => Some(*voice),
                _ => None,
            })
            .collect();
        assert_eq!(voices, vec![0, 1, 2, 3, 0]);
    }

    #[test]
    fn exhaustion_reports_track_and_bar() {
        let lane = TempoLane::constant(48_000, 120.0).expect("lane");
        let hits: Vec<(u64, RawHit)> = (0..5).map(|i| hit(i, i as u64 * 10, 100_000)).collect();
        let err = assign_slots(TrackRole::Bass, 3, hits, 0, 1_000_000, &lane).expect_err("full");
        assert!(matches!(err, CompileError::VoicePoolExhausted { track: 3, .. }));
    }
}
