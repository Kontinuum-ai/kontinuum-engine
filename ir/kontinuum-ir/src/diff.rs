//! IR diff mutations (issue #11): the composer's only write path.
//!
//! Semantics: diffs apply to **future material only** — an op whose target
//! bars overlap `[0, at_bar)` is rejected with [`ApplyError::InPast`].
//! `SetSectionEnergy` and `ScheduleTransition` are future-anchored by their
//! own bar arguments and are exempt. `SetInstrumentParam`/`SwapSample`/
//! `SwapInstrument` and
//! `SetKey` are track-/session-level (no bar target) and affect future
//! renders only. `SetTempo` inserts its breakpoint at `at_bar`, which callers
//! quantize to a section boundary (landing rules in `kontinuum-compose::dj`).
//!
//! Live-move carve-out (issue #38): `ReplacePattern` and `ExtendSection` may
//! also target a section that is **currently playing** — the engine's block
//! cache keeps every bar before the diff boundary bit-identical, so played
//! audio never changes; only sections that have fully *ended* are past.
//!
//! Writes are last-writer-wins; anything displaced is logged in
//! [`ApplyReport::superseded`].
//!
//! allow: SIZE_OK — one match arm per serde op over the session document; the
//! op set is pinned by the IR contract and the file predates issue #38.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::schema::{
    bounds, AutomationLane, InstrumentDef, MusicalKey, Pattern, Section, Session, Transition,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum IrDiff {
    ReplacePattern { section: String, track: String, pattern: Pattern },
    SetAutomation { section: String, track: String, lane: AutomationLane },
    AddSection { after: Option<String>, section: Section },
    ExtendSection { id: String, extra_bars: u32 },
    SetInstrumentParam { track: String, param: String, value: f32 },
    SwapSample { track: String, sample_id: u32 },
    SwapInstrument { track: String, instrument: InstrumentDef },
    SetSectionEnergy { id: String, energy: Vec<f32> },
    ScheduleTransition { at_bar: u32, transition: Transition },
    SetTempo { bpm: f64 },
    SetKey { key: MusicalKey },
}

