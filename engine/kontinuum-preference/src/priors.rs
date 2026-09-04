//! Prior value types and the guardrail math that keeps learned output inside
//! the taste-DNA ranges (#24: "a bad learner can bias, never break, a
//! session").
//!
//! ## Bounding by construction
//!
//! Categorical weights live in a band `[floor, cap]` with `floor = 0.5/n`,
//! `cap = min(2/n, 1)`: each weight is `floor + (cap − floor) · share` where
//! `share` is a softmax score-share in (0, 1) summing to 1, so every weight is
//! inside the band for any input. The density target is
//! `center + (p − 0.5) · width` with `p` a softmax position in [0, 1], so it
//! stays inside the DNA band for any input. Weights are *relative* priors
//! (consumers must not assume they sum to 1).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Errors from building priors (learner ladder error surface).
#[derive(Debug, thiserror::Error)]
pub enum LearnerError {
    #[error("DNA band must satisfy 0 <= lo <= hi <= 1")]
    InvalidBand { lo: f32, hi: f32 },
    #[error("invalid learner config: {reason}")]
    InvalidConfig { reason: &'static str },
    #[error(
        "B2 contextual bandit is parked: ship/park decision pending replay evidence (issue #24)"
    )]
    B2DecisionPending,
}

/// Allowed range of a continuous taste dimension (a slice of the DNA range).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DnaBand {
    pub lo: f32,
    pub hi: f32,
}

impl DnaBand {
    /// Parse at the boundary: bands must be finite and ordered inside 0..=1.
    pub fn new(lo: f32, hi: f32) -> Result<Self, LearnerError> {
        if !lo.is_finite()
            || !hi.is_finite()
            || !(0.0..=1.0).contains(&lo)
            || !(lo..=1.0).contains(&hi)
        {
            return Err(LearnerError::InvalidBand { lo, hi });
        }
        Ok(DnaBand { lo, hi })
    }

    pub fn center(&self) -> f32 {
        (self.lo + self.hi) / 2.0
    }

    pub fn width(&self) -> f32 {
        self.hi - self.lo
    }

    pub fn clamp_to(&self, v: f32) -> f32 {
        v.clamp(self.lo, self.hi)
    }

    pub(crate) fn contains(&self, v: f32) -> bool {
        v >= self.lo - BOUNDS_EPS && v <= self.hi + BOUNDS_EPS
    }
}

/// Float tolerance for guardrail checks (softmax arithmetic is f64 but the
/// API is f32; rounding cannot be visible at this scale).
const BOUNDS_EPS: f32 = 1e-4;

/// Taste-DNA ranges a learner may bias within (issue #21 model, range form).
/// Categorical vocabularies are explicit: a learner may only re-weight
/// palettes/grooves the DNA declares.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TastePriors {
    /// Target tempo, BPM (passed through, mirroring `TasteProfile.bpm`).
    pub bpm: f64,
    pub energy: DnaBand,
    pub density: DnaBand,
    pub darkness: DnaBand,
    /// Palette ids the director may sample.
    pub palettes: Vec<u32>,
    /// Groove template indices the director may sample.
    pub grooves: Vec<u16>,
}

/// Learned/controlled priors for the session director. Bounded by the DNA:
/// see [`SessionPriors::is_within`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionPriors {
    /// Density the director should aim at; guaranteed inside `dna.density`.
    pub density_target: f32,
    /// Relative palette weights in `[floor, cap]` (not a distribution).
    pub palette_weights: BTreeMap<u32, f32>,
    /// Relative groove weights in `[floor, cap]` (not a distribution).
    pub groove_weights: BTreeMap<u16, f32>,
}

/// Lower weight bound for `n` candidates.
pub fn weight_floor(n: usize) -> f32 {
    if n == 0 { 0.0 } else { 0.5 / n as f32 }
}

/// Upper weight bound for `n` candidates.
pub fn weight_cap(n: usize) -> f32 {
    if n == 0 { 0.0 } else { (2.0 / n as f32).min(1.0) }
}

/// Half-width of the band a #21 point profile expands into. The #21 importer
/// (`kontinuum-compose::taste::TasteProfile`) ships point estimates, not
/// ranges; the learner treats each point as the center of a ±0.2 band
/// (clamped into 0..=1) so learned priors always have room to move inside it.
pub const PROFILE_BAND_HALF_WIDTH: f32 = 0.2;

