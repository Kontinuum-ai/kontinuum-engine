//! Musical IR schema (issue #11) — the only contract between the AI composer
//! and the engine.
//!
//! L1 strictness: every struct uses [`serde::deny_unknown_fields`] so a
//! hallucinated field fails at parse time with a precise serde error.
//!
//! Tagging note: serde cannot combine internally-tagged enums with
//! `deny_unknown_fields`, so variant discrimination uses an explicit
//! `"kind"`/`"type"`/`"generator"` discriminant field (a unit-only enum) inside
//! untagged wrappers. This keeps strictness *and* clean JSON.
//!
//! Numeric bounds are declared here (L2 lint lives in `validate`); serde cannot
//! express open ranges, so bounds are checked by `validate_session`.
//!
//! allow: SIZE_OK — this file's location and contents are pinned by issue #11;
//! it is a pure data table of serde type declarations, bounds constants, and
//! default-value functions with no logic to review.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub use crate::jsonschema::export_json_schema;

/// Documented numeric bounds for every validated field.
pub mod bounds {
    pub const KICK_TUNE_HZ: (f32, f32) = (30.0, 120.0);
    pub const KICK_DECAY_MS: (f32, f32) = (50.0, 1500.0);
    pub const HAT_DECAY_MS: (f32, f32) = (5.0, 2000.0);
    pub const BASS_CUTOFF_HZ: (f32, f32) = (40.0, 8000.0);
    pub const BASS_GLIDE_MS: (f32, f32) = (0.0, 1000.0);
    pub const PAD_ATTACK_MS: (f32, f32) = (1.0, 10000.0);
    pub const PAD_RELEASE_MS: (f32, f32) = (10.0, 20000.0);
    pub const PAD_CUTOFF_HZ: (f32, f32) = (40.0, 16000.0);
    pub const PAD_DETUNE_CENTS: (f32, f32) = (-100.0, 100.0);
    pub const UNIT: (f32, f32) = (0.0, 1.0);
    pub const GAIN: (f32, f32) = (0.0, 2.0);
    pub const PAN: (f32, f32) = (-1.0, 1.0);
    pub const GATE_BEATS: (f32, f32) = (0.01, 64.0);
    pub const MICROTIMING_TICKS: (i16, i16) = (-120, 120);
    pub const RATCHET: (u8, u8) = (1, 8);
    pub const REPEATS: (u32, u32) = (1, 64);
    /// Sample-slot tuning (issue #19 v1). Choke groups match the RT
    /// engine's [`kontinuum_core::voice::CHOKE_GROUPS`]; stretch/grain
    /// ranges mirror the sample-pack bounds (`kontinuum_samples::schema`).
    pub const SAMPLE_TRANSPOSE: (f32, f32) = (-36.0, 36.0);
    pub const SAMPLE_FINE: (f32, f32) = (-100.0, 100.0);
    pub const SAMPLE_STRETCH: (f32, f32) = (0.25, 4.0);
    pub const SAMPLE_CHOKE_GROUP: (u8, u8) = (1, 16);
    pub const SAMPLE_GRAIN_MS: (f32, f32) = (20.0, 200.0);
    /// Sound roster v2 (#30). The ranges mirror the voice-side clamps in
    /// `kontinuum-instruments-core` (wavetable / FM perc / texture) so a
    /// validated document never gets re-clamped at the voice.
    pub const WAV_POSITION: (f32, f32) = (0.0, 1.0);
    pub const WAV_DETUNE_CENTS: (f32, f32) = (0.0, 50.0);
    pub const WAV_CUTOFF_HZ: (f32, f32) = (100.0, 12_000.0);
    pub const WAV_RELEASE_MS: (f32, f32) = (20.0, 8_000.0);
    /// FM-percussion carrier ratio ceiling is tighter than the patch-level
    /// FM pair (percussion ratios stay musical).
    pub const FM_PERC_RATIO: (f32, f32) = (0.25, 8.0);
    pub const FM_DECAY_MS: (f32, f32) = (20.0, 3_000.0);
    pub const TEXTURE_DENSITY: (f32, f32) = (0.0, 0.05);
    pub const TEXTURE_GRAIN_MS: (f32, f32) = (2.0, 200.0);
    pub const SAMPLE_GRAIN_DENSITY: (f32, f32) = (1.0, 200.0);
    pub const SAMPLE_GRAIN_SPRAY_MS: (f32, f32) = (0.0, 1000.0);
    pub const SAMPLE_GRAIN_PITCH_JITTER: (f32, f32) = (0.0, 1200.0);
    pub const SAMPLE_GRAIN_LEVEL: (f32, f32) = (0.0, 1.0);
    pub const INSERTS_PER_TRACK: usize = 2;
    /// Validation ceiling on session length; guards the dry-run compiler.
    pub const MAX_SESSION_BARS: u64 = 4096;
    /// Live tempo-move range (issue #38 step 1): the same window the
    /// arrangement generator clamps `GenParams::bpm` to.
    pub const LIVE_BPM: (f64, f64) = (60.0, 200.0);
    /// Maximum tracks (TrackId is u8).
    pub const MAX_TRACKS: usize = 255;
    /// Euclidean pattern resolution cap (16th slots per phrase).
    pub const EUCLID_MAX_N: u32 = 4096;
    /// Kick-sidechain duck release τ (ms) — mirrors the engine's setter
    /// clamp (`kontinuum-core` `DUCK_RELEASE_MIN/MAX_MS`).
    pub const DUCK_RELEASE_MS: (f32, f32) = (20.0, 1_000.0);

    // -- Pattern-engine state (issue #17) -----------------------------------
    /// Swing as the odd-16th delay fraction of one 16th (0 = straight time,
    /// 0.5 = triplet feel). The hand-made groove vocabulary and the genre
    /// swing ranges stay well inside this.
    pub const GROOVE_SWING: (f32, f32) = (0.0, 0.5);
    /// Per-track microtiming push/pull bias in ticks at PPQ 960 (−12 = pull,
    /// +12 = push). The hand-made vocabulary ships within this window.
    pub const GROOVE_BIAS_TICKS: (i16, i16) = (-12, 12);
    /// Per-step timing jitter σ in ticks. The hand-made grooves ship
    /// 1..=4; this is also the recorded envelope for corpus fits.
    pub const GROOVE_JITTER_TICKS: (f32, f32) = (1.0, 4.0);

