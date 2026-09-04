//! Per-block telemetry snapshot (#28 → #25): every stage's working point
//! and the alarm flags, updated once per processed block. Copy-able so
//! the engine can post it to the supervision crate without locking.

use serde::{Deserialize, Serialize};

/// Snapshot of the mastering chain's state after a render block.
/// GR values are positive dB of reduction (0.0 = untouched).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MasteringTelemetry {
    /// Current applied tilt (dB, positive = brighter).
    pub tilt_db: f32,
    /// Dynamic low-control reduction (dB).
    pub low_control_gr_db: f32,
    /// Glue compressor mean reduction (dB).
    pub glue_gr_db: f32,
    /// Glue compressor threshold (dBFS).
    pub glue_threshold_db: f32,
    /// Glue makeup gain (dB).
    pub glue_makeup_db: f32,
    /// Soft clipper mean reduction (dB).
    pub clipper_gr_db: f32,
    /// Soft clipper drive (dB).
    pub clipper_drive_db: f32,
    /// Limiter peak reduction during the block (dB).
    pub limiter_gr_db: f32,
    /// Latched: limiter reduction exceeded the 3 dB policy cap for longer
    /// than the sustain window — the kill-switch (#15) should act.
    pub limiter_gr_alarm: bool,
    /// Breakdown relaxation in effect (0 = full intensity, 1 = relaxed).
    pub section_relax: f32,
    /// Rendered block counter (deterministic bookkeeping).
    pub blocks: u64,
    /// Bit-exact passthrough while true.
    pub bypassed: bool,
}
