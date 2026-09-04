//! Per-backend composer telemetry (issue #36): invalid-IR rate, repair-retry
//! rate, p50/p95 latency and $/session-hour, accumulated per backend id and
//! surfaced as serializable rows the Settings UI can bind. Pure data — no
//! clocks, no I/O; callers supply latencies and session hours so the
//! numbers stay deterministic in tests and the audio path stays untouched.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::scripted::Tier;

/// One backend's running totals.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BackendTelemetry {
    pub backend_id: String,
    pub tier: Tier,
    /// Completed `plan` calls (successful transports, valid or not).
    pub plans: u32,
    /// Diff ops any plan proposed.
    pub proposed_ops: u32,
    /// Proposed ops the IR gate rejected (the invalid-IR signal).
    pub invalid_ops: u32,
    /// Validator-repair rounds spent against this backend.
    pub repair_rounds: u32,
    /// Completed-call latencies in ms (unsorted; percentile on read).
    pub latencies_ms: Vec<u64>,
    /// Cumulative cost in micro-USD (u64 to stay exact across additions).
    pub cost_usd_micros: u64,
}

/// All backends seen this session, keyed by backend id.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ComposerBackendTelemetry {
    backends: BTreeMap<String, BackendTelemetry>,
}

impl ComposerBackendTelemetry {
    pub fn record_plan(
        &mut self,
        backend_id: &str,
        tier: Tier,
        proposed_ops: usize,
        invalid_ops: usize,
        repair_rounds: u32,
        latency_ms: u64,
    ) {
        let entry = self.backends.entry(backend_id.to_string()).or_insert_with(|| BackendTelemetry {
            backend_id: backend_id.to_string(),
            tier,
            ..BackendTelemetry::default()
        });
        entry.plans += 1;
        entry.proposed_ops += proposed_ops as u32;
        entry.invalid_ops += invalid_ops as u32;
        entry.repair_rounds += repair_rounds;
        entry.latencies_ms.push(latency_ms);
    }

    /// Adds token-metered cost from the catalog's pricing. Unknown pricing
    /// records nothing — a free/local backend simply never gains cost.
    pub fn record_tokens(
        &mut self,
        backend_id: &str,
        tier: Tier,
        input_tokens: u64,
        output_tokens: u64,
        pricing: &crate::catalog::ModelInfo,
    ) {
        let Some(input) = pricing.input_cost_per_mtok else { return };
        let Some(output) = pricing.output_cost_per_mtok else { return };
        let micros = (input * (input_tokens as f64 / 1_000_000.0) * 1_000_000.0)
            + (output * (output_tokens as f64 / 1_000_000.0) * 1_000_000.0);
        let entry = self.backends.entry(backend_id.to_string()).or_insert_with(|| BackendTelemetry {
            backend_id: backend_id.to_string(),
            tier,
            ..BackendTelemetry::default()
        });
        entry.cost_usd_micros += micros.round() as u64;
    }

    pub fn backend(&self, backend_id: &str) -> Option<&BackendTelemetry> {
        self.backends.get(backend_id)
    }

    /// The Settings-facing snapshot: one row per backend, ordered by id.
    /// `session_hours` denominates the cost column (0 yields 0.0, not inf).
    pub fn rows(&self, session_hours: f64) -> Vec<BackendTelemetryRow> {
        self.backends
            .values()
            .map(|t| {
                let (p50, p95) = percentiles(&t.latencies_ms);
                BackendTelemetryRow {
                    backend_id: t.backend_id.clone(),
                    tier: t.tier,
                    plans: t.plans,
                    invalid_ir_rate: ratio(t.invalid_ops, t.proposed_ops),
                    repair_retry_rate: ratio(t.repair_rounds, t.plans),
                    latency_p50_ms: p50,
                    latency_p95_ms: p95,
                    cost_per_session_hour_usd: if session_hours > 0.0 {
                        t.cost_usd_micros as f64 / 1_000_000.0 / session_hours
                    } else {
                        0.0
                    },
                }
            })
            .collect()
    }
}

