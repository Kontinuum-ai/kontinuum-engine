//! Implicit preference signals (#24). Every behavioral event the engine can
//! observe — skips, listen-throughs, volume nudges, explicit feedback — is a
//! [`Signal`] tagged with the musical [`StateFingerprint`](crate::StateFingerprint)
//! that was live when it fired.
//!
//! Storage is append-only JSONL (one JSON object per line) with a rotating
//! retention helper; the issue's SQLite upgrade is a follow-up and swaps in
//! behind this module's API, not through it.
//!
//! All timestamps are unix milliseconds (`ts_ms`), matching the iOS layer.

use crate::fingerprint::StateFingerprint;
use serde::{Deserialize, Serialize};
use std::fs;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

/// Kinds of behavioral evidence. `canonical_strength` encodes the issue's
/// valence/magnitude contract: strong ± (skips, explicit feedback, bookmarks),
/// weak ± (listen-through, volume, session length), context (no valence).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalKind {
    /// User skipped ahead — strong negative (reacts late; see attribution).
    Skip,
    /// User let a whole section play through — weak positive.
    ListenThroughSection,
    /// Volume raised — weak positive, debounced vs route changes.
    VolumeUp,
    /// Volume lowered — weak negative, debounced vs route changes.
    VolumeDown,
    /// "More like this" (#33 UI) — strong positive.
    ExplicitMoreLikeThis,
    /// "Less like this" (#33 UI) — strong negative.
    ExplicitLessLikeThis,
    /// Bookmark / save — strong positive.
    Bookmark,
    /// Free-text or picklist mood statement — context only.
    StatedMood,
    /// Hour-of-day observation — context only.
    TimeOfDay,
    /// Weekday observation — context only.
    Weekday,
    /// A session ended naturally — weak positive for everything played.
    SessionLength,
}

impl SignalKind {
    /// Canonical valence/magnitude in [-1, 1]: sign is valence, magnitude is
    /// confidence. Callers may override per event via `Signal::strength`.
    pub fn canonical_strength(self) -> f32 {
        match self {
            SignalKind::Skip | SignalKind::ExplicitLessLikeThis => -1.0,
            SignalKind::ExplicitMoreLikeThis | SignalKind::Bookmark => 1.0,
            SignalKind::ListenThroughSection | SignalKind::VolumeUp | SignalKind::SessionLength => 0.3,
            SignalKind::VolumeDown => -0.3,
            SignalKind::StatedMood | SignalKind::TimeOfDay | SignalKind::Weekday => 0.0,
        }
    }

    /// Kinds whose context must carry route-change debounce metadata.
    pub fn requires_debounce(self) -> bool {
        matches!(self, SignalKind::VolumeUp | SignalKind::VolumeDown)
    }
}

/// Ignore volume signals within this window after an audio route change
/// (issue #24: environment noise, not taste — headphones plugged in, etc.).
pub const ROUTE_CHANGE_DEBOUNCE_MS: u64 = 30_000;

/// Situational metadata attached to a signal. Everything is optional: the
/// capture layer fills what it knows.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SignalContext {
    /// Stated mood text (`StatedMood`).
    pub mood: Option<String>,
    /// UTC hour 0..23 (`TimeOfDay`).
    pub hour_utc: Option<u8>,
    /// 0 = Monday .. 6 = Sunday (`Weekday`).
    pub weekday: Option<u8>,
    /// Debounce metadata: ms since the last audio route change. Volume
    /// signals inside [`ROUTE_CHANGE_DEBOUNCE_MS`] are noise.
    pub since_route_change_ms: Option<u64>,
    /// Elapsed session length in ms (`SessionLength`).
    pub session_ms: Option<u64>,
    /// Propensity of the logging policy for this event, in (0, 1]. Feeds the
    /// IPS estimator in the replay harness; `None` until the director logs
    /// its sampling probabilities.
    pub propensity: Option<f32>,
}

