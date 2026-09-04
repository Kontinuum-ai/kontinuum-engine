//! [`KontinuumEngine`] — the whole live engine in one `Send` object
//! (issues #10/#12): validated session + tempo lane + [`AudioGraph`] +
//! the prepared-block SPSC queue.
//!
//! Threading contract:
//! - **RT (audio render thread)**: [`KontinuumEngine::render`] only. Owns the
//!   graph, the consumer half of the queue, and the pending/active slots.
//!   Allocation-free, lock-free, panic-free. Cross-thread scalars are atomics.
//! - **Control (main thread)**: `play`/`stop`/`telemetry`/`apply_diff_json`/
//!   [`KontinuumEngine::pump`]. Owns the producer half of the queue and the
//!   session. `pump` keeps the queue primed ahead of the playhead and loops
//!   the session so playback never runs dry.

use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use kontinuum_clock::TempoLane;
use kontinuum_core::AudioGraph;
use kontinuum_ir::schema::{InstrumentDef, Session};
use kontinuum_ir::{compile_session, validate_session, ApplyReport, IrDiff, TrackRole, ValidationError};
use kontinuum_mastering::OutputProfile;
use kontinuum_schedule::{retarget_automation_params, CompiledBlock, Event, TrackId};
use kontinuum_supervision::FallbackSource;
use kontinuum_core::MAX_TRACKS;
use serde::Serialize;

use crate::queue::{
    command_queue, prepared_queue, Command, CommandConsumer, CommandProducer, PreparedBlock,
    PreparedConsumer, PreparedProducer,
};
use crate::session_setup::{apply_session_to_graph, apply_track};
use kontinuum_plugin_api::Registry;

/// RT queue capacity in blocks (4-bar blocks; 64 = 256 bars of buffer).
const QUEUE_CAPACITY: usize = kontinuum_schedule::DEFAULT_BLOCK_QUEUE_CAPACITY;
/// Control→RT commands are rare (in-session kit loads): capacity 8 (#53 step 3b).
const COMMAND_QUEUE_CAPACITY: usize = 8;
/// Bars of audio kept queued ahead of the playhead by [`KontinuumEngine::pump`].
const PUMP_LOOKAHEAD_BARS: f64 = 32.0;
/// Waveform history length for the living UI.
const UI_HISTORY: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("session JSON parse failed: {0}")]
    SessionParse(String),
    #[error("session failed validation:\n{0}")]
    SessionInvalid(String),
    #[error("compile failed: {0}")]
    Compile(String),
    #[error("diff JSON parse failed: {0}")]
    DiffParse(String),
    #[error("diff rejected: {0}")]
    DiffRejected(String),
    #[error("instrument JSON parse failed: {0}")]
    InstrumentParse(String),
    #[error("unknown track `{0}`")]
    UnknownTrack(String),
}

impl EngineError {
    fn from_validation(errors: &[ValidationError]) -> Self {
        let listed = errors
            .iter()
            .map(|e| format!("  {} at {}: {} — fix: {}", e.code, e.path, e.message, e.suggested_fix))
            .collect::<Vec<_>>()
            .join("\n");
        EngineError::SessionInvalid(listed)
    }
}

/// Point-in-time safety counters (snapshot for UI/tests).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SafetyCounters {
    /// Callback buffers rendered as silence because no block covered the
    /// playhead (schedule underrun or end of session).
    pub render_gaps: u64,
    /// Diffs rejected by validation/apply semantics.
    pub invalid_diffs: u64,
    /// Publish attempts rejected because the RT queue was full.
    pub queue_overflows: u64,
    /// Per-lap regenerations (vary/validate/compile) that failed or
    /// panicked and were contained by supervision: the previous known-good
    /// lap kept playing (#81 wiring of the #15 watchdog contract).
    pub regeneration_failures: u64,
    /// Times the engine adopted the built-in fallback arrangement because
    /// no known-good compiled set existed (last resort; music never stops).
    pub watchdog_fallbacks: u64,
    /// Latched mastering limiter-alarm episodes (#15/#28 wiring): sustained
    /// over-limit reduction through the live chain, counted per episode.
    pub mastering_gr_alarms: u64,
    /// Latched mastering limiter alarm (#82 wiring of the #15 kill-switch
    /// feed): sustained over-limit reduction through the live chain.
    pub limiter_gr_alarm: bool,
}

/// Plain snapshot for the UI (serde + `Copy`).
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Telemetry {
    pub playhead_bar: f64,
    pub playing: bool,
    pub queue_len: usize,
    pub active_block_bar: Option<u32>,
    pub render_gaps: u64,
    pub invalid_diffs: u64,
    pub mastering: MasteringTelemetryLite,
}

/// Compact mastering working point (#82) for the UI/kill-switch feed;
/// mirrored 1:1 by `MasteringTelemetryFFI` in `ffi.rs` / `KontinuumBridge.h`.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct MasteringTelemetryLite {
    /// Applied tilt target (dB, positive = brighter).
    pub tilt_db: f32,
    /// Glue compressor mean reduction (dB).
    pub glue_gr_db: f32,
    /// Soft clipper mean reduction (dB).
    pub clipper_gr_db: f32,
    /// Limiter peak reduction during the last block (dB).
    pub limiter_gr_db: f32,
    /// Latched sustained-over-limit alarm (kill-switch feed, #15).
    pub limiter_gr_alarm: bool,
    /// Bit-exact passthrough while true.
    pub bypassed: bool,
}

impl From<kontinuum_mastering::MasteringTelemetry> for MasteringTelemetryLite {
    fn from(t: kontinuum_mastering::MasteringTelemetry) -> Self {
        MasteringTelemetryLite {
            tilt_db: t.tilt_db,
            glue_gr_db: t.glue_gr_db,
            clipper_gr_db: t.clipper_gr_db,
            limiter_gr_db: t.limiter_gr_db,
            limiter_gr_alarm: t.limiter_gr_alarm,
            bypassed: t.bypassed,
        }
    }
}

/// One track's live activity for the UI (drained since the last snapshot).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TrackUi {
    pub onsets: u32,
    pub velocity: f32,
    pub pitch: f32,
}

/// Voice kind of a session track (issue #89): one vocabulary with
/// [`TrackRole`], mirrored as a small integer across the FFI boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackVoice {
    Kick = 0,
    Bass = 1,
    Perc = 2,
    Pad = 3,
    Fx = 4,
}

impl From<TrackRole> for TrackVoice {
    fn from(role: TrackRole) -> Self {
        match role {
            TrackRole::Kick => TrackVoice::Kick,
            TrackRole::Bass => TrackVoice::Bass,
            TrackRole::Perc => TrackVoice::Perc,
            TrackRole::Pad => TrackVoice::Pad,
            TrackRole::Fx => TrackVoice::Fx,
        }
    }
}

/// Canonical identity of one loaded session track (issue #89): the UI derives
/// its lanes from these instead of hardcoding an index → name table. `id` is
/// the engine's canonical track id; `name` is a display label derived from the
/// instrument kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackDescriptor {
    pub index: usize,
    pub id: String,
    pub name: String,
    pub voice: TrackVoice,
}

