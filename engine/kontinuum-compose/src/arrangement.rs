//! Hierarchical arrangement engine (issue #16): turns a seed into a
//! structured [`Session`] in the style the genre spec names (issue #87).
//!
//! Structure is drawn, not templated: the seeded skeleton fixes intro and
//! outro, draws the style's dev/breakdown count range, and always closes
//! with a full reintroduction. Middle sections are then scaled so the
//! session sums to [`GenParams::target_bars`]. Per-section energy curves
//! (0..=1) drive density, velocity, binding sparsity, transitions, and
//! fills; all randomness derives from [`kontinuum_clock::stream`], so the
//! same seed reproduces the same session.
//!
//! Sections bind by *class* over whatever the genre's rack contains (#88) —
//! the spine, the pulse, a low voice — never by hardcoded track id, so a
//! kickless ambient rack and a six-track deep-house rig take the same path.

use std::collections::BTreeMap;

use kontinuum_clock::{stream, Rng};
use kontinuum_ir::schema::{
    bounds, AutomationLane, CurveKind, MusicalKey, Section, Session, Transition, TransitionKind,
};
use kontinuum_ir::IR_VERSION;
use serde_json::json;

use crate::genre::{spec_for, BindClass, GenreSpec, RackEntry, Style, Voice};
use crate::motion;
use crate::palette;
use crate::presence;

const INTRO_BARS: u32 = 8;
const REINTRO_BARS: u32 = 8;
const OUTRO_BARS: u32 = 8;
const FIXED_BARS: u32 = INTRO_BARS + REINTRO_BARS + OUTRO_BARS;

/// Bars of music that must contain a near-solo passage (≈5 min at 124 BPM):
/// issue #52 workstream 1 — the record breathes only if passages exist
/// where almost everything is silent.
const BARS_PER_NEAR_SOLO: u32 = 160;

/// RNG stream selectors for `kontinuum_clock::stream(seed, lane, purpose)`.
const LANE_STRUCTURE: u8 = 0xFF;
const PURPOSE_STRUCTURE: u16 = 0xA0;
const PURPOSE_SECTION: u16 = 0xA1;
const PURPOSE_TRANSITION: u16 = 0xA2;
const PURPOSE_VARIATION: u16 = 0xA3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Intro,
    /// The main body: the style's groove, developed.
    Dev,
    /// Rising pressure into a release or breakdown.
    Tension,
    /// The payoff after tension — full stack, peak drive.
    Release,
    Breakdown,
    Reintro,
    /// A reshaped return of known material (motif transforms, thinning).
    Variation,
    Outro,
}

