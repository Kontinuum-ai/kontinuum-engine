//! TOML front-end for world authoring (#30): the same strict [`SoundWorld`]
//! schema, authored in the tiny TOML subset the mastering targets use
//! (`#` comments, `[table.path]` headers, `key = value` with strings,
//! numbers, booleans, and inline arrays of either). No third-party TOML
//! dependency — the parser produces a `serde_json::Value` tree that the
//! serde derive and the shared validator consume, so JSON and TOML worlds
//! accept exactly the same documents.
//!
//! Authoring shape (the world `format_version`, `id`, … keys live in
//! `[world]`; the override maps are table paths):
//!
//! ```toml
//! [world]
//! format_version = 1
//! id = "dust"
//! name = "Dust"
//! description = "Dusty micro."
//! taste_tags = ["micro", "minimal"]
//! sample_packs = ["texture-crackle-v1"]
//!
//! [palette_overrides.kick]
//! voice = "kick"
//! tune_hz = 52.0
//!
//! [patch_overrides.pad]
//! kind = "wavetable"
//! position = 0.35
//!
//! [mix_target_overrides.pad]
//! gain = 0.5
//!
//! [groove_affinities]
//! "drunk-shuffle" = 0.9
//! ```

use serde_json::Value as Json;

use super::load::{load_json, WorldError};

/// Parses and validates a world from TOML text. The `[world]` table (if
/// present) is spliced into the document root, so scalar fields author
/// under `[world]` while the override maps sit at their own paths.
pub fn load_toml(text: &str) -> Result<super::SoundWorld, WorldError> {
    let mut json = match toml_subset_to_json(text)? {
        Json::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    if let Some(Json::Object(world)) = json.remove("world") {
        for (k, v) in world {
            json.insert(k, v);
        }
    }
    let text = serde_json::to_string(&Json::Object(json)).map_err(WorldError::Json)?;
    load_json(&text)
}

fn toml_subset_to_json(text: &str) -> Result<Json, WorldError> {
    let mut root = serde_json::Map::new();
    let mut table_path: Vec<String> = Vec::new();
    for (ln, raw) in text.lines().enumerate() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(header) = line.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
            table_path = header
                .split('.')
                .map(|p| unquote(p.trim()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| WorldError::Range(format!("toml line {}: {e}", ln + 1)))?;
            if table_path.iter().any(String::is_empty) {
                return Err(WorldError::Range(format!("toml line {}: empty path segment", ln + 1)));
            }
            ensure_table(&mut root, &table_path);
            continue;
        }
        let Some((key_raw, value_raw)) = line.split_once('=') else {
            return Err(WorldError::Range(format!("toml line {}: expected key = value", ln + 1)));
        };
        let key = unquote(key_raw.trim())
            .map_err(|e| WorldError::Range(format!("toml line {}: {e}", ln + 1)))?;
        let value = parse_value(value_raw.trim())
            .map_err(|e| WorldError::Range(format!("toml line {}: {e}", ln + 1)))?;
        let table = ensure_table(&mut root, &table_path);
        table.insert(key, value);
    }
    Ok(Json::Object(root))
}

/// Cuts an unquoted `#` comment (quoted strings are preserved).
fn strip_comment(line: &str) -> &str {
    let mut in_string = false;
    let mut escape = false;
    for (i, c) in line.char_indices() {
        match c {
            '\\' if in_string => escape = !escape,
            '"' if !escape => in_string = !in_string,
            '#' if !in_string => return &line[..i],
            _ => escape = false,
        }
    }
    line
}

fn unquote(s: &str) -> Result<String, String> {
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        return Ok(s[1..s.len() - 1].to_string());
    }
    if s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Ok(s.to_string());
    }
    Err(format!("invalid key `{s}`"))
}

fn ensure_table<'a>(root: &'a mut serde_json::Map<String, Json>, path: &[String]) -> &'a mut serde_json::Map<String, Json> {
    let mut cursor = root;
    for seg in path {
        cursor = cursor
            .entry(seg.clone())
            .or_insert_with(|| Json::Object(serde_json::Map::new()))
            .as_object_mut()
            .expect("table paths only create objects");
    }
    cursor
}

