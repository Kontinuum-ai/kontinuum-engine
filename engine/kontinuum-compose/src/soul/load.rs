//! Versioned JSON loading and strict validation for Creative Soul packs
//! (mirrors world/load.rs: unknown fields are a parse error, numeric
//! ranges checked once at the boundary — applied souls are trusted).

use kontinuum_ir::schema::bounds;

use super::{
    CreativeSoul, SoulArrangement, SoulGroove, SoulHarmony, SoulLayers, SoulMix, SoulSamples,
};

/// Current soul authoring format; loaders gate on it like world packs and
/// corpus artifacts gate on their versions.
pub const SOUL_FORMAT_VERSION: u32 = 1;

/// The era every pack must ship; era switching falls back to it per layer.
pub const DEFAULT_ERA: &str = "default";

#[derive(Debug)]
pub enum SoulError {
    Json(serde_json::Error),
    /// Unknown or future authoring format.
    Version { found: u32, want: u32 },
    /// A soul must have a non-empty id and name.
    EmptyId,
    /// Out-of-range value or key/voice mismatch, with the offending path.
    Range(String),
    /// Groove template/affinity must name the hand-made vocabulary.
    Groove(String),
    /// Every pack needs at least the default era.
    MissingDefaultEra,
}

impl std::fmt::Display for SoulError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SoulError::Json(e) => write!(f, "soul JSON parse failed: {e}"),
            SoulError::Version { found, want } => {
                write!(f, "soul format version {found} unsupported (want {want})")
            }
            SoulError::EmptyId => write!(f, "soul id and name must not be empty"),
            SoulError::Range(path) => write!(f, "soul layer out of range at `{path}`"),
            SoulError::Groove(name) => {
                write!(f, "groove `{name}` is not in the hand-made vocabulary")
            }
            SoulError::MissingDefaultEra => write!(f, "soul pack needs a `{DEFAULT_ERA}` era"),
        }
    }
}

impl std::error::Error for SoulError {}

/// Parses and validates a Creative Soul pack from JSON text.
pub fn load_json(text: &str) -> Result<CreativeSoul, SoulError> {
    validate(serde_json::from_str(text).map_err(SoulError::Json)?)
}

fn in_range(path: &str, v: f32, range: (f32, f32)) -> Result<(), SoulError> {
    if v.is_finite() && v >= range.0 && v <= range.1 {
        Ok(())
    } else {
        Err(SoulError::Range(path.to_string()))
    }
}

fn known_groove(name: &str) -> Result<(), SoulError> {
    if crate::groove::ALL.iter().any(|g| g.name == name) {
        Ok(())
    } else {
        Err(SoulError::Groove(name.to_string()))
    }
}

fn validate(mut s: CreativeSoul) -> Result<CreativeSoul, SoulError> {
    if s.format_version != SOUL_FORMAT_VERSION {
        return Err(SoulError::Version { found: s.format_version, want: SOUL_FORMAT_VERSION });
    }
    if s.id.0.trim().is_empty() || s.name.trim().is_empty() {
        return Err(SoulError::EmptyId);
    }
    if !s.eras.contains_key(DEFAULT_ERA) {
        return Err(SoulError::MissingDefaultEra);
    }
    for (era, layers) in &s.eras {
        validate_layers(&format!("eras/{era}"), layers)?;
    }
    s.taste_tags = s.taste_tags.iter().map(|t| t.to_lowercase()).collect();
    Ok(s)
}

fn validate_layers(base: &str, l: &SoulLayers) -> Result<(), SoulError> {
    if let Some(card) = l.style_card.as_ref() {
        if card.trim().is_empty() {
            return Err(SoulError::Range(format!("{base}/style_card")));
        }
    }
    if let Some(h) = l.harmony.as_ref() {
        validate_harmony(base, h)?;
    }
    if let Some(g) = l.groove.as_ref() {
        validate_groove(base, g)?;
    }
    if let Some(r) = l.rack.as_ref() {
        crate::world::load::validate_palette_overrides(&r.palette_overrides).map_err(|e| {
            SoulError::Range(format!("{base}/rack/{}", world_error_path(e)))
        })?;
    }
    if let Some(s) = l.samples.as_ref() {
        validate_samples(base, s)?;
    }
    if let Some(m) = l.mix.as_ref() {
        validate_mix(base, m)?;
    }
    if let Some(a) = l.arrangement.as_ref() {
        validate_arrangement(base, a)?;
    }
    Ok(())
}

fn world_error_path(e: crate::world::WorldError) -> String {
    match e {
        crate::world::WorldError::Range(p) => p,
        other => format!("{other:?}"),
    }
}

fn validate_harmony(base: &str, h: &SoulHarmony) -> Result<(), SoulError> {
    if h.progressions.is_empty() {
        return Err(SoulError::Range(format!("{base}/harmony/progressions")));
    }
    for (pi, prog) in h.progressions.iter().enumerate() {
        if !(2..=8).contains(&prog.len()) {
            return Err(SoulError::Range(format!("{base}/harmony/progressions/{pi}")));
        }
    }
    Ok(())
}

