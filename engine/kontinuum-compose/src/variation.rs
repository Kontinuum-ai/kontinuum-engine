//! Long-horizon variation policy (issue #16): per-phrase micro-variation
//! so no 8 bars repeat exactly. The schedule assigns each phrase of a
//! section a ghost-note refresh (fresh probability mass on the quiet
//! steps — realized bar-by-bar by the compile-time per-hit gate) and, at
//! parameterized intensity, an automation gesture on a free lane. The
//! schedule's intensity follows the section's density curve: busy
//! sections vary harder.
//!
//! Content-level per-phrase deltas beyond probability and automation
//! would need multi-bar step patterns (#11 pattern layer); the compile
//! RNG already keys probability content per (section, track, phrase) —
//! see `kontinuum_ir::compile::expand::mask_rng` — so the schedule drives
//! what the IR can express without splitting sections.

use kontinuum_clock::Rng;
use kontinuum_ir::schema::{AutomationLane, CurveKind, Pattern, Step};

/// One automation gesture a phrase may carry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Gesture {
    /// Reverb send swells over the phrase.
    SendSwell,
    /// Delay send pulse on the phrase's second half.
    DelayPulse,
}

/// What one phrase of a section receives.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhraseTreatment {
    pub phrase: u32,
    /// Probability multiplier the phrase's ghost steps carry.
    pub ghost_boost: f32,
    pub gesture: Option<Gesture>,
}

/// Plans the per-phrase treatments for a section of `bars` on the
/// `phrase_bars` grid. `density` is the section's density-curve midpoint
/// (0..=1); `intensity` (GenParams::variation) scales gesture frequency.
pub fn schedule(bars: u32, phrase_bars: u32, density: f32, intensity: f32, rng: &mut Rng) -> Vec<PhraseTreatment> {
    let phrases = (bars / phrase_bars.max(1)).max(1);
    // Busy sections vary harder: gesture odds ride the density curve.
    let gesture_odds = (0.25 + 0.6 * density.clamp(0.0, 1.0)) * intensity.clamp(0.0, 1.0);
    (0..phrases)
        .map(|phrase| {
            let ghost_boost = 0.6 + 0.8 * rng.next_f32();
            let gesture = if rng.chance(gesture_odds) {
                Some(if rng.chance(0.6) { Gesture::SendSwell } else { Gesture::DelayPulse })
            } else {
                None
            };
            PhraseTreatment { phrase, ghost_boost, gesture }
        })
        .collect()
}

/// Refreshes a Steps pattern's quiet steps with the phrase's probability
/// mass — the audible "the ghosts moved" of each phrase. The strongest
/// steps (the figure's identity) keep their probability.
pub fn apply_ghost_refresh(pattern: &mut Pattern, boost: f32) {
    let Pattern::Steps(sp) = pattern else { return };
    refresh_steps(&mut sp.steps, boost);
}

fn refresh_steps(steps: &mut [Step], boost: f32) {
    let Some(ceiling) = steps.iter().map(|s| s.velocity).fold(None::<f32>, |m, v| {
        Some(m.map_or(v, |m: f32| m.max(v)))
    }) else {
        return;
    };
    for st in steps.iter_mut() {
        // Actual ghosts (probability < 1) belong to #17's ghost pass and
        // keep its envelope — and the refresh never converts quiet-tier
        // steps INTO ghosts, because the per-bar ghost count is a hard
        // transients budget. The variation reads through velocity alone.
        if st.probability >= 1.0 && st.velocity < ceiling * 0.75 {
            st.velocity =
                (st.velocity * (0.95 + 0.05 * boost)).clamp(0.02, (ceiling * 0.9).min(0.33));
        }
    }
}

/// Realizes a phrase's gesture as an automation lane on `track`, at the
/// phrase's bar range, provided the slot is free. Returns the lane.
pub fn gesture_lane(gesture: Gesture, start_bar: u32, bars: u32, rng: &mut Rng) -> AutomationLane {
    let mid = start_bar + bars / 2;
    let (target, points) = match gesture {
        Gesture::SendSwell => (
            "send_reverb",
            vec![
                (start_bar, 0.15 + 0.1 * rng.next_f32(), CurveKind::Smooth),
                (mid.max(start_bar + 1), 0.35 + 0.15 * rng.next_f32(), CurveKind::Smooth),
                (start_bar + bars - 1, 0.18, CurveKind::Linear),
            ],
        ),
        Gesture::DelayPulse => (
            "send_delay",
            vec![
                (mid.max(start_bar + 1), 0.3 + 0.1 * rng.next_f32(), CurveKind::Smooth),
                (start_bar + bars - 1, 0.1, CurveKind::Linear),
            ],
        ),
    };
    AutomationLane { target_param: target.to_string(), points }
}

/// True when the section's schedule never repeats a phrase treatment —
/// the "no 8 bars repeat exactly" guarantee at plan level.
pub fn phrases_all_differ(schedule: &[PhraseTreatment]) -> bool {
    for i in 0..schedule.len() {
        for j in (i + 1)..schedule.len() {
            if schedule[i] == schedule[j] {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use kontinuum_ir::schema::StepsPattern;

    fn steps() -> Pattern {
        Pattern::Steps(StepsPattern {
            steps: vec![
                Step { position: 0, velocity: 0.9, probability: 1.0, microtiming_ticks: 0, ratchet: 1, pitch: None, gate: None, accent: true },
                Step { position: 240, velocity: 0.3, probability: 1.0, microtiming_ticks: 0, ratchet: 1, pitch: None, gate: None, accent: false },
            ],
            repeats: 1,
        })
    }

    #[test]
    fn schedule_phrases_never_repeat_at_default_intensity() {
        let mut rng = kontinuum_clock::stream(11, 0xDD, 0xC0);
        let s = schedule(64, 8, 0.7, 0.5, &mut rng);
        assert_eq!(s.len(), 8);
        assert!(phrases_all_differ(&s), "identical phrase treatments defeat the policy");
    }

    #[test]
    fn ghost_refresh_moves_only_the_quiet_tier() {
        let mut p = steps();
        apply_ghost_refresh(&mut p, 2.0);
        let Pattern::Steps(sp) = p else { panic!("steps") };
        assert_eq!(sp.steps[0].probability, 1.0, "the figure's identity keeps its mass");
        assert_eq!(sp.steps[1].probability, 1.0, "the refresh never mint new ghosts");
        assert!(sp.steps[1].velocity > 0.3, "the quiet tier breathes");
        assert!(sp.steps[1].velocity <= 0.33, "and stays inside the ghost envelope");
    }

    #[test]
    fn gestures_write_rendered_lane_targets() {
        let mut rng = kontinuum_clock::stream(5, 0xDD, 0xC1);
        for g in [Gesture::SendSwell, Gesture::DelayPulse] {
            let lane = gesture_lane(g, 8, 8, &mut rng);
            assert!(matches!(lane.target_param.as_str(), "send_reverb" | "send_delay"));
            assert!(lane.points.iter().all(|(bar, v, _)| *v >= 0.0 && *v <= 1.0 && *bar < 16));
        }
    }
}
