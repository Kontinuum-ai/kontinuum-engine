//! `kontinuum-compose` — the deterministic musical-intelligence layer
//! between the composer (LLM, later) and the engine: pattern generators
//! (#16), the hierarchical arrangement engine (#16), and the rolling
//! lookahead planner (#13/#17).
//!
//! Entry points:
//! - [`arrangement::generate_session`] — seed + knobs → a validation-clean
//!   [`kontinuum_ir::Session`]; with optional #23 corpus artifacts loaded
//!   ([`groove::GrooveBank`], [`structure::StructureParams`]) the groove
//!   vocabulary and section structure follow the fit, with the hand-seeded
//!   path as fallback.
//! - [`bass`] — the named bass archetype vocabulary (#17).
//! - [`fill`] — the boundary-bar fill generator (#17).
//! - [`engine::ArrangementEngine`] — validates a session, caches compiled
//!   blocks, and hot-swaps patterns at bar boundaries via
//!   [`kontinuum_ir::IrDiff`].
//! - [`planner`] — rolling lookahead: [`planner::Planner`] and
//!   [`planner::prime`].
//! - [`dj`] — live DJ performance facade (issue #38): [`dj::DjDeck`] arms
//!   one-shots, loops, and tempo/key moves on the running engine.
//! - [`world`] — sound worlds (issue #30): curated timbre-coherent
//!   parameter sets layered on top of the genre rig, with taste-weighted
//!   selection and section-boundary morphing.

pub mod arrangement;
pub mod bass;
pub mod dj;
pub mod engine;
pub mod fill;
pub mod genre;
pub mod ghost;
pub mod grammar;
pub mod groove;
pub mod harmony;
pub mod motif;
pub mod motion;
pub mod palette;
pub mod pattern;
pub mod planner;
pub mod reward;
pub(crate) mod presence;
pub mod structure;
pub mod trackmap;
pub mod transitions;
pub mod variation;

pub use arrangement::{generate_session, GenParams};
pub use dj::{
    landing_bar, quantized_bar, ArmedAction, DjDeck, LandedOneShot, LiveMoveKind, LoopApplied,
    LoopLength, MoveLanded, OneShot, Quantize,
};
pub use engine::ArrangementEngine;
pub use planner::{new_planner, prime, Planner};
pub use soul::{BlendedSoul, CreativeSoul, SoulId};
pub use world::{SoundWorld, SoundWorldId};
pub mod soul;
pub mod taste;
pub mod reference;
pub mod world;
