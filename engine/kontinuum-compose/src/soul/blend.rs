//! Deterministic soul blending (issue #55 "modularity & blending"):
//! `session.souls = [{id, weight, era?}]` — one dominant artist soul plus a
//! supporting genre/theme soul, with the listener's taste DNA (#21) always
//! blended in upstream of this module (it lives in GenParams/steering).
//!
//! Per-layer blend rules, all deterministic:
//! - **style card**: fragments concatenate weight-ranked under a word budget
//!   (the dominant keeps its head under truncation).
//! - **rack / harmony / groove character**: the dominant (highest-weight)
//!   layer wins outright — tables sample by weight, in the issue's terms.
//! - **mix profile**: concrete targets interpolate by normalized weight
//!   across the souls that name a track.
//! - **groove affinities**: weight-averaged across naming souls.
//! - **arrangement**: numeric medians interpolate; the energy arc is a
//!   shape, so the dominant's arc wins.
//!
//! Ties in weight break by stack order (stable sort), so the same stack
//! always blends to the same view.

use std::collections::BTreeMap;

use kontinuum_ir::schema::SoulRef;

use super::{CreativeSoul, SoulArrangement, SoulGroove, SoulLayers};

/// Word ceiling for the blended style card (the composer's prompt budget;
/// wake requests stay small, so the card must too).
pub const STYLE_CARD_WORD_BUDGET: usize = 220;

/// One stack entry handed to [`blend`]: a loaded pack plus its weight and
/// active era.
pub struct BlendInput<'a> {
    pub soul: &'a CreativeSoul,
    pub weight: f32,
    pub era: Option<&'a str>,
}

/// The blended view of a soul stack: what the generator and the composer
/// request actually consume.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BlendedSoul {
    pub style_card: String,
    pub palette_overrides: BTreeMap<String, crate::world::VoiceOverride>,
    pub mix_profile: BTreeMap<String, super::SoulMixTarget>,
    /// Dominant groove layer (template pin + swing/jitter character).
    pub groove: Option<SoulGroove>,
    pub groove_affinities: BTreeMap<String, f32>,
    /// Dominant harmony vocabulary; `None` keeps the engine's own tables.
    pub progressions: Option<Vec<Vec<crate::harmony::Chord>>>,
    pub arrangement: Option<SoulArrangement>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BlendError {
    EmptyStack,
    Weight { soul: String, weight: f32 },
    UnknownEra { soul: String, era: String },
    MissingDefaultEra { soul: String },
}

impl std::fmt::Display for BlendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BlendError::EmptyStack => write!(f, "cannot blend an empty soul stack"),
            BlendError::Weight { soul, weight } => {
                write!(f, "soul `{soul}` weight {weight} outside 0 < w <= 1")
            }
            BlendError::UnknownEra { soul, era } => {
                write!(f, "soul `{soul}` has no era named `{era}`")
            }
            BlendError::MissingDefaultEra { soul } => {
                write!(f, "soul `{soul}` has no default era")
            }
        }
    }
}

impl std::error::Error for BlendError {}