impl SignalContext {
    /// True when the event landed inside the route-change debounce window,
    /// i.e. it should be dropped before learning.
    pub fn is_route_debounced(&self) -> bool {
        self.since_route_change_ms.is_some_and(|ms| ms < ROUTE_CHANGE_DEBOUNCE_MS)
    }
}

/// One behavioral observation: what happened, how strongly, and over which
/// musical state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Signal {
    /// Unix timestamp in milliseconds.
    pub ts_ms: i64,
    pub kind: SignalKind,
    /// Valence/magnitude in [-1, 1]; defaults to [`SignalKind::canonical_strength`].
    pub strength: f32,
    /// Compact fingerprint of the musical state at t. Log at `Granularity::Fine`
    /// so offline granularity studies can coarsen freely.
    pub state_fingerprint: StateFingerprint,
    pub context: SignalContext,
}

impl Signal {
    /// Canonical-strength signal with empty context; attach metadata with
    /// [`Signal::with_context`].
    pub fn new(ts_ms: i64, kind: SignalKind, state_fingerprint: StateFingerprint) -> Self {
        Signal {
            ts_ms,
            kind,
            strength: kind.canonical_strength(),
            state_fingerprint,
            context: SignalContext::default(),
        }
    }

    /// Builder-style context attachment.
    pub fn with_context(mut self, context: SignalContext) -> Self {
        self.context = context;
        self
    }
}

