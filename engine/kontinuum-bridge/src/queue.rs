//! RT-side queue of *prepared* blocks: a compiled block plus its pre-merged,
//! frame-sorted event list (and the exact end frame from the session tempo
//! lane). Merging happens at publish time on the non-RT side, so the audio
//! thread's block activation is a pointer swap — zero allocation in `render`.
//!
//! Threading (issue #12 hard rule: no locks or allocation on the audio
//! thread): the queue splits into a producer (non-RT control thread, may
//! lock) and a consumer (audio render thread, lock-free). `epoch` lets the
//! control side invalidate stale future material after a diff: the RT side
//! drops queued blocks whose epoch does not match the current one.

use std::sync::Arc;

use kontinuum_core::graph::TrackSwap;
use kontinuum_schedule::{CompiledBlock, Event, TrackId};

/// One block ready for the audio thread. `events` is the merged, frame-sorted,
/// block-relative event list; `end_frame` is the absolute frame where the next
/// block takes over (resolved from the tempo lane at publish time).
#[derive(Clone, Debug)]
pub struct PreparedBlock {
    pub block: Arc<CompiledBlock>,
    pub events: Arc<[(u32, TrackId, Event)]>,
    pub end_frame: u64,
    pub epoch: u64,
}

/// Same fill level invariant on both rings: publish/pop are lockstep.
struct PreparedEvents {
    events: Arc<[(u32, TrackId, Event)]>,
    end_frame: u64,
    epoch: u64,
}

/// Control-side half. `publish` is lock-free (rtrb SPSC) but the struct is
/// only reached behind the engine's `Mutex`, so the RT consumer never waits
/// on anything.
pub struct PreparedProducer {
    capacity: usize,
    block_prod: rtrb::Producer<Arc<CompiledBlock>>,
    event_prod: rtrb::Producer<PreparedEvents>,
}

impl PreparedProducer {
    /// Non-RT: publish a prepared block. Never blocks; `false` on overflow.
    pub fn publish(&mut self, prepared: PreparedBlock) -> bool {
        if self.block_prod.is_full() || self.event_prod.is_full() {
            return false;
        }
        let block = self.block_prod.push(prepared.block).is_ok();
        let events = self
            .event_prod
            .push(PreparedEvents {
                events: prepared.events,
                end_frame: prepared.end_frame,
                epoch: prepared.epoch,
            })
            .is_ok();
        debug_assert_eq!(block, events, "rings desynchronized despite is_full check");
        block && events
    }

