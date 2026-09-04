//! Fixed-topology mixer graph: 8 instrument tracks → 2 insert slots each →
//! equal-power pan → dry to mix bus + stereo sends → delay/reverb buses →
//! master. Sample-accurate event dispatch via [`EventCursor`], 64-frame
//! internal tiles. Each track passes through the hosted [`AutoMixer`]
//! (post-voice/insert, pre-pan): it owns the kick-sidechain duck (#76) and
//! the #27 gain staging — there is no other ducking path.
//!
//! RT rules honored by [`AudioGraph::render_block`]: no allocation, no locks,
//! no syscalls. Event list is prepared off-RT with [`AudioGraph::prepare_block`].

use kontinuum_mastering::{MasteringChain, MasteringTelemetry, MasteringTargets, OutputProfile};

use crate::mix::{AutoMixer, KillFade, KillTelemetry, MixRole, MUTE_FADE_MS, PANIC_FADE_MS};
use crate::params::*;
use crate::pool::VoicePool;
use crate::slice::SliceTable;
use crate::voice::{GrainConfig, ChokeState};
use crate::{InsertFx, Smoother, BusFx, Voice, BLOCK_FRAMES, MAX_TRACKS};
use kontinuum_schedule::{CompiledBlock, Event, EventCursor, ParamId, RampCurve, TrackId};

pub const MAX_EVENT_VOICES: usize = 32;
/// Stem bus of sustained harmonic content (pad-class default).
const PAD_STEM_INDEX: usize = 3;
const PARAM_SMOOTH_MS: f32 = 30.0;
/// Instrument-swap crossfade length in frames (~20 ms at 48 kHz, issue #37).
const SWAP_FADE_FRAMES: usize = 960;
/// PatchVoice pool capacity per custom track (mirrors the role pools).
const PATCH_POOL_CAPACITY: usize = 8;

/// What a swapped-in track strip plays (issue #37 `SwapInstrument`). Built on
/// the control thread, applied on the RT thread at a block boundary.
#[derive(Clone)]
pub enum TrackSwap {
    Patch(std::sync::Arc<kontinuum_ir::compile::CompiledPatch>),
    Factory(VoiceFactory),
    Silent,
}

impl std::fmt::Debug for TrackSwap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrackSwap::Patch(p) => f.debug_tuple("Patch").field(p).finish(),
            // VoiceFactory's closure is not Debug; the kind id identifies it.
            TrackSwap::Factory(vf) => {
                f.debug_struct("Factory").field("kind_id", &vf.kind_id).finish()
            }
            TrackSwap::Silent => f.write_str("Silent"),
        }
    }
}

/// Harness-side voice source (#51): everything the graph needs to build and
/// route a strip's voices, produced by a plugin (or a harness built-in like
/// the sampler). The graph never sees instrument code — just this.
#[derive(Clone)]
pub struct VoiceFactory {
    /// Stable id, == the IR `"kind"` discriminant ("kick", "sampler", …).
    pub kind_id: &'static str,
    pub capacity: usize,
    /// Stem-tap bus (kick 0, bass 1, perc 2, pad 3).
    pub stem: usize,
    pub role: MixRole,
    pub make: std::sync::Arc<dyn Fn(u32) -> Box<dyn Voice> + Send + Sync>,
}

impl VoiceFactory {
    pub fn build(&self, sample_rate: u32) -> Box<dyn Voice> {
        (self.make)(sample_rate)
    }
}

enum TrackVoices {
    None,
    Pool(VoicePool<Box<dyn Voice>>),
    /// Instrument swap in flight (issue #37): `outgoing` fades out while
    /// `incoming` fades in over [`SWAP_FADE_FRAMES`], then `incoming` remains.
    /// Events route to `incoming` immediately; both pools are preallocated so
    /// the fade itself allocates nothing.
    Crossfade {
        outgoing: VoicePool<Box<dyn Voice>>,
        incoming: Box<TrackVoices>,
        remaining: usize,
    },
}

impl TrackVoices {
    fn note_on(&mut self, pitch: f32, velocity: f32) -> usize {
        match self {
            TrackVoices::Pool(p) => p.note_on(pitch, velocity),
            TrackVoices::Crossfade { incoming, .. } => incoming.note_on(pitch, velocity),
            TrackVoices::None => 0,
        }
    }

    fn note_off(&mut self, slot: usize) {
        match self {
            TrackVoices::Pool(p) => p.note_off(slot),
            TrackVoices::Crossfade { incoming, .. } => incoming.note_off(slot),
            TrackVoices::None => {}
        }
    }

    fn render(&mut self, out: &mut [f32]) {
        match self {
            TrackVoices::Pool(p) => p.render(out),
            TrackVoices::Crossfade { outgoing, incoming, remaining } => {
                let n = out.len();
                let mut old_buf = [0.0f32; BLOCK_FRAMES];
                let mut new_buf = [0.0f32; BLOCK_FRAMES];
                outgoing.render(&mut old_buf[..n]);
                incoming.render(&mut new_buf[..n]);
                let start = SWAP_FADE_FRAMES - *remaining;
                for (k, slot) in out.iter_mut().enumerate() {
                    let done = (start + k) as f32 / SWAP_FADE_FRAMES as f32;
                    *slot = old_buf[k] * (1.0 - done) + new_buf[k] * done;
                }
                *remaining = remaining.saturating_sub(n);
                if *remaining == 0 {
                    let incoming = std::mem::replace(incoming.as_mut(), TrackVoices::None);
                    *self = incoming;
                }
            }
            TrackVoices::None => {}
        }
    }

    fn set_param(&mut self, param: ParamId, value: f32) {
        match self {
            TrackVoices::Pool(p) => p.set_param(param, value),
            TrackVoices::Crossfade { incoming, .. } => incoming.set_param(param, value),
            TrackVoices::None => {}
        }
    }

    fn active_count(&self) -> usize {
        match self {
            TrackVoices::Pool(p) => p.active_count(),
            TrackVoices::Crossfade { incoming, .. } => incoming.active_count(),
            TrackVoices::None => 0,
        }
    }

