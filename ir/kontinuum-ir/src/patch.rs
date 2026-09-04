//! Patch-graph schema for `InstrumentDef::Custom` (issue #37): a validated
//! modular patch the LLM (and Producer mode) can author as data.
//!
//! Tagging follows the crate convention (see `schema` module docs): the node
//! enum is untagged and every node struct carries an explicit `"type"`
//! discriminant with `deny_unknown_fields`, so a hallucinated node kind or
//! field fails at parse time.
//!
//! Signal model v1:
//! - audio edges (`"type": "audio"`) route audio-rate signal; every audio
//!   input is an implicit sum, so a `gain` node doubles as a mixer.
//! - mod edges (`"type": "mod"`) route control-rate signal (env/LFO) into a
//!   single named parameter of the target node at `amount` depth.
//! - audio edges may name an input socket via `param`: only `ring` has a
//!   second socket (`"carrier"`); unnamed audio edges feed the default sum.
//! - feedback is allowed ONLY through `delay` nodes: a loop may close through
//!   a delay's output (delay → … → back into the loop path); the compiler
//!   breaks each loop at that outgoing edge and the evaluator applies the tap
//!   one block late. Any cycle bypassing every delay is rejected at
//!   validation.
//!
//! allow: SIZE_OK — pure serde data table and semantic predicates on it; the
//! shape is pinned by the issue #37 IR contract.

use serde::{Deserialize, Serialize};

/// `{"kind": "custom"}` discriminant for [`CustomPatch`] (crate tagging
/// convention: unit-only enum, snake_case).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CustomTag {
    Custom,
}

/// `InstrumentDef::Custom` payload: a named patch graph.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomPatch {
    pub kind: CustomTag,
    #[serde(default)]
    pub patch: PatchGraph,
}

/// Nodes + edges. Empty graph parses but never validates (no `out` node).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchGraph {
    #[serde(default)]
    pub nodes: Vec<PatchNode>,
    #[serde(default)]
    pub edges: Vec<PatchEdge>,
}

/// One modular node. Untagged; the inner `"type"` discriminant picks the
/// variant and `deny_unknown_fields` keeps param sets strict per kind.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PatchNode {
    Osc(OscNode),
    FmPair(FmPairNode),
    Filter(FilterNode),
    Env(EnvNode),
    Lfo(LfoNode),
    Gain(GainNode),
    Delay(DelayNode),
    Ring(RingNode),
    Shaper(ShaperNode),
    Formant(FormantNode),
    Sampler(SamplerNode),
    Out(OutNode),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OscTag {
    Osc,
}