impl TastePriors {
    /// Build the DNA from the documented shape of #21's
    /// `TasteProfile` (`bpm: Option<f64>`, `energy`/`darkness`/`density`:
    /// f32 points in 0..=1). The palette/groove vocabularies come from the
    /// caller — the director owns that vocabulary, the profile does not
    /// carry it.
    pub fn from_profile_point(
        bpm: Option<f64>,
        energy: f32,
        darkness: f32,
        density: f32,
        palettes: Vec<u32>,
        grooves: Vec<u16>,
    ) -> Result<Self, LearnerError> {
        let point_band = |p: f32| -> Result<DnaBand, LearnerError> {
            if !p.is_finite() || !(0.0..=1.0).contains(&p) {
                return Err(LearnerError::InvalidConfig {
                    reason: "profile point must be finite in 0..=1",
                });
            }
            DnaBand::new(
                (p - PROFILE_BAND_HALF_WIDTH).max(0.0),
                (p + PROFILE_BAND_HALF_WIDTH).min(1.0),
            )
        };
        if let Some(b) = bpm {
            if !b.is_finite() || !(30.0..=300.0).contains(&b) {
                return Err(LearnerError::InvalidConfig {
                    reason: "bpm must be finite in 30..=300",
                });
            }
        }
        Ok(TastePriors {
            bpm: bpm.unwrap_or(124.0),
            energy: point_band(energy)?,
            density: point_band(density)?,
            darkness: point_band(darkness)?,
            palettes,
            grooves,
        })
    }
}

fn clamp_weights<K: Ord + Copy>(weights: BTreeMap<K, f32>, vocab: &[K]) -> BTreeMap<K, f32> {
    let (floor, cap) = (weight_floor(vocab.len()), weight_cap(vocab.len()));
    vocab
        .iter()
        .filter_map(|id| {
            weights.get(id).map(|w| {
                let w = if w.is_finite() { (*w).clamp(floor, cap) } else { floor };
                (*id, w)
            })
        })
        .collect()
}

impl SessionPriors {
    /// DNA priors with zero behavioral bias — the B0 output.
    pub fn neutral(dna: &TastePriors) -> Self {
        SessionPriors {
            density_target: dna.density.center(),
            palette_weights: uniform_weights(&dna.palettes),
            groove_weights: uniform_weights(&dna.grooves),
        }
    }

    /// Guardrail: does this output stay inside the DNA it was built from?
    pub fn is_within(&self, dna: &TastePriors) -> bool {
        dna.density.contains(self.density_target)
            && self.palette_weights.values().all(|w| w.is_finite() && *w >= 0.0)
            && self.groove_weights.values().all(|w| w.is_finite() && *w >= 0.0)
    }

    /// Boundary clamp (issue #24 guardrail): force arbitrary learner output
    /// into the DNA — density into its band (non-finite collapses to the
    /// center), weights into `[floor, cap]` (non-finite collapses to the
    /// floor), and only over the DNA's declared vocabulary. Applied where
    /// learner output meets the director, so a bad learner can bias, never
    /// break, a session.
    pub fn sanitize(mut self, dna: &TastePriors) -> Self {
        self.density_target = if self.density_target.is_finite() {
            dna.density.clamp_to(self.density_target)
        } else {
            dna.density.center()
        };
        self.palette_weights = clamp_weights(self.palette_weights, &dna.palettes);
        self.groove_weights = clamp_weights(self.groove_weights, &dna.grooves);
        self
    }

    /// Deterministic 0..=1 preference score for a fingerprint, used by the
    /// replay harness for its skip/session proxies. Unavailable components
    /// are skipped; with none available the score is neutral (0.5).
    pub fn preference_score(&self, fp: &crate::fingerprint::StateFingerprint) -> f32 {
        use crate::fingerprint::bucket_center;
        let (mut sum, mut count) = (0.0f32, 0u32);
        if let Some(b) = fp.density_bucket {
            let center = bucket_center(b, fp.granularity.buckets());
            sum += (1.0 - (center - self.density_target).abs() * 2.0).clamp(0.0, 1.0);
            count += 1;
        }
        if let Some(w) = fp.palette_id.and_then(|id| self.palette_weights.get(&id)) {
            sum += normalized_weight(*w, self.palette_weights.len());
            count += 1;
        }
        if let Some(w) = fp.groove_template.and_then(|g| self.groove_weights.get(&g)) {
            sum += normalized_weight(*w, self.groove_weights.len());
            count += 1;
        }
        if count == 0 { 0.5 } else { sum / count as f32 }
    }
}