/// Persistence errors (JSONL store and SQLite store share the surface).
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("signal store io failure")]
    Io(#[from] std::io::Error),
    #[error("malformed JSONL in signal store")]
    Json(#[from] serde_json::Error),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("signal store schema v{found} is newer than this build supports")]
    Version { found: i64 },
}

const MS_PER_DAY: i64 = 86_400_000;

/// Append-only JSONL signal store, one [`Signal`] per line.
#[derive(Clone, Debug)]
pub struct SignalStore {
    path: PathBuf,
}

impl SignalStore {
    pub fn open(path: impl Into<PathBuf>) -> Self {
        SignalStore { path: path.into() }
    }

    /// Append one signal and flush to disk (crash-safe enough for on-device
    /// capture; SQLite is the follow-up store).
    pub fn append(&self, signal: &Signal) -> Result<(), StoreError> {
        let mut f = OpenOptions::new().create(true).append(true).open(&self.path)?;
        writeln!(f, "{}", serde_json::to_string(signal)?)?;
        Ok(())
    }

    /// Load every stored signal in append order. A missing file is an empty
    /// log, not an error.
    pub fn load(&self) -> Result<Vec<Signal>, StoreError> {
        let data = match fs::read_to_string(&self.path) {
            Ok(d) => d,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        parse_jsonl(&data)
    }

    /// Rotating retention: keep only signals newer than `keep_days`, rewriting
    /// the file atomically via a temp file + rename. Returns the kept count.
    pub fn retain_days(&self, now_ms: i64, keep_days: u32) -> Result<usize, StoreError> {
        let cutoff = now_ms.saturating_sub(keep_days as i64 * MS_PER_DAY);
        let all = self.load()?;
        let kept: Vec<&Signal> = all.iter().filter(|s| s.ts_ms >= cutoff).collect();
        if kept.len() == all.len() {
            return Ok(kept.len());
        }
        let tmp = self.path.with_extension("jsonl.tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            for s in &kept {
                writeln!(f, "{}", serde_json::to_string(s)?)?;
            }
        }
        fs::rename(&tmp, &self.path)?;
        Ok(kept.len())
    }
}

/// Parse JSONL text, skipping blank lines.
pub(crate) fn parse_jsonl<T: serde::de::DeserializeOwned>(data: &str) -> Result<Vec<T>, StoreError> {
    let mut out = Vec::new();
    for line in data.lines() {
        if line.trim().is_empty() {
            continue;
        }
        out.push(serde_json::from_str(line)?);
    }
    Ok(out)
}

/// Convenience for callers probing a path directly (replay harness reuses it).
pub(crate) fn read_jsonl_file<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Vec<T>, StoreError> {
    match fs::read_to_string(path) {
        Ok(data) => parse_jsonl(&data),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fingerprint::{Granularity, MusicalState, SectionKind};

    fn fine_state(density: f32, palette: u32) -> StateFingerprint {
        MusicalState {
            section_kind: SectionKind::Peak,
            energy: 0.8,
            density,
            brightness: 0.5,
            bpm: 124.0,
            palette_id: palette,
            groove_template: 3,
            bass_archetype: 1,
            dominant_sample_classes: [9, 4, 0, 0],
        }
        .fingerprint(Granularity::Fine)
    }

    fn sample_signal(ts_ms: i64, kind: SignalKind) -> Signal {
        Signal::new(ts_ms, kind, fine_state(0.9, 7))
            .with_context(SignalContext {
                mood: Some("late night".into()),
                hour_utc: Some(23),
                weekday: Some(4),
                since_route_change_ms: Some(120_000),
                session_ms: Some(3_600_000),
                propensity: None,
            })
    }

    #[test]
    fn signal_jsonl_roundtrip() {
        let dir = std::env::temp_dir().join(format!("kpref-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let store = SignalStore::open(dir.join("signals.jsonl"));
        store.append(&sample_signal(1_000, SignalKind::Skip)).unwrap();
        store
            .append(&sample_signal(2_000, SignalKind::ListenThroughSection))
            .unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0], sample_signal(1_000, SignalKind::Skip));
        assert_eq!(loaded[1].kind, SignalKind::ListenThroughSection);
        assert_eq!(loaded[1].context.hour_utc, Some(23));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_store_loads_empty() {
        let store = SignalStore::open("/nonexistent/kpref/none.jsonl");
        assert!(store.load().unwrap().is_empty());
    }

    #[test]
    fn retention_rotation_drops_old_signals() {
        let dir = std::env::temp_dir().join(format!("kpref-rot-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let store = SignalStore::open(dir.join("signals.jsonl"));
        let now: i64 = 1_800_000_000_000;
        store.append(&sample_signal(now - 10 * MS_PER_DAY, SignalKind::Bookmark)).unwrap();
        store.append(&sample_signal(now - 1 * MS_PER_DAY, SignalKind::Bookmark)).unwrap();
        store.append(&sample_signal(now, SignalKind::Skip)).unwrap();
        let kept = store.retain_days(now, 7).unwrap();
        assert_eq!(kept, 2);
        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(loaded.iter().all(|s| s.ts_ms >= now - 7 * MS_PER_DAY));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn volume_kinds_require_debounce_metadata() {
        let ctx = SignalContext { since_route_change_ms: Some(5_000), ..Default::default() };
        assert!(ctx.is_route_debounced());
        let quiet = SignalContext { since_route_change_ms: Some(60_000), ..Default::default() };
        assert!(!quiet.is_route_debounced());
        assert!(SignalKind::VolumeDown.requires_debounce());
        assert!(!SignalKind::Skip.requires_debounce());
    }

    #[test]
    fn canonical_strengths_match_issue_contract() {
        assert_eq!(SignalKind::Skip.canonical_strength(), -1.0);
        assert_eq!(SignalKind::ExplicitMoreLikeThis.canonical_strength(), 1.0);
        assert_eq!(SignalKind::Bookmark.canonical_strength(), 1.0);
        assert_eq!(SignalKind::VolumeDown.canonical_strength(), -0.3);
        assert_eq!(SignalKind::ListenThroughSection.canonical_strength(), 0.3);
        assert_eq!(SignalKind::StatedMood.canonical_strength(), 0.0);
        assert_eq!(SignalKind::TimeOfDay.canonical_strength(), 0.0);
    }
}
