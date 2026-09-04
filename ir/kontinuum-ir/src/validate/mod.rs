//! Validation pipeline (issue #11): L1 structural, L2 bounds/density lint,
//! L3 dry-run (compile + polyphony/silence/slew/CPU). All failures are
//! machine-readable [`ValidationError`]s with a `suggested_fix` so an LLM can
//! self-correct in one round trip.

pub(crate) mod bounds;
pub(crate) mod dryrun;
pub mod instruments;
pub(crate) mod patch;
pub(crate) mod structural;

use serde::{Deserialize, Serialize};

use crate::schema::Session;

/// Cap on reported errors per run; keeps the feedback prompt-sized.
pub const MAX_REPORTED_ERRORS: usize = 64;

/// Distinct, stable error codes (issue #11 catalog, >= 25 entries).
pub struct ErrorCatalog;

impl ErrorCatalog {
    // L1 — structural / parse-adjacent
    pub const E_BAD_VERSION: &'static str = "E_BAD_VERSION";
    pub const E_EMPTY_SECTIONS: &'static str = "E_EMPTY_SECTIONS";
    pub const E_NO_TRACKS: &'static str = "E_NO_TRACKS";
    pub const E_TOO_MANY_TRACKS: &'static str = "E_TOO_MANY_TRACKS";
    pub const E_DUPLICATE_SECTION_ID: &'static str = "E_DUPLICATE_SECTION_ID";
    pub const E_DUPLICATE_TRACK_ID: &'static str = "E_DUPLICATE_TRACK_ID";
    pub const E_ZERO_BARS: &'static str = "E_ZERO_BARS";
    pub const E_SESSION_TOO_LONG: &'static str = "E_SESSION_TOO_LONG";
    pub const E_TEMPO_EMPTY: &'static str = "E_TEMPO_EMPTY";
    pub const E_TEMPO_INVALID: &'static str = "E_TEMPO_INVALID";
    pub const E_TEMPO_BAR_ORDER: &'static str = "E_TEMPO_BAR_ORDER";
    pub const E_UNKNOWN_TRACK_BINDING: &'static str = "E_UNKNOWN_TRACK_BINDING";
    pub const E_SAMPLE_QUERY_EMPTY: &'static str = "E_SAMPLE_QUERY_EMPTY";
    pub const E_SAMPLE_REF_MISSING: &'static str = "E_SAMPLE_REF_MISSING";
    pub const E_UNKNOWN_PARAM_TARGET: &'static str = "E_UNKNOWN_PARAM_TARGET";
    pub const E_AUTOMATION_EMPTY: &'static str = "E_AUTOMATION_EMPTY";
    pub const E_AUTO_BAR_ORDER: &'static str = "E_AUTO_BAR_ORDER";
    pub const E_AUTO_BAR_OVERFLOW: &'static str = "E_AUTO_BAR_OVERFLOW";
    // Creative Soul stack (issue #55)
    pub const E_SOUL_EMPTY_ID: &'static str = "E_SOUL_EMPTY_ID";
    pub const E_SOUL_WEIGHT_RANGE: &'static str = "E_SOUL_WEIGHT_RANGE";
    pub const E_SOUL_DUPLICATE: &'static str = "E_SOUL_DUPLICATE";
    pub const E_SOUL_ERA_EMPTY: &'static str = "E_SOUL_ERA_EMPTY";
    // L2 — bounds / density lint
    pub const E_EMPTY_ENERGY_CURVE: &'static str = "E_EMPTY_ENERGY_CURVE";
    pub const E_ENERGY_OUT_OF_RANGE: &'static str = "E_ENERGY_OUT_OF_RANGE";
    pub const E_VELOCITY_RANGE: &'static str = "E_VELOCITY_RANGE";
    pub const E_PROBABILITY_RANGE: &'static str = "E_PROBABILITY_RANGE";
    pub const E_MICROTIMING_RANGE: &'static str = "E_MICROTIMING_RANGE";
    pub const E_RATCHET_RANGE: &'static str = "E_RATCHET_RANGE";
    pub const E_TICKS_OVERFLOW: &'static str = "E_TICKS_OVERFLOW";
    pub const E_GATE_RANGE: &'static str = "E_GATE_RANGE";
    pub const E_PITCH_RANGE: &'static str = "E_PITCH_RANGE";
    pub const E_REPEATS_ZERO: &'static str = "E_REPEATS_ZERO";
    pub const E_REPEATS_RANGE: &'static str = "E_REPEATS_RANGE";
    pub const E_EUCLID_RANGE: &'static str = "E_EUCLID_RANGE";
    pub const E_DENSITY_RANGE: &'static str = "E_DENSITY_RANGE";
    pub const E_GAIN_RANGE: &'static str = "E_GAIN_RANGE";
    pub const E_PAN_RANGE: &'static str = "E_PAN_RANGE";
    pub const E_DUCK_DEPTH_RANGE: &'static str = "E_DUCK_DEPTH_RANGE";
    pub const E_DUCK_RELEASE_RANGE: &'static str = "E_DUCK_RELEASE_RANGE";
    pub const E_SEND_RANGE: &'static str = "E_SEND_RANGE";
    pub const E_INSERT_OVERFLOW: &'static str = "E_INSERT_OVERFLOW";
    pub const E_INSERT_MIX_RANGE: &'static str = "E_INSERT_MIX_RANGE";
    pub const E_KICK_TUNE_RANGE: &'static str = "E_KICK_TUNE_RANGE";
    pub const E_KICK_DECAY_RANGE: &'static str = "E_KICK_DECAY_RANGE";
    pub const E_HAT_DECAY_RANGE: &'static str = "E_HAT_DECAY_RANGE";
    pub const E_BASS_CUTOFF_RANGE: &'static str = "E_BASS_CUTOFF_RANGE";
    pub const E_PAD_ATTACK_RANGE: &'static str = "E_PAD_ATTACK_RANGE";
    pub const E_PARAM_RANGE: &'static str = "E_PARAM_RANGE";
    pub const E_DENSITY_TOO_HIGH: &'static str = "E_DENSITY_TOO_HIGH";
    // Pattern-engine state (issue #17)
    pub const E_SWING_RANGE: &'static str = "E_SWING_RANGE";
    pub const E_PATTERN_BIAS_RANGE: &'static str = "E_PATTERN_BIAS_RANGE";
    pub const E_PATTERN_JITTER_RANGE: &'static str = "E_PATTERN_JITTER_RANGE";
    // Sample slots (issue #19 v1)
    pub const E_SAMPLE_TRANSPOSE_RANGE: &'static str = "E_SAMPLE_TRANSPOSE_RANGE";
    pub const E_SAMPLE_FINE_RANGE: &'static str = "E_SAMPLE_FINE_RANGE";
    pub const E_SAMPLE_STRETCH_RANGE: &'static str = "E_SAMPLE_STRETCH_RANGE";
    pub const E_SAMPLE_CHOKE_RANGE: &'static str = "E_SAMPLE_CHOKE_RANGE";
    pub const E_SAMPLE_GRAIN_RANGE: &'static str = "E_SAMPLE_GRAIN_RANGE";
    // L3 — dry-run
    pub const E_POLYPHONY_EXCEEDED: &'static str = "E_POLYPHONY_EXCEEDED";
    pub const E_UNPLANNED_SILENCE: &'static str = "E_UNPLANNED_SILENCE";
    pub const E_SLEW_TOO_FAST: &'static str = "E_SLEW_TOO_FAST";
    pub const E_CPU_BUDGET_EXCEEDED: &'static str = "E_CPU_BUDGET_EXCEEDED";
    pub const E_COMPILE_FAILED: &'static str = "E_COMPILE_FAILED";
    // Patch graph (issue #37) — structure first, then routing/bounds
    pub const E_PATCH_NO_OUT: &'static str = "E_PATCH_NO_OUT";
    pub const E_PATCH_MULTIPLE_OUT: &'static str = "E_PATCH_MULTIPLE_OUT";
    pub const E_PATCH_TOO_MANY_NODES: &'static str = "E_PATCH_TOO_MANY_NODES";
    pub const E_PATCH_TOO_MANY_EDGES: &'static str = "E_PATCH_TOO_MANY_EDGES";
    pub const E_PATCH_DUPLICATE_NODE_ID: &'static str = "E_PATCH_DUPLICATE_NODE_ID";
    pub const E_PATCH_UNKNOWN_EDGE_NODE: &'static str = "E_PATCH_UNKNOWN_EDGE_NODE";
    pub const E_PATCH_DUPLICATE_EDGE: &'static str = "E_PATCH_DUPLICATE_EDGE";
    pub const E_PATCH_CYCLE: &'static str = "E_PATCH_CYCLE";
    pub const E_PATCH_DISCONNECTED: &'static str = "E_PATCH_DISCONNECTED";
    pub const E_PATCH_SIGNAL_TYPE: &'static str = "E_PATCH_SIGNAL_TYPE";
    pub const E_PATCH_UNKNOWN_MOD_TARGET: &'static str = "E_PATCH_UNKNOWN_MOD_TARGET";
    pub const E_PATCH_RING_NO_CARRIER: &'static str = "E_PATCH_RING_NO_CARRIER";
    pub const E_PATCH_PARSE: &'static str = "E_PATCH_PARSE";