fn validate_groove(base: &str, g: &SoulGroove) -> Result<(), SoulError> {
    if let Some(t) = g.template.as_ref() {
        if let Err(SoulError::Groove(n)) = known_groove(t) {
            return Err(SoulError::Groove(format!("{base}/groove/template: {n}")));
        }
    }
    if let Some(v) = g.swing {
        in_range(&format!("{base}/groove/swing"), v, (0.0, 0.3))?;
    }
    if let Some(v) = g.jitter_ticks {
        in_range(&format!("{base}/groove/jitter_ticks"), v, (0.0, 12.0))?;
    }
    for (name, affinity) in &g.affinities {
        if let Err(SoulError::Groove(n)) = known_groove(name) {
            return Err(SoulError::Groove(format!("{base}/groove/affinities: {n}")));
        }
        in_range(&format!("{base}/groove/affinities/{name}"), *affinity, (0.0, 1.0))?;
    }
    Ok(())
}

fn validate_samples(base: &str, s: &SoulSamples) -> Result<(), SoulError> {
    for (i, q) in s.queries.iter().enumerate() {
        if q.trim().is_empty() {
            return Err(SoulError::Range(format!("{base}/samples/queries/{i}")));
        }
    }
    Ok(())
}

fn validate_mix(base: &str, m: &SoulMix) -> Result<(), SoulError> {
    for (id, t) in &m.profile {
        if !matches!(id.as_str(), "kick" | "perc" | "bass" | "pad") {
            return Err(SoulError::Range(format!("{base}/mix/{id} (unknown track)")));
        }
        in_range(&format!("{base}/mix/{id}/gain"), t.gain, bounds::GAIN)?;
        in_range(&format!("{base}/mix/{id}/pan"), t.pan, bounds::PAN)?;
        in_range(&format!("{base}/mix/{id}/send_delay"), t.send_delay, bounds::UNIT)?;
        in_range(&format!("{base}/mix/{id}/send_reverb"), t.send_reverb, bounds::UNIT)?;
    }
    Ok(())
}

fn validate_arrangement(base: &str, a: &SoulArrangement) -> Result<(), SoulError> {
    if let Some(b) = a.dev_bars {
        if b < 4 {
            return Err(SoulError::Range(format!("{base}/arrangement/dev_bars")));
        }
    }
    if let Some(b) = a.breakdown_bars {
        if b < 4 {
            return Err(SoulError::Range(format!("{base}/arrangement/breakdown_bars")));
        }
    }
    if let Some(arc) = a.energy_arc.as_ref() {
        let in_bounds = arc.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v));
        if arc.len() < 2 || !in_bounds {
            return Err(SoulError::Range(format!("{base}/arrangement/energy_arc")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"{
        "format_version": 1,
        "id": "test-soul",
        "name": "Test Soul",
        "description": "probe soul",
        "kind": "genre",
        "eras": { "default": { "style_card": "sparse and dusty" } }
    }"#;

    #[test]
    fn minimal_soul_loads_with_defaults() {
        let s = load_json(MINIMAL).expect("parses");
        assert_eq!(s.id.0, "test-soul");
        assert_eq!(s.kind, super::super::SoulKind::Genre);
        assert_eq!(s.taste_tags, Vec::<String>::new());
        assert!(s.eras.contains_key("default"));
    }

    #[test]
    fn version_gate_rejects_unknown_formats() {
        let bad = MINIMAL.replace(r#""format_version": 1"#, r#""format_version": 99"#);
        match load_json(&bad) {
            Err(SoulError::Version { found, want }) => assert_eq!((found, want), (99, 1)),
            other => panic!("expected version error, got {other:?}"),
        }
    }

    #[test]
    fn unknown_fields_are_parse_errors() {
        let bad = MINIMAL.replace(r#""kind": "genre","#, r#""kind": "genre","typo": 1,"#);
        assert!(matches!(load_json(&bad), Err(SoulError::Json(_))));
    }

    #[test]
    fn default_era_is_required() {
        let bad = MINIMAL.replace(r#""eras": { "default""#, r#""eras": { "1993""#);
        assert!(matches!(load_json(&bad), Err(SoulError::MissingDefaultEra)));
    }

    #[test]
    fn mix_profile_enforces_ir_bounds_and_rig_keys() {
        let bad = MINIMAL.replace(
            r#""style_card": "sparse and dusty""#,
            r#""style_card": "sparse", "mix": {"profile": {"kick": {"gain": 9.0, "pan": 0.0, "send_delay": 0.0, "send_reverb": 0.0}}}"#,
        );
        assert_eq!(
            load_json(&bad).err().map(|e| e.to_string()),
            Some("soul layer out of range at `eras/default/mix/kick/gain`".to_string())
        );
        let unknown = MINIMAL.replace(
            r#""style_card": "sparse and dusty""#,
            r#""style_card": "sparse", "mix": {"profile": {"stab": {"gain": 1.0, "pan": 0.0, "send_delay": 0.0, "send_reverb": 0.0}}}"#,
        );
        assert!(matches!(load_json(&unknown), Err(SoulError::Range(_))));
    }

    #[test]
    fn groove_layers_must_name_known_templates() {
        let bad = MINIMAL.replace(
            r#""style_card": "sparse and dusty""#,
            r#""style_card": "sparse", "groove": {"template": "swing-time"}"#,
        );
        assert!(matches!(load_json(&bad), Err(SoulError::Groove(_))));
    }

    #[test]
    fn taste_tags_normalize_to_lowercase() {
        let s = load_json(MINIMAL)
            .unwrap()
            ;
        let _ = s;
        let tagged = MINIMAL.replace(
            r#""kind": "genre","#,
            r#""kind": "genre", "taste_tags": ["Microhouse", "DUB"],"#,
        );
        assert_eq!(load_json(&tagged).unwrap().taste_tags, vec!["microhouse", "dub"]);
    }
}
