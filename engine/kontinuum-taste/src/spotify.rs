//! The Spotify connector (issue #21): Auth Code + PKCE, token refresh,
//! paginated library pulls with rate-limit backoff, incremental refresh
//! via cursors, and a full purge on disconnect.
//!
//! Platform reality (PLAN §4 / #4): audio-features and audio-analysis are
//! gone for new apps, so this connector is **metadata-only by design** —
//! profiles, tracks, artists, albums, genres. Audio-derived DNA comes from
//! user files through [`crate::audio`], not from Spotify.
//!
//! Testability contract: every network boundary goes through the injected
//! [`HttpTransport`], time through `now_ms`/`sleep_ms` hooks — the mock-
//! server tests run the *entire* flow (PKCE exchange → paginated sync →
//! DNA) with zero live credentials.
//!
//! Secret handling: access/refresh tokens live only in the injected
//! [`SecretStore`] (Keychain on iOS, per the KeychainStore conventions in
//! [`crate::secrets`]). They are never written to the store, exports, or
//! logs.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::error::TasteError;
use crate::http::{HttpRequest, HttpTransport};
use crate::model::profile_from_events;
use crate::pkce;
use crate::secrets::{SecretStore, MemorySecretStore};
use crate::source::{SyncMode, SyncReport, TasteSource};
use crate::store::{EventContext, LibraryEvent, TasteStore};

pub const SOURCE_ID: &str = "spotify";

const ACCESS_ACCOUNT: &str = "spotify/access-token";
const REFRESH_ACCOUNT: &str = "spotify/refresh-token";
const EXPIRY_ACCOUNT: &str = "spotify/token-expiry-ms";

/// Token freshness margin: refresh this long before actual expiry so an
/// in-flight sync never rides an expiring token.
const EXPIRY_MARGIN_MS: i64 = 60_000;

/// Per-sync request counters, folded into the [`SyncReport`].
#[derive(Clone, Copy, Debug, Default)]
struct RequestCounts {
    requests: usize,
    retries: usize,
}

#[derive(Clone, Debug)]
pub struct SpotifyConfig {
    pub client_id: String,
    pub redirect_uri: String,
    /// Override for tests: the mock server base, e.g. `http://127.0.0.1:PORT`.
    pub accounts_base: Option<String>,
    pub api_base: Option<String>,
    pub scopes: String,
}

impl SpotifyConfig {
    pub fn new(client_id: impl Into<String>, redirect_uri: impl Into<String>) -> Self {
        SpotifyConfig {
            client_id: client_id.into(),
            redirect_uri: redirect_uri.into(),
            accounts_base: None,
            api_base: None,
            scopes: "user-library-read user-top-read user-read-recently-played playlist-read-private".into(),
        }
    }

    fn accounts_base(&self) -> &str {
        self.accounts_base.as_deref().unwrap_or("https://accounts.spotify.com")
    }

    fn api_base(&self) -> &str {
        self.api_base.as_deref().unwrap_or("https://api.spotify.com/v1")
    }
}

/// Retries: `Retry-After` on 429 (Spotify's contract), exponential on
/// 5xx. Delays go through the injectable sleep hook.
#[derive(Clone, Copy, Debug)]
pub struct BackoffPolicy {
    pub max_retries: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        BackoffPolicy { max_retries: 3, base_delay_ms: 500, max_delay_ms: 8_000 }
    }
}

type NowFn = Box<dyn Fn() -> i64 + Send + Sync>;
type SleepFn = Box<dyn Fn(Duration) + Send + Sync>;

/// The Spotify [`TasteSource`].
pub struct SpotifySource {
    config: SpotifyConfig,
    transport: Arc<dyn HttpTransport>,
    secrets: Arc<dyn SecretStore>,
    backoff: BackoffPolicy,
    now_ms: NowFn,
    sleep_ms: SleepFn,
}

impl SpotifySource {
    pub fn new(
        config: SpotifyConfig,
        transport: Arc<dyn HttpTransport>,
        secrets: Arc<dyn SecretStore>,
    ) -> Self {
        SpotifySource {
            config,
            transport,
            secrets,
            backoff: BackoffPolicy::default(),
            now_ms: Box::new(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as i64),
            sleep_ms: Box::new(std::thread::sleep),
        }
    }

