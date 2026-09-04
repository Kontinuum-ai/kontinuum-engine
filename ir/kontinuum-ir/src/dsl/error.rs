//! DSL error surface (issue #39 step 4): mirrors the IR
//! [`crate::validate::ValidationError`] style — stable machine-readable
//! `code`, `path` (JSON-pointer-ish IR location when known), human
//! `message`, and a `suggested_fix` an LLM can apply in one round trip —
//! plus the `line` the error came from.

use serde::{Deserialize, Serialize};

/// Stable DSL error codes. Distinct from the IR catalog (`E_DSL_*` prefix).
pub struct DslCode;

impl DslCode {
    // Lexer (fatal)
    pub const E_DSL_BAD_CHAR: &'static str = "E_DSL_BAD_CHAR";
    pub const E_DSL_BAD_NUMBER: &'static str = "E_DSL_BAD_NUMBER";
    pub const E_DSL_MASK_EMPTY: &'static str = "E_DSL_MASK_EMPTY";
    // Parser (fatal grammar errors)
    pub const E_DSL_UNEXPECTED_TOKEN: &'static str = "E_DSL_UNEXPECTED_TOKEN";
    pub const E_DSL_UNKNOWN_STATEMENT: &'static str = "E_DSL_UNKNOWN_STATEMENT";
    pub const E_DSL_EXPECT_TERMINATOR: &'static str = "E_DSL_EXPECT_TERMINATOR";
    pub const E_DSL_UNCLOSED_BRACE: &'static str = "E_DSL_UNCLOSED_BRACE";
    pub const E_DSL_NESTED_SECTION: &'static str = "E_DSL_NESTED_SECTION";
    // Semantic (collected per line)
    pub const E_DSL_FIELD_OUTSIDE_SECTION: &'static str = "E_DSL_FIELD_OUTSIDE_SECTION";
    pub const E_DSL_PATTERN_OUTSIDE_SECTION: &'static str = "E_DSL_PATTERN_OUTSIDE_SECTION";
    pub const E_DSL_UNKNOWN_FIELD: &'static str = "E_DSL_UNKNOWN_FIELD";
    pub const E_DSL_UNKNOWN_PARAM: &'static str = "E_DSL_UNKNOWN_PARAM";
    pub const E_DSL_DUP_FIELD: &'static str = "E_DSL_DUP_FIELD";
    pub const E_DSL_BARS_REQUIRED: &'static str = "E_DSL_BARS_REQUIRED";
    pub const E_DSL_BARS_RANGE: &'static str = "E_DSL_BARS_RANGE";
    pub const E_DSL_ENERGY_REQUIRED: &'static str = "E_DSL_ENERGY_REQUIRED";
    pub const E_DSL_ENERGY_RANGE: &'static str = "E_DSL_ENERGY_RANGE";
    pub const E_DSL_MASK_RANGE: &'static str = "E_DSL_MASK_RANGE";
    pub const E_DSL_VEL_RANGE: &'static str = "E_DSL_VEL_RANGE";
    pub const E_DSL_EUCLID_RANGE: &'static str = "E_DSL_EUCLID_RANGE";
    pub const E_DSL_SWING_RANGE: &'static str = "E_DSL_SWING_RANGE";
    pub const E_DSL_PARAM_RANGE: &'static str = "E_DSL_PARAM_RANGE";
    // Renderer (ir → text coverage)
    pub const E_DSL_UNSUPPORTED_IR: &'static str = "E_DSL_UNSUPPORTED_IR";

    /// All codes, for the uniqueness test.
    pub const ALL: &'static [&'static str] = &[
        Self::E_DSL_BAD_CHAR,
        Self::E_DSL_BAD_NUMBER,
        Self::E_DSL_MASK_EMPTY,
        Self::E_DSL_UNEXPECTED_TOKEN,
        Self::E_DSL_UNKNOWN_STATEMENT,
        Self::E_DSL_EXPECT_TERMINATOR,
        Self::E_DSL_UNCLOSED_BRACE,
        Self::E_DSL_NESTED_SECTION,
        Self::E_DSL_FIELD_OUTSIDE_SECTION,
        Self::E_DSL_PATTERN_OUTSIDE_SECTION,
        Self::E_DSL_UNKNOWN_FIELD,
        Self::E_DSL_UNKNOWN_PARAM,
        Self::E_DSL_DUP_FIELD,
        Self::E_DSL_BARS_REQUIRED,
        Self::E_DSL_BARS_RANGE,
        Self::E_DSL_ENERGY_REQUIRED,
        Self::E_DSL_ENERGY_RANGE,
        Self::E_DSL_MASK_RANGE,
        Self::E_DSL_VEL_RANGE,
        Self::E_DSL_EUCLID_RANGE,
        Self::E_DSL_SWING_RANGE,
        Self::E_DSL_PARAM_RANGE,
        Self::E_DSL_UNSUPPORTED_IR,
    ];
}

/// One actionable DSL failure, in the IR [`crate::validate::ValidationError`]
/// shape plus a source `line`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DslError {
    pub code: &'static str,
    /// 1-based source line.
    pub line: usize,
    /// JSON-pointer-ish IR target when known, otherwise `"/source"`.
    pub path: String,
    pub message: String,
    pub suggested_fix: String,
}

pub(crate) fn dsl_err(
    code: &'static str,
    line: usize,
    path: impl Into<String>,
    message: impl Into<String>,
    suggested_fix: impl Into<String>,
) -> DslError {
    DslError {
        code,
        line,
        path: path.into(),
        message: message.into(),
        suggested_fix: suggested_fix.into(),
    }
}

/// `param` vocabulary accepted by `track.param = value;`. Provenance: the
/// same names `IrDiff::SetInstrumentParam` accepts at apply time
/// (`diff.rs`'s `INSTRUMENT_PARAMS`); checked here so the compiler gives
/// early, line-local feedback instead of deferring to the apply gate.
pub const KNOWN_PARAMS: &[&str] = &[
    "tune_hz",
    "decay_ms",
    "click",
    "drive",
    "tone",
    "snap",
    "cutoff_hz",
    "resonance",
    "env_amt",
    "glide_ms",
    "attack_ms",
    "release_ms",
    "detune_cents",
    "depth",
    "damping",
    "bright",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_distinct_and_prefixed() {
        let mut seen = std::collections::BTreeSet::new();
        for c in DslCode::ALL {
            assert!(seen.insert(c), "duplicate DSL code: {c}");
            assert!(c.starts_with("E_DSL_"));
        }
    }
}
