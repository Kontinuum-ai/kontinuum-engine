//! Engine half of issue #37: evaluates a compiled [`CompiledPatch`] as a
//! [`Voice`] — the audio path that makes `InstrumentDef::Custom` audible.
//!
//! Execution model (per the compile plan contract, `kontinuum_ir::compile::patch`):
//! - Nodes run in the plan's topological order into per-node scratch buffers
//!   (`BLOCK_FRAMES` frames, preallocated at construction — `render` never
//!   allocates). Audio inputs are implicit sums of incoming edges.
//! - The ONLY feedback-capable node is `delay`. Feedback edges apply their
//!   signal one tile late, reading the delay's wet tap history (`taps`), so
//!   loops close through preallocated ring buffers exactly as the plan's
//!   cycle rules require; the delay itself recirculates at its `feedback`
//!   (clamped ≤ 0.95, so every loop decays by construction).
//! - Mod edges are CONTROL-RATE per the IR signal model: env/LFO state
//!   advances once per tile and the depth-scaled value (`param_eff = base +
//!   amount·v`, multiplicative octaves for `cutoff_hz`/`time_ms`) is applied
//!   to the target for the whole tile (~750 Hz at 48 kHz). Per-sample mods
//!   would cost `n_nodes×` more env/LFO evaluation for no audible gain at
//!   LFO rates ≤ 40 Hz; the tradeoff is recorded here for the CPU table.
//! - Guards: every node output is scanned per tile; a non-finite sample
//!   latches the node silenced (state sanitized, zeros out) so one bad node
//!   can never poison the patch output. Denormals flush at the delay ring
//!   write, env release, FM feedback, and node input sums.
//!
//! Per-node CPU (48 kHz, rough per-sample cost, feeds the cost-table follow-up):
//! - `osc` saw/square: polyBLEP, ~8 flops + 2 branches per unison voice
//!   (square = two corrected edges ≈ 2× a saw); sine = 1 sin; tri = 3 flops
//!   (naive, mild HF aliasing); noise = 3 int ops (xorshift).
//! - `fm_pair`: 2 sin + 4 muladds (cheapest metallic voice class).
//! - `filter`: TPT SVF ~10 flops/sample + 1 tan per TILE when cutoff is
//!   modulated (coeff update at control rate).
//! - `env`/`lfo`: 1 exp or 1 sin per TILE (control rate, ~1/64 of a
//!   per-sample voice's mod cost).
//! - `gain`/`out`: 1 mul/sample.
//! - `delay`: ring read lerp + recirc write, ~8 flops/sample; buffer =
//!   `time_ms·sr` frames (≤ 2 s ≈ 384 KB at 48 kHz) — L2-resident, allocated
//!   once from the declared time, modulated time interpolates in place.
//!
//! Vocabulary notes vs the issue's ~20-type wish list: `sampler` needs the
//! host to install PCM in the voice's [`SampleBank`] (a missing slot mutes
//! that node, never the patch — bridge hot-load wiring is a follow-up);
//! `mix` is the `gain` node (audio inputs already sum); LFO tempo sync is an
//! authoring-time bpm→rate mapping; the kick-duck key on the rumble patch is
//! the host `AutoMixer` `MixRole` concern, not a patch-level node.
//!
//! allow: SIZE_OK — one engine arm per IR node kind plus the five golden
//! fixtures; the shape is pinned by the issue #37 IR contract, and the node
//! match must stay readable in one place.

use std::collections::HashMap;
use std::sync::Arc;

use kontinuum_ir::compile::{compile_patch, CompiledPatch};
use kontinuum_ir::patch::{
    CustomPatch, DelayNode, EdgeKind, EnvNode, FilterMode as PatchFilterMode, FilterNode,
    FormantNode, FormantVowel, FmPairNode, LfoNode, LfoWave, OscNode, OscWave, OutNode, PatchNode,
    RingNode, RING_CARRIER_SOCKET, SamplerNode, ShaperNode,
};
use kontinuum_ir::schema::bounds;

use super::{flush_denormal, midi_to_hz, poly_blep, NoiseGen};
use crate::fx::filter::{FilterMode, Svf};
use crate::{ParamId, Voice, BLOCK_FRAMES, SILENCE_ABS};
use std::f32::consts::TAU;

const MAX_UNISON: usize = 7;

/// PCM the host installs for a patch's `sampler` nodes, keyed by the #19 slot
/// id the IR node references. Empty bank = sampler nodes stay muted.
pub type SampleBank = HashMap<u32, Arc<[f32]>>;

/// Mod-able parameter slots (one per node kind, per `PatchNode::mod_targets`).
#[derive(Clone, Copy, Debug, PartialEq)]
enum ModParam {
    Cutoff,
    Level,
    Cents,
    Index,
    Time,
    Drive,
    Shift,
}

/// Engine state for one patch node. Params are clamped to the IR bounds at
/// construction; `*_eff` fields carry the per-tile modulated value.
enum NodeEngine {
    Osc(OscState),
    FmPair(FmState),
    Filter(FilterState),
    Env(EnvState),
    Lfo(LfoState),
    Gain(GainState),
    Delay(DelayState),
    Ring(RingState),
    Shaper(ShaperState),
    Formant(FormantState),
    Sampler(SamplerState),
    Out(OutState),
}

struct OscState {
    wave: OscWave,
    unison: usize,
    fine_cents: f32,
    cents_eff: f32,
    level: f32,
    phases: [f32; MAX_UNISON],
    noise: NoiseGen,
}

struct FmState {
    ratio: f32,
    index_eff: f32,
    index: f32,
    feedback: f32,
    level: f32,
    c_phase: f32,
    m_phase: f32,
    prev: f32,
}

struct FilterState {
    mode: FilterMode,
    drive: f32,
    svf: Svf,
    cutoff_hz: f32,
}

struct EnvState {
    attack_ms: f32,
    decay_ms: f32,
    sustain: f32,
    release_ms: f32,
    phase_ms: f32,
    rel_ms: f32,
    level: f32,
    idle: bool,
}

struct LfoState {
    rate_hz: f32,
    depth: f32,
    wave: LfoWave,
    phase: f32,
}

struct GainState {
    base: f32,
    gain: f32,
}

struct DelayState {
    time_ms: f32,
    /// Unmodulated ring length in frames (also the modulation ceiling).
    base_frames: f32,
    time_frames: f32,
    feedback: f32,
    mix: f32,
    ring: Box<[f32]>,
    write: usize,
    /// Wet tap of the previous tile — what feedback edges read (one tile late).
    taps: [f32; BLOCK_FRAMES],
}

struct RingState {
    base: f32,
    level: f32,
}

struct ShaperState {
    drive: f32,
    drive_eff: f32,
    level: f32,
}

struct FormantState {
    /// One SVF per formant (F1/F2/F3), all band-pass, summed.
    svfs: [Svf; 3],
    vowel: FormantVowel,
    shift: f32,
    shift_eff: f32,
    level: f32,
}

struct SamplerState {
    level: f32,
    data: Option<Arc<[f32]>>,
    pos: f32,
    playing: bool,
}

struct OutState {
    level: f32,
}

