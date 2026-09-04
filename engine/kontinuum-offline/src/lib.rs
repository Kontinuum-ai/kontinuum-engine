//! `kontinuum-offline` — deterministic offline renderer: IR JSON → WAV
//! (issues #10/#11). Pure, single-threaded, bit-reproducible.
//!
//! Pipeline: `validate_session` → `compile_session` → `AudioGraph` render of
//! the chained 4-bar blocks into one stereo buffer → optional WAV file.
//!
//! Beyond the plain render: [`premium`] is the higher-quality export path
//! of the same session log (#28 — linear-phase EQ, ×8 oversampled
//! limiting, loudness normalization, TPDF dither; used by exports/
//! bookmarks #31), and [`ab`] renders the loudness-matched (mix, master)
//! pairs + manifest the blind listening protocol consumes (#32).
//!
//! Mapping notes (v0):
//! - Tracks attach by role in document order (track index = graph strip):
//!   Kick→Kick, Perc→Hat, Bass→Bass, Pad→Pad, Fx→Hat.
//! - IR `InstrumentDef` values ride `Event::ParamRamp` into the voice param
//!   table; because that path is a 10 ms one-pole smoother, a priming silence
//!   of `sample_rate / 10` frames is rendered (and discarded) so every initial
//!   value is settled before bar 0.
//! - Automation lanes land in blocks as `ParamRamp` events straight from the
//!   compiler, but with the compiler's ParamId layout (class byte `0x01..0x06`
//!   | track). `kontinuum_schedule::retarget_automation_params` translates
//!   them to the core routing ids (`ROUTE_*`, `SATURATE_DRIVE`) the graph
//!   dispatches on.
//! - Inserts: `drive` maps to core `Saturate`, `filter` to core
//!   `FilterInsert` (SVF), both through a wet/dry wrapper honoring the IR
//!   `mix`. Delay/Reverb/Chorus/Compressor inserts are skipped in v0 — no
//!   1:1 core insert exists yet; add them alongside core.

use std::path::Path;
use std::sync::Arc;

use kontinuum_clock::TempoLane;
use kontinuum_core::fx::{Chorus, Delay, FilterInsert, FilterMode, FreqShifter, Phaser, Reverb, ReverbV2, Saturate, TransientDesigner};
use kontinuum_core::params::{CHORUS_DEPTH, CHORUS_RATE, PHASER_DEPTH, PHASER_FEEDBACK, PHASER_RATE, PHASER_STAGES, SHIFT_HZ, TAPE_FLUTTER, TAPE_SAT, TAPE_WOW, TRANSIENT_ATTACK, TRANSIENT_SUSTAIN};
use kontinuum_core::params::{ROUTE_SEND_DELAY, ROUTE_SEND_REVERB};
use kontinuum_core::{fnv1a64, AudioGraph, BusFx, InsertFx, ParamId, MAX_TRACKS, BLOCK_FRAMES};
use kontinuum_plugin_api::Registry;
use kontinuum_ir::{compile_session, validate_session, CompileError, DelayFlavor, InstrumentDef, InsertDef, InsertKind, ReverbFlavor, SendFxSpec, Session, TrackRole};
use kontinuum_schedule::{retarget_automation_params, CompiledBlock, Event, RampCurve};

pub mod ab;
pub mod fft;
pub mod premium;

pub use ab::{render_ab, write_ab, AbManifest, AbPair};
pub use premium::{
    premium_golden_hash, premium_master, premium_master_peaks_bypassed, premium_master_with_drive,
    premium_render, premium_render_to_wav, write_wav16, PremiumDrive, PremiumRender,
    PREMIUM_OVERSAMPLE,
};

/// Offline render ceiling; stricter than the IR compile ceiling (4096).
pub const MAX_RENDER_BARS: u64 = 2048;
/// Sample rate used by the file-based convenience entry points.
pub const DEFAULT_SAMPLE_RATE: u32 = 48_000;

