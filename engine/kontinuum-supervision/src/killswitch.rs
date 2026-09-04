//! Kill-switch metrics (#15): safety counters the engine bumps as it runs,
//! threshold config, and the `is_critical` verdict that trips the kill
//! switch. Counters only ever grow; thresholds decide when supervision must
//! stop trusting the AI stack.

use serde::{Deserialize, Serialize};

/// Running safety counters (monotonic).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyCounters {
    /// IR diffs rejected by validation (composer misbehaving).
    pub invalid_diffs_rejected: u64,
    /// Times the watchdog switched to the fallback arrangement.
    pub watchdog_fallbacks: u64,
    /// Times the RT block queue starved (the audio thread found no block).
    pub render_gaps: u64,
    /// Audible playback dropouts.
    pub dropouts: u64,
    /// Evaluation windows in which the #25 critic raised any fault flag.
    pub critic_faults: u64,
    /// Live per-lap regenerations (vary/validate/compile) that failed or
    /// panicked and were contained by the engine's supervision wrapper
    /// (issue #81 wiring): the previous known-good lap kept playing.
    pub regeneration_failures: u64,
    /// Latched mastering limiter-alarm episodes (#28 wiring): the chain's
    /// gain reduction stayed past its 3 dB policy cap for longer than the
    /// sustain window. The ceiling stays enforced (a pathological mix can
    /// sound dull, never destroyed), so one latch is contained telemetry;
    /// repeated latches mean the program is chronically over the ceiling
    /// and supervision must act.
    pub mastering_gr_alarms: u64,
}

impl SafetyCounters {
    pub fn record_invalid_diff(&mut self) {
        self.invalid_diffs_rejected += 1;
    }

    pub fn record_watchdog_fallback(&mut self) {
        self.watchdog_fallbacks += 1;
    }

    pub fn record_render_gap(&mut self) {
        self.render_gaps += 1;
    }

    pub fn record_dropout(&mut self) {
        self.dropouts += 1;
    }

    /// Records one critic evaluation (#25): any raised flag counts as one
    /// fault window. Returns true when a fault was counted.
    pub fn record_critic_fault(&mut self, any_flag_raised: bool) -> bool {
        if any_flag_raised {
            self.critic_faults += 1;
        }
        any_flag_raised
    }

    /// True when any thresholded counter has reached its limit ("max" is the
    /// last tolerated value; reaching it trips). Dropouts carry no threshold:
    /// they are symptom telemetry, not a trip condition.
    pub fn is_critical(&self, t: &SafetyThresholds) -> bool {
        self.invalid_diffs_rejected >= u64::from(t.max_invalid_diffs_per_hour)
            || self.watchdog_fallbacks >= u64::from(t.max_fallbacks)
            || self.render_gaps >= u64::from(t.max_gaps)
            || self.critic_faults >= u64::from(t.max_critic_faults)
            || self.mastering_gr_alarms >= u64::from(t.max_mastering_gr_alarms)
    }

    /// Records one contained live-regeneration failure (issue #81 wiring).
    pub fn record_regeneration_failure(&mut self) {
        self.regeneration_failures += 1;
    }

    /// Records one latched mastering limiter-alarm episode (#28): call
    /// when the chain's sustained-GR alarm latches (rising edge), not per
    /// block — the counter tracks episodes.
    pub fn record_mastering_gr_alarm(&mut self) {
        self.mastering_gr_alarms += 1;
    }

    /// JSON snapshot for dashboards / telemetry pipelines.
    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }
}

/// Kill-switch thresholds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyThresholds {
    /// Invalid diffs tolerated per hour before tripping.
    pub max_invalid_diffs_per_hour: u32,
    /// Watchdog fallback engagements tolerated before tripping.
    pub max_fallbacks: u32,
    /// Queue-starve render gaps tolerated before tripping.
    pub max_gaps: u32,
    /// Critic fault windows tolerated before tripping.
    pub max_critic_faults: u32,
    /// Latched mastering GR-alarm episodes tolerated before tripping
    /// (#28): the limiter ceiling stays enforced through every latch, so
    /// the first sustained breach is contained; a second means the
    /// program chronically lives over the ceiling and the mix — not the
    /// limiter — must carry loudness.
    pub max_mastering_gr_alarms: u32,
}