impl NodeEngine {
    fn build(node: &PatchNode, sample_rate: u32, bank: &SampleBank) -> Self {
        match node {
            PatchNode::Osc(OscNode { wave, unison, fine_cents, level, .. }) => {
                let mut noise_state = 0u32;
                for b in node.id().bytes() {
                    noise_state = noise_state.wrapping_mul(31).wrapping_add(b as u32);
                }
                NodeEngine::Osc(OscState {
                    wave: *wave,
                    unison: (*unison).clamp(bounds::UNISON.0, bounds::UNISON.1) as usize,
                    fine_cents: fine_cents.clamp(bounds::PAD_DETUNE_CENTS.0, bounds::PAD_DETUNE_CENTS.1),
                    cents_eff: fine_cents.clamp(bounds::PAD_DETUNE_CENTS.0, bounds::PAD_DETUNE_CENTS.1),
                    level: level.clamp(bounds::UNIT.0, bounds::UNIT.1),
                    phases: [0.0; MAX_UNISON],
                    noise: NoiseGen::seeded_at(noise_state | 1),
                })
            }
            PatchNode::FmPair(FmPairNode { ratio, index, feedback, level, .. }) => NodeEngine::FmPair(FmState {
                ratio: ratio.clamp(bounds::FM_RATIO.0, bounds::FM_RATIO.1),
                index: index.clamp(bounds::FM_INDEX.0, bounds::FM_INDEX.1),
                index_eff: index.clamp(bounds::FM_INDEX.0, bounds::FM_INDEX.1),
                feedback: feedback.clamp(bounds::UNIT.0, bounds::UNIT.1),
                level: level.clamp(bounds::UNIT.0, bounds::UNIT.1),
                c_phase: 0.0,
                m_phase: 0.0,
                prev: 0.0,
            }),
            PatchNode::Filter(FilterNode { mode, cutoff_hz, resonance, drive, .. }) => {
                let cutoff = cutoff_hz.clamp(bounds::PATCH_CUTOFF_HZ.0, bounds::PATCH_CUTOFF_HZ.1);
                NodeEngine::Filter(FilterState {
                    mode: match mode {
                        PatchFilterMode::LowPass => FilterMode::LowPass,
                        PatchFilterMode::HighPass => FilterMode::HighPass,
                        PatchFilterMode::BandPass => FilterMode::BandPass,
                    },
                    drive: drive.clamp(bounds::UNIT.0, bounds::UNIT.1),
                    svf: Svf::new(sample_rate, cutoff, resonance.clamp(bounds::UNIT.0, bounds::UNIT.1)),
                    cutoff_hz: cutoff,
                })
            }
            PatchNode::Env(EnvNode { attack_ms, decay_ms, sustain, release_ms, .. }) => NodeEngine::Env(EnvState {
                attack_ms: attack_ms.clamp(0.5, bounds::PAD_ATTACK_MS.1),
                decay_ms: decay_ms.clamp(bounds::ENV_DECAY_MS.0, bounds::ENV_DECAY_MS.1),
                sustain: sustain.clamp(bounds::UNIT.0, bounds::UNIT.1),
                release_ms: release_ms.clamp(bounds::PAD_RELEASE_MS.0, bounds::PAD_RELEASE_MS.1),
                phase_ms: 0.0,
                rel_ms: 0.0,
                level: 0.0,
                idle: true,
            }),
            PatchNode::Lfo(LfoNode { rate_hz, depth, wave, .. }) => NodeEngine::Lfo(LfoState {
                rate_hz: rate_hz.clamp(bounds::LFO_RATE_HZ.0, bounds::LFO_RATE_HZ.1),
                depth: depth.clamp(bounds::UNIT.0, bounds::UNIT.1),
                wave: *wave,
                phase: 0.0,
            }),
            PatchNode::Delay(DelayNode { time_ms, feedback, mix, .. }) => {
                let time = time_ms.clamp(bounds::DELAY_TIME_MS.0, bounds::DELAY_TIME_MS.1);
                let frames = (time * sample_rate as f32 / 1000.0).ceil().max(1.0);
                NodeEngine::Delay(DelayState {
                    time_ms: time,
                    base_frames: frames,
                    time_frames: frames,
                    feedback: feedback.clamp(bounds::DELAY_FEEDBACK.0, bounds::DELAY_FEEDBACK.1),
                    mix: mix.clamp(bounds::UNIT.0, bounds::UNIT.1),
                    ring: vec![0.0; frames as usize].into_boxed_slice(),
                    write: 0,
                    taps: [0.0; BLOCK_FRAMES],
                })
            }
            PatchNode::Gain(level) => {
                let l = level.level.clamp(bounds::GAIN.0, bounds::GAIN.1);
                NodeEngine::Gain(GainState { base: l, gain: l })
            }
            PatchNode::Ring(RingNode { level, .. }) => {
                let l = level.clamp(bounds::UNIT.0, bounds::UNIT.1);
                NodeEngine::Ring(RingState { base: l, level: l })
            }
            PatchNode::Shaper(ShaperNode { drive, level, .. }) => {
                let d = drive.clamp(bounds::UNIT.0, bounds::UNIT.1);
                NodeEngine::Shaper(ShaperState {
                    drive: d,
                    drive_eff: d,
                    level: level.clamp(bounds::UNIT.0, bounds::UNIT.1),
                })
            }
            PatchNode::Formant(FormantNode { vowel, shift, level, .. }) => {
                let shift = shift.clamp(bounds::FORMANT_SHIFT.0, bounds::FORMANT_SHIFT.1);
                let formants = vowel.formants();
                let svfs = formants.map(|f| Svf::new(sample_rate, f * shift, 0.6));
                NodeEngine::Formant(FormantState {
                    svfs,
                    vowel: *vowel,
                    shift,
                    shift_eff: shift,
                    level: level.clamp(bounds::UNIT.0, bounds::UNIT.1),
                })
            }
            PatchNode::Sampler(SamplerNode { slot, level, .. }) => {
                NodeEngine::Sampler(SamplerState {
                    level: level.clamp(bounds::UNIT.0, bounds::UNIT.1),
                    data: bank.get(slot).cloned(),
                    pos: 0.0,
                    playing: false,
                })
            }
            PatchNode::Out(OutNode { level, .. }) => {
                NodeEngine::Out(OutState { level: level.clamp(bounds::UNIT.0, bounds::UNIT.1) })
            }
        }
    }

    /// Per-tile control-rate source value (env/LFO only); everything else
    /// contributes 0 to the mod matrix.
    fn tick_source(&mut self, ms: f32, gate: bool) -> f32 {
        match self {
            NodeEngine::Env(e) => e.tick(ms, gate),
            NodeEngine::Lfo(l) => l.tick(ms),
            _ => 0.0,
        }
    }

    fn reset_eff(&mut self) {
        match self {
            NodeEngine::Osc(o) => o.cents_eff = o.fine_cents,
            NodeEngine::FmPair(f) => f.index_eff = f.index,
            NodeEngine::Filter(f) => f.svf.set_cutoff(f.cutoff_hz),
            NodeEngine::Gain(g) => g.gain = g.base,
            NodeEngine::Ring(r) => r.level = r.base,
            NodeEngine::Delay(d) => d.time_frames = d.base_frames,
            NodeEngine::Shaper(s) => s.drive_eff = s.drive,
            NodeEngine::Formant(f) => f.shift_eff = f.shift,
            NodeEngine::Env(_) | NodeEngine::Lfo(_) | NodeEngine::Sampler(_) | NodeEngine::Out(_) => {}
        }
    }

    fn accepts_audio(&self) -> bool {
        matches!(
            self,
            NodeEngine::Filter(_)
                | NodeEngine::Gain(_)
                | NodeEngine::Delay(_)
                | NodeEngine::Ring(_)
                | NodeEngine::Shaper(_)
                | NodeEngine::Formant(_)
                | NodeEngine::Out(_)
        )
    }

    fn tap(&self, k: usize) -> f32 {
        match self {
            NodeEngine::Delay(d) => d.taps[k],
            _ => 0.0,
        }
    }

    fn settled(&self) -> bool {
        match self {
            NodeEngine::Env(e) => e.idle,
            _ => true,
        }
    }

    fn note_on(&mut self, fresh: bool) {
        match self {
            NodeEngine::Osc(o) => {
                o.phases = [0.0; MAX_UNISON];
                o.noise = NoiseGen::seeded();
            }
            NodeEngine::FmPair(f) => {
                f.c_phase = 0.0;
                f.m_phase = 0.0;
                f.prev = 0.0;
            }
            NodeEngine::Filter(f) => f.svf.reset(),
            NodeEngine::Env(e) => {
                e.phase_ms = 0.0;
                e.rel_ms = 0.0;
                e.level = 0.0;
                e.idle = false;
            }
            // LFO free-runs across notes; delay rings persist through legato
            // retriggers but clear on a fresh trigger after the tail died.
            NodeEngine::Delay(d) if fresh => {
                d.ring.fill(0.0);
                d.taps = [0.0; BLOCK_FRAMES];
                d.write = 0;
            }
            NodeEngine::Sampler(s) => {
                s.pos = 0.0;
                s.playing = s.data.is_some();
            }
            _ => {}
        }
    }

