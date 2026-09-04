//! Block hot-swapping arrangement engine (issues #13/#17): exposes a
//! [`Session`] through the [`BlockSource`] trait behind a bar-indexed cache.
//!
//! Cache semantics: `compile_session` emits chained 4-bar blocks, cached by
//! start bar. A cache miss recompiles the whole session once and swaps all
//! blocks in (per-diff incremental recompilation is tracked in issue #17);
//! blocks **before** a pending diff boundary are kept from cache, so past
//! audio is bit-identical and only future blocks change.

use std::collections::BTreeMap;
use std::sync::Arc;

use kontinuum_ir::{apply_diff as ir_apply_diff, compile_session, validate_session, ApplyError, ApplyReport, IrDiff, Session};
use kontinuum_schedule::{BlockSource, CompiledBlock, TrackEvents};

/// Compile granularity; `compile_session` emits chained blocks of this size.
const BLOCK_BARS: u32 = kontinuum_ir::compile::BLOCK_BARS;

pub struct ArrangementEngine {
    session: Session,
    sample_rate: u32,
    cache: BTreeMap<u32, Arc<CompiledBlock>>,
    pending_pack: Option<(u8, u64)>,
}

impl ArrangementEngine {
    /// Panics when `session` fails `validate_session`: generated sessions are
    /// valid by construction, and LLM-authored sessions must pass validation
    /// (with its actionable error catalog) before they reach the engine.
    pub fn new(session: Session, sample_rate: u32) -> Self {
        if let Err(errors) = validate_session(&session) {
            panic!("ArrangementEngine refuses an invalid session: {errors:?}");
        }
        ArrangementEngine { session, sample_rate, cache: BTreeMap::new(), pending_pack: None }
    }

    pub fn current_session(&self) -> &Session {
        &self.session
    }

    /// Requests a rendered pack (recipe hash from [`kontinuum_samples`] or
    /// the session's \`SampleSlot.recipe_hash\`) hot-loaded onto \`track\`'s
    /// sampler at the next musical boundary (#53 step 3). The host consumes
    /// the request when the boundary lands and calls \`attach_sampler\`.
    pub fn request_pack(&mut self, track: u8, recipe_hash: u64) {
        self.pending_pack = Some((track, recipe_hash));
    }

    /// The pending hot-load request, consumed exactly once by the host.
    pub fn take_pending_pack(&mut self) -> Option<(u8, u64)> {
        self.pending_pack.take()
    }

    /// Applies `diff` at the playhead bar, then drops cached blocks starting
    /// at or after `at_bar` rounded down to the 4-bar boundary. In-flight
    /// blocks already published to the RT queue are unaffected (the rolling
    /// buffer is the boundary guarantee); the next `block_for_bars` call past
    /// the boundary recompiles and swaps.
    pub fn apply_diff(&mut self, diff: &IrDiff, at_bar: u32) -> Result<ApplyReport, ApplyError> {
        let report = ir_apply_diff(&mut self.session, diff, at_bar)?;
        let boundary = (at_bar / BLOCK_BARS) * BLOCK_BARS;
        self.cache.retain(|&start_bar, _| start_bar < boundary);
        Ok(report)
    }

    /// True when the cache holds a chain of blocks covering
    /// `[start_bar, start_bar + bars)`.
    fn covers(&self, start_bar: u32, bars: u32) -> bool {
        let Some(end) = start_bar.checked_add(bars) else {
            return false;
        };
        let mut bar = start_bar;
        while bar < end {
            match self.cache.get(&bar) {
                Some(b) => bar += b.bars,
                None => return false,
            }
        }
        true
    }

    fn refresh_cache(&mut self) -> Option<()> {
        let blocks = compile_session(&self.session, self.sample_rate).ok()?;
        self.cache = blocks.into_iter().map(|b| (b.start_bar, b)).collect();
        Some(())
    }