    /// All codes, for tooling and the uniqueness/distinctness test.
    pub const ALL: &'static [&'static str] = &[
        Self::E_BAD_VERSION,
        Self::E_EMPTY_SECTIONS,
        Self::E_NO_TRACKS,
        Self::E_TOO_MANY_TRACKS,
        Self::E_DUPLICATE_SECTION_ID,
        Self::E_DUPLICATE_TRACK_ID,
        Self::E_ZERO_BARS,
        Self::E_SESSION_TOO_LONG,
        Self::E_TEMPO_EMPTY,
        Self::E_TEMPO_INVALID,
        Self::E_TEMPO_BAR_ORDER,
        Self::E_UNKNOWN_TRACK_BINDING,
        Self::E_SAMPLE_QUERY_EMPTY,
        Self::E_SAMPLE_REF_MISSING,
        Self::E_UNKNOWN_PARAM_TARGET,
        Self::E_AUTOMATION_EMPTY,
        Self::E_AUTO_BAR_ORDER,
        Self::E_AUTO_BAR_OVERFLOW,
        Self::E_SOUL_EMPTY_ID,
        Self::E_SOUL_WEIGHT_RANGE,
        Self::E_SOUL_DUPLICATE,
        Self::E_SOUL_ERA_EMPTY,
        Self::E_EMPTY_ENERGY_CURVE,
        Self::E_ENERGY_OUT_OF_RANGE,
        Self::E_VELOCITY_RANGE,
        Self::E_PROBABILITY_RANGE,
        Self::E_MICROTIMING_RANGE,
        Self::E_RATCHET_RANGE,
        Self::E_TICKS_OVERFLOW,
        Self::E_GATE_RANGE,
        Self::E_PITCH_RANGE,
        Self::E_REPEATS_ZERO,
        Self::E_REPEATS_RANGE,
        Self::E_EUCLID_RANGE,
        Self::E_DENSITY_RANGE,
        Self::E_GAIN_RANGE,
        Self::E_PAN_RANGE,
        Self::E_SEND_RANGE,
        Self::E_INSERT_OVERFLOW,
        Self::E_INSERT_MIX_RANGE,
        Self::E_KICK_TUNE_RANGE,
        Self::E_KICK_DECAY_RANGE,
        Self::E_HAT_DECAY_RANGE,
        Self::E_BASS_CUTOFF_RANGE,
        Self::E_PAD_ATTACK_RANGE,
        Self::E_PARAM_RANGE,
        Self::E_DENSITY_TOO_HIGH,
        Self::E_SWING_RANGE,
        Self::E_PATTERN_BIAS_RANGE,
        Self::E_PATTERN_JITTER_RANGE,
        Self::E_POLYPHONY_EXCEEDED,
        Self::E_UNPLANNED_SILENCE,
        Self::E_SLEW_TOO_FAST,
        Self::E_CPU_BUDGET_EXCEEDED,
        Self::E_COMPILE_FAILED,
        Self::E_PATCH_NO_OUT,
        Self::E_PATCH_MULTIPLE_OUT,
        Self::E_PATCH_TOO_MANY_NODES,
        Self::E_PATCH_TOO_MANY_EDGES,
        Self::E_PATCH_DUPLICATE_NODE_ID,
        Self::E_PATCH_UNKNOWN_EDGE_NODE,
        Self::E_PATCH_DUPLICATE_EDGE,
        Self::E_PATCH_CYCLE,
        Self::E_PATCH_DISCONNECTED,
        Self::E_PATCH_SIGNAL_TYPE,
        Self::E_PATCH_UNKNOWN_MOD_TARGET,
        Self::E_PATCH_RING_NO_CARRIER,
        Self::E_PATCH_PARSE,
    ];
}

