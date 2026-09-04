//! Few-shot patch library for the composer (issue #37): the five canonical
//! patches as annotated, shareable JSON plus a validate-and-estimate entry
//! point. The composer prompt embeds [`prompt_block`]; a designed patch goes
//! back through [`validate_and_estimate`] before it may enter a session —
//! over-budget graphs surface their cost report, they are never clamped.
//!
//! Patch *design* is a frontier-model task (the #36 `deep_planning` role);
//! the on-device tier may pick and parameterize from this library, never
//! design. The canonical files are the same data the engine's golden render
//! tests pin (kontinuum-core parity test), so prompt examples and engine
//! behavior cannot drift apart.

use crate::compile::{node_cost, patch_cost};
use crate::patch::{CustomPatch, PatchNode};
use crate::schema::bounds;
use crate::validate::{err, validate_patch_graph, ErrorCatalog, ValidationError};

/// One canonical patch: its JSON is [`Self::patch_json`], verbatim.
pub struct CanonicalPatch {
    /// Stable id (`hoover`, `fm_rhodes`, …) the prompt references.
    pub id: &'static str,
    /// Human name.
    pub name: &'static str,
    /// What it sounds like — the annotation the LLM matches a brief against.
    pub sounds_like: &'static str,
    patch_json: &'static str,
}

impl CanonicalPatch {
    /// The patch document: `{"kind": "custom", "patch": {…}}`.
    pub fn patch_json(&self) -> &'static str {
        self.patch_json
    }
}

/// The five canonical patches, in prompt order.
pub const CANONICAL_PATCHES: &[CanonicalPatch] = &[
    CanonicalPatch {
        id: "hoover",
        name: "Hoover",
        sounds_like: "classic rave hoover: 7 detuned saws into a slow-opening \
             low-pass — big, wide, aggressive sweep, synth stabs and pads",
        patch_json: include_str!("../fixtures/patches/canonical/hoover.json"),
    },
    CanonicalPatch {
        id: "fm_rhodes",
        name: "FM Rhodes",
        sounds_like: "FM Rhodes electric piano: glassy tine attack over a warm \
             mellow body — jazzy chords, soul keys, lo-fi",
        patch_json: include_str!("../fixtures/patches/canonical/fm_rhodes.json"),
    },
    CanonicalPatch {
        id: "rumble",
        name: "Rumble",
        sounds_like: "sub rumble: band-limited noise swelling under a resonant \
             low-pass — UK-garage/reese-style infra pressure under drops",
        patch_json: include_str!("../fixtures/patches/canonical/rumble.json"),
    },
    CanonicalPatch {
        id: "formant_pad",
        name: "Formant pad",
        sounds_like: "vocal formant pad: detuned saws through parallel vowel \
             band-passes with slow drift — breathy 'ah' choir, ambient wash",
        patch_json: include_str!("../fixtures/patches/canonical/formant_pad.json"),
    },
    CanonicalPatch {
        id: "cowbell_808",
        name: "808 cowbell",
        sounds_like: "808 cowbell: two detuned squares through a band-pass with a \
             short decay — the classic electro/hip-hop metallic ping",
        patch_json: include_str!("../fixtures/patches/canonical/cowbell_808.json"),
    },
];

/// Per-node CPU breakdown of one patch.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeCost {
    pub id: String,
    pub kind: &'static str,
    pub units: f32,
}

/// Validate-and-estimate result: the patch is structurally legal and its CPU
/// cost in estimate units (same units as the rack budget, `CPU_BUDGET_UNITS`
/// per block) with the per-node report.
#[derive(Clone, Debug, PartialEq)]
pub struct PatchEstimate {
    pub cost_units: f32,
    /// Ceiling on concurrent voices a rack may run at the budget: how many
    /// copies of this voice fit `CPU_BUDGET_UNITS`. Fractional — floor it.
    pub voices_at_budget: f32,
    pub nodes: Vec<NodeCost>,
}

/// Validates a patch JSON document and returns its CPU cost report, or the
/// actionable errors (stable codes + `suggested_fix`) for the repair loop.
pub fn validate_and_estimate(patch_json: &str) -> Result<PatchEstimate, Vec<ValidationError>> {
    let patch: CustomPatch = serde_json::from_str(patch_json).map_err(|e| {
        vec![err(
            ErrorCatalog::E_PATCH_PARSE,
            "/patch",
            format!("patch JSON failed to parse: {e}"),
            "fix the JSON against the patch schema (node \"type\" discriminants, no unknown fields)",
        )]
    })?;
    let errors = validate_patch_graph(&patch);
    if !errors.is_empty() {
        return Err(errors);
    }
    let cost_units = patch_cost(&patch);
    let nodes = patch
        .patch
        .nodes
        .iter()
        .map(|n| NodeCost {
            id: n.id().to_string(),
            kind: n.kind_name(),
            units: node_cost(n),
        })
        .collect();
    Ok(PatchEstimate {
        cost_units,
        voices_at_budget: crate::compile::CPU_BUDGET_UNITS as f32 / cost_units.max(1e-6),
        nodes,
    })
}

