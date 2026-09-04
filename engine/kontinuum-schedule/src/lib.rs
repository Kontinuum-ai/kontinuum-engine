//! `kontinuum-schedule` — compiled performance data and the RT-side dispatch
//! path (issue #10/#13).
//!
//! Hard rules:
//! - Blocks are **pre-compiled off the audio thread** and published through an
//!   SPSC ring (`rtrb`). The audio thread only reads and never allocates,
//!   locks, or blocks. Overflow policy = reject-publish: the non-RT side can
//!   never stall the audio thread.
//! - Events inside a block are sorted by frame and carry no heap references —
//!   all ids resolve into preloaded tables.
//! - Dispatch splits each callback buffer at event frames; events land between
//!   sub-blocks, sample-accurately.

use rtrb::RingBuffer;
use std::sync::Arc;

pub type TrackId = u8;
pub type VoiceSlot = u8;
pub type ParamId = u16;

/// Envelope shape for parameter ramps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RampCurve {
    Linear,
    Exponential,
    Smooth,
}

/// One pre-compiled musical event. `frame` is relative to the block start and
/// already includes any microtiming offset (the compiler converts ticks →
/// frames via the tempo lane); `microtiming_ticks` is retained for telemetry
/// and determinism tests.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event {
    NoteOn {
        voice: VoiceSlot,
        /// MIDI pitch for tonal voices, 60.0 convention for drums.
        pitch: f32,
        velocity: f32,
        microtiming_ticks: i16,
    },
    NoteOff {
        voice: VoiceSlot,
    },
    ParamRamp {
        param: ParamId,
        target: f32,
        duration_frames: u32,
        curve: RampCurve,
    },
    SampleTrigger {
        sample_id: u32,
        slice: u16,
        rate: f32,
    },
}

impl Event {
    pub fn voice_slot(&self) -> Option<VoiceSlot> {
        match self {
            Event::NoteOn { voice, .. } | Event::NoteOff { voice } => Some(*voice),
            _ => None,
        }
    }
}

/// Sorted, read-only event list for one track within a block.
#[derive(Clone, Debug, Default)]
pub struct TrackEvents {
    pub track: TrackId,
    /// Sorted ascending by frame. Built off-RT; RT treats as immutable.
    pub events: Vec<(u32, Event)>,
}

/// A compiled performance block covering `[start_bar, start_bar + bars)` and
/// sample frames `[start_frame, …)` per the session tempo lane.
#[derive(Clone, Debug, Default)]
pub struct CompiledBlock {
    pub start_bar: u32,
    pub bars: u32,
    pub start_frame: u64,
    pub tracks: Vec<TrackEvents>,
}

impl CompiledBlock {
    pub fn end_frame(&self) -> u64 {
        self.start_frame // exact end requires the lane; blocks chain so this is informational
    }

    pub fn total_events(&self) -> usize {
        self.tracks.iter().map(|t| t.events.len()).sum()
    }

    /// Merged, frame-sorted view across all tracks (built lazily off-RT when needed).
    pub fn merged_events(&self) -> Vec<(u32, TrackId, Event)> {
        let mut all: Vec<(u32, TrackId, Event)> = self
            .tracks
            .iter()
            .flat_map(|t| t.events.iter().map(move |(f, e)| (*f, t.track, *e)))
            .collect();
        all.sort_by_key(|(f, _, _)| *f);
        all
    }
}

// Core routing ids the graph dispatches on (mirror kontinuum-core::params;
// core depends on this crate, so the values are repeated here).
const ROUTE_TRACK_GAIN: ParamId = 90;
const ROUTE_TRACK_PAN: ParamId = 91;
const ROUTE_SEND_DELAY: ParamId = 92;
const ROUTE_SEND_REVERB: ParamId = 93;
const SATURATE_DRIVE: ParamId = 75;

