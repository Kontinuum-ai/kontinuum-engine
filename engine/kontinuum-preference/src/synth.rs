//! Synthetic-but-realistic replay logs (#24): the ground-truth corpus the
//! learner ladder must prove itself against before any real dogfood logs
//! exist. A planted [`GroundTruth`] utility function decides how a listener
//! reacts to each state, so replay metrics have known truth to hit:
//!
//! - high-utility states → listen-throughs, bookmarks, session end
//! - low-utility states → skips
//! - reaction noise and (optional) mid-log taste drift included
//!
//! Everything is `XorShift`-seeded and bit-reproducible: same config → same
//! corpus. States are drawn uniformly over the vocabulary, so logged
//! propensities are honest uniforms for the IPS hook.

use crate::fingerprint::{Granularity, MusicalState, SectionKind};
use crate::learners::XorShift;
use crate::replay::{ReplayLog, StateObservation};
use crate::signal::{Signal, SignalContext, SignalKind};

/// The planted listener: a utility in [0, 1] over musical states. Replay
/// metrics are judged against this function, not against the noisy signals.
#[derive(Clone, Debug, PartialEq)]
pub struct GroundTruth {
    /// Ideal event density, 0..=1.
    pub ideal_density: f32,
    /// Ideal brightness, 0..=1.
    pub ideal_brightness: f32,
    /// Palettes this listener enjoys.
    pub liked_palettes: Vec<u32>,
    /// Groove templates this listener enjoys.
    pub liked_grooves: Vec<u16>,
}

impl GroundTruth {
    /// Weighted closeness/membership score in [0, 1]. Weights are fixed and
    /// documented here so ground truth is never silently retuned:
    /// density 0.35, palette 0.25, groove 0.20, brightness 0.20.
    pub fn utility(&self, s: &MusicalState) -> f32 {
        let density = 1.0 - (s.density - self.ideal_density).abs();
        let brightness = 1.0 - (s.brightness - self.ideal_brightness).abs();
        let palette = if self.liked_palettes.contains(&s.palette_id) { 1.0 } else { 0.0 };
        let groove = if self.liked_grooves.contains(&s.groove_template) { 1.0 } else { 0.0 };
        0.35 * density + 0.25 * palette + 0.20 * groove + 0.20 * brightness
    }
}

/// Mid-log taste drift: from signal `at_signal` on, reactions follow `truth`.
/// Exponentially-weighted learners must track this; a static aggregation
/// must not.
#[derive(Clone, Debug, PartialEq)]
pub struct DriftSpec {
    pub at_signal: usize,
    pub truth: GroundTruth,
}

/// Corpus configuration. `n_signals` counts emitted reactions; states without
/// a reaction also enter the timeline (they still count for skip-rate proxies).
#[derive(Clone, Debug, PartialEq)]
pub struct SynthConfig {
    pub seed: u64,
    pub n_signals: usize,
    /// Palette id vocabulary the director draws from.
    pub palettes: Vec<u32>,
    /// Groove template vocabulary.
    pub grooves: Vec<u16>,
    pub truth: GroundTruth,
    pub drift: Option<DriftSpec>,
    /// Tag every signal with the uniform logging propensity (1/|palettes|).
    pub log_propensity: bool,
}

/// A generated corpus plus its ground-truth labels: `liked[i]` is the truth
/// verdict (utility ≥ 0.5) for `log.states[i]`.
#[derive(Clone, Debug, PartialEq)]
pub struct SynthLog {
    pub log: ReplayLog,
    pub liked: Vec<bool>,
}

/// Reaction emission probabilities, fixed and documented: skip probability
/// rises quadratically as utility falls; bookmarks only on strong matches.
const SKIP_BASE: f32 = 0.55;
const BOOKMARK_BASE: f32 = 0.35;
const LISTEN_BASE: f32 = 0.40;
const SESSION_LENGTH_P: f32 = 0.05;
const CONTEXT_EVERY: usize = 25;

/// Seeded synthetic-world generator. One world produces one corpus.
pub struct SynthWorld {
    cfg: SynthConfig,
    rng: XorShift,
}

