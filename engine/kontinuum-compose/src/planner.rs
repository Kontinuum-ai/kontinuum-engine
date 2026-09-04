//! Rolling lookahead integration (issue #13): a thin wrapper over the
//! schedule crate's `LookaheadPlanner` plus a stateless prime pass for
//! callers that own the source and queue handles separately.

use kontinuum_schedule::{BlockQueue, BlockSource, LookaheadPlanner};

/// The compose crate's rolling lookahead planner: the schedule crate's
/// `LookaheadPlanner`, re-exported so engine wiring has a single import
/// site.
pub type Planner<S> = LookaheadPlanner<S>;

/// Convenience constructor for [`Planner`].
pub fn new_planner<S: BlockSource>(
    source: S,
    queue: BlockQueue,
    bars_per_block: u32,
    lookahead_bars: u32,
) -> Planner<S> {
    LookaheadPlanner::new(source, queue, bars_per_block, lookahead_bars)
}

/// Stateless prime pass: fills `queue` from the playhead's block grid up to
/// `current_bar + lookahead_bars`, stopping early when the queue is full
/// (reject-publish: never blocks) or the source refuses the bar range.
///
/// Protocol: the queue is filled in order and drained in order, so the
/// frontier is `grid(current_bar) + queue.len() · bars_per_block` (blocks in
/// flight are contiguous from the grid point at/after the playhead).
/// Callers that want the frontier tracked across ticks should hold a
/// [`Planner`] and use `tick` instead.
pub fn prime<S: BlockSource>(
    source: &mut S,
    queue: &mut BlockQueue,
    current_bar: u32,
    bars_per_block: u32,
    lookahead_bars: u32,
) -> usize {
    if bars_per_block == 0 {
        return 0;
    }
    let grid = (current_bar / bars_per_block) * bars_per_block;
    let mut next = grid + (queue.len() as u32) * bars_per_block;
    let horizon = current_bar.saturating_add(lookahead_bars);
    let mut published = 0;
    while next < horizon {
        if queue.is_full() {
            break;
        }
        match source.block_for_bars(next, bars_per_block) {
            Some(block) => {
                if !queue.publish(block) {
                    break;
                }
                published += 1;
                next = match next.checked_add(bars_per_block) {
                    Some(n) => n,
                    None => break,
                };
            }
            None => break,
        }
    }
    published
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{generate_session, ArrangementEngine, GenParams};

    fn engine(seed: u64) -> ArrangementEngine {
        let params = GenParams { seed, ..GenParams::default() };
        ArrangementEngine::new(generate_session(&params), 48_000)
    }

    #[test]
    fn prime_feeds_queue_as_playhead_advances() {
        let mut source = engine(3);
        let mut queue = BlockQueue::new(64);
        assert_eq!(prime(&mut source, &mut queue, 0, 4, 16), 4, "fresh queue primes the horizon");

        for current_bar in [4u32, 8, 12, 16, 20] {
            let popped = queue.pop().expect("a ready block per cycle");
            assert!(popped.start_bar < current_bar + 4, "service stays near the playhead");
            let published = prime(&mut source, &mut queue, current_bar, 4, 16);
            assert!(published >= 1, "horizon must stay fed at bar {current_bar}");

            let mut starts = vec![popped.start_bar];
            while let Some(b) = queue.pop() {
                starts.push(b.start_bar);
            }
            let mut sorted = starts.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(starts, sorted, "in-order service, no duplicates");

            prime(&mut source, &mut queue, current_bar, 4, 16);
        }
    }

    #[test]
    fn prime_respects_capacity_without_blocking() {
        let mut source = engine(4);
        let mut queue = BlockQueue::new(2);
        assert_eq!(prime(&mut source, &mut queue, 0, 4, 16), 2, "stops at capacity");
        assert!(queue.is_full());
        queue.pop();
        queue.pop();
        assert_eq!(prime(&mut source, &mut queue, 8, 4, 16), 2, "recovers after drain");
    }

    #[test]
    fn prime_stops_at_session_end() {
        let params = GenParams { seed: 5, target_bars: 32, ..GenParams::default() };
        let session = generate_session(&params);
        let total_blocks = (session.total_bars() / 4) as usize;
        let mut source = ArrangementEngine::new(session, 48_000);
        let mut queue = BlockQueue::new(64);
        assert_eq!(prime(&mut source, &mut queue, 0, 4, 64), total_blocks);
        assert_eq!(prime(&mut source, &mut queue, 28, 4, 64), 0, "nothing past the end");
    }
}
