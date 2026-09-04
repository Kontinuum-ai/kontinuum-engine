//! Watchdog (#15): delegates to the primary [`BlockSource`] while it works,
//! contains failures (including panics) and switches to the fallback
//! arrangement, then probes the primary periodically to recover. The music
//! never stops: once fallback engages, every request is answered.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use kontinuum_schedule::{BlockSource, CompiledBlock};

use crate::fallback::FallbackSource;

/// Tunables for [`Watchdog`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatchdogPolicy {
    /// Consecutive primary failures (misses or panics) before switching to
    /// the fallback arrangement.
    pub max_consecutive_failures: u32,
    /// While in fallback, probe the primary every this many served blocks.
    pub probe_interval_blocks: u32,
}

impl Default for WatchdogPolicy {
    fn default() -> Self {
        WatchdogPolicy { max_consecutive_failures: 3, probe_interval_blocks: 8 }
    }
}

/// Watchdog operating state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchdogState {
    /// Primary serving; zero consecutive failures.
    Healthy,
    /// Primary missing but below the failure budget; still delegating.
    Degraded,
    /// Serving the fallback arrangement; probing the primary periodically.
    Fallback,
}

/// Counters snapshot for health dashboards and the kill switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchdogHealth {
    pub state: WatchdogState,
    pub consecutive_failures: u32,
    pub fallback_blocks_served: u64,
    pub primary_blocks_served: u64,
    pub probes: u64,
    pub recoveries: u64,
}

/// Wraps a primary source with failure containment and the fallback
/// arrangement generator. The audio thread only ever sees `Some` once the
/// fallback has engaged; before that, misses surface as `None` so the
/// scheduler can retry on its next tick.
pub struct Watchdog<P: BlockSource> {
    primary: P,
    fallback: FallbackSource,
    policy: WatchdogPolicy,
    state: WatchdogState,
    consecutive_failures: u32,
    blocks_since_probe: u32,
    health_counters: Health,
}

#[derive(Default)]
struct Health {
    fallback_blocks_served: u64,
    primary_blocks_served: u64,
    probes: u64,
    recoveries: u64,
}

impl<P: BlockSource> Watchdog<P> {
    pub fn new(primary: P, fallback: FallbackSource, policy: WatchdogPolicy) -> Self {
        Watchdog {
            primary,
            fallback,
            policy,
            state: WatchdogState::Healthy,
            consecutive_failures: 0,
            blocks_since_probe: 0,
            health_counters: Health::default(),
        }
    }

    /// Point-in-time health snapshot (Copy).
    pub fn health(&self) -> WatchdogHealth {
        WatchdogHealth {
            state: self.state,
            consecutive_failures: self.consecutive_failures,
            fallback_blocks_served: self.health_counters.fallback_blocks_served,
            primary_blocks_served: self.health_counters.primary_blocks_served,
            probes: self.health_counters.probes,
            recoveries: self.health_counters.recoveries,
        }
    }

    /// The fallback arrangement (its session is what a supervisor should
    /// snapshot while the watchdog is in [`WatchdogState::Fallback`]).
    pub fn fallback(&self) -> &FallbackSource {
        &self.fallback
    }

    pub fn policy(&self) -> &WatchdogPolicy {
        &self.policy
    }

    /// One primary attempt, panics contained: a panicking planner counts as
    /// a miss and never propagates to the scheduler thread.
    fn try_primary(&mut self, start_bar: u32, bars: u32) -> Option<Arc<CompiledBlock>> {
        catch_unwind(AssertUnwindSafe(|| self.primary.block_for_bars(start_bar, bars)))
            .unwrap_or_default()
    }

    /// Answers the current request from the fallback and advances the
    /// probe cadence.
    fn serve_fallback(&mut self, start_bar: u32, bars: u32) -> Option<Arc<CompiledBlock>> {
        self.state = WatchdogState::Fallback;
        let block = self.fallback.block_for_bars(start_bar, bars);
        self.health_counters.fallback_blocks_served += 1;
        self.blocks_since_probe += 1;
        block
    }
}