impl SynthWorld {
    pub fn new(cfg: SynthConfig) -> Self {
        let rng = XorShift::from_seed(cfg.seed);
        SynthWorld { cfg, rng }
    }

    fn draw_state(&mut self, phrase_idx: usize) -> MusicalState {
        let section = [
            SectionKind::Intro,
            SectionKind::Build,
            SectionKind::Peak,
            SectionKind::Break,
            SectionKind::Body,
            SectionKind::Outro,
        ][phrase_idx % 6];
        let classes = [
            (self.rng.below(12) + 1) as u8,
            (self.rng.below(12) + 1) as u8,
            0,
            0,
        ];
        let pi = self.rng.below(self.cfg.palettes.len() as u64) as usize;
        let gi = self.rng.below(self.cfg.grooves.len() as u64) as usize;
        MusicalState {
            section_kind: section,
            energy: 0.3 + 0.6 * self.rng.next_f32(),
            density: self.rng.next_f32(),
            brightness: self.rng.next_f32(),
            bpm: 120.0 + 10.0 * self.rng.next_f32(),
            palette_id: self.cfg.palettes[pi],
            groove_template: self.cfg.grooves[gi],
            bass_archetype: self.rng.below(6) as u16,
            dominant_sample_classes: classes,
        }
    }

    /// Which reaction (if any) the planted listener has to this state.
    fn react(&mut self, utility: f32) -> Option<SignalKind> {
        if self.rng.chance(SKIP_BASE * (1.0 - utility).powi(2)) {
            return Some(SignalKind::Skip);
        }
        if self.rng.chance(BOOKMARK_BASE * utility.powi(3)) {
            return Some(SignalKind::Bookmark);
        }
        if self.rng.chance(LISTEN_BASE * utility) {
            return Some(SignalKind::ListenThroughSection);
        }
        if utility > 0.75 && self.rng.chance(SESSION_LENGTH_P) {
            return Some(SignalKind::SessionLength);
        }
        None
    }

    /// Generate the corpus. Iteration is bounded so a pathological config
    /// cannot spin forever; the count of emitted signals is returned so
    /// callers can assert the corpus is full-sized.
    pub fn generate(&mut self) -> SynthLog {
        let mut log = ReplayLog::default();
        let mut liked = Vec::new();
        let mut emitted = 0usize;
        let mut ts = 10_000i64;
        let max_iters = self.cfg.n_signals.max(1) * 40 + 1_000;
        for phrase_idx in 0..max_iters {
            if emitted >= self.cfg.n_signals {
                break;
            }
            let drifted = matches!(&self.cfg.drift, Some(d) if emitted >= d.at_signal);
            let state = self.draw_state(phrase_idx);
            let active_truth = if drifted {
                self.cfg.drift.as_ref().map(|d| d.truth.clone()).unwrap_or_else(|| self.cfg.truth.clone())
            } else {
                self.cfg.truth.clone()
            };
            let utility = active_truth.utility(&state);
            log.states.push(StateObservation {
                ts_ms: ts,
                fingerprint: state.fingerprint(Granularity::Fine),
            });
            liked.push(utility >= 0.5);
            if let Some(kind) = self.react(utility) {
                let mut signal = Signal::new(
                    ts + 5_000,
                    kind,
                    state.fingerprint(Granularity::Fine),
                );
                signal.context = self.context_for(emitted);
                if self.cfg.log_propensity {
                    signal.context.propensity = Some(1.0 / self.cfg.palettes.len() as f32);
                }
                log.signals.push(signal);
                emitted += 1;
            }
            if emitted > 0 && emitted % CONTEXT_EVERY == 0 {
                let (kind, ctx) = self.context_signal(emitted);
                log.signals.push(Signal::new(ts + 6_000, kind, state.fingerprint(Granularity::Fine))
                    .with_context(ctx));
            }
            ts += 16_000 + (self.rng.below(8_000) as i64);
        }
        SynthLog { log, liked }
    }