/// Display label for a session track, derived from its instrument kind (the
/// IR's canonical vocabulary) — never hardcoded in the UI.
fn instrument_display_name(instrument: &InstrumentDef) -> &'static str {
    match instrument {
        InstrumentDef::Kick(_) => "Kick",
        InstrumentDef::Hat(_) => "Hi-hat",
        InstrumentDef::Clap(_) => "Clap",
        InstrumentDef::Snare(_) => "Snare",
        InstrumentDef::Shaker(_) => "Shaker",
        InstrumentDef::Bass(_) => "Bass",
        InstrumentDef::Acid(_) => "Acid",
        InstrumentDef::Pad(_) => "Pad",
        InstrumentDef::Ep(_) => "Electric piano",
        InstrumentDef::Pluck(_) => "Pluck",
        InstrumentDef::Stab(_) => "Stab",
        InstrumentDef::Wavetable(_) => "Wavetable",
        InstrumentDef::FmPerc(_) => "FM Perc",
        InstrumentDef::Texture(_) => "Texture",
        InstrumentDef::Sample(_) => "Sampler",
        InstrumentDef::Custom(_) => "Custom",
    }
}

/// One finalized bar of history for the living UI: energy, per-track onset
/// counts, 16-slot hit masks (ground truth from the RT path), last velocity
/// per track, and the bar's measured loudness (issue #90) — the waveform
/// column is drawn from what was actually heard, not from the section scalar.
#[derive(Clone, Copy, Debug, Default)]
pub struct BarFrame {
    pub energy: f32,
    pub onsets: [u32; MAX_TRACKS],
    pub masks: [u32; MAX_TRACKS],
    pub last_velocity: [f32; MAX_TRACKS],
    /// Mixed-output RMS of this bar's audio, metered on the RT path (0..1).
    pub rms: f32,
    /// Mixed-output peak of this bar's audio (0..1).
    pub peak: f32,
    /// Section the finalized bar belongs to; the UI draws a boundary tick
    /// wherever this flips between columns.
    pub section_index: u32,
}

/// Fixed-size RT→control handoff ring for per-bar loudness (issue #90).
/// Each slot packs one closed bar window (low 32 bits RMS, high 32 bits
/// peak). Capacity bounds the backlog if the control thread stalls; the
/// writer never blocks or allocates.
const METER_RING: usize = 128;

fn pack_meter(rms: f32, peak: f32) -> u64 {
    ((peak.to_bits() as u64) << 32) | rms.to_bits() as u64
}

fn unpack_meter(packed: u64) -> (f32, f32) {
    (f32::from_bits(packed as u32), f32::from_bits((packed >> 32) as u32))
}

/// Control-side snapshot for the living UI (issue #33): ground-truth bar
/// position, section energy, and per-track activity.
#[derive(Clone, Copy, Debug, Default)]
pub struct UiSnapshot {
    pub bar: f64,
    pub beat_phase: f64,
    pub energy: f32,
    pub section_index: usize,
    pub bar_in_section: u32,
    pub section_bars: u32,
    pub playing: bool,
    pub tracks: [TrackUi; MAX_TRACKS],
    /// In-progress 16-slot hit masks for the bar being played now.
    pub current_masks: [u32; MAX_TRACKS],
    /// Finalized per-bar history, oldest first (waveform columns).
    pub history_len: usize,
}

/// Result of a successful diff application (mirrors `ir::ApplyReport`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplyOutcome {
    pub applied: Vec<String>,
    pub superseded: Vec<String>,
}

impl From<ApplyReport> for ApplyOutcome {
    fn from(r: ApplyReport) -> Self {
        ApplyOutcome { applied: r.applied, superseded: r.superseded }
    }
}

enum NextBlock {
    /// A block covering `frame`; take it as active.
    Now(PreparedBlock),
    /// The next block starts in the future; silence until then.
    Future(u64),
    /// Nothing queued.
    None,
}

/// RT-written UI counters shared between the graph's event hook and the
/// control-side snapshot. One allocation at engine construction, never again.
struct EngineCounters {
    onsets: [AtomicU32; MAX_TRACKS],
    velocity: [AtomicU32; MAX_TRACKS],
    pitch: [AtomicU32; MAX_TRACKS],
    mask: [AtomicU32; MAX_TRACKS],
    playhead: AtomicU64,
    active_start_frame: AtomicU64,
    frames_per_bar: AtomicU64,
}

impl EngineCounters {
    fn new() -> Self {
        EngineCounters {
            onsets: Default::default(),
            velocity: Default::default(),
            pitch: Default::default(),
            mask: Default::default(),
            playhead: AtomicU64::new(0),
            active_start_frame: AtomicU64::new(0),
            frames_per_bar: AtomicU64::new(0),
        }
    }
}

/// Lap-0 block plus its cached merged events (computed once, reused across laps).
struct CompiledSource {
    block: Arc<CompiledBlock>,
    events: Arc<[(u32, TrackId, Event)]>,
}

/// Control side: merged, frame-sorted events for one block, with the
/// compiler's automation ParamIds retargeted to the routing ids the graph
/// dispatches on — the same mapping the offline renderer applies (#16).
fn compiled_source(block: Arc<CompiledBlock>) -> CompiledSource {
    let mut events = block.merged_events();
    retarget_automation_params(&mut events);
    CompiledSource { block, events: events.into() }
}

pub struct KontinuumEngine {
    sample_rate: u32,
    // -- control side (main thread) --
    session: Session,
    registry: Registry,
    lane: TempoLane,
    producer: Mutex<PreparedProducer>,
    command_producer: CommandProducer,
    compiled: Vec<CompiledSource>,
    publish_idx: usize,
    lap_offset: u64,
    lap: u32,
    published_until: u64,
    epoch: AtomicU64,
    invalid_diffs: AtomicU64,
    queue_overflows: AtomicU64,
    // -- RT side (audio thread) --
    graph: AudioGraph,
    consumer: PreparedConsumer,
    command_consumer: CommandConsumer,
    pending: Vec<PreparedBlock>,
    active: Option<PreparedBlock>,
    render_gaps: AtomicU64,
    // -- shared atomics --
    playhead_frame: Arc<AtomicU64>,
    active_start_frame: Arc<AtomicU64>,
    frames_per_bar: Arc<AtomicU64>,
    playing: AtomicBool,
    /// Start bar of the block currently sounding (`u32::MAX` = none); written
    /// by RT on activation, read by control (diff anchoring + telemetry).
    active_start_bar: AtomicU32,
    /// End bar of the block currently sounding; 0 = none. Diff recompiles
    /// start here.
    active_end_bar: AtomicU32,
    /// Blocks buffered on the RT side (pending list), for telemetry.
    rt_pending: AtomicU32,
    // -- RT-written UI counters (shared with the graph event hook) --
    counters: Arc<EngineCounters>,
    // -- RT per-bar loudness meter (issue #90): plain fields are RT-only,
    //    the ring + write index are the lock-free handoff to the control
    //    thread's bar finalization --
    /// Sum of squared mono samples folded into the open bar window.
    meter_sq: f64,
    /// Peak |sample| of the open bar window.
    meter_peak: f32,
    /// Frames folded into the open bar window.
    meter_frames: u64,
    /// Absolute frame where the open window closes and publishes.
    meter_boundary: u64,
    /// Frames per bar from the most recently activated block (0 = no bar
    /// reference yet, metering idle).
    meter_fpb: u64,
    /// Closed (rms, peak) bar windows, written by RT at `meter_write`.
    meter_ring: [AtomicU64; METER_RING],
    /// Total windows published by RT; slots are addressed `write % METER_RING`.
    meter_write: AtomicU64,
    // -- control-side UI bookkeeping --
    sections: Vec<SectionInfo>,
    /// Section index of the most recently published block — the mastering
    /// section-energy feed fires when a publish crosses into a new section.
    published_section: Option<usize>,
    ui_bar_accum: [u32; MAX_TRACKS],
    ui_mask_accum: [u32; MAX_TRACKS],
    ui_last_bar: u32,
    ui_history: VecDeque<BarFrame>,
    /// Control-only drain position into `meter_ring`.
    meter_read: u64,
    /// Last drained (rms, peak); stands in when a finalized bar has no fresh
    /// meter window (the RT boundary has not been crossed yet).
    last_meter: (f32, f32),
    // -- control-side supervision (#15 wiring, issue #81) --
    /// Safety counters accumulated on the control thread by the guarded
    /// regeneration path; exposed via [`KontinuumEngine::safety_snapshot`].
    safety: kontinuum_supervision::SafetyCounters,
    /// Rising-edge state for the mastering GR-alarm feed (#28): true while
    /// the chain's alarm is latched and already counted as an episode.
    mastering_alarm_counted: bool,
    /// Test seam: force every per-lap regeneration from this lap onward to
    /// fail (`false`) or panic (`true`). Never set outside `cfg(test)`.
    #[cfg(test)]
    regen_fail_from: Option<u32>,
    #[cfg(test)]
    regen_fail_panic: bool,
}

