//! Pattern → onset expansion and engine id mapping for the IR compiler.
//!
//! Determinism contract: expansion consumes randomness only for
//! `probability_mask` patterns, seeded from
//! `(session.seed, section id hash, track index, phrase index)` via
//! [`kontinuum_clock::derive_seed`]. Same input document → identical output,
//! independent of block boundaries.
//!
//! Per-hit `probability` rides on each [`Onset`] but is *not* applied here:
//! expansion stays a pure expansion and emits every hit. The gate runs in
//! `compile_session`, which draws from an RNG stream derived from the session
//! seed (see `LANE_PROBABILITY` there) and keeps each onset iff
//! `rng.chance(onset.probability)`. Same session → same gate decisions, and a
//! session whose probabilities are all 1.0 compiles byte-identically to an
//! ungated one.

use kontinuum_clock::{derive_seed, Rng, TICKS_PER_BAR};
use kontinuum_schedule::ParamId;

use crate::schema::{bounds, Pattern, Step};
use crate::TrackRole;

/// Ticks per 16th (3840 / 16 with PPQ=960, 4 beats per bar).
pub const TICKS_PER_SIXTEENTH: u64 = TICKS_PER_BAR / 16;

/// ParamId layout: `0xTN00 | track_index` (T = param class, N = track 0..255).
pub const PARAM_TRACK_GAIN: u16 = 0x0100;
pub const PARAM_TRACK_PAN: u16 = 0x0200;
pub const PARAM_INSERT0: u16 = 0x0300;
pub const PARAM_INSERT1: u16 = 0x0400;
pub const PARAM_SEND_DELAY: u16 = 0x0500;
pub const PARAM_SEND_REVERB: u16 = 0x0600;

/// Resolves an automation `target_param` for a track index to a ParamId.
pub fn resolve_param(track: u8, target: &str) -> Option<ParamId> {
    let t = track as u16;
    match target {
        "gain" => Some(PARAM_TRACK_GAIN | t),
        "pan" => Some(PARAM_TRACK_PAN | t),
        "insert0" => Some(PARAM_INSERT0 | t),
        "insert1" => Some(PARAM_INSERT1 | t),
        "send_delay" => Some(PARAM_SEND_DELAY | t),
        "send_reverb" => Some(PARAM_SEND_REVERB | t),
        _ => None,
    }
}

/// Concurrent-voice pools per role (issue #11 dry-run budgets).
pub const POOL_KICK: u8 = 8;
pub const POOL_PERC: u8 = 16;
pub const POOL_BASS: u8 = 4;
pub const POOL_PAD: u8 = 8;
pub const POOL_FX: u8 = 8;

/// Voice-slot pool size for a role.
pub fn pool_for_role(role: TrackRole) -> u8 {
    match role {
        TrackRole::Kick => POOL_KICK,
        TrackRole::Perc => POOL_PERC,
        TrackRole::Bass => POOL_BASS,
        TrackRole::Pad => POOL_PAD,
        TrackRole::Fx => POOL_FX,
    }
}

/// Per-voice CPU cost estimate units (issue #11 cost table).
pub fn role_cost(role: TrackRole) -> f32 {
    match role {
        TrackRole::Kick => 1.0,
        TrackRole::Perc => 0.6,
        TrackRole::Bass => 2.0,
        TrackRole::Pad => 3.0,
        TrackRole::Fx => 1.5,
    }
}

/// Roles whose notes are sustained and need NoteOff events.
pub fn is_sustained(role: TrackRole) -> bool {
    matches!(role, TrackRole::Bass | TrackRole::Pad)
}

/// Default gate in beats when a pattern/step does not specify one.
pub fn default_gate_beats(role: TrackRole) -> f32 {
    match role {
        TrackRole::Bass | TrackRole::Pad => 1.0,
        _ => 0.5,
    }
}

/// One expanded hit, positioned in ticks relative to the phrase start.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Onset {
    pub pos_ticks: u64,
    pub velocity: f32,
    pub microtiming_ticks: i16,
    pub pitch: Option<f32>,
    pub gate_beats: Option<f32>,
    /// Kept by the compile-time gate iff `rng.chance(probability)`; sub-hits
    /// inherit the step's value so the whole step is gated, not per sub-hit.
    pub probability: f32,
}