/// Pitched oscillator (pitch comes from the note, not the patch). Wave
/// vocabulary extends the existing `Wave` (saw/square) with sine/tri/noise.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OscNode {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: OscTag,
    #[serde(default = "d_osc_wave")]
    pub wave: OscWave,
    /// Unison voice count 1..=7 (bounds::UNISON).
    #[serde(default = "d_unison")]
    pub unison: u8,
    /// Static per-voice spread in cents (bounds::PAD_DETUNE_CENTS).
    #[serde(default)]
    pub fine_cents: f32,
    /// Output level 0..=1 (bounds::UNIT).
    #[serde(default = "d_node_level")]
    pub level: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OscWave {
    #[default]
    Saw,
    Square,
    Sine,
    Tri,
    Noise,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FmPairTag {
    FmPair,
}

/// Self-contained 2-operator FM pair (modulator → carrier, both sine): the
/// v1 metallic/bell workhorse. `ratio`/`index` in bounds::FM_RATIO/FM_INDEX,
/// `feedback` in 0..=1.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FmPairNode {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: FmPairTag,
    #[serde(default = "d_fm_ratio")]
    pub ratio: f32,
    #[serde(default = "d_fm_index")]
    pub index: f32,
    #[serde(default)]
    pub feedback: f32,
    #[serde(default = "d_node_level")]
    pub level: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterTag {
    Filter,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterMode {
    #[default]
    LowPass,
    HighPass,
    BandPass,
}

/// Resonant filter: `cutoff_hz` in bounds::PATCH_CUTOFF_HZ, `resonance` and
/// `drive` in 0..=1. Mod-able param: `cutoff_hz`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilterNode {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: FilterTag,
    #[serde(default)]
    pub mode: FilterMode,
    #[serde(default = "d_filter_cutoff")]
    pub cutoff_hz: f32,
    #[serde(default = "d_resonance")]
    pub resonance: f32,
    #[serde(default)]
    pub drive: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvTag {
    Env,
}

/// ADSR envelope, retriggered per note. Control-rate source; no audio input.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvNode {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: EnvTag,
    #[serde(default = "d_env_attack")]
    pub attack_ms: f32,
    #[serde(default = "d_env_decay")]
    pub decay_ms: f32,
    #[serde(default)]
    pub sustain: f32,
    #[serde(default = "d_env_release")]
    pub release_ms: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LfoTag {
    Lfo,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LfoWave {
    #[default]
    Sine,
    Tri,
    Square,
}

/// Free-running LFO: `rate_hz` in bounds::LFO_RATE_HZ, `depth` in 0..=1.
/// Tempo-synced LFOs are an engine-side concern (core maps bpm → rate).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LfoNode {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: LfoTag,
    #[serde(default = "d_lfo_rate")]
    pub rate_hz: f32,
    #[serde(default = "d_lfo_depth")]
    pub depth: f32,
    #[serde(default)]
    pub wave: LfoWave,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GainTag {
    Gain,
}

/// Gain/mix stage: scales the implicit sum of its incoming audio edges by
/// `level` (bounds::GAIN, so it can also attenuate to silence).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GainNode {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: GainTag,
    #[serde(default = "d_gain_level")]
    pub level: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelayTag {
    Delay,
}

/// The ONLY feedback-capable node: its output may be routed back into its own
/// input (directly or through downstream nodes) to close a delay loop.
/// `feedback` ceiling bounds::DELAY_FEEDBACK (0..=0.95) keeps loops stable.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelayNode {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: DelayTag,
    #[serde(default = "d_delay_time")]
    pub time_ms: f32,
    #[serde(default = "d_delay_feedback")]
    pub feedback: f32,
    #[serde(default)]
    pub mix: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RingTag {
    Ring,
}

/// Ring modulator / signal multiplier: output = program × carrier × level.
/// Audio edges name their socket via `param`: omitted = program input (the
/// implicit sum), `"carrier"` = carrier input (also an implicit sum). A ring
/// with no incoming carrier edge is rejected at validation
/// (E_PATCH_RING_NO_CARRIER) — silent output is never clamped away.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RingNode {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: RingTag,
    #[serde(default = "d_node_level")]
    pub level: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShaperTag {
    Shaper,
}

/// Waveshaper: tanh soft-clip normalized to unity small-signal gain.
/// `drive` 0..=1 (bounds::UNIT) sets the overdrive; mod-able param: `drive`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShaperNode {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: ShaperTag,
    #[serde(default)]
    pub drive: f32,
    #[serde(default = "d_node_level")]
    pub level: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FormantTag {
    Formant,
}

/// Vowel formants (F1/F2/F3 in Hz), classic single-vowel approximations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FormantVowel {
    #[default]
    Ah,
    Eh,
    Ee,
    Oh,
    Oo,
}

impl FormantVowel {
    /// (F1, F2, F3) in Hz.
    pub fn formants(self) -> [f32; 3] {
        match self {
            FormantVowel::Ah => [800.0, 1150.0, 2900.0],
            FormantVowel::Eh => [550.0, 1750.0, 2600.0],
            FormantVowel::Ee => [350.0, 2100.0, 2800.0],
            FormantVowel::Oh => [450.0, 850.0, 2830.0],
            FormantVowel::Oo => [325.0, 700.0, 2530.0],
        }
    }
}

/// Three-band parallel formant bank: colors the incoming audio with a vowel.
/// `shift` (bounds::FORMANT_SHIFT) scales every formant frequency;
/// mod-able param: `shift`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormantNode {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: FormantTag,
    #[serde(default)]
    pub vowel: FormantVowel,
    #[serde(default = "d_formant_shift")]
    pub shift: f32,
    #[serde(default = "d_node_level")]
    pub level: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SamplerTag {
    Sampler,
}

/// Sample-slot reference (#19): plays the PCM the host installs into the
/// patch's sample bank under `slot`, transposed from C3 by the note pitch.
/// Slot availability is runtime state (the host's sample library), so the IR
/// carries no numeric ceiling — a missing slot mutes the node, never the
/// patch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SamplerNode {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: SamplerTag,
    pub slot: u32,
    #[serde(default = "d_node_level")]
    pub level: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutTag {
    Out,
}

/// Terminal sink. Exactly one per patch; audio inputs sum into it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutNode {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: OutTag,
    #[serde(default = "d_node_level")]
    pub level: f32,
}

/// One routing edge. `amount` is a gain for audio edges (bounds::GAIN) and a
/// modulation depth for mod edges (bounds::UNIT); `param` names the target
/// parameter of mod edges and must be one of the target's mod-able params.
/// For audio edges `param` names the target's input socket — currently only
/// `ring` has a second socket (`"carrier"`); omit it everywhere else.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchEdge {
    pub from: String,
    pub to: String,
    #[serde(rename = "type")]
    pub kind: EdgeKind,
    #[serde(default)]
    pub param: Option<String>,
    #[serde(default = "d_edge_amount")]
    pub amount: f32,
}

