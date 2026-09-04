//! On-device heuristic planner: rule-based, deterministic, infallible. This
//! is the floor of the composer stack — with no LLM and no network, the
//! session still evolves (PLAN §2.4: "a session in airplane mode keeps
//! playing and keeps evolving").
//!
//! Rules read the [`PlanContext`] snapshot and only target material that can
//! validate: the first future section for energy/automation, tracks that
//! exist for instrument params. Values sit well inside the kontinuum-ir
//! bounds (PAD_CUTOFF_HZ, KICK_DECAY_MS), so every diff survives the
//! validate/apply gate.

use crate::backend::{BackendError, ComposerBackend, PlanRequest, PlanResponse};
use serde_json::json;

/// The default, always-available backend (issue #41 ladder floor).
pub struct OnDeviceHeuristicBackend;

/// Back-compat alias: the heuristic backend under its original name.
pub type HeuristicBackend = OnDeviceHeuristicBackend;

/// Prompt keywords that pull energy down / up. Anything else keeps the
/// session's default push.
const DARKER_WORDS: [&str; 3] = ["darker", "calmer", "subtle"];
const BRIGHTER_WORDS: [&str; 4] = ["brighter", "harder", "louder", "energy"];

impl ComposerBackend for OnDeviceHeuristicBackend {
    fn name(&self) -> &str {
        "heuristic"
    }

    fn plan(&mut self, request: &PlanRequest) -> Result<PlanResponse, BackendError> {
        let mut diffs = Vec::new();
        let darker = DARKER_WORDS.iter().any(|w| request.prompt.contains(w));
        let brighter = BRIGHTER_WORDS.iter().any(|w| request.prompt.contains(w));
        let energy_target: f64 = if darker { 0.35 } else { 0.75 };

        // Rule 1: section energy follows the prompt's intensity words. Needs
        // runway — late in a section the re-anchor lands too late to matter.
        if request.bars_left_in_section >= 2 {
            if let Some(target) = target_section(request) {
                diffs.push(
                    json!({"op": "set_section_energy", "id": target, "energy": energy_curve(energy_target)})
                        .to_string(),
                );
            }
        }
        // Rule 2: darkness swells pad reverb on the future section.
        if darker && track_available(request, "pad") {
            if let Some(target) = target_section(request) {
                diffs.push(json!({
                    "op": "set_automation", "section": target, "track": "pad",
                    "lane": {"target_param": "send_reverb", "points": [[0, 0.5, "smooth"]]}
                }).to_string());
            }
        }
        // Rule 3: darkness muffles the pad; brightness tightens the kick.
        if darker && track_available(request, "pad") {
            diffs.push(json!({
                "op": "set_instrument_param", "track": "pad",
                "param": "cutoff_hz", "value": 900.0
            }).to_string());
        }
        if brighter && track_available(request, "kick") {
            diffs.push(json!({
                "op": "set_instrument_param", "track": "kick",
                "param": "decay_ms", "value": 180.0
            }).to_string());
        }
        let rules = diffs.len();
        Ok(PlanResponse {
            diffs,
            notes: format!("heuristic: {rules} rules applied"),
            backend_id: "on-device-heuristic".into(),
            latency_hint_ms: 0,
        })
    }
}

/// Section id the rules write to. With a live [`PlanContext`] that's the
/// first section at or after the playhead; without one, "intro" — generated
/// sessions always have an intro (palette::tracks / arrangement intro-first
/// rule), and SetSectionEnergy is exempt from the future-anchoring rule.
fn target_section(request: &PlanRequest) -> Option<String> {
    match request.context.future_section() {
        Some(section) => Some(section.id.clone()),
        // Empty context: legacy targeting (see PlanContext docs).
        None if request.context.sections.is_empty() => Some("intro".into()),
        None => None,
    }
}

/// Track availability. Without a context snapshot the legacy "intro + pad +
/// kick" palette is assumed; with one, only real tracks are targeted.
fn track_available(request: &PlanRequest, track: &str) -> bool {
    request.context.tracks.is_empty() || request.context.has_track(track)
}

fn energy_curve(target: f64) -> Vec<f64> {
    vec![target * 0.8, target, target, target * 0.9]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::PlanContext;
    use kontinuum_ir::{apply_diff, validate_session, IrDiff, Session};

    fn request(prompt: &str, bars_left: u32, context: PlanContext) -> PlanRequest {
        PlanRequest {
            style: "techno".into(),
            prompt: prompt.into(),
            bars_left_in_section: bars_left,
            progression: vec![(29, true)],
            taste_json: "{}".into(),
            style_card: String::new(),
            context,
            repair_context: String::new(),
        }
    }

    fn session() -> Session {
        let params = kontinuum_compose::arrangement::GenParams {
            seed: 7,
            target_bars: 32,
            ..Default::default()
        };
        kontinuum_compose::arrangement::generate_session(&params)
    }

    #[test]
    fn darker_prompt_yields_energy_and_reverb_rules() {
        let mut b = OnDeviceHeuristicBackend;
        let r = b.plan(&request("make it darker and more subtle", 6, PlanContext::default())).unwrap();
        assert!(r.diffs.len() >= 2);
        assert!(r.diffs.iter().any(|d| d.contains("set_section_energy")));
        assert!(r.diffs.iter().any(|d| d.contains("send_reverb")));
        assert_eq!(r.backend_id, "on-device-heuristic");
    }

    #[test]
    fn short_sections_skip_energy_rules() {
        let mut b = OnDeviceHeuristicBackend;
        let r = b.plan(&request("more", 1, PlanContext::default())).unwrap();
        assert!(r.diffs.is_empty(), "no time to land energy changes: {:?}", r.diffs);
    }

    #[test]
    fn diffs_validate_against_the_live_session() {
        let session = session();
        let context = PlanContext::from_session(&session, 8);
        let mut b = OnDeviceHeuristicBackend;
        let r = b.plan(&request("darker", 2, context)).unwrap();
        assert!(!r.diffs.is_empty());
        for raw in &r.diffs {
            let diff: IrDiff = serde_json::from_str(raw).expect("heuristic diff parses");
            let mut scratch = session.clone();
            apply_diff(&mut scratch, &diff, 8).expect("heuristic diff applies at bar 8");
            validate_session(&scratch).expect("session stays valid after the diff");
        }
    }

    #[test]
    fn targets_future_section_not_the_playing_one() {
        let session = session();
        let context = PlanContext::from_session(&session, 8);
        let mut b = OnDeviceHeuristicBackend;
        let r = b.plan(&request("darker", 4, context)).unwrap();
        let anchored: Vec<&String> = r
            .diffs
            .iter()
            .filter(|d| d.contains("set_automation"))
            .collect();
        assert!(!anchored.is_empty());
        assert!(
            anchored.iter().all(|d| !d.contains(r#""section":"intro""#)),
            "intro is in the past at bar 8: {anchored:?}"
        );
    }

    #[test]
    fn same_inputs_same_plan() {
        let session = session();
        let context = PlanContext::from_session(&session, 8);
        let mut a = OnDeviceHeuristicBackend;
        let mut b = OnDeviceHeuristicBackend;
        let ra = a.plan(&request("darker", 4, context.clone())).unwrap();
        let rb = b.plan(&request("darker", 4, context)).unwrap();
        assert_eq!(serde_json::to_string(&ra).unwrap(), serde_json::to_string(&rb).unwrap());
    }
}
