//! Wake cadence (issue #22): the composer wakes on a bar cadence (16–64
//! bars, configurable) and immediately on steering events. Steering wakes do
//! not shift the cadence grid — a user nudge mid-cycle never delays the next
//! scheduled plan.

use serde::{Deserialize, Serialize};

/// Wake cadence bounds, in bars (issue #22: every 16–64 bars).
pub const MIN_WAKE_BARS: u32 = 16;
pub const MAX_WAKE_BARS: u32 = 64;

/// How often the composer wakes, in bars.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WakeConfig {
    pub cadence_bars: u32,
}

impl WakeConfig {
    /// Clamps the cadence into the 16–64 bar window.
    pub fn new(cadence_bars: u32) -> Self {
        WakeConfig { cadence_bars: cadence_bars.clamp(MIN_WAKE_BARS, MAX_WAKE_BARS) }
    }
}

impl Default for WakeConfig {
    fn default() -> Self {
        WakeConfig::new(32)
    }
}

/// Schedules composer wakes: every `cadence_bars` bars, plus immediately on
/// a steering event.
pub struct ComposerScheduler {
    config: WakeConfig,
    next_wake_bar: u32,
    steering_pending: bool,
}

impl ComposerScheduler {
    /// First scheduled wake lands one full cadence in; hosts that want an
    /// immediate opening plan call [`Self::request_steering`] at start.
    pub fn new(config: WakeConfig) -> Self {
        ComposerScheduler { next_wake_bar: config.cadence_bars, config, steering_pending: false }
    }

    /// A steering event (user prompt / taste nudge mid-section).
    pub fn request_steering(&mut self) {
        self.steering_pending = true;
    }

    /// Poll once per bar; consumes the wake when due. Cadence wakes
    /// reschedule from the wake point; a poll past a missed boundary catches
    /// up and anchors there.
    pub fn should_wake(&mut self, current_bar: u32) -> bool {
        let steering = std::mem::take(&mut self.steering_pending);
        let cadence_due = current_bar >= self.next_wake_bar;
        if !steering && !cadence_due {
            return false;
        }
        if cadence_due {
            self.next_wake_bar = current_bar + self.config.cadence_bars;
        }
        true
    }
}

/// Why the composer woke (issue #22 wake-policy state machine).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WakeReason {
    /// The cadence grid fired (adaptively early when the lookahead margin
    /// thins, otherwise on schedule).
    Scheduled,
    /// User steering arrived: immediate.
    UserInput,
    /// The critic raised a fault flag (#25/#26): immediate.
    CriticAlarm,
    /// The watchdog logged an incident (#15): immediate, after the fact.
    PostIncident,
}

/// Lookahead margin (compiled-but-unheard bars) below which a scheduled
/// wake fires early — adaptive to the #13 pipeline instead of the bare grid.
pub const MIN_LOOKAHEAD_MARGIN_BARS: u32 = 8;

/// The wake-policy state machine (issue #22): scheduled wakes every
/// `cadence_bars` (default 32), pulled forward when the lookahead margin
/// drops below [`MIN_LOOKAHEAD_MARGIN_BARS`]; immediate wakes on
/// user input, critic alarms, and post-incident recovery. Immediate wakes
/// never shift the scheduled grid — same rule as [`ComposerScheduler`].
///
/// Priority when several reasons are pending: UserInput > CriticAlarm >
/// PostIncident > Scheduled (a pending user instruction must not be
/// masked by bookkeeping wakes).
pub struct WakePolicy {
    config: WakeConfig,
    next_scheduled_bar: u32,
    pending: Vec<WakeReason>,
}

impl WakePolicy {
    pub fn new(config: WakeConfig) -> Self {
        WakePolicy { next_scheduled_bar: config.cadence_bars, config, pending: Vec::new() }
    }

    /// Queues an immediate wake.
    pub fn request(&mut self, reason: WakeReason) {
        if matches!(reason, WakeReason::Scheduled) {
            return;
        }
        self.pending.push(reason);
    }

    /// Poll once per bar with the current lookahead margin (#13: bars
    /// compiled ahead of the playhead). Returns the reason when the
    /// composer should wake now.
    pub fn should_wake(&mut self, current_bar: u32, lookahead_margin_bars: u32) -> Option<WakeReason> {
        if let Some(reason) = take_pending(&mut self.pending) {
            return Some(reason);
        }
        let thin_margin = lookahead_margin_bars < MIN_LOOKAHEAD_MARGIN_BARS;
        if current_bar < self.next_scheduled_bar && !thin_margin {
            return None;
        }
        self.next_scheduled_bar = current_bar + self.config.cadence_bars;
        Some(WakeReason::Scheduled)
    }
}

