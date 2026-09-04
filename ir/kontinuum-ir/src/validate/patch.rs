//! Patch-graph validation (issue #37): structural soundness comes from the
//! patch compiler (caps, ids, `out`, cycle rule — a cycle is legal only if it
//! passes through a delay node), then signal-type, connectivity,
//! duplicate-edge, and numeric-bounds lints in the `instruments.rs` style.
//!
//! allow: SIZE_OK — one bounds arm per node kind plus tests; mirrors the
//! per-family layout of `validate/instruments.rs` and `validate/bounds.rs`.

use std::collections::{HashMap, VecDeque};

use crate::compile::patch::{compile_patch, PatchCompileError};
use crate::patch::{
    CustomPatch, EdgeKind, PatchNode, RING_CARRIER_SOCKET,
};
use crate::schema::bounds::{
    DELAY_FEEDBACK, DELAY_TIME_MS, ENV_DECAY_MS, FM_INDEX, FM_RATIO, FORMANT_SHIFT, GAIN,
    LFO_RATE_HZ, PAD_ATTACK_MS, PAD_DETUNE_CENTS, PAD_RELEASE_MS, PATCH_CUTOFF_HZ, UNIT, UNISON,
};
use crate::validate::instruments::range_error;
use crate::validate::{err, f32_in_range, ErrorCatalog, ValidationError};

pub(super) fn check(inst: &CustomPatch, base: &str, out: &mut Vec<ValidationError>) {
    let patch_base = format!("{base}/patch");
    if let Err(e) = compile_patch(inst) {
        out.push(compile_error(&e, &patch_base));
        return;
    }
    check_duplicate_edges(inst, &patch_base, out);
    check_signal_types(inst, &patch_base, out);
    check_ring_carriers(inst, &patch_base, out);
    check_connectivity(inst, &patch_base, out);
    check_bounds(inst, &patch_base, out);
}

fn compile_error(e: &PatchCompileError, base: &str) -> ValidationError {
    match e {
        PatchCompileError::NoOut => err(
            ErrorCatalog::E_PATCH_NO_OUT,
            base,
            "patch defines no `out` node",
            "add exactly one node {\"id\": \"out1\", \"type\": \"out\"} and route audio into it",
        ),
        PatchCompileError::MultipleOut(n) => err(
            ErrorCatalog::E_PATCH_MULTIPLE_OUT,
            base,
            format!("patch defines {n} `out` nodes"),
            "keep exactly one `out` node and merge the rest into it",
        ),
        PatchCompileError::TooManyNodes { count, max } => err(
            ErrorCatalog::E_PATCH_TOO_MANY_NODES,
            base,
            format!("patch has {count} nodes; the ceiling is {max} (per-voice CPU guard)"),
            format!("reduce the patch to at most {max} nodes; split across tracks instead"),
        ),
        PatchCompileError::TooManyEdges { count, max } => err(
            ErrorCatalog::E_PATCH_TOO_MANY_EDGES,
            base,
            format!("patch has {count} edges; the ceiling is {max} (per-voice CPU guard)"),
            format!("reduce the patch to at most {max} edges"),
        ),
        PatchCompileError::DuplicateNode(id) => err(
            ErrorCatalog::E_PATCH_DUPLICATE_NODE_ID,
            format!("{base}/nodes"),
            format!("duplicate node id `{id}`"),
            format!("rename to a unique id (e.g. `{id}_2`)"),
        ),
        PatchCompileError::UnknownEdgeNode { edge, id } => err(
            ErrorCatalog::E_PATCH_UNKNOWN_EDGE_NODE,
            format!("{base}/edges/{edge}"),
            format!("edge references unknown node `{id}`"),
            format!("use one of the declared node ids; `{id}` does not exist"),
        ),
        PatchCompileError::Cycle { path } => err(
            ErrorCatalog::E_PATCH_CYCLE,
            format!("{base}/edges"),
            format!(
                "cycle {} bypasses every delay node; feedback is only legal through `delay`",
                path.join(" -> ")
            ),
            "insert a `delay` node on the loop path, or break the cycle",
        ),
    }
}

