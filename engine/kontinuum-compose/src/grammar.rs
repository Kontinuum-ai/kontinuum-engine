//! Section grammar (issue #16): the eight arrangement kinds as a weighted
//! state machine walked per session, with every weight, length
//! distribution, curve window, transition recipe, arc family, and hard
//! constraint living in the versioned `arrangement-params` artifact
//! (extension of #23's format, additively — see
//! [`kontinuum_corpus::GrammarBlock`]). A hand-seeded base ships embedded
//! ([`BASE_JSON`]); a corpus artifact that carries a grammar block
//! replaces it with zero code change — the walk consumes the same
//! [`GrammarData`] either way.
//!
//! Hard constraints hold regardless of the data: `intro` always opens and
//! `outro` always closes (terminal — nothing follows it); `reintro` is
//! appended directly before the outro and is only well-formed once a
//! `groove_dev` has run (the motif source it must reference); `breakdown`
//! may not start before the constraint bar unless the arc family allows
//! an early one; adjacent-section energy deltas are bounded except across
//! `breakdown`/`release` (the drama points).

use std::collections::BTreeMap;

use kontinuum_clock::Rng;
use kontinuum_corpus::{
    ArcFamilySpec, CurveWindows, GrammarBlock, GrammarConstraints, LengthWindow, RecipeSpec,
    GRAMMAR_VERSION,
};
use kontinuum_ir::schema::TransitionKind;

use crate::arrangement::Kind;

/// Embedded hand-seeded grammar (techno-structure literature; the #23
/// synthetic fit's shape). Replaced by corpus artifacts as they arrive.
pub const BASE_JSON: &str = include_str!("../fixtures/arrangement-grammar-base.json");

/// Director-selectable energy-arc family (#16): names an entry in the
/// grammar's `arc_families` table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArcFamily {
    SlowBurn,
    TwinPeak,
    PlateauHypnotic,
}

impl ArcFamily {
    pub fn from_label(label: &str) -> Option<ArcFamily> {
        match label {
            "slow_burn" => Some(ArcFamily::SlowBurn),
            "twin_peak" => Some(ArcFamily::TwinPeak),
            "plateau_hypnotic" => Some(ArcFamily::PlateauHypnotic),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ArcFamily::SlowBurn => "slow_burn",
            ArcFamily::TwinPeak => "twin_peak",
            ArcFamily::PlateauHypnotic => "plateau_hypnotic",
        }
    }
}

/// One transition recipe's identity + selection data for an edge.
type RecipeTable = BTreeMap<(Kind, Kind), Vec<(TransitionKind, RecipeSpec)>>;

/// The grammar, resolved and ready to walk. Weights are normalized at
/// load; every query falls back to `None`/empty when a kind is absent so
/// a partially-fitted artifact degrades to the base where it is silent.
#[derive(Clone, Debug, PartialEq)]
pub struct GrammarData {
    transitions: BTreeMap<Kind, Vec<(Kind, f32)>>,
    lengths: BTreeMap<Kind, LengthWindow>,
    curves: BTreeMap<Kind, CurveWindows>,
    recipes: RecipeTable,
    arcs: Vec<(ArcFamily, ArcFamilySpec)>,
    pub constraints: GrammarConstraints,
}

impl GrammarData {
    /// The embedded hand-seeded base. The file is compile-time known;
    /// a parse failure here is a programming error, not a runtime case.
    pub fn base() -> GrammarData {
        Self::from_json(BASE_JSON).expect("embedded base grammar parses")
    }

    /// Parses a full arrangement-params artifact and resolves its grammar
    /// block. `None` when the artifact carries no grammar (pre-extension
    /// file) — the caller falls back to [`GrammarData::base`].
    pub fn from_json(text: &str) -> Option<GrammarData> {
        let artifact = kontinuum_corpus::load_arrangement(text).ok()?;
        Self::from_block(artifact.grammar.as_ref()?)
    }

