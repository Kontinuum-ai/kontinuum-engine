//! Mid-session world switching (issue #30: "world switching mid-session =
//! section-boundary palette morph").
//!
//! Mechanism, on the existing IR machinery only:
//!
//! 1. **Palette (synth) parameters** — [`crate::world::apply_to_tracks`]
//!    re-voices `session.tracks` for world B. The rig is session-global in
//!    IR v1, so the re-voicing lands exactly at the start bar of
//!    `at_section` (the caller swaps the session into the engine there);
//!    there is no mid-bar parameter jump by construction.
//! 2. **Mix targets** — the section-boundary crossfade. The automation
//!    slot of the boundary section (`Section.automation`, one lane per
//!    track) is rewritten with a two-point `Smooth` lane that slews the
//!    track's most audible changed mix target from world A's value
//!    (lane start = the pre-morph value, which is what every earlier
//!    section sounds at) to world B's value (lane end = the new static
//!    value). The result is a parameter curve that is constant at A, ramps
//!    across the boundary section, and is constant at B — continuous by
//!    construction and slew-bounded (the IR's ramp engine enforces its
//!    per-bar ceiling; worlds move gains by ≲0.5 over ≥4 bars, orders of
//!    magnitude under the 24 dB/bar limit).
//!
//! The IR allows one automation lane per (section, track), so the
//! crossfade claims the slot for the most audible changed target
//! (gain → send_reverb → send_delay → pan); any remaining changed mix
//! targets step exactly at the boundary, masked by the section transition.
//! The morph owning the slot follows the same precedence convention as the
//! generated pad-reverb gesture (see `presence::apply_presence`).

use kontinuum_ir::schema::{AutomationLane, CurveKind, Section, Session};

use super::{MixTargetOverride, MixValues, SoundWorld};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MorphError {
    /// `at_section` is past the end of the session.
    Section { index: usize, sections: usize },
}

impl std::fmt::Display for MorphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MorphError::Section { index, sections } => {
                write!(f, "morph section {index} out of range ({sections} sections)")
            }
        }
    }
}

impl std::error::Error for MorphError {}

/// Morphs `session` from its current world (A) to `to` (B), landing at the
/// start of `sections[at_section]`.
pub fn morph_world(session: &mut Session, to: &SoundWorld, at_section: usize) -> Result<(), MorphError> {
    if at_section >= session.sections.len() {
        return Err(MorphError::Section { index: at_section, sections: session.sections.len() });
    }
    // World A's audible mix values, read before any override lands.
    let from: Vec<(String, MixValues)> = session
        .tracks
        .iter()
        .filter(|t| to.mix_target_overrides.contains_key(&t.id))
        .map(|t| (t.id.clone(), MixValues::of(t)))
        .collect();
    super::apply_to_tracks(&mut session.tracks, to);
    for (id, from_values) in from {
        let Some(target) = to.mix_target_overrides.get(&id) else {
            continue;
        };
        let Some(sec) = session.sections.get_mut(at_section) else {
            continue;
        };
        // If the section already automates a target, the audible "from" is
        // wherever that lane currently ends — the crossfade continues the
        // existing curve instead of snapping back to the static value.
        let from_values = with_lane_endpoint(sec, &id, from_values);
        write_crossfade(sec, &id, from_values, *target);
    }
    session.palette = Some(serde_json::json!({ "world": to.id.0.clone() }));
    Ok(())
}

pub(crate) fn with_lane_endpoint(sec: &Section, track_id: &str, mut from: MixValues) -> MixValues {
    let Some(lane) = sec.automation.get(track_id) else {
        return from;
    };
    let Some(v) = lane.points.last().map(|(_, v, _)| *v) else {
        return from;
    };
    match lane.target_param.as_str() {
        "gain" => from.gain = v,
        "send_reverb" => from.send_reverb = v,
        "send_delay" => from.send_delay = v,
        "pan" => from.pan = v,
        _ => {}
    }
    from
}

/// Which mix target the crossfade lane drives: the most audible changed
/// one, in gain → reverb → delay → pan order.
pub(crate) fn crossfade_target(from: MixValues, to: MixTargetOverride) -> Option<(&'static str, f32, f32)> {
    let pick = |from_v: f32, to_v: Option<f32>| to_v.filter(|b| (b - from_v).abs() > f32::EPSILON);
    pick(from.gain, to.gain)
        .map(|b| ("gain", from.gain, b))
        .or_else(|| pick(from.send_reverb, to.send_reverb).map(|b| ("send_reverb", from.send_reverb, b)))
        .or_else(|| pick(from.send_delay, to.send_delay).map(|b| ("send_delay", from.send_delay, b)))
        .or_else(|| pick(from.pan, to.pan).map(|b| ("pan", from.pan, b)))
}

pub(crate) fn write_crossfade(sec: &mut Section, track_id: &str, from: MixValues, to: MixTargetOverride) {
    let Some((param, start, end)) = crossfade_target(from, to) else {
        return;
    };
    let points = if sec.bars >= 2 {
        vec![(0, start, CurveKind::Smooth), (sec.bars - 1, end, CurveKind::Smooth)]
    } else {
        vec![(0, end, CurveKind::Linear)]
    };
    sec.automation.insert(
        track_id.to_string(),
        AutomationLane { target_param: param.into(), points },
    );
}
