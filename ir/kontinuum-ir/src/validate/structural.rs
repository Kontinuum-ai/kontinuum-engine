//! L1 structural checks: identity, chaining, tempo lane shape, reference
//! integrity.

use crate::compile::expand::resolve_param;
use crate::schema::bounds::MAX_SESSION_BARS;
use crate::schema::Session;
use crate::validate::{err, ErrorCatalog, ValidationError};

pub(super) fn check(s: &Session, out: &mut Vec<ValidationError>) {
    if s.version != crate::IR_VERSION {
        out.push(err(
            ErrorCatalog::E_BAD_VERSION,
            "/version",
            format!("session declares version {}, engine speaks IR_VERSION={}", s.version, crate::IR_VERSION),
            format!("set \"version\": {}", crate::IR_VERSION),
        ));
    }
    if s.sections.is_empty() {
        out.push(err(
            ErrorCatalog::E_EMPTY_SECTIONS,
            "/sections",
            "a session needs at least one section",
            "add a section: {\"id\":\"a\",\"bars\":8,\"energy_curve\":[0.5]}",
        ));
    }
    if s.tracks.is_empty() {
        out.push(err(
            ErrorCatalog::E_NO_TRACKS,
            "/tracks",
            "a session needs at least one track",
            "add a track: {\"id\":\"kick\",\"role\":\"kick\",\"instrument\":{\"kind\":\"kick\"}}",
        ));
    }
    if s.tracks.len() > MAX_TRACKS {
        out.push(err(
            ErrorCatalog::E_TOO_MANY_TRACKS,
            "/tracks",
            format!("{} tracks exceeds the 255 limit (TrackId is u8)", s.tracks.len()),
            "merge or drop tracks down to 255",
        ));
    }
    duplicate_ids(
        s.sections.iter().map(|sec| &sec.id),
        "/sections",
        ErrorCatalog::E_DUPLICATE_SECTION_ID,
        out,
    );
    duplicate_ids(
        s.tracks.iter().map(|t| &t.id),
        "/tracks",
        ErrorCatalog::E_DUPLICATE_TRACK_ID,
        out,
    );
    for (i, sec) in s.sections.iter().enumerate() {
        if sec.bars == 0 {
            out.push(err(
                ErrorCatalog::E_ZERO_BARS,
                format!("/sections/{i}/bars"),
                format!("section `{}` has zero length", sec.id),
                "set bars >= 1",
            ));
        }
    }
    let total = s.total_bars();
    if total > MAX_SESSION_BARS {
        out.push(err(
            ErrorCatalog::E_SESSION_TOO_LONG,
            "/sections",
            format!("session spans {total} bars; the ceiling is {MAX_SESSION_BARS}"),
            format!("shorten sections to fit {MAX_SESSION_BARS} bars total"),
        ));
    }
    check_tempo(s, out);
    check_references(s, out);
    check_souls(s, out);
}

/// Creative Soul stack structural rules (issue #55): the stack names packs
/// the engine may not have, so only self-contained sanity is checked here —
/// a soul ref can never break the engine regardless of pack content.
fn check_souls(s: &Session, out: &mut Vec<ValidationError>) {
    let Some(souls) = s.souls.as_ref() else { return };
    let mut seen: std::collections::BTreeSet<(String, Option<String>)> = Default::default();
    for (i, soul) in souls.iter().enumerate() {
        let path = format!("/souls/{i}");
        if soul.id.trim().is_empty() {
            out.push(err(
                ErrorCatalog::E_SOUL_EMPTY_ID,
                format!("{path}/id"),
                "soul id must not be empty",
                "set the pack id, e.g. \"detroit-909-minimalism\"",
            ));
        }
        if !soul.weight.is_finite() || soul.weight <= 0.0 || soul.weight > 1.0 {
            out.push(err(
                ErrorCatalog::E_SOUL_WEIGHT_RANGE,
                format!("{path}/weight"),
                format!("soul weight {} is outside 0 < w <= 1", soul.weight),
                "set a blend weight in (0, 1]; the blender normalizes the stack",
            ));
        }
        if soul.era.as_ref().is_some_and(|e| e.trim().is_empty()) {
            out.push(err(
                ErrorCatalog::E_SOUL_ERA_EMPTY,
                format!("{path}/era"),
                "soul era must not be blank when present",
                "drop the field for the pack default era, or name one",
            ));
        }
        if !seen.insert((soul.id.clone(), soul.era.clone())) {
            out.push(err(
                ErrorCatalog::E_SOUL_DUPLICATE,
                path,
                format!("soul `{}` (era {:?}) appears twice in the stack", soul.id, soul.era),
                "keep one entry per (id, era); blend weight belongs on that entry",
            ));
        }
    }
}