    /// Test hooks: fixed clock and no-op sleep (delays are *recorded* via
    /// the mock server's request log instead of waited out).
    pub fn with_test_clock(mut self, now_ms: NowFn, sleep_ms: SleepFn) -> Self {
        self.now_ms = now_ms;
        self.sleep_ms = sleep_ms;
        self
    }

    pub fn with_backoff(mut self, backoff: BackoffPolicy) -> Self {
        self.backoff = backoff;
        self
    }

    /// Step 1 of Auth Code + PKCE: the authorize URL for
    /// `ASWebAuthenticationSession`. `state` is the caller's CSRF nonce;
    /// the verifier stays in memory until [`Self::exchange_code`].
    pub fn authorize_url(&self, verifier: &str, state: &str) -> String {
        let challenge = pkce::challenge_s256(verifier);
        format!(
            "{}/authorize?response_type=code&client_id={}&scope={}&redirect_uri={}&state={}&code_challenge_method=S256&code_challenge={}",
            self.config.accounts_base(),
            urlencode(&self.config.client_id),
            urlencode(&self.config.scopes),
            urlencode(&self.config.redirect_uri),
            urlencode(state),
            challenge,
        )
    }

    /// Step 2: exchange the authorization code for tokens. Stores them in
    /// the secret store; nothing else sees them.
    pub fn exchange_code(&self, code: &str, verifier: &str) -> Result<(), TasteError> {
        let form = format!(
            "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={}",
            urlencode(code),
            urlencode(&self.config.redirect_uri),
            urlencode(&self.config.client_id),
            urlencode(verifier),
        );
        let body = self.token_request(&form)?;
        self.store_token_response(&body)
    }

    /// Drops stored tokens without touching the library data (used for
    /// auth errors; the full wipe is [`TasteSource::disconnect`]).
    pub fn forget_tokens(&self) {
        self.secrets.delete(ACCESS_ACCOUNT);
        self.secrets.delete(REFRESH_ACCOUNT);
        self.secrets.delete(EXPIRY_ACCOUNT);
    }

    fn store_token_response(&self, body: &Value) -> Result<(), TasteError> {
        let access = body["access_token"]
            .as_str()
            .ok_or_else(|| TasteError::BadResponse { provider: SOURCE_ID, reason: "no access_token".into() })?;
        self.secrets.set(ACCESS_ACCOUNT, access);
        if let Some(refresh) = body["refresh_token"].as_str() {
            self.secrets.set(REFRESH_ACCOUNT, refresh);
        }
        let expires_in = body["expires_in"].as_i64().unwrap_or(3600);
        self.secrets.set(EXPIRY_ACCOUNT, &((self.now_ms)() + expires_in * 1000).to_string());
        Ok(())
    }

    fn token_request(&self, form: &str) -> Result<Value, TasteError> {
        let url = format!("{}/api/token", self.config.accounts_base());
        let req = HttpRequest::post_form(&url, form);
        let resp = self.transport.send(&req)?;
        let v: Value = serde_json::from_str(&resp.body).map_err(|e| TasteError::BadResponse {
            provider: SOURCE_ID,
            reason: format!("token endpoint returned non-JSON: {e}"),
        })?;
        if resp.status != 200 {
            let reason = v["error_description"].as_str().unwrap_or(&resp.body).to_string();
            return Err(TasteError::HttpStatus { status: resp.status, url, body: reason });
        }
        Ok(v)
    }

    /// True when the stored token is missing or within the refresh
    /// margin; refreshes it via the stored refresh token.
    fn ensure_fresh_token(&self) -> Result<(), TasteError> {
        if self.secrets.get(ACCESS_ACCOUNT).is_none() {
            return Err(TasteError::NotAuthenticated {
                provider: SOURCE_ID,
                reason: "no access token — run the authorize flow first".into(),
            });
        }
        let expiry: i64 = self.secrets.get(EXPIRY_ACCOUNT).and_then(|v| v.parse().ok()).unwrap_or(0);
        if (self.now_ms)() < expiry - EXPIRY_MARGIN_MS {
            return Ok(());
        }
        let Some(refresh) = self.secrets.get(REFRESH_ACCOUNT) else {
            return Err(TasteError::NotAuthenticated { provider: SOURCE_ID, reason: "no refresh token".into() });
        };
        let form = format!(
            "grant_type=refresh_token&refresh_token={}&client_id={}",
            urlencode(&refresh),
            urlencode(&self.config.client_id),
        );
        let body = self.token_request(&form)?;
        self.store_token_response(&body)?;
        Ok(())
    }

