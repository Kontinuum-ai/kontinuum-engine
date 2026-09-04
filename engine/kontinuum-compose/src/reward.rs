//! Producer reward model (#26): unified quality scores driving the composer.
//!
//! Closes the loop: the critic (#25) measures the running mix, the preference
//! learner (#24) scores user behavior, and this module folds both into one
//! bounded, deterministic quality signal that biases the composer's next wake
//! (#22) through its taste channel. Every output is a *prior*, never a command:
//! deltas are hard-bounded so a misfiring critic can bias a session, never
//! break one (same guardrail philosophy as kontinuum-preference).
//!
//! The input side is a self-contained serde struct ([`QualityInput`]) so hosts
//! can feed live critic snapshots, logged replay data, or both without this
//! crate depending on kontinuum-analysis.

use serde::{Deserialize, Serialize};

/// Per-axis weights of the overall quality score (versioned).
///
/// Version bump policy: changing weights changes generated sessions; log the
/// reason in the changelog and re-baseline the replay harness comparisons.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RewardWeights {
    /// Critic dynamics axis (crest, GR discipline).
    pub dynamics: f32,
    /// Critic spectral axis (tilt, centroid discipline).
    pub spectral: f32,
    /// Critic loudness axis (target window adherence).
    pub loudness: f32,
    /// Critic masking axis (kick/bass and pad collision penalties).
    pub masking: f32,
    /// Preference-learner prior score (#24 B1 output, 0..1).
    pub preference: f32,
}

impl Default for RewardWeights {
    fn default() -> Self {
        // Minimal techno weighting: dynamics and masking carry the genre's
        // signature; preference breaks ties rather than dominating.
        RewardWeights { dynamics: 0.3, spectral: 0.2, loudness: 0.15, masking: 0.2, preference: 0.15 }
    }
}

impl RewardWeights {
    pub fn validate(&self) -> Result<(), String> {
        for (name, w) in [
            ("dynamics", self.dynamics),
            ("spectral", self.spectral),
            ("loudness", self.loudness),
            ("masking", self.masking),
            ("preference", self.preference),
        ] {
            if !(0.0..=1.0).contains(&w) {
                return Err(format!("reward weight {name} out of range: {w}"));
            }
        }
        let sum: f32 = self.dynamics + self.spectral + self.loudness + self.masking + self.preference;
        if (sum - 1.0).abs() > 1e-3 {
            return Err(format!("reward weights must sum to 1.0 (got {sum})"));
        }
        Ok(())
    }
}

/// Everything the reward model scores, normalized to 0..1 per axis.
///
/// The host adapts the critic's [`snapshot`](extern crate) and preference
/// priors into these axes; flags are hard evidence inputs that penalize the
/// score regardless of the numeric axes (a latched GR alarm must hurt even if
/// the averaged axes look fine).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct QualityInput {
    /// Critic dynamics axis, 0 (collapsed) .. 1 (healthy).
    #[serde(default = "default_axis")]
    pub dynamics: f32,
    /// Critic spectral discipline (tilt + centroid), 0..1.
    #[serde(default = "default_axis")]
    pub spectral: f32,
    /// Critic loudness-in-target-window, 0..1.
    #[serde(default = "default_axis")]
    pub loudness: f32,
    /// Critic masking resolution (1 = no collision), 0..1.
    #[serde(default = "default_axis")]
    pub masking: f32,
    /// Preference prior score from #24 (0.5 = neutral DNA).
    #[serde(default = "default_axis")]
    pub preference: f32,
    /// Latched GR/limit alarms (#28 telemetry, #15 kill switch).
    #[serde(default)]
    pub alarm_flags: Vec<String>,
}

fn default_axis() -> f32 {
    0.5
}

impl QualityInput {
    fn clamp_axis(v: f32) -> f32 {
        if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.5 }
    }

    /// Sanitizes hostile input: non-finite or out-of-range axes become neutral
    /// (0.5), never poison the score.
    pub fn sanitized(self) -> Self {
        QualityInput {
            dynamics: Self::clamp_axis(self.dynamics),
            spectral: Self::clamp_axis(self.spectral),
            loudness: Self::clamp_axis(self.loudness),
            masking: Self::clamp_axis(self.masking),
            preference: Self::clamp_axis(self.preference),
            alarm_flags: self.alarm_flags,
        }
    }
}

/// The reward model's verdict for one evaluation window.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RewardScore {
    /// Unified quality, 0..1. This is the number Phase 6 A/B decisions track.
    pub overall: f32,
    pub dynamics: f32,
    pub spectral: f32,
    pub loudness: f32,
    pub masking: f32,
    pub preference: f32,
}

/// Hard bounds on composer bias — the "bias never breaks" guarantee.
pub const MAX_ENERGY_DELTA: f32 = 0.15;
pub const MAX_DENSITY_DELTA: f32 = 0.15;

/// Bounded composer bias derived from a reward score.
///
/// Mapping policy: penalties fire only on axis DEFICITS (one-sided) — a
/// healthy mix yields neutral bias, never inflated priors:
/// - collapsed dynamics → reduce density (fewer simultaneous voices)
/// - loudness shortfall → raise energy target (mix headroom, not limiter)
/// - masking penalty → lower bass energy bias (mix fix over EQ fix)
/// - healthy preference score → widen exploration budget (#24's floor)
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ComposerBias {
    #[serde(default)]
    pub energy_delta: f32,
    #[serde(default)]
    pub density_delta: f32,
    #[serde(default)]
    pub bass_energy_delta: f32,
    /// 0..0.5 fraction of parameter samples reserved for exploration.
    pub exploration_budget: f32,
}

