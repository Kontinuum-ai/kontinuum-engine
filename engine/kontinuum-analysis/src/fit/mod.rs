//! One-shot parameter fitting driver (issue #75): seeded random restarts +
//! bounded Nelder–Mead over the multi-resolution objective, rendering
//! candidates through the real core voices. Deterministic: the same target
//! audio, kind, and seed produce bit-identical parameters on every run
//! (pinned by test).
//!
//! Restart schedule: restart 0 starts from the voice defaults (a sensible
//! prior); restarts 1..N draw uniform starts in the middle 90% of the
//! normalized search box, seeded `seed ^ r·GOLDEN` through [`SplitMix64`].

pub mod nelder_mead;
pub mod objective;
pub mod render;
pub mod rng;
pub mod voice_kind;

pub use nelder_mead::{NelderMead, Optimum};
pub use objective::{FitObjective, LossParts, ENV_WEIGHT, SPEC_WEIGHTS, SPEC_WINDOWS};
pub use rng::SplitMix64;
pub use voice_kind::{ParamSpec, VoiceKind};

/// Restart seed spacing (Knuth's golden-ratio hash).
const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;

/// Fit configuration. `restarts` includes the default-params restart.
#[derive(Clone, Copy, Debug)]
pub struct FitConfig {
    pub restarts: usize,
    pub seed: u64,
    pub sample_rate: u32,
    /// Rendered hit length in frames; both target and candidates are
    /// compared over exactly this many frames.
    pub frames: usize,
}

impl FitConfig {
    /// Default schedule: 8 restarts (documented prior + 7 seeded random).
    pub fn new(sample_rate: u32, frames: usize) -> FitConfig {
        FitConfig { restarts: 8, seed: 0, sample_rate, frames }
    }
}

/// The fitter's answer: real-unit params (in [`VoiceKind::params`] order)
/// and the final objective value of that exact render.
#[derive(Clone, Debug, PartialEq)]
pub struct FitResult {
    pub params: Vec<f32>,
    pub loss: f64,
}

/// The fitter's normalized search box + optimizer settings, threaded
/// through every restart together.
struct SearchSpace<'a> {
    lo: Vec<f64>,
    hi: Vec<f64>,
    nm: &'a NelderMead,
}