/// Blends a non-empty stack. Weights need not sum to 1; interpolation
/// normalizes among the souls that name a layer.
pub fn blend(inputs: &[BlendInput<'_>]) -> Result<BlendedSoul, BlendError> {
    if inputs.is_empty() {
        return Err(BlendError::EmptyStack);
    }
    let order = blend_order(inputs)?;
    let resolved: Vec<(f32, SoulLayers)> = order
        .into_iter()
        .map(|i| {
            let input = &inputs[i];
            Ok((input.weight, resolve_merged(input)?))
        })
        .collect::<Result<_, BlendError>>()?;

    Ok(BlendedSoul {
        style_card: blend_style_card(&resolved),
        palette_overrides: blend_palette(&resolved),
        mix_profile: blend_mix(&resolved),
        groove: resolved.iter().find_map(|(_, l)| l.groove.clone()),
        groove_affinities: blend_affinities(&resolved),
        progressions: resolved.iter().find_map(|(_, l)| l.harmony.clone()).map(|h| h.progressions),
        arrangement: blend_arrangement(&resolved),
    })
}

/// Weight-descending input order; stable, so equal weights keep stack order.
fn blend_order(inputs: &[BlendInput<'_>]) -> Result<Vec<usize>, BlendError> {
    for input in inputs {
        if !input.weight.is_finite() || input.weight <= 0.0 || input.weight > 1.0 {
            return Err(BlendError::Weight { soul: input.soul.id.0.clone(), weight: input.weight });
        }
    }
    let mut order: Vec<usize> = (0..inputs.len()).collect();
    order.sort_by(|&a, &b| inputs[b].weight.total_cmp(&inputs[a].weight));
    Ok(order)
}

/// Era resolution: the named era wins per layer, the default era fills gaps.
fn resolve_merged(input: &BlendInput<'_>) -> Result<SoulLayers, BlendError> {
    let default = input
        .soul
        .eras
        .get(super::load::DEFAULT_ERA)
        .ok_or_else(|| BlendError::MissingDefaultEra { soul: input.soul.id.0.clone() })?;
    let Some(name) = input.era else { return Ok(default.clone()) };
    let era = input
        .soul
        .eras
        .get(name)
        .ok_or_else(|| BlendError::UnknownEra { soul: input.soul.id.0.clone(), era: name.to_string() })?;
    Ok(default.merged(era))
}

fn blend_style_card(resolved: &[(f32, SoulLayers)]) -> String {
    let mut out = String::new();
    let mut budget = STYLE_CARD_WORD_BUDGET;
    for (_, layers) in resolved {
        let Some(card) = layers.style_card.as_ref() else { continue };
        let words: Vec<&str> = card.split_whitespace().collect();
        if out.is_empty() {
            out = words.iter().take(budget).copied().collect::<Vec<_>>().join(" ");
            budget -= out.split_whitespace().count();
        } else if words.len() <= budget {
            out.push_str("\n\n");
            out.push_str(card);
            budget -= words.len();
        }
        if budget == 0 {
            break;
        }
    }
    out
}

/// Dominant-wins per track: the first (weight-desc) soul naming a track
/// provides that track's override wholesale.
fn blend_palette(
    resolved: &[(f32, SoulLayers)],
) -> BTreeMap<String, crate::world::VoiceOverride> {
    let mut out = BTreeMap::new();
    for (_, layers) in resolved {
        let Some(rack) = layers.rack.as_ref() else { continue };
        for (track, over) in &rack.palette_overrides {
            out.entry(track.clone()).or_insert_with(|| *over);
        }
    }
    out
}

/// Normalized-weight interpolation per track across naming souls.
fn blend_mix(resolved: &[(f32, SoulLayers)]) -> BTreeMap<String, super::SoulMixTarget> {
    let mut num: BTreeMap<String, [f32; 4]> = BTreeMap::new();
    let mut den: BTreeMap<String, f32> = BTreeMap::new();
    for (w, layers) in resolved {
        let Some(mix) = layers.mix.as_ref() else { continue };
        for (id, t) in &mix.profile {
            let n = num.entry(id.clone()).or_default();
            n[0] += w * t.gain;
            n[1] += w * t.pan;
            n[2] += w * t.send_delay;
            n[3] += w * t.send_reverb;
            *den.entry(id.clone()).or_default() += w;
        }
    }
    num.into_iter()
        .map(|(id, n)| {
            let d = den[&id];
            (
                id,
                super::SoulMixTarget {
                    gain: n[0] / d,
                    pan: n[1] / d,
                    send_delay: n[2] / d,
                    send_reverb: n[3] / d,
                },
            )
        })
        .collect()
}

/// Weight-averaged affinities across souls that name each template.
fn blend_affinities(resolved: &[(f32, SoulLayers)]) -> BTreeMap<String, f32> {
    let mut num: BTreeMap<String, f32> = BTreeMap::new();
    let mut den: BTreeMap<String, f32> = BTreeMap::new();
    for (w, layers) in resolved {
        let Some(g) = layers.groove.as_ref() else { continue };
        for (name, a) in &g.affinities {
            *num.entry(name.clone()).or_default() += w * a;
            *den.entry(name.clone()).or_default() += w;
        }
    }
    num.into_iter()
        .map(|(name, n)| {
            let d = den[&name];
            (name, n / d)
        })
        .collect()
}

/// Medians interpolate; the arc is the dominant's (shapes don't average).
fn blend_arrangement(resolved: &[(f32, SoulLayers)]) -> Option<SoulArrangement> {
    let present: Vec<(f32, &SoulArrangement)> = resolved
        .iter()
        .filter_map(|(w, l)| l.arrangement.as_ref().map(|a| (*w, a)))
        .collect();
    if present.is_empty() {
        return None;
    }
    Some(SoulArrangement {
        dev_bars: weighted_median(&present, |a| a.dev_bars),
        breakdown_bars: weighted_median(&present, |a| a.breakdown_bars),
        energy_arc: present.iter().find_map(|(_, a)| a.energy_arc.clone()),
    })
}

fn weighted_median(
    present: &[(f32, &SoulArrangement)],
    pick: fn(&SoulArrangement) -> Option<u32>,
) -> Option<u32> {
    let mut num = 0.0f32;
    let mut den = 0.0f32;
    for (w, a) in present {
        if let Some(v) = pick(a) {
            num += w * v as f32;
            den += w;
        }
    }
    (den > 0.0).then(|| (num / den / 4.0).round().max(1.0) as u32 * 4)
}

/// The generator's entry point: a stack of (pack, weight, era) triples plus
/// the refs that survive validation, ready for `Session::souls`. Entries
/// with an out-of-range weight or an era the pack does not ship are dropped
/// deterministically — the session validator is the authoritative gate for
/// hand-written stacks, and the recorded refs always describe what was
/// actually blended.
pub fn prepare(stack: &[SoulStackEntry]) -> SoulPrepared {
    let usable: Vec<SoulStackEntry> = stack
        .iter()
        .filter(|e| e.weight.is_finite() && e.weight > 0.0 && e.weight <= 1.0)
        .filter(|e| match e.era.as_deref() {
            None => e.soul.eras.contains_key(super::load::DEFAULT_ERA),
            Some(era) => e.soul.eras.contains_key(era),
        })
        .cloned()
        .collect();
    let inputs: Vec<BlendInput<'_>> = usable
        .iter()
        .map(|e| BlendInput { soul: &e.soul, weight: e.weight, era: e.era.as_deref() })
        .collect();
    let blended = blend(&inputs).ok();
    // Refs are recorded in blend order (weight desc) — the order a reader
    // should interpret the stack in.
    let mut refs: Vec<SoulRef> = usable
        .iter()
        .map(|e| SoulRef { id: e.soul.id.0.clone(), weight: e.weight, era: e.era.clone() })
        .collect();
    refs.sort_by(|a, b| b.weight.total_cmp(&a.weight));
    SoulPrepared { refs, blended }
}

/// A stack entry for [`prepare`]: a loaded pack plus weight and era.
#[derive(Clone, Debug)]
pub struct SoulStackEntry {
    pub soul: CreativeSoul,
    pub weight: f32,
    pub era: Option<String>,
}

/// What [`prepare`] hands the generator.
#[derive(Clone, Debug, Default)]
pub struct SoulPrepared {
    /// Surviving entries in blend order; stored on `Session::souls`.
    pub refs: Vec<SoulRef>,
    /// The blended layer view; `None` when nothing survived.
    pub blended: Option<BlendedSoul>,
}