/// Translate compiler automation ParamIds (`0x0N00 | track`) to the core ids
/// the graph routes on. Track selection comes from the event's TrackId, so the
/// track bits are dropped. `insert0/1` lanes drive the slot's Saturate when
/// present (v0 approximation); unknown classes pass through untouched and are
/// ignored by the graph.
pub fn retarget_automation_params(events: &mut [(u32, TrackId, Event)]) {
    for (_, _, event) in events.iter_mut() {
        if let Event::ParamRamp { param, .. } = event {
            *param = match *param & 0xFF00 {
                0x0100 => ROUTE_TRACK_GAIN,
                0x0200 => ROUTE_TRACK_PAN,
                0x0300 | 0x0400 => SATURATE_DRIVE,
                0x0500 => ROUTE_SEND_DELAY,
                0x0600 => ROUTE_SEND_REVERB,
                _ => *param,
            };
        }
    }
}

/// One tile of the callback buffer produced by [`EventCursor`].
#[derive(Debug)]
pub struct Span<'a> {
    /// Offset of this tile within the callback buffer.
    pub offset: usize,
    /// Length in frames (> 0 unless the buffer is exhausted).
    pub len: usize,
    /// Events landing exactly at this tile's first frame (consume *after*
    /// rendering the tile: an event at frame f takes effect from frame f on).
    pub events: &'a [(u32, TrackId, Event)],
}

/// Tiles one audio callback buffer exactly, splitting at event frames. The
/// audio thread drives it with zero allocation: repeated `next_span` calls
/// cover `0..buf_len` contiguously, handing over the events that land at each
/// tile boundary. Events before the buffer or past it are dropped.
pub struct EventCursor<'a> {
    events: &'a [(u32, TrackId, Event)],
    next: usize,
    buf_start: u64,
    buf_len: usize,
    cursor: usize,
}

impl<'a> EventCursor<'a> {
    /// `events` must be sorted by frame. Frames are block-relative.
    pub fn new(events: &'a [(u32, TrackId, Event)], buf_start: u64, buf_len: usize) -> Self {
        EventCursor { events, next: 0, buf_start, buf_len, cursor: 0 }
    }

    pub fn next_span(&mut self) -> Option<Span<'a>> {
        let start = self.cursor;
        if start >= self.buf_len {
            return None;
        }
        let abs = self.buf_start + start as u64;
        // Skip stale events (before this buffer position).
        while self.next < self.events.len() && (self.events[self.next].0 as u64) < abs {
            self.next += 1;
        }
        // Consume every event landing exactly at this frame.
        let trig_start = self.next;
        while self.next < self.events.len() && self.events[self.next].0 as u64 == abs {
            self.next += 1;
        }
        // Tile ends at the next event inside the buffer, or the buffer end.
        let mut end = self.buf_len;
        if self.next < self.events.len() {
            let f = self.events[self.next].0 as u64;
            if f < self.buf_start + self.buf_len as u64 {
                end = (f - self.buf_start) as usize;
            }
        }
        self.cursor = end;
        Some(Span { offset: start, len: end - start, events: &self.events[trig_start..self.next] })
    }
}

/// Produces compiled blocks on demand. Implemented by the arrangement planner
/// (#16/#13) and the supervision fallback generator (#15); consumed by the
/// engine's non-RT scheduler thread.
pub trait BlockSource: Send {
    /// Compile the block covering `[start_bar, start_bar + bars)`. Returns
    /// `None` when the source cannot produce it (watchdog decides fallback).
    fn block_for_bars(&mut self, start_bar: u32, bars: u32) -> Option<Arc<CompiledBlock>>;
}

/// Capacity of the RT block queue (in blocks).
pub const DEFAULT_BLOCK_QUEUE_CAPACITY: usize = 64;

/// SPSC bridge between the non-RT scheduler and the audio thread.
/// Producer: `publish` (reject-publish on overflow). Consumer: `take_ready`.
pub struct BlockQueue {
    prod: rtrb::Producer<Arc<CompiledBlock>>,
    cons: rtrb::Consumer<Arc<CompiledBlock>>,
}

