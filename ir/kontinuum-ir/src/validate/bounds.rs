//! L2 bounds lint: every numeric range from the schema contract, plus the
//! static density ceiling (256 onsets/bar/track).

use crate::compile::expand::onsets_per_bar;
use crate::schema::bounds::{
    self, DUCK_RELEASE_MS, EUCLID_MAX_N, GAIN, GATE_BEATS, GROOVE_BIAS_TICKS, GROOVE_JITTER_TICKS,
    GROOVE_SWING, INSERTS_PER_TRACK, MICROTIMING_TICKS, PAN, RATCHET, REPEATS, UNIT,
};
use crate::schema::{EuclideanPattern, Pattern, ProbabilityMaskPattern, Session, Step, StepsPattern, Track};
use crate::validate::instruments::{check_instrument, range_error};
use crate::validate::{err, f32_in_range, ErrorCatalog, ValidationError};

pub(super) const MAX_ONSETS_PER_BAR: f64 = 256.0;

pub(super) fn check(s: &Session, out: &mut Vec<ValidationError>) {
    if !f32_in_range(s.duck_release_ms, DUCK_RELEASE_MS) {
        out.push(err(
            ErrorCatalog::E_DUCK_RELEASE_RANGE,
            "/duck_release_ms".to_string(),
            format!(
                "duck_release_ms {} outside {}..={}",
                s.duck_release_ms, DUCK_RELEASE_MS.0, DUCK_RELEASE_MS.1
            ),
            format!(
                "set duck_release_ms between {} and {} ms",
                DUCK_RELEASE_MS.0, DUCK_RELEASE_MS.1
            ),
        ));
    }
    check_pattern_engine(s, out);
    for (si, sec) in s.sections.iter().enumerate() {
        if sec.energy_curve.is_empty() {
            out.push(err(
                ErrorCatalog::E_EMPTY_ENERGY_CURVE,
                format!("/sections/{si}/energy_curve"),
                format!("section `{}` has an empty energy curve", sec.id),
                "provide at least one value in 0..=1, e.g. [0.5]",
            ));
        }
        for (vi, v) in sec.energy_curve.iter().enumerate() {
            if !f32_in_range(*v, UNIT) {
                out.push(err(
                    ErrorCatalog::E_ENERGY_OUT_OF_RANGE,
                    format!("/sections/{si}/energy_curve/{vi}"),
                    format!("energy {v} outside 0..=1"),
                    "clamp energy values into 0..=1",
                ));
            }
        }
        for (curve, name) in [(&sec.density_curve, "density_curve"), (&sec.brightness_curve, "brightness_curve")] {
            for (vi, v) in curve.iter().enumerate() {
                if !f32_in_range(*v, UNIT) {
                    out.push(err(
                        ErrorCatalog::E_ENERGY_OUT_OF_RANGE,
                        format!("/sections/{si}/{name}/{vi}"),
                        format!("{name} value {v} outside 0..=1"),
                        format!("clamp {name} values into 0..=1"),
                    ));
                }
            }
        }
        for (tid, pattern) in &sec.pattern_bindings {
            let base = format!("/sections/{si}/pattern_bindings/{tid}");
            check_pattern(pattern, &base, out);
        }
    }
    for (ti, track) in s.tracks.iter().enumerate() {
        check_track(track, ti, out);
    }
}