    /// Resolves a grammar block: version gate, label mapping, weight
    /// normalization, and the essential-row check (a block that cannot
    /// name the spine kinds is rejected so the base applies wholesale).
    pub fn from_block(block: &GrammarBlock) -> Option<GrammarData> {
        if block.grammar_version != GRAMMAR_VERSION {
            return None;
        }
        let mut transitions: BTreeMap<Kind, Vec<(Kind, f32)>> = BTreeMap::new();
        for (from, row) in &block.transitions {
            let Some(from_kind) = Kind::from_label(from) else { continue };
            let mut entries: Vec<(Kind, f32)> = row
                .iter()
                .filter_map(|(to, w)| Kind::from_label(to).map(|k| (k, *w)))
                .filter(|(_, w)| *w > 0.0)
                .collect();
            if entries.is_empty() {
                continue;
            }
            let total: f32 = entries.iter().map(|(_, w)| *w).sum();
            for (_, w) in entries.iter_mut() {
                *w /= total;
            }
            entries.sort_by(|a, b| b.1.total_cmp(&a.1));
            transitions.insert(from_kind, entries);
        }
        // The walk appends reintro + outro by construction, so those rows
        // may legitimately be absent; the spine the walk moves through is
        // not optional.
        for essential in [Kind::Intro, Kind::Dev, Kind::Breakdown] {
            if !transitions.contains_key(&essential) {
                return None;
            }
        }
        let lengths: BTreeMap<Kind, LengthWindow> = block
            .lengths
            .iter()
            .filter_map(|(k, w)| Kind::from_label(k).map(|kind| (kind, w.clone())))
            .collect();
        let curves: BTreeMap<Kind, CurveWindows> = block
            .curves
            .iter()
            .filter_map(|(k, w)| Kind::from_label(k).map(|kind| (kind, w.clone())))
            .collect();
        let mut recipes: RecipeTable = BTreeMap::new();
        for (edge, table) in &block.transition_recipes {
            let Some((from, to)) = parse_edge(edge) else { continue };
            let mut entries: Vec<(TransitionKind, RecipeSpec)> = table
                .iter()
                .filter_map(|(recipe, spec)| parse_recipe(recipe).map(|r| (r, spec.clone())))
                .filter(|(_, spec)| spec.weight > 0.0)
                .collect();
            if entries.is_empty() {
                continue;
            }
            let total: f32 = entries.iter().map(|(_, s)| s.weight).sum();
            for (_, s) in entries.iter_mut() {
                s.weight /= total;
            }
            recipes.insert((from, to), entries);
        }
        let mut arcs: Vec<(ArcFamily, ArcFamilySpec)> = block
            .arc_families
            .iter()
            .filter_map(|(name, spec)| ArcFamily::from_label(name).map(|f| (f, spec.clone())))
            .filter(|(_, spec)| spec.weight > 0.0 && spec.arc.len() >= 2)
            .collect();
        arcs.sort_by(|a, b| b.1.weight.total_cmp(&a.1.weight));
        if arcs.is_empty() {
            return None;
        }
        Some(GrammarData {
            transitions,
            lengths,
            curves,
            recipes,
            arcs,
            constraints: block.constraints.clone(),
        })
    }

    /// The family the Director named, or the weighted seeded draw.
    pub fn pick_arc(&self, requested: Option<ArcFamily>, rng: &mut Rng) -> (ArcFamily, &ArcFamilySpec) {
        if let Some(family) = requested {
            if let Some((_, spec)) = self.arcs.iter().find(|(f, _)| *f == family) {
                return (family, spec);
            }
        }
        let total: f32 = self.arcs.iter().map(|(_, s)| s.weight).sum();
        let mut roll = rng.next_f32() * total;
        for (family, spec) in &self.arcs {
            roll -= spec.weight;
            if roll <= 0.0 {
                return (*family, spec);
            }
        }
        let (family, spec) = self.arcs.last().expect("arcs non-empty");
        (*family, spec)
    }

    /// The family's normalized arc resampled to `count` dev sections:
    /// position `i` reads the arc at its relative offset (peak = 1.0).
    pub fn arc_energy(&self, spec: &ArcFamilySpec, i: usize, count: usize) -> f32 {
        if count == 0 {
            return 0.5;
        }
        let t = if count <= 1 { 0.5 } else { i as f32 / (count - 1) as f32 };
        let last = spec.arc.len() - 1;
        spec.arc[(t * last as f32).round() as usize]
    }

    /// Samples a section length for `kind` from its p10/p50/p90 window,
    /// center-biased, snapped to the 4-bar grid. Missing kind → 16.
    pub fn sample_length(&self, kind: Kind, rng: &mut Rng) -> u32 {
        let Some(w) = self.lengths.get(&kind) else {
            return 16;
        };
        let u = (rng.next_f32() + rng.next_f32()) * 0.5;
        let bars = w.p10 as f32 + (w.p90 - w.p10) as f32 * u;
        (bars / 4.0).round().max(1.0) as u32 * 4
    }

    /// The kind's coupled-curve windows; missing kind → a neutral window.
    pub fn curves_for(&self, kind: Kind) -> CurveWindows {
        self.curves.get(&kind).cloned().unwrap_or(CurveWindows {
            energy: (0.5, 0.5),
            density: (0.5, 0.5),
            brightness: (0.5, 0.5),
        })
    }

