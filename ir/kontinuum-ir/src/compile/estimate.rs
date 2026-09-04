//! Peak-CPU estimation for compiled blocks: per-role cost table × peak
//! concurrent voices, maximised over blocks.
//!
//! Issue #37: a `Custom` patch track's per-voice cost comes from the patch's
//! own node graph (the per-node table below), not the role average — so an
//! over-ambitious LLM rack shows up in the estimate and is rejected by the
//! dry-run budget check with a per-track cost report, never clamped.

use std::sync::Arc;

use kontinuum_clock::TempoLane;
use kontinuum_schedule::{CompiledBlock, Event};

use crate::compile::{is_sustained, role_cost, CompileError};
use crate::patch::{CustomPatch, OscWave, PatchNode};
use crate::schema::Session;
use crate::TrackRole;

/// Per-block CPU budget in estimate units (validated, not enforced here).
pub const CPU_BUDGET_UNITS: f64 = 100.0;

/// Per-node cost in estimate units, one active voice. Calibrated against the
/// role table in `expand.rs` (kick 1.0, bass 2.0, pad 3.0): a 7-voice unison
/// saw lands near one kick, a full formant pad voice near two. Audio-rate
/// nodes dominate; env/LFO run at control rate (~1/64 of a sample-rate voice).
pub fn node_cost(node: &PatchNode) -> f32 {
    match node {
        PatchNode::Osc(o) => {
            let per_voice = match o.wave {
                // polyBLEP saw/square cost edges per voice; sine/tri are
                // closed-form; noise is xorshift.
                OscWave::Saw => 0.08,
                OscWave::Square => 0.14,
                OscWave::Sine => 0.05,
                OscWave::Tri => 0.04,
                OscWave::Noise => 0.02,
            };
            per_voice * f32::from(o.unison.max(1))
        }
        PatchNode::FmPair(_) => 0.12,
        PatchNode::Filter(_) => 0.10,
        PatchNode::Ring(_) => 0.03,
        PatchNode::Shaper(_) => 0.06,
        PatchNode::Formant(_) => 0.30,
        PatchNode::Sampler(_) => 0.05,
        PatchNode::Env(_) | PatchNode::Lfo(_) => 0.005,
        PatchNode::Gain(_) | PatchNode::Out(_) => 0.01,
        // Delay-line ring read + recirc write; the buffer is memory, not ALU.
        PatchNode::Delay(_) => 0.04,
    }
}

/// Total per-voice cost of one patch in estimate units.
pub fn patch_cost(patch: &CustomPatch) -> f32 {
    patch.patch.nodes.iter().map(node_cost).sum()
}

/// Peak per-block CPU estimate. One-shot voices are assumed active for half a
/// beat; sustained voices until their NoteOff. Durations use the lane's own
/// frame mapping (finite differences of `time_at_bar`), matching event frames.
pub fn estimate_peak_cpu(
    session: &Session,
    blocks: &[Arc<CompiledBlock>],
    sample_rate: u32,
) -> Result<f64, CompileError> {
    Ok(worst_block_cost(session, blocks, sample_rate)?.0)
}

/// (worst-block cost, per-track cost within that block), descending by cost.
/// The breakdown is the cost report attached to `E_CPU_BUDGET_EXCEEDED`.
pub fn worst_block_cost(
    session: &Session,
    blocks: &[Arc<CompiledBlock>],
    sample_rate: u32,
) -> Result<(f64, Vec<(String, f64)>), CompileError> {
    let lane = TempoLane::new(sample_rate, &session.tempo_lane)
        .map_err(|e| CompileError::Tempo { reason: e.reason })?;
    let mut worst: Option<(f64, Vec<(String, f64)>)> = None;
    for block in blocks {
        let b = f64::from(block.start_bar);
        let bar_frames = (lane.time_at_bar(b + 1.0) - lane.time_at_bar(b)) * f64::from(sample_rate);
        let beat_frames = bar_frames / 4.0;
        let mut costs: Vec<(String, f64)> = Vec::with_capacity(block.tracks.len());
        for te in &block.tracks {
            let Some(track) = session.tracks.get(te.track as usize) else {
                continue;
            };
            let conc = peak_concurrency(track.role, &te.events, beat_frames);
            let per_voice = instrument_cost(&track.instrument, track.role);
            costs.push((track.id.clone(), f64::from(per_voice) * f64::from(conc)));
        }
        let total: f64 = costs.iter().map(|(_, c)| *c).sum();
        if worst.as_ref().is_none_or(|(w, _)| total > *w) {
            costs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            worst = Some((total, costs));
        }
    }
    Ok(worst.unwrap_or_default())
}

/// Per-voice cost for one track's instrument: the patch's own node cost for
/// `Custom`, the role average otherwise.
fn instrument_cost(instrument: &crate::schema::InstrumentDef, role: TrackRole) -> f32 {
    match instrument {
        crate::schema::InstrumentDef::Custom(patch) => patch_cost(patch),
        _ => role_cost(role),
    }
}