struct SectionInfo {
    start_bar: u32,
    bars: u32,
    energy_curve: Vec<f32>,
}

impl SectionInfo {
    fn energy_at(&self, bar_in_section: u32) -> f32 {
        if self.energy_curve.is_empty() || self.bars == 0 {
            return 0.5;
        }
        let frac = (f64::from(bar_in_section) / f64::from(self.bars.max(1) - 1)).clamp(0.0, 1.0);
        let idx = frac * (self.energy_curve.len() - 1) as f64;
        let lo = idx.floor() as usize;
        let hi = (lo + 1).min(self.energy_curve.len() - 1);
        let t = (idx - lo as f64) as f32;
        self.energy_curve[lo] * (1.0 - t) + self.energy_curve[hi] * t
    }
}

impl KontinuumEngine {
    /// Parses + validates the session JSON, compiles the block list, wires the
    /// mixer graph (roles, gains, pans, sends, inserts, instrument params) and
    /// primes the RT queue with the first lookahead.
    pub fn new(sample_rate: u32, session_json: &str) -> Result<Self, EngineError> {
        let session: Session = serde_json::from_str(session_json)
            .map_err(|e| EngineError::SessionParse(e.to_string()))?;
        if let Err(errors) = validate_session(&session) {
            return Err(EngineError::from_validation(&errors));
        }
        let blocks = compile_session(&session, sample_rate)
            .map_err(|e| EngineError::Compile(e.to_string()))?;
        let lane = TempoLane::new(sample_rate, &session.tempo_lane)
            .map_err(|e| EngineError::Compile(format!("tempo lane: {}", e.reason)))?;

        let compiled: Vec<CompiledSource> =
            blocks.into_iter().map(compiled_source).collect();

        let (producer, consumer) = prepared_queue(QUEUE_CAPACITY);
        let (command_producer, command_consumer) = command_queue(COMMAND_QUEUE_CAPACITY);
        let mut engine = KontinuumEngine {
            sample_rate,
            session,
            registry: kontinuum_instruments_core::registry(),
            lane,
            producer: Mutex::new(producer),
            command_producer,
            compiled,
            publish_idx: 0,
            lap_offset: 0,
            lap: 0,
            published_until: 0,
            epoch: AtomicU64::new(0),
            invalid_diffs: AtomicU64::new(0),
            queue_overflows: AtomicU64::new(0),
            graph: AudioGraph::new(sample_rate),
            consumer,
            command_consumer,
            pending: Vec::with_capacity(QUEUE_CAPACITY * 2),
            active: None,
            render_gaps: AtomicU64::new(0),
            playhead_frame: Arc::new(AtomicU64::new(0)),
            active_start_frame: Arc::new(AtomicU64::new(0)),
            frames_per_bar: Arc::new(AtomicU64::new(0)),
            playing: AtomicBool::new(false),
            active_start_bar: AtomicU32::new(u32::MAX),
            active_end_bar: AtomicU32::new(0),
            rt_pending: AtomicU32::new(0),
            counters: Arc::new(EngineCounters::new()),
            meter_sq: 0.0,
            meter_peak: 0.0,
            meter_frames: 0,
            meter_boundary: 0,
            meter_fpb: 0,
            meter_ring: std::array::from_fn(|_| AtomicU64::new(0)),
            meter_write: AtomicU64::new(0),
            meter_read: 0,
            last_meter: (0.0, 0.0),
            sections: Vec::new(),
            ui_bar_accum: [0; MAX_TRACKS],
            ui_mask_accum: [0; MAX_TRACKS],
            ui_last_bar: 0,
            ui_history: VecDeque::with_capacity(UI_HISTORY),
            published_section: None,
            safety: kontinuum_supervision::SafetyCounters::default(),
            mastering_alarm_counted: false,
            #[cfg(test)]
            regen_fail_from: None,
            #[cfg(test)]
            regen_fail_panic: false,
        };
        engine.rebuild_sections();
        engine.apply_session_params();
        engine.install_event_hook();
        engine.pump();
        Ok(engine)
    }

    /// RT hook: counts onsets, last velocity/pitch, and ORs the 16-slot bar
    /// mask using the shared playhead/active-frame atomics. Atomic stores only.
    fn install_event_hook(&mut self) {
        let counters = Arc::clone(&self.counters);
        let playhead = Arc::clone(&self.playhead_frame);
        let active_start_frame = Arc::clone(&self.active_start_frame);
        let frames_per_bar = Arc::clone(&self.frames_per_bar);
        self.graph.set_event_hook(Box::new(move |track, event| {
            let t = track as usize;
            if t >= MAX_TRACKS {
                return;
            }
            match event {
                Event::NoteOn { pitch, velocity, .. } => {
                    counters.onsets[t].fetch_add(1, Ordering::Relaxed);
                    counters.velocity[t].store(velocity.to_bits(), Ordering::Relaxed);
                    counters.pitch[t].store(pitch.to_bits(), Ordering::Relaxed);
                    let ps = playhead.load(Ordering::Relaxed);
                    let sf = active_start_frame.load(Ordering::Relaxed);
                    let fpb = frames_per_bar.load(Ordering::Relaxed).max(1);
                    let frac = (ps.saturating_sub(sf) % fpb) as f32 / fpb as f32;
                    let bit = 1u32 << ((frac * 16.0) as u32).min(15);
                    counters.mask[t].fetch_or(bit, Ordering::Relaxed);
                }
                _ => {}
            }
        }));
    }

    fn rebuild_sections(&mut self) {
        let mut sections = Vec::new();
        let mut start = 0u32;
        for sec in &self.session.sections {
            sections.push(SectionInfo {
                start_bar: start,
                bars: sec.bars,
                energy_curve: sec.energy_curve.clone(),
            });
            start += sec.bars;
        }
        self.sections = sections;
        self.published_section = None;
    }

