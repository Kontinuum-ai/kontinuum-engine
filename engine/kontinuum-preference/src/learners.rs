//! Learner ladder (#24). Strict upgrade discipline: every rung must beat the
//! previous one on offline replay before it ships.
//!
//! - **B0** [`B0Baseline`] — control: taste-DNA priors pass through unchanged.
//! - **B1** [`B1Aggregator`] — exponentially-weighted per-fingerprint-dimension
//!   scores biasing density band, palette weights and groove weights. Fully
//!   deterministic (no sampling; iteration order is `BTreeMap`-ordered), so no
//!   RNG lives on the learning path. Outputs are *strictly bounded* by the DNA
//!   ranges via the [`crate::priors`] guardrail math.
//! - **B2** [`B2Bandit`] — disjoint LinUCB over arms = palette × density band
//!   × groove template, context = {time-of-day, mood, recent-skip-rate},
//!   linear algebra hand-rolled (4×4, Sherman–Morrison rank-1 updates). It
//!   ships **gated off by default** ([`B2Config::default`] sets
//!   `enabled: false` → [`LearnerError::B2DecisionPending`]); turning it on
//!   is the ship decision, made on replay evidence and recorded on #24. The
//!   ≥10% exploration novelty floor is a product decision: see
//!   [`B2Config::exploration_floor`].

use crate::fingerprint::{bucket_center, Granularity, SectionKind, StateFingerprint};
use crate::priors::{biased_weights, softmax, LearnerError, SessionPriors, TastePriors};
use crate::signal::Signal;
use std::collections::BTreeMap;

/// One scored dimension-value pair (BTreeMap key: deterministic order).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum ScoreKey {
    Density(u8),
    Energy(u8),
    Brightness(u8),
    Tempo(u8),
    Section(SectionKind),
    Palette(u32),
    Groove(u16),
    Bass(u16),
    Sample(u8),
}

/// Every dimension the fingerprint keys on, as score keys (sample classes
/// expand to one key each, 0 = empty slot skipped).
fn keys_of(fp: &StateFingerprint) -> impl Iterator<Item = ScoreKey> + '_ {
    fp.section_kind.iter().map(|&k| ScoreKey::Section(k)).chain(
        fp.energy_bucket.iter().map(|&b| ScoreKey::Energy(b)).chain(
            fp.tempo_bucket.iter().map(|&b| ScoreKey::Tempo(b)).chain(
                fp.density_bucket.iter().map(|&b| ScoreKey::Density(b)).chain(
                    fp.brightness_bucket.iter().map(|&b| ScoreKey::Brightness(b)).chain(
                        fp.palette_id.iter().map(|&id| ScoreKey::Palette(id)).chain(
                            fp.groove_template.iter().map(|&g| ScoreKey::Groove(g)).chain(
                                fp.bass_archetype.iter().map(|&b| ScoreKey::Bass(b)).chain(
                                    fp.dominant_sample_classes
                                        .iter()
                                        .flat_map(|cs| cs.iter())
                                        .filter(|&&c| c != 0)
                                        .map(|&c| ScoreKey::Sample(c)),
                                ),
                            ),
                        ),
                    ),
                ),
            ),
        ),
    )
}

/// Learner rung. `observe` ingests attributed fingerprints (current first,
/// previous phrase second); `priors` produces director-ready output bounded
/// by the DNA.
pub trait Learner {
    fn observe(&mut self, attributed: &[StateFingerprint], signal: &Signal);
    fn priors(&self, dna: &TastePriors) -> Result<SessionPriors, LearnerError>;
    fn name(&self) -> &'static str;
}

/// B0 control: taste DNA only, no behavioral learning (#21).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct B0Baseline;

impl Learner for B0Baseline {
    fn observe(&mut self, _attributed: &[StateFingerprint], _signal: &Signal) {}

    fn priors(&self, dna: &TastePriors) -> Result<SessionPriors, LearnerError> {
        Ok(SessionPriors::neutral(dna))
    }

    fn name(&self) -> &'static str {
        "B0"
    }
}