    fn trigger_sample(&mut self, slice: u16, rate_mul: f32) -> usize {
        match self {
            TrackVoices::Pool(p) => p.trigger_sample(60.0, 1.0, slice, rate_mul),
            TrackVoices::Crossfade { incoming, .. } => incoming.trigger_sample(slice, rate_mul),
            TrackVoices::None => 0,
        }
    }

    fn reset(&mut self) {
        match self {
            TrackVoices::Pool(p) => p.reset(),
            TrackVoices::Crossfade { outgoing, incoming, .. } => {
                outgoing.reset();
                incoming.reset();
            }
            TrackVoices::None => {}
        }
    }
}

struct TrackStrip {
    sr: f32,
    kind_id: &'static str,
    stem: usize,
    is_kick: bool,
    voices: TrackVoices,
    gain: Smoother,
    /// Kill mute (#38 step 5): multiplies the strip's gain path; a close
    /// fades to exact zero within [`MUTE_FADE_MS`], open is bit-exact
    /// passthrough.
    mute: KillFade,
    /// Solo gate (#14): the exact [`KillFade`] mirror of `mute` — closed on
    /// every strip while another track is soloed, open on the soloed strip
    /// (and everywhere when no solo is active). Separate from `mute`, so
    /// the two gates combine multiplicatively on the gain path.
    solo: KillFade,
    pan: Smoother,
    send_delay: Smoother,
    send_reverb: Smoother,
    params: Box<[Smoother]>,
    pushed: Box<[f32]>,
    inserts: [Option<Box<dyn InsertFx>>; 2],
    voice_map: [Option<u8>; MAX_EVENT_VOICES],
}

impl TrackStrip {
    fn new(sample_rate: f32) -> Self {
        let params: Vec<Smoother> = (0..PARAM_TABLE_LEN)
            .map(|_| Smoother::new(sample_rate, PARAM_SMOOTH_MS))
            .collect();
        let pushed = vec![0.0f32; PARAM_TABLE_LEN];
        let mut gain = Smoother::new(sample_rate, 20.0);
        gain.snap(1.0);
        let mute = KillFade::new(sample_rate, MUTE_FADE_MS);
        let solo = KillFade::new(sample_rate, MUTE_FADE_MS);
        let mut pan = Smoother::new(sample_rate, 20.0);
        pan.snap(0.5);
        let mut send_delay = Smoother::new(sample_rate, 30.0);
        send_delay.snap(0.0);
        let mut send_reverb = Smoother::new(sample_rate, 30.0);
        send_reverb.snap(0.0);
        TrackStrip {
            sr: sample_rate,
            kind_id: "silence",
            stem: PAD_STEM_INDEX,
            is_kick: false,
            voices: TrackVoices::None,
            gain,
            mute,
            solo,
            pan,
            send_delay,
            send_reverb,
            params: params.into_boxed_slice(),
            pushed: pushed.into_boxed_slice(),
            inserts: [None, None],
            voice_map: [None; MAX_EVENT_VOICES],
        }
    }

    fn attach_with(&mut self, factory: &VoiceFactory, sample_rate: u32) {
        self.is_kick = factory.role == MixRole::Kick;
        self.kind_id = factory.kind_id;
        self.stem = factory.stem;
        let capacity = factory.capacity;
        let make = std::sync::Arc::clone(&factory.make);
        self.voices = TrackVoices::Pool(VoicePool::new(capacity, move || make(sample_rate)));
    }

    fn apply_param(&mut self, param: ParamId, target: f32, duration_frames: u32, curve: RampCurve) {
        match param {
            ROUTE_TRACK_GAIN => self.gain.set_target(target.clamp(0.0, 2.0)),
            ROUTE_TRACK_PAN => self.pan.set_target(target.clamp(0.0, 1.0)),
            ROUTE_SEND_DELAY => self.send_delay.set_target(target.clamp(0.0, 2.0)),
            ROUTE_SEND_REVERB => self.send_reverb.set_target(target.clamp(0.0, 2.0)),
            _ => {
                let idx = param as usize;
                if idx >= PARAM_TABLE_LEN {
                    return;
                }
                let mut ms = (duration_frames as f32 / self.sr * 1000.0).clamp(5.0, 500.0);
                ms = if curve == RampCurve::Smooth { ms * 2.0 } else { ms };
                let current = self.params[idx].value();
                self.params[idx] = Smoother::new(self.sr, ms);
                self.params[idx].snap(current);
                self.params[idx].set_target(target);
            }
        }
    }

    fn push_params(&mut self) {
        for idx in 0..PARAM_TABLE_LEN {
            let value = self.params[idx].tick();
            if (value - self.pushed[idx]).abs() <= 1e-6 {
                continue;
            }
            self.pushed[idx] = value;
            let param = idx as ParamId;
            if idx < FX_PARAM_BASE as usize {
                self.voices.set_param(param, value);
            } else {
                for fx in self.inserts.iter_mut().flatten() {
                    fx.set_param(param, value);
                }
            }
        }
    }

    fn reset(&mut self) {
        self.voices.reset();
        self.gain.snap(1.0);
        self.mute.snap_open();
        self.solo.snap_open();
        self.pan.snap(0.5);
        self.send_delay.snap(0.0);
        self.send_reverb.snap(0.0);
        for s in self.params.iter_mut() {
            s.snap(0.0);
        }
        self.pushed.fill(0.0);
        for fx in self.inserts.iter_mut().flatten() {
            fx.reset();
        }
        self.voice_map = [None; MAX_EVENT_VOICES];
    }
}

/// RT-safe telemetry hook: called from the dispatch path with every routed
/// event. Implementations must be allocation-free (atomic stores only).
pub type EventHook = Box<dyn FnMut(u8, &Event) + Send>;

