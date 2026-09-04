//! Ghost-note pass (issue #17): quiet, low-probability 16th-grid hits that
//! thicken the percussion without ever taking the accent. Ghosts land only
//! on 16th slots no existing step occupies, so they never double-hit a grid
//! cell. The pass runs before [`crate::groove`] so the groove's microtiming
//! and offbeat contour shape the ghosts like any other hit.

use kontinuum_clock::{Rng, TICKS_PER_BAR};
use kontinuum_ir::schema::Step;

/// The 16th-note grid: 16 slots per bar (240 ticks at PPQ 960).
const SLOTS: u32 = 16;
const SLOT_TICKS: u32 = (TICKS_PER_BAR / SLOTS as u64) as u32;
/// Ghost dynamics: felt, not heard. The probability window is tuned so the
/// EXPECTED audible ghosts stay ~0.2/bar — the Easy Lee ratchet (#52) caps
/// transients/sec, so ghosts must thicken feel without adding detected hits.
const VELOCITY_FLOOR: f32 = 0.12;
const VELOCITY_CEILING: f32 = 0.30;
const PROBABILITY: (f32, f32) = (0.08, 0.22);

/// Nearest 16th slot of a step position.
fn slot(position: u32) -> u32 {
    ((position + SLOT_TICKS / 2) / SLOT_TICKS).min(SLOTS - 1)
}

/// Inserts 1–2 ghost hits into `steps` (one bar) on free 16th-grid slots.
/// The count scales with `energy` — quiet sections get one, hot sections
/// reach for two — and with `density` (#16 density curve: busy sections
/// thicken, sparse ones breathe). Velocities tilt mildly up with energy
/// while staying in 0.12..=0.30; probabilities live in 0.08..=0.22.
/// Deterministic in the caller's [`Rng`] stream.
pub fn ghost_pass(steps: &mut Vec<Step>, energy: f32, density: f32, rng: &mut Rng) {
    let energy = energy.clamp(0.0, 1.0);
    let occupied: Vec<u32> = steps.iter().map(|s| slot(s.position)).collect();
    let mut free: Vec<u32> = (0..SLOTS)
        .filter(|s| !occupied.contains(s))
        .map(|s| s * SLOT_TICKS)
        .collect();
    let ceiling = 1 + energy.round() as u64;
    let count = (1 + rng.below(ceiling)) as usize;
    let count = ((count as f32 * (0.75 + 0.5 * density.clamp(0.0, 1.0))).round() as usize).max(1);
    let count = count.min(free.len());
    let velocity_ceiling = VELOCITY_FLOOR + (VELOCITY_CEILING - VELOCITY_FLOOR) * (0.55 + 0.45 * energy);
    for _ in 0..count {
        let picked = rng.below(free.len() as u64) as usize;
        let position = free.swap_remove(picked);
        steps.push(Step {
            position,
            velocity: rng.range_f32(VELOCITY_FLOOR, velocity_ceiling),
            probability: rng.range_f32(PROBABILITY.0, PROBABILITY.1),
            microtiming_ticks: 0,
            ratchet: 1,
            pitch: None,
            gate: None,
            accent: false,
        });
    }
    steps.sort_by_key(|s| s.position);
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn base_steps() -> Vec<Step> {
        [0u32, 480, 960, 1440, 1920, 2400, 2880, 3360]
            .iter()
            .map(|&position| Step {
                position,
                velocity: 0.7,
                probability: 1.0,
                microtiming_ticks: 0,
                ratchet: 1,
                pitch: None,
                gate: None,
                accent: false,
            })
            .collect()
    }

    #[test]
    fn ghosts_avoid_occupied_slots_and_stay_quiet() {
        for energy in [0.1f32, 0.5, 0.95] {
            let mut steps = base_steps();
            let before = steps.clone();
            ghost_pass(&mut steps, energy, 0.6, &mut Rng::from_seed(17));
            let mut twin = before.clone();
            ghost_pass(&mut twin, energy, 0.6, &mut Rng::from_seed(17));
            assert_eq!(steps, twin, "energy {energy}: non-deterministic");
            let ghosts: Vec<&Step> = steps.iter().filter(|s| s.probability < 1.0).collect();
            assert!(
                (1..=2).contains(&ghosts.len()),
                "energy {energy}: {} ghosts",
                ghosts.len()
            );
            let occupied: HashSet<u32> = before.iter().map(|s| slot(s.position)).collect();
            for g in &ghosts {
                assert!(!occupied.contains(&slot(g.position)), "ghost double-hits a slot");
                assert!((0.12..=0.30).contains(&g.velocity), "velocity {}", g.velocity);
                assert!((0.08..0.22).contains(&g.probability), "probability {}", g.probability);
                assert_eq!(g.ratchet, 1);
                assert!(g.pitch.is_none() && g.gate.is_none());
                assert!(!g.accent);
            }
        }
    }

    #[test]
    fn ghost_count_scales_with_energy() {
        let quiet_counts: HashSet<usize> = (0..20u64)
            .map(|i| {
                let mut s = base_steps();
                ghost_pass(&mut s, 0.05, 0.6, &mut Rng::from_seed(i));
                s.iter().filter(|g| g.probability < 1.0).count()
            })
            .collect();
        assert_eq!(quiet_counts, HashSet::from([1]), "quiet bars get exactly one ghost");
        let hot_max = (0..20u64)
            .map(|i| {
                let mut s = base_steps();
                ghost_pass(&mut s, 0.95, 0.6, &mut Rng::from_seed(i));
                s.iter().filter(|g| g.probability < 1.0).count()
            })
            .max()
            .unwrap_or_default();
        assert!(hot_max >= 2, "hot bars reach for more ghosts (max {hot_max})");
        assert!(hot_max <= 2, "ghosts stay bounded at 2 (max {hot_max})");
    }
}
