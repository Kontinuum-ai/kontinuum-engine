//! Shared AST for the LiveCode DSL parser: the statement shapes the grammar
//! produces, plus path helpers for error reporting.

/// One top-level statement.
#[derive(Clone, Debug, PartialEq)]
pub enum Stmt {
    Section { id: String, line: usize, body: Vec<Inner> },
    /// `track.param = value;` — track-level, allowed at top level.
    Param { track: String, param: String, value: f32, line: usize },
}

/// Statements inside a section block.
#[derive(Clone, Debug, PartialEq)]
pub enum Inner {
    Bars { bars: i64, line: usize },
    Energy { value: f32, line: usize },
    /// `track.mask = 0b…;` — slots from the most significant digit.
    Mask { track: String, bits: Vec<bool>, line: usize },
    /// `track.vel = [v0, v1, …];` — consecutive 16th velocities.
    Vel { track: String, values: Vec<f32>, line: usize },
    /// `track: E(k, n, rot)` with optional `@ swing s`.
    Euclid { track: String, k: i64, n: i64, rot: i64, swing: Option<f32>, line: usize },
    /// `track.param = value;` inside a section (same op as top-level).
    Param { track: String, param: String, value: f32, line: usize },
}

/// A parsed `track.<field> = …` assignment, before scope wrapping. `None`
/// results from [`super::parse_assign::ParserExt::assignment`] record a
/// semantic error and skip emission.
pub(super) enum Assign {
    Mask { track: String, bits: Vec<bool>, line: usize },
    Vel { track: String, values: Vec<f32>, line: usize },
    Param { track: String, param: String, value: f32, line: usize },
}

impl Assign {
    pub(super) fn track(&self) -> &str {
        match self {
            Assign::Mask { track, .. }
            | Assign::Vel { track, .. }
            | Assign::Param { track, .. } => track,
        }
    }
}

/// JSON-pointer-ish path for pattern statements; `section` is `Some` inside
/// a block, `None` at top level.
pub(super) fn pattern_path(section: Option<&str>, track: &str) -> String {
    match section {
        Some(id) => format!("/sections/{id}/pattern_bindings/{track}"),
        None => format!("/tracks/{track}/pattern"),
    }
}