    /// Control thread (UI timer, ~30 Hz): ground-truth position, section
    /// energy, per-track activity, and per-bar history for the waveform.
    /// Drains the RT onset counters.
    pub fn ui_snapshot(&mut self) -> UiSnapshot {
        let bar = self.playhead_bar();
        let bar_index = bar.floor() as u32;
        let mut tracks = [TrackUi::default(); MAX_TRACKS];
        for i in 0..MAX_TRACKS {
            let onsets = self.counters.onsets[i].swap(0, Ordering::Relaxed);
            // Saturate, don't wrap: a wrapped counter reaches Swift as a huge
            // u32 and previously crashed the UI's density math.
            self.ui_bar_accum[i] = self.ui_bar_accum[i].saturating_add(onsets);
            self.ui_mask_accum[i] |= self.counters.mask[i].swap(0, Ordering::Relaxed);
            tracks[i] = TrackUi {
                onsets,
                velocity: f32::from_bits(self.counters.velocity[i].load(Ordering::Relaxed)),
                pitch: f32::from_bits(self.counters.pitch[i].load(Ordering::Relaxed)),
            };
        }
        while self.ui_last_bar < bar_index {
            // Finalize every fully-played bar into the waveform history.
            let energy = self.energy_at_bar(self.ui_last_bar);
            let (rms, peak) = self.drain_meter();
            let mut last_velocity = [0.0f32; MAX_TRACKS];
            for i in 0..MAX_TRACKS {
                last_velocity[i] = f32::from_bits(self.counters.velocity[i].load(Ordering::Relaxed));
            }
            self.ui_history.push_back(BarFrame {
                energy,
                onsets: self.ui_bar_accum,
                masks: self.ui_mask_accum,
                last_velocity,
                rms,
                peak,
                section_index: self.section_index_for(self.ui_last_bar) as u32,
            });
            if self.ui_history.len() > UI_HISTORY {
                self.ui_history.pop_front();
            }
            self.ui_bar_accum = [0; MAX_TRACKS];
            self.ui_mask_accum = [0; MAX_TRACKS];
            self.ui_last_bar += 1;
        }
        let (section_index, bar_in_section, section_bars, energy) = self.section_at(bar_index);
        UiSnapshot {
            bar,
            beat_phase: bar.fract(),
            energy,
            section_index,
            bar_in_section,
            section_bars,
            playing: self.playing.load(Ordering::Relaxed),
            tracks,
            current_masks: self.ui_mask_accum,
            history_len: self.ui_history.len(),
        }
    }

    pub fn ui_history(&self) -> impl Iterator<Item = &BarFrame> {
        self.ui_history.iter()
    }

    /// Control thread: canonical descriptors for the loaded session's tracks
    /// (issue #89) — index, engine id, display name and voice kind, in
    /// session track order. This order is the index space every per-track
    /// UI array uses.
    pub fn track_descriptors(&self) -> Vec<TrackDescriptor> {
        self.session
            .tracks
            .iter()
            .enumerate()
            .map(|(index, track)| TrackDescriptor {
                index,
                id: track.id.clone(),
                name: instrument_display_name(&track.instrument).to_string(),
                voice: TrackVoice::from(track.role),
            })
            .collect()
    }

    /// Copy up to `max` finalized bar frames (oldest first). Returns count.
    pub fn ui_history_copy(&self, out: &mut [BarFrame]) -> usize {
        let n = out.len().min(self.ui_history.len());
        let skip = self.ui_history.len() - n;
        for (dst, src) in out.iter_mut().zip(self.ui_history.iter().skip(skip)) {
            *dst = *src;
        }
        n
    }

    /// Ownership-free copy for the FFI boundary.
    pub fn ui_history_copy_owned(&self, max: usize) -> Vec<BarFrame> {
        let skip = self.ui_history.len().saturating_sub(max);
        self.ui_history.iter().skip(skip).copied().collect()
    }

    pub const UI_HISTORY_CAPACITY: usize = UI_HISTORY;

    fn energy_at_bar(&self, bar: u32) -> f32 {
        self.section_at(bar).3
    }

    /// Control thread: take the next closed (rms, peak) meter window for a
    /// finalized bar. When the RT side hasn't published a fresh window yet,
    /// the previous bar's values stand in; after a long UI stall the drain
    /// skips to the oldest retained slot so the pair stays recent.
    fn drain_meter(&mut self) -> (f32, f32) {
        let write = self.meter_write.load(Ordering::Acquire);
        if self.meter_read < write {
            let idx = self.meter_read.max(write.saturating_sub(METER_RING as u64));
            let packed = self.meter_ring[(idx as usize) % METER_RING].load(Ordering::Acquire);
            self.meter_read = idx + 1;
            self.last_meter = unpack_meter(packed);
        }
        self.last_meter
    }

    fn section_index_for(&self, bar: u32) -> usize {
        self.sections
            .iter()
            .rposition(|s| bar >= s.start_bar)
            .unwrap_or(0)
    }

    fn section_at(&self, bar: u32) -> (usize, u32, u32, f32) {
        let idx = self.section_index_for(bar);
        match self.sections.get(idx) {
            Some(sec) => {
                let bin = bar - sec.start_bar;
                (idx, bin, sec.bars, sec.energy_at(bin))
            }
            None => (0, 0, 0, 0.5),
        }
    }

    /// Non-RT: re-applies mixer/instrument settings from the session to the
    /// graph. Only safe **before rendering starts** (it touches the graph the
    /// audio thread owns); the constructor uses it, diff paths must not.
    pub fn apply_session_params(&mut self) {
        apply_session_to_graph(&mut self.graph, &self.session, &self.registry);
    }

    /// Control thread: keep the queue primed `PUMP_LOOKAHEAD_BARS` ahead of
    /// the playhead, looping the session so playback never runs dry. Cheap
    /// enough to call from a UI timer (allocations: one merged-event clone per
    /// 4-bar block worst case, all off-RT).
    pub fn pump(&mut self) {
        if self.compiled.is_empty() {
            // No known-good material at all (supervision #15 wiring): adopt
            // the built-in safe arrangement rather than starve the queue.
            self.engage_fallback();
            if self.compiled.is_empty() {
                return;
            }
        }
        let total_bars = u32::try_from(self.session.total_bars()).unwrap_or(u32::MAX);
        if total_bars == 0 {
            return;
        }
        let session_frames = self.lane.frame_of_bar(f64::from(total_bars));
        if session_frames == 0 {
            return;
        }
        let playhead = self.playhead_frame.load(Ordering::Relaxed);
        let cur_bar = self.lane.bar_at_frame(playhead);
        let horizon = self.lane.frame_of_bar(cur_bar + PUMP_LOOKAHEAD_BARS);
        let epoch = self.epoch.load(Ordering::Relaxed);

        while self.published_until < horizon {
            let src = &self.compiled[self.publish_idx];
            let shift = self.lap_offset;
            let end_frame = self.lane.frame_of_bar(f64::from(src.block.start_bar + src.block.bars)) + shift;
            let block = if shift == 0 {
                Arc::clone(&src.block)
            } else {
                let mut copy = (*src.block).clone();
                copy.start_frame += shift;
                Arc::new(copy)
            };
            let start_bar = block.start_bar;
            let prepared = PreparedBlock {
                block,
                events: Arc::clone(&src.events),
                end_frame,
                epoch,
            };
            let Ok(mut producer) = self.producer.lock() else { return };
            if !producer.publish(prepared) {
                break; // queue full: backpressure, retry on the next pump
            }
            drop(producer);
            self.published_until = end_frame;
            self.publish_idx += 1;
            // Section-aware mastering (#82): when a publish crosses into a
            // new section, feed its energy to the chain (control thread;
            // the chain smooths the transition internally).
            let sec_idx = self.section_index_for(start_bar);
            if self.published_section != Some(sec_idx) {
                self.published_section = Some(sec_idx);
                if let Some(sec) = self.sections.get(sec_idx) {
                    let energy = sec.energy_at(start_bar - sec.start_bar);
                    self.graph.set_mastering_section_energy(energy);
                }
            }
            if self.publish_idx >= self.compiled.len() {
                self.publish_idx = 0;
                self.lap_offset += session_frames;
                self.lap += 1;
                // Live production: every lap is a new take. The regeneration
                // (vary → validate → recompile) is where failure enters the
                // live engine, so it runs under supervision (#15 wiring,
                // issue #81): panics are contained, and on any failure the
                // previous known-good compiled set keeps playing — the music
                // never stops.
                let lap = self.lap;
                let fresh = catch_unwind(AssertUnwindSafe(|| self.compile_lap(lap)))
                    .unwrap_or_else(|_| Err(EngineError::Compile("regeneration panicked".into())));
                match fresh {
                    Ok(set) => self.compiled = set,
                    Err(_) => {
                        // Contained: keep the previous known-good lap and
                        // retry on the next wrap.
                        self.safety.record_regeneration_failure();
                    }
                }
            }
        }
        self.feed_mastering_gr_alarm();
    }