#[derive(Debug)]
pub enum RenderError {
    InvalidSession(String),
    Compile(CompileError),
    Json(serde_json::Error),
    Io(std::io::Error),
    Wav(hound::Error),
    TooLong,
    TooManyTracks(usize),
    /// A/B loudness matching needs audible program on both sides.
    Silent(String),
    /// A mute-set named a track index the session does not have.
    NoSuchTrack(usize),
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderError::InvalidSession(codes) => write!(f, "session failed validation: {codes}"),
            RenderError::Compile(e) => write!(f, "compile failed: {e}"),
            RenderError::Json(e) => write!(f, "invalid session JSON: {e}"),
            RenderError::Io(e) => write!(f, "io error: {e}"),
            RenderError::Wav(e) => write!(f, "wav encode failed: {e}"),
            RenderError::TooLong => write!(f, "session exceeds the {MAX_RENDER_BARS}-bar offline render ceiling"),
            RenderError::TooManyTracks(n) => write!(f, "session has {n} tracks; the fixed graph holds {MAX_TRACKS}"),
            RenderError::Silent(why) => write!(f, "loudness matching failed: {why}"),
            RenderError::NoSuchTrack(i) => write!(f, "no track at index {i}"),
        }
    }
}

impl std::error::Error for RenderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RenderError::Compile(e) => Some(e),
            RenderError::Json(e) => Some(e),
            RenderError::Io(e) => Some(e),
            RenderError::Wav(e) => Some(e),
            _ => None,
        }
    }
}

impl From<CompileError> for RenderError {
    fn from(e: CompileError) -> Self {
        RenderError::Compile(e)
    }
}

impl From<serde_json::Error> for RenderError {
    fn from(e: serde_json::Error) -> Self {
        RenderError::Json(e)
    }
}

impl From<std::io::Error> for RenderError {
    fn from(e: std::io::Error) -> Self {
        RenderError::Io(e)
    }
}

impl From<hound::Error> for RenderError {
    fn from(e: hound::Error) -> Self {
        RenderError::Wav(e)
    }
}

/// Rendered stereo program; `left`/`right` are equal length, one sample frame
/// per element, non-interleaved.
#[derive(Clone, Debug)]
pub struct RenderOutput {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    pub sample_rate: u32,
}

impl RenderOutput {
    /// FNV-1a 64 over interleaved f32 bit patterns (LE), left before right.
    /// The golden-regression fingerprint of a render.
    pub fn fnv_hash(&self) -> u64 {
        let mut bytes = Vec::with_capacity(self.left.len() * 8);
        for (l, r) in self.left.iter().zip(self.right.iter()) {
            bytes.extend_from_slice(&l.to_bits().to_le_bytes());
            bytes.extend_from_slice(&r.to_bits().to_le_bytes());
        }
        fnv1a64(&bytes)
    }
}

/// Reads and parses a session document from disk.
pub fn parse_session(path: &Path) -> Result<Session, RenderError> {
    let raw = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&raw)?)
}

/// How an offline render treats the graph's built-in mastering chain and
/// the session's tracks (#102). [`RenderOptions::mix`] reproduces exactly
/// what `render_session` has always rendered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderOptions {
    /// Run the graph's #98 real-time mastering chain on the master bus.
    /// `false` renders the raw mix — what an offline chain (see
    /// [`premium`]) or a stem deliverable wants as its input.
    pub mastering: bool,
    /// Track indices to mute for this pass, as a stem mute-set.
    ///
    /// Muting closes the strip's `KillFade` and additionally zeroes the
    /// track's delay/reverb sends: the graph taps sends *pre*-mute on
    /// purpose (fx tails ring out past a console mute), which is right for
    /// a live mute and wrong for a stem, where the muted track must leave
    /// no trace on the shared buses.
    ///
    /// What muting deliberately does *not* change: the track's voice still
    /// renders and still goes through the AutoMixer, so a muted track keeps
    /// its influence on the mixer's *shared* state — a muted kick both keys
    /// the #76 sidechain duck on every other track and feeds the gain-staging
    /// anchor that sets every track's servo target (`AutoMixer::process_track`
    /// runs before the mute multiplier, by construction).
    ///
    /// That is the point, not an oversight. A stem is supposed to be the
    /// track *as it sits in the mix*: ducked when the mix ducks it, staged
    /// against the same reference level. Silencing the muted tracks instead
    /// would give each stem its own private gain staging and no duck, and the
    /// stems would no longer line up with the record they came from.
    ///
    /// The consequence is that stems are **not additive**: summing them does
    /// not reconstruct the full mix. What a muted track must never do is put
    /// *audio* into the master or the shared delay/reverb buses, and that is
    /// what the mute and the send-zeroing (plus [`suppress_muted_sends`])
    /// guarantee.
    pub muted_tracks: Vec<usize>,
}

