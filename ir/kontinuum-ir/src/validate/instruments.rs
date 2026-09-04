//! L2 instrument parameter bounds (one place per synth family).

use crate::schema::bounds::{
    self, BASS_CUTOFF_HZ, HAT_DECAY_MS, KICK_DECAY_MS, KICK_TUNE_HZ, PAD_ATTACK_MS,
    PAD_CUTOFF_HZ, PAD_DETUNE_CENTS, PAD_RELEASE_MS, UNIT,
};
use crate::schema::InstrumentDef;
use crate::validate::{err, f32_in_range, ErrorCatalog, ValidationError};

/// World/soul authoring boundary (#30): the same bounds check, exposed so
/// a validated world's `patch_overrides` run through the identical catalog.
pub fn check_instrument(inst: &InstrumentDef, base: &str, out: &mut Vec<ValidationError>) {
    let generic = |name: &str, v: f32, r: (f32, f32)| {
        range_error(ErrorCatalog::E_PARAM_RANGE, format!("{base}/{name}"), name, v, r)
    };
    match inst {
        InstrumentDef::Kick(k) => {
            if !f32_in_range(k.tune_hz, KICK_TUNE_HZ) {
                out.push(range_error(ErrorCatalog::E_KICK_TUNE_RANGE, format!("{base}/tune_hz"), "tune_hz", k.tune_hz, KICK_TUNE_HZ));
            }
            if !f32_in_range(k.decay_ms, KICK_DECAY_MS) {
                out.push(range_error(ErrorCatalog::E_KICK_DECAY_RANGE, format!("{base}/decay_ms"), "decay_ms", k.decay_ms, KICK_DECAY_MS));
            }
            if !f32_in_range(k.click, UNIT) {
                out.push(generic("click", k.click, UNIT));
            }
            if !f32_in_range(k.drive, UNIT) {
                out.push(generic("drive", k.drive, UNIT));
            }
        }
        InstrumentDef::Hat(h) => {
            if !f32_in_range(h.decay_ms, HAT_DECAY_MS) {
                out.push(range_error(ErrorCatalog::E_HAT_DECAY_RANGE, format!("{base}/decay_ms"), "decay_ms", h.decay_ms, HAT_DECAY_MS));
            }
            if !f32_in_range(h.tone, UNIT) {
                out.push(generic("tone", h.tone, UNIT));
            }
        }
        InstrumentDef::Bass(b) => {
            if !f32_in_range(b.cutoff_hz, BASS_CUTOFF_HZ) {
                out.push(range_error(ErrorCatalog::E_BASS_CUTOFF_RANGE, format!("{base}/cutoff_hz"), "cutoff_hz", b.cutoff_hz, BASS_CUTOFF_HZ));
            }
            if !f32_in_range(b.resonance, UNIT) {
                out.push(generic("resonance", b.resonance, UNIT));
            }
            if !f32_in_range(b.glide_ms, bounds::BASS_GLIDE_MS) {
                out.push(generic("glide_ms", b.glide_ms, bounds::BASS_GLIDE_MS));
            }
        }
        InstrumentDef::Clap(c) => {
            if !f32_in_range(c.decay_ms, (50.0, 1500.0)) {
                out.push(generic("decay_ms", c.decay_ms, (50.0, 1500.0)));
            }
            if !f32_in_range(c.tone, UNIT) {
                out.push(generic("tone", c.tone, UNIT));
            }
        }
        InstrumentDef::Snare(sn) => {
            if !f32_in_range(sn.tune_hz, (120.0, 420.0)) {
                out.push(generic("tune_hz", sn.tune_hz, (120.0, 420.0)));
            }
            if !f32_in_range(sn.decay_ms, (60.0, 900.0)) {
                out.push(generic("decay_ms", sn.decay_ms, (60.0, 900.0)));
            }
            if !f32_in_range(sn.snap, UNIT) {
                out.push(generic("snap", sn.snap, UNIT));
            }
        }
        InstrumentDef::Shaker(sh) => {
            if !f32_in_range(sh.decay_ms, (20.0, 600.0)) {
                out.push(generic("decay_ms", sh.decay_ms, (20.0, 600.0)));
            }
            if !f32_in_range(sh.tone, UNIT) {
                out.push(generic("tone", sh.tone, UNIT));
            }
        }
        InstrumentDef::Acid(a) => {
            if !f32_in_range(a.cutoff_hz, (60.0, 8000.0)) {
                out.push(generic("cutoff_hz", a.cutoff_hz, (60.0, 8000.0)));
            }
            if !f32_in_range(a.resonance, UNIT) {
                out.push(generic("resonance", a.resonance, UNIT));
            }
            if !f32_in_range(a.env_amt, (0.0, 4.0)) {
                out.push(generic("env_amt", a.env_amt, (0.0, 4.0)));
            }
            if !f32_in_range(a.glide_ms, (0.0, 500.0)) {
                out.push(generic("glide_ms", a.glide_ms, (0.0, 500.0)));
            }
        }
        InstrumentDef::Ep(e) => {
            if !f32_in_range(e.decay_ms, (200.0, 6000.0)) {
                out.push(generic("decay_ms", e.decay_ms, (200.0, 6000.0)));
            }
            if !f32_in_range(e.depth, (0.0, 6.0)) {
                out.push(generic("depth", e.depth, (0.0, 6.0)));
            }
        }
        InstrumentDef::Pluck(pl) => {
            if !f32_in_range(pl.damping, UNIT) {
                out.push(generic("damping", pl.damping, UNIT));
            }
            if !f32_in_range(pl.bright, UNIT) {
                out.push(generic("bright", pl.bright, UNIT));
            }
        }
        InstrumentDef::Stab(st) => {
            if !f32_in_range(st.cutoff_hz, (200.0, 12000.0)) {
                out.push(generic("cutoff_hz", st.cutoff_hz, (200.0, 12000.0)));
            }
            if !f32_in_range(st.decay_ms, (60.0, 2000.0)) {
                out.push(generic("decay_ms", st.decay_ms, (60.0, 2000.0)));
            }
            if !f32_in_range(st.detune_cents, (0.0, 40.0)) {
                out.push(generic("detune_cents", st.detune_cents, (0.0, 40.0)));
            }
        }
        InstrumentDef::Wavetable(w) => {
            if !f32_in_range(w.position, bounds::WAV_POSITION) {
                out.push(generic("position", w.position, bounds::WAV_POSITION));
            }
            if !f32_in_range(w.detune_cents, bounds::WAV_DETUNE_CENTS) {
                out.push(generic("detune_cents", w.detune_cents, bounds::WAV_DETUNE_CENTS));
            }
            if !f32_in_range(w.osc2_level, UNIT) {
                out.push(generic("osc2_level", w.osc2_level, UNIT));
            }
            if !f32_in_range(w.sub, UNIT) {
                out.push(generic("sub", w.sub, UNIT));
            }
            if !f32_in_range(w.cutoff_hz, bounds::WAV_CUTOFF_HZ) {
                out.push(generic("cutoff_hz", w.cutoff_hz, bounds::WAV_CUTOFF_HZ));
            }
            if !f32_in_range(w.release_ms, bounds::WAV_RELEASE_MS) {
                out.push(generic("release_ms", w.release_ms, bounds::WAV_RELEASE_MS));
            }
        }
        InstrumentDef::FmPerc(f) => {
            if !f32_in_range(f.ratio, bounds::FM_PERC_RATIO) {
                out.push(generic("ratio", f.ratio, bounds::FM_PERC_RATIO));
            }
            if !f32_in_range(f.index, bounds::FM_INDEX) {
                out.push(generic("index", f.index, bounds::FM_INDEX));
            }
            if !f32_in_range(f.feedback, UNIT) {
                out.push(generic("feedback", f.feedback, UNIT));
            }
            if !f32_in_range(f.decay_ms, bounds::FM_DECAY_MS) {
                out.push(generic("decay_ms", f.decay_ms, bounds::FM_DECAY_MS));
            }
        }
        InstrumentDef::Texture(t) => {
            if !f32_in_range(t.density, bounds::TEXTURE_DENSITY) {
                out.push(generic("density", t.density, bounds::TEXTURE_DENSITY));
            }
            if !f32_in_range(t.grain_ms, bounds::TEXTURE_GRAIN_MS) {
                out.push(generic("grain_ms", t.grain_ms, bounds::TEXTURE_GRAIN_MS));
            }
            if !f32_in_range(t.tone, UNIT) {
                out.push(generic("tone", t.tone, UNIT));
            }
        }
        InstrumentDef::Pad(p) => {
            if !f32_in_range(p.attack_ms, PAD_ATTACK_MS) {
                out.push(range_error(ErrorCatalog::E_PAD_ATTACK_RANGE, format!("{base}/attack_ms"), "attack_ms", p.attack_ms, PAD_ATTACK_MS));
            }
            if !f32_in_range(p.release_ms, PAD_RELEASE_MS) {
                out.push(generic("release_ms", p.release_ms, PAD_RELEASE_MS));
            }
            if !f32_in_range(p.detune_cents, PAD_DETUNE_CENTS) {
                out.push(generic("detune_cents", p.detune_cents, PAD_DETUNE_CENTS));
            }
            if !f32_in_range(p.cutoff_hz, PAD_CUTOFF_HZ) {
                out.push(generic("cutoff_hz", p.cutoff_hz, PAD_CUTOFF_HZ));
            }
        }
        InstrumentDef::Sample(slot) => {
            use crate::schema::GranularSlotParams;
            fn check_f32(
                out: &mut Vec<ValidationError>,
                base: &str,
                v: Option<f32>,
                r: (f32, f32),
                name: &str,
                code: &'static str,
            ) {
                if let Some(v) = v {
                    if !f32_in_range(v, r) {
                        out.push(range_error(code, format!("{base}/{name}"), name, v, r));
                    }
                }
            }
            check_f32(out, base, slot.transpose, bounds::SAMPLE_TRANSPOSE, "transpose", ErrorCatalog::E_SAMPLE_TRANSPOSE_RANGE);
            check_f32(out, base, slot.fine, bounds::SAMPLE_FINE, "fine", ErrorCatalog::E_SAMPLE_FINE_RANGE);
            check_f32(out, base, slot.stretch, bounds::SAMPLE_STRETCH, "stretch", ErrorCatalog::E_SAMPLE_STRETCH_RANGE);
            if let Some(g) = slot.choke_group {
                let (lo, hi) = bounds::SAMPLE_CHOKE_GROUP;
                if !(lo..=hi).contains(&g) {
                    out.push(err(
                        ErrorCatalog::E_SAMPLE_CHOKE_RANGE,
                        format!("{base}/choke_group"),
                        format!("choke_group {g} outside {lo}..={hi}"),
                        format!("set choke_group between {lo} and {hi}"),
                    ));
                }
            }
            if let Some(grain) = &slot.granular {
                let GranularSlotParams { grain_ms, density, spray_ms, pitch_jitter_cents, level } = grain;
                for (v, r, name) in [
                    (*grain_ms, bounds::SAMPLE_GRAIN_MS, "granular/grain_ms"),
                    (*density, bounds::SAMPLE_GRAIN_DENSITY, "granular/density"),
                    (*spray_ms, bounds::SAMPLE_GRAIN_SPRAY_MS, "granular/spray_ms"),
                    (*pitch_jitter_cents, bounds::SAMPLE_GRAIN_PITCH_JITTER, "granular/pitch_jitter_cents"),
                    (*level, bounds::SAMPLE_GRAIN_LEVEL, "granular/level"),
                ] {
                    check_f32(out, base, v, r, name, ErrorCatalog::E_SAMPLE_GRAIN_RANGE);
                }
            }
        }
        // Patch graph (issue #37): one place per family, structure + bounds.
        InstrumentDef::Custom(c) => crate::validate::patch::check(c, base, out),
    }
}

