//! kontinuum-mastering — reference-matched mastering chain (#28).
//! Real-time-safe chain: tilt EQ → dynamic low control → glue comp →
//! oversampled soft clipper → true-peak limiter, with hard bounds, slew
//! limits and GR/telemetry on every adaptive parameter.
//!
//! Contract highlights:
//! - fixed stage order, stereo-linked, allocation-free `render`;
//! - section-aware see-through via [`MasteringChain::set_section_energy`]
//!   (breakdowns stay dynamic);
//! - true-peak ceiling −1.0 dBTP always enforced; sustained over-limit
//!   reduction latches [`MasteringTelemetry::limiter_gr_alarm`] for the
//!   kill-switch (#15);
//! - targets are the versioned `mastering-targets.toml` ([`targets`]) —
//!   the shipped values are hypotheses until the reference corpus (#23)
//!   is measured;
//! - [`offline`] helpers (BS.1770-style loudness, normalization, TPDF
//!   dither) run only on rendered buffers, never on the RT path.
//!
//! Determinism: no randomness on the RT path; the only RNG (TPDF dither)
//! is seeded via `kontinuum-clock`. No `unsafe`; no panics in library
//! paths.

pub mod chain;
pub mod clipper;
pub mod dither;
pub mod filters;
pub mod glue;
pub mod limiter;
pub mod loudness;
pub mod low_control;
pub mod oversample;
pub mod targets;
pub mod telemetry;
pub mod tilt;

/// Offline (non-real-time) export helpers: BS.1770-style loudness
/// measurement, loudness normalization, TPDF dither to 16-bit.
pub mod offline {
    pub use crate::dither::{dither_tpdf_16, Dithered16};
    pub use crate::loudness::{
        integrated_lufs, measure_loudness, normalize_to_target, true_peak_dbfs,
        LoudnessMeasurement, NormalizedRender,
    };
}

pub use chain::{MasteringChain, OutputProfile};
pub use telemetry::MasteringTelemetry;
pub use targets::{MasteringTargets, TargetsError, TARGETS_SCHEMA_VERSION};