pub struct AudioGraph {
    sr: f32,
    tracks: Vec<TrackStrip>,
    mixer: AutoMixer,
    delay_bus: Option<Box<dyn BusFx>>,
    reverb_bus: Option<Box<dyn BusFx>>,
    /// Master panic gate (#38 step 5): applied to the summed output
    /// (tracks + sends) just before the mastering chain, so a panic
    /// silences everything including bus returns.
    panic_gate: KillFade,
    /// The #28 mastering chain, live in the render path (#82 — previously
    /// offline-only). Enabled by default; bypass is the bit-exact A/B
    /// reference and the kill-switch's first rung.
    mastering: MasteringChain,
    kill: KillTelemetry,
    /// Number of explicitly soloed tracks (#14): 0 = every solo gate open,
    /// ≥ 1 = every non-soloed strip's solo gate closed.
    solos: usize,
    event_hook: Option<EventHook>,
    /// Optional lock-free master tap (#25): post-master frames for a
    /// control-thread critic. Producer lives on the RT path (push-or-drop).
    master_tap: Option<MasterTapProducers>,
    /// Optional per-stem taps (#25): post-mixer track audio, one ring per
    /// stem bus (kick/bass/perc/pad).
    stem_taps: Option<StemTapProducers>,
    /// Shared choke-group state (issue #19): every sampler voice attached
    /// with a choke group joins this one instance, so tracks choke each
    /// other, not just pool siblings. Relaxed atomics — RT-safe.
    choke: std::sync::Arc<ChokeState>,
}

/// Attach-time tuning for sampler tracks (issue #19 v1). Everything here is
/// control-thread state baked into the voices at attach; `Default` is the
/// neutral configuration that renders exactly as before.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SampleTuning {
    /// Slot transpose in semitones (repitch; RT stays repitch-only).
    pub transpose_semitones: f32,
    /// Slot fine detune in cents.
    pub fine_cents: f32,
    /// Choke group id (1..=16, matching [`crate::voice::CHOKE_GROUPS`]);
    /// `None` = no choke. Voices on ANY track sharing a group choke each
    /// other on retrigger (909 hat logic).
    pub choke_group: Option<u8>,
}

impl SampleTuning {
    /// Tuning multiplier as a rate factor: 2^((semitones + cents/100)/12).
    pub fn rate_mul(&self) -> f32 {
        ((self.transpose_semitones + self.fine_cents / 100.0) / 12.0).exp2()
    }
}

