//! Shared rhythm-grid math and v0 grid constants, used by the emitter, the
//! renderer, and the round-trip invariant between them: a swung
//! `StepsPattern` and the `E(k, n, rot)` shorthand meet at [`bucket`].

/// Ticks per 16th at the IR's PPQ 960 with 4 beats per bar.
pub(super) const TICKS_PER_16TH: u32 = 240;
/// Slots per bar at 16th resolution (the DSL's v0 grid).
pub(super) const SLOTS_PER_BAR: usize = 16;
/// Step velocity for mask hits and euclidean onsets (the IR default).
pub(super) const EUCLID_VELOCITY: f32 = 0.8;
/// Swing delay ceiling = the IR microtiming bound, in ticks.
pub(super) const MAX_SWING_TICKS: f32 = 120.0;

/// Deterministic Euclidean grid via the bucket algorithm (same contract as
/// the engine's generator): slot i fires when the running bucket wraps.
pub(super) fn bucket(k: u32, n: u32, rot: i32) -> Vec<bool> {
    let mut acc = 0u32;
    let mut grid = Vec::with_capacity(n as usize);
    for _ in 0..n {
        acc += k;
        if acc >= n {
            acc -= n;
            grid.push(true);
        } else {
            grid.push(false);
        }
    }
    let r = rot.rem_euclid(n as i32) as usize;
    grid.rotate_left(r);
    grid
}

pub(super) fn is_unit(v: f32) -> bool {
    v.is_finite() && (0.0..=1.0).contains(&v)
}