/// Deterministic Euclidean rhythm via Bresenham spread (`floor(i·k/n)`
/// increments), rotated by `rot` slots (negative rotation rotates right). The
/// first slot always fires so `euclidean(4, 16)` is four-on-the-floor.
pub fn euclidean(k: u32, n: u32, rot: i32) -> Vec<bool> {
    if n == 0 {
        return vec![];
    }
    // `k = 0` is silence. Without this the unconditional first-slot rule below
    // fires one onset for a pattern that asked for none.
    if k == 0 {
        return vec![false; n as usize];
    }
    let k = k.min(n);
    let mut grid = Vec::with_capacity(n as usize);
    let prev = |i: u32| u64::from(i) * u64::from(k) / u64::from(n);
    for i in 0..n {
        grid.push(if i == 0 {
            true
        } else {
            prev(i) > prev(i - 1)
        });
    }
    let r = rot.rem_euclid(n as i32) as usize;
    grid.rotate_left(r);
    grid
}

/// FNV-1a over bytes — stable across platforms, used to salt RNG streams by
/// section id.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// RNG stream for a `(session, section, track, phrase)` probability mask.
pub fn mask_rng(session_seed: u64, section_id: &str, track: u8, phrase: u32) -> Rng {
    let master = session_seed
        ^ fnv1a(section_id.as_bytes())
        ^ ((phrase as u64) << 24);
    Rng::from_seed(derive_seed(master, track, 0x5EED))
}

const RATCHET_TICK_SPACING: u64 = 12;
const RATCHET_VELOCITY_FALLOFF: f32 = 0.85;
const ACCENT_GAIN: f32 = 1.2;

/// Expands one explicit step into ratchet sub-hits.
fn step_onsets(step: &Step, base_ticks: u64, out: &mut Vec<Onset>) {
    let ratchet = step.ratchet.clamp(bounds::RATCHET.0, bounds::RATCHET.1);
    let base_velocity = (step.velocity * if step.accent { ACCENT_GAIN } else { 1.0 })
        .clamp(0.0, 1.0);
    for j in 0..u32::from(ratchet) {
        out.push(Onset {
            pos_ticks: base_ticks + j as u64 * RATCHET_TICK_SPACING,
            velocity: base_velocity * RATCHET_VELOCITY_FALLOFF.powi(j as i32),
            microtiming_ticks: step.microtiming_ticks,
            pitch: step.pitch,
            gate_beats: step.gate,
            probability: step.probability,
        });
    }
}

/// Expands a pattern into phrase-relative onsets. `repeats` is the pattern
/// period in bars: deterministic content loops each bar; `probability_mask`
/// draws once across the whole phrase, so variation lands every `repeats`
/// bars.
pub fn expand_pattern(pattern: &Pattern, rng: &mut Rng) -> Vec<Onset> {
    let repeats = pattern.repeats().max(1);
    let mut out = Vec::new();
    match pattern {
        Pattern::Steps(p) => {
            for bar in 0..u64::from(repeats) {
                let base = bar * TICKS_PER_BAR;
                for step in &p.steps {
                    step_onsets(step, base + step.position as u64, &mut out);
                }
            }
        }
        Pattern::Euclidean(p) => {
            let n = p.n.clamp(1, bounds::EUCLID_MAX_N);
            let grid = euclidean(p.k, n, p.rot);
            for bar in 0..u64::from(repeats) {
                let base = bar * TICKS_PER_BAR;
                for (slot, on) in grid.iter().enumerate() {
                    if *on {
                        out.push(Onset {
                            pos_ticks: base + slot as u64 * TICKS_PER_BAR / n as u64,
                            velocity: p.velocity,
                            microtiming_ticks: 0,
                            pitch: p.pitch,
                            gate_beats: p.gate,
                            probability: p.probability,
                        });
                    }
                }
            }
        }
        Pattern::ProbabilityMask(p) => {
            let slots = u64::from(repeats) * 16;
            for slot in 0..slots {
                if rng.chance(p.density) {
                    out.push(Onset {
                        pos_ticks: slot * TICKS_PER_SIXTEENTH,
                        velocity: p.velocity,
                        microtiming_ticks: 0,
                        pitch: p.pitch,
                        gate_beats: p.gate,
                        probability: p.probability,
                    });
                }
            }
        }
    }
    out
}