impl Default for SafetyThresholds {
    fn default() -> Self {
        SafetyThresholds {
            max_invalid_diffs_per_hour: 50,
            max_fallbacks: 5,
            max_gaps: 3,
            max_critic_faults: 12,
            max_mastering_gr_alarms: 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_trip_exactly_at_defaults() {
        let t = SafetyThresholds::default();
        let mut c = SafetyCounters::default();
        assert!(!c.is_critical(&t), "zero counters are safe");

        // 49 invalid diffs: safe. The 50th trips.
        for _ in 0..t.max_invalid_diffs_per_hour - 1 {
            c.record_invalid_diff();
        }
        assert!(!c.is_critical(&t));
        c.record_invalid_diff();
        assert!(c.is_critical(&t), "50 invalid diffs must trip");

        // Same probe for the other two counters, each in isolation.
        let mut c = SafetyCounters::default();
        for _ in 0..t.max_fallbacks - 1 {
            c.record_watchdog_fallback();
        }
        assert!(!c.is_critical(&t));
        c.record_watchdog_fallback();
        assert!(c.is_critical(&t), "5 fallbacks must trip");

        let mut c = SafetyCounters::default();
        for _ in 0..t.max_gaps - 1 {
            c.record_render_gap();
        }
        assert!(!c.is_critical(&t));
        c.record_render_gap();
        assert!(c.is_critical(&t), "3 render gaps must trip");
    }

    #[test]
    fn dropouts_alone_never_trip() {
        let t = SafetyThresholds::default();
        let mut c = SafetyCounters::default();
        for _ in 0..10_000 {
            c.record_dropout();
        }
        assert!(!c.is_critical(&t));
    }

    #[test]
    fn recording_mutates_exactly_one_counter() {
        let mut c = SafetyCounters::default();
        c.record_invalid_diff();
        assert_eq!(
            c,
            SafetyCounters { invalid_diffs_rejected: 1, ..SafetyCounters::default() }
        );
        c.record_watchdog_fallback();
        c.record_render_gap();
        c.record_dropout();
        c.record_mastering_gr_alarm();
        assert_eq!(
            c,
            SafetyCounters {
                invalid_diffs_rejected: 1,
                watchdog_fallbacks: 1,
                render_gaps: 1,
                dropouts: 1,
                critic_faults: 0,
                regeneration_failures: 0,
                mastering_gr_alarms: 1,
            }
        );
    }

    #[test]
    fn sustained_mastering_gr_alarms_trip_at_the_default_budget() {
        // One latched episode is contained (the ceiling stays enforced);
        // the second is the chronic-over-ceiling verdict.
        let t = SafetyThresholds::default();
        let mut c = SafetyCounters::default();
        c.record_mastering_gr_alarm();
        assert!(!c.is_critical(&t), "one sustained breach is contained telemetry");
        c.record_mastering_gr_alarm();
        assert!(c.is_critical(&t), "a second sustained breach must trip");
    }

    #[test]
    fn snapshot_carries_all_counters() {
        let mut c = SafetyCounters::default();
        c.record_dropout();
        c.record_dropout();
        let snap = c.snapshot();
        assert_eq!(snap["dropouts"], 2);
        assert_eq!(snap["invalid_diffs_rejected"], 0);
        assert_eq!(snap["watchdog_fallbacks"], 0);
        assert_eq!(snap["render_gaps"], 0);
    }

    #[test]
    fn custom_thresholds_are_honored() {
        let t = SafetyThresholds { max_fallbacks: 1, ..SafetyThresholds::default() };
        let mut c = SafetyCounters::default();
        c.record_watchdog_fallback();
        assert!(c.is_critical(&t), "a limit of 1 trips on the first fallback");
    }
}