    /// Weighted draw of the next state from `from`, restricted to
    /// `allowed` kinds. `None` when the row is absent or nothing survives
    /// the filter — the caller forces a fallback.
    pub fn draw_next(&self, from: Kind, allowed: impl Fn(Kind) -> bool, rng: &mut Rng) -> Option<Kind> {
        let row = self.transitions.get(&from)?;
        let total: f32 = row.iter().filter(|(k, _)| allowed(*k)).map(|(_, w)| w).sum();
        if total <= 0.0 {
            return None;
        }
        let mut roll = rng.next_f32() * total;
        for (kind, weight) in row {
            if !allowed(*kind) {
                continue;
            }
            roll -= weight;
            if roll <= 0.0 {
                return Some(*kind);
            }
        }
        row.iter().filter(|(k, _)| allowed(*k)).map(|(k, _)| *k).next()
    }

    /// The recipe for an edge, gated on the adjacent energy delta and
    /// drawn seeded; bars sampled from the recipe's window. `None` leaves
    /// the edge unmarked (the section handoff carries it).
    pub fn pick_recipe(
        &self,
        from: Kind,
        to: Kind,
        energy_delta: f32,
        rng: &mut Rng,
    ) -> Option<(TransitionKind, u32)> {
        let table = self.recipes.get(&(from, to))?;
        let eligible: Vec<&(TransitionKind, RecipeSpec)> = table
            .iter()
            .filter(|(_, spec)| energy_delta.abs() >= spec.min_delta)
            .collect();
        if eligible.is_empty() {
            return None;
        }
        let total: f32 = eligible.iter().map(|(_, s)| s.weight).sum();
        let mut roll = rng.next_f32() * total;
        for (recipe, spec) in &eligible {
            roll -= spec.weight;
            if roll <= 0.0 {
                let span = spec.bars.1.saturating_sub(spec.bars.0);
                let bars = spec.bars.0 + if span == 0 { 0 } else { rng.below(u64::from(span) + 1) as u32 };
                return Some((*recipe, bars));
            }
        }
        let (recipe, spec) = eligible.last().expect("eligible non-empty");
        Some((*recipe, spec.bars.0))
    }
}

/// "from->to" edge key parser.
fn parse_edge(edge: &str) -> Option<(Kind, Kind)> {
    let (from, to) = edge.split_once("->")?;
    Some((Kind::from_label(from)?, Kind::from_label(to)?))
}

fn parse_recipe(label: &str) -> Option<TransitionKind> {
    match label {
        "filter_sweep" => Some(TransitionKind::FilterSweep),
        "mute_choreo" => Some(TransitionKind::MuteChoreo),
        "fill" => Some(TransitionKind::Fill),
        "silence_drop" => Some(TransitionKind::SilenceDrop),
        "riser" => Some(TransitionKind::Riser),
        "reverb_throw" => Some(TransitionKind::ReverbThrow),
        _ => None,
    }
}

/// One walked middle-block state: the kind plus the sampled bar budget
/// the walk charged it (re-scaled to the target afterwards).
#[derive(Clone, Copy, Debug)]
pub struct GrammarStep {
    pub kind: Kind,
    pub bars: u32,
}

