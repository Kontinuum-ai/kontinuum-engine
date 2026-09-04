//! Versioned JSON loading and strict validation for sound worlds
//! (schema.rs L1 convention: unknown fields are a parse error, numeric
//! ranges are checked once at the boundary — applied worlds are trusted).

use kontinuum_ir::schema::bounds;
use kontinuum_ir::schema::InstrumentDef;

use super::{
    BassOverride, KickOverride, MixTargetOverride, PadOverride, PercOverride, SoundWorld,
    VoiceOverride, VoiceTag,
};

/// Current world authoring format; loaders gate on it like the corpus
/// artifacts gate on `artifact_version`.
pub const WORLD_FORMAT_VERSION: u32 = 1;

#[derive(Debug)]
pub enum WorldError {
    Json(serde_json::Error),
    /// Unknown or future authoring format.
    Version { found: u32, want: u32 },
    /// A world id must name the world.
    EmptyId,
    /// Out-of-range value or key/voice mismatch, with the offending path.
    Range(String),
    /// Groove affinities must name templates from [`crate::groove::ALL`].
    Groove(String),
}

impl std::fmt::Display for WorldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorldError::Json(e) => write!(f, "world JSON parse failed: {e}"),
            WorldError::Version { found, want } => {
                write!(f, "world format version {found} unsupported (want {want})")
            }
            WorldError::EmptyId => write!(f, "world id must not be empty"),
            WorldError::Range(path) => write!(f, "world override out of range at `{path}`"),
            WorldError::Groove(name) => {
                write!(f, "groove affinity `{name}` is not in the hand-made vocabulary")
            }
        }
    }
}

impl std::error::Error for WorldError {}

/// Parses and validates a world from JSON text.
pub fn load_json(text: &str) -> Result<SoundWorld, WorldError> {
    validate(serde_json::from_str(text).map_err(WorldError::Json)?)
}

fn in_range(path: &str, v: f32, range: (f32, f32)) -> Result<(), WorldError> {
    if v.is_finite() && v >= range.0 && v <= range.1 {
        Ok(())
    } else {
        Err(WorldError::Range(path.to_string()))
    }
}

fn validate(mut w: SoundWorld) -> Result<SoundWorld, WorldError> {
    if w.format_version != WORLD_FORMAT_VERSION {
        return Err(WorldError::Version { found: w.format_version, want: WORLD_FORMAT_VERSION });
    }
    if w.id.0.trim().is_empty() {
        return Err(WorldError::EmptyId);
    }
    validate_palette_overrides(&w.palette_overrides)?;
    validate_patch_overrides(&w.patch_overrides)?;
    for id in &w.sample_packs {
        if id.trim().is_empty() {
            return Err(WorldError::Range("sample_packs (empty pack id)".to_string()));
        }
    }
    for (id, m) in &w.mix_target_overrides {
        validate_mix(id, m)?;
    }
    for (name, affinity) in &w.groove_affinities {
        if !crate::groove::ALL.iter().any(|g| g.name == name.as_str()) {
            return Err(WorldError::Groove(name.clone()));
        }
        in_range(&format!("groove_affinities/{name}"), *affinity, (0.0, 1.0))?;
    }
    w.taste_tags = w.taste_tags.iter().map(|t| t.to_lowercase()).collect();
    Ok(w)
}

/// Patch swaps must name rig tracks and carry in-bounds instruments — the
/// identical bound catalog a session's own instruments are validated with
/// (kontinuum_ir::validate::instruments), so a validated world can never
/// produce a session that fails L2 on its patches.
fn validate_patch_overrides(
    patches: &std::collections::BTreeMap<String, InstrumentDef>,
) -> Result<(), WorldError> {
    for (id, inst) in patches {
        if !matches!(id.as_str(), "kick" | "perc" | "bass" | "pad") {
            return Err(WorldError::Range(format!("patch_overrides/{id} (unknown track)")));
        }
        let mut errors = Vec::new();
        kontinuum_ir::validate::instruments::check_instrument(inst, id, &mut errors);
        if let Some(e) = errors.into_iter().next() {
            return Err(WorldError::Range(format!("patch_overrides/{} ({})", e.path, e.message)));
        }
    }
    Ok(())
}

/// Rack-layer validation shared with the Creative Soul loader (issue #55):
/// keys name rig tracks, voice tags match, values respect the IR bounds.
pub(crate) fn validate_palette_overrides(
    overrides: &std::collections::BTreeMap<String, VoiceOverride>,
) -> Result<(), WorldError> {
    for (id, o) in overrides {
        check_key_voice(id, o)?;
        match o {
            VoiceOverride::Kick(o) => validate_kick(id, o)?,
            VoiceOverride::Perc(o) => validate_perc(id, o)?,
            VoiceOverride::Bass(o) => validate_bass(id, o)?,
            VoiceOverride::Pad(o) => validate_pad(id, o)?,
        }
    }
    Ok(())
}

fn check_key_voice(track_id: &str, o: &VoiceOverride) -> Result<(), WorldError> {
    let tag = match o {
        VoiceOverride::Kick(o) => o.voice,
        VoiceOverride::Perc(o) => o.voice,
        VoiceOverride::Bass(o) => o.voice,
        VoiceOverride::Pad(o) => o.voice,
    };
    let expected = match track_id {
        "kick" => VoiceTag::Kick,
        "perc" => VoiceTag::Perc,
        "bass" => VoiceTag::Bass,
        "pad" => VoiceTag::Pad,
        other => return Err(WorldError::Range(format!("palette_overrides/{other} (unknown track)"))),
    };
    if tag != expected {
        return Err(WorldError::Range(format!(
            "palette_overrides/{track_id} (voice tag {tag:?} does not match track)"
        )));
    }
    Ok(())
}

