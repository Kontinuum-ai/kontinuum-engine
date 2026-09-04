//! Motion lanes (issue #16): section-scale send and gain automation that
//! gives the arrangement its macro-dynamics — a delay throw off the
//! percussion at the end of long non-development sections, a reverb swell
//! into breakdowns, and a slow gain breathing through development. One lane
//! per (section, track) slot: a gesture that finds its slot occupied (by
//! the pad's reverb arc, a sibling gesture, or presence) stands down.
//!
//! Every move is slew-safe by construction. The validator's ceiling is
//! ratio-based dB between adjacent points, so lanes hold the track's
//! static send value (never exactly 0.0, which would read as an infinite
//! dB jump) and lift at most to 0.55 on the final bar — under 21 dB/bar,
//! far inside the 24 dB/bar ceiling.

use kontinuum_clock::{Rng, stream};
use kontinuum_ir::schema::{AutomationLane, CurveKind, Section, Track};

/// RNG stream selectors for the motion pass.
const LANE_MOTION: u8 = 0xFD;
const PURPOSE_MOTION: u16 = 0xA3;

/// Peak of the final-bar delay throw (sends validate 0..=1).
const THROW_PEAK: f32 = 0.55;
/// Reverb swell target into breakdowns.
const SWELL_PEAK: f32 = 0.5;
/// Depth of the pre-breakdown reverb dip, as a fraction of the static send.
const SWELL_DIP: f32 = 0.15;
/// Floor for held send values: keeps every rise under the slew ceiling.
const SEND_FLOOR: f32 = 0.05;

/// Applies the motion gestures to `sections`, deterministic in `seed`.
pub(crate) fn apply_motion(sections: &mut [Section], tracks: &[Track], seed: u64) {
    let mut rng = stream(seed, LANE_MOTION, PURPOSE_MOTION);
    let track = |id: &str| tracks.iter().find(|t| t.id == id);
    let perc_delay = track("perc").map(|t| t.sends.delay).unwrap_or(SEND_FLOOR).max(SEND_FLOOR);
    let pad_reverb = track("pad").map(|t| t.sends.reverb).unwrap_or(SEND_FLOOR).max(SEND_FLOOR);
    let perc_gain = track("perc").map(|t| t.gain).unwrap_or(1.0);
    for si in 0..sections.len() {
        let into_breakdown = sections.get(si + 1).is_some_and(|s| s.id.starts_with("break_"));
        let sec = &mut sections[si];
        // Development sections breathe on the perc slot; the rest throw.
        if is_development(sec) {
            breathe(sec, perc_gain, &mut rng);
        } else {
            throw_lane(sec, perc_delay);
        }
        if into_breakdown {
            swell(sec, pad_reverb);
        }
    }
}

/// Delay throw: hold the track's static send, lift into the section's final
/// bar; the next section's static value releases it.
fn throw_lane(sec: &mut Section, hold: f32) {
    if sec.bars < 8 || !sec.pattern_bindings.contains_key("perc") || sec.automation.contains_key("perc") {
        return;
    }
    let mut points = vec![(0, hold, CurveKind::Linear)];
    if sec.bars > 2 {
        points.push((sec.bars - 2, hold, CurveKind::Linear));
    }
    points.push((sec.bars - 1, THROW_PEAK, CurveKind::Smooth));
    sec.automation
        .insert("perc".into(), AutomationLane { target_param: "send_delay".into(), points });
}

/// Reverb swell into the breakdown: dip the pad's room dry, then bloom to
/// the peak across the section's last two bars.
fn swell(sec: &mut Section, hold: f32) {
    if sec.bars < 2 || !sec.pattern_bindings.contains_key("pad") || sec.automation.contains_key("pad") {
        return;
    }
    let mut points = vec![(0, hold, CurveKind::Linear)];
    if sec.bars > 2 {
        points.push((sec.bars - 2, (hold * SWELL_DIP).max(SEND_FLOOR), CurveKind::Exp));
    }
    points.push((sec.bars - 1, SWELL_PEAK, CurveKind::Smooth));
    sec.automation
        .insert("pad".into(), AutomationLane { target_param: "send_reverb".into(), points });
}