impl RenderOptions {
    /// The mastered full mix — the historical `render_session` behavior.
    pub fn mix() -> Self {
        RenderOptions { mastering: true, muted_tracks: Vec::new() }
    }

    /// Unmastered render, for callers that master offline themselves.
    pub fn unmastered() -> Self {
        RenderOptions { mastering: false, muted_tracks: Vec::new() }
    }

    /// Mute every track except `keep` — the per-track stem mute-set.
    pub fn stem(track_count: usize, keep: usize) -> Self {
        RenderOptions {
            mastering: false,
            muted_tracks: (0..track_count).filter(|&i| i != keep).collect(),
        }
    }
}

/// Renders a validated session end to end at `sample_rate`, mastered, with
/// every track audible.
pub fn render_session(session: &Session, sample_rate: u32) -> Result<RenderOutput, RenderError> {
    render_session_with(session, sample_rate, &RenderOptions::mix())
}

/// Renders a validated session under explicit [`RenderOptions`] (#102).
pub fn render_session_with(
    session: &Session,
    sample_rate: u32,
    options: &RenderOptions,
) -> Result<RenderOutput, RenderError> {
    let (mut graph, blocks, total) = build_render_graph(session, sample_rate)?;
    graph.set_mastering_bypass(!options.mastering);
    for &track in &options.muted_tracks {
        if track >= session.tracks.len() {
            return Err(RenderError::NoSuchTrack(track));
        }
        let ti = track as u8;
        graph.set_track_mute(ti, true);
        // Sends tap pre-mute in the graph; a stem needs them silent too.
        graph.set_track_send(ti, 0, 0.0);
        graph.set_track_send(ti, 1, 0.0);
    }
    Ok(render_with_graph(
        &mut graph,
        &blocks,
        total,
        sample_rate,
        &options.muted_tracks,
    ))
}