fn take_pending(pending: &mut Vec<WakeReason>) -> Option<WakeReason> {
    if pending.is_empty() {
        return None;
    }
    let rank = |r: &WakeReason| match r {
        WakeReason::UserInput => 0,
        WakeReason::CriticAlarm => 1,
        WakeReason::PostIncident => 2,
        WakeReason::Scheduled => 3,
    };
    pending.sort_by_key(|r| rank(r));
    Some(pending.remove(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cadence_config_clamps_to_16_64_bars() {
        assert_eq!(WakeConfig::new(4).cadence_bars, MIN_WAKE_BARS);
        assert_eq!(WakeConfig::new(128).cadence_bars, MAX_WAKE_BARS);
        assert_eq!(WakeConfig::new(32).cadence_bars, 32);
    }

    #[test]
    fn scheduler_fires_at_cadence_boundaries() {
        let mut s = ComposerScheduler::new(WakeConfig::new(16));
        let wakes: Vec<u32> = (0..80).filter(|bar| s.should_wake(*bar)).collect();
        assert_eq!(wakes, vec![16, 32, 48, 64], "one wake per cadence boundary");
    }

    #[test]
    fn steering_wakes_immediately_without_shifting_cadence() {
        let mut s = ComposerScheduler::new(WakeConfig::new(32));
        let scheduled: Vec<u32> = (0..=40).filter(|bar| s.should_wake(*bar)).collect();
        assert_eq!(scheduled, vec![32], "bar 32 wakes on cadence alone");
        s.request_steering();
        let after_steering: Vec<u32> = (41..=96).filter(|bar| s.should_wake(*bar)).collect();
        assert_eq!(after_steering, vec![41, 64, 96], "steering fires at 41; the grid keeps 64 and 96");
    }

    // -- wake policy state machine (issue #22) --------------------------------

    #[test]
    fn policy_wakes_on_the_scheduled_grid() {
        let mut p = WakePolicy::new(WakeConfig::new(32));
        let wakes: Vec<Option<WakeReason>> =
            (0..=96).map(|bar| p.should_wake(bar, 32)).filter(|r| r.is_some()).collect();
        assert_eq!(
            wakes,
            vec![
                Some(WakeReason::Scheduled),
                Some(WakeReason::Scheduled),
                Some(WakeReason::Scheduled)
            ],
            "grid wakes at 32, 64, 96"
        );
    }

    #[test]
    fn thin_lookahead_margin_pulls_the_scheduled_wake_forward() {
        let mut p = WakePolicy::new(WakeConfig::new(32));
        assert_eq!(p.should_wake(20, 32), None, "healthy margin, off-grid");
        assert_eq!(
            p.should_wake(21, MIN_LOOKAHEAD_MARGIN_BARS - 1),
            Some(WakeReason::Scheduled),
            "margin below the floor: plan now, not at bar 32"
        );
        assert_eq!(p.should_wake(22, 32), None, "grid re-anchors at the wake point (53)");
    }

    #[test]
    fn immediate_reasons_wake_now_and_keep_the_grid() {
        let mut p = WakePolicy::new(WakeConfig::new(32));
        p.request(WakeReason::UserInput);
        assert_eq!(p.should_wake(7, 32), Some(WakeReason::UserInput));
        assert_eq!(p.should_wake(8, 32), None, "grid still owes bar 32");
        assert_eq!(p.should_wake(32, 32), Some(WakeReason::Scheduled));
    }

    #[test]
    fn pending_reasons_resolve_by_priority() {
        let mut p = WakePolicy::new(WakeConfig::new(32));
        p.request(WakeReason::PostIncident);
        p.request(WakeReason::CriticAlarm);
        p.request(WakeReason::UserInput);
        assert_eq!(p.should_wake(3, 32), Some(WakeReason::UserInput), "user first");
        assert_eq!(p.should_wake(3, 32), Some(WakeReason::CriticAlarm));
        assert_eq!(p.should_wake(3, 32), Some(WakeReason::PostIncident));
        assert_eq!(p.should_wake(3, 32), None);
    }

    #[test]
    fn request_ignores_scheduled() {
        let mut p = WakePolicy::new(WakeConfig::new(16));
        p.request(WakeReason::Scheduled);
        assert_eq!(p.should_wake(4, 16), None, "only the grid schedules");
    }
}