fn check_pattern_engine(s: &Session, out: &mut Vec<ValidationError>) {
    let Some(engine) = &s.pattern_engine else { return };
    let base = "/pattern_engine";
    if !f32_in_range(engine.swing, GROOVE_SWING) {
        out.push(err(
            ErrorCatalog::E_SWING_RANGE,
            format!("{base}/swing"),
            format!("swing {} outside {}..={}", engine.swing, GROOVE_SWING.0, GROOVE_SWING.1),
            format!("set swing between {} and {} (0 = straight time)", GROOVE_SWING.0, GROOVE_SWING.1),
        ));
    }
    if !(GROOVE_BIAS_TICKS.0..=GROOVE_BIAS_TICKS.1).contains(&engine.bias_ticks) {
        out.push(err(
            ErrorCatalog::E_PATTERN_BIAS_RANGE,
            format!("{base}/bias_ticks"),
            format!(
                "bias_ticks {} outside {}..={}",
                engine.bias_ticks, GROOVE_BIAS_TICKS.0, GROOVE_BIAS_TICKS.1
            ),
            format!("set bias_ticks between {} and {}", GROOVE_BIAS_TICKS.0, GROOVE_BIAS_TICKS.1),
        ));
    }
    if !f32_in_range(engine.jitter_ticks, GROOVE_JITTER_TICKS) {
        out.push(err(
            ErrorCatalog::E_PATTERN_JITTER_RANGE,
            format!("{base}/jitter_ticks"),
            format!(
                "jitter_ticks {} outside {}..={}",
                engine.jitter_ticks, GROOVE_JITTER_TICKS.0, GROOVE_JITTER_TICKS.1
            ),
            format!(
                "set jitter_ticks between {} and {} ticks",
                GROOVE_JITTER_TICKS.0, GROOVE_JITTER_TICKS.1
            ),
        ));
    }
}

fn check_track(t: &Track, ti: usize, out: &mut Vec<ValidationError>) {    let base = format!("/tracks/{ti}");
    if !f32_in_range(t.gain, GAIN) {
        out.push(err(
            ErrorCatalog::E_GAIN_RANGE,
            format!("{base}/gain"),
            format!("gain {} outside 0..=2", t.gain),
            "set gain between 0.0 and 2.0 (1.0 = unity)",
        ));
    }
    if !f32_in_range(t.pan, PAN) {
        out.push(err(
            ErrorCatalog::E_PAN_RANGE,
            format!("{base}/pan"),
            format!("pan {} outside -1..=1", t.pan),
            "set pan between -1.0 (left) and 1.0 (right)",
        ));
    }
    if let Some(d) = t.duck_depth {
        if !f32_in_range(d, UNIT) {
            out.push(err(
                ErrorCatalog::E_DUCK_DEPTH_RANGE,
                format!("{base}/duck_depth"),
                format!("duck_depth {d} outside 0..=1"),
                "set duck_depth between 0.0 (no duck) and 1.0 (duck to unity)",
            ));
        }
    }
    for (which, v) in [("delay", t.sends.delay), ("reverb", t.sends.reverb)] {
        if !f32_in_range(v, UNIT) {
            out.push(err(
                ErrorCatalog::E_SEND_RANGE,
                format!("{base}/sends/{which}"),
                format!("send {which} {v} outside 0..=1"),
                "set sends between 0.0 and 1.0",
            ));
        }
    }
    if t.inserts.len() > INSERTS_PER_TRACK {
        out.push(err(
            ErrorCatalog::E_INSERT_OVERFLOW,
            format!("{base}/inserts"),
            format!("{} inserts exceeds the limit of {INSERTS_PER_TRACK}", t.inserts.len()),
            "keep at most 2 inserts per track; move the rest to the bus",
        ));
    }
    for (ii, insert) in t.inserts.iter().enumerate() {
        if !f32_in_range(insert.mix, UNIT) {
            out.push(err(
                ErrorCatalog::E_INSERT_MIX_RANGE,
                format!("{base}/inserts/{ii}/mix"),
                format!("insert mix {} outside 0..=1", insert.mix),
                "set mix between 0.0 (dry) and 1.0 (wet)",
            ));
        }
    }
    check_instrument(&t.instrument, &format!("{base}/instrument"), out);
}

fn check_pattern(p: &Pattern, base: &str, out: &mut Vec<ValidationError>) {
    check_repeats(p.repeats(), base, out);
    let density = onsets_per_bar(p);
    if density > MAX_ONSETS_PER_BAR {
        out.push(err(
            ErrorCatalog::E_DENSITY_TOO_HIGH,
            base,
            format!("pattern expands to {density} onsets/bar; ceiling is {MAX_ONSETS_PER_BAR}"),
            format!("thin the pattern below {MAX_ONSETS_PER_BAR} onsets per bar"),
        ));
    }
    match p {
        Pattern::Steps(sp) => check_steps(sp, base, out),
        Pattern::Euclidean(ep) => check_euclidean(ep, base, out),
        Pattern::ProbabilityMask(mp) => check_mask(mp, base, out),
    }
}