    fn context_for(&mut self, emitted: usize) -> SignalContext {
        let hour = ((emitted * 3) % 24) as u8;
        let mood = if emitted % 3 == 0 { Some("late night") } else if emitted % 3 == 1 { Some("focused") } else { None };
        SignalContext {
            mood: mood.map(str::to_owned),
            hour_utc: Some(hour),
            weekday: Some((emitted % 7) as u8),
            // 1-in-8 volume-style noise: inside the route-change debounce
            // window, i.e. environment noise the learner must ignore.
            since_route_change_ms: Some(if emitted % 8 == 0 { 10_000 } else { 120_000 }),
            session_ms: Some(600_000 + emitted as u64 * 30_000),
            propensity: None,
        }
    }

    fn context_signal(&mut self, emitted: usize) -> (SignalKind, SignalContext) {
        let kind = if emitted % 2 == 0 { SignalKind::TimeOfDay } else { SignalKind::StatedMood };
        (kind, self.context_for(emitted))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn truth(palettes: &[u32], grooves: &[u16], density: f32) -> GroundTruth {
        GroundTruth {
            ideal_density: density,
            ideal_brightness: 0.4,
            liked_palettes: palettes.to_vec(),
            liked_grooves: grooves.to_vec(),
        }
    }

    fn config(seed: u64) -> SynthConfig {
        SynthConfig {
            seed,
            n_signals: 120,
            palettes: vec![1, 2, 3, 4, 5],
            grooves: vec![0, 1, 2, 3, 4],
            truth: truth(&[1, 3], &[0, 2], 0.75),
            drift: None,
            log_propensity: true,
        }
    }

    fn reaction_count(corpus: &SynthLog) -> usize {
        corpus
            .log
            .signals
            .iter()
            .filter(|s| !matches!(s.kind, SignalKind::TimeOfDay | SignalKind::StatedMood))
            .count()
    }

    #[test]
    fn corpus_is_deterministic_and_full_sized() {
        let a = SynthWorld::new(config(7)).generate();
        let b = SynthWorld::new(config(7)).generate();
        assert_eq!(a, b);
        assert_eq!(reaction_count(&a), 120);
        assert_eq!(a.log.states.len(), a.liked.len());
        assert!(a.log.signals.len() > a.log.states.len() / 4, "states outnumber reactions");
    }

    #[test]
    fn reactions_track_planted_truth() {
        let mut world = SynthWorld::new(config(11));
        let corpus = world.generate();
        let skip_on_liked = corpus
            .log
            .signals
            .iter()
            .zip(corpus.log.states.iter())
            .filter(|(s, _)| s.kind == SignalKind::Skip)
            .filter(|(s, _)| {
                let idx = corpus
                    .log
                    .states
                    .iter()
                    .position(|o| o.fingerprint == s.state_fingerprint)
                    .unwrap();
                corpus.liked[idx]
            })
            .count();
        let bookmarks = corpus.log.signals.iter().filter(|s| s.kind == SignalKind::Bookmark).count();
        assert!(bookmarks > 0, "planted likes should produce bookmarks");
        // Skips on liked states exist only through reaction noise; they must
        // be rarer than bookmarks on this corpus.
        assert!(skip_on_liked <= bookmarks);
    }

    #[test]
    fn drift_changes_active_truth_at_specified_point() {
        let mut cfg = config(3);
        cfg.drift = Some(DriftSpec {
            at_signal: 60,
            truth: truth(&[2, 4], &[1, 3], 0.25),
        });
        let mut world = SynthWorld::new(cfg);
        let corpus = world.generate();
        assert_eq!(reaction_count(&corpus), 120);
        // Both truths must have left marks: liked-state labels exist under
        // the pre- and post-drift utility functions (labels are per-state
        // utility verdicts, so a mixed corpus has both true and false).
        let trues = corpus.liked.iter().filter(|&&l| l).count();
        assert!(trues > 10 && trues < corpus.liked.len(), "labels split: {trues}/{}", corpus.liked.len());
    }

    #[test]
    fn propensities_are_uniform_when_logged() {
        let mut world = SynthWorld::new(config(5));
        let corpus = world.generate();
        let with_p = corpus
            .log
            .signals
            .iter()
            .filter(|s| s.kind != SignalKind::TimeOfDay && s.kind != SignalKind::StatedMood)
            .all(|s| s.context.propensity == Some(0.2));
        assert!(with_p);
    }
}