/// Expected onset count per bar for a pattern (static density lint; exact for
/// deterministic content, expected value for masks).
pub fn onsets_per_bar(pattern: &Pattern) -> f64 {
    match pattern {
        Pattern::Steps(p) => p
            .steps
            .iter()
            .map(|s| f64::from(s.ratchet.clamp(bounds::RATCHET.0, bounds::RATCHET.1)))
            .sum(),
        Pattern::Euclidean(p) => f64::from(p.k.min(p.n.clamp(1, bounds::EUCLID_MAX_N))),
        Pattern::ProbabilityMask(p) => p.density as f64 * 16.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn euclidean_four_on_floor() {
        let g = euclidean(4, 16, 0);
        assert_eq!(g.iter().filter(|x| **x).count(), 4);
        assert!(g[0]);
        assert!(!g[1]);
        // Rotation moves the pattern without changing onset count; left
        // rotation wraps the last onset to the front.
        let rot = euclidean(4, 16, 1);
        assert_eq!(rot.iter().filter(|x| **x).count(), 4);
        assert!(!rot[0]);
        assert!(rot[3] && rot[15]);
        // k > n clamps to n (every slot).
        assert_eq!(euclidean(20, 8, 0).iter().filter(|x| **x).count(), 8);
        assert!(euclidean(3, 0, 0).is_empty());
        // k = 0 is silence, not a lone downbeat.
        assert_eq!(euclidean(0, 16, 0), vec![false; 16]);
        // Negative rotation is equivalent to positive from the other side.
        assert_eq!(euclidean(4, 16, -1), euclidean(4, 16, 15));
    }

    #[test]
    fn expand_steps_with_ratchet_and_repeats() {
        let p: Pattern = serde_json::from_str(
            r#"{"steps":[{"position":0,"ratchet":2,"velocity":0.8}],"repeats":2}"#,
        )
        .expect("parse");
        let onsets = expand_pattern(&p, &mut Rng::from_seed(1));
        assert_eq!(onsets.len(), 4, "1 step x 2 ratchet x 2 bars");
        assert_eq!(onsets[0].pos_ticks, 0);
        assert_eq!(onsets[1].pos_ticks, RATCHET_TICK_SPACING);
        assert_eq!(onsets[2].pos_ticks, TICKS_PER_BAR);
        assert!(onsets[1].velocity < onsets[0].velocity);
    }

    #[test]
    fn accent_boosts_velocity_and_ratchet_subhits_inherit() {
        let p: Pattern = serde_json::from_str(
            r#"{"steps":[{"position":0,"velocity":0.5,"ratchet":3,"accent":true}]}"#,
        )
        .expect("parse");
        let onsets = expand_pattern(&p, &mut Rng::from_seed(1));
        assert_eq!(onsets.len(), 3);
        let base = 0.5 * ACCENT_GAIN;
        let expected = [base, base * 0.85, base * 0.85 * 0.85];
        for (o, e) in onsets.iter().zip(expected) {
            assert!((o.velocity - e).abs() < 1e-6, "{} vs {e}", o.velocity);
        }
        // An accented velocity saturates at 1.0; the falloff starts from there.
        let p: Pattern =
            serde_json::from_str(r#"{"steps":[{"position":0,"velocity":0.9,"ratchet":2,"accent":true}]}"#)
                .expect("parse");
        let onsets = expand_pattern(&p, &mut Rng::from_seed(1));
        assert_eq!(onsets[0].velocity, 1.0);
        assert!((onsets[1].velocity - 0.85).abs() < 1e-6);
    }

    #[test]
    fn mask_expansion_is_seeded_and_bounded() {
        let p: Pattern =
            serde_json::from_str(r#"{"generator":"probability_mask","density":0.5}"#).expect("p");
        let a = expand_pattern(&p, &mut mask_rng(7, "a", 0, 0));
        let b = expand_pattern(&p, &mut mask_rng(7, "a", 0, 0));
        let c = expand_pattern(&p, &mut mask_rng(8, "a", 0, 0));
        assert_eq!(a, b, "same seed stream = same mask");
        assert_ne!(a, c, "different seed = different mask");
        assert!(a.len() <= 16, "one 16th slot max per bar");
    }

    #[test]
    fn resolve_param_layout() {
        assert_eq!(resolve_param(3, "gain"), Some(0x0103));
        assert_eq!(resolve_param(0, "pan"), Some(0x0200));
        assert_eq!(resolve_param(2, "send_reverb"), Some(0x0602));
        assert_eq!(resolve_param(0, "nope"), None);
    }

    #[test]
    fn onsets_per_bar_matches_expansion() {
        for k in 1..=16u32 {
            let p: Pattern = serde_json::from_str(
                &format!(r#"{{"generator":"euclidean","k":{k},"n":16}}"#),
            )
            .expect("p");
            let expanded = expand_pattern(&p, &mut Rng::from_seed(1)).len() as f64;
            assert!((onsets_per_bar(&p) - expanded).abs() < 1e-9);
        }
    }
}
