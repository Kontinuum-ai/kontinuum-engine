//! SQLite signal store (#24): the durable on-device home for captured
//! [`Signal`]s. Mirrors `kontinuum-samples::store` conventions — schema
//! versioned with `PRAGMA user_version`, migrations as in-code batches that
//! each stamp their version, and a database written by a newer build is a
//! typed error ([`StoreError::Version`]), never a silent downgrade.
//!
//! The JSONL store in [`crate::signal`] stays: the replay harness reads
//! portable log files, this store is the live capture path. Rotation is a
//! `DELETE` on `ts_ms` rather than a file rewrite.

use crate::fingerprint::StateFingerprint;
use crate::signal::{Signal, SignalContext, SignalKind, StoreError};
use rusqlite::{Connection, params};
use std::path::Path;

/// Newest schema version this build understands.
const SCHEMA_VERSION: i64 = 1;

/// Migration batch v1: the initial `signals` table. Ends by stamping its
/// version so re-opening skips it.
const MIGRATE_V1: &str = "
CREATE TABLE signals (
    id               INTEGER PRIMARY KEY,
    ts_ms            INTEGER NOT NULL,
    kind             TEXT NOT NULL,
    strength         REAL NOT NULL,
    fingerprint_json TEXT NOT NULL,
    context_json     TEXT NOT NULL
);
CREATE INDEX signals_ts ON signals(ts_ms);
PRAGMA user_version = 1;
";

const MS_PER_DAY: i64 = 86_400_000;

/// SQLite-backed signal store. `Connection` is internally synchronized; one
/// handle per process is the intended shape (same as the sample catalog).
#[derive(Debug)]
pub struct SqliteSignalStore {
    conn: Connection,
}

fn kind_to_text(kind: SignalKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

impl SqliteSignalStore {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, StoreError> {
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(StoreError::Version { found: version });
        }
        if version < SCHEMA_VERSION {
            conn.execute_batch(MIGRATE_V1)?;
        }
        Ok(SqliteSignalStore { conn })
    }

    /// Append one signal.
    pub fn append(&self, signal: &Signal) -> Result<(), StoreError> {
        self.append_batch(std::slice::from_ref(signal))
    }

    /// Append a batch in one transaction (capture flushes whole phrases).
    pub fn append_batch(&self, signals: &[Signal]) -> Result<(), StoreError> {
        let mut stmt = self.conn.prepare(
            "INSERT INTO signals (ts_ms, kind, strength, fingerprint_json, context_json) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for s in signals {
            stmt.execute(params![
                s.ts_ms,
                kind_to_text(s.kind),
                s.strength,
                serde_json::to_string(&s.state_fingerprint)?,
                serde_json::to_string(&s.context)?,
            ])?;
        }
        Ok(())
    }

    /// Load every signal in append order (ts, then insertion order).
    pub fn load(&self) -> Result<Vec<Signal>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT ts_ms, kind, strength, fingerprint_json, context_json \
             FROM signals ORDER BY ts_ms, id",
        )?;
        let rows = stmt.query_map([], row_to_signal)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Number of stored signals.
    pub fn count(&self) -> Result<usize, StoreError> {
        Ok(self.conn.query_row("SELECT count(*) FROM signals", [], |r| r.get(0))?)
    }

    /// Rotating retention: drop signals older than `keep_days`. Returns the
    /// number of removed rows.
    pub fn retain_days(&self, now_ms: i64, keep_days: u32) -> Result<usize, StoreError> {
        let cutoff = now_ms.saturating_sub(keep_days as i64 * MS_PER_DAY);
        Ok(self
            .conn
            .execute("DELETE FROM signals WHERE ts_ms < ?1", [cutoff])?)
    }
}

/// A text column that must be module-written JSON but isn't — surfaces as a
/// typed sqlite error instead of a silent fallback (samples-store pattern).
fn column_corrupt(column: &'static str) -> rusqlite::Error {
    rusqlite::Error::InvalidColumnType(0, column.to_string(), rusqlite::types::Type::Text)
}

fn row_to_signal(row: &rusqlite::Row<'_>) -> Result<Signal, rusqlite::Error> {
    let ts_ms: i64 = row.get(0)?;
    let kind_text: String = row.get(1)?;
    let kind: SignalKind = serde_json::from_value(serde_json::Value::String(kind_text))
        .map_err(|_| column_corrupt("kind"))?;
    let strength: f32 = row.get(2)?;
    let fingerprint: StateFingerprint = serde_json::from_str(&row.get::<_, String>(3)?)
        .map_err(|_| column_corrupt("fingerprint_json"))?;
    let context: SignalContext = serde_json::from_str(&row.get::<_, String>(4)?)
        .map_err(|_| column_corrupt("context_json"))?;
    Ok(Signal { ts_ms, kind, strength, state_fingerprint: fingerprint, context })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fingerprint::{Granularity, MusicalState, SectionKind};

    fn state() -> MusicalState {
        MusicalState {
            section_kind: SectionKind::Peak,
            energy: 0.8,
            density: 0.9,
            brightness: 0.5,
            bpm: 124.0,
            palette_id: 7,
            groove_template: 3,
            bass_archetype: 1,
            dominant_sample_classes: [9, 4, 0, 0],
        }
    }

    fn signal(ts_ms: i64, kind: SignalKind) -> Signal {
        let mut s = Signal::new(ts_ms, kind, state().fingerprint(Granularity::Fine));
        s.context = SignalContext {
            mood: Some("late night".into()),
            hour_utc: Some(23),
            since_route_change_ms: Some(120_000),
            ..Default::default()
        };
        s
    }

    #[test]
    fn sqlite_roundtrip_preserves_every_field() {
        let store = SqliteSignalStore::open_in_memory().unwrap();
        store
            .append_batch(&[
                signal(1_000, SignalKind::Skip),
                signal(2_000, SignalKind::VolumeUp),
                signal(3_000, SignalKind::Bookmark),
            ])
            .unwrap();
        assert_eq!(store.count().unwrap(), 3);
        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0], signal(1_000, SignalKind::Skip));
        assert_eq!(loaded[1].context.hour_utc, Some(23));
        assert_eq!(loaded[1].context.mood.as_deref(), Some("late night"));
        assert_eq!(loaded[2].state_fingerprint, state().fingerprint(Granularity::Fine));
    }

    #[test]
    fn sqlite_retention_deletes_only_old_rows() {
        let store = SqliteSignalStore::open_in_memory().unwrap();
        let now: i64 = 1_800_000_000_000;
        store
            .append_batch(&[
                signal(now - 10 * MS_PER_DAY, SignalKind::Skip),
                signal(now - 6 * MS_PER_DAY, SignalKind::Bookmark),
                signal(now, SignalKind::ListenThroughSection),
            ])
            .unwrap();
        let removed = store.retain_days(now, 7).unwrap();
        assert_eq!(removed, 1);
        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(loaded.iter().all(|s| s.ts_ms >= now - 7 * MS_PER_DAY));
    }

    #[test]
    fn sqlite_persists_across_reopen_and_rejects_newer_schema() {
        let dir = std::env::temp_dir().join(format!("kpref-sqlite-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("signals.db");
        {
            let store = SqliteSignalStore::open(&path).unwrap();
            store.append(&signal(1_000, SignalKind::ExplicitMoreLikeThis)).unwrap();
        }
        {
            let store = SqliteSignalStore::open(&path).unwrap();
            assert_eq!(store.count().unwrap(), 1);
        }
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA user_version = 99").unwrap();
        assert!(matches!(
            SqliteSignalStore::open(&path),
            Err(StoreError::Version { found: 99 })
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