const MAX_TRACKS: usize = crate::schema::bounds::MAX_TRACKS;

fn duplicate_ids<'a>(
    ids: impl Iterator<Item = &'a String>,
    base: &str,
    code: &'static str,
    out: &mut Vec<ValidationError>,
) {
    let mut seen: std::collections::BTreeSet<&String> = std::collections::BTreeSet::new();
    for (i, id) in ids.enumerate() {
        if !seen.insert(id) {
            out.push(err(
                code,
                format!("{base}/{i}/id"),
                format!("duplicate id `{id}`"),
                format!("rename to a unique id (e.g. `{id}_2`)"),
            ));
        }
    }
}

fn check_tempo(s: &Session, out: &mut Vec<ValidationError>) {
    if s.tempo_lane.is_empty() {
        out.push(err(
            ErrorCatalog::E_TEMPO_EMPTY,
            "/tempo_lane",
            "tempo_lane needs at least the bar-0 anchor",
            "use [[0, 124.0]] for constant tempo",
        ));
        return;
    }
    if s.tempo_lane[0].0 != 0 {
        out.push(err(
            ErrorCatalog::E_TEMPO_BAR_ORDER,
            "/tempo_lane/0",
            format!("first breakpoint is bar {}, must be bar 0", s.tempo_lane[0].0),
            "anchor the lane at bar 0",
        ));
    }
    for (i, w) in s.tempo_lane.windows(2).enumerate() {
        if w[0].0 >= w[1].0 {
            out.push(err(
                ErrorCatalog::E_TEMPO_BAR_ORDER,
                format!("/tempo_lane/{}", i + 1),
                format!("breakpoint bars must strictly ascend: bar {} follows bar {}", w[1].0, w[0].0),
                "sort tempo_lane by bar and drop duplicates",
            ));
        }
    }
    for (i, (bar, bpm)) in s.tempo_lane.iter().enumerate() {
        if !f64_finite_positive(*bpm) || *bpm > MAX_BPM {
            out.push(err(
                ErrorCatalog::E_TEMPO_INVALID,
                format!("/tempo_lane/{i}/1"),
                format!("bpm {bpm} at bar {bar} is not a usable tempo"),
                format!("use a finite bpm in (0, {MAX_BPM}]"),
            ));
        }
    }
}

const MAX_BPM: f64 = 1000.0;

fn f64_finite_positive(v: f64) -> bool {
    v.is_finite() && v > 0.0
}