    // -- Patch graph (issue #37) --------------------------------------------
    /// Max nodes per custom patch (issue #37 ceiling): CPU guard rail — a
    /// patch is ONE voice, and the per-voice budget on the oldest supported
    /// device allows 24 nodes.
    pub const MAX_PATCH_NODES: usize = 24;
    /// Max edges per custom patch: CPU guard rail paired with MAX_PATCH_NODES
    /// (a fully wired 16-node patch stays far below this; the cap exists so a
    /// hallucinated mod matrix is rejected, not clamped silently).
    pub const MAX_PATCH_EDGES: usize = 32;
    /// Oscillator unison voice count.
    pub const UNISON: (u8, u8) = (1, 7);
    /// FM pair modulator:carrier ratio.
    pub const FM_RATIO: (f32, f32) = (0.25, 16.0);
    /// FM pair modulation index.
    pub const FM_INDEX: (f32, f32) = (0.0, 8.0);
    /// Patch filter cutoff (wider than the fixed instruments: patches may
    /// sweep the full audible band).
    pub const PATCH_CUTOFF_HZ: (f32, f32) = (20.0, 20_000.0);
    /// Patch envelope decay stage.
    pub const ENV_DECAY_MS: (f32, f32) = (1.0, 10_000.0);
    /// LFO rate in Hz (audio-rate LFOs are out of scope for the CPU guard).
    pub const LFO_RATE_HZ: (f32, f32) = (0.01, 40.0);
    /// Feedback delay line time.
    pub const DELAY_TIME_MS: (f32, f32) = (1.0, 2000.0);
    /// Delay feedback ceiling (< 1.0): keeps every loop stable by
    /// construction — a stability and CPU guard, not a taste bound.
    pub const DELAY_FEEDBACK: (f32, f32) = (0.0, 0.95);
    /// Formant frequency shift multiplier (0.5 = one octave down).
    pub const FORMANT_SHIFT: (f32, f32) = (0.5, 2.0);
}

// -- Session ---------------------------------------------------------------

/// Root document. All fields strict; unknown fields are a parse error.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    /// Must equal [`crate::IR_VERSION`] (currently 1).
    pub version: u32,
    /// Master seed for every derived randomness stream downstream.
    pub seed: u64,
    /// Tempo automation as `[bar, bpm]` pairs; bar 0 anchor required, bars
    /// strictly ascending, bpm finite and > 0.
    pub tempo_lane: Vec<(u32, f64)>,
    /// Optional musical key hint, e.g. "A minor".
    #[serde(default)]
    pub key: Option<String>,
    pub sections: Vec<Section>,
    pub tracks: Vec<Track>,
    /// Free-form style palette; opaque to the engine.
    #[serde(default)]
    pub palette: Option<serde_json::Value>,
    /// Creative Soul stack (issue #55): blendable identity packs this
    /// session is steered by, weight-ordered by the blender. Structural
    /// rules are checked in L1; the packs themselves live outside the IR.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub souls: Option<Vec<SoulRef>>,
    /// Kick-sidechain duck release τ in ms (issue #76): the groove/genre
    /// template's pump time. One value per session — the engine retimes
    /// every track's duck from it. Defaults to the engine's 160 ms
    /// (≈ one beat at 120–126 BPM) so pre-#76 sessions render unchanged.
    #[serde(default = "d_duck_release_ms")]
    pub duck_release_ms: f32,
    /// Pattern-generator state (issue #17): the groove bundle, bass idiom
    /// and kick/bass collision policy the session was generated with, so
    /// the LLM (and the app) can read and drive them directly. `None` on
    /// pre-#17 sessions; the generator always records it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern_engine: Option<PatternEngine>,
    /// Send-bus flavors (#30): opt into reverb v2 / tape delay. Absent or
    /// `None` fields keep the classic buses — every pre-#30 document
    /// renders bit-identically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_fx: Option<SendFxSpec>,
}

/// Named bus flavors with curated fixed color (param automation onto the
/// shared buses is a later seam; the flavors are the v1 surface).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SendFxSpec {
    /// "fdn8" — the modulated 8-line FDN reverb v2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverb: Option<ReverbFlavor>,
    /// "tape" — the delay bus in tape mode (wow 0.4 / flutter 0.4 / sat 0.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay: Option<DelayFlavor>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReverbFlavor {
    Fdn8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelayFlavor {
    Tape,
}

/// How the bass may sit against the kick's downbeats (issue #17).
/// `avoid` keeps bass onsets off kick positions, `allow` leaves them
/// stacked, `duck_only` leaves placement to the sidechain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownbeatCollision {
    Avoid,
    Allow,
    DuckOnly,
}

impl Default for DownbeatCollision {
    fn default() -> Self {
        DownbeatCollision::DuckOnly
    }
}

/// The resolved pattern-generator parameters for one session (issue #17).
/// Every field defaults so `{}` round-trips; the generator records the
/// values it actually drew, clamped into the schema envelope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatternEngine {
    /// Groove-template name: one of the hand-made six
    /// (`straight-machine`, `mpc-ish`, `drunk-shuffle`, `pushed-hats`,
    /// `laid-back`, `tense`) or a corpus-extracted name (#23).
    #[serde(default)]
    pub groove: Option<String>,
    /// Swing on the 16th grid, 0..=0.5.
    #[serde(default)]
    pub swing: f32,
    /// Per-track microtiming push/pull bias, ticks at PPQ 960.
    #[serde(default)]
    pub bias_ticks: i16,
    /// Per-step timing jitter σ in ticks.
    #[serde(default = "d_groove_jitter")]
    pub jitter_ticks: f32,
    /// Bass archetype name (see the compose crate's bass vocabulary).
    #[serde(default)]
    pub bass_archetype: Option<String>,
    /// How the bass may sit against the kick.
    #[serde(default)]
    pub downbeat_collision: DownbeatCollision,
}

