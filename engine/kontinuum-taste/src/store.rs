//! The on-device SQLite home for taste data (#21), mirroring the
//! `kontinuum-preference` store conventions: schema versioned with
//! `PRAGMA user_version`, migrations as in-code batches, a database
//! written by a newer build is a typed error, never a silent downgrade.
//!
//! What may live here: consent flags, library **metadata** events, sync
//! cursors, abstract per-track DNA (features only), the learned profile.
//! What may never live here: audio, and tokens (tokens go to
//! [`crate::secrets::SecretStore`] only). [`crate::privacy`] tests
//! enforce both.

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::TasteError;

/// Newest schema version this build understands.
const SCHEMA_VERSION: i64 = 1;

const MIGRATE_V1: &str = "
CREATE TABLE consent (
    source         TEXT PRIMARY KEY,
    metadata_sync  INTEGER NOT NULL,
    audio_analysis INTEGER NOT NULL,
    enrichment     INTEGER NOT NULL,
    updated_ms     INTEGER NOT NULL
);
CREATE TABLE events (
    id           INTEGER PRIMARY KEY,
    source       TEXT NOT NULL,
    context      TEXT NOT NULL,
    artist       TEXT NOT NULL,
    track        TEXT NOT NULL,
    album        TEXT,
    label        TEXT,
    release_year INTEGER,
    genres_json  TEXT NOT NULL DEFAULT '[]',
    bpm          REAL,
    occurred_ms  INTEGER NOT NULL
);
CREATE INDEX events_source ON events(source);
CREATE TABLE cursors (
    source TEXT NOT NULL,
    key    TEXT NOT NULL,
    value  TEXT NOT NULL,
    PRIMARY KEY (source, key)
);
CREATE TABLE track_dna (
    source   TEXT NOT NULL,
    track_id TEXT NOT NULL,
    json     TEXT NOT NULL,
    PRIMARY KEY (source, track_id)
);
CREATE TABLE profile (
    id         INTEGER PRIMARY KEY CHECK (id = 1),
    json       TEXT NOT NULL,
    updated_ms INTEGER NOT NULL
);
PRAGMA user_version = 1;
";

/// Per-source consent flags (#21 privacy screen).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Consent {
    pub metadata_sync: bool,
    pub audio_analysis: bool,
    pub enrichment: bool,
}

impl Default for Consent {
    fn default() -> Self {
        // Opt-in by construction: nothing runs until the user flips it.
        Consent { metadata_sync: false, audio_analysis: false, enrichment: false }
    }
}

/// One library observation from a connector (metadata only, never audio).
#[derive(Clone, Debug, PartialEq)]
pub struct LibraryEvent {
    pub context: EventContext,
    pub artist: String,
    pub track: String,
    pub album: Option<String>,
    pub label: Option<String>,
    pub release_year: Option<i32>,
    pub genres: Vec<String>,
    pub bpm: Option<f64>,
    pub occurred_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventContext {
    Saved,
    Playlist,
    TopTracks,
    TopArtists,
    RecentlyPlayed,
}

impl EventContext {
    pub fn as_str(self) -> &'static str {
        match self {
            EventContext::Saved => "saved",
            EventContext::Playlist => "playlist",
            EventContext::TopTracks => "top_tracks",
            EventContext::TopArtists => "top_artists",
            EventContext::RecentlyPlayed => "recently_played",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "saved" => EventContext::Saved,
            "playlist" => EventContext::Playlist,
            "top_tracks" => EventContext::TopTracks,
            "top_artists" => EventContext::TopArtists,
            "recently_played" => EventContext::RecentlyPlayed,
            _ => return None,
        })
    }
}

/// The "what we learned about you" row (#33's inspector view reads this
/// through the bridge; this is the data API).
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct SourceStatus {
    pub source: String,
    pub consent: Consent,
    pub events: u64,
    pub track_dna_rows: u64,
    pub last_sync_ms: Option<i64>,
}

/// Everything the taste layer holds, in one transparent structure.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct WhatWeLearned {
    pub sources: Vec<SourceStatus>,
    pub learned_profile: Option<kontinuum_compose::taste::TasteProfile>,
}