/// Shared offline render setup (#102): validate → compile → graph build
/// (voices/patches/gain/pan/sends/inserts/initial params). The plain render
/// and the mute-set stem render both build through this one mapping path;
/// the returned graph has NOT been primed yet.
fn build_render_graph(
    session: &Session,
    sample_rate: u32,
) -> Result<(AudioGraph, Vec<Arc<CompiledBlock>>, usize), RenderError> {
    if session.total_bars() > MAX_RENDER_BARS {
        return Err(RenderError::TooLong);
    }
    validate_session(session).map_err(|errors| {
        let codes: Vec<&str> = errors.iter().map(|e| e.code).collect();
        RenderError::InvalidSession(codes.join(", "))
    })?;
    if session.tracks.len() > MAX_TRACKS {
        return Err(RenderError::TooManyTracks(session.tracks.len()));
    }
    let blocks = compile_session(session, sample_rate)?;
    // `compile_session` already rejected bad tempo lanes, so this mirrors the
    // lane it used internally (same constructor, same inputs).
    let lane = TempoLane::new(sample_rate, &session.tempo_lane)
        .map_err(|e| RenderError::Compile(CompileError::Tempo { reason: e.reason }))?;
    let total = lane.frame_of_bar(session.total_bars() as f64) as usize;

    let registry = kontinuum_instruments_core::registry();
    let mut graph = AudioGraph::new(sample_rate);
    graph.set_send_fx(
        delay_bus(&session.send_fx, sample_rate),
        reverb_bus(&session.send_fx, sample_rate),
    );
    // Issue #76: the session's groove template retimes the pump.
    graph.set_duck_release_ms(session.duck_release_ms);
    for (i, track) in session.tracks.iter().enumerate() {
        let ti = i as u8;
        if let Some(id) = kind_id_for(&track.instrument, track.role) {
            if let Some(factory) = registry.voice_factory(id) {
                graph.attach_with(ti, &factory);
            }
        } else {
            graph.attach_silent(ti);
        }
        // Issue #76: an explicit IR duck depth overrides the attach's role
        // default; `None` keeps it.
        if let Some(depth) = track.duck_depth {
            graph.set_track_duck_depth(ti, depth);
        }
        // Issue #37/#97: custom patches render through their own graph —
        // same seam as the live engine's session_setup. The role-fallback
        // attach above keeps the mix identity; this swaps the sound source.
        if let InstrumentDef::Custom(c) = &track.instrument {
            if let Ok(plan) = kontinuum_ir::compile::compile_patch(c) {
                graph.attach_patch(ti, &plan);
            }
        }
        graph.snap_track_gain(ti, track.gain);
        graph.snap_track_pan(ti, (track.pan + 1.0) / 2.0);
        graph.set_track_send(ti, 0, track.sends.delay);
        graph.set_track_send(ti, 1, track.sends.reverb);
        for (slot, insert) in track.inserts.iter().enumerate() {
            if let Some(fx) = insert_fx(insert, sample_rate) {
                graph.set_insert(ti, slot, fx);
            }
        }
        apply_initial_params(&mut graph, ti, &track.instrument, &registry);
    }
    Ok((graph, blocks, total))
}

/// Shared render tail (#102): the discarded priming run (lets every initial
/// ramp — instrument params, sends, mute fades — settle before bar 0), then
/// the block loop.
fn render_with_graph(
    graph: &mut AudioGraph,
    blocks: &[Arc<CompiledBlock>],
    total: usize,
    sample_rate: u32,
    muted: &[usize],
) -> RenderOutput {
    kontinuum_core::enable_denormal_protection();
    let mut prime_l = vec![0.0f32; sample_rate as usize / 10];
    let mut prime_r = vec![0.0f32; sample_rate as usize / 10];
    graph.render_block(&mut prime_l, &mut prime_r, &[], 0);

    let mut left = vec![0.0f32; total];
    let mut right = vec![0.0f32; total];
    for (i, block) in blocks.iter().enumerate() {
        let start = block.start_frame as usize;
        let end = blocks
            .get(i + 1)
            .map(|b| b.start_frame as usize)
            .unwrap_or(total)
            .min(total);
        let mut events = AudioGraph::prepare_block(block);
        retarget_automation_params(&mut events);
        suppress_muted_sends(&mut events, muted);
        // Compiler event frames are block-relative and the event cursor
        // compares them against `buf_start`, so tiles render with buf_start 0
        // over the block's slice of the output.
        graph.render_block(&mut left[start..end], &mut right[start..end], &events, 0);
    }

    RenderOutput { left, right, sample_rate }
}

/// Drops send automation aimed at a muted track (#102).
///
/// Zeroing a muted track's sends when the graph is built is not enough:
/// an automation lane on `send_delay`/`send_reverb` compiles to `ParamRamp`
/// events that `retarget_automation_params` maps onto `ROUTE_SEND_*`, and
/// the graph honors them mid-render — putting the send straight back. Since
/// sends tap *pre*-mute, that leaks a silenced track into the shared
/// delay/reverb buses and into every stem. The fixture session automates
/// `pad.send_reverb`, so this is the common case, not a corner one.
///
/// Only the two send params are dropped. The muted track's note events
/// still dispatch, which is what keeps the #76 kick duck alive in a stem,
/// and gain/pan automation is harmless behind a closed mute.
fn suppress_muted_sends(events: &mut Vec<(u32, u8, Event)>, muted: &[usize]) {
    if muted.is_empty() {
        return;
    }
    events.retain(|(_, track, event)| {
        let is_send = matches!(
            event,
            Event::ParamRamp { param, .. }
                if *param == ROUTE_SEND_DELAY || *param == ROUTE_SEND_REVERB
        );
        !(is_send && muted.contains(&(*track as usize)))
    });
}

