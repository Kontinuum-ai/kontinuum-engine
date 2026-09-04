//! Fill generator (issue #17 "Fills" checklist): last-bar-of-section
//! variations — a percussion density ramp across the bar, rising
//! snare/clap rolls, and dropouts — shaped by what the boundary points
//! into. Emits plain IR [`Pattern`]s through the existing vocabulary
//! (steps, per-step probability), determinism contract as [`crate::pattern`].

use kontinuum_clock::{Rng, TICKS_PER_BAR};
use kontinuum_ir::schema::{Pattern, Step, StepsPattern};

use crate::pattern::humanize;

const BAR_END_TICKS: i64 = TICKS_PER_BAR as i64 - 1;
/// 32nd-note roll spacing (~120 ticks at PPQ 960).
const ROLL_SPACING_TICKS: i64 = 120;

/// What follows the fill bar: the gesture leans collapse or ignition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Boundary {
    /// Collapsing into a breakdown — dropouts thin the early bar.
    Breakdown,
    /// Full stack returns — the biggest ramp, no dropouts.
    Reintro,
    /// Ordinary development handoff.
    Dev,
}

/// Section-boundary context for [`fill_pattern`].
pub struct FillContext {
    /// Section length in bars.
    pub section_bars: u32,
    /// 0-based bar within the section.
    pub bar: u32,
    /// Section energy (0..=1), scales roll length and ramp depth.
    pub energy: f32,
    pub into: Boundary,
}

impl FillContext {
    fn is_boundary_bar(&self) -> bool {
        self.bar + 1 == self.section_bars
    }
}

/// The boundary-bar fill: `Some` only when `ctx` names the section's last
/// bar — fills never fire mid-section.
pub fn fill_pattern(ctx: &FillContext, rng: &mut Rng) -> Option<Pattern> {
    if !ctx.is_boundary_bar() {
        return None;
    }
    // Cumulative density ramp: quarters, then 8ths, then a full 16th tail.
    let slots: Vec<usize> = (0..16)
        .step_by(4)
        .chain((8..16).step_by(2))
        .chain(12..16)
        .collect();
    let roll_slot = 15 - usize::from(ctx.into == Boundary::Reintro);
    let dropout_ceiling = match ctx.into {
        Boundary::Breakdown => 0.5,
        Boundary::Dev => 0.85,
        Boundary::Reintro => 1.0,
    };
    let steps: Vec<Step> = slots
        .iter()
        .filter(|&&s| s != roll_slot)
        .map(|&s| {
            let pos = s as i64 * 240;
            let ramp = s as f32 / 15.0;
            let base = 0.35 + 0.4 * ctx.energy;
            // Early slots thin out (dropouts); the tail is solid. Reintro's
            // ceiling of 1.0 zeroes the dropout depth entirely.
            let probability = 1.0 - (1.0 - dropout_ceiling) * (1.0 - ramp);
            let (t, v) = humanize(pos, (base * (0.7 + 0.3 * ramp)).clamp(0.0, 1.0), rng, 6, 0.03);
            step(t, v, probability)
        })
        .collect();
    let roll_pos = (roll_slot as i64 * 240).min(BAR_END_TICKS) as u32;
    let mut steps = steps;
    steps.extend(roll_steps(roll_pos, (0.5 + 0.4 * ctx.energy).clamp(0.0, 1.0), 2 + rng.below(3) as u8, rng));
    Some(Pattern::Steps(StepsPattern { steps, repeats: 1 }))
}

/// Rising snare/clap roll at `position_ticks`: `sub_hits` 32nd-note hits
/// with linearly rising velocity into the boundary (the mirror of
/// [`crate::pattern::ratchet_steps`]'s flam falloff). Deterministic in the
/// caller's stream.
pub fn roll_steps(position_ticks: u32, velocity: f32, sub_hits: u8, rng: &mut Rng) -> Vec<Step> {
    let count = sub_hits.clamp(2, 8) as usize;
    (0..count)
        .map(|j| {
            let rise = j as f32 / (count - 1).max(1) as f32;
            let (t, v) = humanize(
                position_ticks as i64 + j as i64 * ROLL_SPACING_TICKS,
                (velocity * (0.6 + 0.4 * rise)).clamp(0.0, 1.0),
                rng,
                4,
                0.02,
            );
            step(t.min(BAR_END_TICKS), v, 1.0)
        })
        .collect()
}

fn step(position: i64, velocity: f32, probability: f32) -> Step {
    Step {
        position: position as u32,
        velocity,
        probability,
        microtiming_ticks: 0,
        ratchet: 1,
        pitch: None,
        gate: None,
        accent: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(bar: u32, into: Boundary) -> FillContext {
        FillContext { section_bars: 8, bar, energy: 0.6, into }
    }

    #[test]
    fn fills_fire_on_boundary_bars_only() {
        let mut rng = Rng::from_seed(1);
        for bar in 0..7u32 {
            assert!(fill_pattern(&ctx(bar, Boundary::Dev), &mut rng).is_none(), "bar {bar}");
        }
        assert!(fill_pattern(&ctx(7, Boundary::Dev), &mut rng).is_some());
    }

    #[test]
    fn boundary_fill_is_deterministic_and_in_vocabulary() {
        for into in [Boundary::Dev, Boundary::Breakdown, Boundary::Reintro] {
            let mut a = Rng::from_seed(9);
            let mut b = Rng::from_seed(9);
            let pa = fill_pattern(&ctx(7, into), &mut a).expect("boundary fill");
            let pb = fill_pattern(&ctx(7, into), &mut b).expect("boundary fill");
            assert_eq!(pa, pb, "{into:?} non-deterministic");
            let Pattern::Steps(st) = pa else { panic!("fills are step patterns") };
            assert!(!st.steps.is_empty());
            for s in &st.steps {
                assert!(s.position < TICKS_PER_BAR as u32);
                assert!((0.0..=1.0).contains(&s.velocity));
                assert!((0.0..=1.0).contains(&s.probability));
            }
        }
    }

    #[test]
    fn breakdown_boundary_drops_out_reintro_does_not() {
        let mut rng = Rng::from_seed(3);
        let Pattern::Steps(st) = fill_pattern(&ctx(7, Boundary::Breakdown), &mut rng).unwrap()
        else {
            unreachable!()
        };
        assert!(st.steps.iter().any(|s| s.probability < 1.0), "breakdown thins the early bar");
        let Pattern::Steps(ri) = fill_pattern(&ctx(7, Boundary::Reintro), &mut rng).unwrap()
        else {
            unreachable!()
        };
        assert!(ri.steps.iter().all(|s| s.probability == 1.0), "reintro stays solid");
    }

    #[test]
    fn rolls_rise_into_the_boundary() {
        let roll = roll_steps(3600, 0.8, 4, &mut Rng::from_seed(5));
        assert_eq!(roll.len(), 4);
        assert!(roll.windows(2).all(|w| w[0].velocity < w[1].velocity), "rising roll");
        assert!(roll.windows(2).all(|w| w[0].position < w[1].position));
    }
}
