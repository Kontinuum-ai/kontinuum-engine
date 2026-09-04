//! Track presence automation (issue #52 workstream 1, feeding #16): the
//! arrangement must breathe. Presence is a per-track gain walk across the
//! session — elements enter and exit on multi-bar arcs independent of
//! pattern density, sections exist where most tracks sit near-silent, and
//! stripped (near-solo) passages drop everything but the groove.
//!
//! The walk is continuous by construction: each section's arc starts where
//! the previous one ended, so no boundary jumps, and every in-section move
//! stays far under the 24 dB/bar slew ceiling (worst case: 1.0 → 0.15 over
//! 8 bars ≈ 2 dB/bar).

use kontinuum_clock::stream;
use kontinuum_ir::schema::{AutomationLane, CurveKind, Section, Track};

/// RNG stream selectors for the presence walk.
const LANE_PRESENCE: u8 = 0xFE;
const PURPOSE_PRESENCE: u16 = 0xA2;

/// Entry level per track id, as a multiplier of the track's configured gain.
/// Kick carries the floor throughout; harmony enters quietest.
///
/// Looked up by id against the rig that was actually built, because the rig is
/// genre-dependent — styles without an open hat do not carry that track, and
/// writing an automation lane for a track the session does not have is a hard
/// validation failure, not a silent no-op.
fn entry_level(id: &str) -> f32 {
    match id {
        "kick" => 0.85,
        "clap" => 0.7,
        "perc" => 0.55,
        "ohat" => 0.5,
        "bass" => 0.7,
        "pad" => 0.4,
        _ => 0.6,
    }
}

/// Near-solo floor for non-essential tracks (−20 dB).
const FLOOR: f32 = 0.1;

/// Applies presence arcs to `sections`, scaling each track's configured
/// `gain`. `stripped` holds the ids of near-solo dev sections. Deterministic
/// in `seed`.
pub(crate) fn apply_presence(
    sections: &mut [Section],
    tracks: &[Track],
    stripped: &[String],
    seed: u64,
) {
    let mut rng = stream(seed, LANE_PRESENCE, PURPOSE_PRESENCE);
    let binds = |sec: &Section, id: &str| sec.pattern_bindings.contains_key(id);

    for track in tracks {
        let id = track.id.as_str();
        let track_gain = track.gain;
        let entry = entry_level(id);
        let mut prev = entry;
        for si in 0..sections.len() {
            let bound = binds(&sections[si], id);
            let next_bound = sections.get(si + 1).is_some_and(|s| binds(s, id));
            let solo = stripped.iter().any(|s| s == &sections[si].id);

            let end = match section_kind(&sections[si]) {
                SectionKind::Intro => {
                    if bound { entry.max(0.8) } else { prev.min(0.5) }
                }
                SectionKind::Reintro | SectionKind::Release => 1.0,
                SectionKind::Outro => (prev * (0.5 + 0.15 * rng.next_f32())).max(0.2),
                SectionKind::Tension => {
                    // The build rides just under full so the drop reads.
                    if bound {
                        (0.8 + 0.1 * rng.next_f32()).min(1.0)
                    } else {
                        prev.max(FLOOR)
                    }
                }
                SectionKind::Variation => {
                    if bound {
                        (prev.max(0.5) * (0.85 + 0.2 * rng.next_f32())).min(1.0)
                    } else {
                        FLOOR
                    }
                }
                SectionKind::Breakdown => {
                    if bound && id == "pad" {
                        0.8
                    } else if bound {
                        0.55
                    } else {
                        FLOOR
                    }
                }
                _ if solo => {
                    if id == "kick" {
                        1.0
                    } else if bound {
                        0.35
                    } else {
                        FLOOR
                    }
                }
                SectionKind::Dev => {
                    if bound {
                        (0.88 + 0.12 * rng.next_f32()).min(1.0)
                    } else if next_bound {
                        FLOOR
                    } else {
                        (prev * 0.7).max(FLOOR)
                    }
                }
            };

            let sec = &mut sections[si];
            // One lane per (section, track) slot: when a motion or pad
            // reverb lane owns the slot, hold the walk value there (no gain
            // lane) and keep continuity.
            let slot_busy = sec.automation.contains_key(id);
            let moves = (db(end) - db(prev)).abs() > 0.2;
            if (bound || next_bound || moves) && !slot_busy {
                let points = if sec.bars >= 2 {
                    vec![
                        (0, track_gain * prev, curve_for(prev, end)),
                        (sec.bars - 1, track_gain * end, CurveKind::Smooth),
                    ]
                } else {
                    vec![(0, track_gain * end, CurveKind::Linear)]
                };
                sec.automation.insert(
                    id.to_string(),
                    AutomationLane { target_param: "gain".into(), points },
                );
            }
            prev = end;
        }
    }
}

enum SectionKind {
    Intro,
    Dev,
    Tension,
    Release,
    Breakdown,
    Reintro,
    Variation,
    Outro,
}

fn section_kind(sec: &Section) -> SectionKind {
    match sec.id.as_str() {
        "intro" => SectionKind::Intro,
        "reintro" => SectionKind::Reintro,
        "outro" => SectionKind::Outro,
        id if id.starts_with("break_") => SectionKind::Breakdown,
        id if id.starts_with("tension_") => SectionKind::Tension,
        id if id.starts_with("release_") => SectionKind::Release,
        id if id.starts_with("variation_") => SectionKind::Variation,
        _ => SectionKind::Dev,
    }
}

fn db(v: f32) -> f32 {
    20.0 * v.max(1e-4).log10()
}

fn curve_for(from: f32, to: f32) -> CurveKind {
    if to < from {
        CurveKind::Exp // falling gain feels natural logarithmic
    } else {
        CurveKind::Smooth
    }
}