/// B1 knobs. `half_life_updates` is the EWMA half-life measured in *update
/// events* (an update = one signal × one attributed fingerprint); 8 means a
/// single signal's influence halves after 8 subsequent updates on the same key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct B1Config {
    pub half_life_updates: f32,
    pub granularity: Granularity,
}

impl B1Config {
    /// Parse at the boundary: half-life must be finite and positive.
    pub fn new(half_life_updates: f32, granularity: Granularity) -> Result<Self, LearnerError> {
        if !half_life_updates.is_finite() || half_life_updates <= 0.0 {
            return Err(LearnerError::InvalidConfig {
                reason: "half_life_updates must be finite and > 0",
            });
        }
        Ok(B1Config { half_life_updates, granularity })
    }
}

impl Default for B1Config {
    fn default() -> Self {
        B1Config { half_life_updates: 8.0, granularity: Granularity::Mid }
    }
}

/// Weight applied to the t−1 phrase fingerprint when learning (skips react
/// late, so the earlier phrase gets discounted but not ignored).
pub const PREVIOUS_PHRASE_WEIGHT: f32 = 0.5;

/// B1 preference aggregation: exponentially-weighted per-fingerprint-dimension
/// scores. Transparent and debuggable by design — every score is one number
/// per (dimension, value) pair.
#[derive(Clone, Debug)]
pub struct B1Aggregator {
    config: B1Config,
    scores: BTreeMap<ScoreKey, f32>,
    updates: u64,
}

impl Default for B1Aggregator {
    fn default() -> Self {
        B1Aggregator::new(B1Config::default())
    }
}

impl B1Aggregator {
    pub fn new(config: B1Config) -> Self {
        B1Aggregator { config, scores: BTreeMap::new(), updates: 0 }
    }

    pub fn config(&self) -> &B1Config {
        &self.config
    }

    /// Number of observed signals (debugging / transparency view #21).
    pub fn updates(&self) -> u64 {
        self.updates
    }

    /// EWMA decay per update for the configured half-life.
    fn decay(&self) -> f32 {
        0.5f32.powf(1.0 / self.config.half_life_updates)
    }

    /// Per-bucket density scores at the config's granularity (transparency
    /// view #21). All zeros = nothing learned.
    pub fn density_scores(&self) -> Vec<f32> {
        let bins = self.config.granularity.buckets();
        (0..bins)
            .map(|b| *self.scores.get(&ScoreKey::Density(b)).unwrap_or(&0.0))
            .collect()
    }

    /// Softmax position in [0, 1] over the density bucket scores at the
    /// configured granularity. Neutral (0.5) when nothing was learned.
    fn density_position(&self, bins: u8) -> f64 {
        let scores = self.density_scores();
        let shares = softmax(&scores);
        (0..bins)
            .zip(shares)
            .map(|(b, s)| s * bucket_center(b, bins) as f64)
            .sum()
    }
}

impl Learner for B1Aggregator {
    fn observe(&mut self, attributed: &[StateFingerprint], signal: &Signal) {
        let decay = self.decay();
        let strength = signal.strength.clamp(-1.0, 1.0);
        for (i, fp) in attributed.iter().enumerate() {
            // Score on the config's grid so the granularity dial fully controls
            // the study: same log, learner at coarse/mid/fine.
            let fp = fp.coarsen(self.config.granularity);
            let w = if i == 0 { strength } else { strength * PREVIOUS_PHRASE_WEIGHT };
            for key in keys_of(&fp) {
                let entry = self.scores.entry(key).or_insert(0.0);
                *entry = *entry * decay + w;
            }
        }
        self.updates += 1;
    }

    fn priors(&self, dna: &TastePriors) -> Result<SessionPriors, LearnerError> {
        let bins = self.config.granularity.buckets();
        let position = self.density_position(bins) as f32;
        let density_target = dna
            .density
            .clamp_to(dna.density.center() + (position - 0.5) * dna.density.width());
        let palette_weights = biased_weights(&dna.palettes, |id| {
            *self.scores.get(&ScoreKey::Palette(*id)).unwrap_or(&0.0)
        });
        let groove_weights = biased_weights(&dna.grooves, |g| {
            *self.scores.get(&ScoreKey::Groove(*g)).unwrap_or(&0.0)
        });
        Ok(SessionPriors { density_target, palette_weights, groove_weights })
    }

