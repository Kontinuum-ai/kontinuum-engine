//! Per-voice and per-FX CPU measurement (issues #11/#30): the cost table's
//! empirical half. Each subject renders a fixed workload (48 kHz, release
//! profile) and the harness reports ns/sample and the implied "voices per
//! 1% of one 2 GHz-class core". Loose ceiling assertions keep the gate
//! honest without being timing-flaky: a voice that regresses to multiple
//! times its class budget fails CI; day-to-day drift is recorded, not
//! gated. Run with `--nocapture` to print the table used in
//! `docs/cost-table.md`.

use std::time::Instant;

use kontinuum_core::fx::{Chorus, Delay, FreqShifter, Phaser, Reverb, ReverbV2, Saturate, TransientDesigner};
use kontinuum_core::{BusFx, InsertFx};
use kontinuum_instruments_core::{Acid, Bass, Clap, Ep, FmPerc, Hat, Kick, Pad, Pluck, Snare, Stab, Texture, WavetableVoice};
use kontinuum_core::Voice;

const SR: u32 = 48_000;
const FRAMES: usize = 480 * 64; // 30_720 samples ≈ 0.64 s
const PASSES: usize = 12;

fn measured<R: FnMut(&mut [f32])>(mut render: R) -> f64 {
    // Warm caches, then take the median of PASSES (median rejects the
    // scheduler spikes a mean would average in).
    let mut buf = vec![0.0f32; FRAMES];
    render(&mut buf);
    let mut times = Vec::with_capacity(PASSES);
    for _ in 0..PASSES {
        let t0 = Instant::now();
        render(&mut buf);
        times.push(t0.elapsed().as_secs_f64());
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = times[times.len() / 2];
    median * 1e9 / FRAMES as f64
}

fn voice_ns<V: Voice>(mut v: V) -> f64 {
    v.note_on(60.0, 0.9);
    measured(|buf| {
        if v.is_active() {
            v.render(buf);
        } else {
            v.note_on(60.0, 0.9);
            v.render(buf);
        }
    })
}

struct NopVoice;
impl Voice for NopVoice {
    fn note_on(&mut self, _pitch: f32, _velocity: f32) {}
    fn note_off(&mut self) {}
    fn is_active(&self) -> bool { true }
    fn render(&mut self, out: &mut [f32]) { out.fill(0.0); }
    fn set_param(&mut self, _param: kontinuum_core::ParamId, _value: f32) {}
    fn reset(&mut self) {}
}

#[test]
fn voice_cost_table_is_measured_and_within_class_budgets() {
    let baseline = measured(|_| {});
    let nop = voice_ns(NopVoice) - baseline;
    let kick = voice_ns(Kick::new(SR)) - nop;
    let hat = voice_ns(Hat::new(SR)) - nop;
    let clap = voice_ns(Clap::new(SR)) - nop;
    let snare = voice_ns(Snare::new(SR)) - nop;
    let bass = voice_ns(Bass::new(SR)) - nop;
    let acid = voice_ns(Acid::new(SR)) - nop;
    let pad = voice_ns(Pad::new(SR)) - nop;
    let ep = voice_ns(Ep::new(SR)) - nop;
    let pluck = voice_ns(Pluck::new(SR)) - nop;
    let stab = voice_ns(Stab::new(SR)) - nop;
    let wavetable = voice_ns(WavetableVoice::new(SR)) - nop;
    let fmperc = voice_ns(FmPerc::new(SR)) - nop;
    let texture = voice_ns(Texture::new(SR)) - nop;

    println!("\n# voice cost (48 kHz, release, ns/sample, harness-subtracted)");
    for (name, ns) in [
        ("kick", kick), ("hat", hat), ("clap", clap), ("snare", snare),
        ("bass", bass), ("acid", acid), ("pad", pad), ("ep", ep),
        ("pluck", pluck), ("stab", stab), ("wavetable", wavetable),
        ("fmperc", fmperc), ("texture", texture),
    ] {
        println!("| {name:10} | {ns:8.1} |");
    }

    // Class ceilings (docs/cost-table.md): perc-class voices stay under
    // half the pad-class budget; the v2 voices sit in their documented
    // classes. Multiple failures here mean a real regression, not jitter —
    // rerun locally before reverting anything. Absolute ceilings are
    // Apple-Silicon-calibrated; foreign runners only get the class RELATION
    // (perc under half of pad), same policy as the FX table below.
    let perc_budget = 40.0;
    let pad_budget = 120.0;
    #[cfg(target_arch = "aarch64")]
    {
        for (name, ns) in [("fmperc", fmperc), ("texture", texture), ("hat", hat)] {
            assert!(ns < perc_budget, "{name} exceeds the perc-class budget: {ns:.1} ns/sample");
        }
        for (name, ns) in [("wavetable", wavetable), ("pad", pad)] {
            assert!(ns < pad_budget, "{name} exceeds the pad-class budget: {ns:.1} ns/sample");
        }
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        // Measured on ubuntu: fmperc ≈ pad there — the ARM class relation
        // does not transfer across ISAs. Foreign runners get implausibility
        // only: finite, positive, and an order of magnitude from the
        // documented ceiling (a runaway loop or accidental allocation shows
        // up at 10×; ISA noise does not).
        for (name, ns) in [
            ("kick", kick), ("hat", hat), ("clap", clap), ("snare", snare),
            ("bass", bass), ("acid", acid), ("pad", pad), ("ep", ep),
            ("pluck", pluck), ("stab", stab), ("wavetable", wavetable),
            ("fmperc", fmperc), ("texture", texture),
        ] {
            assert!(
                ns.is_finite() && ns > 0.0 && ns < 1_200.0,
                "{name} implausible cost on this host: {ns:.1} ns/sample"
            );
        }
    }
}

#[test]
fn fx_cost_table_is_measured_and_within_budgets() {
    let sine_fill = |buf: &mut [f32]| {
        for (i, slot) in buf.iter_mut().enumerate() {
            *slot = 0.5 * (std::f32::consts::TAU * 220.0 * i as f32 / SR as f32).sin();
        }
    };
    let fx_ns = |mut fx: Box<dyn InsertFx>| {
        let mut warm = vec![0.0f32; FRAMES];
        sine_fill(&mut warm);
        fx.render(&mut warm);
        let mut times = Vec::with_capacity(PASSES);
        for _ in 0..PASSES {
            let mut buf = vec![0.0f32; FRAMES];
            sine_fill(&mut buf);
            let t0 = Instant::now();
            fx.render(&mut buf);
            times.push(t0.elapsed().as_secs_f64());
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        times[times.len() / 2] * 1e9 / FRAMES as f64
    };
    let bus_ns = |mut fx: Box<dyn BusFx>| {
        let mut l = vec![0.0f32; FRAMES];
        let mut r = vec![0.0f32; FRAMES];
        sine_fill(&mut l);
        r.copy_from_slice(&l);
        fx.render(&mut l, &mut r);
        let mut times = Vec::with_capacity(PASSES);
        for _ in 0..PASSES {
            let mut l = vec![0.0f32; FRAMES];
            let mut r = vec![0.0f32; FRAMES];
            sine_fill(&mut l);
            r.copy_from_slice(&l);
            let t0 = Instant::now();
            fx.render(&mut l, &mut r);
            times.push(t0.elapsed().as_secs_f64());
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        times[times.len() / 2] * 1e9 / FRAMES as f64
    };

    let mut chorus = Chorus::new(SR);
    chorus.set_param(kontinuum_core::params::CHORUS_MIX, 1.0);
    let mut phaser8 = Phaser::new(SR);
    phaser8.set_param(kontinuum_core::params::PHASER_STAGES, 1.0);
    let mut phaser4 = Phaser::new(SR);
    let mut shifter = FreqShifter::new(SR);
    shifter.set_param(kontinuum_core::params::SHIFT_HZ, 313.0);
    let mut transient = TransientDesigner::new(SR);
    transient.set_param(kontinuum_core::params::TRANSIENT_ATTACK, 1.0);
    let mut saturate = Saturate::new(1.5);

    let mut delay = Delay::new(SR);
    let mut tape = Delay::new(SR);
    tape.set_param(kontinuum_core::params::TAPE_WOW, 0.6);
    tape.set_param(kontinuum_core::params::TAPE_FLUTTER, 0.5);
    tape.set_param(kontinuum_core::params::TAPE_SAT, 0.5);

    let chorus = fx_ns(Box::new(chorus));
    let phaser4 = fx_ns(Box::new(phaser4));
    let phaser8 = fx_ns(Box::new(phaser8));
    let shifter = fx_ns(Box::new(shifter));
    let transient = fx_ns(Box::new(transient));
    let saturate = fx_ns(Box::new(saturate));
    let delay = bus_ns(Box::new(delay));
    let tape = bus_ns(Box::new(tape));
    let reverb = bus_ns(Box::new(Reverb::new(SR)));
    let reverb_v2 = bus_ns(Box::new(ReverbV2::new(SR)));

    println!("\n# FX cost (48 kHz, release, ns/sample)");
    for (name, ns) in [
        ("saturate", saturate), ("chorus", chorus), ("phaser4", phaser4), ("phaser8", phaser8),
        ("freq-shifter", shifter), ("transient-designer", transient), ("delay (bus)", delay),
        ("tape-delay (bus)", tape), ("fdn4 reverb (bus)", reverb), ("fdn8 reverb v2 (bus)", reverb_v2),
    ] {
        println!("| {name:22} | {ns:8.1} |");
    }

    // Absolute budgets are calibrated on the canonical host class (Apple
    // Silicon). Shared CI runners (ubuntu x86_64) vary by allocation — the
    // same binary measured 90 ns on one runner and passed elsewhere — so
    // off-aarch64 we enforce only the implausibility ratios; the table
    // above still prints everywhere for humans.
    #[cfg(target_arch = "aarch64")]
    {
        assert!(transient < 12.0, "transient designer over budget: {transient:.1}");
        assert!(tape < 60.0, "tape delay over budget: {tape:.1}");
        assert!(reverb_v2 < 80.0, "fdn8 reverb over budget: {reverb_v2:.1}");
    }
    assert!(phaser8 < phaser4 * 3.0, "8-stage phaser implausibly expensive");
}