impl Default for ComposerBias {
    fn default() -> Self {
        ComposerBias { energy_delta: 0.0, density_delta: 0.0, bass_energy_delta: 0.0, exploration_budget: 0.1 }
    }
}

/// Serializes the bias into the composer wake's taste channel so #22's
/// validated-diff loop consumes it like any other prior.
pub fn bias_into_taste_json(taste_json: &str, bias: &ComposerBias) -> String {
    let mut value: serde_json::Value =
        serde_json::from_str(taste_json).unwrap_or_else(|_| serde_json::json!({}));
    if let serde_json::Value::Object(ref mut map) = value {
        map.insert("reward_energy_delta".into(), serde_json::json!(bias.energy_delta));
        map.insert("reward_density_delta".into(), serde_json::json!(bias.density_delta));
        map.insert("reward_bass_energy_delta".into(), serde_json::json!(bias.bass_energy_delta));
        map.insert("reward_exploration_budget".into(), serde_json::json!(bias.exploration_budget));
    }
    value.to_string()
}

/// Scores quality and derives bounded composer bias. Pure and deterministic.
pub fn evaluate(input: &QualityInput, weights: &RewardWeights) -> (RewardScore, ComposerBias) {
    let input = input.clone().sanitized();
    let axes = RewardScore {
        overall: 0.0,
        dynamics: input.dynamics,
        spectral: input.spectral,
        loudness: input.loudness,
        masking: input.masking,
        preference: input.preference,
    };
    let weighted = axes.dynamics * weights.dynamics
        + axes.spectral * weights.spectral
        + axes.loudness * weights.loudness
        + axes.masking * weights.masking
        + axes.preference * weights.preference;
    let flag_penalty = (input.alarm_flags.len() as f32 * 0.05).min(0.3);
    let overall = (weighted - flag_penalty).clamp(0.0, 1.0);

    let deficit = |axis: f32, floor: f32| (floor - axis).max(0.0);
    let bias = ComposerBias {
        energy_delta: (deficit(axes.loudness, 0.6) * 0.5).clamp(0.0, MAX_ENERGY_DELTA),
        density_delta: -(deficit(axes.dynamics, 0.5) * 0.4).clamp(0.0, MAX_DENSITY_DELTA),
        bass_energy_delta: -(deficit(axes.masking, 0.7) * 0.4).clamp(0.0, MAX_ENERGY_DELTA),
        exploration_budget: (0.1 + axes.preference * 0.3).clamp(0.1, 0.4),
    };

    (RewardScore { overall, ..axes }, bias)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_must_sum_to_one() {
        assert!(RewardWeights::default().validate().is_ok());
        let bad = RewardWeights { dynamics: 0.9, ..RewardWeights::default() };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn healthy_mix_scores_high_flagged_mix_penalized() {
        let weights = RewardWeights::default();
        let healthy = QualityInput {
            dynamics: 0.9, spectral: 0.85, loudness: 0.8, masking: 0.9, preference: 0.7,
            alarm_flags: vec![],
        };
        let (score, _) = evaluate(&healthy, &weights);
        assert!(score.overall > 0.8, "healthy mix scored {}", score.overall);

        let mut flagged = healthy.clone();
        flagged.alarm_flags = vec!["gr_sustained".into(), "ceiling".into(), "sub".into()];
        let (flagged_score, _) = evaluate(&flagged, &weights);
        assert!(flagged_score.overall < score.overall, "alarms must penalize");
        assert!(flagged_score.overall >= 0.0);
    }

    #[test]
    fn hostile_input_is_sanitized_not_poisoned() {
        let weights = RewardWeights::default();
        let hostile = QualityInput {
            dynamics: f32::NAN,
            spectral: 42.0,
            loudness: -7.0,
            masking: f32::INFINITY,
            preference: 0.5,
            alarm_flags: vec![],
        };
        let (score, _) = evaluate(&hostile, &weights);
        assert!(score.overall.is_finite());
        assert!((0.0..=1.0).contains(&score.overall));
    }

    #[test]
    fn bias_is_bounded_and_directionally_correct() {
        let weights = RewardWeights::default();
        let collapsing = QualityInput {
            dynamics: 0.1, spectral: 0.5, loudness: 0.2, masking: 0.2, preference: 0.5,
            alarm_flags: vec![],
        };
        let (_, bias) = evaluate(&collapsing, &weights);
        assert!(bias.density_delta < 0.0, "collapsed dynamics must reduce density");
        assert!(bias.energy_delta > 0.0, "loudness shortfall must raise energy");
        assert!(bias.bass_energy_delta < 0.0, "masking penalty must cut bass");
        assert!(bias.density_delta >= -MAX_DENSITY_DELTA - 1e-6);
        assert!(bias.energy_delta <= MAX_ENERGY_DELTA + 1e-6);
        assert!((0.1..=0.4).contains(&bias.exploration_budget));
    }

    #[test]
    fn bias_rides_the_taste_channel() {
        let bias = ComposerBias { energy_delta: 0.1, density_delta: -0.05, bass_energy_delta: 0.0, exploration_budget: 0.2 };
        let merged = bias_into_taste_json("{\"bpm\":126.0}", &bias);
        let v: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(v["bpm"], 126.0, "existing taste fields survive");
        assert!((v["reward_energy_delta"].as_f64().unwrap() - 0.1).abs() < 1e-6);
    }

    #[test]
    fn deterministic() {
        let weights = RewardWeights::default();
        let input = QualityInput { dynamics: 0.7, spectral: 0.6, loudness: 0.65, masking: 0.8, preference: 0.55, alarm_flags: vec!["x".into()] };
        let a = evaluate(&input, &weights);
        let b = evaluate(&input, &weights);
        assert_eq!(a, b);
    }
}