/// The data struct the Settings UI binds (issue #36).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BackendTelemetryRow {
    pub backend_id: String,
    pub tier: Tier,
    pub plans: u32,
    pub invalid_ir_rate: f32,
    pub repair_retry_rate: f32,
    pub latency_p50_ms: u64,
    pub latency_p95_ms: u64,
    pub cost_per_session_hour_usd: f64,
}

fn ratio(numerator: u32, denominator: u32) -> f32 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f32 / denominator as f32
    }
}

/// Nearest-rank percentiles over the observed latencies.
fn percentiles(latencies: &[u64]) -> (u64, u64) {
    if latencies.is_empty() {
        return (0, 0);
    }
    let mut sorted = latencies.to_vec();
    sorted.sort_unstable();
    let at = |pct: f64| -> u64 {
        let idx = ((pct / 100.0) * sorted.len() as f64).ceil() as usize;
        sorted[(idx.max(1) - 1).min(sorted.len() - 1)]
    };
    (at(50.0), at(95.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_compute_rates_percentiles_and_cost() {
        let mut t = ComposerBackendTelemetry::default();
        t.record_plan("http-cloud", Tier::T2Cloud, 10, 1, 0, 900);
        t.record_plan("http-cloud", Tier::T2Cloud, 10, 0, 2, 100);
        t.record_plan("http-cloud", Tier::T2Cloud, 10, 0, 0, 5000);
        t.record_plan("heuristic", Tier::T0Deterministic, 4, 0, 0, 0);

        let rows = t.rows(0.5);
        let cloud = rows.iter().find(|r| r.backend_id == "http-cloud").unwrap();
        assert_eq!(cloud.plans, 3);
        assert!((cloud.invalid_ir_rate - 1.0 / 30.0).abs() < 1e-6);
        assert!((cloud.repair_retry_rate - 2.0 / 3.0).abs() < 1e-6);
        assert_eq!(cloud.latency_p50_ms, 900);
        assert_eq!(cloud.latency_p95_ms, 5000);
        assert_eq!(cloud.cost_per_session_hour_usd, 0.0, "no token records, no cost");

        let floor = rows.iter().find(|r| r.backend_id == "heuristic").unwrap();
        assert_eq!(floor.tier, Tier::T0Deterministic);
        assert_eq!(floor.latency_p95_ms, 0);
    }

    #[test]
    fn token_costs_accumulate_and_free_pricing_stays_zero() {
        let pricing = crate::catalog::ModelInfo {
            id: "m".into(),
            name: "m".into(),
            context_window: 1,
            input_cost_per_mtok: Some(2.0),
            output_cost_per_mtok: Some(10.0),
            tool_call: true,
        };
        let free = crate::catalog::ModelInfo {
            input_cost_per_mtok: None,
            output_cost_per_mtok: None,
            ..pricing.clone()
        };
        let mut t = ComposerBackendTelemetry::default();
        t.record_tokens("http-cloud", Tier::T2Cloud, 2_000_000, 500_000, &pricing);
        t.record_tokens("http-cloud", Tier::T2Cloud, 0, 0, &free);
        let rows = t.rows(1.0);
        let cost = rows[0].cost_per_session_hour_usd;
        assert!((cost - 9.0).abs() < 1e-6, "2×$2 + 0.5×$10 = $9/hr, got {cost}");
    }

    #[test]
    fn zero_session_hours_and_empty_telemetry_degrade_to_zero() {
        let mut t = ComposerBackendTelemetry::default();
        t.record_plan("x", Tier::T2Cloud, 1, 0, 0, 10);
        assert_eq!(t.rows(0.0)[0].cost_per_session_hour_usd, 0.0);
        assert!(t.rows(1.0).is_empty() == false);
        assert!(ComposerBackendTelemetry::default().rows(1.0).is_empty());
    }

    #[test]
    fn rows_serialize_for_the_settings_binding() {
        let mut t = ComposerBackendTelemetry::default();
        t.record_plan("http-cloud", Tier::T2Cloud, 2, 1, 1, 120);
        let json = serde_json::to_string(&t.rows(0.25)).unwrap();
        assert!(json.contains("\"invalid_ir_rate\""));
        assert!(json.contains("\"latency_p95_ms\""));
        assert!(json.contains("\"cost_per_session_hour_usd\""));
        assert!(json.contains("\"t2_cloud\""));
    }
}