    /// One GET against the API with auth, 401-refresh-retry and 429/5xx
    /// backoff. Returns (body, requests_made, retries).
    fn api_get(&self, path: &str, query: &str) -> Result<(Value, usize, usize), TasteError> {
        let url = format!("{}{path}?{query}", self.config.api_base());
        let mut attempts = 0u32;
        let mut requests = 0usize;
        let mut retries = 0usize;
        loop {
            self.ensure_fresh_token()?;
            let token = self
                .secrets
                .get(ACCESS_ACCOUNT)
                .ok_or_else(|| TasteError::NotAuthenticated { provider: SOURCE_ID, reason: "token vanished".into() })?;
            let mut req = HttpRequest::bearer(&url, &token);
            req.headers.push(("Accept".into(), "application/json".into()));
            requests += 1;
            let resp = self.transport.send(&req)?;
            match resp.status {
                200 => {
                    let v: Value = serde_json::from_str(&resp.body).map_err(|e| TasteError::BadResponse {
                        provider: SOURCE_ID,
                        reason: format!("{path} returned non-JSON: {e}"),
                    })?;
                    return Ok((v, requests, retries));
                }
                401 if attempts < 1 => {
                    // Token revoked mid-flight: force a refresh once.
                    attempts += 1;
                    retries += 1;
                    self.secrets.set(EXPIRY_ACCOUNT, "0");
                    continue;
                }
                429 => {
                    let retry_after = resp.header("retry-after").and_then(|v| v.trim().parse::<u64>().ok());
                    if attempts >= self.backoff.max_retries {
                        return Err(TasteError::RateLimited {
                            provider: SOURCE_ID,
                            retries: attempts,
                            retry_after: retry_after.map(Duration::from_secs),
                        });
                    }
                    attempts += 1;
                    retries += 1;
                    let delay = retry_after.unwrap_or(self.backoff.base_delay_ms / 1000).max(1);
                    (self.sleep_ms)(Duration::from_secs(delay));
                }
                s if s >= 500 => {
                    if attempts >= self.backoff.max_retries {
                        return Err(TasteError::HttpStatus { status: s, url, body: resp.body });
                    }
                    attempts += 1;
                    retries += 1;
                    let exp = self.backoff.base_delay_ms << (attempts - 1).min(16);
                    (self.sleep_ms)(Duration::from_millis(exp.min(self.backoff.max_delay_ms)));
                }
                s => return Err(TasteError::HttpStatus { status: s, url, body: resp.body }),
            }
        }
    }

    /// Walks Spotify's `next`-linked pagination to the end. `on_page`
    /// receives each page body (and returns how many events it recorded);
    /// request counts fold into `counts` and the caller's report.
    fn paginate(
        &self,
        path: &str,
        query: &str,
        counts: &mut RequestCounts,
        mut on_page: impl FnMut(&Value) -> Result<usize, TasteError>,
    ) -> Result<(), TasteError> {
        let mut page_path = path.to_string();
        let mut page_query = query.to_string();
        loop {
            let (page, requests, retries) = self.api_get(&page_path, &page_query)?;
            counts.requests += requests;
            counts.retries += retries;
            on_page(&page)?;
            match page["next"].as_str() {
                Some(next) => {
                    let (p, q) = split_url(next, self.config.api_base())?;
                    page_path = p;
                    page_query = q;
                }
                None => break,
            }
        }
        Ok(())
    }

