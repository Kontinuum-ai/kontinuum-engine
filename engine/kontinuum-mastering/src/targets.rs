//! Versioned mastering targets (`mastering-targets.toml`, issue #28).
//!
//! The shipped file is the source: [`MasteringTargets::load`] parses and
//! validates it, and a test pins file ⇄ code agreement. The shipped
//! values are **hypotheses** from the minimal-techno brief, not
//! measurements: streaming-era club masters run short-term LUFS in peak
//! sections around −8…−6 and integrated around −9…−8 with a −1.0 dBTP
//! ceiling. They stay hypothesis-flagged until the reference corpus
//! (issue #23) is measured; the schema version and the `name` field keep
//! provenance explicit so a future measured profile cannot be confused
//! with this guess (see the fixture's provenance header).
//!
//! The file format is the tiny TOML subset parsed by
//! [`MasteringTargets::from_toml`]: comments, one optional `[tolerances]`
//! table, and `key = value` scalars. Strict parsing *is* the validation —
//! unknown keys, unknown tables and non-finite numbers are errors,
//! mirroring the `deny_unknown_fields` serde contract of the JSON
//! round-trip path.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Current schema version. Loading a file with a different version is an
/// error — targets drive level decisions and silent drift is dangerous.
pub const TARGETS_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum TargetsError {
    #[error("targets file unreadable: {0}")]
    Io(#[from] std::io::Error),
    #[error("targets JSON parse failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("targets TOML invalid: {0}")]
    Toml(String),
    #[error("targets schema version {found} unsupported (want {TARGETS_SCHEMA_VERSION})")]
    Version { found: u32 },
}

/// Tolerances around each target. Missing fields default via serde.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TargetsTolerances {
    pub integrated_lufs: f64,
    pub short_term_lufs: f64,
    pub lra_lu: f64,
    pub ceiling_dbtp: f64,
    pub tilt_cdb: f64,
}

impl Default for TargetsTolerances {
    fn default() -> Self {
        TargetsTolerances {
            integrated_lufs: 0.5,
            short_term_lufs: 1.0,
            lra_lu: 1.5,
            ceiling_dbtp: 0.2,
            tilt_cdb: 30.0,
        }
    }
}

/// Mastering loudness/spectrum targets as data. Serialize order matches
/// the fixture; `deny_unknown_fields` catches schema drift early.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MasteringTargets {
    pub schema_version: u32,
    pub name: String,
    /// Program loudness target (LUFS, BS.1770 integrated).
    pub integrated_lufs: f64,
    /// Short-term (3 s) loudness expected in peak sections (LUFS).
    pub short_term_lufs: f64,
    /// Loudness range (LU, EBU R128 style 10th–95th percentile spread).
    pub lra_lu: f64,
    /// True-peak ceiling the limiter enforces (dBTP).
    pub ceiling_dbtp: f64,
    /// Pivot frequency of the corrective tilt EQ (Hz).
    pub tilt_hz: f64,
    /// Corrective tilt toward the subgenre spectral target, in centi-dB
    /// (×0.01 dB). Positive brightens (high shelf up, low shelf down).
    /// 0.0 until the reference corpus gives evidence — the chain never
    /// guesses a spectral move.
    pub tilt_cdb: f64,
    #[serde(default)]
    pub tolerances: TargetsTolerances,
}

impl MasteringTargets {
    /// The shipped minimal-techno hypothesis (see module docs).
    pub fn hypothesis() -> Self {
        MasteringTargets {
            schema_version: TARGETS_SCHEMA_VERSION,
            name: "minimal-techno-hypothesis".into(),
            integrated_lufs: -8.5,
            short_term_lufs: -7.0,
            lra_lu: 4.0,
            ceiling_dbtp: -1.0,
            tilt_hz: 700.0,
            tilt_cdb: 0.0,
            tolerances: TargetsTolerances::default(),
        }
    }

    pub fn load(path: &Path) -> Result<Self, TargetsError> {
        let text = std::fs::read_to_string(path)?;
        Self::from_toml(&text)
    }

    pub fn from_json(text: &str) -> Result<Self, TargetsError> {
        let targets: MasteringTargets = serde_json::from_str(text)?;
        if targets.schema_version != TARGETS_SCHEMA_VERSION {
            return Err(TargetsError::Version { found: targets.schema_version });
        }
        Ok(targets)
    }