fn check_references(s: &Session, out: &mut Vec<ValidationError>) {
    let track_ids: std::collections::BTreeSet<&String> =
        s.tracks.iter().map(|t| &t.id).collect();
    for (si, sec) in s.sections.iter().enumerate() {
        for tid in sec.pattern_bindings.keys() {
            if !track_ids.contains(tid) {
                out.push(err(
                    ErrorCatalog::E_UNKNOWN_TRACK_BINDING,
                    format!("/sections/{si}/pattern_bindings/{tid}"),
                    format!("pattern binding references track `{tid}` which does not exist"),
                    format!(
                        "use one of: {}",
                        s.tracks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>().join(", ")
                    ),
                ));
            }
        }
        for (tid, lane) in &sec.automation {
            if !track_ids.contains(tid) {
                out.push(err(
                    ErrorCatalog::E_UNKNOWN_TRACK_BINDING,
                    format!("/sections/{si}/automation/{tid}"),
                    format!("automation lane references track `{tid}` which does not exist"),
                    format!(
                        "use one of: {}",
                        s.tracks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>().join(", ")
                    ),
                ));
            }
            if resolve_param(0, &lane.target_param).is_none() {
                out.push(err(
                    ErrorCatalog::E_UNKNOWN_PARAM_TARGET,
                    format!("/sections/{si}/automation/{tid}/target_param"),
                    format!("unknown automation target `{}`", lane.target_param),
                    "use one of: gain, pan, insert0, insert1, send_delay, send_reverb",
                ));
            }
            if lane.points.is_empty() {
                out.push(err(
                    ErrorCatalog::E_AUTOMATION_EMPTY,
                    format!("/sections/{si}/automation/{tid}/points"),
                    "automation lane has no points",
                    "add at least one point: [[0, 0.8, \"linear\"]]",
                ));
            }
            for (pi, w) in lane.points.windows(2).enumerate() {
                if w[0].0 >= w[1].0 {
                    out.push(err(
                        ErrorCatalog::E_AUTO_BAR_ORDER,
                        format!("/sections/{si}/automation/{tid}/points/{}", pi + 1),
                        format!(
                            "automation point bars must strictly ascend: bar {} follows bar {}",
                            w[1].0, w[0].0
                        ),
                        "sort points by bar and drop duplicates",
                    ));
                }
            }
            for (pi, (bar, _, _)) in lane.points.iter().enumerate() {
                if *bar >= sec.bars {
                    out.push(err(
                        ErrorCatalog::E_AUTO_BAR_OVERFLOW,
                        format!("/sections/{si}/automation/{tid}/points/{pi}/0"),
                        format!(
                            "automation point at section-relative bar {bar} is outside `{}` ({} bars)",
                            sec.id, sec.bars
                        ),
                        format!("use a bar in 0..{}", sec.bars),
                    ));
                }
            }
        }
    }
    for (ti, t) in s.tracks.iter().enumerate() {
        if let crate::schema::InstrumentDef::Sample(slot) = &t.instrument {
            if let Some(q) = &slot.query {
                if q.is_empty() {
                    out.push(err(
                        ErrorCatalog::E_SAMPLE_QUERY_EMPTY,
                        format!("/tracks/{ti}/instrument/query"),
                        "sample query is empty",
                        "describe the desired sound, e.g. \"short woody percussion\"",
                    ));
                }
            }
            if !slot.has_reference() {
                out.push(err(
                    ErrorCatalog::E_SAMPLE_REF_MISSING,
                    format!("/tracks/{ti}/instrument"),
                    "sample slot has neither query, id, nor recipe_hash",
                    "add a non-empty \"query\", a numeric \"id\", or a \"recipe_hash\"",
                ));
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn base_session_json() -> String {

        r#"{
            "version": 1, "seed": 1,
            "tempo_lane": [[0, 124.0]],
            "sections": [{"id": "a", "bars": 4, "energy_curve": [0.5],
                "pattern_bindings": {"k": {"generator": "euclidean", "k": 4, "n": 16}}}],
            "tracks": [{"id": "k", "role": "kick", "instrument": {"kind": "kick"}}]
        }"#
        .to_string()
    }

    pub(super) fn codes(json: &str) -> Vec<&'static str> {
        let s: Session = serde_json::from_str(json).expect("test fixture must parse");
        crate::validate::validate_session(&s)
            .expect_err("fixture must fail")
            .into_iter()
            .map(|e| e.code)
            .collect()
    }

    #[test]
    fn base_session_is_valid() {
        let s: Session = serde_json::from_str(&base_session_json()).expect("parse");
        crate::validate::validate_session(&s).expect("base must validate clean");
    }

    #[test]
    fn version_must_match() {
        let json = base_session_json().replace(r#""version": 1"#, r#""version": 2"#);
        assert!(codes(&json).contains(&ErrorCatalog::E_BAD_VERSION));
    }

    #[test]
    fn duplicate_section_and_track_ids() {
        let json = base_session_json().replace(
            r#""sections": [{"id": "a""#,
            r#""sections": [{"id": "a", "bars": 1, "energy_curve": [1.0]}, {"id": "a""#,
        );
        assert!(codes(&json).contains(&ErrorCatalog::E_DUPLICATE_SECTION_ID));
        let json = base_session_json().replace(
            r#""tracks": [{"id": "k""#,
            r#""tracks": [{"id": "k", "role": "kick", "instrument": {"kind": "kick"}}, {"id": "k""#,
        );
        assert!(codes(&json).contains(&ErrorCatalog::E_DUPLICATE_TRACK_ID));
    }

    #[test]
    fn tempo_lane_shape() {
        let json = base_session_json().replace("[[0, 124.0]]", "[]");
        assert!(codes(&json).contains(&ErrorCatalog::E_TEMPO_EMPTY));
        let json = base_session_json().replace("[[0, 124.0]]", "[[0, 0.0]]");
        assert!(codes(&json).contains(&ErrorCatalog::E_TEMPO_INVALID));
        let json = base_session_json().replace("[[0, 124.0]]", "[[4, 124.0]]");
        assert!(codes(&json).contains(&ErrorCatalog::E_TEMPO_BAR_ORDER));
        let json = base_session_json().replace("[[0, 124.0]]", "[[4, 124.0], [0, 120.0]]");
        assert!(codes(&json).contains(&ErrorCatalog::E_TEMPO_BAR_ORDER));
    }

    #[test]
    fn unknown_binding_and_param_target() {
        let json =
            base_session_json().replace(r#""pattern_bindings": {"k""#, r#""pattern_bindings": {"ghost""#);
        assert!(codes(&json).contains(&ErrorCatalog::E_UNKNOWN_TRACK_BINDING));
        let json = base_session_json().replace(
            r#""energy_curve": [0.5],"#,
            r#""energy_curve": [0.5], "automation": {"k": {"target_param": "wobble", "points": [[0, 1.0, "linear"]]}},"#,
        );
        assert!(codes(&json).contains(&ErrorCatalog::E_UNKNOWN_PARAM_TARGET));
    }

    #[test]
    fn automation_point_ordering_and_range() {
        let json = base_session_json().replace(
            r#""energy_curve": [0.5],"#,
            r#""energy_curve": [0.5], "automation": {"k": {"target_param": "gain",
                "points": [[2, 1.0, "linear"], [1, 0.5, "linear"], [9, 0.2, "linear"]]}},"#,
        );
        let set: std::collections::BTreeSet<&'static str> = codes(&json).into_iter().collect();
        assert!(set.contains(&ErrorCatalog::E_AUTO_BAR_ORDER));
        assert!(set.contains(&ErrorCatalog::E_AUTO_BAR_OVERFLOW));
    }

    #[test]
    fn sample_reference_rules() {
        let json = base_session_json()
            .replace(r#"{"id": "k", "role": "kick", "instrument": {"kind": "kick"}}"#,
                     r#"{"id": "k", "role": "kick", "instrument": {"kind": "sample", "query": ""}}"#);
        assert!(codes(&json).contains(&ErrorCatalog::E_SAMPLE_QUERY_EMPTY));
        let json = base_session_json()
            .replace(r#"{"id": "k", "role": "kick", "instrument": {"kind": "kick"}}"#,
                     r#"{"id": "k", "role": "kick", "instrument": {"kind": "sample"}}"#);
        assert!(codes(&json).contains(&ErrorCatalog::E_SAMPLE_REF_MISSING));
    }

    #[test]
    fn zero_bars_and_session_too_long() {
        let json = base_session_json().replace(r#""bars": 4"#, r#""bars": 0"#);
        assert!(codes(&json).contains(&ErrorCatalog::E_ZERO_BARS));
        let json = base_session_json().replace(r#""bars": 4"#, r#""bars": 1000000000"#);
        assert!(codes(&json).contains(&ErrorCatalog::E_SESSION_TOO_LONG));
    }
}