    fn sync_playlists(
        &self,
        store: &mut TasteStore,
        counts: &mut RequestCounts,
        report: &mut SyncReport,
    ) -> Result<(), TasteError> {
        let mut ids = Vec::new();
        self.paginate("/me/playlists", "limit=50", counts, |page| {
            for pl in page["items"].as_array().into_iter().flatten() {
                if let Some(id) = pl["id"].as_str() {
                    ids.push(id.to_string());
                }
            }
            Ok(0)
        })?;
        report.playlists = ids.len();
        for id in &ids {
            let path = format!("/playlists/{id}/tracks");
            self.paginate(&path, "limit=100", counts, |page| {
                let events = playlist_events(page);
                let n = store.record_events(SOURCE_ID, &events)?;
                report.events += n;
                Ok(n)
            })?;
        }
        Ok(())
    }

    fn sync_saved(
        &self,
        store: &mut TasteStore,
        counts: &mut RequestCounts,
        report: &mut SyncReport,
    ) -> Result<(), TasteError> {
        self.paginate("/me/tracks", "limit=50", counts, |page| {
            let events = saved_events(page);
            let n = store.record_events(SOURCE_ID, &events)?;
            report.events += n;
            Ok(n)
        })
    }

    fn sync_top(
        &self,
        store: &mut TasteStore,
        now_ms: i64,
        counts: &mut RequestCounts,
        report: &mut SyncReport,
    ) -> Result<(), TasteError> {
        for range in ["long_term", "medium_term", "short_term"] {
            self.paginate("/me/top/artists", &format!("limit=50&time_range={range}"), counts, |page| {
                let mut n = 0;
                for item in page["items"].as_array().into_iter().flatten() {
                    let artist = item["name"].as_str().unwrap_or_default().to_string();
                    let genres: Vec<String> = item["genres"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|g| g.as_str())
                        .map(str::to_string)
                        .collect();
                    if artist.is_empty() && genres.is_empty() {
                        continue;
                    }
                    n += store.record_events(
                        SOURCE_ID,
                        &[LibraryEvent {
                            context: EventContext::TopArtists,
                            artist,
                            track: String::new(),
                            album: None,
                            label: None,
                            release_year: None,
                            genres,
                            bpm: None,
                            occurred_ms: now_ms,
                        }],
                    )?;
                }
                report.events += n;
                Ok(n)
            })?;
            self.paginate("/me/top/tracks", &format!("limit=50&time_range={range}"), counts, |page| {
                let events = top_track_events(page, now_ms);
                let n = store.record_events(SOURCE_ID, &events)?;
                report.events += n;
                Ok(n)
            })?;
        }
        Ok(())
    }

    /// Cursor-paginated recently-played. Incremental mode passes the
    /// stored `after` timestamp so the server returns only newer plays.
    fn sync_recently_played(
        &self,
        store: &mut TasteStore,
        mode: SyncMode,
        counts: &mut RequestCounts,
        report: &mut SyncReport,
    ) -> Result<(), TasteError> {
        let after = if mode == SyncMode::Incremental {
            store.cursor(SOURCE_ID, "recently_played_after")?.and_then(|v| v.parse::<i64>().ok())
        } else {
            None
        };
        let query = match after {
            Some(ts) => format!("limit=50&after={ts}"),
            None => "limit=50".to_string(),
        };
        let mut newest_played_at: Option<i64> = after;
        self.paginate("/me/player/recently-played", &query, counts, |page| {
            let mut n = 0;
            for item in page["items"].as_array().into_iter().flatten() {
                let played_at = item["played_at"].as_str().and_then(iso8601_ms).unwrap_or(0);
                newest_played_at = Some(newest_played_at.map_or(played_at, |m: i64| m.max(played_at)));
                if let Some(track) = item.get("track") {
                    if let Some(e) = track_event(track, EventContext::RecentlyPlayed, played_at) {
                        n += store.record_events(SOURCE_ID, &[e])?;
                    }
                }
            }
            report.events += n;
            Ok(n)
        })?;
        if let Some(ts) = newest_played_at {
            store.set_cursor(SOURCE_ID, "recently_played_after", &ts.to_string())?;
        }
        Ok(())
    }

