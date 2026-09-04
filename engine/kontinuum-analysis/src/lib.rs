//! `kontinuum-analysis` — the self-listening critic v0 (issues #25/#26,
//! #52 workstream 4): DSP metrics ported from `scripts/analysis/ab-profile.py`,
//! reference profiles as data, and a ratchet gate so CI blocks regressions
//! without demanding absolute perfection from a young engine.
//!
//! Two layers live here:
//! * offline render metrics ([`metrics`], [`profile`]) — analyze a whole
//!   rendered buffer, gate against profiles/baselines in CI;
//! * the rolling critic ([`critic`], [`stems`], [`verdict`], issue #25) —
//!   `push_block`-fed real-time-safe metrics on the master bus and the
//!   stem buses, folded into serde-serializable snapshots and rule-based
//!   verdicts. Snapshots feed the composer context (#22), the reward
//!   model (#26), auto-mixing gain staging (#27) and the watchdog's
//!   kill-switch subset (#15). Nothing here ever emits audio — and no
//!   raw audio ever leaves the process; only these compact structs do.

pub mod bandenv;
pub mod corpus;
pub mod critic;
pub mod dsp;
pub mod fit;
pub mod fft;
pub mod filters;
pub mod metrics;
pub mod profile;
pub mod stems;
pub mod synthgen;
pub mod verdict;

pub use bandenv::{
    beat_band_envelope, pump_window_ranges, sub_and_mid_envelopes, BandEnvelope, WindowRanges,
    MID_BAND, PHASE_BINS, SUB_BAND,
};

pub use corpus::{analyze_track, decode_wav, AnalysisError, TrackAnalysis, PIPELINE_VERSION};
pub use metrics::{Metrics, BANDS};
pub use profile::{Baseline, QualityProfile, TargetBound};

pub use critic::{CriticEngine, CriticSnapshot};
pub use stems::{StemBoard, StemBoardSnapshot, StemId, StemSnapshot};
pub use verdict::{CriticFlags, CriticTargets, CriticVerdict};
