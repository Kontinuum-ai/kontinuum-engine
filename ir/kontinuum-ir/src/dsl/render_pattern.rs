//! Rendering — pattern level: `ReplacePattern` values to canonical DSL
//! text. Coverage (everything else is an unsupported-IR error):
//! - `EuclideanPattern` with default velocity/probability/repeats and no
//!   gate/pitch → `track: E(k, n, rot);`
//! - `StepsPattern` on the 16th grid with zero microtiming →
//!   `track.vel = […];` (rests as 0.0; a stored step with velocity 0.0
//!   cannot survive the round trip and is rejected)
//! - `StepsPattern` whose velocities are all the euclid default and whose
//!   odd slots share one positive microtiming value →
//!   `track: E(k, n, rot) @ swing s;` (grid inverted by rotating the
//!   bucket grid until it matches)

use crate::schema::{Pattern, StepsPattern};

use super::error::DslError;
use super::grid::{bucket, EUCLID_VELOCITY, SLOTS_PER_BAR, TICKS_PER_16TH};
use super::render::unsupported;

pub(super) fn render_pattern(
    out: &mut String,
    section: &str,
    track: &str,
    pattern: &Pattern,
) -> Result<(), DslError> {
    let path = format!("/sections/{section}/pattern_bindings/{track}");
    match pattern {
        Pattern::Euclidean(e) => {
            if e.velocity != EUCLID_VELOCITY
                || e.probability != 1.0
                || e.repeats != 1
                || e.gate.is_some()
                || e.pitch.is_some()
            {
                return Err(unsupported(
                    &path,
                    "euclidean pattern uses non-default velocity/probability/repeats or a gate/pitch",
                    "drop the extras, or edit the IR directly (planned for a later DSL rev)",
                ));
            }
            out.push_str(&format!("  {track}: E({}, {}, {});\n", e.k, e.n, e.rot));
        }
        Pattern::Steps(s) => render_steps(out, &path, track, s)?,
        Pattern::ProbabilityMask(_) => {
            return Err(unsupported(
                &path,
                "probability masks have no DSL v0 statement",
                "use mask/vel/E statements, or edit the IR directly",
            ));
        }
    }
    Ok(())
}

fn render_steps(out: &mut String, path: &str, track: &str, s: &StepsPattern) -> Result<(), DslError> {
    if s.repeats != 1 {
        return Err(unsupported(path, "multi-bar steps (repeats != 1) are outside DSL v0", "keep repeats = 1"));
    }
    if s.steps.iter().any(|st| {
        st.probability != 1.0 || st.ratchet != 1 || st.gate.is_some() || st.pitch.is_some()
    }) {
        return Err(unsupported(
            path,
            "steps carry probability, ratchet, gate, or pitch — outside DSL v0",
            "use plain steps, or edit the IR directly",
        ));
    }
    // Swing signature check: on-grid, uniform euclid velocity, one shared
    // positive microtiming value on odd slots (even slots straight).
    let mut grid = [false; SLOTS_PER_BAR];
    let mut swing_ticks: Option<i16> = None;
    let mut candidate = true;
    for st in &s.steps {
        let slot = st.position / TICKS_PER_16TH;
        let slot_ok = st.position % TICKS_PER_16TH == 0
            && (slot as usize) < SLOTS_PER_BAR
            && st.velocity == EUCLID_VELOCITY;
        if !slot_ok {
            candidate = false;
            break;
        }
        if slot % 2 == 1 {
            match swing_ticks {
                Some(t) if t != st.microtiming_ticks => {
                    candidate = false;
                    break;
                }
                Some(_) => {}
                None => swing_ticks = Some(st.microtiming_ticks),
            }
        } else if st.microtiming_ticks != 0 {
            candidate = false;
            break;
        }
        grid[slot as usize] = true;
    }
    if candidate && swing_ticks.is_some_and(|t| t > 0) {
        let (k, n, rot) = invert_grid(&grid).ok_or_else(|| {
            unsupported(
                path,
                "swung steps do not match any E(k, n, rot) grid",
                "use a vel list with straight steps instead",
            )
        })?;
        let swing = swing_ticks.unwrap_or(0) as f32 / TICKS_PER_16TH as f32;
        out.push_str(&format!("  {track}: E({k}, {n}, {rot}) @ swing {};\n", fmt_f32(swing)));
        return Ok(());
    }
    // Plain velocity list: on-grid, zero microtiming, positive velocities.
    let mut vel = [0.0f32; SLOTS_PER_BAR];
    for st in &s.steps {
        let slot = st.position / TICKS_PER_16TH;
        if st.position % TICKS_PER_16TH != 0
            || (slot as usize) >= SLOTS_PER_BAR
            || st.microtiming_ticks != 0
            || !(0.0 < st.velocity && st.velocity <= 1.0)
        {
            return Err(unsupported(
                path,
                "steps must sit on the 16th grid with zero microtiming and velocities in (0, 1]",
                "align steps to 240-tick slots, or edit the IR directly",
            ));
        }
        vel[slot as usize] = st.velocity;
    }
    let list: Vec<String> = vel.iter().map(|v| fmt_f32(*v)).collect();
    out.push_str(&format!("  {track}.vel = [{}];\n", list.join(", ")));
    Ok(())
}