    fn merge_profile(&self, store: &mut TasteStore) -> Result<(), TasteError> {
        let events = store.events_for(SOURCE_ID)?;
        let metadata = profile_from_events(&events, (self.now_ms)());
        let mut merged = store.profile()?.unwrap_or_default();
        merge_metadata_into(&mut merged, metadata);
        store.save_profile(&merged, (self.now_ms)())?;
        Ok(())
    }
}

/// Merges a source's metadata profile into the learned DNA: v1 scalar
/// fields only move when the source actually measured something; list
/// fields are replaced per source (single-source today, unioned when a
/// second connector lands).
fn merge_metadata_into(
    merged: &mut kontinuum_compose::taste::TasteProfile,
    metadata: kontinuum_compose::taste::TasteProfile,
) {
    if metadata.bpm.is_some() {
        merged.bpm = metadata.bpm;
        merged.tempo_dispersion = metadata.tempo_dispersion;
    }
    if !metadata.genre_mix.is_empty() {
        merged.genre_mix = metadata.genre_mix.clone();
        merged.genres = metadata.genres.clone();
        merged.energy = metadata.energy;
        merged.darkness = metadata.darkness;
        merged.density = metadata.density;
        merged.variation = metadata.variation;
    }
    if !metadata.era_weights.is_empty() {
        merged.era_weights = metadata.era_weights;
    }
    if !metadata.scene_weights.is_empty() {
        merged.scene_weights = metadata.scene_weights;
    }
    if metadata.adventurousness.is_some() {
        merged.adventurousness = metadata.adventurousness;
    }
    merged.dna_version = kontinuum_compose::taste::DNA_VERSION;
}

fn iso8601_ms(s: &str) -> Option<i64> {
    // Spotify timestamps are RFC 3339 UTC ("2024-01-02T03:04:05.678Z").
    // Hand-rolled: year-month-day + optional fraction, no timezone juggling
    // (the API always returns Z).
    if !s.ends_with('Z') {
        return None;
    }
    let (date, rest) = s.split_once('T')?;
    let rest = rest.trim_end_matches('Z');
    let (time, frac) = match rest.split_once('.') {
        Some((t, f)) => (t, f),
        None => (rest, ""),
    };
    let mut dp = date.split('-');
    let year: i64 = dp.next()?.parse().ok()?;
    let month: i64 = dp.next()?.parse().ok()?;
    let day: i64 = dp.next()?.parse().ok()?;
    let mut tp = time.split(':');
    let hour: i64 = tp.next()?.parse().ok()?;
    let min: i64 = tp.next()?.parse().ok()?;
    let sec: i64 = tp.next().unwrap_or("0").parse().ok()?;
    // Fraction: pad/truncate to 3 digits (millis).
    let mut ms: i64 = 0;
    for (i, c) in frac.chars().take(3).enumerate() {
        let digit = match c.to_digit(10) {
            Some(d) => d as i64,
            None => return None,
        };
        ms += digit * [100, 10, 1][i];
    }
    // Days since epoch (civil-from-days inverse), then ms.
    let days = days_from_civil(year, month, day);
    Some((((days * 24 + hour) * 60 + min) * 60 + sec) * 1000 + ms)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    // Howard Hinnant's civil_from_days inverse — standard, no deps.
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn split_url<'a>(url: &str, api_base: &str) -> Result<(String, String), TasteError> {
    let base = api_base.trim_end_matches('/');
    let rest = url
        .strip_prefix(base)
        .ok_or_else(|| TasteError::BadResponse { provider: SOURCE_ID, reason: format!("next URL off-base: {url}") })?;
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    Ok((path.to_string(), query.to_string()))
}

pub(crate) fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn playlist_events(page: &Value) -> Vec<LibraryEvent> {
    page["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("track"))
                .filter_map(|track| track_event(track, EventContext::Playlist, 0))
                .collect()
        })
        .unwrap_or_default()
}

