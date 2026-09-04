//! LiveCode DSL v0 (issue #39 step 4): a small text surface over the IR —
//! **only** an IR projection. Every statement maps 1:1 to an IR field or a
//! [`IrDiff`] op; anything that is not an IR field is out of scope by rule.
//! The orchestrator applies the emitted diffs through its validation gate.
//!
//! # Grammar v0
//!
//! ```text
//! program      := (statement | terminator)*
//! statement    := section_block | param_line | pattern_line
//! terminator   := ';' | ','
//! comment      := ('#' | '//') …to end of line
//!
//! section_block := 'section' IDENT '{' inner* '}'
//! inner         := 'bars' INT
//!                | 'energy' NUMBER
//!                | pattern_line | param_line
//!
//! pattern_line  := IDENT '.' 'mask' '=' '0b' bit{bit|'_'}      (1..=16 bits)
//!                | IDENT '.' 'vel'  '=' '[' (NUMBER ',')* ']'  (1..=16 entries)
//!                | IDENT ':' 'E' '(' INT ',' INT ',' INT ')'   (k, n, rot)
//!                  [ '@' 'swing' NUMBER ]                      (0..=0.5)
//! param_line    := IDENT '.' PARAM '=' NUMBER
//! ```
//!
//! PARAM is one of the IR instrument params (`tune_hz`, `decay_ms`,
//! `click`, `drive`, `tone`, `cutoff_hz`, `resonance`, `glide_ms`,
//! `attack_ms`, `release_ms`, `detune_cents`). `n` is 1..=16 slots (16th
//! resolution, one bar at PPQ 960), `k ≤ n`, `rot` any i32.
//!
//! # Emission (1:1 with the IR)
//!
//! - `section` → [`IrDiff::AddSection`] (`after: None`; `bars` and the
//!   first `energy` are the header fields — both required so emitted
//!   sections validate cleanly); a later `energy` line →
//!   [`IrDiff::SetSectionEnergy`]
//! - `mask` → `ReplacePattern` with `StepsPattern`: bit i lights slot i at
//!   position i·240 ticks, velocity 0.8
//! - `vel` → `ReplacePattern` with `StepsPattern`: consecutive 16th
//!   velocities, 0.0 = rest
//! - `E(k, n, rot)` → `ReplacePattern` with `EuclideanPattern`; with
//!   `@ swing s` it expands to an on-grid `StepsPattern` whose odd slots
//!   carry `microtiming_ticks = round(s·240)` (clamped to the IR ±120)
//! - param line → [`IrDiff::SetInstrumentParam`]
//!
//! Track/section ids resolve at apply time (the orchestrator's gate owns
//! existence checks); the compiler checks everything checkable without a
//! session.
//!
//! # Errors
//!
//! Lexical and grammar errors are **fatal** (the stream cannot be trusted);
//! semantic errors (ranges, unknown fields/params, scoping, required
//! headers) are **collected per line** and reported together. Every error
//! carries a stable `code`, the `line`, an IR `path`, and an actionable
//! `suggested_fix` — see [`DslCode`].
//!
//! [`render`] is the inverse over the covered subset and rejects uncovered
//! IR instead of approximating; see its module docs for the canonical form.

mod ast;
mod emit;
mod emit_pattern;
mod error;
mod grid;
mod lex;
mod parse;
mod parse_assign;
mod parse_block;
mod render;
mod render_pattern;

pub use error::{DslCode, DslError, KNOWN_PARAMS};
pub use ast::{Inner, Stmt};

use crate::diff::IrDiff;

/// Compiles DSL source into diff ops for the orchestrator's validation
/// gate. Parse errors are fatal; semantic errors are collected per line.
/// Fatal errors come last in the vector, after any collected semantics.
pub fn compile(source: &str) -> Result<Vec<IrDiff>, Vec<DslError>> {
    let (stmts, semantic) = parse::parse(source)?;
    emit::emit(&stmts, semantic)
}

/// Renders covered diffs back to canonical DSL text (the editor's two-way
/// path). Uncovered IR is an error, never a lossy approximation.
pub fn render(diffs: &[IrDiff]) -> Result<String, DslError> {
    render::render(diffs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::IrDiff;

    #[test]
    fn sketch_from_the_issue_compiles() {
        let src = "# minimal techno, issue #39 sketch\n\
                   section a { bars 4, energy 0.5 }\n\
                   section b {\n\
                     bars 8,\n\
                     energy 0.7,\n\
                     kick.mask = 0b1000_1000_1000_1000;\n\
                     hat.vel = [1.0, 0.0, 0.5, 0.0, 1.0, 0.0, 0.5, 0.0, 1.0, 0.0, 0.5, 0.0, 1.0, 0.0, 0.5, 0.0];\n\
                     perc: E(5, 16, 2) @ swing 0.14;\n\
                     bass.cutoff_hz = 120.0;\n\
                   }\n";
        let diffs = compile(src).expect("compiles");
        assert_eq!(diffs.len(), 6, "2 AddSection + 3 patterns + 1 param");
        assert!(matches!(diffs[0], IrDiff::AddSection { .. }));
        assert!(matches!(diffs[5], IrDiff::SetInstrumentParam { .. }));
        assert!(diffs.iter().all(|d| matches!(d, IrDiff::AddSection { .. } | IrDiff::ReplacePattern { .. } | IrDiff::SetInstrumentParam { .. })));
    }

    #[test]
    fn semantic_errors_collect_while_parse_errors_stop_the_pass() {
        let src = "bass.woozle = 1.0;\nsection a { bars 0, energy 2.0 }\n";
        let errs = compile(src).expect_err("semantic");
        assert!(errs.iter().any(|e| e.code == DslCode::E_DSL_UNKNOWN_PARAM));
        assert!(errs.iter().any(|e| e.code == DslCode::E_DSL_BARS_RANGE));
        assert!(errs.iter().any(|e| e.code == DslCode::E_DSL_ENERGY_RANGE));
        // Every error is machine- and LLM-actionable.
        for e in &errs {
            assert!(e.code.starts_with("E_DSL_"));
            assert!(!e.suggested_fix.is_empty());
            assert!(!e.path.is_empty());
        }
        let fatal = compile("section a { bars 4").expect_err("fatal");
        assert_eq!(fatal[0].code, DslCode::E_DSL_UNCLOSED_BRACE);
    }
}