/// SQLite-backed taste store. One handle per process (see the preference
/// store for the rationale).
#[derive(Debug)]
pub struct TasteStore {
    conn: Connection,
}

impl TasteStore {
    pub fn open(path: &std::path::Path) -> Result<Self, TasteError> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self, TasteError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, TasteError> {
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(TasteError::Store(format!(
                "database written by a newer build (schema {version} > {SCHEMA_VERSION})"
            )));
        }
        if version < 1 {
            conn.execute_batch(MIGRATE_V1)?;
        }
        Ok(TasteStore { conn })
    }

    pub fn set_consent(&self, source: &str, consent: Consent, now_ms: i64) -> Result<(), TasteError> {
        self.conn.execute(
            "INSERT INTO consent (source, metadata_sync, audio_analysis, enrichment, updated_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(source) DO UPDATE SET
               metadata_sync = excluded.metadata_sync,
               audio_analysis = excluded.audio_analysis,
               enrichment = excluded.enrichment,
               updated_ms = excluded.updated_ms",
            params![
                source,
                consent.metadata_sync as i64,
                consent.audio_analysis as i64,
                consent.enrichment as i64,
                now_ms
            ],
        )?;
        Ok(())
    }

    pub fn consent_for(&self, source: &str) -> Result<Consent, TasteError> {
        let row = self
            .conn
            .query_row(
                "SELECT metadata_sync, audio_analysis, enrichment FROM consent WHERE source = ?1",
                params![source],
                |r| {
                    Ok(Consent {
                        metadata_sync: r.get::<_, i64>(0)? != 0,
                        audio_analysis: r.get::<_, i64>(1)? != 0,
                        enrichment: r.get::<_, i64>(2)? != 0,
                    })
                },
            )
            .optional()?;
        Ok(row.unwrap_or_default())
    }

    pub fn record_events(&self, source: &str, events: &[LibraryEvent]) -> Result<usize, TasteError> {
        let mut stmt = self.conn.prepare(
            "INSERT INTO events (source, context, artist, track, album, label, release_year,
                                 genres_json, bpm, occurred_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?;
        let mut n = 0;
        for e in events {
            n += stmt.execute(params![
                source,
                e.context.as_str(),
                e.artist,
                e.track,
                e.album,
                e.label,
                e.release_year,
                serde_json::to_string(&e.genres).unwrap_or_else(|_| "[]".into()),
                e.bpm,
                e.occurred_ms,
            ])?;
        }
        Ok(n)
    }

    pub fn events_for(&self, source: &str) -> Result<Vec<LibraryEvent>, TasteError> {
        let mut stmt = self.conn.prepare(
            "SELECT context, artist, track, album, label, release_year, genres_json, bpm, occurred_ms
             FROM events WHERE source = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![source], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<i32>>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, Option<f64>>(7)?,
                r.get::<_, i64>(8)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (context, artist, track, album, label, release_year, genres_json, bpm, occurred_ms) =
                row?;
            out.push(LibraryEvent {
                context: EventContext::from_str(&context).unwrap_or(EventContext::RecentlyPlayed),
                artist,
                track,
                album,
                label,
                release_year,
                genres: serde_json::from_str(&genres_json).unwrap_or_default(),
                bpm,
                occurred_ms,
            });
        }
        Ok(out)
    }

    pub fn set_cursor(&self, source: &str, key: &str, value: &str) -> Result<(), TasteError> {
        self.conn.execute(
            "INSERT INTO cursors (source, key, value) VALUES (?1, ?2, ?3)
             ON CONFLICT(source, key) DO UPDATE SET value = excluded.value",
            params![source, key, value],
        )?;
        Ok(())
    }

    pub fn cursor(&self, source: &str, key: &str) -> Result<Option<String>, TasteError> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM cursors WHERE source = ?1 AND key = ?2",
                params![source, key],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Abstract per-track DNA (features only — the caller serializes
    /// [`crate::audio::TrackDna`]; no audio ever reaches this table).
    pub fn upsert_track_dna(&self, source: &str, track_id: &str, json: &str) -> Result<(), TasteError> {
        self.conn.execute(
            "INSERT INTO track_dna (source, track_id, json) VALUES (?1, ?2, ?3)
             ON CONFLICT(source, track_id) DO UPDATE SET json = excluded.json",
            params![source, track_id, json],
        )?;
        Ok(())
    }

    pub fn track_dna_jsons(&self, source: &str) -> Result<Vec<(String, String)>, TasteError> {
        let mut stmt = self
            .conn
            .prepare("SELECT track_id, json FROM track_dna WHERE source = ?1 ORDER BY track_id")?;
        let rows = stmt.query_map(params![source], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn save_profile(
        &self,
        profile: &kontinuum_compose::taste::TasteProfile,
        now_ms: i64,
    ) -> Result<(), TasteError> {
        self.conn.execute(
            "INSERT INTO profile (id, json, updated_ms) VALUES (1, ?1, ?2)
             ON CONFLICT(id) DO UPDATE SET json = excluded.json, updated_ms = excluded.updated_ms",
            params![serde_json::to_string(profile)?, now_ms],
        )?;
        Ok(())
    }

    pub fn profile(&self) -> Result<Option<kontinuum_compose::taste::TasteProfile>, TasteError> {
        let json: Option<String> = self
            .conn
            .query_row("SELECT json FROM profile WHERE id = 1", [], |r| r.get(0))
            .optional()?;
        Ok(json.map(|j| serde_json::from_str(&j)).transpose()?)
    }

    /// Full local purge of one source (issue #21: disconnect = purge):
    /// events, cursors, track DNA **and the consent row itself** — a
    /// reconnect starts from a fresh consent screen. Tokens are purged by
    /// the connector through the secret store.
    pub fn purge_source(&self, source: &str) -> Result<(), TasteError> {
        for table in ["events", "cursors", "track_dna", "consent"] {
            self.conn
                .execute(&format!("DELETE FROM {table} WHERE source = ?1"), params![source])?;
        }
        Ok(())
    }

    /// Deletes the learned profile (the "delete profile" privacy action).
    pub fn delete_profile(&self) -> Result<(), TasteError> {
        self.conn.execute("DELETE FROM profile", [])?;
        Ok(())
    }

    /// The transparency surface: everything the layer knows, per source.
    pub fn what_we_learned(&self) -> Result<WhatWeLearned, TasteError> {
        let mut sources = Vec::new();
        let mut stmt = self.conn.prepare("SELECT source FROM consent UNION SELECT DISTINCT source FROM events")?;
        let names = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<String>, _>>()?;
        for name in &names {
            let name: &str = name;
            let events: u64 = self.conn.query_row(
                "SELECT COUNT(*) FROM events WHERE source = ?1",
                params![name],
                |r| r.get(0),
            )?;
            let dna_rows: u64 = self.conn.query_row(
                "SELECT COUNT(*) FROM track_dna WHERE source = ?1",
                params![name],
                |r| r.get(0),
            )?;
            let last_sync_ms = self
                .cursor(&name, "last_full_sync_ms")?
                .and_then(|v| v.parse().ok());
            sources.push(SourceStatus {
                source: name.to_string(),
                consent: self.consent_for(&name)?,
                events,
                track_dna_rows: dna_rows,
                last_sync_ms,
            });
        }
        Ok(WhatWeLearned { sources, learned_profile: self.profile()? })
    }

    /// The "export my profile" privacy action: the learned DNA only —
    /// never tokens (they do not live here), never raw library rows.
    pub fn export_profile_json(&self) -> Result<String, TasteError> {
        let learned = WhatWeLearned { sources: Vec::new(), learned_profile: self.profile()? };
        Ok(serde_json::to_string_pretty(&serde_json::json!({
            "kind": "kontinuum-taste-export",
            "dna_version": kontinuum_compose::taste::DNA_VERSION,
            "profile": learned.learned_profile,
        }))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(ctx: EventContext, artist: &str, track: &str) -> LibraryEvent {
        LibraryEvent {
            context: ctx,
            artist: artist.into(),
            track: track.into(),
            album: None,
            label: None,
            release_year: None,
            genres: vec![],
            bpm: None,
            occurred_ms: 1_000,
        }
    }

    #[test]
    fn consent_round_trips_per_source() {
        let s = TasteStore::open_in_memory().unwrap();
        assert_eq!(s.consent_for("spotify").unwrap(), Consent::default(), "unasked = everything off");
        s.set_consent(
            "spotify",
            Consent { metadata_sync: true, audio_analysis: true, enrichment: true },
            100,
        )
        .unwrap();
        assert!(s.consent_for("spotify").unwrap().metadata_sync);
        assert_eq!(s.consent_for("apple_music").unwrap(), Consent::default());
    }

    #[test]
    fn events_cursors_and_dna_round_trip() {
        let s = TasteStore::open_in_memory().unwrap();
        s.record_events("spotify", &[event(EventContext::Saved, "A", "T1")]).unwrap();
        s.record_events("spotify", &[event(EventContext::Playlist, "B", "T2")]).unwrap();
        assert_eq!(s.events_for("spotify").unwrap().len(), 2);
        assert_eq!(s.events_for("other").unwrap().len(), 0);
        assert_eq!(s.events_for("spotify").unwrap()[0].context.as_str(), "saved");

        s.set_cursor("spotify", "recently_played_after", "1234").unwrap();
        assert_eq!(s.cursor("spotify", "recently_played_after").unwrap().as_deref(), Some("1234"));
        s.set_cursor("spotify", "recently_played_after", "9999").unwrap();
        assert_eq!(s.cursor("spotify", "recently_played_after").unwrap().as_deref(), Some("9999"));

        s.upsert_track_dna("spotify", "t1", r#"{"bpm":128}"#).unwrap();
        s.upsert_track_dna("spotify", "t1", r#"{"bpm":129}"#).unwrap();
        assert_eq!(s.track_dna_jsons("spotify").unwrap().len(), 1);
    }

    #[test]
    fn purge_source_removes_everything_including_consent() {
        let s = TasteStore::open_in_memory().unwrap();
        s.set_consent("spotify", Consent { metadata_sync: true, audio_analysis: true, enrichment: true }, 1)
            .unwrap();
        s.record_events("spotify", &[event(EventContext::Saved, "A", "T")]).unwrap();
        s.set_cursor("spotify", "k", "v").unwrap();
        s.upsert_track_dna("spotify", "t", "{}").unwrap();

        s.purge_source("spotify").unwrap();
        assert_eq!(s.consent_for("spotify").unwrap(), Consent::default(), "consent row is purged too");
        assert!(s.events_for("spotify").unwrap().is_empty());
        assert_eq!(s.cursor("spotify", "k").unwrap(), None);
        assert!(s.track_dna_jsons("spotify").unwrap().is_empty());
    }

    #[test]
    fn profile_round_trip_and_export_has_no_secrets() {
        let s = TasteStore::open_in_memory().unwrap();
        assert!(s.profile().unwrap().is_none());
        s.save_profile(&kontinuum_compose::taste::TasteProfile::default(), 5).unwrap();
        assert_eq!(s.profile().unwrap(), Some(kontinuum_compose::taste::TasteProfile::default()));

        let exported = s.export_profile_json().unwrap();
        for leak in ["token", "secret", "refresh", "Bearer"] {
            assert!(!exported.to_lowercase().contains(&leak.to_lowercase()), "export leaks {leak}");
        }
        s.delete_profile().unwrap();
        assert!(s.profile().unwrap().is_none());
    }

    #[test]
    fn newer_database_is_a_typed_error() {
        let dir = std::env::temp_dir().join(format!("kt-newer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("taste.db");
        {
            let raw = Connection::open(&path).unwrap();
            raw.pragma_update(None, "user_version", SCHEMA_VERSION + 1).unwrap();
        }
        let err = TasteStore::open(&path).unwrap_err();
        assert!(matches!(err, TasteError::Store(ref m) if m.contains("newer build")));
        let _ = std::fs::remove_file(&path);
    }
}