    /// Kill-switch feed for the mastering chain (#15/#28): the limiter's
    /// sustained-GR alarm latches inside the chain; each latch episode is
    /// counted into the supervision counters so `is_critical` can trip.
    /// Rising-edge detect, control thread, once per pump.
    fn feed_mastering_gr_alarm(&mut self) {
        let alarmed = self.graph.mastering_limiter_alarm();
        if alarmed && !self.mastering_alarm_counted {
            self.safety.record_mastering_gr_alarm();
        }
        self.mastering_alarm_counted = alarmed;
    }

    /// RT entry point (audio thread): fills `out_l`/`out_r` and advances the
    /// playhead. When the transport is stopped, renders silence without
    /// advancing. Alloc- and panic-free.
    pub fn render(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        let len = out_l.len().min(out_r.len());
        // The graph accumulates into these buffers (mix-bus semantics), so
        // the engine owns clearing them per callback — CoreAudio does not
        // guarantee incoming contents, and a stale sample would pile up.
        out_l[..len].fill(0.0);
        out_r[..len].fill(0.0);
        // #53 step 3b: control→RT commands land at block boundaries — the
        // drain runs once per callback, before any block is activated, so a
        // pool swap is boundary-aligned by construction. Exceptional and
        // alloc-bearing by design; steady-state rendering never gets here
        // with commands queued.
        while let Some(command) = self.command_consumer.pop() {
            match command {
                Command::AttachSample { track, data, sample_rate } => {
                    self.graph.attach_sampler(track, data, sample_rate);
                }
                Command::SwapTrack { track, swap } => {
                    self.graph.swap_track(track, swap);
                }
            }
        }
        if !self.playing.load(Ordering::Relaxed) {
            out_l[..len].fill(0.0);
            out_r[..len].fill(0.0);
            return;
        }
        let meter_from = self.playhead_frame.load(Ordering::Relaxed);
        let mut off = 0usize;
        while off < len {
            let frame = self.playhead_frame.load(Ordering::Relaxed) + off as u64;
            if let Some(a) = self.active.as_ref() {
                if frame >= a.block.start_frame && frame < a.end_frame {
                    let (block, events) = (Arc::clone(&a.block), Arc::clone(&a.events));
                    let n = ((a.end_frame - frame) as usize).min(len - off);
                    self.graph.render_block(
                        &mut out_l[off..off + n],
                        &mut out_r[off..off + n],
                        &events,
                        frame - block.start_frame,
                    );
                    off += n;
                    continue;
                }
            }
            match self.next_block_for(frame) {
                NextBlock::Now(p) => {
                    self.active_start_bar.store(p.block.start_bar, Ordering::Relaxed);
                    self.active_end_bar.store(p.block.start_bar + p.block.bars, Ordering::Relaxed);
                    let fpb = p.end_frame.saturating_sub(p.block.start_frame)
                        / p.block.bars.max(1) as u64;
                    self.active_start_frame.store(p.block.start_frame, Ordering::Relaxed);
                    self.frames_per_bar.store(fpb.max(1), Ordering::Relaxed);
                    self.meter_activate(p.block.start_frame, fpb.max(1));
                    self.active = Some(p);
                }
                NextBlock::Future(start) => {
                    let n = ((start - frame) as usize).min(len - off);
                    out_l[off..off + n].fill(0.0);
                    out_r[off..off + n].fill(0.0);
                    self.render_gaps.fetch_add(1, Ordering::Relaxed);
                    off += n;
                }
                NextBlock::None => {
                    out_l[off..].fill(0.0);
                    out_r[off..].fill(0.0);
                    self.render_gaps.fetch_add(1, Ordering::Relaxed);
                    break;
                }
            }
        }
        // The buffers now hold the exact mixed output for
        // [meter_from, meter_from + len) — silence gaps included — so folding
        // them in here meters what is actually heard, breakdowns as silence.
        self.meter_feed(&out_l[..len], &out_r[..len], meter_from);
        self.playhead_frame.fetch_add(len as u64, Ordering::Relaxed);
    }

    /// RT: refresh the meter's bar-window reference from an activating block.
    /// The first activation anchors the window; later ones only refresh the
    /// frames-per-bar so tempo changes re-space future boundaries.
    fn meter_activate(&mut self, start_frame: u64, fpb: u64) {
        if self.meter_fpb == 0 {
            self.meter_boundary = start_frame + fpb;
        }
        self.meter_fpb = fpb;
    }

    /// RT: fold `frames` frames of mixed output starting at `frame` into the
    /// open bar window, publishing a (rms, peak) window each time a bar
    /// boundary passes. Lock-free, allocation-free.
    fn meter_feed(&mut self, l: &[f32], r: &[f32], frame: u64) {
        if self.meter_fpb == 0 {
            return;
        }
        let mut offset = 0usize;
        while offset < l.len() {
            let pos = frame + offset as u64;
            if pos >= self.meter_boundary {
                self.meter_close_window();
                continue;
            }
            let take = ((self.meter_boundary - pos) as usize).min(l.len() - offset);
            for i in 0..take {
                let s = f64::from(l[offset + i]);
                let t = f64::from(r[offset + i]);
                self.meter_sq += (s * s + t * t) * 0.5;
                let p = l[offset + i].abs().max(r[offset + i].abs());
                if p > self.meter_peak {
                    self.meter_peak = p;
                }
            }
            self.meter_frames += take as u64;
            offset += take;
        }
    }

    /// RT: publish the open window as one packed (rms, peak) slot and open
    /// the next bar window.
    fn meter_close_window(&mut self) {
        let frames = self.meter_frames.max(1) as f64;
        let rms = (self.meter_sq / frames).sqrt().clamp(0.0, 1.0) as f32;
        let peak = self.meter_peak.clamp(0.0, 1.0);
        let write = self.meter_write.load(Ordering::Relaxed);
        let slot = (write as usize) % METER_RING;
        // Release orders the packed slot before the index that announces it;
        // the control side reads both with Acquire.
        self.meter_ring[slot].store(pack_meter(rms, peak), Ordering::Release);
        self.meter_write.store(write + 1, Ordering::Release);
        self.meter_sq = 0.0;
        self.meter_peak = 0.0;
        self.meter_frames = 0;
        self.meter_boundary += self.meter_fpb;
    }

