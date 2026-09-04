//! Host-level dual-deck engine (issue #38 step 4): two full [`AudioGraph`]s
//! crossfaded with the equal-power pair from [`super::crossfade`], then one
//! shared [`MasterChain`].
//!
//! # Voice budgets are per-deck
//!
//! Each deck owns its own graph, so every track's [`VoicePool`] and
//! [`AutoMixer`] exist once **per deck** — deck B's pre-roll note-ons consume
//! deck B's pool slots only and can never steal from deck A's budget in the
//! render path (CI-pinned: both decks saturate their own pool capacity
//! simultaneously). The cost of pre-rolling is a second full voice load; the
//! CPU validator gate before arming deck B stays host-side (issue #38's
//! "validator gate before arming").
//!
//! # Master chain
//!
//! Both decks share one master stage: the per-deck chains stay at their
//! default unity gain (pure safety limiters) and the shared chain owned by
//! [`DeckMixer`] is the actual master (gain + final limiting). The equal-
//! power mix therefore happens in core's mix path, before the shared master.
//!
//! Routing decks through the bridge/host scheduler is a follow-up (see the
//! issue's step-4 note: kill-switch metrics stay live on both decks).

use kontinuum_schedule::{Event, TrackId};

use super::crossfade::Crossfade;
use super::kill::KillTelemetry;
use crate::graph::AudioGraph;
use crate::master::MasterChain;
use crate::BLOCK_FRAMES;

/// Which deck a control targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Deck {
    A,
    B,
}

impl Deck {
    fn index(self) -> usize {
        match self {
            Deck::A => 0,
            Deck::B => 1,
        }
    }
}

/// Two graphs + equal-power crossfade + shared master. All render-path
/// scratch lives in fixed stack arrays: allocation-free, deterministic.
pub struct DeckMixer {
    decks: [AudioGraph; 2],
    fade: Crossfade,
    master: MasterChain,
}

impl DeckMixer {
    pub fn new(sample_rate: u32) -> Self {
        DeckMixer {
            decks: [AudioGraph::new(sample_rate), AudioGraph::new(sample_rate)],
            fade: Crossfade::new(),
            master: MasterChain::new(sample_rate),
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.decks[0].sample_rate()
    }

    /// Direct deck access for setup (attach tracks, inserts, sends, events).
    pub fn deck(&mut self, deck: Deck) -> &mut AudioGraph {
        &mut self.decks[deck.index()]
    }

    /// Arm an A→B crossfade over `frames` output samples (see
    /// [`super::crossfade_frames`] for the beat-aligned frame count).
    pub fn begin_crossfade(&mut self, frames: u32) {
        self.fade.begin(frames);
    }

    /// Park the crossfade at `pos` (0 = deck A full, 1 = deck B full).
    pub fn park_crossfade(&mut self, pos: f32) {
        self.fade.park(pos);
    }

    pub fn crossfade_position(&self) -> f32 {
        self.fade.position()
    }

    pub fn set_master_gain(&mut self, value: f32) {
        self.master.set_gain_target(value);
    }

    pub fn master_gain_value(&self) -> f32 {
        self.master.gain_value()
    }

    /// Per-track mute on one deck (see [`AudioGraph::set_track_mute`]).
    pub fn set_track_mute(&mut self, deck: Deck, track: u8, muted: bool) {
        self.decks[deck.index()].set_track_mute(track, muted);
    }

    /// Per-track solo on one deck (see [`AudioGraph::set_track_solo`]).
    pub fn set_track_solo(&mut self, deck: Deck, track: u8, solo: bool) {
        self.decks[deck.index()].set_track_solo(track, solo);
    }

    /// Master panic on one deck.
    pub fn panic(&mut self, deck: Deck) {
        self.decks[deck.index()].panic();
    }

    /// Master panic on both decks.
    pub fn panic_all(&mut self) {
        self.decks.iter_mut().for_each(AudioGraph::panic);
    }

    /// Re-arm one deck after a panic (fade back to unity).
    pub fn rearm(&mut self, deck: Deck) {
        self.decks[deck.index()].rearm();
    }

    /// Re-arm both decks.
    pub fn rearm_all(&mut self) {
        self.decks.iter_mut().for_each(AudioGraph::rearm);
    }

    /// Combined kill-switch counters (both decks, saturating).
    pub fn kill_telemetry(&self) -> KillTelemetry {
        let a = self.decks[0].kill_telemetry();
        let b = self.decks[1].kill_telemetry();
        KillTelemetry {
            mute_events: a.mute_events.saturating_add(b.mute_events),
            panic_events: a.panic_events.saturating_add(b.panic_events),
        }
    }

    /// Render one callback buffer. `events_a` / `events_b` are the decks'
    /// block-relative, frame-sorted event lists (as produced by
    /// [`AudioGraph::prepare_block`]); both dispatch sample-accurately.
    pub fn render_block(
        &mut self,
        out_l: &mut [f32],
        out_r: &mut [f32],
        events_a: &[(u32, TrackId, Event)],
        events_b: &[(u32, TrackId, Event)],
        buf_start: u64,
    ) {
        let len = out_l.len().min(out_r.len());
        let mut off = 0;
        while off < len {
            let n = (len - off).min(BLOCK_FRAMES);
            self.render_window(
                &mut out_l[off..off + n],
                &mut out_r[off..off + n],
                events_a,
                events_b,
                buf_start + off as u64,
            );
            off += n;
        }
    }

    /// One ≤ [`BLOCK_FRAMES`] window: render each deck into stack scratch,
    /// equal-power mix per sample, shared master stage.
    fn render_window(
        &mut self,
        out_l: &mut [f32],
        out_r: &mut [f32],
        events_a: &[(u32, TrackId, Event)],
        events_b: &[(u32, TrackId, Event)],
        buf_start: u64,
    ) {
        let n = out_l.len();
        let mut a_l = [0.0f32; BLOCK_FRAMES];
        let mut a_r = [0.0f32; BLOCK_FRAMES];
        let mut b_l = [0.0f32; BLOCK_FRAMES];
        let mut b_r = [0.0f32; BLOCK_FRAMES];
        self.decks[0].render_block(&mut a_l[..n], &mut a_r[..n], events_a, buf_start);
        self.decks[1].render_block(&mut b_l[..n], &mut b_r[..n], events_b, buf_start);
        for i in 0..n {
            let (ga, gb) = self.fade.tick_gains();
            out_l[i] = a_l[i] * ga + b_l[i] * gb;
            out_r[i] = a_r[i] * ga + b_r[i] * gb;
        }
        self.master.render(&mut out_l[..n], &mut out_r[..n]);
    }

    pub fn reset(&mut self) {
        for deck in self.decks.iter_mut() {
            deck.reset();
        }
        self.fade = Crossfade::new();
        self.master.reset();
    }
}