/// Audio-edge input socket of a `ring` node.
pub const RING_CARRIER_SOCKET: &str = "carrier";

#[derive(Clone, Copy, Debug, PartialOrd, Ord, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Audio,
    Mod,
}

impl PatchNode {
    pub fn id(&self) -> &str {
        match self {
            PatchNode::Osc(n) => &n.id,
            PatchNode::FmPair(n) => &n.id,
            PatchNode::Filter(n) => &n.id,
            PatchNode::Env(n) => &n.id,
            PatchNode::Lfo(n) => &n.id,
            PatchNode::Gain(n) => &n.id,
            PatchNode::Delay(n) => &n.id,
            PatchNode::Ring(n) => &n.id,
            PatchNode::Shaper(n) => &n.id,
            PatchNode::Formant(n) => &n.id,
            PatchNode::Sampler(n) => &n.id,
            PatchNode::Out(n) => &n.id,
        }
    }

    /// Stable kind name for error messages.
    pub fn kind_name(&self) -> &'static str {
        match self {
            PatchNode::Osc(_) => "osc",
            PatchNode::FmPair(_) => "fm_pair",
            PatchNode::Filter(_) => "filter",
            PatchNode::Env(_) => "env",
            PatchNode::Lfo(_) => "lfo",
            PatchNode::Gain(_) => "gain",
            PatchNode::Delay(_) => "delay",
            PatchNode::Ring(_) => "ring",
            PatchNode::Shaper(_) => "shaper",
            PatchNode::Formant(_) => "formant",
            PatchNode::Sampler(_) => "sampler",
            PatchNode::Out(_) => "out",
        }
    }

    /// Node emits audio-rate signal on its output.
    pub fn produces_audio(&self) -> bool {
        matches!(
            self,
            PatchNode::Osc(_)
                | PatchNode::FmPair(_)
                | PatchNode::Filter(_)
                | PatchNode::Gain(_)
                | PatchNode::Delay(_)
                | PatchNode::Ring(_)
                | PatchNode::Shaper(_)
                | PatchNode::Formant(_)
                | PatchNode::Sampler(_)
        )
    }

    /// Node accepts audio-rate input (implicit sum of incoming audio edges).
    pub fn accepts_audio(&self) -> bool {
        matches!(
            self,
            PatchNode::Filter(_)
                | PatchNode::Gain(_)
                | PatchNode::Delay(_)
                | PatchNode::Ring(_)
                | PatchNode::Shaper(_)
                | PatchNode::Formant(_)
                | PatchNode::Out(_)
        )
    }

    /// Node emits control-rate signal usable as a mod source.
    pub fn is_mod_source(&self) -> bool {
        matches!(self, PatchNode::Env(_) | PatchNode::Lfo(_))
    }

    /// Node is the feedback-capable delay.
    pub fn is_delay(&self) -> bool {
        matches!(self, PatchNode::Delay(_))
    }

    /// Audio-edge input sockets beyond the default (unnamed) one.
    pub fn audio_sockets(&self) -> &'static [&'static str] {
        match self {
            PatchNode::Ring(_) => &[RING_CARRIER_SOCKET],
            _ => &[],
        }
    }

    /// Params a mod edge may target on this node kind.
    pub fn mod_targets(&self) -> &'static [&'static str] {
        match self {
            PatchNode::Osc(_) => &["fine_cents"],
            PatchNode::FmPair(_) => &["index"],
            PatchNode::Filter(_) => &["cutoff_hz"],
            PatchNode::Gain(_) => &["level"],
            PatchNode::Delay(_) => &["time_ms"],
            PatchNode::Ring(_) => &["level"],
            PatchNode::Shaper(_) => &["drive"],
            PatchNode::Formant(_) => &["shift"],
            PatchNode::Env(_) | PatchNode::Lfo(_) | PatchNode::Sampler(_) | PatchNode::Out(_) => &[],
        }
    }
}