fn check_duplicate_edges(patch: &CustomPatch, base: &str, out: &mut Vec<ValidationError>) {
    let mut seen: std::collections::BTreeSet<(&str, &str, EdgeKind, Option<&str>)> =
        Default::default();
    for (i, e) in patch.patch.edges.iter().enumerate() {
        let key = (e.from.as_str(), e.to.as_str(), e.kind, e.param.as_deref());
        if !seen.insert(key) {
            out.push(err(
                ErrorCatalog::E_PATCH_DUPLICATE_EDGE,
                format!("{base}/edges/{i}"),
                format!("duplicate edge {} -> {} ({:?})", e.from, e.to, e.kind),
                "drop the duplicate edge; audio inputs already sum, so one edge is enough",
            ));
        }
    }
}

fn check_signal_types(patch: &CustomPatch, base: &str, out: &mut Vec<ValidationError>) {
    let nodes = &patch.patch.nodes;
    for (i, edge) in patch.patch.edges.iter().enumerate() {
        let epath = format!("{base}/edges/{i}");
        let Some(from) = nodes.iter().find(|n| n.id() == edge.from) else { continue };
        let Some(to) = nodes.iter().find(|n| n.id() == edge.to) else { continue };
        match edge.kind {
            EdgeKind::Audio => {
                if !from.produces_audio() || !to.accepts_audio() {
                    out.push(err(
                        ErrorCatalog::E_PATCH_SIGNAL_TYPE,
                        epath,
                        format!(
                            "audio edge {} -> {} is type-illegal: `{}` is a {} ({} out), `{}` is a {} ({} in)",
                            edge.from, edge.to, edge.from, from.kind_name(), audio_desc(from), edge.to, to.kind_name(), audio_desc(to)
                        ),
                        format!(
                            "route audio only between audio nodes (osc, fm_pair, filter, gain, delay, ring, shaper, formant, sampler, out); use a mod edge from {} instead",
                            edge.from
                        ),
                    ));
                    continue;
                }
                if let Some(socket) = edge.param.as_deref() {
                    if !to.audio_sockets().contains(&socket) {
                        out.push(err(
                            ErrorCatalog::E_PATCH_SIGNAL_TYPE,
                            epath,
                            format!(
                                "audio edge {} -> {} names socket `{}`, but `{}` (a {}) has no such input socket",
                                edge.from, edge.to, socket, edge.to, to.kind_name()
                            ),
                            format!(
                                "drop \"param\" to feed the default input{}",
                                to.audio_sockets()
                                    .is_empty()
                                    .then(String::new)
                                    .unwrap_or_else(|| format!(" or use one of: {}", to.audio_sockets().join(", ")))
                            ),
                        ));
                    }
                }
            }
            EdgeKind::Mod => {
                if !from.is_mod_source() {
                    out.push(err(
                        ErrorCatalog::E_PATCH_SIGNAL_TYPE,
                        epath,
                        format!(
                            "mod edge {} -> {} is type-illegal: `{}` is a {}, not a control source (env, lfo)",
                            edge.from, edge.to, edge.from, from.kind_name()
                        ),
                        format!("use an env or lfo node as the mod source instead of `{}`", edge.from),
                    ));
                    continue;
                }
                let param = edge.param.as_deref().unwrap_or("");
                if !to.mod_targets().contains(&param) {
                    out.push(err(
                        ErrorCatalog::E_PATCH_UNKNOWN_MOD_TARGET,
                        epath,
                        format!(
                            "`{}` is a {} and has no mod-able param `{}`",
                            edge.to, to.kind_name(), param
                        ),
                        match to.mod_targets() {
                            [] => format!("`{}` accepts no modulation; drop the mod edge", edge.to),
                            targets => format!(
                                "set \"param\" to one of: {} for `{}`",
                                targets.join(", "), edge.to
                            ),
                        },
                    ));
                }
            }
        }
    }
}

fn audio_desc(node: &PatchNode) -> &'static str {
    if node.produces_audio() {
        "audio out"
    } else if node.is_mod_source() {
        "control out"
    } else {
        "no signal out"
    }
}

