//! Mood steering (issue #22): a natural-language or quick-chip instruction
//! becomes a [`SteeringVector`] plus optional direct intents, the vector is
//! mapped onto the closed diff-op schema, and the ops cross the real
//! kontinuum-ir validate/apply gate before anything reaches the engine.
//!
//! Pipeline (who does what):
//! - quick chips ([`QuickChip`]) resolve to predefined vectors with **no
//!   LLM** — instant, tier T0.
//! - free-form NL goes through the [`SteeringProvider`] seam. The scripted
//!   parser ([`ScriptedSteeringProvider`]) stands in for the T1 model in
//!   tests and the eval harness; the real T1 on-device classifier /
//!   guided-generation call replaces it behind the same trait.
//! - the model-facing emitter ([`crate::ComposerBackend`]) receives the
//!   directive inside the plan request and returns raw diff JSON — the same
//!   structured-emission path as scheduled wakes. Diff JSON is parsed into
//!   the closed [`kontinuum_ir::IrDiff`] schema, never free-form.
//! - [`run_steering`] applies with the issue's repair contract: one
//!   self-correction retry with the `{code, path, suggested_fix}` errors fed
//!   back, then drop the diff, log, continue (T0 covers). Invalid-rate per
//!   tier accumulates in [`ComposerTelemetry`].
//!
//! Landing rules (issue #22, PLAN §2.4): T0-servable moves (energy,
//! density, mutes, instrument timbre) target the section under the playhead
//! — the engine's block cache keeps everything before the next 4-bar
//! boundary bit-identical, so they are audible within ≤ 4 bars by
//! construction. Composition-level moves (automation, tempo, scheduled
//! transitions) anchor at the next section boundary, ≤ 16 bars on the
//! standard section grid. The DjDeck one-shot fast path (#38) is wired for
//! UI live-moves, not external diff ops, so external steering lands at
//! these deterministic boundaries instead — same guarantee, documented
//! here.

use kontinuum_compose::engine::ArrangementEngine;
use kontinuum_ir::schema::bounds;
use kontinuum_ir::{IrDiff, Session};
use serde::{Deserialize, Serialize};

use crate::backend::{PlanContext, PlanRequest};
use crate::context::{ComposerContext, ContextInputs};
use crate::orchestrator::validate_diffs;
use crate::scripted::Tier;

/// Content-repair replays for one steering wake (issue #22: one
/// self-correction retry; the second failure drops the diff).
pub const STEERING_REPAIR_ROUNDS: u32 = 1;

/// A T0-servable move is audible at the next 4-bar block boundary.
pub const T0_MAX_BARS: u32 = 4;
/// Composition-level moves anchor at the next section boundary.
pub const COMPOSITION_MAX_BARS: u32 = 16;

/// Direction the steering wants each musical axis pushed. Every field is a
/// delta in `-1..=1` (0 = leave alone); magnitudes are intentional bias,
/// not absolute targets — the composer machinery maps them onto current
/// session state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SteeringVector {
    pub energy: f32,
    pub density: f32,
    pub brightness: f32,
    /// Warmth / valence: space, reverb, softer edges.
    pub warmth: f32,
    /// Tempo nudge, fraction of the ±4% live drift ceiling.
    pub tempo_drift: f32,
    /// Appetite for structural novelty (transitions, re-arrangement).
    pub novelty: f32,
}

impl SteeringVector {
    pub fn new(energy: f32, density: f32, brightness: f32, warmth: f32, tempo_drift: f32, novelty: f32) -> Self {
        SteeringVector {
            energy: energy.clamp(-1.0, 1.0),
            density: density.clamp(-1.0, 1.0),
            brightness: brightness.clamp(-1.0, 1.0),
            warmth: warmth.clamp(-1.0, 1.0),
            tempo_drift: tempo_drift.clamp(-1.0, 1.0),
            novelty: novelty.clamp(-1.0, 1.0),
        }
    }

    pub fn zero() -> Self {
        SteeringVector::default()
    }

    /// Additive merge, clamped — chips and parsed directives can stack.
    pub fn combine(self, other: SteeringVector) -> SteeringVector {
        SteeringVector {
            energy: (self.energy + other.energy).clamp(-1.0, 1.0),
            density: (self.density + other.density).clamp(-1.0, 1.0),
            brightness: (self.brightness + other.brightness).clamp(-1.0, 1.0),
            warmth: (self.warmth + other.warmth).clamp(-1.0, 1.0),
            tempo_drift: (self.tempo_drift + other.tempo_drift).clamp(-1.0, 1.0),
            novelty: (self.novelty + other.novelty).clamp(-1.0, 1.0),
        }
    }

    /// True when no axis crosses the steer threshold — nothing to do.
    pub fn is_quiet(&self) -> bool {
        self.energy.abs() < 0.05
            && self.density.abs() < 0.05
            && self.brightness.abs() < 0.05
            && self.warmth.abs() < 0.05
            && self.tempo_drift.abs() < 0.1
            && self.novelty < 0.1
    }
}

/// Quick-chip vocabulary v1 (issue #22): predefined steering vectors, no
/// LLM, instant. The UI surface (#33) sends these verbatim.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuickChip {
    Darker,
    Brighter,
    MoreEnergy,
    Calmer,
    MoreMinimal,
    Deeper,
    Weirder,
    SleepMode,
}

/// The v1 chip vocabulary, verbatim (case-insensitive).
pub const QUICK_CHIP_VOCABULARY: [(&str, QuickChip); 8] = [
    ("darker", QuickChip::Darker),
    ("brighter", QuickChip::Brighter),
    ("more energy", QuickChip::MoreEnergy),
    ("calmer", QuickChip::Calmer),
    ("more minimal", QuickChip::MoreMinimal),
    ("deeper", QuickChip::Deeper),
    ("weirder", QuickChip::Weirder),
    ("sleep mode", QuickChip::SleepMode),
];

impl QuickChip {
    /// Exact-match resolution, case-insensitive. Deliberately NOT
    /// fuzzy: chips are buttons, not guesses.
    pub fn from_text(text: &str) -> Option<QuickChip> {
        let normalized = text.trim().to_ascii_lowercase();
        QUICK_CHIP_VOCABULARY.iter().find(|(v, _)| *v == normalized).map(|(_, c)| *c)
    }