    fn name(&self) -> &'static str {
        "B1"
    }
}

/// B2 situational context (issue #24: {time-of-day, stated mood,
/// recent-skip-rate}). The director sets it before asking for priors; the
/// replay harness uses the neutral default.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct B2Context {
    pub hour_utc: Option<u8>,
    pub mood: Option<String>,
    /// Fraction of the last ~10 signals that were skips, clamped to 0..=1.
    pub recent_skip_rate: f32,
}

/// Context feature dimension: [bias, time-of-day, mood, recent-skip-rate].
const B2_DIM: usize = 4;

/// UCB exploration bonus scale. Trade-off: higher → more state coverage,
/// lower → exploits learned taste sooner.
pub const B2_DEFAULT_ALPHA: f32 = 0.5;

/// Exploration novelty floor — **product decision** (issue #24): at least
/// 10% of sessions get parameters chosen uniformly over the arm space, so
/// taste never collapses into one comfort zone even after the bandit is
/// confident. Implemented as a deterministic round-robin (every
/// `1/floor`-th decision is forced-exploratory) so the floor is exactly met
/// and auditable, not sampled. Above 0.1 the floor still holds; above 0.5
/// exploration would dominate exploitation, which is rejected at
/// [`B2Config::new`].
pub const B2_EXPLORATION_FLOOR: f32 = 0.10;

/// B2 knobs. `enabled: false` is the shipped default — the bandit exists,
/// the ship decision is pending replay evidence.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct B2Config {
    pub enabled: bool,
    pub alpha: f32,
    pub exploration_floor: f32,
    pub explore_seed: u64,
}

impl B2Config {
    /// Parse at the boundary: alpha must be finite and positive, the floor
    /// must lie in (0, 0.5].
    pub fn new(
        enabled: bool,
        alpha: f32,
        exploration_floor: f32,
        explore_seed: u64,
    ) -> Result<Self, LearnerError> {
        if !alpha.is_finite() || alpha <= 0.0 {
            return Err(LearnerError::InvalidConfig { reason: "alpha must be finite and > 0" });
        }
        if !exploration_floor.is_finite() || exploration_floor <= 0.0 || exploration_floor > 0.5 {
            return Err(LearnerError::InvalidConfig {
                reason: "exploration_floor must be finite in (0, 0.5]",
            });
        }
        Ok(B2Config { enabled, alpha, exploration_floor, explore_seed })
    }
}

impl Default for B2Config {
    fn default() -> Self {
        B2Config {
            enabled: false,
            alpha: B2_DEFAULT_ALPHA,
            exploration_floor: B2_EXPLORATION_FLOOR,
            explore_seed: 0x5EED_B2B2,
        }
    }
}

/// One arm: a discrete style decision (palette family × density band ×
/// groove template), all inside the DNA vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Arm {
    palette: u32,
    density_band: u8,
    groove: u16,
}

/// DENSITY_BANDS splits the unit density interval into thirds; an arm owns
/// one third and the DNA band intersects it at prior time.
const DENSITY_BANDS: u8 = 3;

impl Arm {
    fn from_fingerprint(fp: &StateFingerprint) -> Option<Self> {
        let bins = fp.granularity.buckets();
        Some(Arm {
            palette: fp.palette_id?,
            density_band: fp.density_bucket? * DENSITY_BANDS / bins,
            groove: fp.groove_template?,
        })
    }
}

/// Disjoint LinUCB state for one arm: `a_inv` is the Sherman–Morrison
/// maintained inverse of `A = I + Σ x xᵀ`, `b` the reward-weighted feature
/// sum. Hand-rolled at `B2_DIM = 4` — no linear-algebra dependency.
#[derive(Clone, Copy, Debug)]
struct ArmState {
    a_inv: [[f64; B2_DIM]; B2_DIM],
    b: [f64; B2_DIM],
}