fn check_repeats(repeats: u32, base: &str, out: &mut Vec<ValidationError>) {
    if repeats == 0 {
        out.push(err(
            ErrorCatalog::E_REPEATS_ZERO,
            format!("{base}/repeats"),
            "repeats must be >= 1",
            "drop the field for a 1-bar pattern",
        ));
    } else if repeats > REPEATS.1 {
        out.push(err(
            ErrorCatalog::E_REPEATS_RANGE,
            format!("{base}/repeats"),
            format!("repeats {repeats} exceeds {}", REPEATS.1),
            format!("keep repeats <= {}", REPEATS.1),
        ));
    }
}

fn check_step(step: &Step, path: String, out: &mut Vec<ValidationError>) {
    if !f32_in_range(step.velocity, UNIT) {
        out.push(range_error(ErrorCatalog::E_VELOCITY_RANGE, format!("{path}/velocity"), "velocity", step.velocity, UNIT));
    }
    if !f32_in_range(step.probability, UNIT) {
        out.push(range_error(ErrorCatalog::E_PROBABILITY_RANGE, format!("{path}/probability"), "probability", step.probability, UNIT));
    }
    if !(bounds::MICROTIMING_TICKS.0..=bounds::MICROTIMING_TICKS.1).contains(&step.microtiming_ticks) {
        out.push(err(
            ErrorCatalog::E_MICROTIMING_RANGE,
            format!("{path}/microtiming_ticks"),
            format!("microtiming {} ticks outside {}..={}", step.microtiming_ticks, MICROTIMING_TICKS.0, MICROTIMING_TICKS.1),
            format!("keep microtiming within ±{} ticks (±⅛ of a 16th)", MICROTIMING_TICKS.1),
        ));
    }
    if !(RATCHET.0..=RATCHET.1).contains(&step.ratchet) {
        out.push(err(
            ErrorCatalog::E_RATCHET_RANGE,
            format!("{path}/ratchet"),
            format!("ratchet {} outside {}..={}", step.ratchet, RATCHET.0, RATCHET.1),
            format!("keep ratchet between {} and {} sub-hits", RATCHET.0, RATCHET.1),
        ));
    }
    if step.position >= kontinuum_clock::TICKS_PER_BAR as u32 {
        out.push(err(
            ErrorCatalog::E_TICKS_OVERFLOW,
            format!("{path}/position"),
            format!("step position {} is at or beyond the bar ({} ticks)", step.position, kontinuum_clock::TICKS_PER_BAR),
            format!("use a position in 0..{} (240 ticks per 16th)", kontinuum_clock::TICKS_PER_BAR),
        ));
    }
    if let Some(gate) = step.gate {
        if !f32_in_range(gate, GATE_BEATS) {
            out.push(range_error(ErrorCatalog::E_GATE_RANGE, format!("{path}/gate"), "gate", gate, GATE_BEATS));
        }
    }
    if let Some(pitch) = step.pitch {
        if !pitch.is_finite() {
            out.push(err(
                ErrorCatalog::E_PITCH_RANGE,
                format!("{path}/pitch"),
                format!("pitch {pitch} is not finite"),
                "use a finite MIDI pitch, e.g. 36.0",
            ));
        }
    }
}

fn check_steps(sp: &StepsPattern, base: &str, out: &mut Vec<ValidationError>) {
    for (j, step) in sp.steps.iter().enumerate() {
        check_step(step, format!("{base}/steps/{j}"), out);
    }
}

fn check_euclidean(ep: &EuclideanPattern, base: &str, out: &mut Vec<ValidationError>) {
    if ep.k == 0 || ep.n == 0 || ep.k > ep.n || ep.n > EUCLID_MAX_N {
        out.push(err(
            ErrorCatalog::E_EUCLID_RANGE,
            format!("{base}/k"),
            format!("euclidean k={}, n={} invalid (need 1 <= k <= n <= {EUCLID_MAX_N})", ep.k, ep.n),
            "keep n <= 4096 and k <= n (k = number of onsets)",
        ));
    }
    check_common_gen(&ep.velocity, &ep.probability, &ep.gate, &ep.pitch, base, out);
}