fn saved_events(page: &Value) -> Vec<LibraryEvent> {
    page["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let added_at = item["added_at"].as_str().and_then(iso8601_ms);
                    item.get("track")
                        .and_then(|track| track_event(track, EventContext::Saved, added_at.unwrap_or(0)))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn top_track_events(page: &Value, now: i64) -> Vec<LibraryEvent> {
    page["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|track| track_event(track, EventContext::TopTracks, now))
                .collect()
        })
        .unwrap_or_default()
}

/// One Spotify track object → a library event (metadata only).
fn track_event(track: &Value, context: EventContext, occurred_ms: i64) -> Option<LibraryEvent> {
    let track_name = track["name"].as_str()?.to_string();
    let artist = track["artists"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|a| a["name"].as_str())
        .unwrap_or_default()
        .to_string();
    let album = track["album"]["name"].as_str().map(str::to_string);
    let label = track["album"]["label"].as_str().map(str::to_string);
    // release_date is "YYYY", "YYYY-MM" or "YYYY-MM-DD" — the year is all
    // the era model needs.
    let release_year = track["album"]["release_date"]
        .as_str()
        .and_then(|d| d.get(..4))
        .and_then(|y| y.parse().ok());
    Some(LibraryEvent {
        context,
        artist,
        track: track_name,
        album,
        label,
        release_year,
        genres: vec![],
        bpm: None,
        occurred_ms: occurred_ms,
    })
}

impl TasteSource for SpotifySource {
    fn id(&self) -> &'static str {
        SOURCE_ID
    }

    fn sync(&mut self, store: &mut TasteStore, mode: SyncMode) -> Result<SyncReport, TasteError> {
        let consent = store.consent_for(SOURCE_ID)?;
        if !consent.metadata_sync {
            return Err(TasteError::ConsentRequired { provider: SOURCE_ID });
        }
        let now = (self.now_ms)();
        let mut report = SyncReport::for_source(SOURCE_ID, mode, now);
        let mut counts = RequestCounts::default();
        if mode == SyncMode::Full {
            self.sync_playlists(store, &mut counts, &mut report)?;
            self.sync_saved(store, &mut counts, &mut report)?;
        }
        self.sync_top(store, now, &mut counts, &mut report)?;
        self.sync_recently_played(store, mode, &mut counts, &mut report)?;
        self.merge_profile(store)?;
        report.finished_ms = (self.now_ms)();
        report.requests = counts.requests;
        report.retried_requests = counts.retries;
        store.set_cursor(SOURCE_ID, "last_full_sync_ms", &report.finished_ms.to_string())?;
        Ok(report)
    }

    fn disconnect(&mut self, store: &mut TasteStore) -> Result<(), TasteError> {
        store.purge_source(SOURCE_ID)?;
        self.forget_tokens();
        Ok(())
    }
}

/// Convenience for tests and non-iOS hosts: an in-memory secret store.
pub fn memory_secrets() -> Arc<dyn SecretStore> {
    Arc::new(MemorySecretStore::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencoding_is_rfc3986_unreserved() {
        assert_eq!(urlencode("abcXYZ09-._~"), "abcXYZ09-._~");
        assert_eq!(urlencode("a b&c=d"), "a%20b%26c%3Dd");
        assert_eq!(urlencode("münchen"), "m%C3%BCnchen");
    }

    #[test]
    fn iso_parsing_handles_spotify_timestamps() {
        assert_eq!(iso8601_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(iso8601_ms("2024-03-15T12:30:45Z"), Some(1_710_505_845_000));
        assert_eq!(iso8601_ms("2024-03-15T12:30:45.678Z"), Some(1_710_505_845_678));
        assert_eq!(iso8601_ms("2024-03-15T12:30:45.6Z"), Some(1_710_505_845_600));
        assert_eq!(iso8601_ms("not-a-date"), None);
    }

    #[test]
    fn track_objects_map_to_metadata_only_events() {
        let track: Value = serde_json::from_str(
            r#"{"name": "Track", "artists": [{"name": "Artist"}],
                "album": {"name": "Album", "label": "Perlon", "release_date": "2001-06-01"}}"#,
        )
        .unwrap();
        let e = track_event(&track, EventContext::Saved, 42).unwrap();
        assert_eq!(e.artist, "Artist");
        assert_eq!(e.label.as_deref(), Some("Perlon"));
        assert_eq!(e.release_year, Some(2001));
        assert_eq!(e.occurred_ms, 42);
    }
}