struct StemTapProducers {
    rings: Vec<rtrb::Producer<f32>>,
    dropped: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

/// Control-thread consumer for the four stem buses.
pub struct StemTaps {
    consumers: Vec<rtrb::Consumer<f32>>,
    dropped: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl StemTaps {
    /// Drains one stem bus into `buf`. Index is the fixed stem order
    /// (kick 0, bass 1, perc 2, pad 3). Returns the frame count.
    pub fn drain(&mut self, stem: usize, buf: &mut Vec<f32>) -> usize {
        let Some(c) = self.consumers.get_mut(stem) else { return 0 };
        let n = c.slots();
        for _ in 0..n {
            match c.pop() {
                Ok(x) => buf.push(x),
                Err(_) => break,
            }
        }
        n
    }

    pub fn dropped_frames(&self) -> u64 {
        self.dropped.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// RT side of the master tap: producers + the shared overrun counter.
struct MasterTapProducers {
    left: rtrb::Producer<f32>,
    right: rtrb::Producer<f32>,
    dropped: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

/// Control-thread consumer of the master tap. Drain into buffers and hand
/// them to the critic (`kontinuum_analysis::CriticEngine::push_block_stereo`).
///
/// Drop policy: when the ring is full the RT side drops the remainder and
/// counts the frames — analysis degrades to a slightly stale window, the
/// audio path never waits.
pub struct MasterTap {
    left: rtrb::Consumer<f32>,
    right: rtrb::Consumer<f32>,
    dropped: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl MasterTap {
    /// Drains up to `min(left, right)` available frames into the buffers
    /// (planar, matching `push_block_stereo`). Returns the frame count.
    pub fn drain_stereo(&mut self, left: &mut Vec<f32>, right: &mut Vec<f32>) -> usize {
        let n = self.left.slots().min(self.right.slots());
        for _ in 0..n {
            // slots() guarantees the pop succeeds.
            if let (Ok(l), Ok(r)) = (self.left.pop(), self.right.pop()) {
                left.push(l);
                right.push(r);
            }
        }
        n
    }

    /// Frames the RT side dropped while the ring was full.
    pub fn dropped_frames(&self) -> u64 {
        self.dropped.load(std::sync::atomic::Ordering::Relaxed)
    }
}

use std::sync::Arc;

impl AudioGraph {
    pub fn new(sample_rate: u32) -> Self {
        AudioGraph {
            sr: sample_rate as f32,
            tracks: (0..MAX_TRACKS).map(|_| TrackStrip::new(sample_rate as f32)).collect(),
            mixer: AutoMixer::new(sample_rate),
            delay_bus: None,
            reverb_bus: None,
            panic_gate: KillFade::new(sample_rate as f32, PANIC_FADE_MS),
            mastering: MasteringChain::new_with_targets(sample_rate, &MasteringTargets::hypothesis()),
            kill: KillTelemetry::default(),
            solos: 0,
            event_hook: None,
            master_tap: None,
            stem_taps: None,
            choke: ChokeState::shared(),
        }
    }

    /// Subscribe to routed events (RT-safe closure: atomic stores only).
    pub fn set_event_hook(&mut self, hook: EventHook) {
        self.event_hook = Some(hook);
    }

    /// Attach a sampler track playing the given shared PCM buffer. Sample
    /// triggers / note-ons on this track play the buffer. With no slice
    /// table the whole buffer is one slice. Neutral tuning.
    pub fn attach_sampler(&mut self, track: u8, sample: Arc<[f32]>, sample_rate: u32) {
        let slices: SliceTable = Arc::from(Vec::new());
        self.attach_sampler_with_slices(track, sample, sample_rate, slices, SampleTuning::default());
    }

    /// [`AudioGraph::attach_sampler`] plus a [`SliceTable`]: sorted
    /// sample-frame offsets (first entry 0) from
    /// [`crate::slice::detect_slices`]. Sample triggers one-shot the
    /// requested region; an empty table is one full-buffer slice. `tuning`
    /// applies slot transpose/fine (repitch) and joins the track to a
    /// choke group.
    pub fn attach_sampler_with_slices(
        &mut self,
        track: u8,
        sample: Arc<[f32]>,
        sample_rate: u32,
        slices: SliceTable,
        tuning: SampleTuning,
    ) {
        use crate::voice::Sampler as SamplerV;
        if let Some(strip) = self.tracks.get_mut(track as usize) {
            strip.is_kick = false;
            strip.kind_id = "sampler";
            strip.stem = 3;
            let choke = Arc::clone(&self.choke);
            let group = tuning.choke_group;
            let pool = VoicePool::new(4, move || {
                let mut v = SamplerV::new(sample_rate);
                v.set_sample(Arc::clone(&sample), sample_rate);
                v.set_slices(Arc::clone(&slices));
                v.set_tune(tuning.transpose_semitones, tuning.fine_cents);
                if let Some(g) = group {
                    v.set_choke(Arc::clone(&choke), g);
                }
                Box::new(v) as Box<dyn Voice>
            });
            // Eagerly load: the factory already cloned the Arc per voice.
            strip.voices = TrackVoices::Pool(pool);
        }
        self.mixer.set_role(track, MixRole::Unassigned);
    }

    /// Attach a granular texture-bed track (issue #19): the same shared PCM
    /// played as a single-source grain cloud. Notes gate emission — note-on
    /// starts the cloud (pitch sets the grain rate, velocity the level),
    /// note-off lets the live grains finish. Control-thread only: allocates,
    /// like `attach_sampler`.
    pub fn attach_granular(
        &mut self,
        track: u8,
        sample: Arc<[f32]>,
        sample_rate: u32,
        config: GrainConfig,
    ) {
        use crate::voice::GranularVoice;
        if let Some(strip) = self.tracks.get_mut(track as usize) {
            strip.is_kick = false;
            strip.kind_id = "granular";
            strip.stem = 3;
            let pool = VoicePool::new(2, move || {
                let mut v = GranularVoice::new(sample_rate);
                v.set_source(Arc::clone(&sample), sample_rate);
                v.set_config(config.clone());
                Box::new(v) as Box<dyn Voice>
            });
            strip.voices = TrackVoices::Pool(pool);
        }
        self.mixer.set_role(track, MixRole::Unassigned);
    }

    pub fn sample_rate(&self) -> u32 {
        self.sr as u32
    }

    /// Attach a plugin-produced voice factory (#51). The factory carries
    /// everything: capacity, stem bus, mix role, and the voice constructor.
    pub fn attach_with(&mut self, track: u8, factory: &VoiceFactory) {
        if let Some(strip) = self.tracks.get_mut(track as usize) {
            strip.attach_with(factory, self.sr as u32);
        }
        self.mixer.set_role(track, factory.role);
    }

    /// Detach to a silent strip (#51: the empty registry plays silence).
    pub fn attach_silent(&mut self, track: u8) {
        if let Some(strip) = self.tracks.get_mut(track as usize) {
            strip.voices = TrackVoices::None;
            strip.kind_id = "silence";
            strip.stem = PAD_STEM_INDEX;
            strip.is_kick = false;
        }
        self.mixer.set_role(track, MixRole::Unassigned);
    }

    /// Swap a track's voice pool to PatchVoice evaluators of `patch` (issue
    /// #37; the #97 library presets ride it). The strip keeps the mix
    /// identity its `attach_track` kind established — only the sound source
    /// changes, crossfaded ([`AudioGraph::swap_track`]). Control-thread only:
    /// allocates, like `attach_sampler`.
    pub fn attach_patch(&mut self, track: u8, patch: &kontinuum_ir::compile::CompiledPatch) {
        self.swap_track(track, TrackSwap::Patch(std::sync::Arc::new(patch.clone())));
    }

    /// Swap a track's sound source to `swap`, crossfading over
    /// [`SWAP_FADE_FRAMES`] so in-flight notes fade instead of clicking.
    /// Used by live instrument swaps (issue #37 `SwapInstrument` diffs, #97
    /// preset loads). The Patch arm keeps the strip's existing mix identity;
    /// the Factory arm adopts the factory's, like `attach_with`. Safe while
    /// playing: like `attach_sampler`, the allocation happens on the thread
    /// that calls this and the fade keeps output continuous.
    pub fn swap_track(&mut self, track: u8, swap: TrackSwap) {
        let sample_rate = self.sr as u32;
        let (incoming, kind_id, stem, role) = match swap {
            TrackSwap::Patch(patch) => {
                let patch_for_pool = std::sync::Arc::clone(&patch);
                (
                    TrackVoices::Pool(VoicePool::new(PATCH_POOL_CAPACITY, move || {
                        Box::new(crate::voice::PatchVoice::new(sample_rate, &patch_for_pool))
                            as Box<dyn Voice>
                    })),
                    None,
                    None,
                    None,
                )
            }
            TrackSwap::Factory(factory) => {
                let make = std::sync::Arc::clone(&factory.make);
                let capacity = factory.capacity;
                (
                    TrackVoices::Pool(VoicePool::new(capacity, move || make(sample_rate))),
                    Some(factory.kind_id),
                    Some(factory.stem),
                    Some(factory.role),
                )
            }
            TrackSwap::Silent => (TrackVoices::None, None, None, None),
        };
        let Some(strip) = self.tracks.get_mut(track as usize) else { return };
        if let Some(kind_id) = kind_id {
            strip.kind_id = kind_id;
        }
        if let Some(stem) = stem {
            strip.stem = stem;
        }
        if let Some(role) = role {
            strip.is_kick = role == MixRole::Kick;
            self.mixer.set_role(track, role);
        }
        let old = std::mem::replace(&mut strip.voices, TrackVoices::None);
        strip.voices = match old {
            TrackVoices::Pool(pool) if pool.active_count() > 0 => TrackVoices::Crossfade {
                outgoing: pool,
                incoming: Box::new(incoming),
                remaining: SWAP_FADE_FRAMES,
            },
            // Nothing sounding (or already mid-fade): the swap is inaudible.
            _ => incoming,
        };
    }

    pub fn set_insert(&mut self, track: u8, slot: usize, fx: Box<dyn InsertFx>) {
        if let Some(strip) = self.tracks.get_mut(track as usize) {
            if slot < 2 {
                strip.inserts[slot] = Some(fx);
            }
        }
    }

    pub fn set_send_fx(&mut self, delay: Box<dyn BusFx>, reverb: Box<dyn BusFx>) {
        self.delay_bus = Some(delay);
        self.reverb_bus = Some(reverb);
    }

    pub fn set_track_gain(&mut self, track: u8, value: f32) {
        if let Some(strip) = self.tracks.get_mut(track as usize) {
            strip.gain.set_target(value.clamp(0.0, 2.0));
        }
    }

    pub fn snap_track_gain(&mut self, track: u8, value: f32) {
        if let Some(strip) = self.tracks.get_mut(track as usize) {
            strip.gain.snap(value.clamp(0.0, 2.0));
        }
    }

    /// `value` in 0..1 (0 = hard left, 0.5 = center, 1 = hard right).
    pub fn set_track_pan(&mut self, track: u8, value: f32) {
        if let Some(strip) = self.tracks.get_mut(track as usize) {
            strip.pan.set_target(value.clamp(0.0, 1.0));
        }
    }

    pub fn snap_track_pan(&mut self, track: u8, value: f32) {
        if let Some(strip) = self.tracks.get_mut(track as usize) {
            strip.pan.snap(value.clamp(0.0, 1.0));
        }
    }

    pub fn set_track_send(&mut self, track: u8, bus: usize, value: f32) {
        if let Some(strip) = self.tracks.get_mut(track as usize) {
            let target = value.clamp(0.0, 2.0);
            match bus {
                0 => strip.send_delay.set_target(target),
                _ => strip.send_reverb.set_target(target),
            }
        }
    }

    pub fn track_gain_value(&self, track: u8) -> f32 {
        self.tracks.get(track as usize).map(|s| s.gain.value()).unwrap_or(0.0)
    }

    /// Assign a track's mix role. Re-defaults the track's duck depth to the
    /// role's value; an explicit [`AudioGraph::set_track_duck_depth`] after
    /// this wins.
    pub fn set_track_role(&mut self, track: u8, role: MixRole) {
        self.mixer.set_role(track, role);
    }

    /// Per-track duck depth 0..1 (1 = duck to unity at full key). The
    /// per-track mix parameter seam for #76: the IR `Track.duck_depth`
    /// lands here through session setup (absent = the role's default).
    pub fn set_track_duck_depth(&mut self, track: u8, depth: f32) {
        self.mixer.set_duck_depth(track, depth);
    }

    pub fn track_duck_depth(&self, track: u8) -> f32 {
        self.mixer.duck_depth(track)
    }

    /// Duck release τ (ms) — the groove/genre template seam (#76). Core has
    /// no template plumbing, so hosts retime the pump from the template:
    /// the session's `duck_release_ms` lands here through session setup.
    pub fn set_duck_release_ms(&mut self, ms: f32) {
        self.mixer.set_duck_release_ms(ms);
    }

    /// The configured duck release τ (clamped), ms, for one track.
    pub fn track_duck_release_ms(&self, track: u8) -> f32 {
        self.mixer.duck_release_ms(track)
    }

    pub fn track_active_voices(&self, track: u8) -> usize {
        self.tracks.get(track as usize).map(|s| s.voices.active_count()).unwrap_or(0)
    }

    /// Mastering chain controls (#82). The chain sits at the end of the
    /// render path and starts enabled; bypass is bit-exact passthrough.
    /// All control-thread-safe by the same convention as the mute/solo
    /// setters (scalar stores on the chain's bounded-slew knobs).
    pub fn set_mastering_bypass(&mut self, bypassed: bool) {
        self.mastering.set_bypassed(bypassed);
    }

    /// Corrective tilt target, dB (positive brightens, hard-capped ±3).
    pub fn set_mastering_tilt(&mut self, tilt_db: f32) {
        self.mastering.set_tilt_target_db(tilt_db);
    }

    /// Section-aware see-through: 0 = full-intensity section, 1 = breakdown.
    pub fn set_mastering_section_energy(&mut self, energy: f32) {
        self.mastering.set_section_energy(energy);
    }

    /// Speaker-aware output profile (v0 approximation, issue #82).
    pub fn set_mastering_output_profile(&mut self, profile: OutputProfile) {
        self.mastering.set_output_profile(profile);
    }

    /// Snapshot of the mastering chain's working point after the last tile.
    pub fn mastering_telemetry(&self) -> MasteringTelemetry {
        self.mastering.telemetry()
    }

    /// Latched sustained-over-limit alarm — the kill-switch feed (#15).
    pub fn mastering_limiter_alarm(&self) -> bool {
        self.mastering.limiter_alarm()
    }

    /// Drops the chain's lookahead and envelope state (stop-path hygiene:
    /// stale limiter content must not leak into the next play).
    pub fn reset_mastering(&mut self) {
        self.mastering.reset();
    }

    /// Per-track kill mute (#38 step 5): closes the strip's [`KillFade`], an
    /// ≤ [`MUTE_FADE_MS`] linear ramp to exact zero on the track's gain path
    /// — click-free, allocation-free, re-openable (unmute ramps back up).
    /// Muting an already-muted track is a no-op and does not re-count.
    pub fn set_track_mute(&mut self, track: u8, muted: bool) {
        let Some(strip) = self.tracks.get_mut(track as usize) else { return };
        if muted {
            if strip.mute.is_open() {
                strip.mute.close();
                self.kill.mute_events += 1;
            }
        } else {
            strip.mute.open();
        }
    }

    /// Whether a mute close is currently requested on the track.
    pub fn track_muted(&self, track: u8) -> bool {
        self.tracks.get(track as usize).is_some_and(|s| s.mute.closing())
    }

    /// Per-track solo (#14): opens the track's solo [`KillFade`] and closes
    /// every other strip's with the same ≤ [`MUTE_FADE_MS`] ramp to exact
    /// zero mute uses — click-free, allocation-free, re-openable. The solo
    /// gate is separate from mute on the gain path, so un-muting a
    /// solo-silenced track stays silent until the solo clears. Soloing an
    /// already-soloed track is a no-op.
    pub fn set_track_solo(&mut self, track: u8, solo: bool) {
        let Some(strip) = self.tracks.get_mut(track as usize) else { return };
        let soloed = self.solos > 0 && !strip.solo.closing();
        if solo == soloed {
            return;
        }
        if solo {
            self.solos += 1;
            strip.solo.open();
            if self.solos == 1 {
                for (idx, other) in self.tracks.iter_mut().enumerate() {
                    if idx != track as usize {
                        other.solo.close();
                    }
                }
            }
        } else {
            self.solos -= 1;
            if self.solos == 0 {
                for other in self.tracks.iter_mut() {
                    other.solo.open();
                }
            } else {
                strip.solo.close();
            }
        }
    }

    /// Whether the track is explicitly soloed.
    pub fn track_solo(&self, track: u8) -> bool {
        self.solos > 0
            && self.tracks.get(track as usize).is_some_and(|s| !s.solo.closing())
    }

    /// Master panic (#38 step 5): everything — tracks and send returns —
    /// ramps to exact silence over [`PANIC_FADE_MS`]. The ramp is precomputed
    /// (integer frame counter), so the call is allocation-free and RT-safe.
    /// Panicking while already panicked is a no-op and does not re-count;
    /// re-arm with [`AudioGraph::rearm`].
    pub fn panic(&mut self) {
        if self.panic_gate.is_open() {
            self.panic_gate.close();
            self.kill.panic_events += 1;
        }
    }

    /// Re-arm after a panic: fades back to bit-exact unity over the same
    /// ramp length.
    pub fn rearm(&mut self) {
        self.panic_gate.open();
    }

    /// Whether a panic close is currently requested.
    pub fn is_panicked(&self) -> bool {
        self.panic_gate.closing()
    }

    /// Kill-switch event counters for the #15 watchdog feed.
    pub fn kill_telemetry(&self) -> KillTelemetry {
        self.kill
    }

    /// Off-RT: merge block events into the sorted slice render_block consumes.
    pub fn prepare_block(block: &CompiledBlock) -> Vec<(u32, TrackId, Event)> {
        block.merged_events()
    }

    pub fn apply_event(&mut self, track: u8, event: Event) {
        if let Some(hook) = self.event_hook.as_mut() {
            hook(track, &event);
        }
        let Some(strip) = self.tracks.get_mut(track as usize) else { return };
        match event {
            Event::NoteOn { voice, pitch, velocity, .. } => {
                let slot = strip.voices.note_on(pitch, velocity);
                if (voice as usize) < MAX_EVENT_VOICES {
                    strip.voice_map[voice as usize] = Some(slot as u8);
                }
                if strip.is_kick {
                    self.mixer.kick(velocity);
                }
            }
            Event::NoteOff { voice } => {
                if (voice as usize) < MAX_EVENT_VOICES {
                    if let Some(slot) = strip.voice_map[voice as usize].take() {
                        strip.voices.note_off(slot as usize);
                    }
                }
            }
            Event::ParamRamp { param, target, duration_frames, curve } => {
                strip.apply_param(param, target, duration_frames, curve);
            }
            Event::SampleTrigger { sample_id, slice, rate } => {
                let slot = strip.voices.trigger_sample(slice, rate);
                if (sample_id as usize) < MAX_EVENT_VOICES {
                    strip.voice_map[sample_id as usize] = Some(slot as u8);
                }
            }
        }
    }

    /// Render one callback buffer, dispatching `events` (sorted, block-relative
    /// frames) sample-accurately. `buf_start` is the absolute frame of
    /// `out_l[0]`; pass `block.start_frame` when rendering a whole block.
    /// Attaches the lock-free master tap (#25), replacing any previous one
    /// (the old consumer keeps draining whatever is still buffered).
    /// `capacity_frames` bounds the stale-data window; 1 s @ 48 kHz is a
    /// sensible default. Output audio is unaffected — the tap only reads.
    pub fn attach_master_tap(&mut self, capacity_frames: usize) -> MasterTap {
        let (pl, cl) = rtrb::RingBuffer::new(capacity_frames.max(BLOCK_FRAMES));
        let (pr, cr) = rtrb::RingBuffer::new(capacity_frames.max(BLOCK_FRAMES));
        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        self.master_tap = Some(MasterTapProducers { left: pl, right: pr, dropped: std::sync::Arc::clone(&dropped) });
        MasterTap { left: cl, right: cr, dropped }
    }

    /// Attaches per-stem taps (#25): track audio post-voice/insert and post
    /// auto-mix, pre-gain, one ring per stem bus. Replaces any previous tap.
    pub fn attach_stem_taps(&mut self, capacity_frames: usize) -> StemTaps {
        let cap = capacity_frames.max(BLOCK_FRAMES);
        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let mut producers = Vec::new();
        let mut consumers = Vec::new();
        for _ in 0..4 {
            let (p, c) = rtrb::RingBuffer::new(cap);
            producers.push(p);
            consumers.push(c);
        }
        self.stem_taps = Some(StemTapProducers { rings: producers, dropped: std::sync::Arc::clone(&dropped) });
        StemTaps { consumers, dropped }
    }

    pub fn render_block(
        &mut self,
        out_l: &mut [f32],
        out_r: &mut [f32],
        events: &[(u32, TrackId, Event)],
        buf_start: u64,
    ) {
        let len = out_l.len().min(out_r.len());
        let mut cursor = EventCursor::new(events, buf_start, len);
        while let Some(span) = cursor.next_span() {
            let end = span.offset + span.len;
            let mut off = span.offset;
            while off < end {
                let n = (end - off).min(BLOCK_FRAMES);
                self.render_tile(&mut out_l[off..off + n], &mut out_r[off..off + n]);
                off += n;
            }
            for (_, track, event) in span.events {
                self.apply_event(*track, *event);
            }
        }
    }

    fn render_tile(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        let n = out_l.len();
        let mut bus1_l = [0.0f32; BLOCK_FRAMES];
        let mut bus1_r = [0.0f32; BLOCK_FRAMES];
        let mut bus2_l = [0.0f32; BLOCK_FRAMES];
        let mut bus2_r = [0.0f32; BLOCK_FRAMES];
        let mut voice_buf = [0.0f32; BLOCK_FRAMES];

        for (idx, strip) in self.tracks.iter_mut().enumerate() {
            voice_buf[..n].fill(0.0);
            strip.voices.render(&mut voice_buf[..n]);
            for fx in strip.inserts.iter_mut().flatten() {
                fx.render(&mut voice_buf[..n]);
            }
            // The AutoMixer owns ducking + gain staging; the duck's key was
            // raised at kick-event dispatch time.
            self.mixer.process_track(idx as u8, &mut voice_buf[..n]);
            if let Some(taps) = self.stem_taps.as_mut() {
                let stem = strip.stem;
                if let Some(ring) = taps.rings.get_mut(stem) {
                    let pushed = ring.slots().min(n);
                    for &s in &voice_buf[..pushed] {
                        let _ = ring.push(s);
                    }
                    if pushed < n {
                        taps.dropped.fetch_add((n - pushed) as u64, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
            for i in 0..n {
                let g = strip.gain.tick() * strip.mute.tick() * strip.solo.tick();
                let p = strip.pan.tick();
                let lg = g * (std::f32::consts::FRAC_PI_2 * p).cos();
                let rg = g * (std::f32::consts::FRAC_PI_2 * p).sin();
                let v = voice_buf[i];
                out_l[i] += v * lg;
                out_r[i] += v * rg;
                // Sends tap POST-mixer but PRE-mute/pre-solo, deliberately:
                // fx tails ring out after a mute or solo (console behavior),
                // and keeping the bus input independent of mute/solo state is
                // what lets the deterministic render contract hold bit-exactly
                // across solo toggles (the facade solo test pins this).
                let sd = strip.send_delay.tick();
                bus1_l[i] += v * sd;
                bus1_r[i] += v * strip.send_delay.value();
                let sr = strip.send_reverb.tick();
                bus2_l[i] += v * sr;
                bus2_r[i] += v * strip.send_reverb.value();
            }
            strip.push_params();
        }

        if let Some(d) = self.delay_bus.as_mut() {
            d.render(&mut bus1_l[..n], &mut bus1_r[..n]);
        }
        for i in 0..n {
            out_l[i] += bus1_l[i];
            out_r[i] += bus1_r[i];
        }
        if let Some(rv) = self.reverb_bus.as_mut() {
            rv.render(&mut bus2_l[..n], &mut bus2_r[..n]);
        }
        for i in 0..n {
            out_l[i] += bus2_l[i];
            out_r[i] += bus2_r[i];
        }
        if !self.panic_gate.is_open() {
            for i in 0..n {
                let g = self.panic_gate.tick();
                out_l[i] *= g;
                out_r[i] *= g;
            }
        }
        self.mastering.render(out_l, out_r);
        if let Some(tap) = self.master_tap.as_mut() {
            // Push-or-drop: slots() bounds the copies, so pushes cannot fail.
            let pushed = tap.left.slots().min(tap.right.slots()).min(n);
            for i in 0..pushed {
                let _ = tap.left.push(out_l[i]);
                let _ = tap.right.push(out_r[i]);
            }
            if pushed < n {
                tap.dropped.fetch_add((n - pushed) as u64, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    pub fn reset(&mut self) {
        for strip in self.tracks.iter_mut() {
            strip.reset();
        }
        self.solos = 0;
        self.mixer.reset();
        if let Some(d) = self.delay_bus.as_mut() {
            d.reset();
        }
        if let Some(rv) = self.reverb_bus.as_mut() {
            rv.reset();
        }
        self.panic_gate.snap_open();
        self.mastering.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kontinuum_ir::patch::CustomPatch;

    /// A one-node patch plan: a raw sine straight to out (pitch rides the
    /// note, as everywhere in the patch IR).
    fn drone_plan() -> std::sync::Arc<kontinuum_ir::compile::CompiledPatch> {
        let patch: CustomPatch = serde_json::from_str(
            r#"{"kind": "custom", "patch": {
                "nodes": [
                    {"id": "o", "type": "osc", "wave": "sine", "level": 0.8},
                    {"id": "x", "type": "out"}],
                "edges": [{"from": "o", "to": "x", "type": "audio"}]}}"#,
        )
        .expect("patch json");
        std::sync::Arc::new(kontinuum_ir::compile::compile_patch(&patch).expect("compile"))
    }

    /// A live `SwapInstrument` must be click-free (issue #37): the outgoing
    /// pool crossfades to the incoming one over ~20 ms instead of being cut.
    #[test]
    fn swap_track_crossfades_instead_of_cutting() {
        let mut g = AudioGraph::new(48_000);
        g.set_mastering_bypass(true);
        g.attach_patch(0, &drone_plan());
        g.snap_track_gain(0, 1.0);
        let events = note_on_at(0, 0, 1);
        let (pre, _) = render_span_test(&mut g, &events, 0, 4 * BLOCK_FRAMES);
        let pre_peak = pre.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(pre_peak > 0.05, "drone must be sounding before the swap");

        // Swap to a second drone while the first is mid-note.
        let second = drone_plan();
        g.swap_track(0, TrackSwap::Patch(second));
        let (post, _) = render_span_test(&mut g, &[], 4 * BLOCK_FRAMES as u64, 30 * BLOCK_FRAMES);
        assert!(post.iter().all(|s| s.is_finite()));
        // No cut: the first post-swap tile is still loud (fade just started),
        // and nothing exceeds the pre-swap level by a click-sized spike.
        let first_tile_peak =
            post[..BLOCK_FRAMES].iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(
            first_tile_peak > pre_peak * 0.5,
            "swap cut the sound: first tile peak {first_tile_peak} vs pre {pre_peak}"
        );
        assert!(
            post.iter().fold(0.0f32, |m, &s| m.max(s.abs())) < 1.0,
            "crossfade produced an oversized spike"
        );
        // Past the fade window the outgoing pool is gone: with no note on the
        // incoming pool the strip is exactly silent — a hard cut would have
        // ended up here immediately, a broken fade would never reach it.
        let last_tile = &post[post.len() - BLOCK_FRAMES..];
        assert!(last_tile.iter().all(|&s| s == 0.0), "outgoing pool never faded out");

        // The incoming pool takes live notes after the swap.
        let base = 4 * BLOCK_FRAMES as u32;
        let events = note_on_at(0, base + 1, base + 2);
        let (revived, _) =
            render_span_test(&mut g, &events, u64::from(base), 4 * BLOCK_FRAMES);
        assert!(
            revived.iter().fold(0.0f32, |m, &s| m.max(s.abs())) > 0.05,
            "incoming pool must accept notes after the swap"
        );
    }

    fn render_span_test(
        g: &mut AudioGraph,
        events: &[(u32, TrackId, Event)],
        start: u64,
        frames: usize,
    ) -> (Vec<f32>, f32) {
        let mut l = vec![0.0f32; frames];
        let mut r = vec![0.0f32; frames];
        g.render_block(&mut l, &mut r, events, start);
        let mono = l.iter().zip(r.iter()).map(|(l, r)| 0.5 * (l + r)).collect();
        (mono, 0.0)
    }

    fn sample() -> Arc<[f32]> {
        // Linear ramp: content varies with playback rate, so repitch is
        // visible in the rendered stream (a constant buffer is not).
        (0..48_000).map(|i| i as f32 / 48_000.0).collect::<Vec<_>>().into()
    }

    fn short_sample() -> Arc<[f32]> {
        vec![0.5f32; 640].into()
    }

    /// Note-on for `track` with a span closer (span dispatch applies an
    /// event at the END of the span it opens, so each event needs a later
    /// event to bound its span; voice 99 is a throwaway closer slot).
    fn note_on_at(track: u8, frame: u32, closer: u32) -> Vec<(u32, TrackId, Event)> {
        vec![
            (frame, track, Event::NoteOn { voice: 0, pitch: 60.0, velocity: 1.0, microtiming_ticks: 0 }),
            (closer, track, Event::NoteOff { voice: 99 }),
        ]
    }

    /// Render `frames` of one gated sampler note, bypassing the mastering
    /// chain so assertions see the raw voice path.
    fn sampler_note_span(tuning: SampleTuning, frames: usize) -> Vec<f32> {
        let mut g = AudioGraph::new(48_000);
        g.set_mastering_bypass(true);
        g.attach_sampler_with_slices(0, sample(), 48_000, Arc::from(Vec::new()), tuning);
        g.snap_track_gain(0, 1.0);
        let events = note_on_at(0, 0, 640);
        render_span_test(&mut g, &events, 0, frames).0
    }

    /// Slot transpose must measurably change the rendered content (issue #19
    /// pitch): +12 semitones plays the same buffer at double rate, so the
    /// streams diverge frame-by-frame despite identical triggers.
    #[test]
    fn slot_transpose_changes_playback_content() {
        let neutral = sampler_note_span(SampleTuning::default(), 4_800);
        let up = sampler_note_span(
            SampleTuning { transpose_semitones: 12.0, ..SampleTuning::default() },
            4_800,
        );
        assert!(neutral.iter().any(|s| *s != 0.0), "neutral note silent");
        assert_ne!(neutral, up, "+12 semitones changed nothing");
        // Double rate gets through ~2x the buffer: the tail frame differs too.
        assert!(up.iter().any(|s| *s != 0.0));
    }

    /// A choke group joins tracks, not just pool siblings (issue #19): a
    /// trigger on track 1 in the same group fades a sounding trigger on
    /// track 0 to exact zero within 10 ms; a trigger in another group
    /// leaves it alone. SampleTrigger one-shots (no gate loop), so tails
    /// prove the choke rather than a looping gate.
    #[test]
    fn choke_group_spans_tracks_via_shared_state() {
        let trigger_at = |track: u8, frame: u32, closer: u32| {
            vec![
                (frame, track, Event::SampleTrigger { sample_id: 0, slice: 0, rate: 1.0 }),
                (closer, track, Event::NoteOff { voice: 99 }),
            ]
        };
        let build = |group0: Option<u8>, group1: Option<u8>, short: bool| {
            let mut g = AudioGraph::new(48_000);
            g.set_mastering_bypass(true);
            let t1_sample = if short { short_sample() } else { sample() };
            g.attach_sampler_with_slices(
                0,
                sample(),
                48_000,
                Arc::from(Vec::new()),
                SampleTuning { choke_group: group0, ..SampleTuning::default() },
            );
            g.attach_sampler_with_slices(
                1,
                t1_sample,
                48_000,
                Arc::from(Vec::new()),
                SampleTuning { choke_group: group1, ..SampleTuning::default() },
            );
            g.snap_track_gain(0, 1.0);
            g.snap_track_gain(1, 1.0);
            g
        };

        // Same group: track 1's trigger chokes track 0's. Track 1 plays a
        // short buffer so it ends on its own; a silent tail past both its
        // natural end and the 480-frame choke fade proves track 0 (whose
        // 48k one-shot would still be sounding) was choked.
        let mut g = build(Some(1), Some(1), true);
        let mut events = trigger_at(0, 0, 640);
        events.extend(trigger_at(1, 640, 1280));
        let (mono, _) = render_span_test(&mut g, &events, 0, 9_600);
        assert!(
            mono[1_400..1_700].iter().any(|s| *s != 0.0),
            "track 1 must sound before its one-shot ends"
        );
        let settled = 1280 + 640 + 480 + 64;
        let tail = &mono[settled..9_600];
        assert!(
            tail.iter().all(|s| *s == 0.0),
            "choked note kept sounding: {:?}",
            &tail[..tail.len().min(8)]
        );

        // Different groups: the first trigger survives untouched.
        let mut g = build(Some(2), Some(3), true);
        let mut events = trigger_at(0, 0, 640);
        events.extend(trigger_at(1, 640, 1280));
        let (mono, _) = render_span_test(&mut g, &events, 0, 9_600);
        assert!(
            mono[4_000..5_000].iter().any(|s| *s != 0.0),
            "unrelated group must not choke"
        );
    }

    /// The granular attach plays a real grain cloud through the graph and
    /// is deterministic run-to-run (issue #19 granular, RT half).
    #[test]
    fn granular_track_renders_deterministic_cloud() {
        let run = || {
            let mut g = AudioGraph::new(48_000);
            g.set_mastering_bypass(true);
            g.attach_granular(0, sample(), 48_000, crate::voice::GrainConfig::default());
            g.snap_track_gain(0, 1.0);
            let mut events = note_on_at(0, 0, 38_400);
            render_span_test(&mut g, &events, 0, 48_000).0
        };
        let a = run();
        let b = run();
        assert!(a.iter().any(|s| *s != 0.0), "granular track silent");
        assert!(
            a.iter().zip(b.iter()).all(|(x, y)| x.to_bits() == y.to_bits()),
            "granular render not deterministic"
        );
        assert!(a.iter().all(|s| s.is_finite()));
    }
}