    /// The predefined delta vector.
    pub fn vector(self) -> SteeringVector {
        match self {
            QuickChip::Darker => SteeringVector::new(-0.25, 0.0, -0.45, 0.15, 0.0, 0.0),
            QuickChip::Brighter => SteeringVector::new(0.1, 0.0, 0.45, 0.0, 0.0, 0.0),
            QuickChip::MoreEnergy => SteeringVector::new(0.45, 0.2, 0.1, 0.0, 0.0, 0.0),
            QuickChip::Calmer => SteeringVector::new(-0.4, -0.15, 0.0, 0.1, 0.0, -0.15),
            QuickChip::MoreMinimal => SteeringVector::new(0.0, -0.45, 0.0, 0.05, 0.0, 0.0),
            QuickChip::Deeper => SteeringVector::new(-0.1, 0.0, -0.3, 0.2, 0.0, 0.0),
            QuickChip::Weirder => SteeringVector::new(0.1, 0.0, 0.15, 0.0, 0.0, 0.5),
            QuickChip::SleepMode => SteeringVector::new(-0.55, -0.3, -0.35, 0.1, -0.1, -0.2),
        }
    }
}

/// Direct intents bypass the vector: concrete, mechanical changes the user
/// named ("drop the hats" → mute the perc track).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum DirectIntent {
    /// Silence `track` in the target section (pattern probability → 0).
    MuteTrack(String),
}

/// Where a directive came from — the telemetry tier follows from this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SteeringSource {
    /// Quick chip: deterministic, no model.
    QuickChip,
    /// Parsed by the provider seam (T1 in production, scripted in eval).
    Provider,
}

impl From<SteeringSource> for Tier {
    fn from(source: SteeringSource) -> Tier {
        match source {
            SteeringSource::QuickChip => Tier::T0Deterministic,
            SteeringSource::Provider => Tier::T1OnDevice,
        }
    }
}

/// What a steering parse produces.
#[derive(Clone, Debug, PartialEq)]
pub struct SteeringDirective {
    pub vector: SteeringVector,
    pub intents: Vec<DirectIntent>,
    pub source: SteeringSource,
    pub notes: String,
}

impl SteeringDirective {
    pub fn chip(chip: QuickChip) -> Self {
        SteeringDirective {
            vector: chip.vector(),
            intents: Vec::new(),
            source: SteeringSource::QuickChip,
            notes: format!("chip: {chip:?}"),
        }
    }
}

/// Why the provider could not parse an instruction.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SteeringError {
    #[error("instruction not understood")]
    NotUnderstood,
}

/// The provider seam for NL → steering (issue #22 "small-model task").
/// Production wires the T1 on-device model here; the eval wires
/// [`ScriptedSteeringProvider`]. Same contract either way: pure, offline,
/// deterministic in tests.
pub trait SteeringProvider {
    fn parse(&mut self, instruction: &str) -> Result<SteeringDirective, SteeringError>;
}

/// Deterministic rule-based parser: the scripted stand-in for the T1
/// classifier in tests and the eval harness. Understands the chip
/// vocabulary, direct-intent phrases ("drop the hats"), tempo/density
/// phrasing, and hedging modifiers ("subtle", "a bit" halve the deltas).
/// Chip-matching text resolves via [`QuickChip::from_text`] without this
/// parser ever being consulted.
pub struct ScriptedSteeringProvider;

const HEDGE_WORDS: [&str; 5] = ["subtle", "slightly", "a bit", "a little", "gentle"];

/// Direct-intent aliases → session track ids. Aliases that the session
/// palette does not carry ("drop the claps" on a kit without claps) parse
/// fine and fail at the validation gate — that is the repair loop's job,
/// not the parser's.
const MUTE_ALIASES: [(&str, &str); 6] = [
    ("hats", "perc"),
    ("hat", "perc"),
    ("perc", "perc"),
    ("kick", "kick"),
    ("bass", "bass"),
    ("clap", "clap"),
];

impl SteeringProvider for ScriptedSteeringProvider {
    fn parse(&mut self, instruction: &str) -> Result<SteeringDirective, SteeringError> {
        if QuickChip::from_text(instruction).is_some() {
            // Chips never reach the provider; the caller resolves them.
            return Err(SteeringError::NotUnderstood);
        }
        let text = instruction.to_ascii_lowercase();
        let mut vector = SteeringVector::zero();
        let mut intents = Vec::new();
        let mut matched = false;

        for (needle, chip) in QUICK_CHIP_VOCABULARY {
            if text.contains(needle) {
                vector = vector.combine(chip.vector());
                matched = true;
            }
        }
        for (alias, track) in MUTE_ALIASES {
            let drop = format!("drop the {alias}");
            let kill = format!("kill the {alias}");
            if text.contains(&drop) || text.contains(&kill) || text == format!("{alias} off") {
                let already = intents
                    .iter()
                    .any(|i| matches!(i, DirectIntent::MuteTrack(t) if t == track));
                if !already {
                    intents.push(DirectIntent::MuteTrack(track.to_string()));
                }
                matched = true;
            }
        }
        if text.contains("slower") || text.contains("slow it down") || text.contains("slow down") {
            vector.tempo_drift = (vector.tempo_drift - 0.5).clamp(-1.0, 1.0);
            matched = true;
        }
        if text.contains("faster") || text.contains("speed up") || text.contains("push the tempo") {
            vector.tempo_drift = (vector.tempo_drift + 0.5).clamp(-1.0, 1.0);
            matched = true;
        }
        if text.contains("bump the energy") || text.contains("energy up") {
            vector.energy = (vector.energy + 0.4).clamp(-1.0, 1.0);
            matched = true;
        }
        if text.contains("less going on") || text.contains("simpler") {
            vector.density = (vector.density - 0.4).clamp(-1.0, 1.0);
            matched = true;
        }
        if text.contains("busier") || text.contains("more going on") {
            vector.density = (vector.density + 0.4).clamp(-1.0, 1.0);
            matched = true;
        }
        if text.contains("softer pads") {
            vector.warmth = (vector.warmth + 0.3).clamp(-1.0, 1.0);
            vector.energy = (vector.energy - 0.15).clamp(-1.0, 1.0);
            matched = true;
        }
        if text.contains("riser") || text.contains("take me somewhere") {
            vector.novelty = (vector.novelty + 0.5).clamp(-1.0, 1.0);
            matched = true;
        }
        if text.contains("louder") {
            vector.energy = (vector.energy + 0.45).clamp(-1.0, 1.0);
            matched = true;
        }
        if let Some(rest) = text.strip_prefix("mute the ") {
            let track = rest.split_whitespace().next().unwrap_or_default();
            if !track.is_empty() {
                intents.push(DirectIntent::MuteTrack(track.to_string()));
                matched = true;
            }
        }
        if !matched {
            return Err(SteeringError::NotUnderstood);
        }
        if HEDGE_WORDS.iter().any(|w| text.contains(w)) {
            vector = SteeringVector {
                energy: vector.energy * 0.5,
                density: vector.density * 0.5,
                brightness: vector.brightness * 0.5,
                warmth: vector.warmth * 0.5,
                tempo_drift: vector.tempo_drift * 0.5,
                novelty: vector.novelty * 0.5,
            };
        }
        Ok(SteeringDirective {
            vector,
            intents,
            source: SteeringSource::Provider,
            notes: "scripted parse".into(),
        })
    }
}

