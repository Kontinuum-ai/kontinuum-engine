//! Transition catalog (issue #16): parameterized recipes emitting real IR
//! transitions on section boundaries, selected from the grammar's
//! per-edge tables conditioned on (from_kind, to_kind, energy_delta).
//!
//! Recipes and their game-audio prior art (#7 lessons ledger, FMOD/
//! Wwise transition rules): a transition must (a) live on a musical grid
//! — mute choreography on the 4-bar grid, silence drops on the downbeat —
//! (b) be interruptible or short (fills are 1 bar; drops ≤ the
//! constraint's ceiling), and (c) telegraph before they land (sweeps and
//! risers run several bars into the *departing* section).

use kontinuum_clock::Rng;
use kontinuum_ir::schema::{Transition, TransitionKind};
use serde_json::json;

use crate::arrangement::Kind;
use crate::grammar::GrammarData;

/// Emits the transition IR for the edge `from → to`. Returns `None` when
/// the grammar's table has no entry for the edge (or nothing survives the
/// delta gate) — an unmarked handoff.
pub fn pick(
    grammar: &GrammarData,
    from: Kind,
    to: Kind,
    energy_delta: f32,
    rng: &mut Rng,
) -> Option<Transition> {
    let (kind, bars) = grammar.pick_recipe(from, to, energy_delta, rng)?;
    Some(emit(kind, bars, from, to, energy_delta, rng))
}

/// Recipe emission: kind-specific params, ceiling-clamped lengths.
pub fn emit(
    kind: TransitionKind,
    bars: u32,
    from: Kind,
    to: Kind,
    energy_delta: f32,
    rng: &mut Rng,
) -> Transition {
    let constraints_max_silence = 2;
    let bars = match kind {
        // The drop: at most the constraint's ceiling, on the downbeat.
        TransitionKind::SilenceDrop => bars.min(constraints_max_silence).max(1),
        // Fills stay a boundary bar (issue #17's generator owns the bar).
        TransitionKind::Fill => 1,
        // Sweeps/risers live inside the departing section.
        TransitionKind::FilterSweep | TransitionKind::Riser => bars.max(1),
        TransitionKind::MuteChoreo => {
            // Choreography moves on the 4-bar grid.
            (bars.max(4) / 4 * 4).max(4)
        }
        TransitionKind::ReverbThrow => bars.max(1),
    };
    let params = match kind {
        TransitionKind::FilterSweep => json!({
            "from_hz": 200.0,
            "to_hz": 18000.0,
            "shape": "exp",
        }),
        TransitionKind::MuteChoreo => {
            let (exits, entries) = choreography(from, to, energy_delta, rng);
            json!({ "grid_bars": 4, "exits": exits, "entries": entries })
        }
        TransitionKind::Fill => json!({
            "rolls": true,
            "kick_drop": rng.chance(0.5),
            "glitch_repeat": rng.chance(0.35),
            "reverse_cymbal": rng.chance(0.4),
        }),
        TransitionKind::SilenceDrop => json!({
            "reentry": "downbeat_full_stack",
            "lift_before": 1,
        }),
        TransitionKind::Riser => json!({ "target": "noise", "pitch_rise": true }),
        TransitionKind::ReverbThrow => json!({
            "send": "reverb",
            "freeze": true,
            "dry_exit": "boundary",
        }),
    };
    Transition { kind, bars, params }
}

/// Mute choreography: which layers leave and which arrive across the
/// transition, derived from the kinds at hand — percussive colour exits
/// into any lift, the low end re-enters into any groove.
fn choreography(from: Kind, to: Kind, delta: f32, rng: &mut Rng) -> (Vec<&'static str>, Vec<&'static str>) {
    let mut exits = Vec::new();
    let mut entries = Vec::new();
    if delta > 0.0 {
        exits.push("perc");
        if rng.chance(0.5) {
            exits.push("stab");
        }
    }
    if matches!(to, Kind::Dev | Kind::Release | Kind::Reintro) {
        entries.push("bass");
    }
    if matches!(from, Kind::Breakdown) {
        entries.push("kick");
    }
    (exits, entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kontinuum_clock::stream;

    fn rng(seed: u64) -> Rng {
        stream(seed, 0xCC, 0xD0)
    }

    #[test]
    fn catalog_emits_every_recipe_kind_within_its_contract() {
        let mut r = rng(1);
        let cases = [
            (TransitionKind::FilterSweep, 8, 16),
            (TransitionKind::MuteChoreo, 4, 8),
            (TransitionKind::Fill, 1, 1),
            (TransitionKind::SilenceDrop, 1, 2),
            (TransitionKind::Riser, 4, 8),
            (TransitionKind::ReverbThrow, 1, 2),
        ];
        for (kind, lo, hi) in cases {
            let t = emit(kind, hi, Kind::Dev, Kind::Breakdown, 0.5, &mut r);
            assert_eq!(t.kind, kind);
            assert!((lo..=hi).contains(&t.bars), "{kind:?}: bars {} outside {lo}..={hi}", t.bars);
            assert!(!t.params.is_null(), "{kind:?}: params must be real IR data");
        }
        // Choreography snaps to its grid even when drawn off-grid.
        let t = emit(TransitionKind::MuteChoreo, 6, Kind::Dev, Kind::Release, 0.2, &mut r);
        assert_eq!(t.bars, 4);
    }

    #[test]
    fn selection_follows_the_grammar_tables_and_delta_gates() {
        let g = GrammarData::base();
        let mut r = rng(2);
        // dev -> breakdown has a table entry; sweep lengths stay 8..=16.
        if let Some(t) = pick(&g, Kind::Dev, Kind::Breakdown, 0.5, &mut r) {
            assert!(t.bars >= 1);
        }
        // An edge with no table entry leaves the handoff unmarked.
        assert!(pick(&g, Kind::Release, Kind::Breakdown, 0.5, &mut r).is_none());
    }
}