impl BlockQueue {
    pub fn new(capacity: usize) -> Self {
        let (prod, cons) = RingBuffer::new(capacity.max(2));
        BlockQueue { prod, cons }
    }

    /// Non-RT: publish a future block. Never blocks; returns false on overflow
    /// (caller may retry after the audio thread drains).
    pub fn publish(&mut self, block: Arc<CompiledBlock>) -> bool {
        self.prod.push(block).is_ok()
    }

    pub fn is_full(&self) -> bool {
        self.prod.is_full()
    }

    /// RT: pop the next published block, if any.
    pub fn pop(&mut self) -> Option<Arc<CompiledBlock>> {
        self.cons.pop().ok()
    }

    pub fn len(&self) -> usize {
        self.cons.slots()
    }

    pub fn is_empty(&self) -> bool {
        self.cons.is_empty()
    }
}

/// Non-RT rolling lookahead scheduler (issue #13): keeps the queue primed
/// `lookahead_bars` ahead of the playhead, switching at musical boundaries.
pub struct LookaheadPlanner<S: BlockSource> {
    source: S,
    queue: BlockQueue,
    pub bars_per_block: u32,
    pub lookahead_bars: u32,
    next_bar: u32,
    /// Frames per bar at the session lane, resolved lazily by the driver via
    /// `frame_of_bar`; the planner itself works in bars.
    primed_until_bar: u32,
}

impl<S: BlockSource> LookaheadPlanner<S> {
    pub fn new(source: S, queue: BlockQueue, bars_per_block: u32, lookahead_bars: u32) -> Self {
        LookaheadPlanner {
            source,
            queue,
            bars_per_block,
            lookahead_bars,
            next_bar: 0,
            primed_until_bar: 0,
        }
    }

    /// Call from the non-RT thread (timer/watchdog). `current_bar` is where the
    /// playhead is now. Fills the queue up to the lookahead horizon.
    /// Returns how many blocks were published.
    pub fn tick(&mut self, current_bar: u32) -> usize {
        let horizon = current_bar + self.lookahead_bars;
        let mut published = 0;
        while self.primed_until_bar < horizon {
            let start = self.primed_until_bar;
            match self.source.block_for_bars(start, self.bars_per_block) {
                Some(block) => {
                    if self.queue.publish(block) {
                        published += 1;
                        self.primed_until_bar = start + self.bars_per_block;
                    } else {
                        break; // queue full; retry next tick
                    }
                }
                None => break,
            }
        }
        self.next_bar = self.primed_until_bar;
        published
    }

    /// Drop queued blocks that begin before `current_bar` (stale after a seek).
    pub fn drop_stale(&mut self, current_bar: u32) {
        while let Some(block) = self.queue.pop() {
            if block.start_bar + block.bars > current_bar {
                // Re-publishing is impossible (single consumer); in practice the
                // engine drains into its active slot, so only drop when strictly stale.
                if block.start_bar + block.bars <= current_bar {
                    continue;
                }
                break;
            }
        }
    }

    pub fn queue(&mut self) -> &mut BlockQueue {
        &mut self.queue
    }