/// 32-bit float stereo WAV writer (hound).
pub fn write_wav(path: &Path, out: &RenderOutput) -> Result<(), RenderError> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: out.sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for (l, r) in out.left.iter().zip(out.right.iter()) {
        writer.write_sample(*l)?;
        writer.write_sample(*r)?;
    }
    writer.finalize()?;
    Ok(())
}

/// Convenience: parse a session JSON file → render at [`DEFAULT_SAMPLE_RATE`]
/// → write a WAV file.
pub fn render_to_wav(session_json_path: &Path, out_wav_path: &Path) -> Result<(), RenderError> {
    let session = parse_session(session_json_path)?;
    let out = render_session(&session, DEFAULT_SAMPLE_RATE)?;
    write_wav(out_wav_path, &out)
}

fn kind_id_for(inst: &InstrumentDef, role: TrackRole) -> Option<&'static str> {
    // The instrument definition wins when it names a concrete machine; the
    // role decides only for generic/sample/patch tracks (preserves the
    // pre-#51 fallback: those tracks sounded like their role's voice).
    inst.kind_id().or_else(|| match inst {
        InstrumentDef::Sample(_) | InstrumentDef::Custom(_) => Some(match role {
            TrackRole::Kick => "kick",
            TrackRole::Perc | TrackRole::Fx => "hat",
            TrackRole::Bass => "bass",
            TrackRole::Pad => "pad",
        }),
        _ => None,
    })
}

/// IR instrument values → voice param table, via the graph's ParamRamp path
/// (duration 1, Smooth). Callers must render priming silence afterwards: the
/// ramp lands on a 10 ms one-pole smoother, not a snap.
fn apply_initial_params(
    graph: &mut AudioGraph,
    track: u8,
    def: &InstrumentDef,
    registry: &Registry,
) {
    let Some(id) = def.kind_id() else {
        return; // Sample/Custom have no synth params (unchanged).
    };
    let Some(plugin) = registry.get(id) else { return };
    let schema = plugin.params();
    for (name, value) in def.param_values() {
        if let Some(spec) = schema.iter().find(|s| s.name == name) {
            graph.apply_event(
                track,
                Event::ParamRamp {
                    param: spec.param,
                    target: value,
                    duration_frames: 1,
                    curve: RampCurve::Smooth,
                },
            );
        }
    }
}

/// Wet/dry wrapper: core inserts process in-place, the IR `mix` wants a blend,
/// so the dry signal is kept in a fixed scratch (render tiles are ≤ 64 frames).
struct MixedInsert {
    fx: Box<dyn InsertFx>,
    mix: f32,
    dry: Box<[f32; BLOCK_FRAMES]>,
}

impl MixedInsert {
    fn new(fx: Box<dyn InsertFx>, mix: f32) -> Self {
        MixedInsert { fx, mix: mix.clamp(0.0, 1.0), dry: Box::new([0.0; BLOCK_FRAMES]) }
    }
}

impl InsertFx for MixedInsert {
    fn render(&mut self, io: &mut [f32]) {
        let n = io.len();
        self.dry[..n].copy_from_slice(io);
        self.fx.render(io);
        for (i, slot) in io.iter_mut().enumerate() {
            *slot = self.dry[i] * (1.0 - self.mix) + *slot * self.mix;
        }
    }

    fn set_param(&mut self, param: ParamId, value: f32) {
        self.fx.set_param(param, value);
    }

    fn reset(&mut self) {
        self.fx.reset();
    }
}