    /// Concatenates consecutive cached blocks into one block spanning
    /// `[start_bar, start_bar + bars)`. Merged blocks are rebuilt per request
    /// and never cached (cache entries are always compiler-granularity).
    fn merged(&self, start_bar: u32, bars: u32) -> Option<CompiledBlock> {
        let end = start_bar.checked_add(bars)?;
        let mut parts = Vec::new();
        let mut bar = start_bar;
        while bar < end {
            let part = self.cache.get(&bar)?;
            parts.push(Arc::clone(part));
            bar += part.bars;
        }
        let first = parts.first()?;
        let start_frame = first.start_frame;
        let mut tracks: Vec<TrackEvents> = Vec::new();
        for part in &parts {
            let offset = part.start_frame - start_frame;
            for te in &part.tracks {
                let shifted = te
                    .events
                    .iter()
                    .map(|(f, e)| ((u64::from(*f) + offset).min(u64::from(u32::MAX)) as u32, *e));
                match tracks.iter_mut().find(|t| t.track == te.track) {
                    Some(t) => t.events.extend(shifted),
                    None => tracks.push(TrackEvents {
                        track: te.track,
                        events: shifted.collect(),
                    }),
                }
            }
        }
        for te in &mut tracks {
            te.events.sort_by_key(|(f, _)| *f);
        }
        Some(CompiledBlock { start_bar, bars, start_frame, tracks })
    }
}

impl BlockSource for ArrangementEngine {
    fn block_for_bars(&mut self, start_bar: u32, bars: u32) -> Option<Arc<CompiledBlock>> {
        if u64::from(start_bar) >= self.session.total_bars() {
            return None;
        }
        if !self.covers(start_bar, bars) {
            self.refresh_cache()?;
        }
        if bars == BLOCK_BARS {
            return self.cache.get(&start_bar).map(Arc::clone);
        }
        self.merged(start_bar, bars).map(Arc::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dj::{DjDeck, LoopLength};
    use crate::{generate_session, GenParams};

    const SR: u32 = 48_000;

    fn engine(seed: u64) -> ArrangementEngine {
        let params = GenParams { seed, target_bars: 32, ..GenParams::default() };
        ArrangementEngine::new(generate_session(&params), SR)
    }

    fn warm_blocks(engine: &mut ArrangementEngine, total: u32) -> Vec<Arc<CompiledBlock>> {
        let mut blocks = Vec::new();
        let mut bar = 0;
        while bar < total {
            let b = engine.block_for_bars(bar, 4).expect("warm block");
            bar += b.bars;
            blocks.push(b);
        }
        blocks
    }

    #[test]
    fn loop_region_keeps_block_cache_stable() {
        let mut engine = engine(11);
        let starts = engine.current_session().section_start_bars();
        let playing = starts[1] + 1;
        let dev_bars = engine.current_session().sections[1].bars;
        let total = engine.current_session().total_bars() as u32;
        let before = warm_blocks(&mut engine, total);
        let keys_before: Vec<u32> = engine.cache.keys().copied().collect();

        let mut deck = DjDeck::new();
        let looped = deck
            .loop_current_section(LoopLength::Half, playing, &mut engine)
            .expect("loop");
        assert_eq!(looped.section, engine.current_session().sections[1].id);
        assert_eq!(looped.extra_bars, dev_bars / 2);
        assert_eq!(looped.landing_bar, starts[1] + dev_bars);

        let boundary = (playing / BLOCK_BARS) * BLOCK_BARS;
        let keys_after: Vec<u32> = engine.cache.keys().copied().collect();
        assert_eq!(
            keys_before[..keys_after.len()],
            keys_after[..],
            "looping a played section drops at most the tail: the cache key set does not grow"
        );

        for b in &before {
            if b.start_bar + b.bars <= boundary {
                let again = engine.block_for_bars(b.start_bar, b.bars).expect("cached");
                assert!(Arc::ptr_eq(b, &again), "block {} was not recompiled", b.start_bar);
            }
        }

        for b in &before {
            if b.start_bar >= boundary && b.start_bar + b.bars <= looped.landing_bar {
                let again = engine.block_for_bars(b.start_bar, b.bars).expect("recompiled");
                assert_eq!(
                    format!("{b:?}"),
                    format!("{again:?}"),
                    "bars {} are untouched by the loop",
                    b.start_bar
                );
            }
        }

        assert_eq!(
            engine.current_session().sections[1].bars,
            dev_bars + looped.extra_bars,
            "the playing section extends"
        );
        let new_total = engine.current_session().total_bars() as u32;
        assert_eq!(new_total, total + looped.extra_bars);
        let mut keys: Vec<u32> = Vec::new();
        let mut bar = 0;
        while bar < new_total {
            let b = engine.block_for_bars(bar, 4).expect("block past the loop");
            bar += b.bars;
            keys.push(b.start_bar);
        }
        assert_eq!(
            engine.cache.keys().copied().collect::<Vec<_>>(),
            keys,
            "exactly one key per 4-bar block of the extended session, nothing extra"
        );
    }
}