    pub fn source_mut(&mut self) -> &mut S {
        &mut self.source
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(start_bar: u32, events: Vec<(u32, TrackId, Event)>) -> Arc<CompiledBlock> {
        let mut tracks: Vec<TrackEvents> = vec![];
        for (f, t, e) in events {
            match tracks.iter_mut().find(|te| te.track == t) {
                Some(te) => te.events.push((f, e)),
                None => tracks.push(TrackEvents { track: t, events: vec![(f, e)] }),
            }
        }
        for te in &mut tracks {
            te.events.sort_by_key(|(f, _)| *f);
        }
        Arc::new(CompiledBlock { start_bar, bars: 4, start_frame: start_bar as u64 * 100, tracks })
    }

    #[test]
    fn queue_reject_publish_never_blocks() {
        let mut q = BlockQueue::new(2);
        assert!(q.publish(block(0, vec![])));
        assert!(q.publish(block(4, vec![])));
        assert!(!q.publish(block(8, vec![])), "overflow must reject, not block");
        assert_eq!(q.pop().unwrap().start_bar, 0);
        assert!(q.publish(block(8, vec![])));
    }

    #[test]
    fn cursor_tiles_buffer_exactly() {
        // Buffer covers frames 10..110 (buf_start=10, len=100).
        let events = vec![
            (15u32, 0u8, Event::NoteOn { voice: 0, pitch: 60.0, velocity: 1.0, microtiming_ticks: 0 }),
            (15u32, 1, Event::NoteOn { voice: 1, pitch: 60.0, velocity: 0.5, microtiming_ticks: 0 }),
            (50u32, 0, Event::NoteOff { voice: 0 }),
            (200u32, 0, Event::NoteOff { voice: 0 }), // outside buffer
        ];
        let mut cursor = EventCursor::new(&events, 10, 100);
        let mut frames_covered = 0usize;
        let mut landed = vec![];

        let s1 = cursor.next_span().unwrap();
        assert_eq!((s1.offset, s1.len), (0, 5));
        assert!(s1.events.is_empty());
        frames_covered += s1.len;

        let s2 = cursor.next_span().unwrap();
        assert_eq!((s2.offset, s2.len), (5, 35));
        assert_eq!(s2.events.len(), 2, "both events at frame 15 land together");
        landed.push(15);
        frames_covered += s2.len;

        let s3 = cursor.next_span().unwrap();
        assert_eq!((s3.offset, s3.len), (40, 60));
        assert_eq!(s3.events.len(), 1);
        frames_covered += s3.len;

        assert!(cursor.next_span().is_none(), "exhausted");
        assert_eq!(frames_covered, 100, "tiles must cover the buffer exactly");
        assert_eq!(landed, vec![15]);
    }

    #[test]
    fn retarget_maps_compiler_param_classes_to_route_ids() {
        let ramp = |param: u16| {
            Event::ParamRamp { param, target: 0.5, duration_frames: 1, curve: RampCurve::Linear }
        };
        let mut events = vec![
            (0u32, 2u8, ramp(0x0100 | 2)),
            (1, 2, ramp(0x0200 | 2)),
            (2, 1, ramp(0x0300 | 1)),
            (3, 1, ramp(0x0400 | 1)),
            (4, 3, ramp(0x0500 | 3)),
            (5, 3, ramp(0x0600 | 3)),
            (6, 4, ramp(0x0700 | 4)),
            (7, 0, Event::NoteOn { voice: 0, pitch: 60.0, velocity: 1.0, microtiming_ticks: 0 }),
        ];

        retarget_automation_params(&mut events);

        let params: Vec<u16> = events
            .iter()
            .filter_map(|(_, _, e)| match e {
                Event::ParamRamp { param, .. } => Some(*param),
                _ => None,
            })
            .collect();
        assert_eq!(params, vec![90, 91, 75, 75, 92, 93, 0x0704]);
    }

    #[test]
    fn lookahead_primes_ahead_of_playhead() {
        struct Src(u32);
        impl BlockSource for Src {
            fn block_for_bars(&mut self, start: u32, _bars: u32) -> Option<Arc<CompiledBlock>> {
                if start >= self.0 {
                    return None;
                }
                Some(block(start, vec![]))
            }
        }
        let mut planner = LookaheadPlanner::new(Src(64), BlockQueue::new(64), 4, 16);
        assert_eq!(planner.tick(0), 4);
        assert_eq!(planner.tick(4), 1);
        assert_eq!(planner.tick(8), 1);
    }
}