/// Finds a deterministic (k, n, rot) whose bucket grid matches `grid`:
/// smallest n, then k, then rotation.
fn invert_grid(grid: &[bool; SLOTS_PER_BAR]) -> Option<(u32, u32, i32)> {
    for n in 1..=SLOTS_PER_BAR as u32 {
        for k in 0..=n {
            for rot in 0..n as i32 {
                if bucket(k, n, rot) == *grid {
                    return Some((k, n, rot));
                }
            }
        }
    }
    None
}

/// Shortest round-trip float text (Rust's Display is exact for f32).
pub(super) fn fmt_f32(v: f32) -> String {
    format!("{v}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{EuclideanPattern, EuclideanTag, Step};

    fn step(position: u32, velocity: f32, micro: i16) -> Step {
        Step {
            position,
            velocity,
            probability: 1.0,
            microtiming_ticks: micro,
            ratchet: 1,
            pitch: None,
            gate: None,
            accent: false,
        }
    }

    fn steps_pattern(steps: Vec<Step>) -> Pattern {
        Pattern::Steps(StepsPattern { steps, repeats: 1 })
    }

    #[test]
    fn swung_steps_render_through_the_e_shorthand() {
        let steps: Vec<Step> = bucket(4, 16, 0)
            .iter()
            .enumerate()
            .filter(|(_, on)| **on)
            .map(|(i, _)| step(i as u32 * TICKS_PER_16TH, 0.8, if i % 2 == 1 { 30 } else { 0 }))
            .collect();
        let mut out = String::new();
        render_pattern(&mut out, "a", "perc", &steps_pattern(steps)).expect("render");
        assert!(out.contains("perc: E(4, 16, 0) @ swing 0.125;"), "text: {out}");
    }

    #[test]
    fn plain_steps_render_as_vel_lists() {
        let mut out = String::new();
        render_pattern(
            &mut out,
            "a",
            "kick",
            &steps_pattern(vec![step(0, 0.8, 0), step(TICKS_PER_16TH, 0.5, 0)]),
        )
        .expect("render");
        assert!(
            out.contains("kick.vel = [0.8, 0.5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];"),
            "text: {out}"
        );
    }

    #[test]
    fn euclid_patterns_render_directly_from_fields() {
        let mut out = String::new();
        render_pattern(
            &mut out,
            "a",
            "hat",
            &Pattern::Euclidean(EuclideanPattern {
                generator: EuclideanTag::Euclidean,
                k: 4,
                n: 16,
                rot: 2,
                velocity: EUCLID_VELOCITY,
                probability: 1.0,
                repeats: 1,
                gate: None,
                pitch: None,
            }),
        )
        .expect("render");
        assert_eq!(out, "  hat: E(4, 16, 2);\n");
    }
}