impl Default for ArmState {
    fn default() -> Self {
        let mut a_inv = [[0.0; B2_DIM]; B2_DIM];
        for (i, row) in a_inv.iter_mut().enumerate() {
            row[i] = 1.0;
        }
        ArmState { a_inv, b: [0.0; B2_DIM] }
    }
}

fn mat_vec(m: &[[f64; B2_DIM]; B2_DIM], v: &[f64; B2_DIM]) -> [f64; B2_DIM] {
    let mut out = [0.0; B2_DIM];
    for (r, row) in m.iter().enumerate() {
        out[r] = dot(row, v);
    }
    out
}

fn dot(a: &[f64; B2_DIM], b: &[f64; B2_DIM]) -> f64 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// Mood string → situational valence in 0..=1. Unknown or absent moods are
/// neutral; the mapping is documented here so the transparency layer can
/// show exactly what the bandit saw.
fn mood_valence(mood: Option<&str>) -> f64 {
    match mood.map(str::to_ascii_lowercase).as_deref() {
        Some("energetic") | Some("peak") => 0.9,
        Some("focused") => 0.65,
        Some("relaxed") | Some("chill") => 0.25,
        Some("late night") => 0.1,
        _ => 0.5,
    }
}

fn context_features(ctx: &B2Context) -> [f64; B2_DIM] {
    [
        1.0,
        ctx.hour_utc.map(|h| h as f64 / 24.0).unwrap_or(0.5),
        mood_valence(ctx.mood.as_deref()),
        ctx.recent_skip_rate.clamp(0.0, 1.0) as f64,
    ]
}

/// B2 contextual bandit — disjoint LinUCB, shipped gated off (see
/// [`B2Config`]). Behavior when enabled: per session, pick the arm with the
/// best UCB score for the current context; every `1/floor`-th decision is
/// forced-exploratory (uniform over the arm space) to meet the novelty
/// floor. Decisions and exploration draws are deterministic under
/// `explore_seed`.
#[derive(Clone, Debug)]
pub struct B2Bandit {
    config: B2Config,
    context: B2Context,
    arms: std::cell::RefCell<BTreeMap<Arm, ArmState>>,
    decisions: std::cell::Cell<u64>,
    exploratory: std::cell::Cell<u64>,
    rng: std::cell::RefCell<XorShift>,
}

impl Default for B2Bandit {
    fn default() -> Self {
        B2Bandit::new(B2Config::default())
    }
}

impl B2Bandit {
    pub fn new(config: B2Config) -> Self {
        let rng = XorShift::from_seed(config.explore_seed);
        B2Bandit {
            config,
            context: B2Context::default(),
            arms: std::cell::RefCell::new(BTreeMap::new()),
            decisions: std::cell::Cell::new(0),
            exploratory: std::cell::Cell::new(0),
            rng: std::cell::RefCell::new(rng),
        }
    }

    pub fn config(&self) -> &B2Config {
        &self.config
    }

    /// Set the situational context for the next `priors` call.
    pub fn set_context(&mut self, context: B2Context) {
        self.context = context;
    }

    /// Fraction of decisions that were exploratory (≥ the configured floor).
    pub fn exploratory_share(&self) -> f64 {
        let decisions = self.decisions.get();
        if decisions == 0 { 0.0 } else { self.exploratory.get() as f64 / decisions as f64 }
    }

    fn x(&self) -> [f64; B2_DIM] {
        context_features(&self.context)
    }

    /// Sherman–Morrison rank-1 update of `A⁻¹` plus the reward sum.
    fn update_arm(&self, arm: Arm, x: [f64; B2_DIM], reward: f64) {
        let mut arms = self.arms.borrow_mut();
        let state = arms.entry(arm).or_default();
        let ax = mat_vec(&state.a_inv, &x);
        let denom = 1.0 + dot(&x, &ax);
        if denom <= f64::EPSILON || !denom.is_finite() {
            return;
        }
        for i in 0..B2_DIM {
            for j in 0..B2_DIM {
                state.a_inv[i][j] -= ax[i] * ax[j] / denom;
            }
        }
        for i in 0..B2_DIM {
            state.b[i] += reward * x[i];
        }
    }

