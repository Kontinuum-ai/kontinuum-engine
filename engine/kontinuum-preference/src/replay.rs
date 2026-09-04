//! Offline replay harness (#24): deterministic, log-based counterfactual
//! evaluation of learner candidates.
//!
//! Same log → same report, bit for bit: signals replay in file order, all
//! maps are `BTreeMap`s, learners on this path never sample (B1 is pure
//! arithmetic), and every metric is a closed-form function of the log.
//!
//! Metrics are *proxies*, honestly named:
//!
//! - `skip_rate_proxy` — fraction of replayed states the learner's preference
//!   score rates below the skip threshold. A bad learner reads high.
//! - `session_length_proxy` — mean preference score at `SessionLength`
//!   signals; a good learner reads high (0.5 = neutral when absent).
//! - `ips_estimate` — inverse-propensity-score hook for when the director
//!   starts logging its sampling propensities; `None` until then.

use crate::fingerprint::{attribute, StateFingerprint};
use crate::learners::{B0Baseline, B1Aggregator, Learner};
use crate::priors::{LearnerError, SessionPriors, TastePriors};
use crate::signal::{read_jsonl_file, Signal, SignalKind, StoreError};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// A logged musical-state observation with its timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct StateObservation {
    pub ts_ms: i64,
    pub fingerprint: StateFingerprint,
}

/// Log under replay: signals plus the state timeline they fired over.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReplayLog {
    pub signals: Vec<Signal>,
    pub states: Vec<StateObservation>,
}

impl ReplayLog {
    /// Load both JSONL files. A missing file is an empty stream (consistent
    /// with [`crate::signal::SignalStore::load`]).
    pub fn load(signals_path: &Path, states_path: &Path) -> Result<Self, StoreError> {
        Ok(ReplayLog {
            signals: read_jsonl_file(signals_path)?,
            states: read_jsonl_file(states_path)?,
        })
    }
}

/// Preference score at or below which a state counts as a predicted skip.
pub const SKIP_THRESHOLD: f32 = 0.5;

/// Metrics for one learner on one log.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReplayMetrics {
    pub skip_rate_proxy: f32,
    pub session_length_proxy: f32,
    pub ips_estimate: Option<f64>,
}

/// Deterministic B0-vs-B1 report. Serialize this for the CI report artifact.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LearnerComparison {
    pub b0: ReplayMetrics,
    pub b1: ReplayMetrics,
    pub signals_replayed: usize,
}

/// Latest state observation at least one phrase before `ts_ms`.
fn previous_phrase<'a>(
    states: &'a [StateObservation],
    ts_ms: i64,
    phrase_ms: i64,
) -> Option<&'a StateObservation> {
    states.iter().rev().find(|o| o.ts_ms <= ts_ms.saturating_sub(phrase_ms))
}

/// IPS-style estimate over signals that carry a logging propensity: the
/// mean of `reward · 1[learner would keep] / propensity`. `None` when the
/// log has no propensities (v0 logs do not yet).
fn ips_estimate(log: &ReplayLog, priors: &SessionPriors) -> Option<f64> {
    let (mut acc, mut count) = (0.0f64, 0u64);
    for s in &log.signals {
        if let Some(p) = s.context.propensity {
            if p <= 0.0 {
                continue;
            }
            let kept = priors.preference_score(&s.state_fingerprint) >= SKIP_THRESHOLD;
            if kept {
                acc += s.strength as f64 / p as f64;
            }
            count += 1;
        }
    }
    (count > 0).then(|| acc / count as f64)
}

fn mean_session_length_score(log: &ReplayLog, priors: &SessionPriors) -> f32 {
    let (mut sum, mut count) = (0.0f32, 0u32);
    for s in &log.signals {
        if s.kind == SignalKind::SessionLength {
            sum += priors.preference_score(&s.state_fingerprint);
            count += 1;
        }
    }
    if count == 0 { SKIP_THRESHOLD } else { sum / count as f32 }
}

/// Replays candidate learners over recorded logs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReplayHarness {
    /// Phrase length used for t−1 attribution, in ms (~8 bars at 120–125 BPM).
    pub phrase_ms: i64,
}

impl Default for ReplayHarness {
    fn default() -> Self {
        ReplayHarness { phrase_ms: 16_000 }
    }
}

impl ReplayHarness {
    /// Signals that must not train a learner: volume events inside the
    /// route-change debounce window are environment noise, not taste.
    fn learnable(signal: &Signal) -> bool {
        !(signal.kind.requires_debounce() && signal.context.is_route_debounced())
    }

    /// Train-free metrics for a ready learner.
    pub fn metrics(&self, log: &ReplayLog, priors: &SessionPriors) -> ReplayMetrics {
        let (mut skips, mut total) = (0u32, 0u32);
        for obs in &log.states {
            total += 1;
            if priors.preference_score(&obs.fingerprint) < SKIP_THRESHOLD {
                skips += 1;
            }
        }
        ReplayMetrics {
            skip_rate_proxy: if total == 0 { 0.0 } else { skips as f32 / total as f32 },
            session_length_proxy: mean_session_length_score(log, priors),
            ips_estimate: ips_estimate(log, priors),
        }
    }

