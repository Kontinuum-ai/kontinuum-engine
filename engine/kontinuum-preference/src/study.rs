//! Offline experiments (#24): the granularity study and the learner-ladder
//! comparison, run over [`crate::synth`] corpora with planted ground truth.
//!
//! Metrics, honestly named:
//!
//! - `liked_score` / `disliked_score` — mean predicted preference score over
//!   ground-truth-liked / -disliked states. A perfect learner reads 1 / 0.
//! - `separation` — liked − disliked; the study's pick criterion (higher is
//!   better: it measures whether learned scores transfer across states at a
//!   given granularity, the exact failure mode of too-fine and too-coarse).
//! - `skip_rate_proxy` / `session_length_proxy` — the harness proxies.
//!
//! One learner per log (one log ≈ one user), averaged over the corpus.

use crate::fingerprint::Granularity;
use crate::learners::{
    B0Baseline, B1Aggregator, B1Config, B2Bandit, B2Config, B2_DEFAULT_ALPHA,
    B2_EXPLORATION_FLOOR, Learner,
};
use crate::priors::{LearnerError, TastePriors};
use crate::replay::{ReplayHarness, ReplayMetrics};
use crate::signal::SignalKind;
use crate::synth::SynthLog;
use serde::{Deserialize, Serialize};

/// Metrics for one learner configuration, averaged over a corpus.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CorpusMetrics {
    pub liked_score: f32,
    pub disliked_score: f32,
    pub separation: f32,
    pub skip_rate_proxy: f32,
    pub session_length_proxy: f32,
    pub ips_estimate: Option<f64>,
    pub logs: usize,
}

/// One rung of the granularity study.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GranularityResult {
    pub granularity: Granularity,
    #[serde(flatten)]
    pub metrics: CorpusMetrics,
}

/// The ladder comparison report (B2 optional — it is gated off by default).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LadderReport {
    pub b0: CorpusMetrics,
    pub b1: CorpusMetrics,
    pub b2_enabled: Option<CorpusMetrics>,
}

fn label_scores(corpus: &[SynthLog], priors_per_log: &[crate::priors::SessionPriors]) -> (f32, f32) {
    let (mut liked_sum, mut disliked_sum, mut liked_n, mut disliked_n) = (0.0f32, 0.0f32, 0u32, 0u32);
    for (log, priors) in corpus.iter().zip(priors_per_log.iter()) {
        for (obs, &liked) in log.log.states.iter().zip(log.liked.iter()) {
            let score = priors.preference_score(&obs.fingerprint);
            if liked {
                liked_sum += score;
                liked_n += 1;
            } else {
                disliked_sum += score;
                disliked_n += 1;
            }
        }
    }
    (
        if liked_n == 0 { 0.5 } else { liked_sum / liked_n as f32 },
        if disliked_n == 0 { 0.5 } else { disliked_sum / disliked_n as f32 },
    )
}

fn replay_with(
    corpus: &[SynthLog],
    dna: &TastePriors,
    mut new_learner: impl FnMut() -> Box<dyn Learner>,
) -> Result<CorpusMetrics, LearnerError> {
    let harness = ReplayHarness::default();
    let (mut liked, mut skip, mut session, mut ips) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for log in corpus {
        let mut learner = new_learner();
        let priors = harness.train_and_priors(&log.log, learner.as_mut(), dna)?;
        let metrics: ReplayMetrics = harness.metrics(&log.log, &priors);
        skip.push(metrics.skip_rate_proxy);
        session.push(metrics.session_length_proxy);
        if let Some(v) = metrics.ips_estimate {
            ips.push(v);
        }
        liked.push(priors);
    }
    let (liked_score, disliked_score) = label_scores(corpus, &liked);
    let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len().max(1) as f32;
    Ok(CorpusMetrics {
        liked_score,
        disliked_score,
        separation: liked_score - disliked_score,
        skip_rate_proxy: mean(&skip),
        session_length_proxy: mean(&session),
        ips_estimate: if ips.is_empty() { None } else { Some(ips.iter().sum::<f64>() / ips.len() as f64) },
        logs: corpus.len(),
    })
}

/// Replay B1 at every granularity over the same corpus; the winner is the
/// highest separation (ties broken by lower skip-rate proxy).
pub fn granularity_study(
    corpus: &[SynthLog],
    dna: &TastePriors,
    half_life_updates: f32,
) -> Result<Vec<GranularityResult>, LearnerError> {
    let mut out = Vec::new();
    for g in [Granularity::Coarse, Granularity::Mid, Granularity::Fine] {
        let metrics = replay_with(corpus, dna, || {
            Box::new(B1Aggregator::new(B1Config { half_life_updates, granularity: g }))
        })?;
        out.push(GranularityResult { granularity: g, metrics });
    }
    Ok(out)
}