impl<P: BlockSource> BlockSource for Watchdog<P> {
    fn block_for_bars(&mut self, start_bar: u32, bars: u32) -> Option<Arc<CompiledBlock>> {
        if self.state == WatchdogState::Fallback {
            if self.blocks_since_probe >= self.policy.probe_interval_blocks {
                self.blocks_since_probe = 0;
                self.health_counters.probes += 1;
                if let Some(block) = self.try_primary(start_bar, bars) {
                    // Primary is back: serve its block for this request and
                    // hand control back to it.
                    self.state = WatchdogState::Healthy;
                    self.consecutive_failures = 0;
                    self.health_counters.recoveries += 1;
                    self.health_counters.primary_blocks_served += 1;
                    return Some(block);
                }
            }
            return self.serve_fallback(start_bar, bars);
        }

        if let Some(block) = self.try_primary(start_bar, bars) {
            self.state = WatchdogState::Healthy;
            self.consecutive_failures = 0;
            self.health_counters.primary_blocks_served += 1;
            return Some(block);
        }

        self.consecutive_failures += 1;
        if self.consecutive_failures >= self.policy.max_consecutive_failures {
            // Failure budget spent: engage the fallback for this very request.
            self.serve_fallback(start_bar, bars)
        } else {
            self.state = WatchdogState::Degraded;
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kontinuum_schedule::CompiledBlock;

    /// Primary that succeeds except in the `[fail_from, fail_until]` call
    /// window (1-based call numbers), where it either misses or panics.
    struct Flaky {
        calls: u32,
        fail_from: u32,
        fail_until: u32,
        panic_instead: bool,
    }

    impl Flaky {
        fn misses(fail_from: u32, fail_until: u32) -> Self {
            Flaky { calls: 0, fail_from, fail_until, panic_instead: false }
        }

        fn panics(fail_from: u32, fail_until: u32) -> Self {
            Flaky { calls: 0, fail_from, fail_until, panic_instead: true }
        }

        fn failing(&self) -> bool {
            self.calls >= self.fail_from && self.calls <= self.fail_until
        }
    }

    impl BlockSource for Flaky {
        fn block_for_bars(&mut self, start_bar: u32, bars: u32) -> Option<Arc<CompiledBlock>> {
            self.calls += 1;
            if self.failing() {
                if self.panic_instead {
                    panic!("planner exploded mid-arrangement");
                }
                return None;
            }
            Some(Arc::new(CompiledBlock {
                start_bar,
                bars,
                ..CompiledBlock::default()
            }))
        }
    }

    fn watchdog(primary: Flaky, max_failures: u32, probe_every: u32) -> Watchdog<Flaky> {
        Watchdog::new(
            primary,
            FallbackSource::new(5, 48_000),
            WatchdogPolicy {
                max_consecutive_failures: max_failures,
                probe_interval_blocks: probe_every,
            },
        )
    }

    #[test]
    fn policy_defaults_are_sane() {
        let p = WatchdogPolicy::default();
        assert_eq!((p.max_consecutive_failures, p.probe_interval_blocks), (3, 8));
    }

    #[test]
    fn fallback_engages_after_consecutive_misses() {
        // 2 good calls, then permanent misses; budget of 3.
        let mut wd = watchdog(Flaky::misses(3, u32::MAX), 3, 8);
        for i in 0..2u32 {
            let b = wd.block_for_bars(i * 4, 4).expect("primary still works");
            assert_eq!(b.start_bar, i * 4);
        }
        assert_eq!(wd.health().state, WatchdogState::Healthy);

        // Calls 3 and 4: primary misses, degraded, scheduler sees None.
        assert!(wd.block_for_bars(8, 4).is_none());
        assert_eq!(wd.health().state, WatchdogState::Degraded);
        assert!(wd.block_for_bars(12, 4).is_none());

        // Call 5: budget spent, fallback engages and answers this request.
        let b = wd.block_for_bars(16, 4).expect("music never stops");
        assert_eq!(b.start_bar, 16);
        assert!(b.total_events() > 0, "block came from the fallback arrangement");

        let h = wd.health();
        assert_eq!(h.state, WatchdogState::Fallback);
        assert_eq!(h.consecutive_failures, 3);
        assert_eq!((h.primary_blocks_served, h.fallback_blocks_served), (2, 1));
    }

    #[test]
    fn recovers_when_primary_comes_back() {
        // Miss on calls 4..=6, healthy before and after.
        let mut wd = watchdog(Flaky::misses(4, 6), 3, 4);
        for i in 0..3u32 {
            assert!(wd.block_for_bars(i * 4, 4).is_some());
        }
        // Calls 4, 5 miss (degraded, scheduler sees None); call 6 trips the
        // budget -> fallback.
        assert!(wd.block_for_bars(12, 4).is_none());
        assert!(wd.block_for_bars(16, 4).is_none());
        assert!(wd.block_for_bars(20, 4).is_some());
        assert_eq!(wd.health().state, WatchdogState::Fallback);

        // Calls 7..9 are fallback-served (3 since probe); call 10 hits the
        // probe cadence, the healed primary answers, and we recover.
        for i in 7..10u32 {
            assert!(wd.block_for_bars(i * 4, 4).is_some());
        }
        let healed = wd.block_for_bars(40, 4).expect("probe recovers");
        assert_eq!(healed.total_events(), 0, "probe block came from the (empty) primary");

        let h = wd.health();
        assert_eq!(h.state, WatchdogState::Healthy);
        assert_eq!((h.probes, h.recoveries), (1, 1));
        assert_eq!(h.consecutive_failures, 0);
    }

    #[test]
    fn panicking_primary_is_contained() {
        let mut wd = watchdog(Flaky::panics(1, u32::MAX), 3, 8);
        // First two calls: panic contained, reported as misses.
        assert!(wd.block_for_bars(0, 4).is_none());
        assert!(wd.block_for_bars(4, 4).is_none());
        // Third call: budget spent, fallback serves real music.
        let b = wd.block_for_bars(8, 4).expect("fallback after panics");
        assert!(b.total_events() > 0);
        assert_eq!(wd.health().state, WatchdogState::Fallback);
        // Fallback keeps serving arbitrary far ranges.
        assert!(wd.block_for_bars(1_000_000, 4).is_some());
    }

    #[test]
    fn probe_failure_keeps_fallback_serving() {
        let mut wd = watchdog(Flaky::misses(4, u32::MAX), 3, 2);
        // Given a primary that works for three calls then misses forever:
        assert!(wd.block_for_bars(0, 4).is_some());
        assert!(wd.block_for_bars(4, 4).is_some());
        assert!(wd.block_for_bars(8, 4).is_some());
        // When two degraded misses burn down to the failure budget:
        assert!(wd.block_for_bars(12, 4).is_none());
        assert!(wd.block_for_bars(16, 4).is_none());
        // Then the third miss engages the fallback:
        assert!(wd.block_for_bars(20, 4).is_some());
        assert_eq!(wd.health().state, WatchdogState::Fallback);
        // And every further request is answered, probes failing on cadence.
        for i in 6..12u32 {
            let b = wd.block_for_bars(i * 4, 4).expect("music never stops");
            assert!(b.total_events() > 0);
        }
        let h = wd.health();
        assert_eq!(h.state, WatchdogState::Fallback);
        assert_eq!(h.recoveries, 0);
        assert_eq!(h.probes, 3);
        assert_eq!(h.fallback_blocks_served, 7);
    }
}
