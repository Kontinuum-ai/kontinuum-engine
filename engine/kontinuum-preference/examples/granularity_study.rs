//! Granularity study + learner-ladder replay (issue #24).
//!
//! Builds the synthetic ground-truth corpus (stable and drifting listeners),
//! replays B1 at coarse/mid/fine granularity, picks the winner by preference
//! separation, then compares the ladder B0 → B1 → B2 (B2 with the gate
//! explicitly opened — the shipped default is off).
//!
//! Run: `cargo run --release -p kontinuum-preference --example granularity_study`

use kontinuum_preference::{
    granularity_study, ladder_comparison, pick_granularity, DnaBand, SynthConfig, SynthWorld,
    TastePriors,
};
use kontinuum_preference::{DriftSpec, GroundTruth, SynthLog};

const N_SIGNALS: usize = 400;
const SEEDS: std::ops::Range<u64> = 1..17;
const HALF_LIFE: f32 = 8.0;

fn truth(palettes: &[u32], grooves: &[u16], density: f32) -> GroundTruth {
    GroundTruth {
        ideal_density: density,
        ideal_brightness: 0.4,
        liked_palettes: palettes.to_vec(),
        liked_grooves: grooves.to_vec(),
    }
}

fn corpus() -> Vec<SynthLog> {
    let mut logs = Vec::new();
    for seed in SEEDS {
        let drifted = seed > SEEDS.end / 2;
        let cfg = SynthConfig {
            seed,
            n_signals: N_SIGNALS,
            palettes: vec![1, 2, 3, 4, 5],
            grooves: vec![0, 1, 2, 3, 4],
            truth: truth(&[1, 3], &[0, 2], 0.75),
            drift: drifted.then(|| DriftSpec {
                at_signal: N_SIGNALS / 2,
                truth: truth(&[2, 4], &[1, 3], 0.25),
            }),
            log_propensity: true,
        };
        logs.push(SynthWorld::new(cfg).generate());
    }
    logs
}

fn dna() -> TastePriors {
    TastePriors {
        bpm: 124.0,
        energy: DnaBand::new(0.3, 0.9).unwrap(),
        density: DnaBand::new(0.2, 0.8).unwrap(),
        darkness: DnaBand::new(0.2, 0.9).unwrap(),
        palettes: vec![1, 2, 3, 4, 5],
        grooves: vec![0, 1, 2, 3, 4],
    }
}

fn main() {
    let logs = corpus();
    let d = dna();
    let signals: usize = logs.iter().map(|l| l.log.signals.len()).sum();
    let states: usize = logs.iter().map(|l| l.log.states.len()).sum();
    println!(
        "corpus: {} logs ({} stable, {} drifting), {states} states, {signals} signals",
        logs.len(),
        logs.len() / 2,
        logs.len() - logs.len() / 2,
    );

    println!("\n== granularity study (B1, half-life {HALF_LIFE}) ==");
    println!("{:<10} {:>12} {:>13} {:>11} {:>10} {:>11}", "level", "liked", "disliked", "separation", "skip", "session");
    let study = granularity_study(&logs, &d, HALF_LIFE).unwrap();
    for r in &study {
        println!(
            "{:<10} {:>12.4} {:>13.4} {:>11.4} {:>10.4} {:>11.4}",
            format!("{:?}", r.granularity).to_lowercase(),
            r.metrics.liked_score,
            r.metrics.disliked_score,
            r.metrics.separation,
            r.metrics.skip_rate_proxy,
            r.metrics.session_length_proxy,
        );
    }
    let winner = pick_granularity(&study);
    println!("winner: {winner:?} (coarsest within 5% of best separation)");

    println!("\n== ladder comparison (at {winner:?}) ==");
    println!("{:<10} {:>12} {:>13} {:>11} {:>10} {:>11} {:>8}", "learner", "liked", "disliked", "separation", "skip", "session", "ips");
    let report = ladder_comparison(&logs, &d, HALF_LIFE, winner, true).unwrap();
    for (name, m) in [("B0", Some(report.b0)), ("B1", Some(report.b1)), ("B2*", report.b2_enabled)] {
        let Some(m) = m else { continue };
        println!(
            "{:<10} {:>12.4} {:>13.4} {:>11.4} {:>10.4} {:>11.4} {:>8}",
            name,
            m.liked_score,
            m.disliked_score,
            m.separation,
            m.skip_rate_proxy,
            m.session_length_proxy,
            m.ips_estimate.map(|v| format!("{v:.3}")).unwrap_or_else(|| "-".into()),
        );
    }
    println!("\nB2* = gate explicitly opened for this experiment; shipped default is off.");
}
