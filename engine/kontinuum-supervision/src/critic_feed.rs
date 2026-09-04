//! Bridge from the #25 critic to the kill-switch counters (triage: "#15 …
//! kill-switch metrics from #52's critic feed it"). The critic never trips
//! the switch on its own numerics — only *raised flags* count as fault
//! windows, so a marginal-but-healthy mix cannot accumulate toward a trip.

use kontinuum_analysis::CriticVerdict;

use crate::killswitch::SafetyCounters;

/// True when the verdict raised any fault flag.
pub fn any_fault(verdict: &CriticVerdict) -> bool {
    let f = &verdict.flags;
    f.dynamics_collapsed
        || f.spectral_imbalance
        || f.sub_rumble
        || f.loudness_shortfall
        || f.loudness_excess
}

/// Records one critic evaluation window into the counters. Returns the
/// post-update critical state so callers can arm the watchdog in one call.
pub fn feed(counters: &mut SafetyCounters, verdict: &CriticVerdict, thresholds: &crate::killswitch::SafetyThresholds) -> bool {
    counters.record_critic_fault(any_fault(verdict));
    counters.is_critical(thresholds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kontinuum_analysis::{CriticFlags, CriticVerdict};

    fn clean() -> CriticVerdict {
        CriticVerdict {
            dynamics_score: 0.0,
            spectral_score: 0.0,
            low_end_score: 0.0,
            loudness_score: 0.0,
            flags: CriticFlags::default(),
        }
    }

    fn collapsed() -> CriticVerdict {
        CriticVerdict { flags: CriticFlags { dynamics_collapsed: true, ..CriticFlags::default() }, ..clean() }
    }

    #[test]
    fn clean_windows_never_accumulate() {
        let mut c = SafetyCounters::default();
        for _ in 0..20 {
            assert!(!feed(&mut c, &clean(), &crate::killswitch::SafetyThresholds::default()));
        }
        assert_eq!(c.critic_faults, 0);
    }

    #[test]
    fn sustained_faults_trip_exactly_at_threshold() {
        let t = crate::killswitch::SafetyThresholds::default();
        let mut c = SafetyCounters::default();
        for _ in 0..t.max_critic_faults - 1 {
            assert!(!feed(&mut c, &collapsed(), &t));
        }
        assert!(feed(&mut c, &collapsed(), &t), "the 12th fault window trips");
        assert_eq!(c.critic_faults, u64::from(t.max_critic_faults));
    }
}
