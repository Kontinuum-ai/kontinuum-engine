//! Round-trip property (issue #39 step 4): `ir → text → ir` is identity
//! over a deterministic seeded corpus (200 cases), generated inside the
//! subset the v0 grammar covers, in canonical op order.

use kontinuum_ir::dsl::{compile, render};
use kontinuum_ir::diff::IrDiff;
use kontinuum_ir::schema::{
    EuclideanPattern, EuclideanTag, Pattern, Section, Step, StepsPattern,
};

fn bucket(k: u32, n: u32, rot: i32) -> Vec<bool> {
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
    grid.rotate_left(rot.rem_euclid(n as i32) as usize);
    grid
}

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

fn section(id: &str, bars: u32, energy: f32) -> Section {
    Section {
        id: id.into(),
        bars,
        energy_curve: vec![energy],
        density_curve: Vec::new(),
        brightness_curve: Vec::new(),
        transition_in: None,
        transition_out: None,
        pattern_bindings: Default::default(),
        automation: Default::default(),
    }
}

/// SplitMix64: deterministic, dependency-free.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len() as u64) as usize]
    }
}

/// Generates a covered, canonical diff program: per section — AddSection,
/// patterns, then energy ops; params last. Canonical order is what
/// [`render`] emits, so `compile(render(x)) == x` must hold exactly.
fn gen_program(rng: &mut Rng) -> Vec<IrDiff> {
    const TRACKS: [&str; 4] = ["kick", "hat", "perc", "bass"];
    const ENERGIES: [f32; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];
    const VELS: [f32; 4] = [0.25, 0.5, 0.8, 1.0];
    const PARAMS: [&str; 11] = [
        "tune_hz", "decay_ms", "click", "drive", "tone", "cutoff_hz", "resonance", "glide_ms",
        "attack_ms", "release_ms", "detune_cents",
    ];
    let mut diffs = Vec::new();
    for s in 0..1 + rng.below(3) {
        let id = format!("s{s}");
        diffs.push(IrDiff::AddSection {
            after: None,
            section: section(&id, 1 + rng.below(16) as u32, *rng.pick(&ENERGIES)),
        });
        let mut used: Vec<&str> = Vec::new();
        for _ in 0..rng.below(4) {
            let track = *rng.pick(&TRACKS);
            if used.contains(&track) {
                continue;
            }
            used.push(track);
            match rng.below(3) {
                0 => {
                    let mut active = [false; 16];
                    for slot in active.iter_mut() {
                        *slot = rng.below(2) == 0;
                    }
                    let steps: Vec<Step> = active
                        .iter()
                        .enumerate()
                        .filter(|(_, on)| **on)
                        .map(|(i, _)| step((i * 240) as u32, *rng.pick(&VELS), 0))
                        .collect();
                    diffs.push(IrDiff::ReplacePattern {
                        section: id.clone(),
                        track: track.into(),
                        pattern: Pattern::Steps(StepsPattern { steps, repeats: 1 }),
                    });
                }
                1 => {
                    let n = 1 + rng.below(16) as u32;
                    diffs.push(IrDiff::ReplacePattern {
                        section: id.clone(),
                        track: track.into(),
                        pattern: Pattern::Euclidean(EuclideanPattern {
                            generator: EuclideanTag::Euclidean,
                            k: rng.below(n as u64 + 1) as u32,
                            n,
                            rot: rng.below(17) as i32 - 8,
                            velocity: 0.8,
                            probability: 1.0,
                            repeats: 1,
                            gate: None,
                            pitch: None,
                        }),
                    });
                }
                _ => {
                    let swing_ticks = *rng.pick(&[30i16, 60, 120]);
                    let steps: Vec<Step> = bucket(4, 16, rng.below(16) as i32)
                        .iter()
                        .enumerate()
                        .filter(|(_, on)| **on)
                        .map(|(i, _)| step(
                            (i * 240) as u32,
                            0.8,
                            if i % 2 == 1 { swing_ticks } else { 0 },
                        ))
                        .collect();
                    diffs.push(IrDiff::ReplacePattern {
                        section: id.clone(),
                        track: track.into(),
                        pattern: Pattern::Steps(StepsPattern { steps, repeats: 1 }),
                    });
                }
            }
        }
        for _ in 0..rng.below(3) {
            diffs.push(IrDiff::SetSectionEnergy { id: id.clone(), energy: vec![*rng.pick(&ENERGIES)] });
        }
    }
    for _ in 0..rng.below(4) {
        diffs.push(IrDiff::SetInstrumentParam {
            track: (*rng.pick(&TRACKS)).into(),
            param: (*rng.pick(&PARAMS)).into(),
            value: *rng.pick(&[48.0f32, 120.0, 0.4, 300.0, 9000.0]),
        });
    }
    diffs
}

#[test]
fn round_trip_ir_text_ir_is_identity_over_200_generated_cases() {
    for case in 0..200u64 {
        let mut rng = Rng(0xC0FFEE ^ case);
        let diffs = gen_program(&mut rng);
        let text = render(&diffs)
            .unwrap_or_else(|e| panic!("case {case}: uncovered fragment: {e:?}"));
        let back = compile(&text)
            .unwrap_or_else(|e| panic!("case {case}: generated text must compile: {text} → {e:?}"));
        assert_eq!(back, diffs, "case {case}: round trip drifted\ntext:\n{text}");
    }
}

#[test]
fn generated_corpus_never_rejects_a_case() {
    // Guards against the generator drifting out of the covered subset
    // (which would silently turn the property test into a no-op via the
    // unwrap_or_else above only catching render, not coverage shrinkage).
    let mut rendered = 0;
    for case in 0..50u64 {
        let mut rng = Rng(7 ^ case);
        if render(&gen_program(&mut rng)).is_ok() {
            rendered += 1;
        }
    }
    assert_eq!(rendered, 50, "every generated program must be covered");
}