    /// Parses the shipped file's TOML subset: `#` comments, blank lines,
    /// the optional `[tolerances]` table, and `key = value` scalars
    /// (quoted strings or finite numbers). Anything else — unknown keys,
    /// unknown tables, non-finite numbers — is an error: targets drive
    /// level decisions and silent drift is dangerous.
    pub fn from_toml(text: &str) -> Result<Self, TargetsError> {
        let mut doc = TomlDoc::default();
        for (line_no, raw) in text.lines().enumerate() {
            let line_no = line_no + 1;
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(table) = line.strip_prefix('[') {
                let name = table.strip_suffix(']').ok_or_else(|| {
                    TargetsError::Toml(format!("line {line_no}: unclosed table header"))
                })?;
                doc.enter_table(name, line_no)?;
                continue;
            }
            let (key, value) = line.split_once('=').ok_or_else(|| {
                TargetsError::Toml(format!("line {line_no}: expected `key = value`"))
            })?;
            doc.assign(key.trim(), strip_comment(value.trim()), line_no)?;
        }
        doc.finish()
    }
}

/// Strips a trailing `# comment` from a scalar value. Quoted strings are
/// cut after the closing quote; bare numbers at the first `#`.
fn strip_comment(value: &str) -> &str {
    match value.strip_prefix('"') {
        Some(rest) => match rest.find('"') {
            Some(end) => &value[..end + 2],
            None => value,
        },
        None => match value.find('#') {
            Some(at) => value[..at].trim_end(),
            None => value,
        },
    }
}

/// Root-table keys [`MasteringTargets::from_toml`] accepts.
const ROOT_KEYS: [&str; 8] = [
    "schema_version",
    "name",
    "integrated_lufs",
    "short_term_lufs",
    "lra_lu",
    "ceiling_dbtp",
    "tilt_hz",
    "tilt_cdb",
];

const TOML_TABLES: [&str; 2] = ["", "tolerances"];

/// Accumulator for [`MasteringTargets::from_toml`]: keys land in the root
/// table or `[tolerances]`, then `finish` assembles the validated struct.
#[derive(Default)]
struct TomlDoc {
    root: Vec<(String, f64)>,
    root_names: Vec<(String, String)>,
    tolerances: Vec<(String, f64)>,
    table: &'static str,
}

impl TomlDoc {
    fn enter_table(&mut self, name: &str, line_no: usize) -> Result<(), TargetsError> {
        match TOML_TABLES.iter().find(|t| **t == name) {
            Some(t) => {
                self.table = t;
                Ok(())
            }
            None => Err(TargetsError::Toml(format!(
                "line {line_no}: unknown table [{name}] (want one of {TOML_TABLES:?})"
            ))),
        }
    }

    fn number(&self, raw: &str, line_no: usize) -> Result<f64, TargetsError> {
        let v: f64 = raw
            .parse()
            .map_err(|_| TargetsError::Toml(format!("line {line_no}: `{raw}` is not a number")))?;
        if !v.is_finite() {
            return Err(TargetsError::Toml(format!("line {line_no}: `{raw}` is not finite")));
        }
        Ok(v)
    }