pub(super) fn range_error(
    code: &'static str,
    path: String,
    name: &str,
    v: f32,
    r: (f32, f32),
) -> ValidationError {
    err(
        code,
        path,
        format!("{name} {v} outside {}..={}", r.0, r.1),
        format!("set {name} between {} and {}", r.0, r.1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_range_kick_reports_specific_code() {
        let inst: InstrumentDef =
            serde_json::from_str(r#"{"kind": "kick", "tune_hz": 5.0}"#).expect("parse");
        let mut out = Vec::new();
        check_instrument(&inst, "/tracks/0/instrument", &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].code, ErrorCatalog::E_KICK_TUNE_RANGE);
        assert_eq!(out[0].path, "/tracks/0/instrument/tune_hz");
    }

    #[test]
    fn sample_slot_v1_fields_report_specific_codes() {
        let cases: &[(&str, &str)] = &[
            ("transpose", r#"{"kind": "sample", "transpose": -40.0}"#),
            ("fine", r#"{"kind": "sample", "fine": 200.0}"#),
            ("stretch", r#"{"kind": "sample", "stretch": 8.0}"#),
            ("choke_group", r#"{"kind": "sample", "choke_group": 17}"#),
            (
                "granular/grain_ms",
                r#"{"kind": "sample", "granular": {"grain_ms": 5.0}}"#,
            ),
            (
                "granular/level",
                r#"{"kind": "sample", "granular": {"level": 1.5}}"#,
            ),
        ];
        for (name, json) in cases {
            let inst: InstrumentDef = serde_json::from_str(json).expect("parse");
            let mut out = Vec::new();
            check_instrument(&inst, "/tracks/0/instrument", &mut out);
            assert_eq!(out.len(), 1, "{name}: expected exactly one error");
            assert!(
                out[0].path.ends_with(name),
                "{name}: wrong path {}",
                out[0].path
            );
        }
        // In-range values produce nothing.
        let inst: InstrumentDef = serde_json::from_str(
            r#"{"kind": "sample", "transpose": 12.0, "fine": -50.0, "stretch": 2.0,
                "choke_group": 1, "granular": {"grain_ms": 80.0, "density": 25.0}}"#,
        )
        .expect("parse");
        let mut out = Vec::new();
        check_instrument(&inst, "/tracks/0/instrument", &mut out);
        assert!(out.is_empty(), "in-range slot flagged: {:?}", out);
    }
}