/// One Creative Soul entry in [`Session::souls`] (issue #55): which pack,
/// how loud it speaks in the blend, and which named era/phase of the pack
/// is active (packs may ship several; `None` = the pack's "default" era).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SoulRef {
    /// Pack id, e.g. "detroit-909-minimalism".
    pub id: String,
    /// Blend weight, 0 < w <= 1 (need not sum to 1; the blender normalizes).
    pub weight: f32,
    /// Named era inside the pack; omitted means the default era.
    #[serde(default)]
    pub era: Option<String>,
}

impl Session {
    /// Total session length in bars.
    pub fn total_bars(&self) -> u64 {
        self.sections.iter().map(|s| s.bars as u64).sum()
    }

    /// Start bar of each section, in document order (sections chain back to
    /// back from bar 0).
    pub fn section_start_bars(&self) -> Vec<u32> {
        let mut out = Vec::with_capacity(self.sections.len());
        let mut acc = 0u32;
        for s in &self.sections {
            out.push(acc);
            acc = acc.saturating_add(s.bars);
        }
        out
    }
}

// -- Section ---------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Section {
    pub id: String,
    /// Length in bars; must be >= 1.
    pub bars: u32,
    /// Energy arc, one or more values in 0..=1.
    #[serde(default)]
    pub energy_curve: Vec<f32>,
    /// Event-count arc, one or more values in 0..=1 (#16): the density the
    /// pattern layer budgets against. Empty = not planned; consumers fall
    /// back to the energy curve.
    #[serde(default)]
    pub density_curve: Vec<f32>,
    /// Spectral arc, one or more values in 0..=1 (#16): filter-openness
    /// and tonal-colour bias. Empty = not planned; consumers fall back to
    /// the energy curve.
    #[serde(default)]
    pub brightness_curve: Vec<f32>,
    #[serde(default)]
    pub transition_in: Option<Transition>,
    #[serde(default)]
    pub transition_out: Option<Transition>,
    /// Track id -> pattern for this section. Tracks without a binding are
    /// silent here; a section where *every* track is silent is rejected.
    #[serde(default)]
    pub pattern_bindings: BTreeMap<String, Pattern>,
    /// Track id -> automation lane (deviation from the issue text: lanes need
    /// a home keyed by (section, track) to make `SetAutomation` well-defined).
    #[serde(default)]
    pub automation: BTreeMap<String, AutomationLane>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transition {
    #[serde(rename = "type")]
    pub kind: TransitionKind,
    pub bars: u32,
    /// Transition-specific parameters, opaque to the engine core.
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionKind {
    FilterSweep,
    MuteChoreo,
    Fill,
    SilenceDrop,
    Riser,
    /// Reverb-throw tail into a breakdown (#16 transition catalog): the
    /// send freezes while the dry signal exits.
    ReverbThrow,
}

/// Musical key for live key moves (issue #38 step 1): a stable closed set of
/// the 12 chromatic tonics × major/minor. Serde tags are snake_case
/// (`"f_minor"`); unknown keys fail at parse time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MusicalKey {
    CMajor,
    CSharpMajor,
    DMajor,
    DSharpMajor,
    EMajor,
    FMajor,
    FSharpMajor,
    GMajor,
    GSharpMajor,
    AMajor,
    ASharpMajor,
    BMajor,
    CMinor,
    CSharpMinor,
    DMinor,
    DSharpMinor,
    EMinor,
    FMinor,
    FSharpMinor,
    GMinor,
    GSharpMinor,
    AMinor,
    ASharpMinor,
    BMinor,
}

impl MusicalKey {
    /// Human-readable hint as stored in [`Session::key`] (e.g. "F minor").
    pub fn key_hint(self) -> String {
        let (tonic, mode) = match self {
            MusicalKey::CMajor => ("C", "major"),
            MusicalKey::CSharpMajor => ("C sharp", "major"),
            MusicalKey::DMajor => ("D", "major"),
            MusicalKey::DSharpMajor => ("D sharp", "major"),
            MusicalKey::EMajor => ("E", "major"),
            MusicalKey::FMajor => ("F", "major"),
            MusicalKey::FSharpMajor => ("F sharp", "major"),
            MusicalKey::GMajor => ("G", "major"),
            MusicalKey::GSharpMajor => ("G sharp", "major"),
            MusicalKey::AMajor => ("A", "major"),
            MusicalKey::ASharpMajor => ("A sharp", "major"),
            MusicalKey::BMajor => ("B", "major"),
            MusicalKey::CMinor => ("C", "minor"),
            MusicalKey::CSharpMinor => ("C sharp", "minor"),
            MusicalKey::DMinor => ("D", "minor"),
            MusicalKey::DSharpMinor => ("D sharp", "minor"),
            MusicalKey::EMinor => ("E", "minor"),
            MusicalKey::FMinor => ("F", "minor"),
            MusicalKey::FSharpMinor => ("F sharp", "minor"),
            MusicalKey::GMinor => ("G", "minor"),
            MusicalKey::GSharpMinor => ("G sharp", "minor"),
            MusicalKey::AMinor => ("A", "minor"),
            MusicalKey::ASharpMinor => ("A sharp", "minor"),
            MusicalKey::BMinor => ("B", "minor"),
        };
        format!("{tonic} {mode}")
    }
}

/// `target_param` vocabulary: "gain" | "pan" | "insert0" | "insert1" |
/// "send_delay" | "send_reverb" (mapped to ParamIds in `compile`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationLane {
    pub target_param: String,
    /// `[bar (section-relative), value, curve]` triples, bars ascending.
    #[serde(default)]
    pub points: Vec<(u32, f32, CurveKind)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CurveKind {
    Linear,
    Exp,
    Smooth,
}

impl CurveKind {
    /// Maps to the engine ramp curve.
    pub fn to_ramp(self) -> kontinuum_schedule::RampCurve {
        match self {
            CurveKind::Linear => kontinuum_schedule::RampCurve::Linear,
            CurveKind::Exp => kontinuum_schedule::RampCurve::Exponential,
            CurveKind::Smooth => kontinuum_schedule::RampCurve::Smooth,
        }
    }
}

