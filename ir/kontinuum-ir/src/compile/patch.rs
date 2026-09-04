//! Patch graph → evaluation plan (issue #37).
//!
//! Pure function: same [`CustomPatch`] → identical [`CompiledPatch`]. The
//! engine-side audio evaluator (kontinuum-core follow-up) consumes the plan
//! directly: run `nodes` in `exec_order`, wire `edges` between node outputs
//! and inputs (audio inputs sum), and implement each entry of `delay_lines`
//! as a ring buffer whose write input is the sum of its incoming edges.
//!
//! Feedback rule: the ONLY feedback-capable node is `delay`. An edge is a
//! feedback edge iff its source is a delay and its target reaches that delay
//! elsewhere in the graph — i.e. it closes a loop through the delay. Feedback
//! edges are excluded from the topological order (the delay therefore sits
//! AFTER its forward feeders) and the evaluator applies their signal one
//! block late, reading the delay line's output tap. Any remaining cycle
//! bypasses every delay node and is rejected.
//!
//! allow: SIZE_OK — one match arm per node kind plus deterministic graph
//! passes (reachability classification, Kahn ordering, cycle extraction) and
//! their tests; the node-kind surface is pinned by the issue #37 IR contract.

use std::collections::{BTreeSet, HashMap};

use crate::patch::{CustomPatch, DelayNode, PatchEdge, PatchNode};
use crate::schema::bounds;

/// One delay node's engine spec: a ring buffer of `time_ms` fed by the sum of
/// the node's incoming edges, recirculating at `feedback`, mixed wet/dry by
/// `mix` onto the node's output.
#[derive(Clone, Debug, PartialEq)]
pub struct DelayLineSpec {
    pub node: String,
    pub time_ms: f32,
    pub feedback: f32,
    pub mix: f32,
}