    /// UCB score of one arm: `θ·x + α·√(xᵀ A⁻¹ x)`.
    fn ucb(&self, arm: &Arm, x: &[f64; B2_DIM]) -> f64 {
        let state = self.arms.borrow().get(arm).copied().unwrap_or_default();
        let theta = mat_vec(&state.a_inv, &state.b);
        let bonus = dot(x, &mat_vec(&state.a_inv, x)).max(0.0).sqrt();
        dot(&theta, x) + self.config.alpha as f64 * bonus
    }

    /// Pick the arm for the current context. Every `k`-th decision (k =
    /// round(1/floor)) is forced-uniform exploration; the rest are UCB
    /// argmax with deterministic `BTreeMap` tie-breaking.
    fn select(&self, dna: &TastePriors) -> Arm {
        let decisions = self.decisions.get() + 1;
        self.decisions.set(decisions);
        let mut arms = all_arms(dna);
        let k = (1.0 / self.config.exploration_floor).round().max(1.0) as u64;
        if decisions % k == 0 {
            self.exploratory.set(self.exploratory.get() + 1);
            let idx = self.rng.borrow_mut().below(arms.len() as u64) as usize;
            return arms.swap_remove(idx);
        }
        let x = self.x();
        arms.into_iter()
            .max_by(|a, b| self.ucb(a, &x).total_cmp(&self.ucb(b, &x)))
            .expect("dna vocabularies are non-empty by construction")
    }

    fn priors_for(&self, dna: &TastePriors) -> SessionPriors {
        let arm = self.select(dna);
        let band_lo = arm.density_band as f32 / DENSITY_BANDS as f32;
        let band_hi = (arm.density_band + 1) as f32 / DENSITY_BANDS as f32;
        let lo = dna.density.lo.max(band_lo);
        let hi = dna.density.hi.min(band_hi);
        let target = if lo <= hi { (lo + hi) / 2.0 } else { dna.density.center() };
        let density_target = dna.density.clamp_to(target);
        let palette_weights = biased_weights(&dna.palettes, |id| {
            if *id == arm.palette { 3.0 } else { 0.0 }
        });
        let groove_weights = biased_weights(&dna.grooves, |g| {
            if *g == arm.groove { 3.0 } else { 0.0 }
        });
        SessionPriors { density_target, palette_weights, groove_weights }
    }
}

/// The full arm space: DNA palettes × density bands × DNA grooves.
fn all_arms(dna: &TastePriors) -> Vec<Arm> {
    let mut arms = Vec::with_capacity(dna.palettes.len() * DENSITY_BANDS as usize * dna.grooves.len());
    for &p in &dna.palettes {
        for band in 0..DENSITY_BANDS {
            for &g in &dna.grooves {
                arms.push(Arm { palette: p, density_band: band, groove: g });
            }
        }
    }
    arms
}

impl Learner for B2Bandit {
    fn observe(&mut self, attributed: &[StateFingerprint], signal: &Signal) {
        if !self.config.enabled {
            return;
        }
        let x = self.x();
        let reward = signal.strength.clamp(-1.0, 1.0) as f64;
        for (i, fp) in attributed.iter().enumerate() {
            if let Some(arm) = Arm::from_fingerprint(&fp.coarsen(Granularity::Mid)) {
                let w = if i == 0 { 1.0 } else { PREVIOUS_PHRASE_WEIGHT as f64 };
                self.update_arm(arm, x, reward * w);
            }
        }
    }

    fn priors(&self, dna: &TastePriors) -> Result<SessionPriors, LearnerError> {
        if !self.config.enabled {
            // Ship decision pending replay evidence (issue #24).
            return Err(LearnerError::B2DecisionPending);
        }
        Ok(self.priors_for(dna))
    }

    fn name(&self) -> &'static str {
        "B2"
    }
}

/// Tiny deterministic RNG (xorshift64\*), for seeded synthetic replay data and
/// future B2 exploration — the learning path above stays fully deterministic
/// without it. Seed 0 maps to a nonzero constant (fixed point of the
/// recurrence); streams are reproducible across targets.
#[derive(Clone, Debug)]
pub struct XorShift(u64);