/// Map a weight to a 0..=1 preference component with the *neutral point at
/// the uniform share* (1/n): equal scores must read 0.5, not the share
/// itself, or every state reads below the 0.5 skip threshold for n > 2.
/// Scale pins share 1 → 1; shares below uniform compress toward 0.
fn normalized_weight(w: f32, n: usize) -> f32 {
    let (floor, cap) = (weight_floor(n), weight_cap(n));
    if cap <= floor {
        return 0.5;
    }
    let share = ((w - floor) / (cap - floor)).clamp(0.0, 1.0);
    let scale = 0.5 * n as f32 / (n as f32 - 1.0).max(1.0);
    (0.5 + (share - 1.0 / n as f32) * scale).clamp(0.0, 1.0)
}

fn uniform_weights<K: Ord + Copy>(ids: &[K]) -> BTreeMap<K, f32> {
    biased_weights(ids, |_| 0.0)
}

pub(crate) fn biased_weights<K: Ord + Copy>(
    ids: &[K],
    score: impl Fn(&K) -> f32,
) -> BTreeMap<K, f32> {
    let n = ids.len();
    if n == 0 {
        return BTreeMap::new();
    }
    let scores: Vec<f32> = ids.iter().map(&score).collect();
    let shares = softmax(&scores);
    let (floor, cap) = (weight_floor(n), weight_cap(n));
    ids.iter()
        .zip(shares)
        .map(|(id, s)| (*id, floor + (cap - floor) * s as f32))
        .collect()
}

/// Numerically stable softmax over f32 scores; shares sum to 1 (f64 math).
pub(crate) fn softmax(scores: &[f32]) -> Vec<f64> {
    if scores.is_empty() {
        return Vec::new();
    }
    let max = scores.iter().copied().fold(f32::MIN, f32::max) as f64;
    let exps: Vec<f64> = scores.iter().map(|&s| (s as f64 - max).exp()).collect();
    let total: f64 = exps.iter().sum();
    exps.into_iter().map(|e| e / total).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fingerprint::{Granularity, MusicalState, SectionKind};

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

    #[test]
    fn band_validation_rejects_garbage() {
        assert!(matches!(
            DnaBand::new(0.8, 0.2),
            Err(LearnerError::InvalidBand { .. })
        ));
        assert!(matches!(
            DnaBand::new(-0.1, 0.5),
            Err(LearnerError::InvalidBand { .. })
        ));
        assert!(matches!(
            DnaBand::new(f32::NAN, 0.5),
            Err(LearnerError::InvalidBand { .. })
        ));
        assert!(DnaBand::new(0.2, 0.8).is_ok());
    }

    #[test]
    fn neutral_priors_are_centered_and_uniform() {
        let d = dna();
        let p = SessionPriors::neutral(&d);
        assert_eq!(p.density_target, d.density.center());
        let n = d.palettes.len();
        let uniform = weight_floor(n) + (weight_cap(n) - weight_floor(n)) / n as f32;
        assert!(p.palette_weights.values().all(|w| (*w - uniform).abs() < 1e-5));
        assert!(p.is_within(&d));
    }

    #[test]
    fn softmax_shares_sum_to_one_and_are_stable() {
        let shares = softmax(&[0.0, 0.0, 0.0]);
        assert!((shares.iter().sum::<f64>() - 1.0).abs() < 1e-9);
        assert!((shares[0] - shares[2]).abs() < 1e-12);
        let skewed = softmax(&[1000.0, -1000.0]);
        assert!((skewed.iter().sum::<f64>() - 1.0).abs() < 1e-9);
        assert!(softmax(&[]).is_empty());
    }

    #[test]
    fn preference_score_is_neutral_without_overlap_and_directional_with() {
        let d = dna();
        let neutral = SessionPriors::neutral(&d);
        let state = MusicalState {
            section_kind: SectionKind::Peak,
            energy: 0.7,
            density: 0.5,
            brightness: 0.5,
            bpm: 124.0,
            palette_id: 99, // not in the DNA vocabulary
            groove_template: 77,
            bass_archetype: 2,
            dominant_sample_classes: [5, 0, 0, 0],
        };
        let fp = state.fingerprint(Granularity::Mid);
        assert!((neutral.preference_score(&fp) - 0.5).abs() > 0.0); // density still scores
        let fp_no_density = fp.coarsen(Granularity::Coarse);
        assert!((neutral.preference_score(&fp_no_density) - 0.5).abs() < 1e-6);
    }
}