impl Kind {
    /// The label the parameter file and corpus artifacts use.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Intro => "intro",
            Kind::Dev => "groove_dev",
            Kind::Tension => "tension",
            Kind::Release => "release",
            Kind::Breakdown => "breakdown",
            Kind::Reintro => "reintro",
            Kind::Variation => "variation",
            Kind::Outro => "outro",
        }
    }

    /// The section-id prefix the planner emits for the kind (`dev_0`,
    /// `tension_1`, …); Intro/Reintro/Outro carry fixed ids.
    fn id_prefix(self) -> Option<&'static str> {
        match self {
            Kind::Intro | Kind::Reintro | Kind::Outro => None,
            Kind::Dev => Some("dev"),
            Kind::Tension => Some("tension"),
            Kind::Release => Some("release"),
            Kind::Breakdown => Some("break"),
            Kind::Variation => Some("variation"),
        }
    }

    pub fn from_label(label: &str) -> Option<Kind> {
        match label {
            "intro" => Some(Kind::Intro),
            "groove_dev" | "dev" => Some(Kind::Dev),
            "tension" => Some(Kind::Tension),
            "release" => Some(Kind::Release),
            "breakdown" => Some(Kind::Breakdown),
            "reintro" => Some(Kind::Reintro),
            "variation" => Some(Kind::Variation),
            "outro" => Some(Kind::Outro),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SectionPlan {
    pub(crate) id: String,
    pub(crate) kind: Kind,
    pub(crate) bars: u32,
    /// (start, end) windows for the three coupled curves (#16 energy
    /// model): drive, event count, spectral colour. Curves interpolate
    /// linearly across the section with seeded per-bar wobble.
    pub(crate) energy: (f32, f32),
    pub(crate) density: (f32, f32),
    pub(crate) brightness: (f32, f32),
    pub(crate) fill: bool,
    pub(crate) sparse_kick: bool,
    /// Near-solo passage: the groove carries alone (issue #52 WS1).
    pub(crate) stripped: bool,
}

/// Generation knobs for [`generate_session`].
#[derive(Clone, Debug)]
pub struct GenParams {
    pub seed: u64,
    /// Total session length in bars (sections are scaled to match).
    pub target_bars: u32,
    /// Tempo override. `None` takes the genre's own tempo, which is the usual
    /// case — a style is partly defined by the speed it runs at, and pinning
    /// every genre to one BPM was a large part of why they all sounded alike.
    pub bpm: Option<f64>,
    /// Global energy bias, 0..1 (0 = muted arc, 1 = pushed arc).
    pub intensity: f32,
    /// Genre flavor; resolved through [`crate::genre::spec_for`], which owns
    /// tempo band, swing, groove pool, rack, key tendencies and structure
    /// tendencies for the style.
    pub genre: Option<String>,
    /// Groove template name (see [`crate::groove::ALL`]); `None` draws from
    /// the genre's groove pool.
    pub groove: Option<String>,
    /// 0..1 — how much of the rig plays at once (binding odds) and how many
    /// events per bar (hat/pluck onset budgets). Scales the genre's own
    /// tendencies rather than replacing them; 0.6 is the neutral centre.
    pub density: f32,
    /// 0..1 — how much the arrangement changes between sections (fills,
    /// redrawn figures).
    pub variation: f32,
    /// 0..1 — how dark the harmony sits; biases the progression toward the
    /// all-minor templates.
    pub darkness: f32,
    /// Bass archetype name pin (see [`crate::bass::ALL`]); `None` draws from
    /// the genre's bass pool, seeded once per session so the whole record
    /// rides one coherent low-end idiom.
    pub bass_archetype: Option<String>,
    /// Corpus groove vocabulary (#23): when loaded, grooves are drawn from
    /// it instead of the hand-made six.
    pub groove_bank: Option<crate::groove::GrooveBank>,
    /// Corpus arrangement structure (#23): when loaded, section lengths and
    /// dev energies follow the fit; `None` keeps the hand-seeded defaults.
    pub structure: Option<crate::structure::StructureParams>,
    /// Director's energy-arc family pin (#16): `None` draws seeded from
    /// the grammar's arc-family table.
    pub arc: Option<crate::grammar::ArcFamily>,
    /// Curated sound world (#30): when set, its palette/mix overrides layer
    /// on top of the genre rig (after `palette::tracks_for_genre`) and the
    /// session's palette stamp names the world.
    pub world: Option<crate::world::SoundWorld>,
    /// Creative Soul stack (issue #55): blended identity packs layered
    /// between the genre rig and an explicit world. Entries with an
    /// out-of-range weight or an era the pack does not ship are dropped
    /// deterministically (see `soul::blend::prepare`); an empty stack keeps
    /// the output byte-identical.
    pub souls: Vec<crate::soul::SoulStackEntry>,
}

impl Default for GenParams {
    fn default() -> Self {
        GenParams {
            seed: 1,
            target_bars: 128,
            bpm: None,
            intensity: 0.5,
            genre: None,
            groove: None,
            density: 0.6,
            variation: 0.5,
            darkness: 0.7,
            bass_archetype: None,
            groove_bank: None,
            structure: None,
            arc: None,
            world: None,
            souls: Vec::new(),
        }
    }
}

/// Generates a deterministic, validation-clean session from `params`.
pub fn generate_session(params: &GenParams) -> Session {
    let intensity = params.intensity.clamp(0.0, 1.0);
    let density = params.density.clamp(0.0, 1.0);
    let variation = params.variation.clamp(0.0, 1.0);
    let mut spec = spec_for(params.genre.as_deref());
    // Density nudges the genre's own binding odds rather than replacing them,
    // centred on the default so an unset profile changes nothing.
    let density_shift = (density - 0.6) * 0.5;
    let nudge = |p: f32| (p + density_shift).clamp(0.05, 1.0);
    spec.dev_bind.low = nudge(spec.dev_bind.low);
    spec.dev_bind.pulse = nudge(spec.dev_bind.pulse);
    spec.dev_bind.harmony = nudge(spec.dev_bind.harmony);
    spec.dev_bind.texture = nudge(spec.dev_bind.texture);
    let bpm = params.bpm.unwrap_or(spec.bpm).clamp(60.0, 200.0);
    let mut rng = stream(params.seed, LANE_STRUCTURE, PURPOSE_STRUCTURE);
    let soul_prepared = crate::soul::prepare(&params.souls);
    let blended = soul_prepared.blended.as_ref();
    // Swing and groove belong to the style, not to a coin flip. Drawing them
    // freely meant a "techno" session could come out shuffled and a "house"
    // one stiff, which is most of what a listener uses to tell the two apart.
    // The style's swing is a *range*, drawn seeded per session (straight
    // styles have a zero range and stay straight); the groove comes from the
    // style's template pool. A blended soul's groove layer outranks the spec
    // where it names values; an explicit request outranks both.
    let mut swing = rng.range_f32(spec.swing.0, spec.swing.1);
    if let Some(sw) = blended.and_then(|b| b.groove.as_ref()).and_then(|g| g.swing) {
        swing = sw;
    }
    let soul_groove_template =
        blended.and_then(|b| b.groove.as_ref()).and_then(|g| g.template.clone());
    let groove_name = match params.groove.clone().or(soul_groove_template) {
        Some(name) => Some(name),
        None => Some(
            spec.grooves[rng.below(spec.grooves.len().max(1) as u64) as usize].to_string(),
        ),
    };
    let groove_name_ref = groove_name.as_deref();
    // Key tendency: the seeded pick stamps the session and transposes the
    // whole progression (issue #87).
    let key = spec.keys[rng.below(spec.keys.len().max(1) as u64) as usize];
    // Low-end idiom: the seeded pick from the style's bass pool rides the
    // whole record (the legacy free draw remains for pool-less specs).
    let bass_archetype = params.bass_archetype.clone().or_else(|| {
        if spec.bass_pool.is_empty() {
            None
        } else {
            Some(spec.bass_pool[rng.below(spec.bass_pool.len() as u64) as usize].to_string())
        }
    });
    let (mut plans, grammar) = plan_structure(
        &mut rng,
        params.target_bars.max(FIXED_BARS),
        intensity,
        spec,
        params.structure.as_ref(),
        blended.and_then(|b| b.arrangement.as_ref()),
        params.arc,
    );
    // Transition catalog (#16): one recipe per out-edge, selected on
    // (from_kind, to_kind, Δenergy) from the grammar's tables. A Fill
    // recipe also bakes the boundary fill into the departing section's
    // patterns (issue #17's generator owns that bar).
    let mut transition_out: Vec<Option<Transition>> = Vec::with_capacity(plans.len());
    {
        let mut trans_rng = stream(params.seed, LANE_STRUCTURE, PURPOSE_TRANSITION);
        for i in 0..plans.len() {
            let Some(to) = plans.get(i + 1).map(|p| p.kind) else {
                transition_out.push(None);
                continue;
            };
            let delta = plans[i + 1].energy.0 - plans[i].energy.1;
            let mut picked = crate::transitions::pick(&grammar, plans[i].kind, to, delta, &mut trans_rng);
            // The variation knob rides on top of the catalog: inside the
            // groove block it can still demand a boundary fill when the
            // table drew something quieter.
            if picked.as_ref().map(|t| t.kind) != Some(TransitionKind::Fill)
                && matches!(plans[i].kind, Kind::Dev | Kind::Variation)
                && matches!(to, Kind::Dev | Kind::Variation)
                && rng.chance(0.5 * variation)
            {
                picked = Some(crate::transitions::emit(
                    TransitionKind::Fill,
                    1,
                    plans[i].kind,
                    to,
                    delta,
                    &mut trans_rng,
                ));
            }
            if picked.as_ref().map(|t| t.kind) == Some(TransitionKind::Fill) {
                plans[i].fill = true;
            }
            transition_out.push(picked);
        }
    }
    let groove = match params.groove_bank.as_ref() {
        Some(bank) => bank
            .pick(groove_name_ref, intensity, &mut rng)
            .map(crate::groove::ActiveGroove::from_corpus)
            .unwrap_or_else(|| crate::groove::ActiveGroove::from_static(crate::groove::pick(groove_name_ref, &mut rng))),
        None => crate::groove::ActiveGroove::from_static(crate::groove::pick(groove_name_ref, &mut rng)),
    };
    let deep = spec.style == Style::DeepHouse;
    let mut sections: Vec<Section> = plans
        .iter()
        .enumerate()
        .map(|(si, plan)| {
            {
                let mut section = build_section(
                    plan,
                    si,
                    swing,
                    spec,
                    &groove,
                    bass_archetype.as_deref(),
                    density,
                    params.seed,
                    transition_out[si].clone(),
                );
                if deep {
                    apply_deep_house_section(&mut section);
                }
                section
            }
        })
        .collect();
    // Motif memory (#16): the first introduction of each track's figure is
    // stored under a stable id; `reintro` rebuilds its bound tracks from
    // stored motifs run through the seeded transform, so material returns
    // changed. Tracks a reintro binds that never introduced a motif keep
    // their fresh draw.
    let mut motifs = crate::motif::MotifMemory::new();
    for (si, (plan, section)) in plans.iter().zip(sections.iter_mut()).enumerate() {
        match plan.kind {
            Kind::Dev => {
                for (track, pattern) in &section.pattern_bindings {
                    motifs.observe(track, pattern, &plan.id);
                }
            }
            Kind::Reintro => {
                let mut rng = stream(params.seed, si as u8, PURPOSE_SECTION);
                for (track, pattern) in section.pattern_bindings.iter_mut() {
                    if let Some(m) = motifs.motif_for(track) {
                        *pattern = crate::motif::request_and_apply(&m.pattern, &mut rng);
                    }
                }
            }
            _ => {}
        }
    }
    // Chord-aware pass: sections follow a progression (issue #46) in the
    // style's key; a soul's harmony layer (issue #55) replaces the engine's
    // tables when present. Melody voices transpose, chord voices re-voice.
    let melody: Vec<&str> =
        spec.rack.iter().filter(|e| e.class == BindClass::Low).map(|e| e.id).collect();
    let poly: Vec<&str> =
        spec.rack.iter().filter(|e| e.class == BindClass::Harmony).map(|e| e.id).collect();
    match blended.and_then(|b| b.progressions.as_ref()) {
        Some(prog) => crate::harmony::apply_progression_with(
            &mut sections,
            params.seed,
            prog,
            &melody,
            &poly,
            spec.harmony_color,
        ),
        None => crate::harmony::apply_progression(
            &mut sections,
            params.seed,
            params.darkness.clamp(0.0, 1.0),
            tonic_pc(key),
            &melody,
            &poly,
            spec.harmony_color,
        ),
    }
    // Presence arcs: the session breathes (issue #52 WS1).
    let mut tracks = palette::tracks_for_genre(params.genre.as_deref());
    // Layering (issue #55): genre rig -> blended souls -> explicit world ->
    // user diffs. A world names its fields explicitly, so it wins on them.
    if let Some(b) = blended {
        crate::soul::apply_to_tracks(&mut tracks, b);
    }
    if let Some(world) = params.world.as_ref() {
        crate::world::apply_to_tracks(&mut tracks, world);
    }
    let stripped: Vec<String> =
        plans.iter().filter(|p| p.stripped).map(|p| p.id.clone()).collect();
    // Long-horizon variation policy (#16): per-phrase automation gestures
    // on slots still free after the section build, plus one ghost refresh
    // per section at the schedule's peak boost. Intensity follows the
    // density curve; ghost steps keep sub-1.0 probability, and the
    // compile-time per-hit gate re-rolls every bar, so no two bars of the
    // loop are identical.
    {
        let mut var_rng = stream(params.seed, LANE_STRUCTURE, PURPOSE_VARIATION);
        for (plan, sec) in plans.iter().zip(sections.iter_mut()) {
            if !matches!(plan.kind, Kind::Dev | Kind::Variation | Kind::Tension | Kind::Release) {
                continue;
            }
            let density_mid = (plan.density.0 + plan.density.1) * 0.5;
            let phrase_bars = grammar.constraints.phrase_bars;
            let schedule = crate::variation::schedule(
                plan.bars,
                phrase_bars,
                density_mid,
                variation,
                &mut var_rng,
            );
            for t in &schedule {
                let Some(gesture) = t.gesture else { continue };
                let phrase_start = t.phrase * phrase_bars;
                if phrase_start + phrase_bars > plan.bars {
                    continue;
                }
                if let Some(track) = sec
                    .pattern_bindings
                    .keys()
                    .find(|id| {
                        // The percussive spine's send vocabulary belongs to
                        // the motion layer; variation gestures land on the
                        // harmonic and colour tracks.
                        !matches!(id.as_str(), "kick" | "clap" | "perc" | "shaker" | "hat" | "ohat" | "snare")
                            && !sec.automation.contains_key(*id)
                    })
                    .cloned()
                {
                    let lane =
                        crate::variation::gesture_lane(gesture, phrase_start, phrase_bars, &mut var_rng);
                    sec.automation.insert(track, lane);
                }
            }
            let peak_boost = schedule.iter().map(|t| t.ghost_boost).fold(1.0f32, f32::max);
            // The variation knob sets how far the ghosts move: it scales
            // the refresh magnitude deterministically, so higher variation
            // always reads as more per-phrase change.
            let boost = (peak_boost * (0.75 + 0.5 * variation)).max(1.2);
            if let Some((_, pattern)) = sec
                .pattern_bindings
                .iter_mut()
                .find(|(id, _)| matches!(id.as_str(), "perc" | "shaker" | "hat" | "stab"))
            {
                crate::variation::apply_ghost_refresh(pattern, boost);
            }
        }
    }
    // Motion lanes (#16): macro send/gain gestures claim slots first so
    // presence composes around them instead of overwriting them.
    motion::apply_motion(&mut sections, &tracks, params.seed);
    presence::apply_presence(&mut sections, &tracks, &stripped, params.seed);
    Session {
        version: IR_VERSION,
        seed: params.seed,
        tempo_lane: vec![(0, bpm)],
        key: Some(key.key_hint()),
        souls: (!soul_prepared.refs.is_empty()).then(|| soul_prepared.refs.clone()),
        send_fx: None,
        pattern_engine: Some(kontinuum_ir::schema::PatternEngine {
            groove: groove_name_ref.map(str::to_string),
            swing: swing.clamp(kontinuum_ir::schema::bounds::GROOVE_SWING.0, kontinuum_ir::schema::bounds::GROOVE_SWING.1),
            bias_ticks: groove.bias_ticks().clamp(
                kontinuum_ir::schema::bounds::GROOVE_BIAS_TICKS.0,
                kontinuum_ir::schema::bounds::GROOVE_BIAS_TICKS.1,
            ),
            jitter_ticks: groove.jitter_ticks().clamp(
                kontinuum_ir::schema::bounds::GROOVE_JITTER_TICKS.0,
                kontinuum_ir::schema::bounds::GROOVE_JITTER_TICKS.1,
            ),
            bass_archetype: bass_archetype
                .clone()
                .filter(|n| crate::bass::ALL.iter().any(|a| a.name() == *n)),
            downbeat_collision: spec.downbeat_collision,
        }),
        sections,
        tracks,
        palette: params
            .world
            .as_ref()
            .map(|w| json!({ "world": w.id.0.clone() })),
        duck_release_ms: spec.duck_release_ms,
    }
}

/// Pitch class of a [`MusicalKey`] tonic (the progression's degree zero).
fn tonic_pc(key: MusicalKey) -> u8 {
    use MusicalKey::*;
    match key {
        CMajor | CMinor => 0,
        CSharpMajor | CSharpMinor => 1,
        DMajor | DMinor => 2,
        DSharpMajor | DSharpMinor => 3,
        EMajor | EMinor => 4,
        FMajor | FMinor => 5,
        FSharpMajor | FSharpMinor => 6,
        GMajor | GMinor => 7,
        GSharpMajor | GSharpMinor => 8,
        AMajor | AMinor => 9,
        ASharpMajor | ASharpMinor => 10,
        BMajor | BMinor => 11,
    }
}

#[allow(clippy::too_many_arguments)]
fn build_section(
    plan: &SectionPlan,
    si: usize,
    swing: f32,
    spec: GenreSpec,
    groove: &crate::groove::ActiveGroove,
    bass_archetype: Option<&str>,
    density: f32,
    seed: u64,
    transition_out: Option<Transition>,
) -> Section {
    let mut rng = stream(seed, si as u8, PURPOSE_SECTION);
    let energy = (plan.energy.0 + plan.energy.1) * 0.5;
    let density_curve = (plan.density.0 + plan.density.1) * 0.5;
    let brightness_curve = (plan.brightness.0 + plan.brightness.1) * 0.5;
    // The density curve rides the global knob multiplicatively, centered
    // so the base file's ~0.6 windows reproduce the knob alone.
    let density_value = (density * (density_curve / 0.6)).clamp(0.05, 1.0);
    let rack = spec.rack;
    let of_class = |class: BindClass| -> Vec<&RackEntry> {
        rack.iter().filter(|e| e.class == class).collect()
    };
    let mut bindings: BTreeMap<String, _> = BTreeMap::new();
    let build = |entry: &RackEntry, bindings: &mut BTreeMap<String, _>, rng: &mut Rng| {
        let pattern = build_pattern(entry, plan, energy, swing, density_value, brightness_curve, spec, groove, bass_archetype, rng);
        bindings.insert(entry.id.to_string(), pattern);
    };

    match plan.kind {
        Kind::Intro => {
            for e in of_class(BindClass::Spine) {
                build(e, &mut bindings, &mut rng);
            }
            // The backbeat arrives with the first lift, not on bar one.
            if energy > 0.4 {
                for e in of_class(BindClass::Backbeat) {
                    build(e, &mut bindings, &mut rng);
                }
            }
            for e in of_class(BindClass::Pulse) {
                build(e, &mut bindings, &mut rng);
            }
            for e in of_class(BindClass::Low) {
                build(e, &mut bindings, &mut rng);
            }
            // Beatless racks open on a single pad over the texture.
            if bindings.is_empty() {
                if let Some(e) = of_class(BindClass::Harmony).first() {
                    build(e, &mut bindings, &mut rng);
                }
                if let Some(e) = of_class(BindClass::Texture).first() {
                    build(e, &mut bindings, &mut rng);
                }
            }
        }
        Kind::Dev if plan.stripped => {
            let spine = of_class(BindClass::Spine);
            if let Some(kick) = spine.first() {
                build(kick, &mut bindings, &mut rng);
                let low = of_class(BindClass::Low);
                let pulse = of_class(BindClass::Pulse);
                let companion =
                    if rng.chance(0.5) { low.first() } else { pulse.first() }
                        .or_else(|| low.first().or(pulse.first()));
                if let Some(e) = companion {
                    build(e, &mut bindings, &mut rng);
                }
            } else {
                // A beatless near-solo: one harmony voice carries the passage.
                let harmony = of_class(BindClass::Harmony);
                let texture = of_class(BindClass::Texture);
                let lone = if rng.chance(0.5) { harmony.first() } else { texture.first() }
                    .or_else(|| harmony.first().or(texture.first()));
                if let Some(e) = lone {
                    build(e, &mut bindings, &mut rng);
                }
            }
        }
        Kind::Dev | Kind::Variation => {
            for e in of_class(BindClass::Spine) {
                build(e, &mut bindings, &mut rng);
            }
            for e in of_class(BindClass::Backbeat) {
                build(e, &mut bindings, &mut rng);
            }
            // Dev and variation sections roll the style's binding odds per
            // entry (class prob × entry chance).
            let mut supports: Vec<(usize, &RackEntry)> = rack
                .iter()
                .enumerate()
                .filter(|(_, e)| !matches!(e.class, BindClass::Spine | BindClass::Backbeat))
                .filter(|(_, e)| rng.chance((spec.bind_prob(e.class) * e.chance).clamp(0.0, 1.0)))
                .collect();
            // Concurrency cap (issue #52 WS1): restraint is the sound. Overage
            // comes off the harmonic and colour layers first — the groove
            // spine is never the thing that gets dropped.
            let bound_always = of_class(BindClass::Spine).len() + of_class(BindClass::Backbeat).len();
            let cap = spec.max_concurrent.saturating_sub(bound_always).max(1);
            if supports.len() > cap {
                supports.sort_by(|(ia, a), (ib, b)| {
                    a.class
                        .drop_rank()
                        .cmp(&b.class.drop_rank())
                        .then_with(|| a.chance.total_cmp(&b.chance))
                        .then_with(|| ib.cmp(ia))
                });
                supports.truncate(cap);
            }
            for (_, e) in supports {
                build(e, &mut bindings, &mut rng);
            }
        }
        Kind::Tension => {
            // The build: the groove holds, harmony is forced in, colour
            // layers roll their odds on top.
            for e in of_class(BindClass::Spine) {
                build(e, &mut bindings, &mut rng);
            }
            for e in of_class(BindClass::Backbeat) {
                build(e, &mut bindings, &mut rng);
            }
            for e in of_class(BindClass::Harmony) {
                build(e, &mut bindings, &mut rng);
            }
            let bound_always = of_class(BindClass::Spine).len()
                + of_class(BindClass::Backbeat).len()
                + of_class(BindClass::Harmony).len();
            let cap = spec.max_concurrent.saturating_sub(bound_always).max(1);
            let mut extras: Vec<(usize, &RackEntry)> = rack
                .iter()
                .enumerate()
                .filter(|(_, e)| {
                    !matches!(e.class, BindClass::Spine | BindClass::Backbeat | BindClass::Harmony)
                })
                .filter(|(_, e)| rng.chance((spec.bind_prob(e.class) * e.chance).clamp(0.0, 1.0)))
                .collect();
            if extras.len() > cap {
                extras.sort_by(|(ia, a), (ib, b)| {
                    a.class.drop_rank().cmp(&b.class.drop_rank()).then_with(|| ib.cmp(ia))
                });
                extras.truncate(cap);
            }
            for (_, e) in extras {
                build(e, &mut bindings, &mut rng);
            }
        }
        Kind::Release | Kind::Reintro => {
            // The payoff and the reintroduction mean everything, all at
            // once — within the style's concurrency cap (#52 WS1).
            for e in of_class(BindClass::Spine) {
                build(e, &mut bindings, &mut rng);
            }
            for e in of_class(BindClass::Backbeat) {
                build(e, &mut bindings, &mut rng);
            }
            let mut supports: Vec<(usize, &RackEntry)> = rack
                .iter()
                .enumerate()
                .filter(|(_, e)| !matches!(e.class, BindClass::Spine | BindClass::Backbeat))
                .collect();
            let bound_always = of_class(BindClass::Spine).len() + of_class(BindClass::Backbeat).len();
            let cap = spec.max_concurrent.saturating_sub(bound_always).max(1);
            if supports.len() > cap {
                supports.sort_by(|(ia, a), (ib, b)| {
                    a.class
                        .drop_rank()
                        .cmp(&b.class.drop_rank())
                        .then_with(|| a.chance.total_cmp(&b.chance))
                        .then_with(|| ib.cmp(ia))
                });
                supports.truncate(cap);
            }
            for (_, e) in supports {
                build(e, &mut bindings, &mut rng);
            }
        }
        Kind::Breakdown => {
            // Depth: breakdowns collapse to 1–2 elements, never "all minus
            // kick". The chord voice holds; the second element is the sparse
            // kick, the low end — or, on a beatless rack, the texture.
            if let Some(e) = of_class(BindClass::Harmony).first() {
                build(e, &mut bindings, &mut rng);
            }
            if rng.below(3) > 0 {
                let second = if plan.sparse_kick {
                    of_class(BindClass::Spine).first().map(|e| (*e, ()))
                } else {
                    None
                }
                .or_else(|| of_class(BindClass::Low).first().map(|e| (*e, ())))
                .or_else(|| of_class(BindClass::Texture).first().map(|e| (*e, ())));
                if let Some((e, ())) = second {
                    build(e, &mut bindings, &mut rng);
                }
            }
        }
        Kind::Outro => {
            for e in of_class(BindClass::Spine) {
                build(e, &mut bindings, &mut rng);
            }
            for e in of_class(BindClass::Backbeat) {
                build(e, &mut bindings, &mut rng);
            }
            for e in of_class(BindClass::Pulse) {
                build(e, &mut bindings, &mut rng);
            }
            if let Some(e) = of_class(BindClass::Harmony).first() {
                build(e, &mut bindings, &mut rng);
            }
        }
    }
    // A rolled-out section binds nothing only if every draw failed; never
    // emit a silent section (the validator rejects it).
    if bindings.is_empty() {
        if let Some(e) = rack.first() {
            build(e, &mut bindings, &mut rng);
        }
    }
    // Gentle reverb swell on a bound chord voice — the pad lane's original
    // home, generalized to whatever harmony track this rack performs.
    let automation = if matches!(plan.kind, Kind::Dev | Kind::Reintro) && rng.chance(0.5) {
        of_class(BindClass::Harmony)
            .iter()
            .find(|e| bindings.contains_key(e.id))
            .map(|e| pad_reverb_lane(e.id, plan.bars, &mut rng))
            .unwrap_or_default()
    } else {
        BTreeMap::new()
    };
    let mut section = Section {
        id: plan.id.clone(),
        bars: plan.bars,
        energy_curve: curve(plan.energy, plan.bars, &mut rng),
        density_curve: curve(plan.density, plan.bars, &mut rng),
        brightness_curve: curve(plan.brightness, plan.bars, &mut rng),
        transition_in: None,
        transition_out,
        pattern_bindings: bindings,
        automation,
    };
    apply_hat_choke(&mut section, spec);
    section
}

/// The pattern one rack entry plays in one section — the idiom dispatch
/// (issue #87's per-genre pattern set).
#[allow(clippy::too_many_arguments)]
fn build_pattern(
    entry: &RackEntry,
    plan: &SectionPlan,
    energy: f32,
    swing: f32,
    density: f32,
    brightness: f32,
    spec: GenreSpec,
    groove: &crate::groove::ActiveGroove,
    bass_archetype: Option<&str>,
    rng: &mut Rng,
) -> kontinuum_ir::schema::Pattern {
    match entry.voice {
        Voice::Kick => palette::kick_pattern(plan, energy, rng),
        Voice::Clap | Voice::Snare => palette::backbeat_pattern(energy, rng),
        // An open hat always takes the offbeat-eighth line, whatever the
        // style's closed-hat idiom.
        Voice::Hat if is_open_hat(entry) => palette::open_hat_pattern(energy, rng),
        Voice::Hat => palette::perc_pattern(plan, energy, swing, spec.hats, density, groove, rng),
        Voice::Shaker => {
            palette::perc_pattern(plan, energy, swing, crate::genre::HatIdiom::Euclid, density, groove, rng)
        }
        Voice::Bass => {
            palette::bass_pattern(bass_archetype, energy, swing, spec.downbeat_collision, Some(groove), rng)
        }
        Voice::Acid => palette::acid_pattern(energy, swing, Some(groove), rng),
        Voice::Pad => palette::pad_pattern(energy, spec.restrained_harmony(), rng),
        Voice::Ep => palette::ep_pattern(energy, spec.restrained_harmony(), rng),
        Voice::Pluck => palette::pluck_pattern(energy, swing, density, spec.restrained_harmony(), rng),
        Voice::Stab => {
            // Gated-16th stabs are the driving styles' colour; the pump
            // styles (house family) keep the one-two stab so the sidechain
            // swing stays deep (see the duck_pump acceptance gate).
            let gated = matches!(spec.style, Style::Techno | Style::Minimal | Style::Default);
            palette::stab_pattern(
                energy,
                spec.style == Style::DubTechno,
                spec.restrained_harmony(),
                gated,
                rng,
            )
        }
        Voice::Texture => palette::texture_pattern(energy, brightness, rng),
    }
}

fn is_open_hat(entry: &RackEntry) -> bool {
    matches!(
        &entry.inst,
        kontinuum_ir::schema::InstrumentDef::Hat(h) if h.open
    )
}

fn plan_structure(
    rng: &mut Rng,
    target_bars: u32,
    intensity: f32,
    spec: GenreSpec,
    structure: Option<&crate::structure::StructureParams>,
    soul: Option<&crate::soul::SoulArrangement>,
    arc_pin: Option<crate::grammar::ArcFamily>,
) -> (Vec<SectionPlan>, crate::grammar::GrammarData) {
    let grammar = structure
        .and_then(|s| s.grammar.clone())
        .unwrap_or_else(crate::grammar::GrammarData::base);
    let (_arc_family, arc_spec) = grammar.pick_arc(arc_pin, rng);

    let shift = (intensity - 0.5) * 0.3 + spec.energy_bias;
    let e = move |lo: f32, hi: f32| {
        ((lo + shift).clamp(0.05, 0.95), (hi + shift).clamp(0.05, 0.95))
    };
    let draw_count = |range: (u32, u32), rng: &mut Rng| {
        range.0 + rng.below(u64::from(range.1 - range.0 + 1)) as u32
    };
    let dev_count = draw_count(spec.dev_count, rng);
    let breakdown_count = draw_count(spec.breakdown_count, rng);

    // The middle-block walk: weighted grammar states, style quotas, hard
    // constraints (module docs in `grammar`). Dev energy follows the arc
    // family, with the #23 corpus fit and the #55 soul layer outranking it
    // where present — data over defaults, in that order.
    let intro_bars = grammar.sample_length(Kind::Intro, rng);
    let outro_bars = grammar.sample_length(Kind::Outro, rng);
    let reintro_bars = grammar.sample_length(Kind::Reintro, rng);
    let budget = target_bars
        .max(intro_bars + reintro_bars + outro_bars)
        .saturating_sub(intro_bars + reintro_bars + outro_bars);
    let middles = crate::grammar::walk(
        &grammar,
        rng,
        budget,
        arc_spec.allows_early_breakdown,
        dev_count,
        breakdown_count,
    );

    let mut plans = Vec::with_capacity(middles.len() + 3);
    let intro_curves = grammar.curves_for(Kind::Intro);
    plans.push(SectionPlan {
        id: "intro".into(),
        kind: Kind::Intro,
        bars: intro_bars,
        energy: e(intro_curves.energy.0, intro_curves.energy.1),
        density: e(intro_curves.density.0, intro_curves.density.1),
        brightness: e(intro_curves.brightness.0, intro_curves.brightness.1),
        fill: false,
        sparse_kick: false,
        stripped: false,
    });
    for step in &middles {
        let curves = grammar.curves_for(step.kind);
        plans.push(SectionPlan {
            id: section_id(step.kind, &plans),
            kind: step.kind,
            bars: step.bars,
            energy: e(curves.energy.0, curves.energy.1),
            density: e(curves.density.0, curves.density.1),
            brightness: e(curves.brightness.0, curves.brightness.1),
            fill: false,
            sparse_kick: step.kind == Kind::Breakdown && rng.chance(0.5),
            stripped: false,
        });
    }
    let reintro_curves = grammar.curves_for(Kind::Reintro);
    plans.push(SectionPlan {
        id: "reintro".into(),
        kind: Kind::Reintro,
        bars: reintro_bars,
        energy: e(reintro_curves.energy.0, reintro_curves.energy.1),
        density: e(reintro_curves.density.0, reintro_curves.density.1),
        brightness: e(reintro_curves.brightness.0, reintro_curves.brightness.1),
        fill: false,
        sparse_kick: false,
        stripped: false,
    });
    let outro_curves = grammar.curves_for(Kind::Outro);
    plans.push(SectionPlan {
        id: "outro".into(),
        kind: Kind::Outro,
        bars: outro_bars,
        energy: e(outro_curves.energy.0, outro_curves.energy.1),
        density: e(outro_curves.density.0, outro_curves.density.1),
        brightness: e(outro_curves.brightness.0, outro_curves.brightness.1),
        fill: false,
        sparse_kick: false,
        stripped: false,
    });
    apply_arc_tilt(&mut plans, &grammar, &arc_spec, structure, soul, dev_count);
    let fixed_bars = intro_bars + reintro_bars + outro_bars;
    let middle_end = plans.len() - 2;
    scale_middles(&mut plans[1..middle_end], target_bars, fixed_bars);
    enforce_breakdown_gate(&mut plans, &grammar, arc_spec.allows_early_breakdown);
    bound_energy_deltas(&mut plans, grammar.constraints.max_adjacent_energy_delta);
    // Near-solo marking rides the dev spine, as before.
    let dev_positions: Vec<usize> = plans
        .iter()
        .enumerate()
        .filter(|(_, p)| p.kind == Kind::Dev)
        .map(|(i, _)| i)
        .collect();
    mark_near_solos(&mut plans, &dev_positions, target_bars);
    (plans, grammar)
}

/// Assigns each plan's section id (`<prefix>_<n>`, per-kind counter).
fn section_id(kind: Kind, built: &[SectionPlan]) -> String {
    let prefix = kind.id_prefix().unwrap_or("sec");
    let n = built.iter().filter(|p| p.kind == kind).count();
    format!("{prefix}_{n}")
}

/// Tilts the dev sections' energy windows onto the arc family's shape.
fn apply_arc_tilt(
    plans: &mut [SectionPlan],
    grammar: &crate::grammar::GrammarData,
    arc_spec: &kontinuum_corpus::ArcFamilySpec,
    structure: Option<&crate::structure::StructureParams>,
    soul: Option<&crate::soul::SoulArrangement>,
    dev_count: u32,
) {
    // The walk can overdraw its dev quota (self-loop weights), so the
    // arc resamples against the actual dev count.
    let actual = plans.iter().filter(|p| p.kind == Kind::Dev).count().max(1);
    let dev_count = dev_count.max(actual as u32).max(1) as usize;
    let mut di = 0usize;
    for p in plans.iter_mut().filter(|p| p.kind == Kind::Dev) {
        let i = di;
        di += 1;
        let base = structure
            .and_then(|s| s.dev_energy(i, dev_count))
            .or_else(|| soul_arc_energy(soul, i, dev_count))
            .unwrap_or_else(|| 0.2 + 0.7 * grammar.arc_energy(arc_spec, i, actual));
        p.energy = (base.clamp(0.05, 0.95), (base + 0.12).min(0.95));
    }
}

/// Samples a soul's energy arc at dev position `i` of `dev_count` — the
/// same resample-and-rescale rule `StructureParams::dev_energy` uses.
fn soul_arc_energy(
    soul: Option<&crate::soul::SoulArrangement>,
    i: usize,
    dev_count: usize,
) -> Option<f32> {
    let arc = soul?.energy_arc.as_ref()?;
    if arc.is_empty() || dev_count == 0 {
        return None;
    }
    let t = if dev_count <= 1 { 0.5 } else { i as f32 / (dev_count - 1) as f32 };
    let last = arc.len() - 1;
    let raw = arc[(t * last as f32).round() as usize];
    Some((0.2 + raw * 0.7).clamp(0.05, 0.95))
}

/// Marks dev sections as near-solo passages, one per [`BARS_PER_NEAR_SOLO`]
/// bars of target length, spread evenly across the dev block.
fn mark_near_solos(plans: &mut [SectionPlan], dev_positions: &[usize], target_bars: u32) {
    let count = target_bars.div_ceil(BARS_PER_NEAR_SOLO).max(1) as usize;
    let count = count.min(dev_positions.len());
    for k in 0..count {
        let idx = k * dev_positions.len() / count;
        plans[dev_positions[idx]].stripped = true;
    }
}

/// Post-scale gate: a breakdown may not *start* before the constraint bar
/// unless the arc family allows an early one. Scaling shrinks positions,
/// so violators are moved to the tail of the middle block — directly
/// before the reintro, where they read as the last drop before the full
/// return.
fn enforce_breakdown_gate(
    plans: &mut Vec<SectionPlan>,
    grammar: &crate::grammar::GrammarData,
    arc_allows_early: bool,
) {
    if arc_allows_early {
        return;
    }
    let min_bar = grammar.constraints.min_breakdown_bar;
    let mut late: Vec<SectionPlan> = Vec::new();
    let mut bar = 0u32;
    let mut kept = Vec::with_capacity(plans.len());
    for p in plans.drain(..) {
        if p.kind == Kind::Breakdown && bar < min_bar {
            late.push(p);
            continue;
        }
        bar += p.bars;
        kept.push(p);
    }
    kept.splice(kept.len().saturating_sub(2)..kept.len().saturating_sub(2), late);
    *plans = kept;
}

/// Adjacent-section energy delta bound (#16): |next.start − prev.end| is
/// clamped unless either side is a breakdown or release — the drama
/// points where the arc is allowed to fall off a cliff.
fn bound_energy_deltas(plans: &mut [SectionPlan], max_delta: f32) {
    for i in 1..plans.len() {
        let (prev_kind, prev_end) = (plans[i - 1].kind, plans[i - 1].energy.1);
        let exempt =
            matches!(prev_kind, Kind::Breakdown | Kind::Release) || matches!(plans[i].kind, Kind::Breakdown | Kind::Release);
        if exempt {
            continue;
        }
        let span = plans[i].energy.1 - plans[i].energy.0;
        let start = plans[i]
            .energy
            .0
            .clamp(prev_end - max_delta, prev_end + max_delta);
        plans[i].energy = (start, (start + span).clamp(0.02, 0.98));
    }
}

/// Scales the middle block proportionally so the whole session hits
/// `target_bars` (`fixed_bars` covers intro/reintro/outro; 4-bar aligned,
/// the last middle absorbs rounding drift and may overshoot by 3 bars).
fn scale_middles(middles: &mut [SectionPlan], target_bars: u32, fixed_bars: u32) {
    let cap = (bounds::MAX_SESSION_BARS - u64::from(fixed_bars)) as u32;
    let goal = target_bars.saturating_sub(fixed_bars).clamp(8, cap.max(8));
    let drawn: u64 = middles.iter().map(|p| u64::from(p.bars)).sum();
    let mut alloc: Vec<u32> = middles
        .iter()
        .map(|p| (((u64::from(p.bars) * u64::from(goal)) / drawn.max(1)) as u32 / 4 * 4).max(4))
        .collect();
    let last = alloc.len() - 1;
    let used: u32 = alloc[..last].iter().sum();
    alloc[last] = goal.saturating_sub(used).max(4);
    for (p, bars) in middles.iter_mut().zip(alloc) {
        p.bars = bars;
    }
}

/// A coupled curve over one section: window interpolated linearly with
/// seeded per-bar wobble (shared shape across energy/density/brightness).
fn curve(window: (f32, f32), bars: u32, rng: &mut Rng) -> Vec<f32> {
    let span = bars.saturating_sub(1).max(1);
    (0..bars)
        .map(|b| {
            let t = b as f32 / span as f32;
            let wobble = rng.range_f32(-0.03, 0.03);
            (window.0 + (window.1 - window.0) * t + wobble).clamp(0.02, 0.98)
        })
        .collect()
}

/// Closed/open hat interplay (issue #17): where a closed-hat onset lands
/// inside an open hat's ring window, the open hat's gate is cut to end just
/// before the choke. The engine's choke groups (#19) cover sample slots;
/// the synth hats choke here, in the pattern.
fn apply_hat_choke(section: &mut Section, spec: GenreSpec) {
    let pattern_steps = |id: &str| -> Option<Vec<kontinuum_ir::schema::Step>> {
        match section.pattern_bindings.get(id)? {
            kontinuum_ir::schema::Pattern::Steps(st) => Some(st.steps.clone()),
            _ => None,
        }
    };
    let closed: Vec<u32> = spec
        .rack
        .iter()
        .filter(|e| e.voice == Voice::Hat && !is_open_hat(e))
        .filter_map(|e| pattern_steps(e.id))
        .flatten()
        .map(|s| s.position)
        .collect();
    if closed.is_empty() {
        return;
    }
    for e in spec.rack.iter().filter(|e| e.voice == Voice::Hat && is_open_hat(e)) {
        let Some(kontinuum_ir::schema::Pattern::Steps(st)) = section.pattern_bindings.get_mut(e.id)
        else {
            continue;
        };
        for step in st.steps.iter_mut() {
            let ring_ticks = step.gate.map_or(380, |g| (g * 960.0) as i64);
            let end = i64::from(step.position) + ring_ticks;
            let Some(&choke) =
                closed.iter().filter(|&&c| c > step.position && i64::from(c) < end).min()
            else {
                continue;
            };
            let cut = (i64::from(choke) - i64::from(step.position)) as f32 / 960.0;
            step.gate = Some(cut.max(0.02).min(step.gate.unwrap_or(0.4)));
        }
    }
}

/// Gentle reverb gesture on a chord voice; values stay ≤ 0.5 so slew stays
/// far under the 24 dB/bar ceiling.
fn pad_reverb_lane(target: &str, bars: u32, rng: &mut Rng) -> BTreeMap<String, AutomationLane> {
    let mut points = vec![
        (0, 0.20 + 0.10 * rng.next_f32(), CurveKind::Linear),
        (bars / 2, 0.40 + 0.10 * rng.next_f32(), CurveKind::Smooth),
    ];
    if bars > 2 {
        points.push((bars - 1, 0.25 + 0.10 * rng.next_f32(), CurveKind::Linear));
    }
    let mut lanes = BTreeMap::new();
    lanes.insert(
        target.to_string(),
        AutomationLane { target_param: "send_reverb".into(), points },
    );
    lanes
}

/// Deep-house vocabulary on a section: bass notes ring rather than stab.
///
/// The offbeat open-hat line this used to write by hand is now the open-hat
/// voice's own pattern, and the chord colour comes from the harmony pass,
/// which voices the chord tracks properly instead of stacking a lone tenth
/// on top of one note.
fn apply_deep_house_section(section: &mut Section) {
    use kontinuum_ir::schema::Pattern;
    if let Some(Pattern::Steps(b)) = section.pattern_bindings.get_mut("bass") {
        for st in b.steps.iter_mut() {
            st.gate = Some(0.8);
        }
        // The blanket ring used to overflow the 4-slot bass pool wherever
        // the archetype packed onsets tighter than 0.8 beats apart —
        // acid-slide flams over the bar-32 dev transition were the classic
        // (issue #86) — and the session failed its own validator. Hold the
        // ring inside the pool exactly the way the compiler assigns slots.
        crate::bass::enforce_gate_pool(&mut b.steps);
    }
}
