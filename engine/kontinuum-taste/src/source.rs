//! The `TasteSource` protocol (issue #21): pluggable library sources.
//!
//! One trait, implemented per streaming service; the Spotify connector is
//! the reference implementation. The iOS side binds through the bridge —
//! `ASWebAuthenticationSession` performs the authorize redirect and hands
//! the callback URL to [`crate::spotify::SpotifySource::exchange_code`].

use crate::error::TasteError;
use crate::store::TasteStore;

/// How much to pull. `Full` re-walks every endpoint (daily cadence);
/// `Incremental` rides the cursors (on-open cadence) — recently-played
/// only fetches plays newer than the stored cursor, and unchanged
/// endpoints are skipped entirely.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SyncMode {
    #[default]
    Full,
    Incremental,
}

/// What one sync did (surfaced in the privacy screen's sync log).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct SyncReport {
    pub source: String,
    pub mode: SyncMode,
    pub events: usize,
    pub requests: usize,
    pub retried_requests: usize,
    pub playlists: usize,
    pub started_ms: i64,
    pub finished_ms: i64,
}

impl SyncReport {
    pub(crate) fn for_source(source: &'static str, mode: SyncMode, started_ms: i64) -> Self {
        SyncReport { source: source.to_string(), mode, started_ms, ..Default::default() }
    }
}

/// A pluggable taste source. Implementations hold their own transport +
/// secret store handles; the store is shared on-device state.
pub trait TasteSource {
    fn id(&self) -> &'static str;

    /// Pulls the source's library into `store` and merges its metadata
    /// into the learned profile. Consent (`metadata_sync`) is checked
    /// here — a source without consent is [`TasteError::ConsentRequired`].
    fn sync(&mut self, store: &mut TasteStore, mode: SyncMode) -> Result<SyncReport, TasteError>;

    /// Disconnect = full local purge: everything this source put in the
    /// store **and** its stored tokens.
    fn disconnect(&mut self, store: &mut TasteStore) -> Result<(), TasteError>;
}