fn parse_value(s: &str) -> Result<Json, String> {
    if s.starts_with('[') {
        let inner = s
            .strip_prefix('[')
            .and_then(|v| v.strip_suffix(']'))
            .ok_or_else(|| "unterminated array".to_string())?;
        let mut items = Vec::new();
        for part in split_array(inner) {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            items.push(parse_value(part)?);
        }
        return Ok(Json::Array(items));
    }
    if s.starts_with('"') {
        return unquote(s).map(Json::String);
    }
    match s {
        "true" => return Ok(Json::Bool(true)),
        "false" => return Ok(Json::Bool(false)),
        _ => {}
    }
    s.parse::<f64>().map(|n| {
        // Integer-looking literals stay integers: serde's u32/usize fields
        // reject `1.0` even though TOML has one number type.
        if n.fract() == 0.0 && n.abs() <= 9.0e15 {
            Json::Number(serde_json::Number::from(n as i64))
        } else {
            Json::Number(serde_json::Number::from_f64(n).unwrap_or_else(|| serde_json::Number::from(0)))
        }
    }).map_err(|_| format!("invalid value `{s}`"))
}

/// Splits an inline array on commas outside quoted strings.
fn split_array(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    let mut escape = false;
    for c in s.chars() {
        match c {
            '\\' if in_string => {
                escape = !escape;
                current.push(c);
            }
            '"' if !escape => {
                in_string = !in_string;
                current.push(c);
            }
            ',' if !in_string => {
                parts.push(std::mem::take(&mut current));
            }
            _ => {
                escape = false;
                current.push(c);
            }
        }
    }
    parts.push(current);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUST_TOML: &str = r#"
# shipped world, TOML authoring form
[world]
format_version = 1
id = "dust"
name = "Dust"
description = "Dusty micro."
taste_tags = ["micro", "minimal"]
sample_packs = ["texture-crackle-v1", "chord-oneshots-v1"]

[palette_overrides.kick]
voice = "kick"
tune_hz = 52.0
click = 0.35

[mix_target_overrides.pad]
gain = 0.5
send_reverb = 0.4

[groove_affinities]
"drunk-shuffle" = 0.9
"#;

    #[test]
    fn toml_world_matches_json_semantics() {
        let w = load_toml(DUST_TOML).expect("parses and validates");
        assert_eq!(w.id.0, "dust");
        assert_eq!(w.taste_tags, vec!["micro".to_string(), "minimal".to_string()]);
        assert_eq!(w.sample_packs, vec!["texture-crackle-v1", "chord-oneshots-v1"]);
        let kick = w.palette_overrides.get("kick").expect("kick override");
        if let super::super::VoiceOverride::Kick(k) = kick {
            assert_eq!(k.tune_hz, Some(52.0));
            assert_eq!(k.click, Some(0.35));
        } else {
            panic!("wrong override kind");
        }
        let pad = w.mix_target_overrides.get("pad").expect("pad mix");
        assert_eq!(pad.gain, Some(0.5));
        assert_eq!(w.groove_affinities.get("drunk-shuffle"), Some(&0.9));
    }

    #[test]
    fn toml_patch_override_round_trips_through_the_same_validator() {
        let text = DUST_TOML.replace(
            "[groove_affinities]",
            "[patch_overrides.pad]\nkind = \"wavetable\"\nposition = 0.35\n\n[groove_affinities]",
        );
        let w = load_toml(&text).expect("patch world");
        assert!(matches!(
            w.patch_overrides.get("pad"),
            Some(kontinuum_ir::schema::InstrumentDef::Wavetable(_))
        ));
    }

    #[test]
    fn toml_errors_name_the_line() {
        let bad = DUST_TOML.replace("tune_hz = 52.0", "tune_hz = 5.0");
        match load_toml(&bad) {
            Err(WorldError::Range(path)) => assert!(path.contains("kick/tune_hz"), "{path}"),
            other => panic!("expected range error, got {other:?}"),
        }
        let syntax = DUST_TOML.replace("tune_hz = 52.0", "tune_hz");
        assert!(matches!(load_toml(&syntax), Err(WorldError::Range(_))));
    }
}