/// Peak simultaneous voices for one track's sorted event list.
fn peak_concurrency(role: TrackRole, events: &[(u32, Event)], beat_frames: f64) -> u32 {
    let drum_frames = (0.5 * beat_frames).round() as u64;
    let sustained = is_sustained(role);
    let mut active: Vec<(u8, u64)> = Vec::new();
    let mut peak = 0u32;
    for (f, e) in events {
        let frame = u64::from(*f);
        active.retain(|(_, off)| *off > frame);
        match e {
            Event::NoteOn { voice, .. } => {
                let assumed = if sustained { u64::MAX } else { drum_frames };
                active.push((*voice, frame.saturating_add(assumed)));
                peak = peak.max(active.len() as u32);
            }
            Event::NoteOff { voice } => {
                for a in &mut active {
                    if a.0 == *voice {
                        a.1 = frame;
                    }
                }
            }
            Event::ParamRamp { .. } | Event::SampleTrigger { .. } => {}
        }
    }
    peak
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::{CustomTag, OscNode, OscTag, OutNode, OutTag, PatchGraph};

    #[test]
    fn one_shot_decay_bounds_concurrency() {
        let evs = vec![
            (0u32, Event::NoteOn { voice: 0, pitch: 60.0, velocity: 1.0, microtiming_ticks: 0 }),
            (10, Event::NoteOn { voice: 1, pitch: 60.0, velocity: 1.0, microtiming_ticks: 0 }),
            (1000, Event::NoteOn { voice: 2, pitch: 60.0, velocity: 1.0, microtiming_ticks: 0 }),
        ];
        // Half-beat window of 500 frames: the third hit lands after the first
        // two expired.
        assert_eq!(peak_concurrency(TrackRole::Kick, &evs, 1000.0), 2);
    }

    #[test]
    fn sustained_voices_count_until_noteoff() {
        let evs = vec![
            (0u32, Event::NoteOn { voice: 0, pitch: 60.0, velocity: 1.0, microtiming_ticks: 0 }),
            (10, Event::NoteOn { voice: 1, pitch: 60.0, velocity: 1.0, microtiming_ticks: 0 }),
            (20, Event::NoteOff { voice: 0 }),
            (30, Event::NoteOff { voice: 1 }),
            (40, Event::NoteOn { voice: 0, pitch: 36.0, velocity: 1.0, microtiming_ticks: 0 }),
        ];
        assert_eq!(peak_concurrency(TrackRole::Pad, &evs, 1000.0), 2);
    }

    fn osc(id: &str, wave: OscWave, unison: u8) -> PatchNode {
        PatchNode::Osc(OscNode {
            id: id.into(),
            kind: OscTag::Osc,
            wave,
            unison,
            fine_cents: 0.0,
            level: 1.0,
        })
    }

    #[test]
    fn node_cost_table_scales_with_unison_and_wave() {
        let mut g = PatchGraph { nodes: vec![osc("o", OscWave::Saw, 7)], edges: Vec::new() };
        g.nodes.push(PatchNode::Out(OutNode { id: "x".into(), kind: OutTag::Out, level: 1.0 }));
        let p = CustomPatch { kind: CustomTag::Custom, patch: g };
        // 7 saw voices (7 × 0.08) + out (0.01): one hoover voice ≈ one kick.
        let cost = patch_cost(&p);
        assert!((0.55..=0.60).contains(&cost), "hoover cost {cost}");
        assert_eq!(cost, patch_cost(&p), "cost is a pure function of the patch");
    }

    #[test]
    fn custom_patch_cost_replaces_role_cost_in_estimate() {
        let doc = r#"{
            "version": 1, "seed": 1,
            "tempo_lane": [[0, 120.0]],
            "sections": [{"id": "a", "bars": 2, "energy_curve": [0.5],
                "pattern_bindings": {"p": {"generator": "euclidean", "k": 2, "n": 16, "gate": 4.0}}}],
            "tracks": [{"id": "p", "role": "pad", "instrument": {"kind": "custom", "patch": {
                "nodes": [
                    {"id": "o", "type": "osc", "wave": "sine", "unison": 1},
                    {"id": "x", "type": "out"}],
                "edges": [{"from": "o", "to": "x", "type": "audio"}]}}}]
        }"#;
        let s: Session = serde_json::from_str(doc).expect("session");
        let blocks = crate::compile::compile_session(&s, 48_000).expect("compile");
        let est = estimate_peak_cpu(&s, &blocks, 48_000).expect("estimate");
        // One sustained sine voice at 0.05 + 0.01 units: the patch cost, not
        // the pad role's 3.0.
        assert!(est < 0.2, "patch cost must drive the estimate, got {est}");
        let (_, report) = worst_block_cost(&s, &blocks, 48_000).expect("report");
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].0, "p");
        assert!(report[0].1 > 0.0);
    }
}