    /// Control thread: validates + applies one diff op, recompiles the session
    /// and republishes every block at/after the currently sounding block's end
    /// (the next musical boundary). The sounding block keeps playing until
    /// then; stale queued blocks are dropped by the RT side via the epoch bump.
    ///
    /// `SwapInstrument` (issue #37) additionally queues the track's graph
    /// re-attach: the RT side swaps the strip at the next block boundary with
    /// a pool crossfade, so the new instrument fades in click-free.
    pub fn apply_diff_json(
        &mut self,
        diff_json: &str,
        at_bar: u32,
    ) -> Result<ApplyOutcome, EngineError> {
        let diff: IrDiff =
            serde_json::from_str(diff_json).map_err(|e| EngineError::DiffParse(e.to_string()))?;
        let report = kontinuum_ir::apply_diff(&mut self.session, &diff, at_bar).map_err(|e| {
            self.invalid_diffs.fetch_add(1, Ordering::Relaxed);
            EngineError::DiffRejected(e.to_string())
        })?;
        if let IrDiff::SwapInstrument { track, .. } = &diff {
            if let Some(ti) = self.session.tracks.iter().position(|t| &t.id == track) {
                let slot = u8::try_from(ti).unwrap_or(u8::MAX);
                let swap = crate::session_setup::swap_for(&self.session.tracks[ti], &self.registry);
                if !self.command_producer.send(Command::SwapTrack { track: slot, swap }) {
                    return Err(EngineError::DiffRejected(
                        "swap command queue full; retry after the next block".into(),
                    ));
                }
            }
        }
        self.recompile_and_refill();
        Ok(ApplyOutcome::from(report))
    }

    /// Control thread: replaces one track's instrument from plain instrument
    /// JSON (issue #97 library presets; the bridge stays catalog-agnostic —
    /// the caller resolves any catalog entry to `{"kind": …}` first).
    /// Validates the mutated session, re-attaches the track's strip so the
    /// new voice sounds on the next notes, and recompiles so future blocks
    /// carry the change. Like `load_sample`, this touches the graph: keep
    /// the transport stopped across the call.
    pub fn set_track_instrument(
        &mut self,
        track_id: &str,
        instrument_json: &str,
    ) -> Result<(), EngineError> {
        let instrument: InstrumentDef = serde_json::from_str(instrument_json)
            .map_err(|e| EngineError::InstrumentParse(e.to_string()))?;
        let ti = self
            .session
            .tracks
            .iter()
            .position(|t| t.id == track_id)
            .ok_or_else(|| EngineError::UnknownTrack(track_id.to_string()))?;

        let mut candidate = self.session.clone();
        candidate.tracks[ti].instrument = instrument;
        if let Err(errors) = validate_session(&candidate) {
            return Err(EngineError::from_validation(&errors));
        }

        self.session = candidate;
        let slot = u8::try_from(ti).unwrap_or(u8::MAX);
        apply_track(&mut self.graph, self.sample_rate, slot, &self.session.tracks[ti], &self.registry);
        self.recompile_and_refill();
        Ok(())
    }

    pub fn play(&mut self) {
        self.playing.store(true, Ordering::Relaxed);
    }