/// Every `ring` node needs at least one audio edge into its `carrier` socket:
/// with no carrier the multiplier is identically zero, so the node would be
/// silently dead — rejected with a fix instead.
fn check_ring_carriers(patch: &CustomPatch, base: &str, out: &mut Vec<ValidationError>) {
    for (i, node) in patch.patch.nodes.iter().enumerate() {
        let PatchNode::Ring(_) = node else { continue };
        let has_carrier = patch.patch.edges.iter().any(|e| {
            e.to == *node.id()
                && e.kind == EdgeKind::Audio
                && e.param.as_deref() == Some(RING_CARRIER_SOCKET)
                && patch.patch.nodes.iter().any(|n| n.id() == e.from && n.produces_audio())
        });
        if !has_carrier {
            out.push(err(
                ErrorCatalog::E_PATCH_RING_NO_CARRIER,
                format!("{base}/nodes/{i}"),
                format!(
                    "ring node `{}` has no audio edge into its `carrier` socket; its output would be silent",
                    node.id()
                ),
                format!(
                    "add an audio edge into `{0}` with \"param\": \"carrier\", e.g. {{\"from\": \"<osc id>\", \"to\": \"{0}\", \"type\": \"audio\", \"param\": \"carrier\"}}",
                    node.id()
                ),
            ));
        }
    }
}

/// Forward pass: every node reachable from a source node (no incoming edges).
/// Backward pass: every node reaches `out`. Both passes use ALL edges — env/LFO
/// count as connected when they modulate a node that reaches `out`.
fn check_connectivity(patch: &CustomPatch, base: &str, out: &mut Vec<ValidationError>) {
    let nodes = &patch.patch.nodes;
    let n = nodes.len();
    let index: HashMap<&str, usize> =
        nodes.iter().enumerate().map(|(i, nd)| (nd.id(), i)).collect();
    let mut adj = vec![Vec::new(); n];
    let mut radj = vec![Vec::new(); n];
    let mut indegree = vec![0usize; n];
    for e in &patch.patch.edges {
        let from = index[e.from.as_str()];
        let to = index[e.to.as_str()];
        adj[from].push(to);
        radj[to].push(from);
        indegree[to] += 1;
    }
    let mut seen = vec![false; n];
    let mut queue: VecDeque<usize> = VecDeque::new();
    for i in 0..n {
        if indegree[i] == 0 {
            seen[i] = true;
            queue.push_back(i);
        }
    }
    while let Some(i) = queue.pop_front() {
        for &j in &adj[i] {
            if !seen[j] {
                seen[j] = true;
                queue.push_back(j);
            }
        }
    }
    if let Some(i) = (0..n).find(|&i| !seen[i]) {
        out.push(err(
            ErrorCatalog::E_PATCH_DISCONNECTED,
            format!("{base}/nodes/{i}"),
            format!("node `{}` is not reachable from any patch input", nodes[i].id()),
            "route an edge from a source node (osc, fm_pair, env, lfo) into it, or remove it",
        ));
        return;
    }
    let out_idx = nodes.iter().position(|nd| matches!(nd, PatchNode::Out(_)));
    let mut seen2 = vec![false; n];
    let mut queue2: VecDeque<usize> = VecDeque::new();
    if let Some(o) = out_idx {
        seen2[o] = true;
        queue2.push_back(o);
    }
    while let Some(i) = queue2.pop_front() {
        for &j in &radj[i] {
            if !seen2[j] {
                seen2[j] = true;
                queue2.push_back(j);
            }
        }
    }
    if let Some(i) = (0..n).find(|&i| !seen2[i]) {
        out.push(err(
            ErrorCatalog::E_PATCH_DISCONNECTED,
            format!("{base}/nodes/{i}"),
            format!("node `{}` cannot reach the `out` node", nodes[i].id()),
            "route it (directly or through other nodes) into `out`, or remove it",
        ));
    }
}