/// The op class a steering move belongs to — the eval harness scores
/// correctness against the expected class (issue #22: "correctness vs
/// expected op class").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpClass {
    Energy,
    Density,
    Mute,
    Timbre,
    Space,
    Tempo,
    Transition,
}

/// Classifies a diff op. The schema is closed, so the match is total;
/// `ReplacePattern` splits into `Mute` (probability silenced) vs `Density`
/// so the eval can score "drop the hats" against the mute class.
pub fn classify(diff: &IrDiff) -> OpClass {
    match diff {
        IrDiff::SetSectionEnergy { .. } => OpClass::Energy,
        IrDiff::ReplacePattern { pattern, .. } => {
            use kontinuum_ir::schema::Pattern;
            let silenced = match pattern {
                Pattern::Euclidean(e) => e.probability == 0.0,
                Pattern::ProbabilityMask(m) => m.probability == 0.0,
                Pattern::Steps(s) => {
                    !s.steps.is_empty() && s.steps.iter().all(|st| st.probability == 0.0)
                }
            };
            if silenced { OpClass::Mute } else { OpClass::Density }
        }
        IrDiff::SetInstrumentParam { .. } => OpClass::Timbre,
        IrDiff::SetAutomation { .. } => OpClass::Space,
        IrDiff::SetTempo { .. } => OpClass::Tempo,
        IrDiff::ScheduleTransition { .. } => OpClass::Transition,
        _ => OpClass::Timbre,
    }
}

/// A planned op plus the bar it becomes audible.
#[derive(Clone, Debug, PartialEq)]
pub struct PlannedOp {
    pub diff: IrDiff,
    pub landing_bar: u32,
}

/// The steering plan: T0-servable moves (≤ [`T0_MAX_BARS`] to audible) and
/// composition-level moves (≤ [`COMPOSITION_MAX_BARS`] on the standard
/// grid).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SteeringPlan {
    pub t0: Vec<PlannedOp>,
    pub composition: Vec<PlannedOp>,
}

/// Next 4-bar block boundary strictly after `at_bar` — the earliest bar a
/// recompiled block reaches the RT queue after a diff invalidates the cache.
fn next_block_boundary(at_bar: u32) -> u32 {
    (at_bar / kontinuum_ir::compile::BLOCK_BARS + 1) * kontinuum_ir::compile::BLOCK_BARS
}

/// Section containing the playhead (the live-move carve-out's editable
/// section), or the next one if the playhead sits on a boundary.
fn t0_section(session: &Session, at_bar: u32) -> Option<(String, u32)> {
    let starts = session.section_start_bars();
    session
        .sections
        .iter()
        .zip(starts.iter())
        .find(|&(sec, start)| at_bar < start + sec.bars)
        .map(|(sec, start)| (sec.id.clone(), *start))
}

/// First section boundary at or after the playhead.
fn next_section_boundary(session: &Session, at_bar: u32) -> Option<u32> {
    session.section_start_bars().into_iter().find(|&start| start >= at_bar)
}

fn section_by_id<'a>(session: &'a Session, id: &str) -> Option<&'a kontinuum_ir::schema::Section> {
    session.sections.iter().find(|s| s.id == id)
}

fn pattern_probability(pattern: &mut kontinuum_ir::schema::Pattern, value: f32) {
    use kontinuum_ir::schema::Pattern;
    match pattern {
        Pattern::Euclidean(e) => e.probability = value,
        Pattern::Steps(s) => {
            for step in &mut s.steps {
                step.probability = value;
            }
        }
        Pattern::ProbabilityMask(m) => m.probability = value,
    }
}

/// Scales a pattern's onset density. Euclidean k moves toward 1 (a silent
/// k=0 is invalid); step/mask forms scale probability/density instead.
fn scale_pattern_density(pattern: &kontinuum_ir::schema::Pattern, delta: f32) -> kontinuum_ir::schema::Pattern {
    use kontinuum_ir::schema::Pattern;
    let factor = 1.0 + 0.4 * delta;
    match pattern {
        Pattern::Euclidean(e) => {
            let mut e = e.clone();
            e.k = ((e.k as f32 * factor).round() as u32).clamp(1, e.n.max(1));
            Pattern::Euclidean(e)
        }
        Pattern::ProbabilityMask(m) => {
            let mut m = m.clone();
            m.density = (m.density * factor).clamp(0.05, 1.0);
            Pattern::ProbabilityMask(m)
        }
        Pattern::Steps(s) => {
            let mut s = s.clone();
            for step in &mut s.steps {
                step.probability = (step.probability * factor).clamp(0.0, 1.0);
            }
            Pattern::Steps(s)
        }
    }
}

fn track_index(session: &Session, id: &str) -> Option<usize> {
    session.tracks.iter().position(|t| t.id == id)
}

/// The density move's track: percussion first, bass as the fallback.
fn density_track(session: &Session) -> Option<String> {
    ["perc", "bass"].iter().find(|id| track_index(session, id).is_some()).map(|id| id.to_string())
}