fn insert_fx(def: &InsertDef, sample_rate: u32) -> Option<Box<dyn InsertFx>> {
    let param = |key: &str, default: f32| -> f32 {
        def.params.get(key).and_then(|v| v.as_f64()).map_or(default, |v| v as f32)
    };
    match def.kind {
        InsertKind::Drive => {
            let drive = def.params.get("amount").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
            Some(Box::new(MixedInsert::new(Box::new(Saturate::new(drive)), def.mix)))
        }
        InsertKind::Filter => {
            let cutoff = def.params.get("cutoff_hz").and_then(|v| v.as_f64()).unwrap_or(1000.0) as f32;
            let resonance = def.params.get("resonance").and_then(|v| v.as_f64()).unwrap_or(0.05) as f32;
            let mode = match def.params.get("mode").and_then(|v| v.as_str()) {
                Some("highpass") => FilterMode::HighPass,
                Some("bandpass") => FilterMode::BandPass,
                _ => FilterMode::LowPass,
            };
            let fx = FilterInsert::new(sample_rate, cutoff, resonance, mode);
            Some(Box::new(MixedInsert::new(Box::new(fx), def.mix)))
        }
        // FX v2 (#30): the core insert inventory finally has hosts. The IR
        // `mix` blends through the shared wet/dry wrapper; rate/depth-style
        // params come from the insert's own `params` object.
        InsertKind::Chorus => {
            let mut fx = Chorus::new(sample_rate);
            fx.set_param(CHORUS_RATE, param("rate_hz", 0.6));
            fx.set_param(CHORUS_DEPTH, param("depth", 0.5));
            Some(Box::new(MixedInsert::new(Box::new(fx), def.mix)))
        }
        InsertKind::Phaser => {
            let mut fx = Phaser::new(sample_rate);
            fx.set_param(PHASER_RATE, param("rate_hz", 0.4));
            fx.set_param(PHASER_DEPTH, param("depth", 0.6));
            fx.set_param(PHASER_FEEDBACK, param("feedback", 0.5));
            fx.set_param(PHASER_STAGES, param("stages", 0.0));
            Some(Box::new(MixedInsert::new(Box::new(fx), def.mix)))
        }
        InsertKind::FreqShifter => {
            let mut fx = FreqShifter::new(sample_rate);
            fx.set_param(SHIFT_HZ, param("shift_hz", 0.0));
            Some(Box::new(MixedInsert::new(Box::new(fx), def.mix)))
        }
        InsertKind::Transient => {
            let mut fx = TransientDesigner::new(sample_rate);
            fx.set_param(TRANSIENT_ATTACK, param("attack", 0.5));
            fx.set_param(TRANSIENT_SUSTAIN, param("sustain", 0.5));
            Some(Box::new(MixedInsert::new(Box::new(fx), def.mix)))
        }
        // Bus-class effects stay send-only (the graph hosts them once, not
        // per track): author them through `send_fx`, not inserts.
        InsertKind::Delay | InsertKind::Reverb | InsertKind::Compressor => None,
    }
}

/// Send-bus construction from the session's optional `send_fx` flavors.
/// Absent spec = the classic buses, byte-for-byte the pre-#30 graph.
fn delay_bus(spec: &Option<SendFxSpec>, sample_rate: u32) -> Box<dyn BusFx> {
    let mut delay = Delay::new(sample_rate);
    if matches!(spec.as_ref().and_then(|s| s.delay), Some(DelayFlavor::Tape)) {
        delay.set_param(TAPE_WOW, 0.4);
        delay.set_param(TAPE_FLUTTER, 0.4);
        delay.set_param(TAPE_SAT, 0.5);
    }
    Box::new(delay)
}

fn reverb_bus(spec: &Option<SendFxSpec>, sample_rate: u32) -> Box<dyn BusFx> {
    if matches!(spec.as_ref().and_then(|s| s.reverb), Some(ReverbFlavor::Fdn8)) {
        Box::new(ReverbV2::new(sample_rate))
    } else {
        Box::new(Reverb::new(sample_rate))
    }
}