/// Fits `kind` to `target` (mono f32 at `cfg.sample_rate`). Bounds come
/// from the voices' `set_param` clamps, so the returned params are valid
/// IR by construction — see [`VoiceKind`].
///
/// Restart 0 (the defaults prior) seeds the incumbent; restarts 1..N run
/// on parallel std threads. The winner is chosen deterministically —
/// lowest loss, ties broken by restart index — so results are
/// bit-identical regardless of scheduling or core count. The winner then
/// gets one [`NelderMead::polish`] pass: the voice landscapes have flat
/// click/drive-style valleys where a coarse simplex stops early.
pub fn fit(target: &[f32], kind: VoiceKind, cfg: &FitConfig) -> FitResult {
    let lo = vec![0.0; kind.params().len()];
    let hi = vec![1.0; kind.params().len()];
    let space = SearchSpace { lo, hi, nm: &NelderMead::default() };
    let frames = cfg.frames.min(target.len());
    let restarts = cfg.restarts.max(1);

    let mut obj = FitObjective::new(&target[..frames], cfg.sample_rate);
    let (mut best_x, mut best_f) = run_restart(&mut obj, kind, &space, 0, cfg);

    if restarts > 1 {
        let threads = (restarts - 1)
            .min(std::thread::available_parallelism().map_or(1, |n| n.get()));
        let space_ref = &space;
        let found = std::thread::scope(|s| {
            let handles: Vec<_> = (0..threads)
                .map(|t| {
                    s.spawn(move || {
                        let mut obj =
                            FitObjective::new(&target[..frames], cfg.sample_rate);
                        (t..restarts - 1)
                            .step_by(threads)
                            .map(|r| run_restart(&mut obj, kind, space_ref, r + 1, cfg))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                // A worker that died (it cannot, but never lose restart 0's
                // answer over it) degrades to fewer restarts.
                .filter_map(|h| h.join().ok())
                .flatten()
                .collect::<Vec<_>>()
        });
        for (x, f) in found {
            if f < best_f {
                best_x = x;
                best_f = f;
            }
        }
    }

    let polish = SearchSpace { lo: space.lo, hi: space.hi, nm: &NelderMead::polish() };
    let (x, f) = run_nm_from(&mut obj, kind, &polish, cfg, best_x.clone());
    if f < best_f {
        best_x = x;
        best_f = f;
    }

    FitResult { params: kind.from_normalized(&best_x), loss: best_f }
}

/// Seeded NM from restart `r`'s start point (0 = voice defaults, else a
/// seeded uniform draw in the middle 90% of the box).
fn run_restart(
    obj: &mut FitObjective,
    kind: VoiceKind,
    space: &SearchSpace,
    r: usize,
    cfg: &FitConfig,
) -> (Vec<f64>, f64) {
    let mut rng = SplitMix64::new(cfg.seed ^ (r as u64).wrapping_mul(GOLDEN));
    let x0: Vec<f64> = if r == 0 {
        kind.to_normalized(&kind.params().iter().map(|p| p.default).collect::<Vec<f32>>())
    } else {
        (0..kind.params().len()).map(|_| rng.next_range(0.05, 0.95)).collect()
    };
    run_nm_from(obj, kind, space, cfg, x0)
}

/// NM from an explicit normalized start point.
fn run_nm_from(
    obj: &mut FitObjective,
    kind: VoiceKind,
    space: &SearchSpace,
    cfg: &FitConfig,
    x0: Vec<f64>,
) -> (Vec<f64>, f64) {
    let mut objective = |x: &[f64]| -> f64 {
        let params = kind.from_normalized(x);
        let audio = render::render_note(kind, &params, cfg.sample_rate, cfg.frames);
        obj.loss(&audio)
    };
    let opt =
        nelder_mead::minimize(&mut objective, &x0, &space.lo, &space.hi, space.nm);
    (opt.x, opt.f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fit::render::render_note;

    const SR: u32 = 48_000;

    /// Renders a target at known params for the round-trip gate. Frames
    /// cover the hit's actual ring-out (−90 dB ≈ 1.5× the decay setting,
    /// plus margin) — real one-shot files are exactly this long.
    fn target(kind: VoiceKind, params: &[f32]) -> Vec<f32> {
        let ring_out_sec = (params[1] / 1000.0 * 1.5 + 0.15).min(1.5);
        render_note(kind, params, SR, (ring_out_sec * SR as f32) as usize)
    }

    /// THE GATE (issue #75): targets rendered by our own voices at known
    /// params must be recovered within tolerance, with the final loss far
    /// below any plausible bad fit. Proves the fitter before real material.
    #[test]
    fn round_trip_recovers_known_params_for_every_voice_kind() {
        let cases: &[(VoiceKind, &[f32])] = &[
            (VoiceKind::Kick, &[50.0, 430.0, 0.55, 2.2]),
            (VoiceKind::Kick, &[95.0, 180.0, 0.2, 4.5]),
            (VoiceKind::Kick, &[40.0, 800.0, 0.9, 1.0]),
            (VoiceKind::Hat, &[45.0, 0.4]),
            (VoiceKind::Hat, &[400.0, 0.1]),
            (VoiceKind::Hat, &[120.0, 0.8]),
            (VoiceKind::Clap, &[350.0, 0.55]),
            (VoiceKind::Clap, &[150.0, 0.1]),
            (VoiceKind::Clap, &[900.0, 0.9]),
        ];
        for (kind, true_params) in cases {
            let t = target(*kind, true_params);
            let cfg = FitConfig {
                restarts: 4,
                seed: 1234,
                sample_rate: SR,
                frames: t.len(),
            };
            let r = fit(&t, *kind, &cfg);
            for ((spec, &got), &want) in
                kind.params().iter().zip(r.params.iter()).zip(true_params.iter())
            {
                let tol = match spec.name {
                    "tune_hz" => 3.0 + 0.03 * want.abs(),
                    "decay_ms" => 0.08 * want.abs(),
                    "drive" => 0.10 * want.abs(),
                    _ => 0.15,
                };
                assert!(
                    (got - want).abs() <= tol,
                    "{kind:?} {} recovered {} wants {} (tol {tol})",
                    spec.name,
                    got,
                    want
                );
            }
            assert!(r.loss < 0.05, "{kind:?} round-trip loss too high: {}", r.loss);
        }
    }

    #[test]
    fn same_seed_is_bit_identical() {
        let t = target(VoiceKind::Clap, &[280.0, 0.35]);
        let cfg = FitConfig { restarts: 2, seed: 99, sample_rate: SR, frames: t.len() };
        let a = fit(&t, VoiceKind::Clap, &cfg);
        let b = fit(&t, VoiceKind::Clap, &cfg);
        assert_eq!(a.params.len(), b.params.len());
        for (x, y) in a.params.iter().zip(b.params.iter()) {
            assert_eq!(x.to_bits(), y.to_bits());
        }
        assert_eq!(a.loss.to_bits(), b.loss.to_bits());
    }

    #[test]
    fn different_seed_can_change_the_trajectory() {
        let t = target(VoiceKind::Kick, &[70.0, 300.0, 0.5, 3.0]);
        let mk = |seed| FitConfig { restarts: 2, seed, sample_rate: SR, frames: t.len() };
        let a = fit(&t, VoiceKind::Kick, &mk(1));
        let b = fit(&t, VoiceKind::Kick, &mk(2));
        // Both must fit well (the gate holds regardless of seed) — this
        // only asserts seeds are actually wired through.
        assert!(a.loss < 0.05 && b.loss < 0.05);
    }
}