fn check_bounds(patch: &CustomPatch, base: &str, out: &mut Vec<ValidationError>) {
    for (i, node) in patch.patch.nodes.iter().enumerate() {
        let npath = format!("{base}/nodes/{i}");
        let generic = |field: &str, v: f32, r: (f32, f32)| {
            range_error(ErrorCatalog::E_PARAM_RANGE, format!("{npath}/{field}"), field, v, r)
        };
        match node {
            PatchNode::Osc(o) => {
                if !(UNISON.0..=UNISON.1).contains(&o.unison) {
                    out.push(err(
                        ErrorCatalog::E_PARAM_RANGE,
                        format!("{npath}/unison"),
                        format!("unison {} outside {}..={}", o.unison, UNISON.0, UNISON.1),
                        format!("set unison between {} and {} voices", UNISON.0, UNISON.1),
                    ));
                }
                if !f32_in_range(o.fine_cents, PAD_DETUNE_CENTS) {
                    out.push(generic("fine_cents", o.fine_cents, PAD_DETUNE_CENTS));
                }
                if !f32_in_range(o.level, UNIT) {
                    out.push(generic("level", o.level, UNIT));
                }
            }
            PatchNode::FmPair(f) => {
                if !f32_in_range(f.ratio, FM_RATIO) {
                    out.push(generic("ratio", f.ratio, FM_RATIO));
                }
                if !f32_in_range(f.index, FM_INDEX) {
                    out.push(generic("index", f.index, FM_INDEX));
                }
                if !f32_in_range(f.feedback, UNIT) {
                    out.push(generic("feedback", f.feedback, UNIT));
                }
                if !f32_in_range(f.level, UNIT) {
                    out.push(generic("level", f.level, UNIT));
                }
            }
            PatchNode::Filter(f) => {
                if !f32_in_range(f.cutoff_hz, PATCH_CUTOFF_HZ) {
                    out.push(generic("cutoff_hz", f.cutoff_hz, PATCH_CUTOFF_HZ));
                }
                if !f32_in_range(f.resonance, UNIT) {
                    out.push(generic("resonance", f.resonance, UNIT));
                }
                if !f32_in_range(f.drive, UNIT) {
                    out.push(generic("drive", f.drive, UNIT));
                }
            }
            PatchNode::Env(e) => {
                if !f32_in_range(e.attack_ms, PAD_ATTACK_MS) {
                    out.push(generic("attack_ms", e.attack_ms, PAD_ATTACK_MS));
                }
                if !f32_in_range(e.decay_ms, ENV_DECAY_MS) {
                    out.push(generic("decay_ms", e.decay_ms, ENV_DECAY_MS));
                }
                if !f32_in_range(e.sustain, UNIT) {
                    out.push(generic("sustain", e.sustain, UNIT));
                }
                if !f32_in_range(e.release_ms, PAD_RELEASE_MS) {
                    out.push(generic("release_ms", e.release_ms, PAD_RELEASE_MS));
                }
            }
            PatchNode::Lfo(l) => {
                if !f32_in_range(l.rate_hz, LFO_RATE_HZ) {
                    out.push(generic("rate_hz", l.rate_hz, LFO_RATE_HZ));
                }
                if !f32_in_range(l.depth, UNIT) {
                    out.push(generic("depth", l.depth, UNIT));
                }
            }
            PatchNode::Gain(g) => {
                if !f32_in_range(g.level, GAIN) {
                    out.push(generic("level", g.level, GAIN));
                }
            }
            PatchNode::Delay(d) => {
                if !f32_in_range(d.time_ms, DELAY_TIME_MS) {
                    out.push(generic("time_ms", d.time_ms, DELAY_TIME_MS));
                }
                if !f32_in_range(d.feedback, DELAY_FEEDBACK) {
                    out.push(err(
                        ErrorCatalog::E_PARAM_RANGE,
                        format!("{npath}/feedback"),
                        format!("feedback {} outside {}..={}", d.feedback, DELAY_FEEDBACK.0, DELAY_FEEDBACK.1),
                        format!("keep feedback <= {} so the loop decays instead of exploding", DELAY_FEEDBACK.1),
                    ));
                }
                if !f32_in_range(d.mix, UNIT) {
                    out.push(generic("mix", d.mix, UNIT));
                }
            }
            PatchNode::Ring(r) => {
                if !f32_in_range(r.level, UNIT) {
                    out.push(generic("level", r.level, UNIT));
                }
            }
            PatchNode::Shaper(s) => {
                if !f32_in_range(s.drive, UNIT) {
                    out.push(generic("drive", s.drive, UNIT));
                }
                if !f32_in_range(s.level, UNIT) {
                    out.push(generic("level", s.level, UNIT));
                }
            }
            PatchNode::Formant(f) => {
                if !f32_in_range(f.shift, FORMANT_SHIFT) {
                    out.push(generic("shift", f.shift, FORMANT_SHIFT));
                }
                if !f32_in_range(f.level, UNIT) {
                    out.push(generic("level", f.level, UNIT));
                }
            }
            // `slot` is a runtime key into the host's sample bank (#19), not a
            // physical quantity — no numeric ceiling, availability is checked
            // when the voice is built.
            PatchNode::Sampler(s) => {
                if !f32_in_range(s.level, UNIT) {
                    out.push(generic("level", s.level, UNIT));
                }
            }
            PatchNode::Out(o) => {
                if !f32_in_range(o.level, UNIT) {
                    out.push(generic("level", o.level, UNIT));
                }
            }
        }
    }
    for (i, edge) in patch.patch.edges.iter().enumerate() {
        let range = match edge.kind {
            EdgeKind::Audio => GAIN,
            EdgeKind::Mod => UNIT,
        };
        if !f32_in_range(edge.amount, range) {
            out.push(range_error(
                ErrorCatalog::E_PARAM_RANGE,
                format!("{base}/edges/{i}/amount"),
                "amount",
                edge.amount,
                range,
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::{CustomTag, PatchEdge, PatchGraph};

    fn edge(from: &str, to: &str, kind: EdgeKind, param: Option<&str>, amount: f32) -> PatchEdge {
        PatchEdge { from: from.into(), to: to.into(), kind, param: param.map(Into::into), amount }
    }

    /// osc -> filter -> out, env + lfo modulating filter cutoff.
    fn good_patch() -> CustomPatch {
        let nodes = vec![
            serde_json::from_str(r#"{"id": "o1", "type": "osc"}"#).expect("osc"),
            serde_json::from_str(r#"{"id": "e1", "type": "env"}"#).expect("env"),
            serde_json::from_str(r#"{"id": "l1", "type": "lfo"}"#).expect("lfo"),
            serde_json::from_str(r#"{"id": "f1", "type": "filter"}"#).expect("filter"),
            serde_json::from_str(r#"{"id": "x", "type": "out"}"#).expect("out"),
        ];
        let edges = vec![
            edge("o1", "f1", EdgeKind::Audio, None, 1.0),
            edge("f1", "x", EdgeKind::Audio, None, 1.0),
            edge("e1", "f1", EdgeKind::Mod, Some("cutoff_hz"), 0.8),
            edge("l1", "f1", EdgeKind::Mod, Some("cutoff_hz"), 0.2),
        ];
        CustomPatch { kind: CustomTag::Custom, patch: PatchGraph { nodes, edges } }
    }

    fn codes(patch: &CustomPatch) -> Vec<&'static str> {
        let mut out = Vec::new();
        check(patch, "/tracks/0/instrument", &mut out);
        out.into_iter().map(|e| e.code).collect()
    }

    #[test]
    fn good_patch_is_clean() {
        let mut out = Vec::new();
        check(&good_patch(), "/tracks/0/instrument", &mut out);
        assert_eq!(out, Vec::<ValidationError>::new(), "{out:?}");
    }

    #[test]
    fn signal_type_violations() {
        // osc cannot be a mod source.
        let mut p = good_patch();
        p.patch.edges.push(edge("o1", "f1", EdgeKind::Mod, Some("cutoff_hz"), 0.5));
        assert!(codes(&p).contains(&ErrorCatalog::E_PATCH_SIGNAL_TYPE));
        // env cannot carry audio.
        let mut p = good_patch();
        p.patch.edges.push(edge("e1", "f1", EdgeKind::Audio, None, 1.0));
        assert!(codes(&p).contains(&ErrorCatalog::E_PATCH_SIGNAL_TYPE));
    }

    #[test]
    fn unknown_mod_target() {
        let mut p = good_patch();
        p.patch.edges.push(edge("l1", "f1", EdgeKind::Mod, Some("wave"), 0.5));
        assert!(codes(&p).contains(&ErrorCatalog::E_PATCH_UNKNOWN_MOD_TARGET));
        // out accepts no modulation at all.
        let mut p = good_patch();
        p.patch.edges.push(edge("e1", "x", EdgeKind::Mod, Some("level"), 0.5));
        assert!(codes(&p).contains(&ErrorCatalog::E_PATCH_UNKNOWN_MOD_TARGET));
    }

    #[test]
    fn disconnected_node_reports() {
        // Orphan env with no edges: a source itself, but never reaches out.
        let mut p = good_patch();
        p.patch
            .nodes
            .push(serde_json::from_str(r#"{"id": "e2", "type": "env"}"#).expect("env"));
        let set: std::collections::BTreeSet<&'static str> = codes(&p).into_iter().collect();
        assert!(set.contains(&ErrorCatalog::E_PATCH_DISCONNECTED));
    }

    #[test]
    fn duplicate_edge_reports() {
        let mut p = good_patch();
        p.patch.edges.push(edge("o1", "f1", EdgeKind::Audio, None, 1.0));
        assert!(codes(&p).contains(&ErrorCatalog::E_PATCH_DUPLICATE_EDGE));
    }

    #[test]
    fn bounds_violations_report_param_range() {
        let mut p = good_patch();
        p.patch.nodes[3] = serde_json::from_str(
            r#"{"id": "f1", "type": "filter", "cutoff_hz": 99999.0, "resonance": 1.5}"#,
        )
        .expect("filter");
        p.patch.edges[0].amount = 5.0;
        let set: std::collections::BTreeSet<&'static str> = codes(&p).into_iter().collect();
        assert!(set.contains(&ErrorCatalog::E_PARAM_RANGE), "{set:?}");
        // Paths point at the offending fields.
        let mut out = Vec::new();
        check(&p, "/tracks/0/instrument", &mut out);
        let paths: Vec<&str> = out.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"/tracks/0/instrument/patch/nodes/3/cutoff_hz"));
        assert!(paths.contains(&"/tracks/0/instrument/patch/nodes/3/resonance"));
        assert!(paths.contains(&"/tracks/0/instrument/patch/edges/0/amount"));
    }

    #[test]
    fn delay_feedback_ceiling_is_stability_guard() {
        let nodes = vec![
            serde_json::from_str(r#"{"id": "o1", "type": "osc"}"#).expect("osc"),
            serde_json::from_str(
                r#"{"id": "d1", "type": "delay", "time_ms": 250.0, "feedback": 0.99, "mix": 0.3}"#,
            )
            .expect("delay"),
            serde_json::from_str(r#"{"id": "x", "type": "out"}"#).expect("out"),
        ];
        let edges = vec![
            edge("o1", "d1", EdgeKind::Audio, None, 1.0),
            edge("d1", "x", EdgeKind::Audio, None, 1.0),
            edge("d1", "d1", EdgeKind::Audio, None, 1.0),
        ];
        let p = CustomPatch { kind: CustomTag::Custom, patch: PatchGraph { nodes, edges } };
        let mut out = Vec::new();
        check(&p, "/tracks/0/instrument", &mut out);
        assert!(
            out.iter()
                .any(|e| e.code == ErrorCatalog::E_PARAM_RANGE && e.path.ends_with("/feedback")),
            "{out:?}"
        );
    }

    #[test]
    fn session_level_codes_surface_through_validate_session() {
        let json = r#"{
            "version": 1, "seed": 1,
            "tempo_lane": [[0, 124.0]],
            "sections": [{"id": "a", "bars": 2, "energy_curve": [0.5],
                "pattern_bindings": {"p": {"generator": "euclidean", "k": 4, "n": 16}}}],
            "tracks": [{"id": "p", "role": "perc", "instrument": {"kind": "custom", "patch": {
                "nodes": [
                    {"id": "o1", "type": "osc"},
                    {"id": "f1", "type": "filter", "cutoff_hz": 99.0},
                    {"id": "x", "type": "out"}
                ],
                "edges": [
                    {"from": "o1", "to": "f1", "type": "audio"},
                    {"from": "f1", "to": "o1", "type": "audio"},
                    {"from": "f1", "to": "x", "type": "audio"}
                ]
            }}}]
        }"#;
        let s: crate::schema::Session = serde_json::from_str(json).expect("session");
        let errors = crate::validate::validate_session(&s).expect_err("cycle must fail");
        let set: std::collections::BTreeSet<&'static str> =
            errors.iter().map(|e| e.code).collect();
        assert!(set.contains(&ErrorCatalog::E_PATCH_CYCLE), "{set:?}");
    }

    // -- New node kinds (issue #37 vocabulary round 2) -------------------------

    fn ring_patch() -> CustomPatch {
        let nodes = vec![
            serde_json::from_str(r#"{"id": "o1", "type": "osc", "wave": "square"}"#).expect("osc"),
            serde_json::from_str(r#"{"id": "c1", "type": "osc", "wave": "sine"}"#).expect("osc"),
            serde_json::from_str(r#"{"id": "rm", "type": "ring"}"#).expect("ring"),
            serde_json::from_str(r#"{"id": "x", "type": "out"}"#).expect("out"),
        ];
        let edges = vec![
            edge("o1", "rm", EdgeKind::Audio, None, 1.0),
            edge("c1", "rm", EdgeKind::Audio, Some(crate::patch::RING_CARRIER_SOCKET), 1.0),
            edge("rm", "x", EdgeKind::Audio, None, 1.0),
        ];
        CustomPatch { kind: crate::patch::CustomTag::Custom, patch: crate::patch::PatchGraph { nodes, edges } }
    }

    #[test]
    fn ring_without_carrier_edge_is_rejected_with_fix() {
        let mut p = ring_patch();
        p.patch.edges.remove(1);
        let mut out = Vec::new();
        check(&p, "/tracks/0/instrument", &mut out);
        assert!(
            out.iter().any(|e| e.code == ErrorCatalog::E_PATCH_RING_NO_CARRIER
                && e.suggested_fix.contains("\"param\": \"carrier\"")),
            "{out:?}"
        );
        let mut clean = Vec::new();
        check(&ring_patch(), "/tracks/0/instrument", &mut clean);
        assert!(clean.is_empty(), "{clean:?}");
    }

    #[test]
    fn unknown_audio_socket_is_a_signal_type_error() {
        let mut p = ring_patch();
        p.patch.edges[0].param = Some("sidechain".to_string());
        let set: std::collections::BTreeSet<&'static str> = codes(&p).into_iter().collect();
        assert!(set.contains(&ErrorCatalog::E_PATCH_SIGNAL_TYPE), "{set:?}");
    }

    #[test]
    fn new_node_bounds_are_checked() {
        let nodes = vec![
            serde_json::from_str(
                r#"{"id": "w", "type": "shaper", "drive": 1.5, "level": 0.9}"#,
            )
            .expect("shaper"),
            serde_json::from_str(
                r#"{"id": "f", "type": "formant", "vowel": "ah", "shift": 4.0}"#,
            )
            .expect("formant"),
            serde_json::from_str(r#"{"id": "s", "type": "sampler", "slot": 1, "level": 2.0}"#)
                .expect("sampler"),
            serde_json::from_str(r#"{"id": "x", "type": "out"}"#).expect("out"),
        ];
        let edges = vec![
            edge("w", "x", EdgeKind::Audio, None, 1.0),
            edge("f", "x", EdgeKind::Audio, None, 1.0),
            edge("s", "x", EdgeKind::Audio, None, 1.0),
        ];
        let p = CustomPatch { kind: crate::patch::CustomTag::Custom, patch: crate::patch::PatchGraph { nodes, edges } };
        let mut out = Vec::new();
        check(&p, "/tracks/0/instrument", &mut out);
        let paths: Vec<&str> = out.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"/tracks/0/instrument/patch/nodes/0/drive"), "{paths:?}");
        assert!(paths.contains(&"/tracks/0/instrument/patch/nodes/1/shift"), "{paths:?}");
        assert!(paths.contains(&"/tracks/0/instrument/patch/nodes/2/level"), "{paths:?}");
    }
}