/// The brightness move's target: (track, param, base value, bound). Pad and
/// bass expose cutoffs; percussion exposes tone (0..1).
fn brightness_target(session: &Session) -> Option<(String, &'static str, f32, (f32, f32))> {
    use kontinuum_ir::InstrumentDef;
    if let Some(ti) = track_index(session, "pad") {
        let base = match &session.tracks[ti].instrument {
            InstrumentDef::Pad(p) => p.cutoff_hz,
            other => other_cutoff(other).unwrap_or(4_000.0),
        };
        return Some(("pad".into(), "cutoff_hz", base, bounds::PAD_CUTOFF_HZ));
    }
    if let Some(ti) = track_index(session, "bass") {
        let base = match &session.tracks[ti].instrument {
            InstrumentDef::Bass(b) => b.cutoff_hz,
            InstrumentDef::Acid(a) => a.cutoff_hz,
            other => other_cutoff(other).unwrap_or(900.0),
        };
        return Some(("bass".into(), "cutoff_hz", base, bounds::BASS_CUTOFF_HZ));
    }
    if let Some(ti) = track_index(session, "perc") {
        let base = match &session.tracks[ti].instrument {
            InstrumentDef::Hat(h) => h.tone,
            _ => 0.5,
        };
        return Some(("perc".into(), "tone", base, bounds::UNIT));
    }
    None
}

fn other_cutoff(def: &kontinuum_ir::InstrumentDef) -> Option<f32> {
    use kontinuum_ir::InstrumentDef;
    match def {
        InstrumentDef::Stab(s) => Some(s.cutoff_hz),
        _ => None,
    }
}

fn current_bpm(session: &Session) -> f64 {
    session.tempo_lane.last().map(|&(_, bpm)| bpm).unwrap_or(124.0)
}

/// Maps a directive onto diff ops against live session state. This is the
/// reference mapping the eval scores against and the T0 chip path executes
/// directly; a real T1 emitter is prompted toward the same op shapes.
pub fn plan_ops(directive: &SteeringDirective, session: &Session, at_bar: u32) -> SteeringPlan {
    let mut plan = SteeringPlan::default();
    let v = directive.vector;
    let Some((t0_id, _)) = t0_section(session, at_bar) else {
        return plan;
    };
    let boundary = next_section_boundary(session, at_bar);

    if v.energy.abs() >= 0.05 {
        let base = section_by_id(session, &t0_id)
            .and_then(|s| s.energy_curve.first().copied())
            .unwrap_or(0.6);
        let target = (base + 0.35 * v.energy).clamp(0.05, 1.0);
        plan.t0.push(PlannedOp {
            landing_bar: next_block_boundary(at_bar),
            diff: IrDiff::SetSectionEnergy {
                id: t0_id.clone(),
                energy: vec![target * 0.85, target, target, target * 0.9],
            },
        });
    }
    if v.density.abs() >= 0.05 {
        if let Some(track) = density_track(session) {
            if let Some(sec) = section_by_id(session, &t0_id) {
                if let Some(pattern) = sec.pattern_bindings.get(&track) {
                    plan.t0.push(PlannedOp {
                        landing_bar: next_block_boundary(at_bar),
                        diff: IrDiff::ReplacePattern {
                            section: t0_id.clone(),
                            track,
                            pattern: scale_pattern_density(pattern, v.density),
                        },
                    });
                }
            }
        }
    }
    for intent in &directive.intents {
        let DirectIntent::MuteTrack(track) = intent;
        if let Some(sec) = section_by_id(session, &t0_id) {
            if let Some(pattern) = sec.pattern_bindings.get(track) {
                let mut muted = pattern.clone();
                pattern_probability(&mut muted, 0.0);
                plan.t0.push(PlannedOp {
                    landing_bar: next_block_boundary(at_bar),
                    diff: IrDiff::ReplacePattern { section: t0_id.clone(), track: track.clone(), pattern: muted },
                });
            }
            // No binding for the requested track: nothing to mute. The gate
            // never sees an op, and the outcome reports the miss.
        }
    }
    if v.brightness.abs() >= 0.05 {
        if let Some((track, param, base, (lo, hi))) = brightness_target(session) {
            let value = (base * (1.0 + 0.4 * v.brightness)).clamp(lo, hi);
            plan.t0.push(PlannedOp {
                landing_bar: next_block_boundary(at_bar),
                diff: IrDiff::SetInstrumentParam { track, param: param.into(), value },
            });
        }
    }
    let Some(boundary) = boundary else {
        return plan;
    };
    if v.warmth.abs() >= 0.05 && track_index(session, "pad").is_some() {
        let send = (0.3 + 0.3 * v.warmth).clamp(0.0, 1.0);
        plan.composition.push(PlannedOp {
            landing_bar: boundary,
            diff: IrDiff::SetAutomation {
                section: boundary_section_id(session, boundary),
                track: "pad".into(),
                lane: kontinuum_ir::schema::AutomationLane {
                    target_param: "send_reverb".into(),
                    points: vec![(0, send, kontinuum_ir::schema::CurveKind::Smooth)],
                },
            },
        });
    }
    if v.tempo_drift.abs() >= 0.1 {
        let bpm = (current_bpm(session) * (1.0 + 0.04 * v.tempo_drift as f64))
            .clamp(bounds::LIVE_BPM.0, bounds::LIVE_BPM.1);
        plan.composition.push(PlannedOp {
            landing_bar: boundary,
            diff: IrDiff::SetTempo { bpm },
        });
    }
    if v.novelty >= 0.1 {
        plan.composition.push(PlannedOp {
            landing_bar: boundary,
            diff: IrDiff::ScheduleTransition {
                at_bar: boundary,
                transition: kontinuum_ir::schema::Transition {
                    kind: kontinuum_ir::schema::TransitionKind::Riser,
                    bars: 1,
                    params: serde_json::Value::Null,
                },
            },
        });
    }
    plan
}

fn boundary_section_id(session: &Session, boundary: u32) -> String {
    let starts = session.section_start_bars();
    session
        .sections
        .iter()
        .zip(starts.iter())
        .find(|&(_, start)| *start == boundary)
        .map(|(sec, _)| sec.id.clone())
        .unwrap_or_default()
}

/// Per-tier invalid-rate telemetry (issue #22: "track invalid-rate per
/// tier"; #36 surfaces the same numbers per backend in Settings).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TierStats {
    pub proposals: u32,
    pub invalid: u32,
    pub invalid_after_retry: u32,
    pub repairs: u32,
    pub dropped_ops: u32,
}