    /// Latch a non-finite node: freeze state, zero signal memory. The node
    /// stays muted for the voice's lifetime — output can never be re-poisoned.
    fn sanitize(&mut self) {
        match self {
            NodeEngine::Osc(o) => {
                o.phases = [0.0; MAX_UNISON];
                o.noise = NoiseGen::seeded();
            }
            NodeEngine::FmPair(f) => {
                f.c_phase = 0.0;
                f.m_phase = 0.0;
                f.prev = 0.0;
            }
            NodeEngine::Filter(f) => f.svf.reset(),
            NodeEngine::Env(e) => {
                e.idle = true;
                e.level = 0.0;
            }
            NodeEngine::Lfo(l) => l.phase = 0.0,
            NodeEngine::Gain(g) => g.gain = 0.0,
            NodeEngine::Ring(r) => r.level = 0.0,
            NodeEngine::Delay(d) => {
                d.ring.fill(0.0);
                d.taps = [0.0; BLOCK_FRAMES];
                d.write = 0;
            }
            NodeEngine::Shaper(s) => {
                s.drive_eff = s.drive;
            }
            NodeEngine::Formant(f) => {
                f.svfs.iter_mut().for_each(Svf::reset);
                f.shift_eff = f.shift;
            }
            NodeEngine::Sampler(s) => {
                s.pos = 0.0;
                s.playing = false;
            }
            NodeEngine::Out(_) => {}
        }
    }

    /// Evaluate one tile into `out` (len ≤ BLOCK_FRAMES). `input` is the
    /// flushed sum of the node's incoming audio edges (zeros for sources);
    /// `carrier` is the `ring` carrier-socket sum (zeros everywhere else).
    fn eval(
        &mut self,
        input: &[f32; BLOCK_FRAMES],
        carrier: &[f32; BLOCK_FRAMES],
        out: &mut [f32],
        freq: f32,
        sr: f32,
        velocity: f32,
    ) {
        match self {
            NodeEngine::Osc(o) => {
                let unison = o.unison;
                let mut dets = [1.0f32; MAX_UNISON];
                for (u, det) in dets.iter_mut().enumerate().take(unison) {
                    let cents = o.cents_eff * (u as f32 - (unison as f32 - 1.0) * 0.5);
                    *det = (cents / 1200.0).exp2();
                }
                let dt = freq / sr;
                let norm = o.level / unison as f32;
                let (wave, phases, noise) = (&o.wave, &mut o.phases, &mut o.noise);
                for slot in out.iter_mut() {
                    let mut sum = 0.0f32;
                    for (u, p) in phases.iter_mut().enumerate().take(unison) {
                        let dtu = dt * dets[u];
                        *p += dtu;
                        if *p >= 1.0 {
                            *p -= 1.0;
                        }
                        sum += match wave {
                            OscWave::Saw => saw_value(*p, dtu),
                            OscWave::Square => square_value(*p, dtu),
                            OscWave::Sine => (TAU * *p).sin(),
                            OscWave::Tri => 4.0 * (*p - 0.5).abs() - 1.0,
                            OscWave::Noise => noise.next_f32(),
                        };
                    }
                    *slot = sum * norm;
                }
            }
            NodeEngine::FmPair(f) => {
                let dt_c = freq * f.ratio / sr;
                let dt_m = freq / sr;
                for slot in out.iter_mut() {
                    f.c_phase += dt_c;
                    if f.c_phase >= 1.0 {
                        f.c_phase -= 1.0;
                    }
                    f.m_phase += dt_m;
                    if f.m_phase >= 1.0 {
                        f.m_phase -= 1.0;
                    }
                    let y = (TAU * f.c_phase + f.index_eff * (TAU * f.m_phase).sin() + f.feedback * f.prev).sin();
                    f.prev = flush_denormal(y);
                    *slot = y * f.level;
                }
            }
            NodeEngine::Filter(f) => {
                let drive_gain = 1.0 + 3.0 * f.drive;
                for (k, slot) in out.iter_mut().enumerate() {
                    let mut x = input[k] * drive_gain;
                    if f.drive > 0.0 {
                        x = x.clamp(-1.0, 1.0);
                    }
                    *slot = f.svf.process(x, f.mode);
                }
            }
            NodeEngine::Gain(g) => {
                for (k, slot) in out.iter_mut().enumerate() {
                    *slot = input[k] * g.gain;
                }
            }
            NodeEngine::Ring(r) => {
                for (k, slot) in out.iter_mut().enumerate() {
                    *slot = flush_denormal(input[k] * carrier[k] * r.level);
                }
            }
            NodeEngine::Shaper(s) => {
                // tanh soft-clip normalized so small signals pass at unity:
                // k = 1 + 9·drive, y = tanh(x·k) / tanh(k) ≤ 1.
                let k = 1.0 + 9.0 * s.drive_eff;
                let norm = k.tanh().recip();
                for (k_i, slot) in out.iter_mut().enumerate() {
                    *slot = (input[k_i] * k).tanh() * norm * s.level;
                }
            }
            NodeEngine::Formant(f) => {
                let formants = f.vowel.formants();
                for (svf, hz) in f.svfs.iter_mut().zip(formants) {
                    svf.set_cutoff(hz * f.shift_eff);
                }
                for (k, slot) in out.iter_mut().enumerate() {
                    let x = input[k];
                    let summed: f32 = f.svfs.iter_mut().map(|svf| svf.process(x, FilterMode::BandPass)).sum();
                    *slot = flush_denormal(summed * f.level * 0.4);
                }
            }
            NodeEngine::Sampler(s) => {
                let Some(data) = s.data.as_ref() else {
                    out.iter_mut().for_each(|slot| *slot = 0.0);
                    return;
                };
                // Pitch tracked from C3: rate 1.0 plays the sample as-is.
                let rate = freq / midi_to_hz(60.0);
                let len = data.len();
                for slot in out.iter_mut() {
                    if !s.playing {
                        *slot = 0.0;
                        continue;
                    }
                    let i = (s.pos as usize).min(len - 1);
                    let fr = s.pos - s.pos.floor();
                    let older = if i + 1 < len { data[i + 1] } else { data[i] };
                    *slot = flush_denormal((data[i] * (1.0 - fr) + older * fr) * s.level);
                    s.pos += rate;
                    if s.pos >= len as f32 {
                        s.playing = false;
                        *slot = 0.0;
                    }
                }
            }
            NodeEngine::Delay(d) => {
                let cap = d.ring.len();
                for (k, slot) in out.iter_mut().enumerate() {
                    let di = (d.time_frames as usize).min(cap);
                    let fr = d.time_frames - di as f32;
                    let base = (d.write + cap - di) % cap;
                    let older = (base + cap - 1) % cap;
                    let tap = d.ring[base] * (1.0 - fr) + d.ring[older] * fr;
                    d.ring[d.write] = flush_denormal(input[k] + d.feedback * tap);
                    d.write = (d.write + 1) % cap;
                    d.taps[k] = tap;
                    *slot = d.mix * tap + (1.0 - d.mix) * input[k];
                }
            }
            NodeEngine::Out(o) => {
                for (k, slot) in out.iter_mut().enumerate() {
                    *slot = input[k] * o.level * velocity;
                }
            }
            // Control sources: signal flows through the mod matrix only.
            NodeEngine::Env(_) | NodeEngine::Lfo(_) => {}
        }
    }
}

/// Band-limited falling saw value at phase `p` (caller advances the phase).
fn saw_value(p: f32, dt: f32) -> f32 {
    let mut v = 1.0 - 2.0 * p;
    v += poly_blep(p, dt);
    v
}

/// Band-limited square: naive ±1 with polyBLEP correction at both edges
/// (rising at 0, falling at 0.5). Value-only — caller advances the phase.
fn square_value(p: f32, dt: f32) -> f32 {
    let mut v = if p < 0.5 { 1.0 } else { -1.0 };
    v += poly_blep(p, dt);
    let q = p + 0.5;
    let q = if q >= 1.0 { q - 1.0 } else { q };
    v -= poly_blep(q, dt);
    v
}

impl EnvState {
    fn tick(&mut self, ms: f32, gate: bool) -> f32 {
        if self.idle {
            return 0.0;
        }
        if gate {
            self.rel_ms = 0.0;
            self.phase_ms += ms;
            let v = if self.phase_ms < self.attack_ms {
                self.phase_ms / self.attack_ms
            } else {
                let t = (self.phase_ms - self.attack_ms) / self.decay_ms;
                self.sustain + (1.0 - self.sustain) * (-t).exp()
            };
            self.level = v;
            v
        } else {
            self.rel_ms += ms;
            let v = flush_denormal(self.level * (-self.rel_ms / self.release_ms).exp());
            self.level = v;
            if v < SILENCE_ABS {
                self.idle = true;
                0.0
            } else {
                v
            }
        }
    }
}

