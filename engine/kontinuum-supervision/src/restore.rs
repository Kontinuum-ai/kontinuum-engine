//! Session snapshot / restore (#15): capture what is playing (session +
//! playhead), persist it as JSON, and rebuild a live [`BlockSource`] from it.
//! A snapshot that no longer validates degrades to the built-in fallback
//! arrangement instead of failing — restoration must never leave the engine
//! without a source.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use kontinuum_ir::compile::BLOCK_BARS;
use kontinuum_ir::schema::Session;
use kontinuum_ir::validate_session;
use kontinuum_schedule::{BlockSource, CompiledBlock};

use crate::fallback::FallbackSource;

/// Everything needed to resume playback on this process or a fresh one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSnapshot {
    pub session: Session,
    /// Bar the playhead sat at when the snapshot was taken.
    pub playhead_bar: u32,
    /// Pre-compiled blocks the engine had queued ahead of the playhead at
    /// snapshot time. [`save`] cannot know this, so it stores 0 and the
    /// engine patches the field before persisting.
    pub saved_blocks_ahead: u32,
}

/// Captures the engine's live session at `playhead_bar`.
pub fn save(engine_session: &Session, playhead_bar: u32) -> SessionSnapshot {
    SessionSnapshot {
        session: engine_session.clone(),
        playhead_bar,
        saved_blocks_ahead: 0,
    }
}

/// Pretty JSON form (the on-disk / cross-process format).
pub fn to_json(snap: &SessionSnapshot) -> serde_json::Result<String> {
    serde_json::to_string_pretty(snap)
}

/// Parses a snapshot produced by [`to_json`].
pub fn from_json(json: &str) -> serde_json::Result<SessionSnapshot> {
    serde_json::from_str(json)
}

/// First 4-bar boundary at or after the playhead: where the restored
/// scheduler resumes compiling.
pub fn resume_bar(playhead_bar: u32) -> u32 {
    let over = u64::from(playhead_bar) % u64::from(BLOCK_BARS);
    if over == 0 {
        playhead_bar
    } else {
        ((u64::from(playhead_bar) / u64::from(BLOCK_BARS) + 1) * u64::from(BLOCK_BARS))
            .min(u32::MAX as u64) as u32
    }
}

/// A session brought back to life from a [`SessionSnapshot`]. Serves the
/// snapshot's session with endless wrap-around; if the session failed
/// validation it serves the built-in fallback arrangement instead and
/// reports degraded via [`RestoredSource::is_fallback`].
pub struct RestoredSource {
    src: FallbackSource,
    playhead_bar: u32,
    degraded: bool,
}

impl RestoredSource {
    /// True when the snapshot session was invalid and the built-in fallback
    /// arrangement is playing in its place.
    pub fn is_fallback(&self) -> bool {
        self.degraded
    }

    /// Where the scheduler should resume (next 4-bar boundary >= playhead).
    pub fn resume_bar(&self) -> u32 {
        resume_bar(self.playhead_bar)
    }

    /// The session actually being served (snapshot or fallback).
    pub fn session(&self) -> &Session {
        self.src.session()
    }
}

impl BlockSource for RestoredSource {
    fn block_for_bars(&mut self, start_bar: u32, bars: u32) -> Option<Arc<CompiledBlock>> {
        self.src.block_for_bars(start_bar, bars)
    }
}

/// Rebuilds a live source from a snapshot. The session is re-validated at
/// the boundary: only clean sessions drive playback.
pub fn restore(snap: &SessionSnapshot, sample_rate: u32) -> RestoredSource {
    match validate_session(&snap.session) {
        Ok(()) => RestoredSource {
            src: FallbackSource::from_session(snap.session.clone(), sample_rate),
            playhead_bar: snap.playhead_bar,
            degraded: false,
        },
        Err(_) => RestoredSource {
            src: FallbackSource::new(snap.session.seed, sample_rate),
            playhead_bar: snap.playhead_bar,
            degraded: true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kontinuum_ir::compile_session;

    fn snapshot_of_valid_session() -> (Session, SessionSnapshot) {
        let session = FallbackSource::new(11, 48_000).session().clone();
        (session.clone(), save(&session, 6))
    }

    #[test]
    fn json_roundtrip_preserves_snapshot() {
        let (session, snap) = snapshot_of_valid_session();
        assert_eq!(snap.playhead_bar, 6);
        let json = to_json(&snap).expect("serialize");
        assert!(json.contains('\n'), "pretty-printed");
        let back = from_json(&json).expect("parse");
        assert_eq!(back, snap);
        assert_eq!(back.session, session);
    }

    #[test]
    fn restored_blocks_match_direct_compile() {
        let (session, snap) = snapshot_of_valid_session();
        let restored = restore(&snap, 48_000);
        assert!(!restored.is_fallback());
        assert_eq!(restored.resume_bar(), 8, "playhead 6 resumes at the next 4-bar boundary");

        let direct = compile_session(&session, 48_000).expect("compile");
        let mut restored_src = restored;
        for b in &direct {
            let got = restored_src
                .block_for_bars(b.start_bar, b.bars)
                .unwrap_or_else(|| panic!("no block at {}", b.start_bar));
            assert_eq!(got.start_frame, b.start_frame);
            assert_eq!(
                format!("{:?}", got.tracks),
                format!("{:?}", b.tracks),
                "event vectors must be identical at bar {}",
                b.start_bar
            );
        }
    }

    #[test]
    fn invalid_snapshot_degrades_to_fallback() {
        let mut session = FallbackSource::new(3, 48_000).session().clone();
        session.sections.clear();
        let snap = save(&session, 10);
        let restored = restore(&snap, 48_000);
        assert!(restored.is_fallback());
        assert!(
            validate_session(restored.session()).is_ok(),
            "degraded restore must serve a valid session"
        );
        let mut src = restored;
        let b = src.block_for_bars(resume_bar(10), 4).expect("blocks still flow");
        assert!(b.total_events() > 0);
    }

    #[test]
    fn resume_bar_snaps_forward_to_4_bar_grid() {
        assert_eq!(resume_bar(0), 0);
        assert_eq!(resume_bar(1), 4);
        assert_eq!(resume_bar(6), 8);
        assert_eq!(resume_bar(8), 8);
        assert_eq!(resume_bar(u32::MAX), u32::MAX, "saturates instead of overflowing");
    }
}