impl TierStats {
    /// Invalid fraction of all proposed ops (0 when nothing proposed).
    pub fn invalid_rate(&self) -> f32 {
        if self.proposals == 0 {
            0.0
        } else {
            self.invalid as f32 / self.proposals as f32
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ComposerTelemetry {
    pub t0: TierStats,
    pub t1: TierStats,
    pub t2: TierStats,
}

impl ComposerTelemetry {
    pub fn stats(&self, tier: Tier) -> &TierStats {
        match tier {
            Tier::T0Deterministic => &self.t0,
            Tier::T1OnDevice => &self.t1,
            Tier::T2Cloud => &self.t2,
        }
    }

    pub fn stats_mut(&mut self, tier: Tier) -> &mut TierStats {
        match tier {
            Tier::T0Deterministic => &mut self.t0,
            Tier::T1OnDevice => &mut self.t1,
            Tier::T2Cloud => &mut self.t2,
        }
    }

    /// Attributes a validated plan batch (wake path) to the backend's tier.
    pub fn record_plan(&mut self, backend: &str, proposed: usize, invalid: usize, invalid_after_retry: usize, repairs: u32) {
        let s = self.stats_mut(Tier::of_backend(backend));
        s.proposals += proposed as u32;
        s.invalid += invalid as u32;
        s.invalid_after_retry += invalid_after_retry as u32;
        s.repairs += repairs;
        s.dropped_ops += invalid_after_retry as u32;
    }
}

/// How one steering wake ended.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SteeringOutcome {
    pub applied: Vec<String>,
    pub rejected: usize,
    pub repairs: u32,
    /// Ops dropped after the retry also failed (logged, session untouched).
    pub dropped: usize,
    /// Bars from the instruction to the first audible change.
    pub bars_to_audible: u32,
    pub op_classes: Vec<OpClass>,
    pub tier: Option<Tier>,
}

/// Validates and applies a [`SteeringPlan`] through the real IR gate.
///
/// Per-op anchoring: T0 ops cross the gate at the playhead (the live-move
/// carve-out makes the playing section editable), composition ops at their
/// own boundary bar. Ops that fail validation are dropped and logged — T0
/// ops are deterministic, so there is nothing to re-ask; the provider path
/// in [`run_steering`] owns the model-driven retry.
pub fn apply_plan(
    engine: &mut ArrangementEngine,
    plan: &SteeringPlan,
    at_bar: u32,
    telemetry: &mut ComposerTelemetry,
    tier: Tier,
) -> SteeringOutcome {
    let mut outcome = SteeringOutcome {
        tier: Some(tier),
        bars_to_audible: u32::MAX,
        ..SteeringOutcome::default()
    };
    let mut proposed = 0u32;
    let mut invalid = 0u32;
    for op in plan.t0.iter().chain(plan.composition.iter()) {
        proposed += 1;
        let anchor = match op.diff {
            IrDiff::SetTempo { .. } => op.landing_bar,
            _ => at_bar,
        };
        let raw = serde_json::to_string(&op.diff).expect("IrDiff serializes");
        let mut scratch = engine.current_session().clone();
        let gate = validate_diffs(&mut scratch, anchor, &[raw.clone()]);
        if gate.valid.is_empty() {
            invalid += 1;
            outcome.rejected += 1;
            outcome.dropped += 1;
            log_dropped_op(tier, &raw, &gate.problems);
            continue;
        }
        match engine.apply_diff(&op.diff, anchor) {
            Ok(_) => {
                outcome.applied.push(raw);
                outcome.op_classes.push(classify(&op.diff));
                outcome.bars_to_audible =
                    outcome.bars_to_audible.min(op.landing_bar.saturating_sub(at_bar));
            }
            Err(e) => {
                invalid += 1;
                outcome.rejected += 1;
                outcome.dropped += 1;
                log_dropped_op(tier, &raw, &[format!("apply: {e}")]);
            }
        }
    }
    let stats = telemetry.stats_mut(tier);
    stats.proposals += proposed;
    stats.invalid += invalid;
    stats.dropped_ops += invalid;
    if outcome.bars_to_audible == u32::MAX {
        outcome.bars_to_audible = 0;
    }
    outcome
}

fn log_dropped_op(tier: Tier, raw: &str, problems: &[String]) {
    // Structured drop log: tier, op, and the validator's actionable errors.
    // The host ships this to telemetry (#36 Settings surface).
    eprintln!("steering drop [tier {tier:?}]: {raw} :: {}", problems.join("; "));
}

fn log_dropped_batch(tier: Tier, batch: &[String]) {
    for raw in batch {
        log_dropped_op(tier, raw, &["dropped after repair retry".to_string()]);
    }
}

/// One full steering wake through the provider seam (issue #22). The chip
/// path never reaches the emitter; the NL path builds a plan request with
/// the directive summary and the serialized context document embedded, walks
/// the emitter's diffs through the gate, retries once with the
/// `{code, path, suggested_fix}` errors fed back, then drops, logs, and
/// continues.
pub fn run_steering(
    engine: &mut ArrangementEngine,
    provider: &mut dyn SteeringProvider,
    emitter: &mut dyn crate::ComposerBackend,
    at_bar: u32,
    instruction: &str,
    context_inputs: ContextInputs<'_>,
    telemetry: &mut ComposerTelemetry,
) -> SteeringOutcome {
    if let Some(chip) = QuickChip::from_text(instruction) {
        let plan = plan_ops(&SteeringDirective::chip(chip), engine.current_session(), at_bar);
        return apply_plan(engine, &plan, at_bar, telemetry, Tier::T0Deterministic);
    }
    let Ok(directive) = provider.parse(instruction) else {
        // Not understood: log and continue — the session never stalls on a
        // parse miss.
        eprintln!("steering: instruction not understood: {instruction}");
        return SteeringOutcome::default();
    };
    let tier = Tier::from(directive.source);
    let context = ComposerContext::build(engine.current_session(), at_bar, context_inputs);
    let mut request = PlanRequest {
        style: String::new(),
        prompt: format!("{instruction} [directive: {}]", directive.notes),
        bars_left_in_section: bars_left(engine.current_session(), at_bar),
        progression: Vec::new(),
        taste_json: serde_json::to_string(&directive.vector).unwrap_or_default(),
        style_card: context.serialize(),
        context: PlanContext::from_session(engine.current_session(), at_bar),
        repair_context: String::new(),
    };

    let mut outcome = SteeringOutcome {
        tier: Some(tier),
        bars_to_audible: u32::MAX,
        ..SteeringOutcome::default()
    };
    let mut pending = emitter.plan(&request).map(|r| r.diffs).unwrap_or_default();
    let mut repairs_spent = 0u32;
    loop {
        let problems = gate_and_apply(engine, &pending, at_bar, &mut outcome);
        let invalid = problems.len();
        if invalid == 0 {
            break;
        }
        if repairs_spent >= STEERING_REPAIR_ROUNDS {
            // Second failure: drop the diff, log, continue (T0 covers).
            outcome.dropped += invalid;
            log_dropped_batch(tier, &pending);
            break;
        }
        repairs_spent += 1;
        outcome.repairs += 1;
        request.repair_context = problems.join("; ");
        match emitter.plan(&request) {
            Ok(p) => pending = p.diffs,
            Err(_) => {
                outcome.dropped += invalid;
                log_dropped_batch(tier, &pending);
                break;
            }
        }
    }
    if outcome.bars_to_audible == u32::MAX {
        outcome.bars_to_audible = 0;
    }
    let stats = telemetry.stats_mut(tier);
    stats.proposals += outcome.applied.len() as u32 + outcome.rejected as u32;
    stats.invalid += outcome.rejected as u32;
    stats.invalid_after_retry += outcome.dropped as u32;
    stats.repairs += repairs_spent;
    outcome
}

/// Validates each raw diff of an emitter batch at its correct anchor (T0
/// ops at the playhead, tempo moves at the section boundary), applies the
/// survivors, and returns the failure problems for the repair round.
fn gate_and_apply(
    engine: &mut ArrangementEngine,
    batch: &[String],
    at_bar: u32,
    outcome: &mut SteeringOutcome,
) -> Vec<String> {
    let mut problems = Vec::new();
    for raw in batch {
        let parsed: Result<IrDiff, _> = serde_json::from_str(raw);
        let Ok(diff) = parsed else {
            problems.push(format!("parse: {raw}"));
            outcome.rejected += 1;
            continue;
        };
        let anchor = match &diff {
            IrDiff::SetTempo { .. } => next_section_boundary(engine.current_session(), at_bar)
                .unwrap_or(at_bar),
            _ => at_bar,
        };
        let mut scratch = engine.current_session().clone();
        let gate = validate_diffs(&mut scratch, anchor, std::slice::from_ref(raw));
        if gate.valid.is_empty() {
            problems.extend(gate.problems);
            outcome.rejected += 1;
            continue;
        }
        match engine.apply_diff(&diff, anchor) {
            Ok(_) => {
                outcome.applied.push(raw.clone());
                outcome.op_classes.push(classify(&diff));
                let audible = match &diff {
                    IrDiff::SetTempo { .. } => anchor.saturating_sub(at_bar),
                    _ => next_block_boundary(at_bar).saturating_sub(at_bar),
                };
                outcome.bars_to_audible = outcome.bars_to_audible.min(audible);
            }
            Err(e) => {
                problems.push(format!("apply: {e}"));
                outcome.rejected += 1;
            }
        }
    }
    problems
}

fn bars_left(session: &Session, at_bar: u32) -> u32 {
    let starts = session.section_start_bars();
    session
        .sections
        .iter()
        .zip(starts.iter())
        .find(|&(sec, start)| at_bar < start + sec.bars)
        .map(|(sec, start)| start + sec.bars - at_bar)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scripted::ScriptedBackend;
    use kontinuum_compose::arrangement::{generate_session, GenParams};

    fn session() -> Session {
        generate_session(&GenParams { seed: 7, target_bars: 32, ..Default::default() })
    }

    fn engine() -> ArrangementEngine {
        ArrangementEngine::new(session(), 48_000)
    }

    fn telemetry() -> ComposerTelemetry {
        ComposerTelemetry::default()
    }

    // -- chips ---------------------------------------------------------------

    #[test]
    fn chip_vocabulary_resolves_exactly_and_case_insensitively() {
        for (text, chip) in QUICK_CHIP_VOCABULARY {
            assert_eq!(QuickChip::from_text(text), Some(chip));
            assert_eq!(QuickChip::from_text(&text.to_ascii_uppercase()), Some(chip));
        }
        assert_eq!(QuickChip::from_text("dark"), None, "no fuzzy matching");
        assert_eq!(QuickChip::from_text("make it darker please"), None, "chips are exact");
    }

    #[test]
    fn every_chip_produces_a_nonzero_predefined_vector() {
        for (_, chip) in QUICK_CHIP_VOCABULARY {
            let v = chip.vector();
            assert!(!v.is_quiet(), "{chip:?} must steer");
            assert!(
                v.energy.abs() <= 1.0 && v.density.abs() <= 1.0 && v.brightness.abs() <= 1.0,
                "{chip:?} vector in range"
            );
        }
    }

    #[test]
    fn vectors_clamp_and_combine() {
        let a = SteeringVector::new(0.8, 0.0, 0.0, 0.0, 0.0, 0.0);
        let b = SteeringVector::new(0.8, 0.0, 0.0, 0.0, 0.0, 0.0);
        assert_eq!(a.combine(b).energy, 1.0, "clamped at +1");
        let mut extreme = SteeringVector::new(9.0, -9.0, 0.0, 0.0, 0.0, 0.0);
        extreme = SteeringVector::new(extreme.energy, extreme.density, 0.0, 0.0, 0.0, 0.0);
        assert_eq!(extreme.energy, 1.0);
        assert_eq!(extreme.density, -1.0);
    }

    // -- scripted NL parser ---------------------------------------------------

    #[test]
    fn scripted_parser_understands_direct_intents_and_compounds() {
        let mut p = ScriptedSteeringProvider;
        let d = p.parse("drop the hats").expect("parse");
        assert_eq!(d.intents, vec![DirectIntent::MuteTrack("perc".into())]);
        let d = p.parse("slower please").expect("parse");
        assert!(d.vector.tempo_drift < 0.0);
        let d = p.parse("darker and slower").expect("parse");
        assert!(d.vector.brightness < 0.0, "darker component present");
        assert!(d.vector.tempo_drift < 0.0, "slower component present");
        assert!(p.parse("what time is it").is_err(), "unknown instructions refuse");
    }

    #[test]
    fn hedge_words_halve_the_deltas() {
        let mut p = ScriptedSteeringProvider;
        let plain = p.parse("bump the energy").unwrap().vector.energy;
        let subtle = p.parse("more energy but keep it subtle").unwrap().vector.energy;
        assert!(subtle > 0.0, "contradiction still steers up");
        assert!(subtle < plain, "subtle halves the magnitude: {subtle} vs {plain}");
    }

    // -- op planning -----------------------------------------------------------

    #[test]
    fn plan_maps_energy_to_section_energy_on_the_live_section() {
        let s = session();
        let d = SteeringDirective {
            vector: SteeringVector::new(0.5, 0.0, 0.0, 0.0, 0.0, 0.0),
            intents: vec![],
            source: SteeringSource::QuickChip,
            notes: "test".into(),
        };
        let plan = plan_ops(&d, &s, 10);
        assert_eq!(plan.t0.len(), 1);
        // The grammar (#16) draws the layout — resolve the section under
        // bar 10 from the session instead of assuming the old 8-bar intro.
        let starts = s.section_start_bars();
        let live_id = starts
            .iter()
            .zip(s.sections.iter())
            .find(|&(start, sec)| 10 >= *start && 10 < start + sec.bars)
            .map(|(_, sec)| sec.id.clone())
            .expect("bar 10 inside the session");
        match &plan.t0[0].diff {
            IrDiff::SetSectionEnergy { id, .. } => {
                assert_eq!(id, &live_id, "bar 10 plays {live_id}");
            }
            other => panic!("unexpected op {other:?}"),
        }
        assert!(plan.t0[0].landing_bar - 10 <= T0_MAX_BARS, "T0 lands within 4 bars");
    }

    #[test]
    fn plan_maps_mute_intent_to_zero_probability_pattern() {
        let s = session();
        let d = SteeringDirective {
            vector: SteeringVector::zero(),
            intents: vec![DirectIntent::MuteTrack("perc".into())],
            source: SteeringSource::QuickChip,
            notes: "test".into(),
        };
        let plan = plan_ops(&d, &s, 2);
        assert_eq!(plan.t0.len(), 1);
        let IrDiff::ReplacePattern { track, pattern, .. } = &plan.t0[0].diff else {
            panic!("expected a replace-pattern mute");
        };
        assert_eq!(track, "perc");
        let kontinuum_ir::schema::Pattern::Steps(steps) = pattern else {
            panic!("expected steps perc pattern");
        };
        assert!(!steps.steps.is_empty());
        assert!(steps.steps.iter().all(|st| st.probability == 0.0), "muted");
    }

    #[test]
    fn plan_maps_tempo_and_transition_to_section_boundaries() {
        let s = session();
        let d = SteeringDirective {
            vector: SteeringVector::new(0.0, 0.0, 0.0, 0.0, -0.5, 0.6),
            intents: vec![],
            source: SteeringSource::Provider,
            notes: "test".into(),
        };
        let plan = plan_ops(&d, &s, 10);
        assert_eq!(plan.composition.len(), 2);
        for op in &plan.composition {
            assert_eq!(op.landing_bar, 12, "next section boundary after bar 10 (dev_0 ends at 12)");
            assert!(op.landing_bar - 10 <= COMPOSITION_MAX_BARS);
        }
        assert!(plan.composition.iter().any(|op| matches!(op.diff, IrDiff::SetTempo { .. })));
        assert!(plan
            .composition
            .iter()
            .any(|op| matches!(op.diff, IrDiff::ScheduleTransition { .. })));
    }

    #[test]
    fn quiet_vector_plans_nothing() {
        let s = session();
        let d = SteeringDirective {
            vector: SteeringVector::zero(),
            intents: vec![],
            source: SteeringSource::QuickChip,
            notes: "test".into(),
        };
        let plan = plan_ops(&d, &s, 4);
        assert!(plan.t0.is_empty() && plan.composition.is_empty());
    }

    // -- validated application -------------------------------------------------

    #[test]
    fn chip_steering_applies_through_the_real_gate_and_updates_telemetry() {
        let mut engine = engine();
        let mut telemetry = telemetry();
        let plan = plan_ops(
            &SteeringDirective::chip(QuickChip::MoreEnergy),
            engine.current_session(),
            4,
        );
        assert!(!plan.t0.is_empty(), "more energy plans an energy move");
        let outcome = apply_plan(&mut engine, &plan, 4, &mut telemetry, Tier::T0Deterministic);
        assert!(!outcome.applied.is_empty());
        assert_eq!(outcome.rejected, 0);
        assert!(outcome.op_classes.contains(&OpClass::Energy));
        assert!(outcome.bars_to_audible <= T0_MAX_BARS, "T0 audible within 4 bars");
        assert_eq!(telemetry.t0.proposals, plan.t0.len() as u32 + plan.composition.len() as u32);
        assert_eq!(telemetry.t0.invalid, 0);
    }

    #[test]
    fn invalid_ops_are_dropped_logged_and_counted_per_tier() {
        let mut engine = engine();
        let mut telemetry = telemetry();
        // An op targeting a section that has fully ended is past: the gate
        // rejects it, the plan applier drops and logs it.
        let plan = SteeringPlan {
            t0: vec![PlannedOp {
                landing_bar: next_block_boundary(20),
                diff: IrDiff::ReplacePattern {
                    section: "intro".into(),
                    track: "kick".into(),
                    pattern: kontinuum_ir::schema::Pattern::Euclidean(
                        kontinuum_ir::schema::EuclideanPattern {
                            generator: kontinuum_ir::schema::EuclideanTag::Euclidean,
                            k: 4,
                            n: 16,
                            rot: 0,
                            velocity: 0.9,
                            probability: 1.0,
                            repeats: 1,
                            gate: None,
                            pitch: None,
                        },
                    ),
                },
            }],
            composition: vec![],
        };
        let outcome = apply_plan(&mut engine, &plan, 20, &mut telemetry, Tier::T0Deterministic);
        assert!(outcome.applied.is_empty());
        assert_eq!(outcome.dropped, 1, "the past-targeting op is dropped");
        assert_eq!(telemetry.t0.invalid, 1);
        assert!(telemetry.t0.invalid_rate() > 0.0);
    }

    #[test]
    fn mute_of_a_missing_track_is_a_no_op_not_a_crash() {
        let mut engine = engine();
        let mut telemetry = telemetry();
        let d = SteeringDirective {
            vector: SteeringVector::zero(),
            intents: vec![DirectIntent::MuteTrack("clap".into())],
            source: SteeringSource::Provider,
            notes: "test".into(),
        };
        let plan = plan_ops(&d, engine.current_session(), 4);
        assert!(plan.t0.is_empty(), "no clap binding: nothing to mute");
        let outcome = apply_plan(&mut engine, &plan, 4, &mut telemetry, Tier::T1OnDevice);
        assert!(outcome.applied.is_empty());
    }

    // -- full steering wake (provider path) --------------------------------------

    fn ops_of(directive: &SteeringDirective, engine: &ArrangementEngine, at_bar: u32) -> Vec<String> {
        let plan = plan_ops(directive, engine.current_session(), at_bar);
        plan.t0
            .iter()
            .chain(plan.composition.iter())
            .map(|op| serde_json::to_string(&op.diff).unwrap())
            .collect()
    }

    #[test]
    fn provider_path_applies_scripted_emission_with_context_document_riding_along() {
        let mut engine = engine();
        let at_bar = 4;
        let directive = SteeringDirective {
            vector: SteeringVector::new(0.4, 0.0, 0.0, 0.0, 0.0, 0.0),
            intents: vec![],
            source: SteeringSource::Provider,
            notes: "scripted".into(),
        };
        let mut emitter = ScriptedBackend::new(
            "t1-scripted",
            vec![ops_of(&directive, &engine, at_bar)],
        );
        let mut telemetry = telemetry();
        let outcome = run_steering(
            &mut engine,
            &mut ScriptedSteeringProvider,
            &mut emitter,
            at_bar,
            "bump the energy for me",
            ContextInputs::default(),
            &mut telemetry,
        );
        assert!(!outcome.applied.is_empty());
        assert_eq!(outcome.repairs, 0);
        assert_eq!(telemetry.t1.proposals >= 1, true);
        // The wake carries the versioned context document to the emitter.
        let req = emitter.last_request().expect("emitter saw the request");
        assert!(req.style_card.starts_with("ctx v1"));
        assert!(req.prompt.contains("bump the energy"));
    }

    #[test]
    fn repair_loop_retries_once_with_validator_errors_then_drops() {
        let mut engine = engine();
        let at_bar = 4;
        let good = ops_of(
            &SteeringDirective {
                vector: SteeringVector::new(0.4, 0.0, 0.0, 0.0, 0.0, 0.0),
                intents: vec![],
                source: SteeringSource::Provider,
                notes: "scripted".into(),
            },
            &engine,
            at_bar,
        );
        let bad = vec![r#"{"op":"set_instrument_param","track":"kick","param":"decay_ms","value":99999.0}"#.to_string()];
        let mut emitter = ScriptedBackend::new("t1-scripted", vec![bad, good]);
        let mut telemetry = telemetry();
        let outcome = run_steering(
            &mut engine,
            &mut ScriptedSteeringProvider,
            &mut emitter,
            at_bar,
            "bump the energy",
            ContextInputs::default(),
            &mut telemetry,
        );
        assert_eq!(outcome.repairs, 1, "exactly one self-correction retry");
        assert!(!outcome.applied.is_empty(), "the corrected batch lands");
        assert!(
            emitter
                .last_request()
                .expect("repair request")
                .repair_context
                .contains("E_KICK_DECAY_RANGE"),
            "validator {{code, path, suggested_fix}} errors feed the retry"
        );
        assert_eq!(telemetry.t1.repairs, 1);
    }

    #[test]
    fn unrepairable_batch_is_dropped_and_the_session_continues() {
        let mut engine = engine();
        let before = engine.current_session().clone();
        let bad = vec![
            r#"{"op":"set_instrument_param","track":"kick","param":"decay_ms","value":99999.0}"#.to_string(),
        ];
        let mut emitter = ScriptedBackend::new("t1-scripted", vec![bad]);
        let mut telemetry = telemetry();
        let outcome = run_steering(
            &mut engine,
            &mut ScriptedSteeringProvider,
            &mut emitter,
            4,
            "bump the energy",
            ContextInputs::default(),
            &mut telemetry,
        );
        assert_eq!(outcome.applied.len(), 0);
        assert_eq!(outcome.dropped, 1, "second failure drops the diff");
        assert_eq!(outcome.repairs, 1);
        assert_eq!(engine.current_session(), &before, "dropped diffs never touch the session");
        assert_eq!(telemetry.t1.invalid_after_retry, 1);
    }

    #[test]
    fn chip_instruction_never_consults_the_provider_or_emitter() {
        let mut engine = engine();
        let mut emitter = ScriptedBackend::new("t1-scripted", vec![vec!["{ not json }".to_string()]]);
        struct Refusing;
        impl SteeringProvider for Refusing {
            fn parse(&mut self, _i: &str) -> Result<SteeringDirective, SteeringError> {
                Err(SteeringError::NotUnderstood)
            }
        }
        let mut telemetry = telemetry();
        let outcome = run_steering(
            &mut engine,
            &mut Refusing,
            &mut emitter,
            4,
            "darker",
            ContextInputs::default(),
            &mut telemetry,
        );
        assert!(!outcome.applied.is_empty(), "the chip path works with no model at all");
        assert_eq!(emitter.calls(), 0, "chips are instant: no LLM call");
        assert_eq!(outcome.tier, Some(Tier::T0Deterministic));
    }

    #[test]
    fn unknown_instruction_is_logged_and_ignored() {
        let mut engine = engine();
        let before = engine.current_session().clone();
        let mut emitter = ScriptedBackend::new("t1-scripted", vec![]);
        let mut telemetry = telemetry();
        let outcome = run_steering(
            &mut engine,
            &mut ScriptedSteeringProvider,
            &mut emitter,
            4,
            "what does the fox say",
            ContextInputs::default(),
            &mut telemetry,
        );
        assert!(outcome.applied.is_empty());
        assert_eq!(outcome.tier, None);
        assert_eq!(engine.current_session(), &before);
        assert_eq!(emitter.calls(), 0, "a parse miss never spends a model call");
    }
}