#[derive(Clone, Debug, PartialEq, Error)]
pub enum ApplyError {
    #[error("operation targets material before bar {0}; diffs apply to future material only")]
    InPast(u32),
    #[error("unknown section `{0}`")]
    UnknownSection(String),
    #[error("unknown track `{0}`")]
    UnknownTrack(String),
    #[error("{0}")]
    Invalid(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApplyReport {
    pub applied: Vec<String>,
    pub superseded: Vec<String>,
}

impl ApplyReport {
    fn single(applied: String, superseded: Vec<String>) -> Self {
        ApplyReport { applied: vec![applied], superseded }
    }
}

/// Start bar of every section, by document index.
fn section_starts(s: &Session) -> Vec<u32> {
    s.section_start_bars()
}

fn section_index(s: &Session, id: &str) -> Result<usize, ApplyError> {
    s.sections
        .iter()
        .position(|sec| sec.id == id)
        .ok_or_else(|| ApplyError::UnknownSection(id.to_string()))
}

fn track_index(s: &Session, id: &str) -> Result<usize, ApplyError> {
    s.tracks
        .iter()
        .position(|t| t.id == id)
        .ok_or_else(|| ApplyError::UnknownTrack(id.to_string()))
}

/// Live-move carve-out (issue #38): a section still in flight counts as
/// editable — the engine's block cache keeps bars before the diff boundary
/// bit-identical — while a section that has fully ended is past.
fn section_in_past(s: &Session, si: usize, at_bar: u32) -> bool {
    let end = section_starts(s)[si].saturating_add(s.sections[si].bars);
    end <= at_bar
}

/// Applies one diff op. `at_bar` is the playhead: material at or after it is
/// mutable, everything before is frozen.
pub fn apply_diff(
    session: &mut Session,
    diff: &IrDiff,
    at_bar: u32,
) -> Result<ApplyReport, ApplyError> {
    match diff {
        IrDiff::ReplacePattern { section, track, pattern } => {
            let si = section_index(session, section)?;
            track_index(session, track)?;
            if section_in_past(session, si, at_bar) {
                return Err(ApplyError::InPast(at_bar));
            }
            let prev = session.sections[si]
                .pattern_bindings
                .insert(track.clone(), pattern.clone());
            let superseded = prev.is_some().then(|| format!("replace_pattern:{track}@{section}"));
            Ok(ApplyReport::single(
                format!("replace_pattern:{track}@{section}"),
                superseded.into_iter().collect(),
            ))
        }
        IrDiff::SetAutomation { section, track, lane } => {
            let si = section_index(session, section)?;
            track_index(session, track)?;
            if section_starts(session)[si] < at_bar {
                return Err(ApplyError::InPast(at_bar));
            }
            let prev = session.sections[si]
                .automation
                .insert(track.clone(), lane.clone());
            let superseded = prev.is_some().then(|| format!("set_automation:{track}@{section}"));
            Ok(ApplyReport::single(
                format!("set_automation:{track}@{section}"),
                superseded.into_iter().collect(),
            ))
        }
        IrDiff::AddSection { after, section } => {
            if session.sections.iter().any(|s| s.id == section.id) {
                return Err(ApplyError::Invalid(format!("section id `{}` already exists", section.id)));
            }
            let idx = match after {
                Some(id) => section_index(session, id)? + 1,
                None => 0,
            };
            let start: u32 = session.sections[..idx.min(session.sections.len())]
                .iter()
                .map(|s| s.bars)
                .sum();
            if start < at_bar {
                return Err(ApplyError::InPast(at_bar));
            }
            session.sections.insert(idx, section.clone());
            Ok(ApplyReport::single(
                format!("add_section:{}@bar{start}", section.id),
                vec![],
            ))
        }
        IrDiff::ExtendSection { id, extra_bars } => {
            if *extra_bars == 0 {
                return Err(ApplyError::Invalid("extra_bars must be >= 1".into()));
            }
            let si = section_index(session, id)?;
            if section_in_past(session, si, at_bar) {
                return Err(ApplyError::InPast(at_bar));
            }
            let sec = &mut session.sections[si];
            let new_bars = sec
                .bars
                .checked_add(*extra_bars)
                .ok_or_else(|| ApplyError::Invalid("bars overflow".into()))?;
            let superseded = vec![format!("extend_section:{id} (was {} bars)", sec.bars)];
            sec.bars = new_bars;
            Ok(ApplyReport::single(format!("extend_section:{id}+{extra_bars}"), superseded))
        }
        IrDiff::SetInstrumentParam { track, param, value } => {
            let ti = track_index(session, track)?;
            if !value.is_finite() {
                return Err(ApplyError::Invalid(format!("param value {value} is not finite")));
            }
            let is_custom = matches!(session.tracks[ti].instrument, InstrumentDef::Custom(_));
            let inst = &mut session.tracks[ti].instrument;
            let prev = set_instrument_param(inst, param, *value).ok_or_else(|| {
                let patch_hint = if is_custom {
                    "; custom patches address nodes as patch.<node_id>.<field>"
                } else {
                    ""
                };
                ApplyError::Invalid(format!(
                    "param `{param}` not supported; use one of: {INSTRUMENT_PARAMS}{patch_hint}"
                ))
            })?;
            Ok(ApplyReport::single(
                format!("set_instrument_param:{track}.{param}={value}"),
                vec![format!("{track}.{param}={prev}")],
            ))
        }
        IrDiff::SwapSample { track, sample_id } => {
            let ti = track_index(session, track)?;
            let inst = &mut session.tracks[ti].instrument;
            let InstrumentDef::Sample(slot) = inst else {
                return Err(ApplyError::Invalid(format!(
                    "track `{track}` is not a sample slot"
                )));
            };
            let superseded = slot.id.map(|p| format!("{track}.sample={p}")).into_iter().collect();
            slot.id = Some(*sample_id);
            Ok(ApplyReport::single(format!("swap_sample:{track}={sample_id}"), superseded))
        }
        // Issue #37: instrument-as-data swap. The session's instrument changes
        // here; the audible swap lands at the next pattern boundary when the
        // engine re-attaches the track (crossfaded pool swap, see the core
        // `AudioGraph::swap_track`). The candidate session is validated first:
        // an over-ambitious or malformed patch is rejected with the
        // validation report, never clamped.
        IrDiff::SwapInstrument { track, instrument } => {
            let ti = track_index(session, track)?;
            let mut candidate = session.clone();
            candidate.tracks[ti].instrument = instrument.clone();
            if let Err(errors) = crate::validate::validate_session(&candidate) {
                let report = errors
                    .iter()
                    .take(3)
                    .map(|e| format!("[{} at {}: {}]", e.code, e.path, e.message))
                    .collect::<Vec<_>>()
                    .join(" ");
                return Err(ApplyError::Invalid(format!(
                    "swap_instrument rejected: {report}"
                )));
            }
            let old = std::mem::replace(&mut session.tracks[ti].instrument, instrument.clone());
            let superseded = vec![format!("{track}.instrument={}", instrument_kind(&old))];
            Ok(ApplyReport::single(
                format!("swap_instrument:{track}={}", instrument_kind(instrument)),
                superseded,
            ))
        }
        IrDiff::SetSectionEnergy { id, energy } => {
            let si = section_index(session, id)?;
            if energy.is_empty() || energy.iter().any(|v| !v.is_finite() || *v < 0.0 || *v > 1.0) {
                return Err(ApplyError::Invalid(
                    "energy must be a non-empty list of values in 0..=1".into(),
                ));
            }
            let sec = &mut session.sections[si];
            sec.energy_curve = energy.clone();
            Ok(ApplyReport::single(
                format!("set_section_energy:{id}"),
                vec![format!("set_section_energy:{id}")],
            ))
        }
        IrDiff::ScheduleTransition { at_bar: at, transition } => {
            if *at < at_bar {
                return Err(ApplyError::InPast(at_bar));
            }
            let total = session.total_bars() as u32;
            if *at >= total {
                return Err(ApplyError::Invalid(format!(
                    "transition bar {at} is beyond the session ({total} bars)"
                )));
            }
            let starts = section_starts(session);
            let si = starts
                .iter()
                .position(|b| *b == *at)
                .ok_or_else(|| {
                    ApplyError::Invalid(format!(
                        "transitions anchor at section boundaries; bar {at} is mid-section"
                    ))
                })?;
            session.sections[si].transition_in = Some(transition.clone());
            Ok(ApplyReport::single(
                format!("schedule_transition:{:?}@bar{at}", transition.kind),
                vec![],
            ))
        }
        IrDiff::SetTempo { bpm } => {
            let (lo, hi) = bounds::LIVE_BPM;
            if !bpm.is_finite() || !(lo..=hi).contains(bpm) {
                return Err(ApplyError::Invalid(format!("bpm {bpm} outside the live range {lo}..={hi}")));
            }
            let lane = &mut session.tempo_lane;
            match lane.last().copied() {
                None => {
                    if at_bar != 0 {
                        return Err(ApplyError::Invalid("tempo lane must anchor at bar 0".into()));
                    }
                    lane.push((at_bar, *bpm));
                }
                Some((bar, _)) if bar > at_bar => {
                    return Err(ApplyError::Invalid(format!(
                        "tempo move at bar {at_bar} precedes the last breakpoint at bar {bar}"
                    )));
                }
                Some((bar, prev)) if bar == at_bar => {
                    let i = lane.len() - 1;
                    lane[i].1 = *bpm;
                    return Ok(ApplyReport::single(
                        format!("set_tempo:bar{at_bar}={bpm}"),
                        vec![format!("set_tempo:bar{at_bar} (was {prev} bpm)")],
                    ));
                }
                Some(_) => lane.push((at_bar, *bpm)),
            }
            Ok(ApplyReport::single(format!("set_tempo:bar{at_bar}={bpm}"), vec![]))
        }
        IrDiff::SetKey { key } => {
            let hint = key.key_hint();
            let superseded = session.key.take().map(|prev| format!("key={prev}")).into_iter().collect();
            session.key = Some(hint.clone());
            Ok(ApplyReport::single(format!("set_key:{hint}"), superseded))
        }
    }
}

/// Applies a batch in order; last-writer-wins with an accumulated supersede
/// log. Stops at the first error (session state stays at the last good op).
pub fn apply_diffs(
    session: &mut Session,
    diffs: &[IrDiff],
    at_bar: u32,
) -> Result<ApplyReport, ApplyError> {
    let mut report = ApplyReport { applied: vec![], superseded: vec![] };
    for d in diffs {
        let r = apply_diff(session, d, at_bar)?;
        report.applied.extend(r.applied);
        report.superseded.extend(r.superseded);
    }
    Ok(report)
}

const INSTRUMENT_PARAMS: &str =
    "tune_hz, decay_ms, click, drive, tone, snap, cutoff_hz, resonance, env_amt, glide_ms, attack_ms, release_ms, detune_cents, depth, damping, bright, position, osc2_level, sub, ratio, index, feedback, density, grain_ms";

fn instrument_kind(inst: &InstrumentDef) -> &str {
    inst.kind_id().unwrap_or(match inst {
        InstrumentDef::Sample(_) => "sample",
        _ => "custom",
    })
}

fn set_instrument_param(inst: &mut InstrumentDef, param: &str, value: f32) -> Option<f32> {
    let set = |slot: &mut f32| std::mem::replace(slot, value);
    match (inst, param) {
        (InstrumentDef::Kick(k), "tune_hz") => Some(set(&mut k.tune_hz)),
        (InstrumentDef::Kick(k), "decay_ms") => Some(set(&mut k.decay_ms)),
        (InstrumentDef::Kick(k), "click") => Some(set(&mut k.click)),
        (InstrumentDef::Kick(k), "drive") => Some(set(&mut k.drive)),
        (InstrumentDef::Hat(h), "decay_ms") => Some(set(&mut h.decay_ms)),
        (InstrumentDef::Hat(h), "tone") => Some(set(&mut h.tone)),
        (InstrumentDef::Bass(b), "cutoff_hz") => Some(set(&mut b.cutoff_hz)),
        (InstrumentDef::Bass(b), "resonance") => Some(set(&mut b.resonance)),
        (InstrumentDef::Bass(b), "glide_ms") => Some(set(&mut b.glide_ms)),
        (InstrumentDef::Pad(p), "attack_ms") => Some(set(&mut p.attack_ms)),
        (InstrumentDef::Pad(p), "release_ms") => Some(set(&mut p.release_ms)),
        (InstrumentDef::Pad(p), "detune_cents") => Some(set(&mut p.detune_cents)),
        (InstrumentDef::Pad(p), "cutoff_hz") => Some(set(&mut p.cutoff_hz)),
        (InstrumentDef::Clap(c), "decay_ms") => Some(set(&mut c.decay_ms)),
        (InstrumentDef::Clap(c), "tone") => Some(set(&mut c.tone)),
        (InstrumentDef::Snare(sn), "tune_hz") => Some(set(&mut sn.tune_hz)),
        (InstrumentDef::Snare(sn), "decay_ms") => Some(set(&mut sn.decay_ms)),
        (InstrumentDef::Snare(sn), "snap") => Some(set(&mut sn.snap)),
        (InstrumentDef::Shaker(sh), "decay_ms") => Some(set(&mut sh.decay_ms)),
        (InstrumentDef::Shaker(sh), "tone") => Some(set(&mut sh.tone)),
        (InstrumentDef::Acid(a), "cutoff_hz") => Some(set(&mut a.cutoff_hz)),
        (InstrumentDef::Acid(a), "resonance") => Some(set(&mut a.resonance)),
        (InstrumentDef::Acid(a), "env_amt") => Some(set(&mut a.env_amt)),
        (InstrumentDef::Acid(a), "glide_ms") => Some(set(&mut a.glide_ms)),
        (InstrumentDef::Ep(e), "decay_ms") => Some(set(&mut e.decay_ms)),
        (InstrumentDef::Ep(e), "depth") => Some(set(&mut e.depth)),
        (InstrumentDef::Pluck(pl), "damping") => Some(set(&mut pl.damping)),
        (InstrumentDef::Pluck(pl), "bright") => Some(set(&mut pl.bright)),
        (InstrumentDef::Stab(st), "cutoff_hz") => Some(set(&mut st.cutoff_hz)),
        (InstrumentDef::Stab(st), "decay_ms") => Some(set(&mut st.decay_ms)),
        (InstrumentDef::Stab(st), "detune_cents") => Some(set(&mut st.detune_cents)),
        (InstrumentDef::Wavetable(w), "position") => Some(set(&mut w.position)),
        (InstrumentDef::Wavetable(w), "detune_cents") => Some(set(&mut w.detune_cents)),
        (InstrumentDef::Wavetable(w), "osc2_level") => Some(set(&mut w.osc2_level)),
        (InstrumentDef::Wavetable(w), "sub") => Some(set(&mut w.sub)),
        (InstrumentDef::Wavetable(w), "cutoff_hz") => Some(set(&mut w.cutoff_hz)),
        (InstrumentDef::Wavetable(w), "release_ms") => Some(set(&mut w.release_ms)),
        (InstrumentDef::FmPerc(f), "ratio") => Some(set(&mut f.ratio)),
        (InstrumentDef::FmPerc(f), "index") => Some(set(&mut f.index)),
        (InstrumentDef::FmPerc(f), "feedback") => Some(set(&mut f.feedback)),
        (InstrumentDef::FmPerc(f), "decay_ms") => Some(set(&mut f.decay_ms)),
        (InstrumentDef::Texture(t), "density") => Some(set(&mut t.density)),
        (InstrumentDef::Texture(t), "grain_ms") => Some(set(&mut t.grain_ms)),
        (InstrumentDef::Texture(t), "tone") => Some(set(&mut t.tone)),
        // Patch addressing (issue #37): `patch.<node_id>.<field>` targets one
        // float param of one node in a custom instrument's graph.
        (InstrumentDef::Custom(c), p) => {
            let rest = p.strip_prefix("patch.")?;
            let (node_id, field) = rest.split_once('.')?;
            let node = c.patch.nodes.iter_mut().find(|n| n.id() == node_id)?;
            set_patch_node_param(node, field, value)
        }
        _ => None,
    }
}

fn set_patch_node_param(node: &mut crate::patch::PatchNode, field: &str, value: f32) -> Option<f32> {
    let set = |slot: &mut f32| std::mem::replace(slot, value);
    match node {
        crate::patch::PatchNode::Osc(o) => match field {
            "fine_cents" => Some(set(&mut o.fine_cents)),
            "level" => Some(set(&mut o.level)),
            _ => None,
        },
        crate::patch::PatchNode::FmPair(n) => match field {
            "ratio" => Some(set(&mut n.ratio)),
            "index" => Some(set(&mut n.index)),
            "feedback" => Some(set(&mut n.feedback)),
            "level" => Some(set(&mut n.level)),
            _ => None,
        },
        crate::patch::PatchNode::Filter(n) => match field {
            "cutoff_hz" => Some(set(&mut n.cutoff_hz)),
            "resonance" => Some(set(&mut n.resonance)),
            "drive" => Some(set(&mut n.drive)),
            _ => None,
        },
        crate::patch::PatchNode::Env(n) => match field {
            "attack_ms" => Some(set(&mut n.attack_ms)),
            "decay_ms" => Some(set(&mut n.decay_ms)),
            "sustain" => Some(set(&mut n.sustain)),
            "release_ms" => Some(set(&mut n.release_ms)),
            _ => None,
        },
        crate::patch::PatchNode::Lfo(n) => match field {
            "rate_hz" => Some(set(&mut n.rate_hz)),
            "depth" => Some(set(&mut n.depth)),
            _ => None,
        },
        crate::patch::PatchNode::Gain(n) => match field {
            "level" => Some(set(&mut n.level)),
            _ => None,
        },
        crate::patch::PatchNode::Delay(n) => match field {
            "time_ms" => Some(set(&mut n.time_ms)),
            "feedback" => Some(set(&mut n.feedback)),
            "mix" => Some(set(&mut n.mix)),
            _ => None,
        },
        crate::patch::PatchNode::Ring(n) => match field {
            "level" => Some(set(&mut n.level)),
            _ => None,
        },
        crate::patch::PatchNode::Shaper(n) => match field {
            "drive" => Some(set(&mut n.drive)),
            "level" => Some(set(&mut n.level)),
            _ => None,
        },
        crate::patch::PatchNode::Formant(n) => match field {
            "shift" => Some(set(&mut n.shift)),
            "level" => Some(set(&mut n.level)),
            _ => None,
        },
        crate::patch::PatchNode::Sampler(n) => match field {
            "level" => Some(set(&mut n.level)),
            _ => None,
        },
        crate::patch::PatchNode::Out(n) => match field {
            "level" => Some(set(&mut n.level)),
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::EuclideanPattern;

    fn fixture() -> Session {
        serde_json::from_str(
            r#"{
            "version": 1, "seed": 1,
            "tempo_lane": [[0, 124.0]],
            "sections": [
                {"id": "a", "bars": 4, "energy_curve": [0.5],
                 "pattern_bindings": {"k": {"generator": "euclidean", "k": 4, "n": 16}}},
                {"id": "b", "bars": 4, "energy_curve": [0.8],
                 "pattern_bindings": {"k": {"generator": "euclidean", "k": 8, "n": 16}}}
            ],
            "tracks": [{"id": "k", "role": "kick", "instrument": {"kind": "kick"}}]
        }"#,
        )
        .expect("fixture")
    }

    fn pattern(k: u32) -> Pattern {
        Pattern::Euclidean(EuclideanPattern {
            generator: crate::schema::EuclideanTag::Euclidean,
            k,
            n: 16,
            rot: 0,
            velocity: 0.8,
            probability: 1.0,
            repeats: 1,
            gate: None,
            pitch: None,
        })
    }

    #[test]
    fn diff_ops_roundtrip_through_json() {
        let json = r#"{"op":"replace_pattern","section":"b","track":"k",
            "pattern":{"generator":"euclidean","k":2,"n":16,"rot":0}}"#;
        let d: IrDiff = serde_json::from_str(json).expect("parse");
        let out = serde_json::to_string(&d).expect("serialize");
        assert!(out.contains(r#""op":"replace_pattern""#));
        let d2: IrDiff = serde_json::from_str(&out).expect("reparse");
        assert_eq!(d, d2);
    }

    #[test]
    fn replace_pattern_future_inflight_and_past() {
        let mut s = fixture();
        let d = IrDiff::ReplacePattern {
            section: "b".into(),
            track: "k".into(),
            pattern: pattern(2),
        };
        let r = apply_diff(&mut s, &d, 4).expect("section b starts at bar 4");
        assert_eq!(r.applied, vec!["replace_pattern:k@b".to_string()]);
        assert_eq!(
            r.superseded,
            vec!["replace_pattern:k@b".to_string()],
            "displaces the section's existing binding"
        );
        apply_diff(&mut s, &d, 5).expect("in-flight sections are editable (live one-shots, #38)");
        let mut s = fixture();
        assert_eq!(
            apply_diff(&mut s, &d, 8),
            Err(ApplyError::InPast(8)),
            "section b has fully ended once the playhead reaches bar 8"
        );
    }

    #[test]
    fn replace_pattern_supersedes() {
        let mut s = fixture();
        let d = IrDiff::ReplacePattern {
            section: "b".into(),
            track: "k".into(),
            pattern: pattern(2),
        };
        apply_diff(&mut s, &d, 0).expect("first write");
        let r = apply_diff(&mut s, &d, 0).expect("second write");
        assert_eq!(r.superseded, vec!["replace_pattern:k@b".to_string()]);
    }

    #[test]
    fn unknown_section_and_track() {
        let mut s = fixture();
        let d = IrDiff::ReplacePattern { section: "z".into(), track: "k".into(), pattern: pattern(1) };
        assert_eq!(apply_diff(&mut s, &d, 0), Err(ApplyError::UnknownSection("z".into())));
        let d = IrDiff::ReplacePattern { section: "a".into(), track: "x".into(), pattern: pattern(1) };
        assert_eq!(apply_diff(&mut s, &d, 8), Err(ApplyError::UnknownTrack("x".into())));
    }

    #[test]
    fn add_section_rejects_past_and_duplicate_ids() {
        let mut s = fixture();
        let sec: Section =
            serde_json::from_str(r#"{"id": "c", "bars": 4, "energy_curve": [0.6]}"#).expect("section");
        let after_b = IrDiff::AddSection { after: Some("b".into()), section: sec.clone() };
        apply_diff(&mut s, &after_b, 0).expect("insert at end is future");
        assert_eq!(s.total_bars(), 12);
        let other: Section =
            serde_json::from_str(r#"{"id": "d", "bars": 4, "energy_curve": [0.6]}"#).expect("section");
        let at_start = IrDiff::AddSection { after: None, section: other };
        assert_eq!(apply_diff(&mut s, &at_start, 1), Err(ApplyError::InPast(1)));
        let dup = IrDiff::AddSection { after: Some("b".into()), section: sec };
        assert!(matches!(apply_diff(&mut s, &dup, 0), Err(ApplyError::Invalid(_))));
    }

    #[test]
    fn extend_section_future_inflight_and_past() {
        let mut s = fixture();
        let d = IrDiff::ExtendSection { id: "b".into(), extra_bars: 4 };
        apply_diff(&mut s, &d, 4).expect("b starts exactly at the playhead");
        assert_eq!(s.sections[1].bars, 8);
        let mut s = fixture();
        apply_diff(&mut s, &d, 5).expect("in-flight sections extend (live loop, #38)");
        assert_eq!(s.sections[1].bars, 8, "b (bars 4..8) extended while playing");
        assert_eq!(s.total_bars(), 12);
        assert_eq!(
            apply_diff(&mut s, &d, 12),
            Err(ApplyError::InPast(12)),
            "b has fully ended once the playhead reaches its (extended) end"
        );
    }

    #[test]
    fn set_instrument_param_writes_and_supersedes() {
        let mut s = fixture();
        let d = IrDiff::SetInstrumentParam { track: "k".into(), param: "tune_hz".into(), value: 55.0 };
        let r = apply_diff(&mut s, &d, 0).expect("apply");
        assert_eq!(r.superseded, vec!["k.tune_hz=48".to_string()]);
        let d2 = IrDiff::SetInstrumentParam { track: "k".into(), param: "tune_hz".into(), value: 60.0 };
        let r2 = apply_diff(&mut s, &d2, 0).expect("apply");
        assert_eq!(r2.superseded, vec!["k.tune_hz=55".to_string()]);
        let bad = IrDiff::SetInstrumentParam { track: "k".into(), param: "wood".into(), value: 1.0 };
        assert!(matches!(apply_diff(&mut s, &bad, 0), Err(ApplyError::Invalid(_))));
    }

    #[test]
    fn set_instrument_param_covers_every_builtin_voice() {
        fn one_track(track_json: &str) -> Session {
            serde_json::from_str(&format!(
                r#"{{
                "version": 1, "seed": 1,
                "tempo_lane": [[0, 124.0]],
                "sections": [{{"id": "a", "bars": 4, "energy_curve": [0.5]}}],
                "tracks": [{track_json}]
            }}"#
            ))
            .expect("session")
        }

        // Every float param of every built-in voice, incl. the clap/snare
        // params the ManualDrumEditor surfaces. Two writes per param: the
        // second's supersede log echoes the field's previous value, proving
        // the first landed on the real field.
        let cases: &[(&str, &str, &[(&str, f32)])] = &[
            (
                r#"{"id": "c", "role": "perc", "instrument": {"kind": "clap"}}"#,
                "c",
                &[("decay_ms", 400.0), ("tone", 0.6)],
            ),
            (
                r#"{"id": "sn", "role": "perc", "instrument": {"kind": "snare"}}"#,
                "sn",
                &[("tune_hz", 200.0), ("decay_ms", 250.0), ("snap", 0.7)],
            ),
            (
                r#"{"id": "sh", "role": "perc", "instrument": {"kind": "shaker"}}"#,
                "sh",
                &[("decay_ms", 100.0), ("tone", 0.6)],
            ),
            (
                r#"{"id": "ac", "role": "bass", "instrument": {"kind": "acid"}}"#,
                "ac",
                &[("cutoff_hz", 900.0), ("resonance", 0.6), ("env_amt", 3.0), ("glide_ms", 60.0)],
            ),
            (
                r#"{"id": "ep", "role": "pad", "instrument": {"kind": "ep"}}"#,
                "ep",
                &[("decay_ms", 1500.0), ("depth", 2.0)],
            ),
            (
                r#"{"id": "pl", "role": "fx", "instrument": {"kind": "pluck"}}"#,
                "pl",
                &[("damping", 0.6), ("bright", 0.5)],
            ),
            (
                r#"{"id": "st", "role": "pad", "instrument": {"kind": "stab"}}"#,
                "st",
                &[("cutoff_hz", 2400.0), ("decay_ms", 400.0), ("detune_cents", 12.0)],
            ),
        ];
        for (track_json, track_id, params) in cases {
            let mut s = one_track(track_json);
            for (param, v1) in *params {
                let op = |v: f32| IrDiff::SetInstrumentParam {
                    track: (*track_id).into(),
                    param: (*param).into(),
                    value: v,
                };
                apply_diff(&mut s, &op(*v1), 0)
                    .unwrap_or_else(|e| panic!("{track_id}.{param}={v1}: {e}"));
                let r = apply_diff(&mut s, &op(v1 + 7.0), 0)
                    .unwrap_or_else(|e| panic!("{track_id}.{param} rewrite: {e}"));
                assert_eq!(
                    r.superseded,
                    vec![format!("{track_id}.{param}={v1}")],
                    "rewrite supersedes the value the first write put in the field"
                );
            }
        }

        for (track_json, track_id, param) in [
            (r#"{"id": "h", "role": "perc", "instrument": {"kind": "hat"}}"#, "h", "open"),
            (r#"{"id": "b", "role": "bass", "instrument": {"kind": "bass"}}"#, "b", "wave"),
        ] {
            let mut s = one_track(track_json);
            let d =
                IrDiff::SetInstrumentParam { track: track_id.into(), param: param.into(), value: 1.0 };
            assert!(
                matches!(apply_diff(&mut s, &d, 0), Err(ApplyError::Invalid(_))),
                "`{param}` must not be settable via param diff"
            );
        }
    }

    #[test]
    fn swap_sample_needs_sample_slot_and_supersedes() {
        let mut s = fixture();
        let not_sample = IrDiff::SwapSample { track: "k".into(), sample_id: 9 };
        assert!(matches!(apply_diff(&mut s, &not_sample, 0), Err(ApplyError::Invalid(_))));

        let mut s2: Session = serde_json::from_str(
            r#"{
            "version": 1, "seed": 1, "tempo_lane": [[0, 124.0]],
            "sections": [{"id": "a", "bars": 4, "energy_curve": [0.5]}],
            "tracks": [{"id": "s", "role": "perc",
                        "instrument": {"kind": "sample", "id": 3}}]
        }"#,
        )
        .expect("session");
        let d: IrDiff = serde_json::from_str(r#"{"op":"swap_sample","track":"s","sample_id":9}"#)
            .expect("parse");
        let r = apply_diff(&mut s2, &d, 0).expect("apply");
        assert_eq!(r.superseded, vec!["s.sample=3".to_string()]);
        match &s2.tracks[0].instrument {
            InstrumentDef::Sample(slot) => assert_eq!(slot.id, Some(9)),
            other => panic!("expected sample slot, got {other:?}"),
        }
    }

    #[test]
    fn set_section_energy_is_future_anchored_exempt() {
        let mut s = fixture();
        let d = IrDiff::SetSectionEnergy { id: "a".into(), energy: vec![0.9, 0.95] };
        let r = apply_diff(&mut s, &d, 100).expect("exempt from InPast");
        assert_eq!(r.applied, vec!["set_section_energy:a".to_string()]);
        assert_eq!(s.sections[0].energy_curve, vec![0.9, 0.95]);
        let bad = IrDiff::SetSectionEnergy { id: "a".into(), energy: vec![2.0] };
        assert!(matches!(apply_diff(&mut s, &bad, 0), Err(ApplyError::Invalid(_))));
    }

    #[test]
    fn schedule_transition_anchors_and_rejects_past() {
        let mut s = fixture();
        let t: Transition = serde_json::from_str(r#"{"type": "riser", "bars": 1}"#).expect("t");
        let d = IrDiff::ScheduleTransition { at_bar: 4, transition: t.clone() };
        let r = apply_diff(&mut s, &d, 4).expect("bar 4 is now/future");
        assert_eq!(r.applied, vec!["schedule_transition:Riser@bar4".to_string()]);
        assert!(s.sections[1].transition_in.is_some());
        assert_eq!(apply_diff(&mut s, &d, 5), Err(ApplyError::InPast(5)));
        let mid = IrDiff::ScheduleTransition { at_bar: 5, transition: t };
        assert!(matches!(apply_diff(&mut s, &mid, 0), Err(ApplyError::Invalid(_))));
    }

    #[test]
    fn set_tempo_lands_at_boundary_and_supersedes() {
        let mut s = fixture();
        let d = IrDiff::SetTempo { bpm: 130.0 };
        let r = apply_diff(&mut s, &d, 4).expect("lands at the playhead bar");
        assert_eq!(r.applied, vec!["set_tempo:bar4=130".to_string()]);
        assert_eq!(s.tempo_lane, vec![(0, 124.0), (4, 130.0)]);
        let r2 = apply_diff(&mut s, &d, 4).expect("same-boundary rewrite is last-writer-wins");
        assert_eq!(r2.applied, vec!["set_tempo:bar4=130".to_string()]);
        assert_eq!(r2.superseded, vec!["set_tempo:bar4 (was 130 bpm)".to_string()]);
        assert_eq!(s.tempo_lane, vec![(0, 124.0), (4, 130.0)], "one breakpoint, replaced in place");
    }

    #[test]
    fn set_tempo_rejects_out_of_range_and_mid_lane() {
        let mut s = fixture();
        for bpm in [59.0, 200.5, 0.0, f64::NAN, f64::INFINITY] {
            let d = IrDiff::SetTempo { bpm };
            assert!(
                matches!(apply_diff(&mut s, &d, 4), Err(ApplyError::Invalid(_))),
                "bpm {bpm} must be rejected"
            );
        }
        assert_eq!(s.tempo_lane.len(), 1, "rejected moves leave the lane untouched");
        apply_diff(&mut s, &IrDiff::SetTempo { bpm: 132.0 }, 8).expect("future breakpoint");
        let mid_lane = IrDiff::SetTempo { bpm: 128.0 };
        assert!(matches!(apply_diff(&mut s, &mid_lane, 4), Err(ApplyError::Invalid(_))));
        assert!(matches!(apply_diff(&mut s, &mid_lane, 0), Err(ApplyError::Invalid(_))));
    }

    #[test]
    fn set_key_updates_hint_and_roundtrips() {
        let mut s = fixture();
        let d = IrDiff::SetKey { key: crate::schema::MusicalKey::FMinor };
        let r = apply_diff(&mut s, &d, 100).expect("session-level: no bar target");
        assert_eq!(r.applied, vec!["set_key:F minor".to_string()]);
        assert_eq!(s.key.as_deref(), Some("F minor"));
        let d2 = IrDiff::SetKey { key: crate::schema::MusicalKey::CSharpMajor };
        let r2 = apply_diff(&mut s, &d2, 0).expect("apply");
        assert_eq!(r2.superseded, vec!["key=F minor".to_string()]);
        assert_eq!(s.key.as_deref(), Some("C sharp major"));

        let json = r#"{"op":"set_key","key":"a_minor"}"#;
        let parsed: IrDiff = serde_json::from_str(json).expect("snake_case tag parses");
        assert_eq!(parsed, IrDiff::SetKey { key: crate::schema::MusicalKey::AMinor });
        let out = serde_json::to_string(&parsed).expect("serialize");
        assert!(out.contains(r#""op":"set_key""#));
        let bad: Result<IrDiff, _> = serde_json::from_str(r#"{"op":"set_key","key":"h_minor"}"#);
        assert!(bad.is_err(), "closed enum rejects unknown keys at parse time");
    }

    #[test]
    fn apply_diffs_accumulates_supersede_log() {
        let mut s = fixture();
        let ops = vec![
            IrDiff::ReplacePattern { section: "b".into(), track: "k".into(), pattern: pattern(2) },
            IrDiff::ReplacePattern { section: "b".into(), track: "k".into(), pattern: pattern(3) },
        ];
        let r = apply_diffs(&mut s, &ops, 0).expect("batch");
        assert_eq!(r.applied.len(), 2);
        assert_eq!(
            r.superseded.len(),
            2,
            "both writes displace prior material (fixture binding, then op 1)"
        );
    }

    #[test]
    fn swap_instrument_replaces_and_supersedes() {
        let mut s: Session = serde_json::from_str(
            r#"{
            "version": 1, "seed": 1, "tempo_lane": [[0, 124.0]],
            "sections": [{"id": "a", "bars": 4, "energy_curve": [0.5],
                "pattern_bindings": {"k": {"generator": "euclidean", "k": 4, "n": 16}}}],
            "tracks": [{"id": "k", "role": "kick", "instrument": {"kind": "kick"}}]
        }"#,
        )
        .expect("fixture");
        let new_inst: InstrumentDef =
            serde_json::from_str(r#"{"kind": "hat", "decay_ms": 90.0}"#).expect("hat");
        let d = IrDiff::SwapInstrument { track: "k".into(), instrument: new_inst };
        let r = apply_diff(&mut s, &d, 0).expect("apply");
        assert_eq!(r.applied, vec!["swap_instrument:k=hat".to_string()]);
        assert_eq!(r.superseded, vec!["k.instrument=kick".to_string()]);
        assert!(matches!(s.tracks[0].instrument, InstrumentDef::Hat(_)));

        // JSON round trip: the op is composer-writable as data.
        let parsed: IrDiff = serde_json::from_str(
            r#"{"op":"swap_instrument","track":"k","instrument":{"kind":"kick"}}"#,
        )
        .expect("parse");
        assert_eq!(apply_diff(&mut s, &parsed, 0).expect("apply").superseded,
            vec!["k.instrument=hat".to_string()]);
    }

    #[test]
    fn swap_instrument_rejects_invalid_patches_with_report() {
        let mut s: Session = serde_json::from_str(
            r#"{
            "version": 1, "seed": 1, "tempo_lane": [[0, 124.0]],
            "sections": [{"id": "a", "bars": 4, "energy_curve": [0.5],
                "pattern_bindings": {"p": {"generator": "euclidean", "k": 4, "n": 16}}}],
            "tracks": [{"id": "p", "role": "perc", "instrument": {"kind": "kick"}}]
        }"#,
        )
        .expect("fixture");
        // No `out` node: structurally broken patch.
        let broken: InstrumentDef = serde_json::from_str(
            r#"{"kind": "custom", "patch": {
                "nodes": [{"id": "o1", "type": "osc"}], "edges": []}}"#,
        )
        .expect("parse");
        let d = IrDiff::SwapInstrument { track: "p".into(), instrument: broken };
        let err = apply_diff(&mut s, &d, 0).expect_err("broken patch must be rejected");
        let ApplyError::Invalid(msg) = err else { panic!("Invalid expected, got {err:?}") };
        assert!(msg.contains("E_PATCH_NO_OUT"), "{msg}");
        // Rejection leaves the session untouched.
        assert!(matches!(s.tracks[0].instrument, InstrumentDef::Kick(_)));

        // A 25-node patch busts the node ceiling and is rejected the same way.
        let nodes: Vec<String> =
            (0..=24).map(|i| format!(r#"{{"id": "g{i}", "type": "gain"}}"#)).collect();
        let fat: InstrumentDef = serde_json::from_str(&format!(
            r#"{{"kind": "custom", "patch": {{"nodes": [{}], "edges": []}}}}"#,
            nodes.join(",")
        ))
        .expect("parse");
        let d = IrDiff::SwapInstrument { track: "p".into(), instrument: fat };
        let err = apply_diff(&mut s, &d, 0).expect_err("over-cap patch must be rejected");
        let ApplyError::Invalid(msg) = err else { panic!("Invalid expected, got {err:?}") };
        assert!(msg.contains("E_PATCH_TOO_MANY_NODES"), "{msg}");
    }

    #[test]
    fn swap_instrument_accepts_a_valid_patch() {
        let mut s: Session = serde_json::from_str(
            r#"{
            "version": 1, "seed": 1, "tempo_lane": [[0, 124.0]],
            "sections": [{"id": "a", "bars": 4, "energy_curve": [0.5],
                "pattern_bindings": {"p": {"generator": "euclidean", "k": 4, "n": 16}}}],
            "tracks": [{"id": "p", "role": "perc", "instrument": {"kind": "kick"}}]
        }"#,
        )
        .expect("fixture");
        let hoover: InstrumentDef = serde_json::from_str(
            r#"{"kind": "custom", "patch": {
                "nodes": [
                    {"id": "saws", "type": "osc", "wave": "saw", "unison": 7, "fine_cents": 35.0, "level": 0.7},
                    {"id": "lp", "type": "filter", "mode": "low_pass", "cutoff_hz": 500.0, "resonance": 0.55, "drive": 0.15},
                    {"id": "vca", "type": "gain", "level": 0.0},
                    {"id": "amp", "type": "env", "attack_ms": 180.0, "decay_ms": 900.0, "sustain": 0.7, "release_ms": 500.0},
                    {"id": "out", "type": "out", "level": 0.9}
                ],
                "edges": [
                    {"from": "saws", "to": "lp", "type": "audio"},
                    {"from": "lp", "to": "vca", "type": "audio"},
                    {"from": "vca", "to": "out", "type": "audio"},
                    {"from": "amp", "to": "vca", "type": "mod", "param": "level", "amount": 1.0}
                ]}}"#,
        )
        .expect("parse");
        let d = IrDiff::SwapInstrument { track: "p".into(), instrument: hoover };
        let r = apply_diff(&mut s, &d, 0).expect("valid patch swaps");
        assert_eq!(r.applied, vec!["swap_instrument:p=custom".to_string()]);
        assert_eq!(r.superseded, vec!["p.instrument=kick".to_string()]);
    }

    #[test]
    fn set_instrument_param_addresses_patch_nodes_by_dotted_path() {
        let mut s: Session = serde_json::from_str(
            r#"{
            "version": 1, "seed": 1, "tempo_lane": [[0, 124.0]],
            "sections": [{"id": "a", "bars": 4, "energy_curve": [0.5]}],
            "tracks": [{"id": "p", "role": "perc", "instrument": {"kind": "custom", "patch": {
                "nodes": [
                    {"id": "o1", "type": "osc", "level": 0.9},
                    {"id": "f1", "type": "filter", "cutoff_hz": 3000.0},
                    {"id": "x", "type": "out", "level": 0.8}
                ],
                "edges": [
                    {"from": "o1", "to": "f1", "type": "audio"},
                    {"from": "f1", "to": "x", "type": "audio"}
                ]
            }}}]
        }"#,
        )
        .expect("fixture");
        let d = IrDiff::SetInstrumentParam {
            track: "p".into(),
            param: "patch.f1.cutoff_hz".into(),
            value: 420.0,
        };
        let r = apply_diff(&mut s, &d, 0).expect("apply");
        assert_eq!(r.superseded, vec!["p.patch.f1.cutoff_hz=3000".to_string()]);
        let InstrumentDef::Custom(c) = &s.tracks[0].instrument else {
            panic!("expected custom instrument");
        };
        let f1 = c
            .patch
            .nodes
            .iter()
            .find(|n| n.id() == "f1")
            .expect("node survives diff");
        match f1 {
            crate::patch::PatchNode::Filter(f) => assert_eq!(f.cutoff_hz, 420.0),
            other => panic!("expected filter node, got {other:?}"),
        }
        // Second write supersedes the first (last-writer-wins log).
        let r2 = apply_diff(
            &mut s,
            &IrDiff::SetInstrumentParam {
                track: "p".into(),
                param: "patch.f1.cutoff_hz".into(),
                value: 800.0,
            },
            0,
        )
        .expect("apply");
        assert_eq!(r2.superseded, vec!["p.patch.f1.cutoff_hz=420".to_string()]);
        // Unknown node, unknown field, and non-dotted forms are rejected and
        // leave the patch untouched.
        for bad in ["patch.ghost.cutoff_hz", "patch.f1.wave", "cutoff_hz", "patch.f1"] {
            let bad_op = IrDiff::SetInstrumentParam {
                track: "p".into(),
                param: bad.into(),
                value: 1.0,
            };
            assert!(
                matches!(apply_diff(&mut s, &bad_op, 0), Err(ApplyError::Invalid(_))),
                "{bad} must be rejected"
            );
        }
    }
}
