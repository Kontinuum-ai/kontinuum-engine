//! `kontinuum-ir` — the only contract between AI and the engine (issue #11).
//!
//! Public API contract. Implementations are in the submodules; this file pins
//! the exact surface the compiler, offline renderer, compose and supervision
//! crates build against.

// The exported JSON schema is one large `json!` literal (issue #37 pushed the
// patch-node `oneOf` past the default 128 macro-expansion depth).
#![recursion_limit = "512"]

use serde::{Deserialize, Serialize};

pub mod compile;
pub mod diff;
pub mod dsl;
pub mod fewshot;
pub mod jsonschema;
pub mod patch;
pub mod schema;
pub mod validate;

pub use compile::{
    compile_patch, compile_session, compile_session_summary, CompiledPatch, CompileError,
    CompileSummary, PatchCompileError,
};
pub use diff::{apply_diff, ApplyError, ApplyReport, IrDiff};
pub use fewshot::{validate_and_estimate, PatchEstimate};
pub use patch::*;
pub use schema::*;
pub use validate::{validate_patch_graph, validate_session, ErrorCatalog, ValidationError};

pub const IR_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackRole {
    Kick,
    Bass,
    Perc,
    Pad,
    Fx,
}