impl LfoState {
    fn tick(&mut self, ms: f32) -> f32 {
        self.phase += self.rate_hz * ms / 1000.0;
        self.phase -= self.phase.floor();
        let q = self.phase;
        let w = match self.wave {
            LfoWave::Sine => (TAU * q).sin(),
            LfoWave::Tri => {
                if q < 0.25 {
                    4.0 * q
                } else if q < 0.75 {
                    2.0 - 4.0 * q
                } else {
                    4.0 * q - 4.0
                }
            }
            LfoWave::Square => {
                if q < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
        };
        flush_denormal(w * self.depth)
    }
}

/// Depth-scaled mod application for node `i`'s mod edges (control rate:
/// once per tile, before the node evaluates).
fn apply_mods(nodes: &mut [NodeEngine], i: usize, mods: &[(usize, ModParam, f32)], src_vals: &[f32], sr: f32) {
    for &(src, param, amount) in mods {
        let v = src_vals[src] * amount;
        match (&mut nodes[i], param) {
            (NodeEngine::Filter(f), ModParam::Cutoff) => {
                let eff = (f.cutoff_hz * v.exp2()).clamp(bounds::PATCH_CUTOFF_HZ.0, bounds::PATCH_CUTOFF_HZ.1);
                f.svf.set_cutoff(eff);
            }
            (NodeEngine::Gain(g), ModParam::Level) => g.gain = (g.base + v).clamp(bounds::GAIN.0, bounds::GAIN.1),
            (NodeEngine::Osc(o), ModParam::Cents) => {
                o.cents_eff = (o.cents_eff + v).clamp(bounds::PAD_DETUNE_CENTS.0, bounds::PAD_DETUNE_CENTS.1)
            }
            (NodeEngine::FmPair(f), ModParam::Index) => {
                f.index_eff = (f.index_eff + v).clamp(bounds::FM_INDEX.0, bounds::FM_INDEX.1)
            }
            (NodeEngine::Delay(d), ModParam::Time) => {
                let ms = (d.time_ms * v.exp2()).clamp(bounds::DELAY_TIME_MS.0, bounds::DELAY_TIME_MS.1);
                d.time_frames = (ms * sr / 1000.0).clamp(1.0, d.ring.len() as f32);
            }
            (NodeEngine::Shaper(s), ModParam::Drive) => {
                s.drive_eff = (s.drive_eff + v).clamp(bounds::UNIT.0, bounds::UNIT.1)
            }
            (NodeEngine::Formant(f), ModParam::Shift) => {
                f.shift_eff = (f.shift_eff * v.exp2()).clamp(bounds::FORMANT_SHIFT.0, bounds::FORMANT_SHIFT.1)
            }
            (NodeEngine::Ring(r), ModParam::Level) => {
                r.level = (r.level + v).clamp(bounds::UNIT.0, bounds::UNIT.1)
            }
            // Construction only wires legal (target, param) pairs.
            _ => {}
        }
    }
}

/// Mono voice evaluating one compiled patch graph. Polyphony comes from the
/// existing [`crate::pool::VoicePool`] — one `PatchVoice` per note.
pub struct PatchVoice {
    sr: f32,
    freq: f32,
    velocity: f32,
    gate: bool,
    active: bool,
    out_idx: Option<usize>,
    plan: CompiledPatch,
    nodes: Box<[NodeEngine]>,
    /// Latched non-finite nodes (silenced for the voice's lifetime).
    muted: Box<[bool]>,
    bufs: Box<[[f32; BLOCK_FRAMES]]>,
    /// Per node: (source exec index, edge gain) forward audio feeds.
    feeds: Box<[Box<[(usize, f32)]>]>,
    /// Per node: (source exec index, edge gain) carrier-socket feeds (ring).
    carrier_feeds: Box<[Box<[(usize, f32)]>]>,
    /// Per node: (delay exec index, depth) one-tile-late feedback taps.
    fb_taps: Box<[Box<[(usize, f32)]>]>,
    /// Per node: (mod source exec index, target param, depth).
    mods: Box<[Box<[(usize, ModParam, f32)]>]>,
    src_vals: Box<[f32]>,
    bank: Arc<SampleBank>,
}

impl PatchVoice {
    /// Compile a validated patch and build the evaluator.
    pub fn from_patch(sample_rate: u32, patch: &CustomPatch) -> Result<Self, kontinuum_ir::compile::PatchCompileError> {
        let plan = compile_patch(patch)?;
        Ok(Self::new(sample_rate, &plan))
    }

    /// [`PatchVoice::from_patch`] with PCM for the patch's `sampler` nodes.
    pub fn from_patch_with_bank(
        sample_rate: u32,
        patch: &CustomPatch,
        bank: Arc<SampleBank>,
    ) -> Result<Self, kontinuum_ir::compile::PatchCompileError> {
        let plan = compile_patch(patch)?;
        Ok(Self::new_with_bank(sample_rate, &plan, bank))
    }

    /// Build the evaluator from an already-compiled plan. All wiring and
    /// buffers are allocated here (off-RT); `render` never allocates.
    pub fn new(sample_rate: u32, plan: &CompiledPatch) -> Self {
        Self::new_with_bank(sample_rate, plan, Arc::new(SampleBank::new()))
    }

    /// [`PatchVoice::new`] with PCM for the patch's `sampler` nodes.
    pub fn new_with_bank(sample_rate: u32, plan: &CompiledPatch, bank: Arc<SampleBank>) -> Self {
        let n = plan.nodes.len();
        let mut index: HashMap<&str, usize> = HashMap::with_capacity(n);
        for (i, node) in plan.nodes.iter().enumerate() {
            index.insert(node.id(), i);
        }
        let nodes: Box<[NodeEngine]> = plan
            .nodes
            .iter()
            .map(|node| NodeEngine::build(node, sample_rate, &bank))
            .collect();
        let mut feeds: Vec<Vec<(usize, f32)>> = vec![Vec::new(); n];
        let mut carrier_feeds: Vec<Vec<(usize, f32)>> = vec![Vec::new(); n];
        let fb_taps: Vec<Vec<(usize, f32)>> = vec![Vec::new(); n];
        let mut mods: Vec<Vec<(usize, ModParam, f32)>> = vec![Vec::new(); n];
        for e in &plan.edges {
            let (Some(&from), Some(&to)) = (index.get(e.from.as_str()), index.get(e.to.as_str())) else {
                continue;
            };
            if from == to {
                // d→d self edge: the loop is the delay's own recirculation,
                // already applied at `feedback` inside the ring write.
                continue;
            }
            match e.kind {
                EdgeKind::Audio => {
                    if !plan.nodes[from].produces_audio() || !plan.nodes[to].accepts_audio() {
                        continue;
                    }
                    if e.param.as_deref() == Some(RING_CARRIER_SOCKET) {
                        carrier_feeds[to].push((from, e.amount.clamp(bounds::GAIN.0, bounds::GAIN.1)));
                    } else {
                        feeds[to].push((from, e.amount.clamp(bounds::GAIN.0, bounds::GAIN.1)));
                    }
                }
                EdgeKind::Mod => {
                    if let Some(param) = mod_param(&plan.nodes[to], e.param.as_deref()) {
                        if plan.nodes[from].is_mod_source() {
                            mods[to].push((from, param, e.amount.clamp(bounds::UNIT.0, bounds::UNIT.1)));
                        }
                    }
                }
            }
        }
        let boxed = |v: Vec<Vec<(usize, f32)>>| v.into_iter().map(Vec::into_boxed_slice).collect::<Vec<_>>();
        PatchVoice {
            sr: sample_rate as f32,
            freq: midi_to_hz(60.0),
            velocity: 0.0,
            gate: false,
            active: false,
            out_idx: plan.nodes.iter().position(|node| matches!(node, PatchNode::Out(_))),
            plan: plan.clone(),
            nodes,
            muted: vec![false; n].into_boxed_slice(),
            bufs: vec![[0.0f32; BLOCK_FRAMES]; n].into_boxed_slice(),
            feeds: boxed(feeds).into_boxed_slice(),
            carrier_feeds: boxed(carrier_feeds).into_boxed_slice(),
            fb_taps: boxed(fb_taps).into_boxed_slice(),
            mods: mods.into_iter().map(Vec::into_boxed_slice).collect::<Vec<_>>().into_boxed_slice(),
            src_vals: vec![0.0f32; n].into_boxed_slice(),
            bank,
        }
    }