    fn assign(&mut self, key: &str, value: &str, line_no: usize) -> Result<(), TargetsError> {
        if value.is_empty() {
            return Err(TargetsError::Toml(format!("line {line_no}: `{key}` has no value")));
        }
        if self.table == "tolerances" {
            let v = self.number(value, line_no)?;
            self.tolerances.push((key.to_string(), v));
            return Ok(());
        }
        if !ROOT_KEYS.contains(&key) {
            return Err(TargetsError::Toml(format!("unknown key `{key}`")));
        }
        if let Some(s) = value.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
            self.root_names.push((key.to_string(), s.to_string()));
        } else {
            let v = self.number(value, line_no)?;
            self.root.push((key.to_string(), v));
        }
        Ok(())
    }

    fn finish(self) -> Result<MasteringTargets, TargetsError> {
        let num = |entries: &[(String, f64)], key: &str| {
            entries.iter().find(|(k, _)| k == key).map(|(_, v)| *v)
        };
        let name = self
            .root_names
            .iter()
            .find(|(k, _)| k == "name")
            .map(|(_, v)| v.clone())
            .ok_or_else(|| TargetsError::Toml("missing `name`".to_string()))?;
        let schema_version = num(&self.root, "schema_version")
            .ok_or_else(|| TargetsError::Toml("missing `schema_version`".to_string()))?;
        let schema_version = u32::try_from(schema_version as i64)
            .map_err(|_| TargetsError::Toml(format!("`schema_version` {schema_version} is not a u32")))?;
        if schema_version != TARGETS_SCHEMA_VERSION {
            return Err(TargetsError::Version { found: schema_version });
        }
        let missing = |key: &str| TargetsError::Toml(format!("missing `{key}`"));
        let mut tolerances = TargetsTolerances::default();
        for (key, value) in &self.tolerances {
            match key.as_str() {
                "integrated_lufs" => tolerances.integrated_lufs = *value,
                "short_term_lufs" => tolerances.short_term_lufs = *value,
                "lra_lu" => tolerances.lra_lu = *value,
                "ceiling_dbtp" => tolerances.ceiling_dbtp = *value,
                "tilt_cdb" => tolerances.tilt_cdb = *value,
                other => {
                    return Err(TargetsError::Toml(format!("unknown tolerances key `{other}`")))
                }
            }
        }
        Ok(MasteringTargets {
            schema_version,
            name,
            integrated_lufs: num(&self.root, "integrated_lufs")
                .ok_or_else(|| missing("integrated_lufs"))?,
            short_term_lufs: num(&self.root, "short_term_lufs")
                .ok_or_else(|| missing("short_term_lufs"))?,
            lra_lu: num(&self.root, "lra_lu").ok_or_else(|| missing("lra_lu"))?,
            ceiling_dbtp: num(&self.root, "ceiling_dbtp").ok_or_else(|| missing("ceiling_dbtp"))?,
            tilt_hz: num(&self.root, "tilt_hz").ok_or_else(|| missing("tilt_hz"))?,
            tilt_cdb: num(&self.root, "tilt_cdb").ok_or_else(|| missing("tilt_cdb"))?,
            tolerances,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_toml_fixture_matches_hypothesis() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/mastering-targets.toml");
        let loaded = MasteringTargets::load(&path).expect("fixture must load");
        assert_eq!(loaded, MasteringTargets::hypothesis(), "fixture drifted from hypothesis()");
        // Brief hypotheses: short-term −8…−6, integrated −9…−8, ceiling −1.0.
        assert!((-8.0..=-6.0).contains(&loaded.short_term_lufs));
        assert!((-9.0..=-8.0).contains(&loaded.integrated_lufs));
        assert_eq!(loaded.ceiling_dbtp, -1.0);
    }

    #[test]
    fn toml_parser_honors_comments_inline_notes_and_default_tolerances() {
        let text = "# leading note\n\
                    schema_version = 1\n\
                    name = \"profile-x\" # trailing note\n\
                    integrated_lufs = -8.5\n\
                    short_term_lufs = -7.0\n\
                    lra_lu = 4\n\
                    ceiling_dbtp = -1.0\n\
                    tilt_hz = 700.0\n\
                    tilt_cdb = 0.0\n";
        let parsed = MasteringTargets::from_toml(text).expect("parse");
        assert_eq!(parsed.name, "profile-x");
        assert_eq!(parsed.lra_lu, 4.0, "integer scalars parse as numbers");
        assert_eq!(parsed.tolerances, TargetsTolerances::default());
    }

    #[test]
    fn toml_parser_rejects_unknown_keys_tables_and_bad_numbers() {
        let ok = |extra: &str| {
            format!(
                "schema_version = 1\nname = \"x\"\nintegrated_lufs = -8.5\n\
                 short_term_lufs = -7.0\nlra_lu = 4.0\nceiling_dbtp = -1.0\n\
                 tilt_hz = 700.0\ntilt_cdb = 0.0\n{extra}"
            )
        };
        assert!(MasteringTargets::from_toml(&ok("surprise = 1")).is_err(), "unknown key");
        assert!(
            MasteringTargets::from_toml(&ok("[ghost]\nintegrated_lufs = 0.5")).is_err(),
            "unknown table"
        );
        assert!(
            MasteringTargets::from_toml(&ok("[tolerances]\nsurprise = 1.0")).is_err(),
            "unknown tolerances key"
        );
        assert!(MasteringTargets::from_toml(&ok("tilt_hz = NaN")).is_err(), "non-finite");
        assert!(MasteringTargets::from_toml(&ok("tilt_hz = inf")).is_err(), "non-finite");
        assert!(
            MasteringTargets::from_toml("schema_version = 1\nname = \"x\"").is_err(),
            "missing keys"
        );
        let bad_version = ok("").replace("schema_version = 1", "schema_version = 99");
        let err = MasteringTargets::from_toml(&bad_version).expect_err("version must be rejected");
        assert!(matches!(err, TargetsError::Version { found: 99 }));
    }

    #[test]
    fn wrong_schema_version_is_rejected() {
        let bad = r#"{ "schema_version": 99, "name": "x", "integrated_lufs": -9.0,
            "short_term_lufs": -7.0, "lra_lu": 4.0, "ceiling_dbtp": -1.0,
            "tilt_hz": 700.0, "tilt_cdb": 0.0 }"#;
        let err = MasteringTargets::from_json(bad).expect_err("version must be rejected");
        assert!(matches!(err, TargetsError::Version { found: 99 }));
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let bad = r#"{ "schema_version": 1, "name": "x", "integrated_lufs": -9.0,
            "short_term_lufs": -7.0, "lra_lu": 4.0, "ceiling_dbtp": -1.0,
            "tilt_hz": 700.0, "tilt_cdb": 0.0, "surprise": 1 }"#;
        assert!(serde_json::from_str::<MasteringTargets>(bad).is_err());
    }
}