    /// Train `learner` on the log and return its sanitized priors — the
    /// exact path `run` uses, exposed for the granularity study so candidate
    /// learners are always trained identically.
    pub fn train_and_priors(
        &self,
        log: &ReplayLog,
        learner: &mut dyn Learner,
        dna: &TastePriors,
    ) -> Result<SessionPriors, LearnerError> {
        for signal in &log.signals {
            if !Self::learnable(signal) {
                continue;
            }
            let previous = previous_phrase(&log.states, signal.ts_ms, self.phrase_ms)
                .map(|o| o.fingerprint);
            let attributed = attribute(signal, previous).fingerprints();
            learner.observe(&attributed, signal);
        }
        Ok(learner.priors(dna)?.sanitize(dna))
    }

    /// Full run: train `learner` on the log's signals, then score it.
    /// Learner output passes through [`SessionPriors::sanitize`] — the
    /// guardrail boundary every candidate must survive.
    pub fn run(
        &self,
        log: &ReplayLog,
        learner: &mut dyn Learner,
        dna: &TastePriors,
    ) -> Result<ReplayMetrics, LearnerError> {
        let priors = self.train_and_priors(log, learner, dna)?;
        Ok(self.metrics(log, &priors))
    }

    /// The standard report: B0 control vs B1 aggregation on the same log.
    pub fn compare(&self, log: &ReplayLog, dna: &TastePriors) -> Result<LearnerComparison, LearnerError> {
        let mut b0 = B0Baseline;
        let mut b1 = B1Aggregator::default();
        Ok(LearnerComparison {
            b0: self.run(log, &mut b0, dna)?,
            b1: self.run(log, &mut b1, dna)?,
            signals_replayed: log.signals.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fingerprint::{Granularity, MusicalState, SectionKind};
    use crate::priors::{DnaBand, TastePriors};
    use crate::learners::XorShift;
    use crate::signal::SignalContext;

    fn state(density: f32, palette: u32) -> MusicalState {
        MusicalState {
            section_kind: SectionKind::Peak,
            energy: 0.7,
            density,
            brightness: 0.5,
            bpm: 124.0,
            palette_id: palette,
            groove_template: 1,
            bass_archetype: 2,
            dominant_sample_classes: [5, 0, 0, 0],
        }
    }

    fn dna() -> TastePriors {
        TastePriors {
            bpm: 124.0,
            energy: DnaBand::new(0.3, 0.9).unwrap(),
            density: DnaBand::new(0.2, 0.8).unwrap(),
            darkness: DnaBand::new(0.2, 0.9).unwrap(),
            palettes: vec![1, 2],
            grooves: vec![0, 1],
        }
    }

    /// Planted-preference log: dense/palette-1 states get bookmarked and
    /// listened through; sparse/palette-2 states get skipped. Includes a
    /// t−1 phrase pair and one propensity-tagged signal for the IPS hook.
    fn planted_log() -> ReplayLog {
        let mut log = ReplayLog::default();
        let mut ts = 10_000i64;
        for i in 0..10u64 {
            let liked = i % 2 == 0;
            let fp = if liked {
                state(0.95, 1).fingerprint(Granularity::Fine)
            } else {
                state(0.05, 2).fingerprint(Granularity::Fine)
            };
            log.states.push(StateObservation { ts_ms: ts, fingerprint: fp });
            log.states.push(StateObservation {
                ts_ms: ts + 1,
                fingerprint: state(0.9, 1).fingerprint(Granularity::Fine),
            });
            let kind = if liked {
                if i == 0 { SignalKind::SessionLength } else { SignalKind::Bookmark }
            } else {
                SignalKind::Skip
            };
            let mut s = Signal::new(ts + 5_000, kind, fp);
            if i == 4 {
                s.context = SignalContext { propensity: Some(0.5), ..Default::default() };
            }
            log.signals.push(s);
            ts += 60_000;
        }
        log
    }

    #[test]
    fn debounced_volume_signals_are_not_trained_on() {
        let mut log = ReplayLog::default();
        // Volume-down inside the route-change window (noise) vs outside
        // (taste), both on sparse states a learner should learn to skip.
        let debounced = Signal::new(1_000, SignalKind::VolumeDown, state(0.05, 1).fingerprint(Granularity::Mid))
            .with_context(SignalContext { since_route_change_ms: Some(5_000), ..Default::default() });
        let real = Signal::new(2_000, SignalKind::VolumeDown, state(0.05, 1).fingerprint(Granularity::Mid))
            .with_context(SignalContext { since_route_change_ms: Some(120_000), ..Default::default() });
        log.signals = vec![debounced.clone(), real.clone()];
        log.states = vec![
            StateObservation { ts_ms: 1_000, fingerprint: debounced.state_fingerprint },
            StateObservation { ts_ms: 2_000, fingerprint: real.state_fingerprint },
        ];
        // Only the non-debounced signal may train: one volume-down at the
        // Mid grid's lowest density bucket gives scores[0] a small negative.
        let d = dna();
        let mut b1 = B1Aggregator::default();
        let metrics = ReplayHarness::default().run(&log, &mut b1, &d).unwrap();
        let scores = b1.density_scores();
        let negatives = scores.iter().filter(|s| **s < 0.0).count();
        assert_eq!(negatives, 1, "only the in-window volume signal must train: {scores:?}");
        assert!(metrics.skip_rate_proxy.is_finite());
        assert!(!ReplayHarness::learnable(&debounced));
        assert!(ReplayHarness::learnable(&real));
    }

    #[test]
    fn replay_is_deterministic_same_log_same_report() {
        let log = planted_log();
        let d = dna();
        let a = ReplayHarness::default().compare(&log, &d).unwrap();
        let b = ReplayHarness::default().compare(&log, &d).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.signals_replayed, log.signals.len());
    }

    #[test]
    fn b1_beats_b0_on_planted_log() {
        let log = planted_log();
        let report = ReplayHarness::default().compare(&log, &dna()).unwrap();
        assert!(
            report.b1.skip_rate_proxy < report.b0.skip_rate_proxy,
            "b1 skip rate {} not below b0 {}",
            report.b1.skip_rate_proxy,
            report.b0.skip_rate_proxy
        );
        assert!(
            report.b1.session_length_proxy > report.b0.session_length_proxy,
            "b1 session proxy {} not above b0 {}",
            report.b1.session_length_proxy,
            report.b0.session_length_proxy
        );
        assert!(report.b1.skip_rate_proxy > 0.0, "sparse disliked states should still predict skip");
    }

    #[test]
    fn ips_hook_returns_none_without_propensity_and_some_with() {
        let mut log = planted_log();
        let d = dna();
        let priors = SessionPriors::neutral(&d);
        // Strip the planted propensity before asserting the None case.
        log.signals[4].context.propensity = None;
        assert_eq!(ips_estimate(&log, &priors), None);
        log.signals[2].context.propensity = Some(0.25);
        log.signals[2].strength = 1.0;
        // Neutral priors must rate this mid-density state as kept.
        log.signals[2].state_fingerprint = state(0.5, 1).fingerprint(Granularity::Mid);
        let ips = ips_estimate(&log, &priors).unwrap();
        assert!(ips.is_finite());
        // reward 1.0 / propensity 0.25 = 4.0 exactly (single tagged signal).
        assert!((ips - 4.0).abs() < 1e-9, "ips {ips}");
    }

    #[test]
    fn log_jsonl_roundtrip_and_previous_phrase_lookup() {
        let dir = std::env::temp_dir().join(format!("kpref-replay-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = planted_log();
        let sp = dir.join("signals.jsonl");
        let st = dir.join("states.jsonl");
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&sp).unwrap();
            for s in &log.signals {
                writeln!(f, "{}", serde_json::to_string(s).unwrap()).unwrap();
            }
            let mut f = std::fs::File::create(&st).unwrap();
            for o in &log.states {
                writeln!(f, "{}", serde_json::to_string(o).unwrap()).unwrap();
            }
        }
        let loaded = ReplayLog::load(&sp, &st).unwrap();
        assert_eq!(loaded, log);
        // Latest state ≥1 phrase (16 s) before the first signal at 15 s → none.
        assert!(previous_phrase(&loaded.states, 15_000, 16_000).is_none());
        // Second signal at 75 s → latest state ≥1 phrase earlier is 10_001
        // (the duplicate current-state observation of group 0).
        assert_eq!(previous_phrase(&loaded.states, 75_000, 16_000).unwrap().ts_ms, 10_001);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn synthetic_replay_feeds_guardrail_bounded_learner() {
        let d = dna();
        let mut rng = XorShift::from_seed(99);
        let mut log = ReplayLog::default();
        for i in 0..50u64 {
            let fp = state(rng.next_f32(), d.palettes[rng.below(2) as usize])
                .fingerprint(Granularity::Mid);
            log.states.push(StateObservation { ts_ms: i as i64 * 1_000, fingerprint: fp });
            let kind = if rng.chance(0.5) { SignalKind::Bookmark } else { SignalKind::Skip };
            log.signals.push(Signal::new(i as i64 * 1_000 + 500, kind, fp));
        }
        let report = ReplayHarness::default().compare(&log, &d).unwrap();
        assert_eq!(report.signals_replayed, 50);
        assert!(report.b1.skip_rate_proxy >= 0.0 && report.b1.skip_rate_proxy <= 1.0);
        assert!(report.b0.session_length_proxy > 0.0 && report.b0.session_length_proxy <= 1.0);
    }
}
