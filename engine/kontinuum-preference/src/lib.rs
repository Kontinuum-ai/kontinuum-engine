//! kontinuum-preference — preference learning from implicit signals (#24).
//!
//! The crate learns *priors*, never commands: it observes behavioral signals,
//! attributes them to compact state fingerprints, and biases the session
//! director strictly inside the taste-DNA ranges. A bad learner can bias,
//! never break, a session (issue #24 guardrail).
//!
//! Layout:
//! - [`signal`] — event schema, valence conventions, JSONL store with
//!   rotating retention.
//! - [`sqlite_store`] — the on-device SQLite capture store (samples-store
//!   conventions: `PRAGMA user_version`, in-code migrations).
//! - [`synth`] — seeded synthetic-log generator with planted ground-truth
//!   preferences, reaction noise and optional taste drift.
//! - [`fingerprint`] — the credit-assignment key at three selectable
//!   granularity levels (coarse/mid/fine); the granularity-study outcome is
//!   recorded in `docs/preference-learning.md`.
//! - [`learners`] — the strict ladder: B0 control (DNA priors unchanged) →
//!   B1 exponentially-weighted aggregation (deterministic, DNA-bounded) →
//!   B2 disjoint LinUCB, shipped gated off pending replay evidence.
//! - [`priors`] — the DNA prior/band value types and the bounded-mapping
//!   guardrail math every learner output must pass through.
//! - [`replay`] — deterministic offline harness: replays JSONL logs through
//!   candidate learners and emits a B0-vs-B1 report with skip-rate and
//!   session-length proxies plus an IPS-style estimate hook.
//!
//! On-device constraints honored throughout: no `unsafe`, no `rand`
//! dependency (tiny in-crate xorshift for seeded fixtures and B2
//! exploration), allocation-light structures, and bit-reproducible reports.

pub mod fingerprint;
pub mod learners;
pub mod priors;
pub mod replay;
pub mod signal;
pub mod sqlite_store;
pub mod study;
pub mod synth;

pub use fingerprint::{
    attribute, Attribution, Granularity, MusicalState, SectionKind, StateFingerprint,
};
pub use learners::{
    B0Baseline, B1Aggregator, B1Config, B2Bandit, B2Config, B2Context, Learner, XorShift,
};
pub use priors::{DnaBand, LearnerError, SessionPriors, TastePriors};
pub use replay::{LearnerComparison, ReplayHarness, ReplayLog, ReplayMetrics, StateObservation};
pub use signal::{Signal, SignalContext, SignalKind, SignalStore, StoreError};
pub use sqlite_store::SqliteSignalStore;
pub use study::{granularity_study, ladder_comparison, pick_granularity, CorpusMetrics, GranularityResult, LadderReport};
pub use synth::{DriftSpec, GroundTruth, SynthConfig, SynthLog, SynthWorld};