    pub fn len(&self) -> usize {
        self.capacity
            .saturating_sub(self.block_prod.slots())
            .min(self.capacity.saturating_sub(self.event_prod.slots()))
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// RT-side half: owned by the render path, never locked.
pub struct PreparedConsumer {
    block_cons: rtrb::Consumer<Arc<CompiledBlock>>,
    event_cons: rtrb::Consumer<PreparedEvents>,
}

impl PreparedConsumer {
    /// RT: take the next prepared block, if any.
    pub fn pop(&mut self) -> Option<PreparedBlock> {
        let block = self.block_cons.pop().ok()?;
        let prepared = self.event_cons.pop().ok()?;
        Some(PreparedBlock {
            block,
            events: prepared.events,
            end_frame: prepared.end_frame,
            epoch: prepared.epoch,
        })
    }

    pub fn len(&self) -> usize {
        self.block_cons.slots().min(self.event_cons.slots())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Build the SPSC pair.
pub fn prepared_queue(capacity: usize) -> (PreparedProducer, PreparedConsumer) {
    let capacity = capacity.max(2);
    let (block_prod, block_cons) = rtrb::RingBuffer::new(capacity);
    let (event_prod, event_cons) = rtrb::RingBuffer::new(capacity);
    (
        PreparedProducer { capacity, block_prod, event_prod },
        PreparedConsumer { block_cons, event_cons },
    )
}

/// Control→RT command (#53 step 3b): rare, bounded mutations the audio
/// thread applies at block boundaries. Steady-state rendering stays
/// allocation-free — draining a command is an exceptional event (`attach`
/// builds a fresh voice pool), never part of the per-block path.
#[derive(Clone, Debug)]
pub enum Command {
    AttachSample { track: u8, data: Arc<[f32]>, sample_rate: u32 },
    /// Issue #37 `SwapInstrument`: re-attach a track's strip to a new sound
    /// source, crossfaded by the core graph (`AudioGraph::swap_track`).
    SwapTrack { track: u8, swap: TrackSwap },
}

/// Control-side half: `send` is lock-free, never blocks; `false` on overflow.
pub struct CommandProducer {
    prod: rtrb::Producer<Command>,
}

impl CommandProducer {
    pub fn send(&mut self, command: Command) -> bool {
        self.prod.push(command).is_ok()
    }
}

/// RT-side half: owned by the render path, never locked.
pub struct CommandConsumer {
    cons: rtrb::Consumer<Command>,
}

impl CommandConsumer {
    /// RT: take the next command, if any.
    pub fn pop(&mut self) -> Option<Command> {
        self.cons.pop().ok()
    }
}

/// Build the SPSC pair (capacity 8 suffices: commands are rare).
pub fn command_queue(capacity: usize) -> (CommandProducer, CommandConsumer) {
    let (prod, cons) = rtrb::RingBuffer::new(capacity.max(2));
    (CommandProducer { prod }, CommandConsumer { cons })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kontinuum_schedule::{CompiledBlock, TrackEvents};

    fn block(start_bar: u32) -> Arc<CompiledBlock> {
        Arc::new(CompiledBlock {
            start_bar,
            bars: 4,
            start_frame: u64::from(start_bar) * 100,
            tracks: vec![TrackEvents { track: 0, events: vec![(0, Event::NoteOff { voice: 0 })] }],
        })
    }

    fn prepared(start_bar: u32, epoch: u64) -> PreparedBlock {
        let block = block(start_bar);
        PreparedBlock { events: block.merged_events().into(), block, end_frame: 123, epoch }
    }

    #[test]
    fn lockstep_publish_pop_preserves_block_and_events() {
        let (mut prod, mut cons) = prepared_queue(4);
        assert!(prod.publish(prepared(0, 0)));
        assert!(prod.publish(prepared(4, 0)));
        assert_eq!(prod.len(), 2);
        let a = cons.pop().expect("first");
        assert_eq!(a.block.start_bar, 0);
        assert_eq!(a.end_frame, 123);
        assert_eq!(a.epoch, 0);
        assert_eq!(a.events.len(), 1);
        let b = cons.pop().expect("second");
        assert_eq!(b.block.start_bar, 4);
        assert!(cons.pop().is_none());
        assert_eq!(prod.len(), 0);
    }

    #[test]
    fn overflow_rejects_never_blocks() {
        let (mut prod, mut cons) = prepared_queue(2);
        assert!(prod.publish(prepared(0, 0)));
        assert!(prod.publish(prepared(4, 0)));
        assert!(!prod.publish(prepared(8, 0)), "overflow must reject, not block");
        assert_eq!(cons.pop().unwrap().block.start_bar, 0);
        assert!(prod.publish(prepared(8, 0)));
    }

    #[test]
    fn commands_roundtrip_fifo_and_overflow_rejects() {
        let (mut prod, mut cons) = command_queue(2);
        let a = Command::AttachSample { track: 1, data: Arc::from([0.1f32; 8].as_slice()), sample_rate: 48_000 };
        let b = Command::AttachSample { track: 2, data: Arc::from([0.2f32; 8].as_slice()), sample_rate: 48_000 };
        let c = |track: u8| Command::AttachSample {
            track,
            data: Arc::from([0.3f32; 8].as_slice()),
            sample_rate: 48_000,
        };
        assert!(prod.send(a));
        assert!(prod.send(b));
        assert!(!prod.send(c(3)), "overflow must reject, not block");
        match cons.pop() {
            Some(Command::AttachSample { track, data, sample_rate }) => {
                assert_eq!((track, sample_rate), (1, 48_000));
                assert_eq!(&data[..], &[0.1f32; 8]);
            }
            other => panic!("expected attach command, got {other:?}"),
        }
        assert!(prod.send(c(3)));
        assert!(matches!(cons.pop(), Some(Command::AttachSample { track: 2, .. })));
        assert!(matches!(cons.pop(), Some(Command::AttachSample { track: 3, .. })));
        assert!(cons.pop().is_none());
    }
}