/// Renders the few-shot block for the composer prompt: each canonical patch
/// with its sound annotation and full JSON. Deterministic (fixed order).
pub fn prompt_block() -> String {
    let mut out = String::from(
        "Canonical custom patches (reuse or adapt; validate with validate_and_estimate):\n",
    );
    for p in CANONICAL_PATCHES {
        out.push_str(&format!("\n## {} — sounds like: {}\n{}\n", p.id, p.sounds_like, p.patch_json()));
    }
    out
}

/// Cost ceiling hint for the prompt: the per-node table, so the LLM can
/// self-budget before calling [`validate_and_estimate`].
pub fn cost_table_doc() -> String {
    let mut rows: Vec<(String, f32)> = Vec::new();
    // Representative nodes per kind at default params — enough for the LLM
    // to rank kinds; exact numbers come back from validate_and_estimate.
    let samples: [(&str, &str); 12] = [
        ("osc saw (x1 voice)", r#"{"id":"o","type":"osc","wave":"saw"}"#),
        ("osc saw (x7 unison)", r#"{"id":"o","type":"osc","wave":"saw","unison":7}"#),
        ("osc noise", r#"{"id":"o","type":"osc","wave":"noise"}"#),
        ("fm_pair", r#"{"id":"f","type":"fm_pair"}"#),
        ("filter", r#"{"id":"f","type":"filter"}"#),
        ("ring", r#"{"id":"r","type":"ring"}"#),
        ("shaper", r#"{"id":"w","type":"shaper"}"#),
        ("formant", r#"{"id":"v","type":"formant"}"#),
        ("sampler", r#"{"id":"s","type":"sampler","slot":1}"#),
        ("env / lfo", r#"{"id":"e","type":"env"}"#),
        ("gain / out", r#"{"id":"g","type":"gain"}"#),
        ("delay", r#"{"id":"d","type":"delay"}"#),
    ];
    for (label, node_json) in samples {
        let node: PatchNode = serde_json::from_str(node_json)
            .unwrap_or_else(|e| panic!("static cost-table sample {label}: {e}"));
        rows.push((label.to_string(), node_cost(&node)));
    }
    let mut out = format!(
        "Patch CPU cost table (estimate units; block budget {}, node ceiling {}):\n",
        crate::compile::CPU_BUDGET_UNITS,
        bounds::MAX_PATCH_NODES,
    );
    for (label, units) in rows {
        out.push_str(&format!("- {label}: {units:.3}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_patches_parse_validate_and_estimate() {
        for p in CANONICAL_PATCHES {
            let est = validate_and_estimate(p.patch_json())
                .unwrap_or_else(|e| panic!("{}: {e:?}", p.id));
            assert!(est.cost_units > 0.0, "{}: cost must be positive", p.id);
            assert!(!est.nodes.is_empty(), "{}: node report empty", p.id);
            assert!(est.nodes.last().map(|n| n.kind == "out").unwrap_or(false));
            // Sum of node costs equals the total (the report is the sum).
            let summed: f32 = est.nodes.iter().map(|n| n.units).sum();
            assert!((summed - est.cost_units).abs() < 1e-4, "{}: report {summed} vs {est:?}", p.id);
        }
    }

    #[test]
    fn hoover_costs_about_one_kick_voice() {
        let est = validate_and_estimate(CANONICAL_PATCHES[0].patch_json()).expect("hoover");
        // 7 saw voices + filter + env-VCAd gain + out.
        assert!((0.6..=0.75).contains(&est.cost_units), "hoover cost {}", est.cost_units);
        assert!(est.voices_at_budget > 100.0, "one hoover must not dominate the block budget");
    }

    #[test]
    fn broken_patches_return_actionable_errors() {
        let cases: [(&str, &str); 3] = [
            ("not json", ErrorCatalog::E_PATCH_PARSE),
            (
                r#"{"kind": "custom", "patch": {"nodes": [{"id": "o", "type": "osc"}], "edges": []}}"#,
                ErrorCatalog::E_PATCH_NO_OUT,
            ),
            (
                r#"{"kind": "custom", "patch": {"nodes": [
                    {"id": "o", "type": "osc"},
                    {"id": "x", "type": "out"}],
                    "edges": [{"from": "o", "to": "x", "type": "audio", "amount": 9.0}]}}"#,
                ErrorCatalog::E_PARAM_RANGE,
            ),
        ];
        for (json, code) in cases {
            let errors = validate_and_estimate(json).expect_err("must fail");
            assert!(errors.iter().any(|e| e.code == code), "{json}: {errors:?}");
            assert!(errors.iter().all(|e| !e.suggested_fix.is_empty()));
        }
    }

    #[test]
    fn annotations_and_prompt_block_are_complete() {
        assert_eq!(CANONICAL_PATCHES.len(), 5);
        for p in CANONICAL_PATCHES {
            assert!(!p.sounds_like.is_empty(), "{}: needs a sounds_like", p.id);
            assert!(p.patch_json().contains("\"kind\": \"custom\""));
        }
        let block = prompt_block();
        for p in CANONICAL_PATCHES {
            assert!(block.contains(p.id), "prompt block missing {}", p.id);
            assert!(block.contains(p.sounds_like));
        }
        // Deterministic: same bytes every call.
        assert_eq!(block, prompt_block());
        let table = cost_table_doc();
        assert!(table.contains("osc saw (x7 unison)"));
        assert!(table.contains("formant"));
    }
}