fn validate_kick(id: &str, o: &KickOverride) -> Result<(), WorldError> {
    if let Some(v) = o.tune_hz {
        in_range(&format!("{id}/tune_hz"), v, bounds::KICK_TUNE_HZ)?;
    }
    if let Some(v) = o.decay_ms {
        in_range(&format!("{id}/decay_ms"), v, bounds::KICK_DECAY_MS)?;
    }
    if let Some(v) = o.click {
        in_range(&format!("{id}/click"), v, bounds::UNIT)?;
    }
    if let Some(v) = o.drive {
        in_range(&format!("{id}/drive"), v, bounds::UNIT)?;
    }
    Ok(())
}

fn validate_perc(id: &str, o: &PercOverride) -> Result<(), WorldError> {
    if let Some(v) = o.decay_ms {
        in_range(&format!("{id}/decay_ms"), v, bounds::HAT_DECAY_MS)?;
    }
    if let Some(v) = o.tone {
        in_range(&format!("{id}/tone"), v, bounds::UNIT)?;
    }
    Ok(())
}

fn validate_bass(id: &str, o: &BassOverride) -> Result<(), WorldError> {
    if let Some(v) = o.cutoff_hz {
        in_range(&format!("{id}/cutoff_hz"), v, bounds::BASS_CUTOFF_HZ)?;
    }
    if let Some(v) = o.resonance {
        in_range(&format!("{id}/resonance"), v, bounds::UNIT)?;
    }
    if let Some(v) = o.glide_ms {
        in_range(&format!("{id}/glide_ms"), v, bounds::BASS_GLIDE_MS)?;
    }
    Ok(())
}

fn validate_pad(id: &str, o: &PadOverride) -> Result<(), WorldError> {
    if let Some(v) = o.attack_ms {
        in_range(&format!("{id}/attack_ms"), v, bounds::PAD_ATTACK_MS)?;
    }
    if let Some(v) = o.release_ms {
        in_range(&format!("{id}/release_ms"), v, bounds::PAD_RELEASE_MS)?;
    }
    if let Some(v) = o.detune_cents {
        in_range(&format!("{id}/detune_cents"), v, bounds::PAD_DETUNE_CENTS)?;
    }
    if let Some(v) = o.cutoff_hz {
        in_range(&format!("{id}/cutoff_hz"), v, bounds::PAD_CUTOFF_HZ)?;
    }
    Ok(())
}

fn validate_mix(id: &str, m: &MixTargetOverride) -> Result<(), WorldError> {
    if !matches!(id, "kick" | "perc" | "bass" | "pad") {
        return Err(WorldError::Range(format!("mix_target_overrides/{id} (unknown track)")));
    }
    if let Some(v) = m.gain {
        in_range(&format!("{id}/gain"), v, bounds::GAIN)?;
    }
    if let Some(v) = m.pan {
        in_range(&format!("{id}/pan"), v, bounds::PAN)?;
    }
    if let Some(v) = m.send_delay {
        in_range(&format!("{id}/send_delay"), v, bounds::UNIT)?;
    }
    if let Some(v) = m.send_reverb {
        in_range(&format!("{id}/send_reverb"), v, bounds::UNIT)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"{
        "format_version": 1,
        "id": "test",
        "name": "Test",
        "description": "probe world",
        "taste_tags": ["Techno"]
    }"#;

    #[test]
    fn minimal_world_loads_with_defaults() {
        let w = load_json(MINIMAL).expect("parses");
        assert_eq!(w.id.0, "test");
        assert_eq!(w.taste_tags, vec!["techno".to_string()], "tags normalize to lowercase");
        assert!(w.palette_overrides.is_empty());
        assert!(w.mix_target_overrides.is_empty());
    }

    #[test]
    fn version_gate_rejects_unknown_formats() {
        let bad = MINIMAL.replace(r#""format_version": 1"#, r#""format_version": 99"#);
        match load_json(&bad) {
            Err(WorldError::Version { found, want }) => assert_eq!((found, want), (99, 1)),
            other => panic!("expected version error, got {other:?}"),
        }
    }

    #[test]
    fn unknown_fields_are_parse_errors() {
        let bad = MINIMAL.replace(r#""description": "probe world""#, r#""descript": "x""#);
        assert!(matches!(load_json(&bad), Err(WorldError::Json(_))));
    }

    #[test]
    fn overrides_enforce_ir_bounds_and_key_voice() {
        let bad = MINIMAL.replace(
            r#""taste_tags": ["Techno"]"#,
            r#""palette_overrides": {"kick": {"voice": "kick", "tune_hz": 5.0}}"#,
        );
        assert_eq!(
            load_json(&bad).err().map(|e| e.to_string()),
            Some("world override out of range at `kick/tune_hz`".to_string()),
            "kick tune below the IR bound is rejected"
        );
        let mismatched =
            MINIMAL.replace(r#""taste_tags": ["Techno"]"#, r#""palette_overrides": {"kick": {"voice": "pad"}}"#);
        assert!(matches!(load_json(&mismatched), Err(WorldError::Range(_))));
        let unknown_track =
            MINIMAL.replace(r#""taste_tags": ["Techno"]"#, r#""mix_target_overrides": {"stab": {"gain": 1.0}}"#);
        assert!(matches!(load_json(&unknown_track), Err(WorldError::Range(_))));
    }

    #[test]
    fn groove_affinities_must_name_known_templates() {
        let bad = MINIMAL.replace(r#""taste_tags": ["Techno"]"#, r#""groove_affinities": {"swing-time": 0.5}"#);
        match load_json(&bad) {
            Err(WorldError::Groove(name)) => assert_eq!(name, "swing-time"),
            other => panic!("expected groove error, got {other:?}"),
        }
    }
}