    fn render_tile(&mut self, tile: &mut [f32]) {
        let n = tile.len();
        let ms = n as f32 * 1000.0 / self.sr;
        let Self {
            sr, freq, velocity, gate, active, out_idx, nodes, muted, bufs, feeds, carrier_feeds,
            fb_taps, mods, src_vals, ..
        } = self;

        // Control-rate sources advance once per tile; non-finite values latch.
        for (i, node) in nodes.iter_mut().enumerate() {
            let v = node.tick_source(ms, *gate);
            src_vals[i] = if v.is_finite() {
                v
            } else {
                muted[i] = true;
                node.sanitize();
                0.0
            };
        }

        for i in 0..nodes.len() {
            if muted[i] {
                bufs[i][..n].fill(0.0);
                continue;
            }
            let mut input = [0.0f32; BLOCK_FRAMES];
            if nodes[i].accepts_audio() {
                for &(src, amt) in feeds[i].iter() {
                    let b = &bufs[src];
                    for (k, acc) in input.iter_mut().enumerate().take(n) {
                        *acc += b[k] * amt;
                    }
                }
                for &(d, amt) in fb_taps[i].iter() {
                    for (k, acc) in input.iter_mut().enumerate().take(n) {
                        *acc += nodes[d].tap(k) * amt;
                    }
                }
                for acc in input.iter_mut().take(n) {
                    *acc = flush_denormal(*acc);
                }
            }
            let mut carrier_buf = [0.0f32; BLOCK_FRAMES];
            let mut has_carrier = false;
            for &(src, amt) in carrier_feeds[i].iter() {
                has_carrier = true;
                let b = &bufs[src];
                for (k, acc) in carrier_buf.iter_mut().enumerate().take(n) {
                    *acc += b[k] * amt;
                }
            }
            // No carrier wired ⇒ carrier is silence (output identically zero);
            // validation rejects such rings, this keeps construction honest.
            const ZERO: [f32; BLOCK_FRAMES] = [0.0; BLOCK_FRAMES];
            let carrier = if has_carrier {
                for acc in carrier_buf.iter_mut().take(n) {
                    *acc = flush_denormal(*acc);
                }
                &carrier_buf
            } else {
                &ZERO
            };
            nodes[i].reset_eff();
            apply_mods(nodes, i, &mods[i], src_vals, *sr);
            nodes[i].eval(&input, carrier, &mut bufs[i][..n], *freq, *sr, *velocity);
            let bad = bufs[i][..n].iter().any(|s| !s.is_finite());
            if bad {
                muted[i] = true;
                nodes[i].sanitize();
                bufs[i][..n].fill(0.0);
            }
        }

        // Deactivation: gate off, every env released, output sub-silent.
        // A patch with no env nodes has no gate-aware sustain path, so a
        // silent output retires it even under a held gate — the one-shot
        // sampler convention (matches the built-in perc voices, whose
        // `note_off` is a no-op).
        if *active {
            let no_gate_path = nodes.iter().all(|n| !matches!(n, NodeEngine::Env(_)));
            if !*gate || no_gate_path {
                let settled = nodes.iter().all(NodeEngine::settled);
                let tail = out_idx.map(|i| bufs[i][n - 1]).unwrap_or(0.0);
                if settled && tail.abs() < SILENCE_ABS {
                    *active = false;
                }
            }
        }
        if let Some(i) = *out_idx {
            tile.copy_from_slice(&bufs[i][..n]);
        } else {
            tile.fill(0.0);
        }
    }
}

/// Resolves a mod edge's `param` against the target node's mod-able params.
fn mod_param(target: &PatchNode, param: Option<&str>) -> Option<ModParam> {
    let name = param?;
    if !target.mod_targets().contains(&name) {
        return None;
    }
    match name {
        "cutoff_hz" => Some(ModParam::Cutoff),
        "level" => Some(ModParam::Level),
        "fine_cents" => Some(ModParam::Cents),
        "index" => Some(ModParam::Index),
        "time_ms" => Some(ModParam::Time),
        "drive" => Some(ModParam::Drive),
        "shift" => Some(ModParam::Shift),
        _ => None,
    }
}

impl Voice for PatchVoice {
    fn note_on(&mut self, pitch: f32, velocity: f32) {
        let fresh = !self.active;
        self.freq = midi_to_hz(pitch.clamp(24.0, 96.0));
        self.velocity = velocity.clamp(0.0, 1.0);
        self.gate = true;
        self.active = true;
        for node in self.nodes.iter_mut() {
            node.note_on(fresh);
        }
    }

    fn note_off(&mut self) {
        self.gate = false;
    }

    fn is_active(&self) -> bool {
        self.active
    }

    fn render(&mut self, out: &mut [f32]) {
        for tile in out.chunks_mut(BLOCK_FRAMES) {
            if self.active {
                self.render_tile(tile);
            } else {
                tile.fill(0.0);
            }
        }
    }

    /// Patch parameters are data (the mod matrix is the automation seam);
    /// no host ParamIds are defined for v1.
    fn set_param(&mut self, _param: ParamId, _value: f32) {}