/// Pick the winning granularity: the *coarsest* level whose separation is
/// within 5% of the best. Parsimony tie-break — at equal measured
/// separation, fewer keyed dimensions generalize better to sparse real
/// logs, so a marginal fine-level edge (seed noise) must not buy complexity.
pub fn pick_granularity(results: &[GranularityResult]) -> Granularity {
    let best = results
        .iter()
        .map(|r| r.metrics.separation)
        .fold(f32::MIN, f32::max);
    results
        .iter()
        .filter(|r| r.metrics.separation >= best * 0.95)
        .min_by_key(|r| r.granularity)
        .map(|r| r.granularity)
        .unwrap_or(Granularity::Mid)
}

/// The standard report: B0 control vs B1 aggregation, plus B2 with the gate
/// explicitly opened when `run_b2` is set (shipping default is off).
pub fn ladder_comparison(
    corpus: &[SynthLog],
    dna: &TastePriors,
    half_life_updates: f32,
    granularity: Granularity,
    run_b2: bool,
) -> Result<LadderReport, LearnerError> {
    let b0 = replay_with(corpus, dna, || Box::new(B0Baseline))?;
    let b1 = replay_with(corpus, dna, || {
        Box::new(B1Aggregator::new(B1Config { half_life_updates, granularity }))
    })?;
    let b2_enabled = run_b2.then(|| {
        replay_with(corpus, dna, || {
            Box::new(B2Bandit::new(
                B2Config::new(true, B2_DEFAULT_ALPHA, B2_EXPLORATION_FLOOR, 0x5EED_B2B2).unwrap(),
            ))
        })
        .unwrap()
    });
    Ok(LadderReport { b0, b1, b2_enabled })
}

/// Whether any signal in the corpus is a reaction a learner trains on (the
/// context-only kinds are excluded everywhere by the valence contract).
pub fn has_reactions(corpus: &[SynthLog]) -> bool {
    corpus.iter().any(|c| {
        c.log
            .signals
            .iter()
            .any(|s| !matches!(s.kind, SignalKind::TimeOfDay | SignalKind::StatedMood))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::priors::{DnaBand, TastePriors};
    use crate::synth::{DriftSpec, GroundTruth, SynthConfig, SynthWorld};

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

    fn truth(palettes: &[u32], grooves: &[u16], density: f32) -> GroundTruth {
        GroundTruth {
            ideal_density: density,
            ideal_brightness: 0.4,
            liked_palettes: palettes.to_vec(),
            liked_grooves: grooves.to_vec(),
        }
    }

    /// Small stable + drifting corpus, shared shape with the example binary.
    fn corpus(n_signals: usize) -> Vec<SynthLog> {
        let mut logs = Vec::new();
        for seed in 1..=4u64 {
            let cfg = SynthConfig {
                seed,
                n_signals,
                palettes: vec![1, 2, 3, 4, 5],
                grooves: vec![0, 1, 2, 3, 4],
                truth: truth(&[1, 3], &[0, 2], 0.75),
                drift: None,
                log_propensity: true,
            };
            logs.push(SynthWorld::new(cfg).generate());
        }
        for seed in 5..=8u64 {
            let cfg = SynthConfig {
                seed,
                n_signals,
                palettes: vec![1, 2, 3, 4, 5],
                grooves: vec![0, 1, 2, 3, 4],
                truth: truth(&[1, 3], &[0, 2], 0.75),
                drift: Some(DriftSpec {
                    at_signal: n_signals / 2,
                    truth: truth(&[2, 4], &[1, 3], 0.25),
                }),
                log_propensity: true,
            };
            logs.push(SynthWorld::new(cfg).generate());
        }
        logs
    }

    #[test]
    fn granularity_study_ranks_separation_and_is_deterministic() {
        let logs = corpus(150);
        let d = dna();
        let a = granularity_study(&logs, &d, 8.0).unwrap();
        let b = granularity_study(&logs, &d, 8.0).unwrap();
        assert_eq!(a, b);
        for r in &a {
            assert!(r.metrics.liked_score >= r.metrics.disliked_score - 0.05,
                "{:?}: separation negative {:?}", r.granularity, r.metrics);
        }
        assert!(has_reactions(&logs));
    }

    #[test]
    fn b1_beats_b0_on_the_ground_truth_corpus() {
        let logs = corpus(150);
        let d = dna();
        let study = granularity_study(&logs, &d, 8.0).unwrap();
        let winner = pick_granularity(&study);
        let report = ladder_comparison(&logs, &d, 8.0, winner, false).unwrap();
        assert!(
            report.b1.separation > report.b0.separation,
            "b1 separation {} not above b0 {}",
            report.b1.separation,
            report.b0.separation
        );
        assert!(
            report.b1.skip_rate_proxy < report.b0.skip_rate_proxy,
            "b1 skip proxy {} not below b0 {}",
            report.b1.skip_rate_proxy,
            report.b0.skip_rate_proxy
        );
        assert!(report.b2_enabled.is_none(), "b2 stays gated off in the default report");
    }
}