/// One actionable failure: stable `code`, JSON-pointer-ish `path`, human
/// `message`, and a `suggested_fix` an LLM can apply directly.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ValidationError {
    pub code: &'static str,
    pub path: String,
    pub message: String,
    pub suggested_fix: String,
}

pub(crate) fn err(
    code: &'static str,
    path: impl Into<String>,
    message: impl Into<String>,
    suggested_fix: impl Into<String>,
) -> ValidationError {
    ValidationError {
        code,
        path: path.into(),
        message: message.into(),
        suggested_fix: suggested_fix.into(),
    }
}

pub(crate) fn f32_in_range(v: f32, r: (f32, f32)) -> bool {
    v.is_finite() && v >= r.0 && v <= r.1
}

/// Validates a session end to end. L3 runs only when L1/L2 are clean (the
/// dry-run compiler assumes sane structure).
pub fn validate_session(session: &Session) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();
    structural::check(session, &mut errors);
    bounds::check(session, &mut errors);
    if errors.is_empty() {
        dryrun::check(session, &mut errors);
    }
    if errors.is_empty() {
        Ok(())
    } else {
        errors.truncate(MAX_REPORTED_ERRORS);
        Err(errors)
    }
}

/// Validates one standalone patch graph (issue #37): the same structural,
/// signal-type, connectivity, and bounds rules as a session-embedded patch,
/// addressable as `/patch/…`. The composer's validate-and-estimate seam
/// (see [`crate::fewshot`]) is built on this.
pub fn validate_patch_graph(patch: &crate::patch::CustomPatch) -> Vec<ValidationError> {
    let mut errors = Vec::new();
    patch::check(patch, "", &mut errors);
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_at_least_25_distinct_codes() {
        assert!(ErrorCatalog::ALL.len() >= 25);
        let mut seen = std::collections::BTreeSet::new();
        for c in ErrorCatalog::ALL {
            assert!(seen.insert(c), "duplicate catalog code: {c}");
            assert!(c.starts_with("E_"));
        }
    }
}