    fn reset(&mut self) {
        let bank = Arc::clone(&self.bank);
        *self = Self::new_with_bank(self.sr as u32, &self.plan, bank);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kontinuum_ir::patch::{
        CustomTag, DelayTag, EnvTag, FilterTag, FormantTag, FmPairTag, GainNode, GainTag, LfoTag,
        OscTag, OutTag, PatchEdge, PatchGraph, RingTag, SamplerTag, ShaperTag,
    };
    use std::sync::Arc;

    const SR: u32 = 48_000;
    /// 10 s at 48 kHz in 64-frame tiles.
    const TILES_10S: usize = 7_500;

    // -- IR constructors (the issue's canonical patches, v1 vocabulary) ------

    fn osc(id: &str, wave: OscWave, unison: u8, fine_cents: f32, level: f32) -> PatchNode {
        PatchNode::Osc(OscNode {
            id: id.into(),
            kind: OscTag::Osc,
            wave,
            unison,
            fine_cents,
            level,
        })
    }

    fn fm(id: &str, ratio: f32, index: f32, feedback: f32, level: f32) -> PatchNode {
        PatchNode::FmPair(FmPairNode { id: id.into(), kind: FmPairTag::FmPair, ratio, index, feedback, level })
    }

    fn filt(id: &str, mode: PatchFilterMode, cutoff: f32, res: f32, drive: f32) -> PatchNode {
        PatchNode::Filter(FilterNode {
            id: id.into(),
            kind: FilterTag::Filter,
            mode,
            cutoff_hz: cutoff,
            resonance: res,
            drive,
        })
    }

    fn env(id: &str, a: f32, d: f32, s: f32, r: f32) -> PatchNode {
        PatchNode::Env(EnvNode { id: id.into(), kind: EnvTag::Env, attack_ms: a, decay_ms: d, sustain: s, release_ms: r })
    }

    fn lfo(id: &str, rate: f32, depth: f32) -> PatchNode {
        PatchNode::Lfo(LfoNode { id: id.into(), kind: LfoTag::Lfo, rate_hz: rate, depth, wave: Default::default() })
    }

    fn gain(id: &str, level: f32) -> PatchNode {
        PatchNode::Gain(GainNode { id: id.into(), kind: GainTag::Gain, level })
    }

    fn ring(id: &str, level: f32) -> PatchNode {
        PatchNode::Ring(RingNode { id: id.into(), kind: RingTag::Ring, level })
    }

    fn shaper(id: &str, drive: f32) -> PatchNode {
        PatchNode::Shaper(ShaperNode { id: id.into(), kind: ShaperTag::Shaper, drive, level: 1.0 })
    }

    fn formant(id: &str, vowel: FormantVowel, shift: f32) -> PatchNode {
        PatchNode::Formant(FormantNode { id: id.into(), kind: FormantTag::Formant, vowel, shift, level: 1.0 })
    }

    fn sampler(id: &str, slot: u32) -> PatchNode {
        PatchNode::Sampler(SamplerNode { id: id.into(), kind: SamplerTag::Sampler, slot, level: 1.0 })
    }

    fn delay(id: &str, time_ms: f32, feedback: f32, mix: f32) -> PatchNode {
        PatchNode::Delay(DelayNode { id: id.into(), kind: DelayTag::Delay, time_ms, feedback, mix })
    }

    fn out(id: &str, level: f32) -> PatchNode {
        PatchNode::Out(OutNode { id: id.into(), kind: OutTag::Out, level })
    }

    fn audio(from: &str, to: &str) -> PatchEdge {
        PatchEdge { from: from.into(), to: to.into(), kind: EdgeKind::Audio, param: None, amount: 1.0 }
    }

    fn audio_socket(from: &str, to: &str, socket: &str) -> PatchEdge {
        PatchEdge {
            from: from.into(),
            to: to.into(),
            kind: EdgeKind::Audio,
            param: Some(socket.into()),
            amount: 1.0,
        }
    }

    fn mod_edge(from: &str, to: &str, param: &str, amount: f32) -> PatchEdge {
        PatchEdge {
            from: from.into(),
            to: to.into(),
            kind: EdgeKind::Mod,
            param: Some(param.into()),
            amount,
        }
    }

    fn patch(nodes: Vec<PatchNode>, edges: Vec<PatchEdge>) -> CustomPatch {
        CustomPatch { kind: CustomTag::Custom, patch: PatchGraph { nodes, edges } }
    }

    fn hoover_patch() -> CustomPatch {
        patch(
            vec![
                osc("saws", OscWave::Saw, 7, 35.0, 0.7),
                filt("lp", PatchFilterMode::LowPass, 500.0, 0.55, 0.15),
                gain("vca", 0.0),
                env("amp", 180.0, 900.0, 0.7, 500.0),
                out("out", 0.9),
            ],
            vec![
                audio("saws", "lp"),
                audio("lp", "vca"),
                audio("vca", "out"),
                mod_edge("amp", "vca", "level", 1.0),
            ],
        )
    }

    fn rhodes_patch() -> CustomPatch {
        patch(
            vec![
                fm("tine", 4.0, 1.5, 0.0, 0.5),
                fm("body", 1.0, 2.0, 0.1, 0.8),
                gain("tine_vca", 0.0),
                gain("body_vca", 0.0),
                env("tine_env", 1.0, 350.0, 0.0, 200.0),
                env("body_env", 2.0, 1500.0, 0.25, 600.0),
                out("out", 0.9),
            ],
            vec![
                audio("tine", "tine_vca"),
                audio("body", "body_vca"),
                audio("tine_vca", "out"),
                audio("body_vca", "out"),
                mod_edge("tine_env", "tine_vca", "level", 1.0),
                mod_edge("body_env", "body_vca", "level", 1.0),
            ],
        )
    }

    fn rumble_patch() -> CustomPatch {
        patch(
            vec![
                osc("noise", OscWave::Noise, 1, 0.0, 1.0),
                filt("lp", PatchFilterMode::LowPass, 90.0, 0.85, 0.0),
                gain("vca", 0.0),
                env("amp", 300.0, 2000.0, 0.9, 900.0),
                out("out", 1.0),
            ],
            vec![
                audio("noise", "lp"),
                audio("lp", "vca"),
                audio("vca", "out"),
                mod_edge("amp", "vca", "level", 1.0),
            ],
        )
    }

    fn formant_patch() -> CustomPatch {
        patch(
            vec![
                osc("saw", OscWave::Saw, 3, 12.0, 0.6),
                filt("f1", PatchFilterMode::BandPass, 700.0, 0.6, 0.0),
                filt("f2", PatchFilterMode::BandPass, 1200.0, 0.6, 0.0),
                filt("f3", PatchFilterMode::BandPass, 2500.0, 0.6, 0.0),
                gain("g1", 0.9),
                gain("g2", 0.6),
                gain("g3", 0.4),
                gain("vca", 0.0),
                env("amp", 400.0, 1000.0, 0.8, 800.0),
                lfo("drift", 0.6, 1.0),
                out("out", 0.9),
            ],
            vec![
                audio("saw", "f1"),
                audio("saw", "f2"),
                audio("saw", "f3"),
                audio("f1", "g1"),
                audio("f2", "g2"),
                audio("f3", "g3"),
                audio("g1", "vca"),
                audio("g2", "vca"),
                audio("g3", "vca"),
                audio("vca", "out"),
                mod_edge("amp", "vca", "level", 1.0),
                mod_edge("drift", "f1", "cutoff_hz", 0.5),
            ],
        )
    }

    fn cowbell_patch() -> CustomPatch {
        // sq2's detune sits at the ±100 cent bound (the IR detune vocabulary);
        // the interval character comes from the band-pass, as rendered.
        patch(
            vec![
                osc("sq1", OscWave::Square, 1, 0.0, 0.5),
                osc("sq2", OscWave::Square, 1, 100.0, 0.5),
                filt("bp", PatchFilterMode::BandPass, 800.0, 0.45, 0.0),
                gain("vca", 0.0),
                env("amp", 1.0, 350.0, 0.0, 150.0),
                out("out", 0.9),
            ],
            vec![
                audio("sq1", "bp"),
                audio("sq2", "bp"),
                audio("bp", "vca"),
                audio("vca", "out"),
                mod_edge("amp", "vca", "level", 1.0),
            ],
        )
    }

    // -- Helpers --------------------------------------------------------------

    fn render_tiles(v: &mut PatchVoice, tiles: usize) -> Vec<f32> {
        let mut out = vec![0.0f32; tiles * BLOCK_FRAMES];
        for chunk in out.chunks_mut(BLOCK_FRAMES) {
            v.render(chunk);
        }
        out
    }

    fn peak(buf: &[f32]) -> f32 {
        buf.iter().fold(0.0f32, |m, &s| m.max(s.abs()))
    }

    fn rms(buf: &[f32]) -> f32 {
        (buf.iter().map(|s| s * s).sum::<f32>() / buf.len().max(1) as f32).sqrt()
    }

    /// Two independent 10 s renders of one patch: bit-identical, finite,
    /// audible, bounded. The long run doubles as the denormal-blowup check.
    fn assert_golden(make: fn() -> CustomPatch, name: &str) {
        let tiles = TILES_10S;
        let mut runs = Vec::new();
        for _ in 0..2 {
            let mut v = PatchVoice::from_patch(SR, &make()).expect(name);
            assert!(!v.is_active(), "{name}: active before note_on");
            v.note_on(60.0, 0.9);
            runs.push(render_tiles(&mut v, tiles));
        }
        let a = &runs[0];
        let b = &runs[1];
        assert!(
            a.iter().zip(b.iter()).all(|(x, y)| x.to_bits() == y.to_bits()),
            "{name}: renders diverged across runs"
        );
        assert!(a.iter().all(|s| s.is_finite()), "{name}: non-finite sample over 10 s");
        let p = peak(a);
        assert!(p > 0.01, "{name}: silent render, peak {p}");
        assert!(p < 16.0, "{name}: unbounded peak {p}");
    }

    // -- The five golden patches (#37 Engine) ---------------------------------

    #[test]
    fn golden_hoover_bit_deterministic_finite_audible_over_10s() {
        assert_golden(hoover_patch, "hoover");
    }

    #[test]
    fn golden_fm_rhodes_bit_deterministic_finite_audible_over_10s() {
        assert_golden(rhodes_patch, "fm rhodes");
    }

    #[test]
    fn golden_rumble_bit_deterministic_finite_audible_over_10s() {
        assert_golden(rumble_patch, "rumble");
    }

    #[test]
    fn golden_formant_pad_bit_deterministic_finite_audible_over_10s() {
        assert_golden(formant_patch, "formant pad");
    }

    #[test]
    fn golden_cowbell_bit_deterministic_finite_audible_over_10s() {
        assert_golden(cowbell_patch, "cowbell");
    }

    /// The shareable canonical JSON (the composer few-shot library) must be
    /// the exact data these golden tests render — prompt examples and engine
    /// behavior cannot drift apart.
    #[test]
    fn canonical_fixtures_equal_the_golden_constructors() {
        let cases: [(&str, fn() -> CustomPatch); 5] = [
            ("hoover", hoover_patch),
            ("fm_rhodes", rhodes_patch),
            ("rumble", rumble_patch),
            ("formant_pad", formant_patch),
            ("cowbell_808", cowbell_patch),
        ];
        for (id, make) in cases {
            let path = format!(
                "{}/../../ir/kontinuum-ir/fixtures/patches/canonical/{id}.json",
                env!("CARGO_MANIFEST_DIR")
            );
            let json = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{id}: read {path}: {e}"));
            let parsed: CustomPatch = serde_json::from_str(&json).expect("canonical json");
            assert_eq!(parsed, make(), "{id}: fixture drifted from the golden constructor");
        }
    }

    // -- Feedback loop under the cycle rules -----------------------------------

    #[test]
    fn feedback_delay_loop_decays_and_stays_bounded() {
        // osc → env VCA → delay(120 ms, fb 0.7, mix 0.5) → out, loop closed
        // by the d→d edge the IR compiler classifies as feedback.
        let p = patch(
            vec![
                osc("saw", OscWave::Saw, 1, 0.0, 0.5),
                gain("vca", 0.0),
                delay("dly", 120.0, 0.7, 0.5),
                env("amp", 1.0, 60.0, 0.0, 40.0),
                out("out", 1.0),
            ],
            vec![
                audio("saw", "vca"),
                audio("vca", "dly"),
                audio("dly", "out"),
                audio("dly", "dly"),
                mod_edge("amp", "vca", "level", 1.0),
            ],
        );
        let mut v = PatchVoice::from_patch(SR, &p).expect("compile");
        v.note_on(60.0, 0.9);
        let gated = render_tiles(&mut v, 750); // 1 s gated
        v.note_off();
        let mut tail = render_tiles(&mut v, 4_688); // ~6 s of release + ring-down
        tail.splice(0..0, gated.clone());

        assert!(gated.iter().all(|s| s.is_finite()) && tail.iter().all(|s| s.is_finite()));
        assert!(peak(&gated) > 0.01, "gated segment silent");
        // Ring-down, not blowup: a late window must sit far under the gated
        // level (0.7^5 taps ≈ -17 dB/s), and the voice must retire itself
        // once the tail drops below the silence floor.
        let late_rms = rms(&tail[tail.len() - 48_000..]);
        let mid_rms = rms(&gated[24_000..48_000]);
        assert!(late_rms < mid_rms / 8.0, "feedback loop did not decay: late {late_rms} vs gated {mid_rms}");
        assert!(!v.is_active(), "voice never retired after the loop rang down");
        assert!(tail[tail.len() - BLOCK_FRAMES..].iter().all(|&s| s == 0.0), "tail not exactly zero");
    }

    // -- Modulation is audible --------------------------------------------------

    #[test]
    fn lfo_cutoff_modulation_audibly_changes_output() {
        let mut with_mod = hoover_patch().patch;
        with_mod.nodes.push(lfo("wobble", 5.0, 1.0));
        with_mod.edges.push(mod_edge("wobble", "lp", "cutoff_hz", 1.0));
        let with_mod = CustomPatch { kind: CustomTag::Custom, patch: with_mod };

        let mut a = PatchVoice::from_patch(SR, &hoover_patch()).expect("compile");
        let mut b = PatchVoice::from_patch(SR, &with_mod).expect("compile");
        a.note_on(60.0, 0.9);
        b.note_on(60.0, 0.9);
        let ra = render_tiles(&mut a, 1_500); // 2 s: 10 LFO cycles
        let rb = render_tiles(&mut b, 1_500);
        assert!(ra.iter().zip(rb.iter()).any(|(x, y)| x.to_bits() != y.to_bits()), "mod edge changed nothing");
        let (ma, mb) = (rms(&ra), rms(&rb));
        let spread = (ma - mb).abs() / ma.max(1e-9);
        assert!(spread > 0.08, "LFO cutoff sweep not audible: rms {ma} vs {mb}");
    }

    // -- Envelope lifecycle ------------------------------------------------------

    #[test]
    fn note_on_off_envelope_lifecycle_ends_silent() {
        let mut v = PatchVoice::from_patch(SR, &rhodes_patch()).expect("compile");
        assert!(!v.is_active());
        v.note_on(60.0, 0.9);
        assert!(v.is_active());
        let head = render_tiles(&mut v, 16);
        assert!(peak(&head) > 0.01, "note_on produced no sound");
        v.note_off();
        let mut blocks = 0;
        while v.is_active() && blocks < 4_000 {
            let mut buf = [0.0f32; BLOCK_FRAMES];
            v.render(&mut buf);
            blocks += 1;
        }
        assert!(blocks < 4_000, "voice never released");
        let mut tail = [1.0f32; BLOCK_FRAMES];
        v.render(&mut tail);
        assert!(tail.iter().all(|&s| s == 0.0), "tail not exactly zero after release");
    }

    // -- Independence / polyphony -------------------------------------------------

    #[test]
    fn two_patch_voices_render_independently() {
        let solo = |make: fn() -> CustomPatch, tiles: usize| {
            let mut v = PatchVoice::from_patch(SR, &make()).expect("compile");
            v.note_on(60.0, 0.9);
            render_tiles(&mut v, tiles)
        };
        let a_solo = solo(hoover_patch, 8);
        let c_solo = solo(cowbell_patch, 8);
        // Interleave tile-by-tile; each voice's stream must match its solo
        // render bit-for-bit (no shared state, no cross-talk).
        let mut a_mix = PatchVoice::from_patch(SR, &hoover_patch()).expect("compile");
        let mut c_mix = PatchVoice::from_patch(SR, &cowbell_patch()).expect("compile");
        a_mix.note_on(60.0, 0.9);
        c_mix.note_on(60.0, 0.9);
        let mut a_buf = vec![0.0f32; 8 * BLOCK_FRAMES];
        let mut c_buf = vec![0.0f32; 8 * BLOCK_FRAMES];
        for t in 0..8 {
            a_mix.render(&mut a_buf[t * BLOCK_FRAMES..(t + 1) * BLOCK_FRAMES]);
            c_mix.render(&mut c_buf[t * BLOCK_FRAMES..(t + 1) * BLOCK_FRAMES]);
        }
        assert!(a_solo.iter().zip(a_buf.iter()).all(|(x, y)| x.to_bits() == y.to_bits()));
        assert!(c_solo.iter().zip(c_buf.iter()).all(|(x, y)| x.to_bits() == y.to_bits()));
    }

    #[test]
    fn patch_voice_pool_integration_steals_and_stays_finite() {
        let plan = Arc::new(kontinuum_ir::compile::compile_patch(&hoover_patch()).expect("compile"));
        let mut pool: crate::pool::VoicePool<Box<dyn Voice>> = crate::pool::VoicePool::new(2, {
            let plan = Arc::clone(&plan);
            move || Box::new(PatchVoice::new(SR, &plan)) as Box<dyn Voice>
        });
        assert_eq!(pool.capacity(), 2);
        for voice in 0..3u8 {
            pool.note_on(60.0 + voice as f32, 0.8);
        }
        let mut out = [0.0f32; BLOCK_FRAMES];
        pool.render(&mut out);
        assert!(out.iter().all(|s| s.is_finite()));
        assert!(pool.active_count() <= 2, "pool exceeded capacity");
    }

    // -- RT hygiene + guards ------------------------------------------------------

    #[test]
    fn render_is_allocation_free() {
        // Feedback patch: exercises the ring-buffer path, the heaviest one.
        let p = patch(
            vec![
                osc("saw", OscWave::Saw, 3, 12.0, 0.5),
                delay("dly", 90.0, 0.6, 0.4),
                gain("vca", 0.8),
                env("amp", 5.0, 500.0, 0.5, 200.0),
                out("out", 0.9),
            ],
            vec![
                audio("saw", "dly"),
                audio("dly", "vca"),
                audio("vca", "out"),
                audio("dly", "dly"),
                mod_edge("amp", "vca", "level", 1.0),
            ],
        );
        let mut v = PatchVoice::from_patch(SR, &p).expect("compile");
        v.note_on(60.0, 0.9);
        let mut buf = [0.0f32; BLOCK_FRAMES];
        v.render(&mut buf); // warm
        assert_no_alloc::assert_no_alloc(|| v.render(&mut buf));
        assert_no_alloc::assert_no_alloc(|| v.note_on(62.0, 0.8));
        assert_no_alloc::assert_no_alloc(|| v.render(&mut buf));
    }

    #[test]
    fn non_finite_node_state_is_silenced_without_poisoning_output() {
        let mut v = PatchVoice::from_patch(SR, &hoover_patch()).expect("compile");
        v.note_on(60.0, 0.9);
        // Corrupt the first node's state directly (guard is test-injected;
        // clamped construction cannot produce NaN from data).
        for node in v.nodes.iter_mut() {
            if let NodeEngine::Osc(o) = node {
                o.phases[0] = f32::NAN;
            }
            break;
        }
        let mut out = [0.0f32; BLOCK_FRAMES];
        for _ in 0..8 {
            v.render(&mut out);
            assert!(out.iter().all(|s| s.is_finite()), "NaN escaped the node guard");
        }
    }

    #[test]
    fn illegal_edges_are_ignored_at_construction() {
        // compile_patch only guards structure — signal-type lints live in
        // validate — so construction must drop these without panicking.
        let p = patch(
            vec![
                osc("saw", OscWave::Saw, 1, 0.0, 0.8),
                env("e", 5.0, 300.0, 0.5, 200.0),
                gain("g", 1.0),
                out("out", 1.0),
            ],
            vec![
                audio("saw", "g"),
                audio("g", "out"),
                audio("e", "g"),
                mod_edge("saw", "g", "bogus_param", 1.0),
                mod_edge("saw", "out", "level", 1.0),
            ],
        );
        let mut v = PatchVoice::from_patch(SR, &p).expect("compile");
        v.note_on(60.0, 0.9);
        let out = render_tiles(&mut v, 4);
        assert!(out.iter().all(|s| s.is_finite()) && peak(&out) > 0.01);
    }

    // -- New node kinds (issue #37 vocabulary round 2) --------------------------

    #[test]
    fn ring_modulator_outputs_the_signal_product() {
        let build = |with_carrier: bool| {
            let mut edges = vec![audio("o1", "rm"), audio("rm", "out")];
            if with_carrier {
                edges.push(audio_socket("c1", "rm", RING_CARRIER_SOCKET));
            }
            patch(
                vec![
                    osc("o1", OscWave::Sine, 1, 0.0, 0.8),
                    osc("c1", OscWave::Sine, 1, 0.0, 0.8),
                    ring("rm", 1.0),
                    out("out", 1.0),
                ],
                edges,
            )
        };
        let mut v = PatchVoice::from_patch(SR, &build(true)).expect("compile");
        v.note_on(60.0, 1.0);
        let product = render_tiles(&mut v, 16);
        assert!(product.iter().all(|s| s.is_finite()));
        let p = peak(&product);
        assert!(p > 0.05 && p <= 0.9, "ring product peak {p}");

        // Deterministic render.
        let mut v2 = PatchVoice::from_patch(SR, &build(true)).expect("compile");
        v2.note_on(60.0, 1.0);
        let again = render_tiles(&mut v2, 16);
        assert!(product.iter().zip(again.iter()).all(|(x, y)| x.to_bits() == y.to_bits()));

        // Without a carrier the node is identically zero — never self-modding.
        let mut silent = PatchVoice::from_patch(SR, &build(false)).expect("compile");
        silent.note_on(60.0, 1.0);
        let out = render_tiles(&mut silent, 16);
        assert!(out.iter().all(|&s| s == 0.0), "carrier-less ring must be silent");
    }

    #[test]
    fn shaper_drive_soft_clips_peaks() {
        let build = |drive: f32| {
            patch(
                vec![
                    osc("saw", OscWave::Saw, 3, 12.0, 1.0),
                    shaper("ws", drive),
                    out("out", 1.0),
                ],
                vec![audio("saw", "ws"), audio("ws", "out")],
            )
        };
        let mut clean = PatchVoice::from_patch(SR, &build(0.0)).expect("compile");
        clean.note_on(60.0, 1.0);
        let a = render_tiles(&mut clean, 8);
        let mut hot = PatchVoice::from_patch(SR, &build(1.0)).expect("compile");
        hot.note_on(60.0, 1.0);
        let b = render_tiles(&mut hot, 8);
        let (pa, pb) = (peak(&a), peak(&b));
        // Driven saw peaks are folded under the un-driven ones.
        assert!(pb < pa, "drive must clip peaks: clean {pa} vs driven {pb}");
        assert!(b.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn formant_bank_colors_and_distinguishes_vowels() {
        let build = |vowel: FormantVowel| {
            patch(
                vec![
                    osc("saw", OscWave::Saw, 1, 0.0, 1.0),
                    formant("f", vowel, 1.0),
                    out("out", 1.0),
                ],
                vec![audio("saw", "f"), audio("f", "out")],
            )
        };
        let mut ah = PatchVoice::from_patch(SR, &build(FormantVowel::Ah)).expect("compile");
        ah.note_on(60.0, 1.0);
        let ra = render_tiles(&mut ah, 8);
        let mut oo = PatchVoice::from_patch(SR, &build(FormantVowel::Oo)).expect("compile");
        oo.note_on(60.0, 1.0);
        let ro = render_tiles(&mut oo, 8);
        // Band-passed output is audible but quieter than the input saw, and
        // the vowel tables are distinguishable.
        assert!(peak(&ra) > 0.01, "formant output silent");
        assert!(rms(&ra) < 0.9, "formant must band-limit the saw");
        assert!(
            ra.iter().zip(ro.iter()).any(|(x, y)| x.to_bits() != y.to_bits()),
            "vowels must differ"
        );
    }

    #[test]
    fn sampler_plays_bank_pcm_and_mutes_missing_slots() {
        // A 0.25 s sine at C3 in slot 7.
        let n = SR as usize / 4;
        let pcm: Arc<[f32]> = (0..n)
            .map(|i| 0.8 * (TAU * midi_to_hz(60.0) * i as f32 / SR as f32).sin())
            .collect();
        let mut bank = SampleBank::new();
        bank.insert(7, pcm);
        let bank = Arc::new(bank);

        let build = |slot: u32| {
            patch(
                vec![sampler("s", slot), out("out", 1.0)],
                vec![audio("s", "out")],
            )
        };
        let mut v = PatchVoice::from_patch_with_bank(SR, &build(7), Arc::clone(&bank)).expect("compile");
        v.note_on(60.0, 1.0);
        let head = render_tiles(&mut v, 16); // ~21 ms: inside the sample
        assert!(peak(&head) > 0.05, "sampler head silent");
        // The sample ends after 0.25 s; the voice retires itself afterwards.
        let mut blocks = 0;
        while v.is_active() && blocks < 1_000 {
            let mut buf = [0.0f32; BLOCK_FRAMES];
            v.render(&mut buf);
            blocks += 1;
        }
        assert!(!v.is_active(), "one-shot sample must retire the voice");

        // A slot the bank does not have mutes the node, not the patch.
        let mut missing = PatchVoice::from_patch_with_bank(SR, &build(9), bank).expect("compile");
        missing.note_on(60.0, 1.0);
        let out = render_tiles(&mut missing, 8);
        assert!(out.iter().all(|&s| s == 0.0), "missing slot must be muted");

        // No bank at all (from_patch): same muted behavior.
        let mut nobank = PatchVoice::from_patch(SR, &build(7)).expect("compile");
        nobank.note_on(60.0, 1.0);
        assert!(render_tiles(&mut nobank, 8).iter().all(|&s| s == 0.0));
    }

    #[test]
    fn new_node_patches_stay_bit_deterministic_over_10s() {
        let ring_patch = || {
            patch(
                vec![
                    osc("o1", OscWave::Square, 1, 0.0, 0.6),
                    osc("c1", OscWave::Tri, 1, 0.0, 0.6),
                    ring("rm", 0.9),
                    env("e", 2.0, 200.0, 0.8, 300.0),
                    gain("vca", 0.0),
                    out("out", 0.9),
                ],
                vec![
                    audio("o1", "rm"),
                    audio_socket("c1", "rm", RING_CARRIER_SOCKET),
                    audio("rm", "vca"),
                    audio("vca", "out"),
                    mod_edge("e", "vca", "level", 1.0),
                ],
            )
        };
        assert_golden(ring_patch, "ring metallic");
    }
}