fn check_mask(mp: &ProbabilityMaskPattern, base: &str, out: &mut Vec<ValidationError>) {
    if !f32_in_range(mp.density, UNIT) {
        out.push(range_error(ErrorCatalog::E_DENSITY_RANGE, format!("{base}/density"), "density", mp.density, UNIT));
    }
    check_common_gen(&mp.velocity, &mp.probability, &mp.gate, &mp.pitch, base, out);
}

fn check_common_gen(
    velocity: &f32,
    probability: &f32,
    gate: &Option<f32>,
    pitch: &Option<f32>,
    base: &str,
    out: &mut Vec<ValidationError>,
) {
    if !f32_in_range(*velocity, UNIT) {
        out.push(range_error(ErrorCatalog::E_VELOCITY_RANGE, format!("{base}/velocity"), "velocity", *velocity, UNIT));
    }
    if !f32_in_range(*probability, UNIT) {
        out.push(range_error(ErrorCatalog::E_PROBABILITY_RANGE, format!("{base}/probability"), "probability", *probability, UNIT));
    }
    if let Some(gate) = gate {
        if !f32_in_range(*gate, GATE_BEATS) {
            out.push(range_error(ErrorCatalog::E_GATE_RANGE, format!("{base}/gate"), "gate", *gate, GATE_BEATS));
        }
    }
    if let Some(pitch) = pitch {
        if !pitch.is_finite() {
            out.push(err(
                ErrorCatalog::E_PITCH_RANGE,
                format!("{base}/pitch"),
                format!("pitch {pitch} is not finite"),
                "use a finite MIDI pitch, e.g. 36.0",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::structural::tests::base_session_json;

    fn codes(json: &str) -> Vec<&'static str> {
        let s: Session = serde_json::from_str(json).expect("fixture parses");
        crate::validate::validate_session(&s)
            .expect_err("fixture must fail")
            .into_iter()
            .map(|e| e.code)
            .collect()
    }

    #[test]
    fn velocity_and_probability_bounds() {
        let json = base_session_json().replace(
            r#"{"generator": "euclidean", "k": 4, "n": 16}"#,
            r#"{"steps": [{"position": 0, "velocity": 5.0, "probability": 2.0}]}"#,
        );
        let set: std::collections::BTreeSet<_> = codes(&json).into_iter().collect();
        assert!(set.contains(&ErrorCatalog::E_VELOCITY_RANGE));
        assert!(set.contains(&ErrorCatalog::E_PROBABILITY_RANGE));
    }

    #[test]
    fn microtiming_and_ratchet_and_ticks() {
        let json = base_session_json().replace(
            r#"{"generator": "euclidean", "k": 4, "n": 16}"#,
            r#"{"steps": [{"position": 9999, "microtiming_ticks": 9999, "ratchet": 99}]}"#,
        );
        let set: std::collections::BTreeSet<_> = codes(&json).into_iter().collect();
        assert!(set.contains(&ErrorCatalog::E_TICKS_OVERFLOW));
        assert!(set.contains(&ErrorCatalog::E_MICROTIMING_RANGE));
        assert!(set.contains(&ErrorCatalog::E_RATCHET_RANGE));
    }

    #[test]
    fn gain_pan_send_bounds() {
        let json = base_session_json().replace(
            r#"{"id": "k", "role": "kick", "instrument": {"kind": "kick"}}"#,
            r#"{"id": "k", "role": "kick", "instrument": {"kind": "kick"},
                "gain": 5.0, "pan": -3.0, "sends": {"delay": 1.5, "reverb": -0.1},
                "inserts": [{"type": "drive", "mix": 4.0}, {"type": "delay"}, {"type": "reverb"}]}"#,
        );
        let set: std::collections::BTreeSet<_> = codes(&json).into_iter().collect();
        for c in [
            ErrorCatalog::E_GAIN_RANGE,
            ErrorCatalog::E_PAN_RANGE,
            ErrorCatalog::E_SEND_RANGE,
            ErrorCatalog::E_INSERT_MIX_RANGE,
            ErrorCatalog::E_INSERT_OVERFLOW,
        ] {
            assert!(set.contains(&c), "missing {c}");
        }
    }

    #[test]
    fn instrument_param_bounds() {
        for (inst, code) in [
            (r#"{"kind": "kick", "tune_hz": 5.0, "decay_ms": 99999.0}"#, ErrorCatalog::E_KICK_TUNE_RANGE),
            (r#"{"kind": "kick", "decay_ms": 9.0}"#, ErrorCatalog::E_KICK_DECAY_RANGE),
            (r#"{"kind": "hat", "decay_ms": 0.1}"#, ErrorCatalog::E_HAT_DECAY_RANGE),
            (r#"{"kind": "bass", "cutoff_hz": 99999.0}"#, ErrorCatalog::E_BASS_CUTOFF_RANGE),
            (r#"{"kind": "pad", "attack_ms": 0.0}"#, ErrorCatalog::E_PAD_ATTACK_RANGE),
            (r#"{"kind": "kick", "click": 9.0}"#, ErrorCatalog::E_PARAM_RANGE),
        ] {
            let json = base_session_json()
                .replace(r#"{"kind": "kick"}"#, inst);
            assert!(codes(&json).contains(&code), "missing {code} for {inst}");
        }
    }

    #[test]
    fn euclid_and_density_and_energy() {
        let json = base_session_json().replace(
            r#"{"generator": "euclidean", "k": 4, "n": 16}"#,
            r#"{"generator": "euclidean", "k": 20, "n": 16}"#,
        );
        assert!(codes(&json).contains(&ErrorCatalog::E_EUCLID_RANGE));
        let json = base_session_json().replace(
            r#"{"generator": "euclidean", "k": 4, "n": 16}"#,
            r#"{"generator": "probability_mask", "density": 2.0}"#,
        );
        assert!(codes(&json).contains(&ErrorCatalog::E_DENSITY_RANGE));
        let json = base_session_json().replace("[0.5]", "[1.5]");
        assert!(codes(&json).contains(&ErrorCatalog::E_ENERGY_OUT_OF_RANGE));
        let json = base_session_json().replace("[0.5]", "[]");
        assert!(codes(&json).contains(&ErrorCatalog::E_EMPTY_ENERGY_CURVE));
    }

    #[test]
    fn density_flood_is_rejected() {
        let json = base_session_json().replace(
            r#"{"generator": "euclidean", "k": 4, "n": 16}"#,
            r#"{"generator": "euclidean", "k": 10000, "n": 10000}"#,
        );
        assert!(codes(&json).contains(&ErrorCatalog::E_DENSITY_TOO_HIGH));
    }

    #[test]
    fn duck_depth_and_release_bounds() {
        let json = base_session_json().replace(
            r#"{"id": "k", "role": "kick", "instrument": {"kind": "kick"}}"#,
            r#"{"id": "k", "role": "kick", "instrument": {"kind": "kick"}, "duck_depth": 1.5}"#,
        );
        assert!(codes(&json).contains(&ErrorCatalog::E_DUCK_DEPTH_RANGE));
        // `None` (field absent) is the role-default sentinel and must pass.
        let ok = base_session_json();
        let s: Session = serde_json::from_str(&ok).expect("fixture parses");
        assert!(crate::validate::validate_session(&s).is_ok());
    }

    #[test]
    fn duck_release_bounds() {
        let json = base_session_json().replace(
            r#""seed": 1,"#,
            r#""seed": 1, "duck_release_ms": 5000.0,"#,
        );
        assert!(codes(&json).contains(&ErrorCatalog::E_DUCK_RELEASE_RANGE));
    }

    #[test]
    fn gate_and_repeats_bounds() {
        let json = base_session_json().replace(
            r#"{"generator": "euclidean", "k": 4, "n": 16}"#,
            r#"{"steps": [{"position": 0, "gate": 999.0}], "repeats": 0}"#,
        );
        let set: std::collections::BTreeSet<_> = codes(&json).into_iter().collect();
        assert!(set.contains(&ErrorCatalog::E_GATE_RANGE));
        assert!(set.contains(&ErrorCatalog::E_REPEATS_ZERO));
    }
}