/// Evaluation plan for one custom instrument patch. `nodes` are in execution
/// order (sources first, `out` last); `edges` keep document order; feedback
/// edges (into a delay) are included so the evaluator can wire the loop.
#[derive(Clone, Debug, PartialEq)]
pub struct CompiledPatch {
    pub nodes: Vec<PatchNode>,
    pub edges: Vec<PatchEdge>,
    pub delay_lines: Vec<DelayLineSpec>,
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum PatchCompileError {
    #[error("patch defines no `out` node")]
    NoOut,
    #[error("patch defines {0} `out` nodes; exactly one is allowed")]
    MultipleOut(usize),
    #[error("patch has {count} nodes; the ceiling is {max}")]
    TooManyNodes { count: usize, max: usize },
    #[error("patch has {count} edges; the ceiling is {max}")]
    TooManyEdges { count: usize, max: usize },
    #[error("duplicate node id `{0}`")]
    DuplicateNode(String),
    #[error("edge {edge} references unknown node `{id}`")]
    UnknownEdgeNode { edge: usize, id: String },
    #[error("patch contains a cycle that bypasses every delay node: {}", path.join(" -> "))]
    Cycle { path: Vec<String> },
}

/// Compiles a validated patch into an evaluation plan. Structural guards
/// (caps, ids, out, cycle rule) are enforced here so the output is always
/// directly executable; signal-type and bounds lints live in `validate`.
pub fn compile_patch(patch: &CustomPatch) -> Result<CompiledPatch, PatchCompileError> {
    let nodes = &patch.patch.nodes;
    let edges = &patch.patch.edges;
    if nodes.len() > bounds::MAX_PATCH_NODES {
        return Err(PatchCompileError::TooManyNodes {
            count: nodes.len(),
            max: bounds::MAX_PATCH_NODES,
        });
    }
    if edges.len() > bounds::MAX_PATCH_EDGES {
        return Err(PatchCompileError::TooManyEdges {
            count: edges.len(),
            max: bounds::MAX_PATCH_EDGES,
        });
    }
    let index_of = node_index_map(nodes)?;
    let out_count = nodes.iter().filter(|n| matches!(n, PatchNode::Out(_))).count();
    match out_count {
        0 => return Err(PatchCompileError::NoOut),
        1 => {}
        n => return Err(PatchCompileError::MultipleOut(n)),
    }

    // Edge classification. Feedback edge: source is a delay AND the target
    // can reach that delay elsewhere — the edge closes a loop through the
    // delay. Deterministic: adjacency follows edge document order, the ready
    // set is index-ordered, so the execution order is a pure function of the
    // patch.
    let mut index_of_edges = Vec::with_capacity(edges.len());
    let mut adj_full: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for e in edges {
        let from = *index_of
            .get(e.from.as_str())
            .ok_or_else(|| PatchCompileError::UnknownEdgeNode { edge: index_of_edges.len(), id: e.from.clone() })?;
        let to = *index_of
            .get(e.to.as_str())
            .ok_or_else(|| PatchCompileError::UnknownEdgeNode { edge: index_of_edges.len(), id: e.to.clone() })?;
        index_of_edges.push((from, to));
        adj_full[from].push(to);
    }
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    let mut indegree = vec![0usize; nodes.len()];
    for &(from, to) in &index_of_edges {
        if nodes[from].is_delay() && reaches(&adj_full, to, from) {
            continue;
        }
        adj[from].push(to);
        indegree[to] += 1;
    }

    let Some(order) = topo_order(&adj, &indegree) else {
        return Err(PatchCompileError::Cycle { path: find_cycle(&adj, nodes) });
    };

    let mut ordered: Vec<PatchNode> = Vec::with_capacity(nodes.len());
    let mut delay_lines = Vec::new();
    for &i in &order {
        ordered.push(nodes[i].clone());
        if let PatchNode::Delay(DelayNode { id, time_ms, feedback, mix, .. }) = &nodes[i] {
            delay_lines.push(DelayLineSpec {
                node: id.clone(),
                time_ms: *time_ms,
                feedback: *feedback,
                mix: *mix,
            });
        }
    }
    Ok(CompiledPatch { nodes: ordered, edges: edges.clone(), delay_lines })
}

/// True when `from` reaches `to` through at least one edge.
fn reaches(adj: &[Vec<usize>], from: usize, to: usize) -> bool {
    let mut seen = vec![false; adj.len()];
    let mut queue = std::collections::VecDeque::from([from]);
    seen[from] = true;
    while let Some(i) = queue.pop_front() {
        for &j in &adj[i] {
            if j == to {
                return true;
            }
            if !seen[j] {
                seen[j] = true;
                queue.push_back(j);
            }
        }
    }
    false
}

/// Node id → declaration index; rejects duplicate ids.
fn node_index_map(nodes: &[PatchNode]) -> Result<HashMap<&str, usize>, PatchCompileError> {
    let mut map = HashMap::with_capacity(nodes.len());
    for (i, n) in nodes.iter().enumerate() {
        if map.insert(n.id(), i).is_some() {
            return Err(PatchCompileError::DuplicateNode(n.id().to_string()));
        }
    }
    Ok(map)
}

/// Kahn's algorithm with declaration-index tie-break. `None` = cycle remains.
fn topo_order(adj: &[Vec<usize>], indegree: &[usize]) -> Option<Vec<usize>> {
    let mut indegree = indegree.to_vec();
    let mut ready: BTreeSet<usize> = (0..adj.len()).filter(|&i| indegree[i] == 0).collect();
    let mut order = Vec::with_capacity(adj.len());
    while let Some(&i) = ready.iter().next() {
        ready.remove(&i);
        for &j in &adj[i] {
            indegree[j] -= 1;
            if indegree[j] == 0 {
                ready.insert(j);
            }
        }
        order.push(i);
    }
    (order.len() == adj.len()).then_some(order)
}

/// Locates one concrete cycle for the error message (node ids in cycle order).
fn find_cycle(adj: &[Vec<usize>], nodes: &[PatchNode]) -> Vec<String> {
    #[derive(Clone, Copy, PartialEq)]
    enum Color {
        White,
        Gray,
        Black,
    }
    fn visit(
        i: usize,
        adj: &[Vec<usize>],
        color: &mut [Color],
        stack: &mut Vec<usize>,
    ) -> Option<Vec<usize>> {
        color[i] = Color::Gray;
        stack.push(i);
        for &j in adj[i].iter() {
            match color[j] {
                Color::White => {
                    if let Some(c) = visit(j, adj, color, stack) {
                        return Some(c);
                    }
                }
                Color::Gray => {
                    if let Some(start) = stack.iter().position(|&x| x == j) {
                        return Some(stack[start..].to_vec());
                    }
                }
                Color::Black => {}
            }
        }
        stack.pop();
        color[i] = Color::Black;
        None
    }
    let mut color = vec![Color::White; adj.len()];
    let mut stack = Vec::new();
    for i in 0..adj.len() {
        if color[i] == Color::White {
            if let Some(cycle) = visit(i, adj, &mut color, &mut stack) {
                return cycle.into_iter().map(|ix| nodes[ix].id().to_string()).collect();
            }
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::{
        CustomTag, DelayTag, EdgeKind, EnvNode, EnvTag, FilterNode, FilterTag, GainNode, GainTag,
        LfoNode, LfoTag, OscNode, OscTag, OutNode, OutTag, PatchGraph,
    };

    fn osc(id: &str) -> PatchNode {
        PatchNode::Osc(OscNode {
            id: id.into(),
            kind: OscTag::Osc,
            wave: Default::default(),
            unison: 1,
            fine_cents: 0.0,
            level: 1.0,
        })
    }

    fn filter(id: &str) -> PatchNode {
        PatchNode::Filter(FilterNode {
            id: id.into(),
            kind: FilterTag::Filter,
            mode: Default::default(),
            cutoff_hz: 3000.0,
            resonance: 0.2,
            drive: 0.0,
        })
    }

    fn env(id: &str) -> PatchNode {
        PatchNode::Env(EnvNode {
            id: id.into(),
            kind: EnvTag::Env,
            attack_ms: 1.0,
            decay_ms: 300.0,
            sustain: 0.0,
            release_ms: 300.0,
        })
    }

    fn lfo(id: &str) -> PatchNode {
        PatchNode::Lfo(LfoNode {
            id: id.into(),
            kind: LfoTag::Lfo,
            rate_hz: 1.0,
            depth: 1.0,
            wave: Default::default(),
        })
    }

    fn gain(id: &str) -> PatchNode {
        PatchNode::Gain(GainNode { id: id.into(), kind: GainTag::Gain, level: 1.0 })
    }

    fn delay(id: &str) -> PatchNode {
        PatchNode::Delay(DelayNode {
            id: id.into(),
            kind: DelayTag::Delay,
            time_ms: 250.0,
            feedback: 0.4,
            mix: 0.3,
        })
    }

    fn out(id: &str) -> PatchNode {
        PatchNode::Out(OutNode { id: id.into(), kind: OutTag::Out, level: 1.0 })
    }

    fn edge(from: &str, to: &str, kind: EdgeKind, param: Option<&str>) -> PatchEdge {
        PatchEdge {
            from: from.into(),
            to: to.into(),
            kind,
            param: param.map(Into::into),
            amount: 1.0,
        }
    }

    fn patch(nodes: Vec<PatchNode>, edges: Vec<PatchEdge>) -> CustomPatch {
        CustomPatch { kind: CustomTag::Custom, patch: PatchGraph { nodes, edges } }
    }

    #[test]
    fn cycle_through_plain_nodes_is_rejected() {
        let p = patch(
            vec![osc("o1"), filter("f1"), out("x")],
            vec![
                edge("o1", "f1", EdgeKind::Audio, None),
                edge("f1", "o1", EdgeKind::Audio, None),
                edge("f1", "x", EdgeKind::Audio, None),
            ],
        );
        let err = compile_patch(&p).expect_err("raw cycle must fail");
        match err {
            PatchCompileError::Cycle { path } => {
                assert_eq!(path, vec!["o1".to_string(), "f1".to_string()]);
            }
            other => panic!("expected Cycle, got {other:?}"),
        }
    }

    #[test]
    fn cycle_through_delay_is_accepted() {
        let p = patch(
            vec![osc("o1"), filter("f1"), delay("d1"), out("x")],
            vec![
                edge("o1", "f1", EdgeKind::Audio, None),
                edge("f1", "d1", EdgeKind::Audio, None),
                edge("d1", "x", EdgeKind::Audio, None),
                // Loop closes back through the filter into the delay input.
                edge("d1", "f1", EdgeKind::Audio, None),
            ],
        );
        let plan = compile_patch(&p).expect("delay loop must compile");
        let pos = |id: &str| plan.nodes.iter().position(|n| n.id() == id).expect("node in plan");
        assert!(pos("o1") < pos("f1"));
        assert!(pos("f1") < pos("d1"));
        assert!(pos("d1") < pos("x"));
        assert_eq!(plan.delay_lines.len(), 1);
        assert_eq!(plan.delay_lines[0].node, "d1");
        assert_eq!(plan.delay_lines[0].feedback, 0.4);
        assert_eq!(plan.edges.len(), 4, "feedback edge rides the plan");
    }

    #[test]
    fn delay_self_feedback_compiles() {
        let p = patch(
            vec![osc("o1"), delay("d1"), out("x")],
            vec![
                edge("o1", "d1", EdgeKind::Audio, None),
                edge("d1", "x", EdgeKind::Audio, None),
                edge("d1", "d1", EdgeKind::Audio, None),
            ],
        );
        let plan = compile_patch(&p).expect("self-feedback is the classic delay tap");
        assert_eq!(plan.delay_lines.len(), 1);
        let pos = |id: &str| plan.nodes.iter().position(|n| n.id() == id).expect("node");
        assert!(pos("o1") < pos("d1") && pos("d1") < pos("x"));
    }

    #[test]
    fn topo_order_respects_dependencies_with_deterministic_tiebreak() {
        let p = patch(
            vec![osc("o1"), env("e1"), lfo("l1"), filter("f1"), gain("g1"), out("x")],
            vec![
                edge("o1", "f1", EdgeKind::Audio, None),
                edge("f1", "g1", EdgeKind::Audio, None),
                edge("g1", "x", EdgeKind::Audio, None),
                edge("e1", "g1", EdgeKind::Mod, Some("level")),
                edge("l1", "f1", EdgeKind::Mod, Some("cutoff_hz")),
            ],
        );
        let plan = compile_patch(&p).expect("compile");
        let ids: Vec<&str> = plan.nodes.iter().map(|n| n.id()).collect();
        assert_eq!(
            ids,
            vec!["o1", "e1", "l1", "f1", "g1", "x"],
            "smallest declaration index wins among ready nodes"
        );
    }

    #[test]
    fn compile_is_deterministic() {
        let p = patch(
            vec![osc("o1"), filter("f1"), delay("d1"), out("x")],
            vec![
                edge("o1", "f1", EdgeKind::Audio, None),
                edge("f1", "d1", EdgeKind::Audio, None),
                edge("d1", "x", EdgeKind::Audio, None),
                edge("d1", "d1", EdgeKind::Audio, None),
            ],
        );
        let a = compile_patch(&p).expect("compile");
        let b = compile_patch(&p).expect("compile");
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
    }

    #[test]
    fn structural_guards() {
        let p = patch(vec![osc("o1")], vec![]);
        assert_eq!(compile_patch(&p), Err(PatchCompileError::NoOut));

        let p = patch(
            vec![out("x1"), out("x2")],
            vec![edge("x1", "x2", EdgeKind::Audio, None)],
        );
        assert_eq!(compile_patch(&p), Err(PatchCompileError::MultipleOut(2)));

        let p = patch(
            vec![osc("o1"), osc("o1"), out("x")],
            vec![edge("o1", "x", EdgeKind::Audio, None)],
        );
        assert_eq!(compile_patch(&p), Err(PatchCompileError::DuplicateNode("o1".into())));

        let p = patch(
            vec![osc("o1"), out("x")],
            vec![edge("o1", "ghost", EdgeKind::Audio, None)],
        );
        assert_eq!(
            compile_patch(&p),
            Err(PatchCompileError::UnknownEdgeNode { edge: 0, id: "ghost".into() })
        );

        let nodes: Vec<PatchNode> = (0..=bounds::MAX_PATCH_NODES)
            .map(|i| gain(&format!("g{i}")))
            .chain(vec![out("x")])
            .collect();
        let p = patch(nodes, vec![]);
        assert!(matches!(
            compile_patch(&p),
            Err(PatchCompileError::TooManyNodes { .. })
        ));
    }
}
