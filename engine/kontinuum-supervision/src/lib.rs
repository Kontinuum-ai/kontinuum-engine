//! `kontinuum-supervision` — watchdog, fallback arrangements, kill-switch
//! metrics, session restore (#15).
//!
//! AI-failure containment: the music NEVER stops, even when the composer /
//! watchdog stack misbehaves.
//!
//! - [`fallback::FallbackSource`] serves a built-in safe arrangement for any
//!   bar range, forever (wrap-around).
//! - [`watchdog::Watchdog`] delegates to the primary planner, contains
//!   misses *and* panics, switches to the fallback after a failure budget,
//!   and probes periodically to recover.
//! - [`killswitch::SafetyCounters`] accumulates safety telemetry and trips
//!   the kill switch against [`killswitch::SafetyThresholds`].
//! - [`restore`] snapshots the live session to JSON and rebuilds a playing
//!   source from it, degrading to the fallback if the session went bad.

pub mod fallback;
pub mod killswitch;
pub mod critic_feed;
pub mod restore;
pub mod watchdog;

pub use fallback::{FallbackSource, FALLBACK_BARS};
pub use killswitch::{SafetyCounters, SafetyThresholds};
pub use restore::{RestoredSource, SessionSnapshot};
pub use watchdog::{Watchdog, WatchdogHealth, WatchdogPolicy, WatchdogState};

#[cfg(test)]
mod tests {
    use super::*;
    use kontinuum_ir::IR_VERSION;
    use kontinuum_schedule::{BlockSource, CompiledBlock};
    use std::sync::Arc;

    /// Primary planner whose session gets corrupted mid-flight: it serves a
    /// few blocks, then starts returning `None` for everything.
    struct CorruptingPrimary {
        calls: u32,
        dies_after: u32,
    }

    impl BlockSource for CorruptingPrimary {
        fn block_for_bars(&mut self, _start: u32, _bars: u32) -> Option<Arc<CompiledBlock>> {
            self.calls += 1;
            if self.calls > self.dies_after {
                None
            } else {
                Some(Arc::new(CompiledBlock::default()))
            }
        }
    }

    #[test]
    fn corrupted_session_contained_then_restored() {
        // Given a healthy-looking primary that dies after 2 blocks:
        let mut wd = Watchdog::new(
            CorruptingPrimary { calls: 0, dies_after: 2 },
            FallbackSource::new(20260830, 48_000),
            WatchdogPolicy::default(),
        );

        // When the engine pumps blocks through the watchdog:
        let mut served = 0u32;
        for i in 0..12u32 {
            if wd.block_for_bars(i * 4, 4).is_some() {
                served += 1;
            }
        }
        // Then the watchdog never starved the pipeline for long and is now
        // playing the fallback arrangement.
        assert_eq!(served, 10, "2 primary blocks + 8 fallback blocks");
        let h = wd.health();
        assert_eq!(h.state, WatchdogState::Fallback);
        assert_eq!(h.fallback_blocks_served, 8);

        // When supervision snapshots whatever is actually playing:
        let snap = restore::save(wd.fallback().session(), 32);
        let json = restore::to_json(&snap).expect("snapshot serializes");
        let reloaded = restore::from_json(&json).expect("snapshot parses");

        // Then restore hands back a live source that keeps blocks flowing.
        let restored = restore::restore(&reloaded, 48_000);
        assert!(!restored.is_fallback(), "the fallback session is valid");
        assert_eq!(restored.resume_bar(), 32);
        let mut src = restored;
        let block = src
            .block_for_bars(src.resume_bar(), 4)
            .expect("restored source serves blocks");
        assert_eq!(block.start_bar, 32);
        assert!(block.total_events() > 0, "and it is real music, not silence");
    }

    #[test]
    fn snapshot_type_is_stable_across_json() {
        // The snapshot format is the crash-recovery contract: it must not
        // drift silently. A hand-written minimal document must keep parsing.
        let doc = r#"{
            "session": {
                "version": 1, "seed": 1,
                "tempo_lane": [[0, 120.0]],
                "sections": [{"id": "a", "bars": 4, "energy_curve": [0.5],
                    "pattern_bindings": {"k": {"generator": "euclidean", "k": 4, "n": 16}}}],
                "tracks": [{"id": "k", "role": "kick", "instrument": {"kind": "kick"}}]
            },
            "playhead_bar": 4,
            "saved_blocks_ahead": 2
        }"#;
        let snap: SessionSnapshot = serde_json::from_str(doc).expect("format drift");
        assert_eq!(snap.playhead_bar, 4);
        assert_eq!(snap.saved_blocks_ahead, 2);
        assert_eq!(snap.session.version, IR_VERSION);
        let restored = restore::restore(&snap, 48_000);
        assert!(!restored.is_fallback());
    }
}