/// Walks the middle block: weighted draws with the hard filters applied,
/// breakdown/reintro/outro handled by construction (see module docs).
/// `dev_count`/`breakdown_count` are the style's drawn quotas (#87) —
/// under-quota walks are repaired by deterministic insertion at eligible
/// positions, so the grammar orders the style's sections rather than
/// replacing its tendencies.
pub fn walk(
    grammar: &GrammarData,
    rng: &mut Rng,
    budget_bars: u32,
    arc_allows_early_breakdown: bool,
    dev_count: u32,
    breakdown_count: u32,
) -> Vec<GrammarStep> {
    let mut steps: Vec<GrammarStep> = vec![Kind::Dev, Kind::Dev]
        .into_iter()
        .map(|kind| GrammarStep { kind, bars: grammar.sample_length(kind, rng) })
        .collect();
    let mut bars: u32 = steps.iter().map(|s| s.bars).sum();
    while bars < budget_bars && steps.len() < 40 {
        let running_bar = bars;
        let has_dev = steps.iter().any(|s| s.kind == Kind::Dev);
        let next = grammar
            .draw_next(
                steps.last().expect("non-empty").kind,
                |kind| match kind {
                    // Terminal states are appended by construction; an
                    // early breakdown waits for the constraint bar.
                    Kind::Outro | Kind::Reintro => false,
                    Kind::Breakdown => {
                        arc_allows_early_breakdown || running_bar >= grammar.constraints.min_breakdown_bar
                    }
                    // A release without its tension/breakdown source row
                    // can still be drawn (weights only list valid preds),
                    // but never as the opener.
                    _ => has_dev || kind != Kind::Release,
                },
                rng,
            )
            .unwrap_or(Kind::Dev);
        let step = GrammarStep { kind: next, bars: grammar.sample_length(next, rng) };
        bars += step.bars;
        steps.push(step);
    }
    // Quota repair: the walk may have under- or over-shot the style's
    // drawn counts. Devs patch from the back between existing middles;
    // breakdowns insert at the latest dev slot; over-shoot trims the
    // earliest surplus sections.
    let devs = steps.iter().filter(|s| s.kind == Kind::Dev).count();
    if (devs as u32) < dev_count {
        let needed = dev_count as usize - devs;
        for _ in 0..needed {
            let Some(slot) = steps.iter().rposition(|s| s.kind == Kind::Dev) else { break };
            steps.insert(
                slot,
                GrammarStep { kind: Kind::Dev, bars: grammar.sample_length(Kind::Dev, rng) },
            );
        }
    }
    let breakdowns = steps.iter().filter(|s| s.kind == Kind::Breakdown).count();
    if breakdowns > breakdown_count as usize {
        let mut excess = breakdowns - breakdown_count as usize;
        steps.retain(|s| {
            if s.kind == Kind::Breakdown && excess > 0 {
                excess -= 1;
                false
            } else {
                true
            }
        });
    } else if breakdowns < breakdown_count as usize {
        let needed = breakdown_count as usize - breakdowns;
        // Walk the eligible slots from the back so patching two breakdowns
        // does not stack them adjacently.
        for k in 0..needed {
            let search_end = steps.len().saturating_sub(k);
            let Some(slot) = steps[..search_end].iter().rposition(|s| s.kind == Kind::Dev) else { break };
            steps.insert(
                slot + 1,
                GrammarStep { kind: Kind::Breakdown, bars: grammar.sample_length(Kind::Breakdown, rng) },
            );
        }
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;
    use kontinuum_clock::stream;

    const LANE: u8 = 0xEE;
    const PURPOSE: u16 = 0xB0;

    fn rng(seed: u64) -> Rng {
        stream(seed, LANE, PURPOSE)
    }

    #[test]
    fn base_parses_and_normalizes() {
        let g = GrammarData::base();
        let row = g.transitions.get(&Kind::Intro).expect("intro row");
        let total: f32 = row.iter().map(|(_, w)| w).sum();
        assert!((total - 1.0).abs() < 1e-5);
        assert!(g.constraints.min_breakdown_bar == 64);
        assert_eq!(g.arcs.len(), 3);
    }

    #[test]
    fn block_without_essential_rows_rejected() {
        let mut block = serde_block();
        block.transitions.clear();
        assert!(GrammarData::from_block(&block).is_none());
    }

    #[test]
    fn recipe_delta_gate_excludes_heavy_recipes() {
        let g = GrammarData::base();
        // breakdown->release silence_drop carries min_delta 0.4.
        let mut r = rng(7);
        for _ in 0..20 {
            if let Some((recipe, bars)) = g.pick_recipe(Kind::Breakdown, Kind::Release, 0.1, &mut r) {
                assert_ne!(recipe, TransitionKind::SilenceDrop, "gate must hold");
                assert!(bars >= 1);
            }
        }
        let mut seen_silence = false;
        for _ in 0..40 {
            if let Some((recipe, bars)) = g.pick_recipe(Kind::Breakdown, Kind::Release, 0.6, &mut r) {
                if recipe == TransitionKind::SilenceDrop {
                    seen_silence = true;
                    assert!(bars <= g.constraints.max_silence_bars);
                }
            }
        }
        assert!(seen_silence, "large delta must open the silence drop");
    }

    #[test]
    fn arc_selection_honors_the_director_pin() {
        let g = GrammarData::base();
        let mut r = rng(3);
        let (family, _) = g.pick_arc(Some(ArcFamily::TwinPeak), &mut r);
        assert_eq!(family, ArcFamily::TwinPeak);
    }

    fn serde_block() -> GrammarBlock {
        serde_json::from_str::<kontinuum_corpus::ArrangementParamsArtifact>(BASE_JSON)
            .expect("base parses")
            .grammar
            .expect("grammar present")
    }
}