// -- Defaults (schema.rs convention: sits inside declared bounds) -----------

fn d_osc_wave() -> OscWave {
    OscWave::Saw
}
fn d_unison() -> u8 {
    1
}
fn d_node_level() -> f32 {
    1.0
}
fn d_fm_ratio() -> f32 {
    1.0
}
fn d_fm_index() -> f32 {
    2.0
}
fn d_filter_cutoff() -> f32 {
    3000.0
}
fn d_resonance() -> f32 {
    0.2
}
fn d_env_attack() -> f32 {
    5.0
}
fn d_env_decay() -> f32 {
    300.0
}
fn d_env_release() -> f32 {
    300.0
}
fn d_lfo_rate() -> f32 {
    1.0
}
fn d_lfo_depth() -> f32 {
    1.0
}
fn d_gain_level() -> f32 {
    1.0
}
fn d_delay_time() -> f32 {
    250.0
}
fn d_delay_feedback() -> f32 {
    0.4
}
fn d_formant_shift() -> f32 {
    1.0
}
fn d_edge_amount() -> f32 {
    1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_node_roundtrip_and_discriminants() {
        let json = r#"{"id": "o1", "type": "osc", "wave": "sine", "unison": 3}"#;
        let n: PatchNode = serde_json::from_str(json).expect("parse");
        assert_eq!(n.id(), "o1");
        assert_eq!(n.kind_name(), "osc");
        let out = serde_json::to_string(&n).expect("serialize");
        let n2: PatchNode = serde_json::from_str(&out).expect("reparse");
        assert_eq!(n, n2);
    }

    #[test]
    fn unknown_node_type_rejected() {
        assert!(serde_json::from_str::<PatchNode>(r#"{"id": "x", "type": "gong"}"#).is_err());
        assert!(serde_json::from_str::<PatchNode>(r#"{"id": "x", "type": "osc", "bogus": 1}"#).is_err());
    }

    #[test]
    fn defaults_satisfy_bounds() {
        use crate::schema::bounds;
        assert!((bounds::UNISON.0..=bounds::UNISON.1).contains(&d_unison()));
        let r = bounds::FM_RATIO;
        assert!(d_fm_ratio() >= r.0 && d_fm_ratio() <= r.1);
        let r = bounds::FM_INDEX;
        assert!(d_fm_index() >= r.0 && d_fm_index() <= r.1);
        let r = bounds::PATCH_CUTOFF_HZ;
        assert!(d_filter_cutoff() >= r.0 && d_filter_cutoff() <= r.1);
        let r = bounds::LFO_RATE_HZ;
        assert!(d_lfo_rate() >= r.0 && d_lfo_rate() <= r.1);
        let r = bounds::DELAY_TIME_MS;
        assert!(d_delay_time() >= r.0 && d_delay_time() <= r.1);
        let r = bounds::DELAY_FEEDBACK;
        assert!(d_delay_feedback() >= r.0 && d_delay_feedback() <= r.1);
    }

    #[test]
    fn signal_predicates() {
        let osc: PatchNode = serde_json::from_str(r#"{"id": "o", "type": "osc"}"#).expect("osc");
        let env: PatchNode = serde_json::from_str(r#"{"id": "e", "type": "env"}"#).expect("env");
        let out: PatchNode = serde_json::from_str(r#"{"id": "x", "type": "out"}"#).expect("out");
        let dly: PatchNode = serde_json::from_str(r#"{"id": "d", "type": "delay"}"#).expect("dly");
        let ring: PatchNode = serde_json::from_str(r#"{"id": "r", "type": "ring"}"#).expect("ring");
        let shaper: PatchNode =
            serde_json::from_str(r#"{"id": "s", "type": "shaper", "drive": 0.5}"#).expect("shaper");
        let formant: PatchNode =
            serde_json::from_str(r#"{"id": "f", "type": "formant", "vowel": "oo"}"#).expect("formant");
        let sampler: PatchNode =
            serde_json::from_str(r#"{"id": "s2", "type": "sampler", "slot": 3}"#).expect("sampler");
        assert!(osc.produces_audio() && !osc.accepts_audio() && !osc.is_mod_source());
        assert!(env.is_mod_source() && !env.produces_audio() && !env.accepts_audio());
        assert!(out.accepts_audio() && out.mod_targets().is_empty());
        assert!(dly.is_delay() && dly.accepts_audio() && dly.produces_audio());
        assert_eq!(dly.mod_targets(), &["time_ms"]);
        assert!(ring.accepts_audio() && ring.produces_audio());
        assert_eq!(ring.audio_sockets(), &["carrier"]);
        assert_eq!(shaper.mod_targets(), &["drive"]);
        assert!(formant.accepts_audio() && formant.produces_audio());
        assert_eq!(formant.mod_targets(), &["shift"]);
        assert!(sampler.produces_audio() && !sampler.accepts_audio());
        assert!(sampler.mod_targets().is_empty());
    }

    #[test]
    fn new_node_kinds_roundtrip_and_reject_unknown_fields() {
        let ring: PatchNode = serde_json::from_str(r#"{"id": "r", "type": "ring", "level": 0.8}"#).expect("ring");
        assert_eq!(serde_json::to_string(&ring).unwrap(), r#"{"id":"r","type":"ring","level":0.8}"#);
        assert!(serde_json::from_str::<PatchNode>(r#"{"id": "r", "type": "ring", "gain": 1.0}"#).is_err());
        let formant: PatchNode = serde_json::from_str(
            r#"{"id": "f", "type": "formant", "vowel": "ee", "shift": 1.25, "level": 0.9}"#,
        )
        .expect("formant");
        let out = serde_json::to_string(&formant).expect("serialize");
        assert_eq!(formant, serde_json::from_str(&out).expect("reparse"));
        assert!(
            serde_json::from_str::<PatchNode>(r#"{"id": "f", "type": "formant", "vowel": "q"}"#).is_err(),
            "closed vowel enum rejects hallucinated vowels"
        );
        let sh: PatchNode = serde_json::from_str(r#"{"id": "w", "type": "shaper"}"#).expect("shaper");
        assert_eq!(sh.kind_name(), "shaper");
        let sm: PatchNode = serde_json::from_str(r#"{"id": "s", "type": "sampler", "slot": 7}"#).expect("sampler");
        assert_eq!(sm.kind_name(), "sampler");
    }

    #[test]
    fn defaults_satisfy_new_bounds() {
        use crate::schema::bounds;
        let formant: PatchNode = serde_json::from_str(r#"{"id": "f", "type": "formant"}"#).expect("formant");
        let PatchNode::Formant(f) = formant else { panic!("formant") };
        assert!(f32_in_bounds(f.shift, bounds::FORMANT_SHIFT));
        let shaper: PatchNode = serde_json::from_str(r#"{"id": "w", "type": "shaper"}"#).expect("shaper");
        let PatchNode::Shaper(s) = shaper else { panic!("shaper") };
        assert!(f32_in_bounds(s.drive, bounds::UNIT) && f32_in_bounds(s.level, bounds::UNIT));
    }

    fn f32_in_bounds(v: f32, r: (f32, f32)) -> bool {
        v.is_finite() && v >= r.0 && v <= r.1
    }
}