impl XorShift {
    pub fn from_seed(seed: u64) -> Self {
        XorShift(if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed })
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform integer in [0, n).
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }

    /// Uniform [0, 1).
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Bernoulli trial with probability `p` in [0, 1].
    pub fn chance(&mut self, p: f32) -> bool {
        self.next_f32() < p
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fingerprint::{attribute, MusicalState};
    use crate::priors::{weight_cap, weight_floor, DnaBand};
    use crate::signal::SignalKind;

    fn dna() -> TastePriors {
        TastePriors {
            bpm: 124.0,
            energy: DnaBand::new(0.4, 0.9).unwrap(),
            density: DnaBand::new(0.2, 0.8).unwrap(),
            darkness: DnaBand::new(0.3, 0.9).unwrap(),
            palettes: vec![1, 2, 3, 4],
            grooves: vec![0, 1, 2],
        }
    }

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

    fn plant_preferences() -> (B1Aggregator, B0Baseline) {
        // 30 bookmarks on dense, palette-1 states; 30 skips on sparse,
        // palette-2 states. Both ids are in the DNA vocabulary.
        let mut b1 = B1Aggregator::default();
        let mut b0 = B0Baseline;
        for i in 0..30u64 {
            let learners: [&mut dyn Learner; 2] = [&mut b1, &mut b0];
            for learner in learners {
                let like = Signal::new(
                    1_000 + i as i64 * 10,
                    SignalKind::Bookmark,
                    state(0.95, 1).fingerprint(Granularity::Fine),
                );
                learner.observe(&attribute(&like, None).fingerprints(), &like);
                let dislike = Signal::new(
                    2_000 + i as i64 * 10,
                    SignalKind::Skip,
                    state(0.05, 2).fingerprint(Granularity::Fine),
                );
                learner.observe(&attribute(&dislike, None).fingerprints(), &dislike);
            }
        }
        (b1, b0)
    }

    #[test]
    fn b0_stays_flat_on_dna_center() {
        let (b1, b0) = plant_preferences();
        let d = dna();
        let p0 = b0.priors(&d).unwrap();
        assert_eq!(p0.density_target, d.density.center());
        // Uniform shares (1/n each) put every declared weight at the same
        // midpoint of the weight band: floor + (cap - floor) / n.
        let n = d.palettes.len();
        let uniform = weight_floor(n) + (weight_cap(n) - weight_floor(n)) / n as f32;
        assert!(p0.palette_weights.values().all(|w| (*w - uniform).abs() < 1e-5));
        let p1 = b1.priors(&d).unwrap();
        assert_ne!(p1.density_target, p0.density_target);
    }

    #[test]
    fn b1_converges_on_planted_preferences() {
        let (b1, _b0) = plant_preferences();
        let d = dna();
        let p = b1.priors(&d).unwrap();
        // Liked dense states pull the density target up the DNA band.
        assert!(
            p.density_target > d.density.center() + 0.1,
            "density_target {} did not move up",
            p.density_target
        );
        assert!(p.density_target <= d.density.hi + 1e-4);
        // Palette 1 (liked) must out-weigh palette 2 (skipped).
        let w1 = p.palette_weights[&1];
        let w2 = p.palette_weights[&2];
        assert!(w1 > w2, "liked palette weight {w1} not above skipped {w2}");
        // Grooves were never signaled asymmetrically → neutral spread.
        assert!(!p.groove_weights.is_empty());
    }

    #[test]
    fn guardrail_outputs_stay_bounded_under_random_streams() {
        let d = dna();
        let kinds = [
            SignalKind::Skip,
            SignalKind::Bookmark,
            SignalKind::ListenThroughSection,
            SignalKind::ExplicitLessLikeThis,
            SignalKind::SessionLength,
        ];
        for seed in 1..=200u64 {
            let mut rng = XorShift::from_seed(seed);
            let mut b1 = B1Aggregator::default();
            for i in 0..40u64 {
                let kind = kinds[rng.below(kinds.len() as u64) as usize];
                let density = rng.next_f32();
                let palette = d.palettes[rng.below(d.palettes.len() as u64) as usize];
                let s = Signal::new(
                    i as i64 * 1_000,
                    kind,
                    state(density, palette).fingerprint(Granularity::Mid),
                );
                let strength = (rng.next_f32() - 0.5) * 2.0;
                let s = Signal { strength, ..s };
                b1.observe(&attribute(&s, None).fingerprints(), &s);
            }
            let p = b1.priors(&d).unwrap();
            assert!(p.is_within(&d), "seed {seed}: priors escaped DNA: {:?}", p);
            let n = d.palettes.len();
            for w in p.palette_weights.values() {
                assert!(
                    *w >= weight_floor(n) - 1e-4 && *w <= weight_cap(n) + 1e-4,
                    "seed {seed}: palette weight {w} outside [{}, {}]",
                    weight_floor(n),
                    weight_cap(n)
                );
            }
        }
    }

    #[test]
    fn b1_config_validation_rejects_bad_half_life() {
        assert!(matches!(
            B1Config::new(0.0, Granularity::Mid),
            Err(LearnerError::InvalidConfig { .. })
        ));
        assert!(matches!(
            B1Config::new(f32::NAN, Granularity::Mid),
            Err(LearnerError::InvalidConfig { .. })
        ));
        assert!(B1Config::new(8.0, Granularity::Coarse).is_ok());
    }

    #[test]
    fn b2_is_gated_off_by_default() {
        let err = B2Bandit::default().priors(&dna()).unwrap_err();
        assert!(matches!(err, LearnerError::B2DecisionPending));
    }

    #[test]
    fn b2_config_validation_rejects_bad_alpha_and_floor() {
        assert!(matches!(
            B2Config::new(true, 0.0, 0.1, 1),
            Err(LearnerError::InvalidConfig { .. })
        ));
        assert!(matches!(
            B2Config::new(true, 0.5, 0.0, 1),
            Err(LearnerError::InvalidConfig { .. })
        ));
        assert!(matches!(
            B2Config::new(true, 0.5, 0.9, 1),
            Err(LearnerError::InvalidConfig { .. })
        ));
        assert!(B2Config::new(true, 0.5, 0.1, 1).is_ok());
    }

    #[test]
    fn b2_exploration_floor_is_met_exactly_and_deterministically() {
        let cfg = B2Config::new(true, 0.5, 0.10, 42).unwrap();
        let mut bandit = B2Bandit::new(cfg);
        bandit.set_context(B2Context {
            hour_utc: Some(22),
            mood: Some("late night".into()),
            recent_skip_rate: 0.1,
        });
        let d = dna();
        // Planted strong preference: reward every palette-1 state, punish
        // everything else, so UCB would collapse to palette 1 without the
        // floor.
        for i in 0..200u64 {
            let liked = i % 4 == 0;
            let palette = if liked { 1 } else { 2 };
            let s = Signal::new(
                i as i64 * 1_000,
                if liked { SignalKind::Bookmark } else { SignalKind::Skip },
                state(0.5, palette).fingerprint(Granularity::Mid),
            );
            bandit.observe(&attribute(&s, None).fingerprints(), &s);
        }
        let mut runs = Vec::new();
        for _ in 0..300 {
            runs.push(bandit.priors(&d).unwrap());
        }
        let share = bandit.exploratory_share();
        assert!((share - 0.10).abs() < 1e-9, "exploratory share {share}");
        assert!(share * 300.0 >= 30.0 - 1e-9, "floor violated: {share} of 300");
        // Determinism: same seed → identical arm sequence.
        let mut again = B2Bandit::new(B2Config::new(true, 0.5, 0.10, 42).unwrap());
        again.observe(&[], &Signal::new(0, SignalKind::Bookmark, state(0.5, 1).fingerprint(Granularity::Mid)));
        let first = again.priors(&d).unwrap();
        let mut again2 = B2Bandit::new(B2Config::new(true, 0.5, 0.10, 42).unwrap());
        again2.observe(&[], &Signal::new(0, SignalKind::Bookmark, state(0.5, 1).fingerprint(Granularity::Mid)));
        let second = again2.priors(&d).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn b2_learns_planted_palette_and_stays_bounded() {
        let cfg = B2Config::new(true, 0.5, 0.10, 7).unwrap();
        let mut bandit = B2Bandit::new(cfg);
        let d = dna();
        for i in 0..120u64 {
            let liked = i % 2 == 0;
            let s = Signal::new(
                i as i64 * 1_000,
                if liked { SignalKind::Bookmark } else { SignalKind::Skip },
                state(if liked { 0.9 } else { 0.1 }, if liked { 1 } else { 2 })
                    .fingerprint(Granularity::Fine),
            );
            bandit.observe(&attribute(&s, None).fingerprints(), &s);
        }
        // Exploitation decisions (non-forced) must favor the liked palette.
        let mut liked_top = 0;
        let mut decisions = 0;
        for i in 0..100 {
            if i % 10 == 9 {
                continue; // forced-exploratory decision
            }
            bandit.set_context(B2Context::default());
            let p = bandit.priors(&d).unwrap();
            assert!(p.is_within(&d));
            decisions += 1;
            let top = p.palette_weights.iter().max_by(|a, b| a.1.total_cmp(b.1)).unwrap();
            if *top.0 == 1 {
                liked_top += 1;
            }
        }
        assert!(liked_top > decisions / 2, "liked palette topped {liked_top}/{decisions}");
    }

    #[test]
    fn adversarial_learner_cannot_escape_the_dna_boundary() {
        // A hostile learner: NaN density, infinite/NaN weights, weights on
        // undeclared vocabulary. The boundary clamp (harness `sanitize`)
        // must make its output DNA-safe regardless.
        struct Adversarial;
        impl Learner for Adversarial {
            fn observe(&mut self, _a: &[StateFingerprint], _s: &Signal) {}
            fn priors(&self, _dna: &TastePriors) -> Result<SessionPriors, LearnerError> {
                let mut weights = std::collections::BTreeMap::new();
                weights.insert(1u32, f32::INFINITY);
                weights.insert(2, f32::NEG_INFINITY);
                weights.insert(999, 42.0); // not in the DNA vocabulary
                let mut grooves = std::collections::BTreeMap::new();
                grooves.insert(0u16, f32::NAN);
                Ok(SessionPriors {
                    density_target: f32::NAN,
                    palette_weights: weights,
                    groove_weights: grooves,
                })
            }
            fn name(&self) -> &'static str {
                "adversarial"
            }
        }
        let d = dna();
        let sanitized = Adversarial.priors(&d).unwrap().sanitize(&d);
        assert!(sanitized.is_within(&d), "sanitize failed: {sanitized:?}");
        assert!(sanitized.density_target.is_finite());
        assert_eq!(sanitized.density_target, d.density.center());
        assert!(sanitized.palette_weights.contains_key(&999) == false);
        let (floor, cap) = (weight_floor(d.palettes.len()), weight_cap(d.palettes.len()));
        for w in sanitized.palette_weights.values() {
            assert!(*w >= floor - 1e-4 && *w <= cap + 1e-4);
        }
        for w in sanitized.groove_weights.values() {
            assert!(w.is_finite() && *w >= 0.0);
        }
        // Through the harness: same learner, same outcome.
        let log = crate::replay::ReplayLog {
            signals: vec![Signal::new(1_000, SignalKind::Skip, state(0.5, 1).fingerprint(Granularity::Mid))],
            states: vec![crate::replay::StateObservation {
                ts_ms: 1_000,
                fingerprint: state(0.5, 1).fingerprint(Granularity::Mid),
            }],
        };
        let m = crate::replay::ReplayHarness::default().run(&log, &mut Adversarial, &d).unwrap();
        assert!(m.skip_rate_proxy.is_finite() && m.session_length_proxy.is_finite());
    }

    #[test]
    fn xorshift_is_deterministic_and_seed0_safe() {
        let mut a = XorShift::from_seed(42);
        let mut b = XorShift::from_seed(42);
        for _ in 0..1_000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        let mut z = XorShift::from_seed(0);
        assert_ne!(z.next_u64(), 0);
        let mut f = XorShift::from_seed(7);
        for _ in 0..10_000 {
            let v = f.next_f32();
            assert!((0.0..1.0).contains(&v));
        }
    }
}