// -- Track ------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Track {
    pub id: String,
    pub role: crate::TrackRole,
    pub instrument: InstrumentDef,
    /// Max 2 insert effects per track.
    #[serde(default)]
    pub inserts: Vec<InsertDef>,
    #[serde(default)]
    pub sends: Sends,
    #[serde(default = "d_gain")]
    pub gain: f32,
    #[serde(default = "d_pan")]
    pub pan: f32,
    /// Kick-sidechain duck depth 0..=1 (issue #76): attenuation at full
    /// kick key, 1.0 = duck to unity. `None` (the default) leaves the
    /// engine's per-role default in charge; an explicit value overrides it
    /// per track.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duck_depth: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sends {
    #[serde(default)]
    pub delay: f32,
    #[serde(default)]
    pub reverb: f32,
}

impl Default for Sends {
    fn default() -> Self {
        Sends { delay: 0.0, reverb: 0.0 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InsertDef {
    #[serde(rename = "type")]
    pub kind: InsertKind,
    /// Insert-specific parameters, opaque to the engine core.
    #[serde(default)]
    pub params: serde_json::Value,
    /// Wet/dry mix 0..=1.
    #[serde(default = "d_mix")]
    pub mix: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsertKind {
    Filter,
    Drive,
    Delay,
    Reverb,
    Chorus,
    Compressor,
    Phaser,
    FreqShifter,
    Transient,
}

// -- Pattern ----------------------------------------------------------------

/// A 1-bar pattern (multi-bar via `repeats`): an explicit step list or a
/// generator reference. Untagged: the discriminant is the shape itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Pattern {
    Steps(StepsPattern),
    Euclidean(EuclideanPattern),
    ProbabilityMask(ProbabilityMaskPattern),
}

/// Common tail shared by all pattern forms.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatternTail {
    /// Pattern spans this many bars before looping; explicit steps land in
    /// phrase-bar 0 (later bars of the phrase are silent by design).
    #[serde(default = "d_repeats")]
    pub repeats: u32,
    /// Default gate in beats for steps/onsets without an explicit gate.
    #[serde(default)]
    pub gate: Option<f32>,
    /// Default pitch (MIDI) when a step has none.
    #[serde(default)]
    pub pitch: Option<f32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepsPattern {
    pub steps: Vec<Step>,
    #[serde(default = "d_repeats")]
    pub repeats: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EuclideanPattern {
    /// Must be the string "euclidean".
    pub generator: EuclideanTag,
    /// Onsets.
    pub k: u32,
    /// Slots (16th resolution: n <= 16 per bar).
    pub n: u32,
    /// Rotation in slots.
    #[serde(default)]
    pub rot: i32,
    #[serde(default = "d_velocity")]
    pub velocity: f32,
    #[serde(default = "d_probability")]
    pub probability: f32,
    #[serde(default = "d_repeats")]
    pub repeats: u32,
    #[serde(default)]
    pub gate: Option<f32>,
    #[serde(default)]
    pub pitch: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EuclideanTag {
    Euclidean,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbabilityMaskPattern {
    /// Must be the string "probability_mask".
    pub generator: ProbabilityMaskTag,
    /// Per-16th inclusion probability 0..=1.
    pub density: f32,
    #[serde(default = "d_velocity")]
    pub velocity: f32,
    #[serde(default = "d_probability")]
    pub probability: f32,
    #[serde(default = "d_repeats")]
    pub repeats: u32,
    #[serde(default)]
    pub gate: Option<f32>,
    #[serde(default)]
    pub pitch: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbabilityMaskTag {
    ProbabilityMask,
}

impl Pattern {
    /// Phrase length in bars.
    pub fn repeats(&self) -> u32 {
        match self {
            Pattern::Steps(p) => p.repeats,
            Pattern::Euclidean(p) => p.repeats,
            Pattern::ProbabilityMask(p) => p.repeats,
        }
    }
}

/// One explicit hit. `position` is in ticks within one bar (0..3840 with
/// PPQ=960 and 4 beats/bar).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub position: u32,
    #[serde(default = "d_velocity")]
    pub velocity: f32,
    #[serde(default = "d_probability")]
    pub probability: f32,
    #[serde(default)]
    pub microtiming_ticks: i16,
    /// Sub-hits per onset (flam); 1 = single hit.
    #[serde(default = "d_ratchet")]
    pub ratchet: u8,
    #[serde(default)]
    pub pitch: Option<f32>,
    /// Gate length in beats.
    #[serde(default)]
    pub gate: Option<f32>,
    /// Accent: boosts the hit velocity at compile time (issue #17).
    #[serde(default)]
    pub accent: bool,
}

// -- Instruments ------------------------------------------------------------

/// Built-in synth voices or a sample-library slot. Untagged with explicit
/// `"kind"` discriminants (strict-friendly tagging, see module docs).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InstrumentDef {
    Kick(KickInstrument),
    Hat(HatInstrument),
    Clap(ClapInstrument),
    Snare(SnareInstrument),
    Shaker(ShakerInstrument),
    Bass(BassInstrument),
    Acid(AcidInstrument),
    Pad(PadInstrument),
    Ep(EpInstrument),
    Pluck(PluckInstrument),
    Stab(StabInstrument),
    Wavetable(WavetableInstrument),
    FmPerc(FmPercInstrument),
    Texture(TextureInstrument),
    Sample(SampleSlot),
    Custom(crate::patch::CustomPatch),
}

impl InstrumentDef {
    /// The plugin-registry key for synth kinds; `None` for the harness
    /// built-ins (sample slots resolve to PCM; custom patches run on the
    /// patch interpreter).
    pub fn kind_id(&self) -> Option<&'static str> {
        match self {
            InstrumentDef::Kick(_) => Some("kick"),
            InstrumentDef::Hat(_) => Some("hat"),
            InstrumentDef::Clap(_) => Some("clap"),
            InstrumentDef::Snare(_) => Some("snare"),
            InstrumentDef::Shaker(_) => Some("shaker"),
            InstrumentDef::Bass(_) => Some("bass"),
            InstrumentDef::Acid(_) => Some("acid"),
            InstrumentDef::Ep(_) => Some("ep"),
            InstrumentDef::Pad(_) => Some("pad"),
            InstrumentDef::Pluck(_) => Some("pluck"),
            InstrumentDef::Stab(_) => Some("stab"),
            InstrumentDef::Wavetable(_) => Some("wavetable"),
            InstrumentDef::FmPerc(_) => Some("fmperc"),
            InstrumentDef::Texture(_) => Some("texture"),
            InstrumentDef::Sample(_) | InstrumentDef::Custom(_) => None,
        }
    }

    /// Current float parameter values by IR name — the vocabulary of the
    /// plugin `params_schema`. Bool/enum params surface as 0/1 (hat open,
    /// bass wave).
    pub fn param_values(&self) -> Vec<(&'static str, f32)> {
        match self {
            InstrumentDef::Kick(k) => vec![
                ("tune_hz", k.tune_hz),
                ("decay_ms", k.decay_ms),
                ("click", k.click),
                ("drive", k.drive),
            ],
            InstrumentDef::Hat(h) => vec![
                ("decay_ms", h.decay_ms),
                ("tone", h.tone),
                ("open", f32::from(h.open)),
            ],
            InstrumentDef::Clap(c) => vec![("decay_ms", c.decay_ms), ("tone", c.tone)],
            InstrumentDef::Snare(s) => vec![
                ("tune_hz", s.tune_hz),
                ("decay_ms", s.decay_ms),
                ("snap", s.snap),
            ],
            InstrumentDef::Shaker(sh) => vec![("decay_ms", sh.decay_ms), ("tone", sh.tone)],
            InstrumentDef::Bass(b) => vec![
                ("cutoff_hz", b.cutoff_hz),
                ("resonance", b.resonance),
                ("glide_ms", b.glide_ms),
                ("wave", b.wave.route_value()),
            ],
            InstrumentDef::Acid(a) => vec![
                ("cutoff_hz", a.cutoff_hz),
                ("resonance", a.resonance),
                ("env_amt", a.env_amt),
                ("glide_ms", a.glide_ms),
            ],
            InstrumentDef::Ep(e) => vec![("decay_ms", e.decay_ms), ("depth", e.depth)],
            InstrumentDef::Pad(p) => vec![
                ("attack_ms", p.attack_ms),
                ("release_ms", p.release_ms),
                ("detune_cents", p.detune_cents),
                ("cutoff_hz", p.cutoff_hz),
            ],
            InstrumentDef::Pluck(pl) => vec![("damping", pl.damping), ("bright", pl.bright)],
            InstrumentDef::Stab(st) => vec![
                ("cutoff_hz", st.cutoff_hz),
                ("decay_ms", st.decay_ms),
                ("detune_cents", st.detune_cents),
            ],
            InstrumentDef::Wavetable(w) => vec![
                ("position", w.position),
                ("detune_cents", w.detune_cents),
                ("osc2_level", w.osc2_level),
                ("sub", w.sub),
                ("cutoff_hz", w.cutoff_hz),
                ("release_ms", w.release_ms),
            ],
            InstrumentDef::FmPerc(f) => vec![
                ("ratio", f.ratio),
                ("index", f.index),
                ("feedback", f.feedback),
                ("decay_ms", f.decay_ms),
                ("preset", f.preset.route_value()),
            ],
            InstrumentDef::Texture(t) => vec![
                ("crackle", f32::from(t.crackle)),
                ("density", t.density),
                ("grain_ms", t.grain_ms),
                ("tone", t.tone),
            ],
            InstrumentDef::Sample(_) | InstrumentDef::Custom(_) => vec![],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpTag {
    Ep,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EpInstrument {
    pub kind: EpTag,
    #[serde(default = "d_ep_decay")]
    pub decay_ms: f32,
    #[serde(default = "d_ep_depth")]
    pub depth: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KickInstrument {
    pub kind: KickTag,
    #[serde(default = "d_kick_tune")]
    pub tune_hz: f32,
    #[serde(default = "d_kick_decay")]
    pub decay_ms: f32,
    #[serde(default = "d_click")]
    pub click: f32,
    #[serde(default = "d_drive")]
    pub drive: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KickTag {
    Kick,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HatInstrument {
    pub kind: HatTag,
    #[serde(default = "d_hat_decay")]
    pub decay_ms: f32,
    #[serde(default = "d_tone")]
    pub tone: f32,
    #[serde(default)]
    pub open: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HatTag {
    Hat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClapTag {
    Clap,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClapInstrument {
    pub kind: ClapTag,
    #[serde(default = "d_clap_decay")]
    pub decay_ms: f32,
    #[serde(default = "d_tone")]
    pub tone: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnareTag {
    Snare,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnareInstrument {
    pub kind: SnareTag,
    #[serde(default = "d_snare_tune")]
    pub tune_hz: f32,
    #[serde(default = "d_snare_decay")]
    pub decay_ms: f32,
    #[serde(default = "d_snap")]
    pub snap: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShakerTag {
    Shaker,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShakerInstrument {
    pub kind: ShakerTag,
    #[serde(default = "d_shaker_decay")]
    pub decay_ms: f32,
    #[serde(default = "d_tone")]
    pub tone: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BassInstrument {
    pub kind: BassTag,
    #[serde(default = "d_bass_cutoff")]
    pub cutoff_hz: f32,
    #[serde(default = "d_resonance")]
    pub resonance: f32,
    #[serde(default)]
    pub wave: Wave,
    #[serde(default = "d_glide")]
    pub glide_ms: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BassTag {
    Bass,
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Wave {
    #[default]
    Saw,
    Square,
}

impl Wave {
    /// Control-route value the bass voice expects (0 = saw, 1 = square).
    pub fn route_value(self) -> f32 {
        match self {
            Wave::Saw => 0.0,
            Wave::Square => 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcidTag {
    Acid,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcidInstrument {
    pub kind: AcidTag,
    #[serde(default = "d_acid_cutoff")]
    pub cutoff_hz: f32,
    #[serde(default = "d_resonance")]
    pub resonance: f32,
    #[serde(default = "d_acid_env_amt")]
    pub env_amt: f32,
    #[serde(default = "d_glide")]
    pub glide_ms: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluckTag {
    Pluck,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluckInstrument {
    pub kind: PluckTag,
    #[serde(default = "d_tone")]
    pub damping: f32,
    #[serde(default = "d_tone")]
    pub bright: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StabTag {
    Stab,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StabInstrument {
    pub kind: StabTag,
    #[serde(default = "d_stab_cutoff")]
    pub cutoff_hz: f32,
    #[serde(default = "d_stab_decay")]
    pub decay_ms: f32,
    #[serde(default = "d_stab_detune")]
    pub detune_cents: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WavetableTag {
    Wavetable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FmPercTag {
    /// `snake_case` would split this to "fm_perc"; the wire discriminant is
    /// the plugin-registry kind id.
    #[serde(rename = "fmperc")]
    FmPerc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextureTag {
    Texture,
}

/// FM perc operator recipe (DX7-lineage families); the control-route value
/// selects the voice-side preset (0 = metallic, 1 = tom, 2 = bell).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FmPercPreset {
    #[default]
    Metallic,
    Tom,
    Bell,
}

impl FmPercPreset {
    pub fn route_value(self) -> f32 {
        match self {
            FmPercPreset::Metallic => 0.0,
            FmPercPreset::Tom => 1.0,
            FmPercPreset::Bell => 2.0,
        }
    }
}

/// Sound roster v2 instruments (#30): two morphing wavetable oscs + sine
/// sub, a 2-modulator FM percussion voice, and a noise/crackle texture
/// generator. Field ranges live in [`bounds`] and mirror the voice clamps.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WavetableInstrument {
    pub kind: WavetableTag,
    #[serde(default = "d_wav_position")]
    pub position: f32,
    #[serde(default = "d_wav_detune")]
    pub detune_cents: f32,
    #[serde(default = "d_wav_osc2")]
    pub osc2_level: f32,
    #[serde(default = "d_wav_sub")]
    pub sub: f32,
    #[serde(default = "d_wav_cutoff")]
    pub cutoff_hz: f32,
    #[serde(default = "d_wav_release")]
    pub release_ms: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FmPercInstrument {
    pub kind: FmPercTag,
    #[serde(default = "d_fm_ratio")]
    pub ratio: f32,
    #[serde(default = "d_fm_index")]
    pub index: f32,
    #[serde(default = "d_fm_feedback")]
    pub feedback: f32,
    #[serde(default = "d_fm_decay")]
    pub decay_ms: f32,
    #[serde(default)]
    pub preset: FmPercPreset,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextureInstrument {
    pub kind: TextureTag,
    /// false = granulated noise bed, true = vinyl/tape crackle.
    #[serde(default)]
    pub crackle: bool,
    #[serde(default = "d_tex_density")]
    pub density: f32,
    #[serde(default = "d_tex_grain")]
    pub grain_ms: f32,
    #[serde(default = "d_tex_tone")]
    pub tone: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PadInstrument {
    pub kind: PadTag,
    #[serde(default = "d_pad_attack")]
    pub attack_ms: f32,
    #[serde(default = "d_pad_release")]
    pub release_ms: f32,
    #[serde(default = "d_detune")]
    pub detune_cents: f32,
    #[serde(default = "d_pad_cutoff")]
    pub cutoff_hz: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PadTag {
    Pad,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SampleSlot {
    pub kind: SampleTag,
    /// Vector-search query into the licensed sample library.
    #[serde(default)]
    pub query: Option<String>,
    /// Direct sample id.
    #[serde(default)]
    pub id: Option<u32>,
    /// Rendered sample-pack identity (issue #53 step 2): the FNV-1a hash of
    /// the recipe document. Sessions carry no audio — the engine re-derives
    /// the PCM from the recipe+seed on import.
    #[serde(default)]
    pub recipe_hash: Option<u64>,
    /// Slot transpose in semitones (issue #19 v1). Repitch at playback —
    /// the RT path is repitch-only by design (stretch handles duration).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transpose: Option<f32>,
    /// Slot fine detune in cents, applied with `transpose`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fine: Option<f32>,
    /// Time-stretch factor (issue #19 v1): output duration = input / factor.
    /// Applied control-side (WSOLA) when the PCM is derived from a recipe;
    /// the RT path stays repitch-only — a slot stretched at load then
    /// repitched at play keeps pitch and length independent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stretch: Option<f32>,
    /// Choke group (issue #19 hat logic): slots sharing a group choke each
    /// other on retrigger. 1..=16 to match the RT engine's groups.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choke_group: Option<u8>,
    /// Granular playback mode (issue #19): when present, the slot plays as
    /// a single-source grain cloud instead of a one-shot sampler.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub granular: Option<GranularSlotParams>,
}

/// Granular-mode parameters (issue #19). All optional: omitted fields take
/// the engine defaults in `kontinuum_core::voice::GrainConfig`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GranularSlotParams {
    /// Grain size in ms (20..=200).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grain_ms: Option<f32>,
    /// Grains per second (1..=200).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub density: Option<f32>,
    /// Random read-position jitter per grain in +/- ms (0..=1000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spray_ms: Option<f32>,
    /// Random per-grain tuning jitter in +/- cents (0..=1200).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pitch_jitter_cents: Option<f32>,
    /// Bed mix level (0..=1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleTag {
    Sample,
}

impl SampleSlot {
    /// At least one of `query`/`id`/`recipe_hash` must be present (validated).
    pub fn has_reference(&self) -> bool {
        self.query.as_ref().is_some_and(|q| !q.is_empty())
            || self.id.is_some()
            || self.recipe_hash.is_some()
    }
}

// -- Defaults ---------------------------------------------------------------

/// Engine-side duck release τ (ms) sessions default to — the same value
/// `kontinuum-core`'s duck node starts from, so a session without the
/// field renders exactly as before #76.
pub const DEFAULT_DUCK_RELEASE_MS: f32 = 160.0;

fn d_clap_decay() -> f32 { 350.0 }
fn d_ep_decay() -> f32 { 1400.0 }
fn d_ep_depth() -> f32 { 2.4 }
fn d_snare_tune() -> f32 { 185.0 }
fn d_snare_decay() -> f32 { 220.0 }
fn d_snap() -> f32 { 0.6 }
fn d_shaker_decay() -> f32 { 90.0 }
fn d_acid_cutoff() -> f32 { 700.0 }
fn d_acid_env_amt() -> f32 { 2.6 }
fn d_stab_cutoff() -> f32 { 2600.0 }
fn d_stab_decay() -> f32 { 420.0 }
fn d_stab_detune() -> f32 { 11.0 }
fn d_wav_position() -> f32 { 0.5 }
fn d_wav_detune() -> f32 { 14.0 }
fn d_wav_osc2() -> f32 { 0.8 }
fn d_wav_sub() -> f32 { 0.35 }
fn d_wav_cutoff() -> f32 { 6000.0 }
fn d_wav_release() -> f32 { 220.0 }
fn d_fm_ratio() -> f32 { 1.0 }
fn d_fm_index() -> f32 { 3.0 }
fn d_fm_feedback() -> f32 { 0.3 }
fn d_fm_decay() -> f32 { 320.0 }
fn d_tex_density() -> f32 { 0.002 }
fn d_tex_grain() -> f32 { 30.0 }
fn d_tex_tone() -> f32 { 0.5 }
fn d_velocity() -> f32 {
    0.8
}
fn d_probability() -> f32 {
    1.0
}
fn d_ratchet() -> u8 {
    1
}
fn d_repeats() -> u32 {
    1
}
fn d_gain() -> f32 {
    1.0
}
fn d_pan() -> f32 {
    0.0
}
fn d_duck_release_ms() -> f32 {
    DEFAULT_DUCK_RELEASE_MS
}
fn d_groove_jitter() -> f32 {
    bounds::GROOVE_JITTER_TICKS.0
}
fn d_mix() -> f32 {
    0.5
}
fn d_kick_tune() -> f32 {
    48.0
}
fn d_kick_decay() -> f32 {
    300.0
}
fn d_click() -> f32 {
    0.4
}
fn d_drive() -> f32 {
    0.2
}
fn d_hat_decay() -> f32 {
    60.0
}
fn d_tone() -> f32 {
    0.6
}
fn d_bass_cutoff() -> f32 {
    900.0
}
fn d_resonance() -> f32 {
    0.2
}
fn d_glide() -> f32 {
    30.0
}
fn d_pad_attack() -> f32 {
    400.0
}
fn d_pad_release() -> f32 {
    1200.0
}
fn d_detune() -> f32 {
    10.0
}
fn d_pad_cutoff() -> f32 {
    3000.0
}

/// Sanity: defaults must sit inside the declared bounds.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::IR_VERSION;

    #[test]
    fn defaults_satisfy_bounds() {
        let b = bounds::KICK_TUNE_HZ;
        assert!(d_kick_tune() >= b.0 && d_kick_tune() <= b.1);
        let b = bounds::KICK_DECAY_MS;
        assert!(d_kick_decay() >= b.0 && d_kick_decay() <= b.1);
        assert!(d_click() >= bounds::UNIT.0 && d_click() <= bounds::UNIT.1);
        assert!(d_drive() >= bounds::UNIT.0 && d_drive() <= bounds::UNIT.1);
        let b = bounds::HAT_DECAY_MS;
        assert!(d_hat_decay() >= b.0 && d_hat_decay() <= b.1);
        let b = bounds::BASS_CUTOFF_HZ;
        assert!(d_bass_cutoff() >= b.0 && d_bass_cutoff() <= b.1);
        let b = bounds::PAD_ATTACK_MS;
        assert!(d_pad_attack() >= b.0 && d_pad_attack() <= b.1);
        let b = bounds::PAD_RELEASE_MS;
        assert!(d_pad_release() >= b.0 && d_pad_release() <= b.1);
        let b = bounds::PAD_CUTOFF_HZ;
        assert!(d_pad_cutoff() >= b.0 && d_pad_cutoff() <= b.1);
        let b = bounds::PAD_DETUNE_CENTS;
        assert!(d_detune() >= b.0 && d_detune() <= b.1);
    }

    #[test]
    fn roundtrip_minimal_session() {
        let json = r#"{
            "version": 1, "seed": 7,
            "tempo_lane": [[0, 124.0]],
            "sections": [{"id": "a", "bars": 4, "energy_curve": [0.5]}],
            "tracks": [
                {"id": "kick", "role": "kick",
                 "instrument": {"kind": "kick", "tune_hz": 48.0, "decay_ms": 300.0}}
            ]
        }"#;
        let s: Session = serde_json::from_str(json).expect("parse");
        assert_eq!(s.version, IR_VERSION);
        assert_eq!(s.total_bars(), 4);
        assert_eq!(s.section_start_bars(), vec![0]);
        let out = serde_json::to_string(&s).expect("serialize");
        let s2: Session = serde_json::from_str(&out).expect("reparse");
        assert_eq!(s, s2);
    }

    #[test]
    fn unknown_field_rejected() {
        let json = r#"{
            "version": 1, "seed": 7, "tempo_lane": [[0, 124.0]],
            "sections": [{"id": "a", "bars": 4, "energy_curve": [0.5]}],
            "tracks": [{"id": "k", "role": "kick", "instrument": {"kind": "kick"}}],
            "vibe": "deep"
        }"#;
        assert!(serde_json::from_str::<Session>(json).is_err());
    }

    #[test]
    fn accent_defaults_to_false_and_parses() {
        let s: Step = serde_json::from_str(r#"{"position": 0}"#).expect("parse");
        assert!(!s.accent);
        let s: Step = serde_json::from_str(r#"{"position": 0, "accent": true}"#).expect("parse");
        assert!(s.accent);
    }

    #[test]
    fn pattern_variants_parse() {
        let steps: Pattern = serde_json::from_str(
            r#"{"steps": [{"position": 0, "velocity": 0.9}], "repeats": 2}"#,
        )
        .expect("steps");
        assert_eq!(steps.repeats(), 2);
        let euclid: Pattern = serde_json::from_str(
            r#"{"generator": "euclidean", "k": 4, "n": 16, "rot": 0}"#,
        )
        .expect("euclid");
        assert_eq!(euclid.repeats(), 1);
        let mask: Pattern =
            serde_json::from_str(r#"{"generator": "probability_mask", "density": 0.5}"#)
                .expect("mask");
        assert_eq!(mask.repeats(), 1);
        // Wrong tag must not match the other generator variant.
        assert!(serde_json::from_str::<Pattern>(
            r#"{"generator": "probability_mask", "k": 4, "n": 16}"#
        )
        .is_err());
        assert!(serde_json::from_str::<Pattern>(r#"{"generator": "euclidean", "density": 0.5}"#)
            .is_err());
    }

    #[test]
    fn instrument_variants_parse() {
        for (json, expect_kick) in [
            (r#"{"kind": "kick", "tune_hz": 45.0}"#, true),
            (r#"{"kind": "hat", "open": true}"#, false),
            (r#"{"kind": "bass", "wave": "square"}"#, false),
            (r#"{"kind": "pad", "detune_cents": -20.0}"#, false),
            (r#"{"kind": "sample", "query": "woody perc"}"#, false),
            (r#"{"kind": "sample", "id": 42}"#, false),
            (
                r#"{"kind": "custom", "patch": {"nodes": [
                    {"id": "o1", "type": "osc"},
                    {"id": "out1", "type": "out", "level": 0.9}
                ], "edges": [
                    {"from": "o1", "to": "out1", "type": "audio", "amount": 1.0}
                ]}}"#,
                false,
            ),
        ] {
            let inst: InstrumentDef = serde_json::from_str(json).expect(json);
            assert_eq!(matches!(inst, InstrumentDef::Kick(_)), expect_kick);
        }
        let custom = r#"{"kind": "custom", "patch": {"nodes": [{"id": "o1", "type": "osc"}]}}"#;
        let inst: InstrumentDef = serde_json::from_str(custom).expect("custom parses");
        assert!(matches!(inst, InstrumentDef::Custom(_)));
        assert!(serde_json::from_str::<InstrumentDef>(r#"{"kind": "kick", "bogus": 1}"#).is_err());
        assert!(serde_json::from_str::<InstrumentDef>(r#"{"kind": "gong"}"#).is_err());
        // Custom is strict on unknown fields too, both nesting levels.
        assert!(
            serde_json::from_str::<InstrumentDef>(r#"{"kind": "custom", "vibe": "deep"}"#).is_err()
        );
        assert!(serde_json::from_str::<InstrumentDef>(
            r#"{"kind": "custom", "patch": {"nodes": [{"id": "o1", "type": "osc", "bogus": 1}]}}"#
        )
        .is_err());
    }

    #[test]
    fn sample_slot_reference_rule() {
        let s: SampleSlot = serde_json::from_str(r#"{"kind": "sample"}"#).expect("parse");
        assert!(!s.has_reference());
        let s: SampleSlot =
            serde_json::from_str(r#"{"kind": "sample", "query": ""}"#).expect("parse");
        assert!(!s.has_reference());
        let s: SampleSlot =
            serde_json::from_str(r#"{"kind": "sample", "id": 3}"#).expect("parse");
        assert!(s.has_reference());
        let s: SampleSlot =
            serde_json::from_str(r#"{"kind": "sample", "recipe_hash": 1234567890}"#).expect("parse");
        assert!(s.has_reference(), "recipe hash is a sample reference (#53)");
    }

    /// Issue #19 v1: the new sample-slot fields are optional with serde
    /// defaults, so sessions written before them still parse bit-identically
    /// at the schema level (no new required fields, no serialization change).
    #[test]
    fn sample_slot_v1_fields_default_and_parse() {
        // Pre-#19 session documents parse unchanged: every new field is
        // absent-and-None.
        let s: SampleSlot = serde_json::from_str(r#"{"kind": "sample"}"#).expect("parse");
        assert_eq!(s.transpose, None);
        assert_eq!(s.fine, None);
        assert_eq!(s.stretch, None);
        assert_eq!(s.choke_group, None);
        assert_eq!(s.granular, None);

        let s: SampleSlot = serde_json::from_str(
            r#"{"kind": "sample", "transpose": -12.0, "fine": 7.5, "stretch": 1.25,
                "choke_group": 1}"#,
        )
        .expect("parse");
        assert_eq!(s.transpose, Some(-12.0));
        assert_eq!(s.fine, Some(7.5));
        assert_eq!(s.stretch, Some(1.25));
        assert_eq!(s.choke_group, Some(1));

        let s: SampleSlot = serde_json::from_str(
            r#"{"kind": "sample", "granular": {"grain_ms": 60.0, "density": 40.0,
                "spray_ms": 30.0, "pitch_jitter_cents": 25.0, "level": 0.6}}"#,
        )
        .expect("parse");
        let g = s.granular.expect("granular");
        assert_eq!(g.grain_ms, Some(60.0));
        assert_eq!(g.density, Some(40.0));
        assert_eq!(g.spray_ms, Some(30.0));
        assert_eq!(g.pitch_jitter_cents, Some(25.0));
        assert_eq!(g.level, Some(0.6));

        // Partial granular docs parse; the engine fills the rest.
        let s: SampleSlot =
            serde_json::from_str(r#"{"kind": "sample", "granular": {}}"#).expect("parse");
        assert!(s.granular.is_some());

        // Round trip: None fields never serialize.
        let out = serde_json::to_string(&s).expect("serialize");
        assert!(!out.contains("transpose"), "None fields must not serialize: {out}");
    }

    #[test]
    fn duck_fields_default_and_parse() {
        let json = r#"{
            "version": 1, "seed": 7,
            "tempo_lane": [[0, 124.0]],
            "sections": [{"id": "a", "bars": 4, "energy_curve": [0.5]}],
            "tracks": [
                {"id": "kick", "role": "kick", "instrument": {"kind": "kick"}},
                {"id": "bass", "role": "bass", "instrument": {"kind": "bass"},
                 "duck_depth": 1.0}
            ]
        }"#;
        let s: Session = serde_json::from_str(json).expect("parse");
        assert_eq!(s.duck_release_ms, 160.0, "session release must default to the engine's");
        assert_eq!(s.tracks[0].duck_depth, None, "unset depth means role default");
        assert_eq!(s.tracks[1].duck_depth, Some(1.0), "full range to unity must parse");
        let out = serde_json::to_string(&s).expect("serialize");
        let s2: Session = serde_json::from_str(&out).expect("reparse");
        assert_eq!(s, s2);
        assert!(
            !out.contains("duck_depth\":null"),
            "None depths must not serialize"
        );
    }

    #[test]
    fn rejects_wrong_generator_tag() {
        assert!(serde_json::from_str::<EuclideanPattern>(
            r#"{"generator": "probability_mask", "k": 1, "n": 2}"#
        )
        .is_err());
    }
}