/// Gain breathing: every 4 bars the percussion sways between its configured
/// gain and a shallow dip (3–8%), four bars full, four bars dipped.
fn breathe(sec: &mut Section, gain: f32, rng: &mut Rng) {
    if sec.bars < 8 || !sec.pattern_bindings.contains_key("perc") || sec.automation.contains_key("perc") {
        return;
    }
    let dip = 1.0 - rng.range_f32(0.03, 0.08);
    let value = |bar: u32| if (bar % 8) / 4 == 1 { gain * dip } else { gain };
    let mut points: Vec<(u32, f32, CurveKind)> =
        (0..sec.bars).step_by(4).map(|b| (b, value(b), CurveKind::Smooth)).collect();
    let last = sec.bars - 1;
    if points.last().is_some_and(|p| p.0 != last) {
        points.push((last, value(last), CurveKind::Smooth));
    }
    sec.automation.insert("perc".into(), AutomationLane { target_param: "gain".into(), points });
}

/// Dev sections are the ids the arrangement engine generates for
/// development (`dev_N`); everything named intro/reintro/outro/break is not.
fn is_development(sec: &Section) -> bool {
    let id = sec.id.as_str();
    !(id == "intro" || id == "reintro" || id == "outro" || id.starts_with("break_"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use kontinuum_ir::schema::{Pattern, StepsPattern};

    use super::*;

    fn section(id: &str, bars: u32, bound: &[&str]) -> Section {
        Section {
            id: id.into(),
            bars,
            energy_curve: vec![0.5],
            density_curve: Vec::new(),
            brightness_curve: Vec::new(),
            transition_in: None,
            transition_out: None,
            pattern_bindings: bound
                .iter()
                .map(|&t| {
                    (
                        t.to_string(),
                        Pattern::Steps(StepsPattern { steps: vec![], repeats: 1 }),
                    )
                })
                .collect(),
            automation: BTreeMap::new(),
        }
    }

    #[test]
    fn long_perc_sections_throw_and_dev_sections_breathe() {
        let tracks = crate::palette::tracks();
        let mut sections = vec![
            section("intro", 8, &["perc"]),
            section("dev_0", 16, &["perc", "pad"]),
            section("break_0", 8, &["pad"]),
            section("reintro", 8, &["perc", "pad"]),
        ];
        apply_motion(&mut sections, &tracks, 7);
        assert_eq!(sections[0].automation["perc"].target_param, "send_delay");
        assert_eq!(sections[1].automation["perc"].target_param, "gain");
        // The breakdown's predecessor is the dev section, which leaves the
        // pad slot free — the swell lands there.
        assert_eq!(sections[1].automation["pad"].target_param, "send_reverb");
        // The breakdown itself precedes the reintro: no swell, bare pad slot.
        assert!(!sections[2].automation.contains_key("pad"));
        assert_eq!(sections[3].automation["perc"].target_param, "send_delay");
        assert!(!sections[3].automation.contains_key("pad"));
    }

    #[test]
    fn motion_lanes_stay_inside_sections_and_slew_clean() {
        for seed in 0..20u64 {
            let tracks = crate::palette::tracks();
            let mut sections = vec![
                section("intro", 8, &["perc", "pad"]),
                section("dev_0", 12, &["perc", "pad"]),
                section("break_0", 8, &["pad"]),
                section("outro", 8, &["perc", "pad"]),
            ];
            apply_motion(&mut sections, &tracks, seed);
            for sec in &sections {
                for (tid, lane) in &sec.automation {
                    assert!(!lane.points.is_empty());
                    for w in lane.points.windows(2) {
                        assert!(w[0].0 < w[1].0, "bars must strictly ascend");
                        let bars = f64::from((w[1].0 - w[0].0).max(1));
                        let ratio = f64::from(w[1].1) / f64::from(w[0].1);
                        let db = 20.0 * ratio.abs().max(1e-4).log10();
                        assert!(db / bars < 24.0, "slew {db:.1} dB/bar on {tid}");
                    }
                    for (bar, v, _) in &lane.points {
                        assert!(*bar < sec.bars, "{tid} point at {bar} outside {}", sec.bars);
                        assert!((0.0..=1.0).contains(v), "{tid} value {v}");
                    }
                }
            }
        }
    }

    #[test]
    fn occupied_slots_stand_down() {
        let tracks = crate::palette::tracks();
        let mut sections = vec![section("intro", 8, &["perc"])];
        sections[0].automation.insert(
            "perc".into(),
            AutomationLane { target_param: "send_reverb".into(), points: vec![(0, 0.3, CurveKind::Linear)] },
        );
        apply_motion(&mut sections, &tracks, 7);
        assert_eq!(sections[0].automation["perc"].target_param, "send_reverb");
    }
}