    pub fn stop(&mut self) {
        self.playing.store(false, Ordering::Relaxed);
        // The mastering chain's limiter lookahead must not leak stale
        // content into the next play (#82): drop its state exactly at stop.
        self.graph.reset_mastering();
    }

    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }

    /// Fractional bar position of the transport, via the session tempo lane.
    pub fn playhead_bar(&self) -> f64 {
        self.lane.bar_at_frame(self.playhead_frame.load(Ordering::Relaxed))
    }

    pub fn telemetry(&self) -> Telemetry {
        let queue_len = self.producer.lock().map(|p| p.len()).unwrap_or(0)
            + self.rt_pending.load(Ordering::Relaxed) as usize;
        let active = self.active_start_bar.load(Ordering::Relaxed);
        Telemetry {
            playhead_bar: self.playhead_bar(),
            playing: self.playing.load(Ordering::Relaxed),
            queue_len,
            active_block_bar: (active != u32::MAX).then_some(active),
            render_gaps: self.render_gaps.load(Ordering::Relaxed),
            invalid_diffs: self.invalid_diffs.load(Ordering::Relaxed),
            mastering: MasteringTelemetryLite::from(self.graph.mastering_telemetry()),
        }
    }

    pub fn safety_snapshot(&self) -> SafetyCounters {
        SafetyCounters {
            render_gaps: self.render_gaps.load(Ordering::Relaxed),
            invalid_diffs: self.invalid_diffs.load(Ordering::Relaxed),
            queue_overflows: self.queue_overflows.load(Ordering::Relaxed),
            regeneration_failures: self.safety.regeneration_failures,
            watchdog_fallbacks: self.safety.watchdog_fallbacks,
            mastering_gr_alarms: self.safety.mastering_gr_alarms,
            limiter_gr_alarm: self.graph.mastering_limiter_alarm(),
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Control thread: queue a PCM buffer (mono f32) as the sample played by
    /// `track`'s sampler pool. Safe while playing (#53 step 3b): the audio
    /// thread applies the attach at the next block boundary, where a pool
    /// swap cannot kill voices mid-note. Errors only on command overflow.
    pub fn load_sample(
        &mut self,
        track: u8,
        pcm: &[f32],
        sample_rate: u32,
    ) -> Result<(), EngineError> {
        let data: Arc<[f32]> = pcm.to_vec().into();
        if !self
            .command_producer
            .send(Command::AttachSample { track, data, sample_rate })
        {
            return Err(EngineError::DiffRejected(
                "sample command queue full; retry after the next block".into(),
            ));
        }
        Ok(())
    }

    /// Control thread: per-track kill mute (#14) — a click-free fade to exact
    /// zero and back, safe to call while the transport is playing.
    pub fn set_track_mute(&mut self, track: u8, muted: bool) {
        self.graph.set_track_mute(track, muted);
    }

    /// Control thread: per-track solo (#14) — silences every other strip with
    /// the same fade, safe to call while the transport is playing.
    pub fn set_track_solo(&mut self, track: u8, solo: bool) {
        self.graph.set_track_solo(track, solo);
    }

    /// Whether a mute close is currently requested on the track.
    pub fn track_muted(&self, track: u8) -> bool {
        self.graph.track_muted(track)
    }

    /// Whether the track is explicitly soloed.
    pub fn track_solo(&self, track: u8) -> bool {
        self.graph.track_solo(track)
    }

    /// Control thread: mastering bypass (#82) — bit-exact passthrough for
    /// A/B and as the kill-switch's first rung; safe while playing.
    pub fn set_mastering_bypass(&mut self, bypassed: bool) {
        self.graph.set_mastering_bypass(bypassed);
    }

    /// Control thread: mastering tilt target, dB (positive brightens,
    /// hard-capped ±3); safe while playing.
    pub fn set_mastering_tilt(&mut self, tilt_db: f32) {
        self.graph.set_mastering_tilt(tilt_db);
    }

    /// Control thread: section-aware see-through (0 = full-intensity
    /// section, 1 = breakdown); safe while playing.
    pub fn set_mastering_section_energy(&mut self, energy: f32) {
        self.graph.set_mastering_section_energy(energy);
    }

    /// Control thread: speaker-aware output profile (v0 approximation).
    pub fn set_mastering_output_profile(&mut self, profile: OutputProfile) {
        self.graph.set_mastering_output_profile(profile);
    }

    /// Control thread: the current session as pretty JSON — the saved
    /// composition. Diffs applied via `apply_diff_json` are baked in.
    pub fn export_session_json(&self) -> String {
        serde_json::to_string_pretty(&self.session).unwrap_or_else(|_| "{}".to_string())
    }

    /// Playhead position in absolute frames (for tests and tooling).
    pub fn playhead_frame(&self) -> u64 {
        self.playhead_frame.load(Ordering::Relaxed)
    }

    // -- control-side internals ----------------------------------------------

    /// Control thread: varies, validates and recompiles the session for one
    /// lap of playback. Pure with respect to engine state — returns the fresh
    /// compiled set or an error; the caller decides what to keep. Runs inside
    /// `catch_unwind` at the call site, so panics count as failures too.
    fn compile_lap(&self, lap: u32) -> Result<Vec<CompiledSource>, EngineError> {
        #[cfg(test)]
        if self.regen_fail_from.is_some_and(|from| lap >= from) {
            if self.regen_fail_panic {
                panic!("forced regeneration failure (test seam)");
            }
            return Err(EngineError::Compile("forced regeneration failure (test seam)".into()));
        }
        let varied = kontinuum_compose::taste::vary_session(&self.session, self.session.seed, lap);
        if let Err(errors) = validate_session(&varied) {
            return Err(EngineError::from_validation(&errors));
        }
        let blocks = compile_session(&varied, self.sample_rate)
            .map_err(|e| EngineError::Compile(e.to_string()))?;
        Ok(blocks.into_iter().map(compiled_source).collect())
    }

    /// Control thread, last resort (supervision #15): adopt the built-in safe
    /// arrangement as the engine's lap material when no known-good compiled
    /// set exists. The fallback session is 16 bars at a constant tempo, so
    /// the lap/wrap machinery stays frame-continuous from wherever playback
    /// already is.
    fn engage_fallback(&mut self) {
        let fallback = FallbackSource::new(self.session.seed, self.sample_rate);
        let session = fallback.session().clone();
        if let Ok(blocks) = compile_session(&session, self.sample_rate) {
            self.session = session;
            if let Ok(lane) = TempoLane::new(self.sample_rate, &self.session.tempo_lane) {
                self.lane = lane;
            }
            self.rebuild_sections();
            self.compiled = blocks.into_iter().map(compiled_source).collect();
        }
        self.safety.record_watchdog_fallback();
    }

    /// Recompiles from the (already-mutated) session and republishes future
    /// blocks from the currently sounding block's end. The epoch bump makes
    /// the RT side drop queued material from the previous compile.
    fn recompile_and_refill(&mut self) {
        let blocks = match compile_session(&self.session, self.sample_rate) {
            Ok(b) => b,
            Err(e) => {
                self.invalid_diffs.fetch_add(1, Ordering::Relaxed);
                debug_assert!(false, "recompile after accepted diff failed: {e}");
                return;
            }
        };
        self.epoch.fetch_add(1, Ordering::Relaxed);
        self.rebuild_sections();
        self.compiled = blocks.into_iter().map(compiled_source).collect();

        let total_bars = u32::try_from(self.session.total_bars()).unwrap_or(u32::MAX);
        let session_frames = self.lane.frame_of_bar(f64::from(total_bars));
        let playhead = self.playhead_frame.load(Ordering::Relaxed);
        let min_start_bar = self.active_end_bar.load(Ordering::Relaxed);
        self.lap_offset = if session_frames > 0 { playhead / session_frames * session_frames } else { 0 };
        self.publish_idx = self
            .compiled
            .iter()
            .position(|c| c.block.start_bar >= min_start_bar)
            .unwrap_or(0);
        self.published_until = self
            .compiled
            .get(self.publish_idx)
            .map(|c| self.lane.frame_of_bar(f64::from(c.block.start_bar)) + self.lap_offset)
            .unwrap_or(u64::MAX);
        self.pump();
    }

    // -- RT-side internals -----------------------------------------------------

    /// RT: drain the queue into the pending slot list (dropping stale-epoch
    /// blocks), expire ended pendings, and classify the next one.
    fn next_block_for(&mut self, frame: u64) -> NextBlock {
        let current_epoch = self.epoch.load(Ordering::Relaxed);
        while let Some(p) = self.consumer.pop() {
            if p.epoch != current_epoch {
                continue; // superseded by a diff recompile
            }
            if self.pending.len() >= self.pending.capacity() {
                self.queue_overflows.fetch_add(1, Ordering::Relaxed);
                break;
            }
            self.pending.push(p);
        }
        self.pending.retain(|p| p.epoch == current_epoch && p.end_frame > frame);
        self.rt_pending.store(self.pending.len() as u32, Ordering::Relaxed);
        if let Some(first) = self.pending.first() {
            if first.block.start_frame <= frame {
                return NextBlock::Now(self.pending.remove(0));
            }
            return NextBlock::Future(first.block.start_frame);
        }
        NextBlock::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;
    const CHUNK: usize = 512;

    /// 4-bar one-kick session; one lap is ~7.74 s at 124 bpm.
    fn short_session() -> String {
        r#"{
            "version": 1, "seed": 7,
            "tempo_lane": [[0, 124.0]],
            "sections": [{"id": "a", "bars": 4, "energy_curve": [0.5, 0.5, 0.6, 0.6],
                "pattern_bindings": {"k": {"generator": "euclidean", "k": 4, "n": 16}}}],
            "tracks": [{"id": "k", "role": "kick", "instrument": {"kind": "kick"}}]
        }"#
        .to_string()
    }

    /// Renders `seconds` of audio at the facade tests' pump cadence, returning
    /// the peak amplitude measured from `from_seconds` onward.
    fn drive(engine: &mut KontinuumEngine, seconds: f64, from_seconds: f64) -> f32 {
        let chunks_total = (seconds * f64::from(SR) / CHUNK as f64) as usize;
        let cps = SR as usize / CHUNK;
        let mut l = [0.0f32; CHUNK];
        let mut r = [0.0f32; CHUNK];
        let mut peak = 0.0f32;
        for i in 0..chunks_total {
            engine.render(&mut l, &mut r);
            if i as f64 / cps as f64 >= from_seconds {
                peak = peak.max(l.iter().fold(0.0f32, |m, s| m.max(s.abs())));
            }
            if i % cps == 0 {
                engine.pump();
            }
        }
        peak
    }

    #[test]
    fn failing_regeneration_keeps_previous_lap_and_counts() {
        let mut engine = KontinuumEngine::new(SR, &short_session()).expect("engine");
        engine.play();
        engine.regen_fail_from = Some(1); // every wrap from the next lap fails
        engine.regen_fail_panic = false;

        // ~26 s is more than three 4-bar laps past the constructor's lookahead.
        let peak = drive(&mut engine, 26.0, 9.0);

        let t = engine.telemetry();
        assert_eq!(t.render_gaps, 0, "the music must never stop");
        assert!(t.playhead_bar > 12.0, "playhead advanced: {}", t.playhead_bar);
        assert!(peak > 0.01, "audio kept playing across failed laps, peak {peak}");
        let s = engine.safety_snapshot();
        assert!(s.regeneration_failures >= 2, "failures must accumulate: {s:?}");
        assert_eq!(s.watchdog_fallbacks, 0, "known-good lap made fallback unnecessary");
        assert!(engine.lap >= 2, "wraps kept happening: lap {}", engine.lap);
    }

    #[test]
    fn panicking_regeneration_is_contained_and_counted() {
        let mut engine = KontinuumEngine::new(SR, &short_session()).expect("engine");
        engine.play();
        engine.regen_fail_from = Some(1);
        engine.regen_fail_panic = true;

        let peak = drive(&mut engine, 18.0, 9.0);

        let t = engine.telemetry();
        assert_eq!(t.render_gaps, 0, "panics must not reach the audio stream");
        assert!(peak > 0.01, "audio kept playing across the panicking lap, peak {peak}");
        let s = engine.safety_snapshot();
        assert!(s.regeneration_failures >= 1, "contained panic must count: {s:?}");
    }

    #[test]
    fn no_known_good_material_serves_the_fallback_arrangement() {
        let mut engine = KontinuumEngine::new(SR, &short_session()).expect("engine");
        // Simulate total loss of the compiled set before anything serves.
        engine.compiled.clear();
        engine.publish_idx = 0;
        engine.play();
        engine.pump();

        assert!(!engine.compiled.is_empty(), "fallback arrangement adopted");
        assert_eq!(
            engine.session.total_bars() as u32,
            kontinuum_supervision::FALLBACK_BARS,
            "the built-in safe arrangement is the session now"
        );
        assert_eq!(engine.safety_snapshot().watchdog_fallbacks, 1);
        assert_eq!(engine.safety_snapshot().regeneration_failures, 0);

        // ~45 s is ~1.5 laps of the 16-bar fallback: it must loop gaplessly.
        let peak = drive(&mut engine, 45.0, 1.0);
        assert!(peak > 0.01, "fallback must be audible, peak {peak}");
        assert_eq!(engine.telemetry().render_gaps, 0, "fallback must loop gaplessly");
        assert_eq!(
            engine.safety_snapshot().watchdog_fallbacks,
            1,
            "adopted exactly once, then it just plays"
        );
    }

    #[test]
    fn bar_frames_carry_measured_loudness_and_section_boundaries() {
        // Two 4-bar sections: driving past bar 4 must finalize frames whose
        // rms/peak describe the actual audio and whose section index flips.
        let session = r#"{
            "version": 1, "seed": 7,
            "tempo_lane": [[0, 124.0]],
            "sections": [
                {"id": "a", "bars": 4, "energy_curve": [0.4, 0.4, 0.45, 0.45],
                 "pattern_bindings": {"k": {"generator": "euclidean", "k": 4, "n": 16}}},
                {"id": "b", "bars": 4, "energy_curve": [0.9, 0.9, 0.9, 0.9],
                 "pattern_bindings": {"k": {"generator": "euclidean", "k": 7, "n": 16},
                                      "h": {"generator": "euclidean", "k": 9, "n": 16}}}
            ],
            "tracks": [
                {"id": "k", "role": "kick", "instrument": {"kind": "kick"}},
                {"id": "h", "role": "perc", "instrument": {"kind": "hat"}}
            ]
        }"#
        .to_string();
        let mut engine = KontinuumEngine::new(SR, &session).expect("engine");
        engine.play();

        // ~10.5 s with snapshot polls: bar 5 of 8 is well behind the playhead.
        let chunks_total = (10.5 * f64::from(SR) / CHUNK as f64) as usize;
        let mut l = [0.0f32; CHUNK];
        let mut r = [0.0f32; CHUNK];
        for i in 0..chunks_total {
            engine.render(&mut l, &mut r);
            if i % 15 == 0 {
                engine.pump();
                engine.ui_snapshot();
            }
        }

        let history: Vec<BarFrame> = engine.ui_history().copied().collect();
        assert!(history.len() >= 5, "bars finalized: {}", history.len());
        for f in &history {
            assert!(f.rms.is_finite() && f.peak.is_finite(), "meter stays finite: {f:?}");
            assert!((0.0..=1.0).contains(&f.rms) && (0.0..=1.0).contains(&f.peak));
            assert!(f.rms <= f.peak + 1e-6, "rms cannot exceed peak: {f:?}");
        }
        assert!(
            history.iter().any(|f| f.rms > 0.0),
            "measured audio lands in the history, not just section scalars"
        );
        assert!(
            history.windows(2).any(|w| w[0].section_index != w[1].section_index),
            "the section boundary at bar 4 shows up as a section_index flip"
        );
        assert_eq!(history[0].section_index, 0);
    }

    #[test]
    fn track_descriptors_match_session_order_ids_and_voices() {
        let session = r#"{
            "version": 1, "seed": 7,
            "tempo_lane": [[0, 124.0]],
            "sections": [{"id": "a", "bars": 4, "energy_curve": [0.5],
                "pattern_bindings": {
                    "k": {"generator": "euclidean", "k": 4, "n": 16},
                    "p": {"generator": "euclidean", "k": 3, "n": 16},
                    "b": {"generator": "euclidean", "k": 2, "n": 16},
                    "h": {"generator": "euclidean", "k": 1, "n": 16}
                }
            }],
            "tracks": [
                {"id": "k", "role": "kick", "instrument": {"kind": "kick"}},
                {"id": "p", "role": "perc", "instrument": {"kind": "hat"}},
                {"id": "b", "role": "bass", "instrument": {"kind": "bass"}},
                {"id": "h", "role": "pad", "instrument": {"kind": "ep"}}
            ]
        }"#
        .to_string();
        let engine = KontinuumEngine::new(SR, &session).expect("engine");

        let d = engine.track_descriptors();
        assert_eq!(
            d.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            vec!["k", "p", "b", "h"],
            "descriptors follow session track order — the index space every \
             per-track UI array uses"
        );
        assert_eq!(
            d.iter().map(|t| t.voice).collect::<Vec<_>>(),
            vec![TrackVoice::Kick, TrackVoice::Perc, TrackVoice::Bass, TrackVoice::Pad]
        );
        assert_eq!(
            d.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            vec!["Kick", "Hi-hat", "Bass", "Electric piano"],
            "display names come from the instrument kind, not the id"
        );
        assert_eq!(d.iter().map(|t| t.index).collect::<Vec<_>>(), vec![0, 1, 2, 3]);
    }

    /// Issue #37: `swap_instrument` applied while playing must become audible
    /// without a restart — the RT strip swaps at the next boundary and the
    /// pool crossfade lands click-free (here: fades a drone to exact silence).
    #[test]
    fn swap_instrument_diff_reaches_the_graph_crossfaded() {
        let session = r#"{
            "version": 1, "seed": 7,
            "tempo_lane": [[0, 124.0]],
            "sections": [{"id": "a", "bars": 8, "energy_curve": [0.6, 0.6, 0.7, 0.7],
                "pattern_bindings": {"p": {"generator": "euclidean", "k": 2, "n": 16,
                    "gate": 16.0, "pitch": 48.0}}}],
            "tracks": [{"id": "p", "role": "pad", "instrument": {"kind": "custom", "patch": {
                "nodes": [
                    {"id": "o", "type": "osc", "wave": "sine", "level": 0.7},
                    {"id": "x", "type": "out", "level": 0.9}],
                "edges": [{"from": "o", "to": "x", "type": "audio"}]}}}]
        }"#;
        let mut engine = KontinuumEngine::new(SR, session).expect("engine");
        engine.play();

        let mut l = [0.0f32; CHUNK];
        let mut r = [0.0f32; CHUNK];
        let peak_of = |buf: &[f32]| buf.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        let chunks_per_sec = (SR as usize / CHUNK).max(1);
        for _ in 0..chunks_per_sec {
            engine.render(&mut l, &mut r);
        }
        assert!(peak_of(&l) > 0.01, "the patch drone must be audible before the swap");

        let diff = r#"{"op":"swap_instrument","track":"p","instrument":{"kind":"custom",
            "patch":{"nodes":[{"id":"x","type":"out"}],"edges":[]}}}"#;
        engine
            .apply_diff_json(diff, engine.playhead_bar().floor() as u32)
            .expect("swap_instrument must apply");

        // Fade is ~20 ms; after a second the old voice must be fully retired.
        let mut late_peak = 0.0f32;
        for i in 0..chunks_per_sec {
            engine.render(&mut l, &mut r);
            if i > chunks_per_sec / 2 {
                late_peak = late_peak.max(peak_of(&l));
            }
        }
        assert!(
            late_peak < 0.005,
            "swap to a silent patch must fade out, peak {late_peak}"
        );
        let s = engine.safety_snapshot();
        assert_eq!(s.render_gaps, 0, "the swap must not stall the render path");
    }
}
