//! End-to-end Spotify connector tests against the local mock server:
//! PKCE exchange → paginated sync → learned DNA. No live credentials —
//! this is the acceptance harness for the full auth→sync→DNA flow.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{MockServer, RecordedRequest, Responder};
use kontinuum_taste::error::TasteError;
use kontinuum_taste::http::TcpTransport;
use kontinuum_taste::secrets::{MemorySecretStore, SecretStore};
use kontinuum_taste::source::{SyncMode, TasteSource};
use kontinuum_taste::spotify::{SpotifyConfig, SpotifySource};
use kontinuum_taste::store::{Consent, TasteStore};

mod common;

const NOW: i64 = 1_700_000_000_000;

fn track_json(name: &str, artist: &str) -> String {
    format!(
        r#"{{"name": "{name}", "artists": [{{"name": "{artist}"}}],
            "album": {{"name": "Album {artist}", "label": "Perlon", "release_date": "2001-06-01"}}}}"#
    )
}

fn source_for(server: &MockServer, secrets: Arc<MemorySecretStore>) -> SpotifySource {
    let config = SpotifyConfig {
        accounts_base: Some(server.base_url.clone()),
        api_base: Some(format!("{}/v1", server.base_url)),
        ..SpotifyConfig::new("test-client-id", "kontinuum://oauth/callback")
    };
    SpotifySource::new(config, Arc::new(TcpTransport::default()), secrets)
        .with_test_clock(Box::new(|| NOW), Box::new(|_| {}))
}

/// Standard token endpoint responder; returns a fresh access token per
/// exchange/refresh.
fn token_responder() -> (Arc<Mutex<Vec<RecordedRequest>>>, impl Fn(&RecordedRequest) -> (u16, Vec<(String, String)>, String)) {
    let token_calls = Arc::new(Mutex::new(Vec::new()));
    let calls = token_calls.clone();
    let responder = move |req: &RecordedRequest| {
        if req.path == "/api/token" {
            calls.lock().unwrap().push(req.clone());
            let n = calls.lock().unwrap().len();
            return (
                200,
                vec![],
                format!(
                    r#"{{"access_token": "at-{n}", "token_type": "Bearer",
                        "expires_in": 3600, "refresh_token": "rt-{n}"}}"#
                ),
            );
        }
        (404, vec![], "{}".into())
    };
    (token_calls, responder)
}

fn ok_json_responder() -> impl Fn(&RecordedRequest) -> (u16, Vec<(String, String)>, String) {
    |req: &RecordedRequest| {
        let body = match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/v1/me/playlists") => {
                r#"{"items": [{"id": "pl1"}], "next": null}"#.to_string()
            }
            ("GET", "/v1/playlists/pl1/tracks") => format!(
                r#"{{"items": [{{"track": {}}}], "next": null}}"#,
                track_json("Track A", "Artist A")
            ),
            ("GET", "/v1/me/tracks") => format!(
                r#"{{"items": [{{"added_at": "2024-01-01T00:00:00Z", "track": {}}}],
                    "next": null}}"#,
                track_json("Track B", "Artist B")
            ),
            ("GET", "/v1/me/top/artists") => {
                r#"{"items": [{"name": "Artist A", "genres": ["minimal techno"]}], "next": null}"#.to_string()
            }
            ("GET", "/v1/me/top/tracks") => format!(
                r#"{{"items": [{}], "next": null}}"#,
                track_json("Track C", "Artist A")
            ),
            ("GET", "/v1/me/player/recently-played") => format!(
                r#"{{"items": [{{"played_at": "2024-03-15T12:30:45Z", "track": {}}}], "next": null}}"#,
                track_json("Track D", "Artist B")
            ),
            _ => r#"{"items": [], "next": null}"#.to_string(),
        };
        (200, vec![], body)
    }
}

fn combined_responder(
    token: impl Fn(&RecordedRequest) -> (u16, Vec<(String, String)>, String) + Send + Sync + 'static,
    api: impl Fn(&RecordedRequest) -> (u16, Vec<(String, String)>, String) + Send + Sync + 'static,
) -> Responder {
    Arc::new(move |req| {
        if req.path == "/api/token" {
            return token(req);
        }
        api(req)
    })
}

fn consented_store() -> TasteStore {
    let store = TasteStore::open_in_memory().unwrap();
    store
        .set_consent(
            "spotify",
            Consent { metadata_sync: true, audio_analysis: true, enrichment: true },
            NOW,
        )
        .unwrap();
    store
}

#[test]
fn authorize_url_carries_pkce_contract() {
    let server = MockServer::start_simple(200, "{}");
    let src = source_for(&server, Arc::new(MemorySecretStore::new()));
    let url = src.authorize_url("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk", "nonce-1");
    assert!(url.starts_with(&format!("{}/authorize", server.base_url)));
    assert!(url.contains("response_type=code"));
    assert!(url.contains("code_challenge_method=S256"));
    assert!(url.contains("code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"));
    assert!(url.contains("state=nonce-1"));
    assert!(url.contains("redirect_uri=kontinuum%3A%2F%2Foauth%2Fcallback"));
}

#[test]
fn full_flow_exchange_then_sync_then_dna() {
    let (token_calls, token) = token_responder();
    let server = MockServer::start(combined_responder(token, ok_json_responder()));
    let secrets = Arc::new(MemorySecretStore::new());
    let mut src = source_for(&server, secrets.clone());
    let mut store = consented_store();

    // Auth: code exchange against the mock accounts endpoint.
    src.exchange_code("auth-code-1", "some-verifier-at-least-43-chars-long-aaaaaaaaaaaa").unwrap();
    assert_eq!(secrets.get("spotify/access-token").as_deref(), Some("at-1"));
    assert_eq!(secrets.get("spotify/refresh-token").as_deref(), Some("rt-1"));
    {
        let calls = token_calls.lock().unwrap();
        let exchange = &calls[0];
        assert!(exchange.body.contains("grant_type=authorization_code"));
        assert!(exchange.body.contains("code=auth-code-1"));
        assert!(exchange.body.contains("client_id=test-client-id"));
    }

    // Sync the whole library (mock serves one page per endpoint).
    let report = src.sync(&mut store, SyncMode::Full).unwrap();
    assert_eq!(report.events, 9, "playlist(1) + saved(1) + top-artists(3 ranges) + top-tracks(3 ranges) + recent(1)");
    assert_eq!(report.playlists, 1);
    assert!(report.requests > 6, "every endpoint hit at least once: {}", report.requests);
    let events = store.events_for("spotify").unwrap();
    assert!(events.iter().any(|e| e.track == "Track A" && e.context.as_str() == "playlist"));
    assert!(events.iter().any(|e| e.track == "Track B" && e.context.as_str() == "saved"));
    assert!(events.iter().any(|e| e.artist == "Artist A" && e.genres.contains(&"minimal techno".to_string())));
    // Auth headers were Bearer tokens.
    assert!(server.recorded().iter().any(|r| r.header("Authorization").as_deref() == Some("Bearer at-1")));

    // The learned DNA carries the metadata model's output.
    let profile = store.profile().unwrap().expect("profile learned");
    assert!(profile.genres.iter().any(|g| g == "minimal techno"));
    assert!(!profile.genre_mix.is_empty());
    assert_eq!(profile.era_weights[0].0, "2000s", "release years land in eras");
    assert_eq!(profile.scene_weights[0].0, "Perlon", "labels become scenes");
    assert_eq!(profile.dna_version, kontinuum_compose::taste::DNA_VERSION);

    // The "what we learned" surface reports the source.
    let learned = store.what_we_learned().unwrap();
    let status = learned.sources.iter().find(|s| s.source == "spotify").unwrap();
    assert_eq!(status.events, report.events as u64);
    assert!(status.consent.metadata_sync);

    // Sync requests carried the API contract: pagination params present.
    let logged = server.recorded();
    assert!(logged.iter().any(|r| r.query_param("limit").as_deref() == Some("50")));
    assert!(logged.iter().any(|r| r.path == "/v1/me/player/recently-played"));
}

#[test]
fn paginated_endpoints_walk_every_page() {
    let call_count = AtomicUsize::new(0);
    let server = MockServer::start_with_base(|base: &str| {
        let base = base.to_string();
        Arc::new(move |req: &RecordedRequest| {
            if req.path == "/api/token" {
                return (200, vec![], r#"{"access_token":"at","expires_in":3600}"#.into());
            }
            match req.path.as_str() {
                "/v1/me/playlists" => {
                    if req.query_param("offset").is_none() {
                        let next = format!("{base}/v1/me/playlists?limit=50&offset=50");
                        (200, vec![], format!(r#"{{"items": [{{"id": "pl1"}}], "next": "{next}"}}"#))
                    } else {
                        (200, vec![], r#"{"items": [{"id": "pl2"}], "next": null}"#.into())
                    }
                }
                "/v1/playlists/pl1/tracks" | "/v1/playlists/pl2/tracks" => {
                    let page = call_count.fetch_add(1, Ordering::SeqCst);
                    let next = if page % 2 == 0 {
                        let pl = if req.path.contains("pl1") { "pl1" } else { "pl2" };
                        format!(r#""{base}/v1/playlists/{pl}/tracks?limit=100&offset=100""#)
                    } else {
                        "null".into()
                    };
                    (200, vec![], format!(r#"{{"items": [{{"track": {}}}], "next": {next}}}"#, track_json("T", "A")))
                }
                _ => (200, vec![], r#"{"items": [], "next": null}"#.into()),
            }
        })
    });
    let mut src = source_for(&server, Arc::new(MemorySecretStore::new()));
    let mut store = consented_store();
    src.exchange_code("c", "verifier-verifier-verifier-verifier-verifier-1234").unwrap();
    let report = src.sync(&mut store, SyncMode::Full).unwrap();
    assert_eq!(report.playlists, 2, "both playlist pages walked");
    // 2 playlists × 2 track pages = 4 playlist-track calls at minimum.
    let track_calls = server
        .recorded()
        .iter()
        .filter(|r| r.path.starts_with("/v1/playlists/"))
        .count();
    assert_eq!(track_calls, 4, "each playlist walked to its last page");
}

#[test]
fn rate_limit_backs_off_per_retry_after_then_succeeds() {
    let hits = AtomicUsize::new(0);
    let slept: Arc<Mutex<Vec<Duration>>> = Arc::new(Mutex::new(vec![]));
    let slept2 = slept.clone();
    let server = MockServer::start(Arc::new(move |req: &RecordedRequest| {
        if req.path == "/api/token" {
            return (200, vec![], r#"{"access_token":"at","expires_in":3600}"#.into());
        }
        if req.path == "/v1/me/tracks" {
            let hit = hits.fetch_add(1, Ordering::SeqCst);
            if hit == 0 {
                return (429, vec![("Retry-After".into(), "2".into())], r#"{"error": "slow down"}"#.into());
            }
            return (
                200,
                vec![],
                format!(
                    r#"{{"items": [{{"added_at": "2024-01-01T00:00:00Z", "track": {}}}], "next": null}}"#,
                    track_json("T", "A")
                ),
            );
        }
        (200, vec![], r#"{"items": [], "next": null}"#.into())
    }));
    let config = SpotifyConfig {
        accounts_base: Some(server.base_url.clone()),
        api_base: Some(format!("{}/v1", server.base_url)),
        ..SpotifyConfig::new("cid", "redir")
    };
    let mut src = SpotifySource::new(config, Arc::new(TcpTransport::default()), Arc::new(MemorySecretStore::new()))
        .with_test_clock(Box::new(|| NOW), Box::new(move |d| slept2.lock().unwrap().push(d)))
        .with_backoff(kontinuum_taste::spotify::BackoffPolicy { max_retries: 3, base_delay_ms: 500, max_delay_ms: 8_000 });
    let mut store = consented_store();
    src.exchange_code("c", "verifier-verifier-verifier-verifier-verifier-1234").unwrap();
    let report = src.sync(&mut store, SyncMode::Full).unwrap();
    assert!(report.retried_requests >= 1, "the 429 was retried");
    assert_eq!(slept.lock().unwrap().as_slice(), &[Duration::from_secs(2)], "Retry-After honored");
    assert_eq!(report.events, 1, "saved(1) landed after the retry; other endpoints empty");
}

#[test]
fn rate_limit_gives_up_after_max_retries() {
    let server = MockServer::start(Arc::new(|req: &RecordedRequest| {
        if req.path == "/api/token" {
            return (200, vec![], r#"{"access_token":"at","expires_in":3600}"#.into());
        }
        if req.path == "/v1/me/tracks" {
            return (429, vec![("Retry-After".into(), "1".into())], "{}".into());
        }
        (200, vec![], r#"{"items": [], "next": null}"#.into())
    }));
    let mut src = source_for(&server, Arc::new(MemorySecretStore::new()));
    let mut store = consented_store();
    src.exchange_code("c", "verifier-verifier-verifier-verifier-verifier-1234").unwrap();
    let err = src.sync(&mut store, SyncMode::Full).unwrap_err();
    assert!(matches!(err, TasteError::RateLimited { retries: 3, .. }), "got: {err:?}");
}

#[test]
fn expired_token_refreshes_via_refresh_grant_and_401_retries_once() {
    // expires_in=30 < the 60s freshness margin → every API call
    // proactively refreshes; the first top-artists call also 401s, which
    // forces the retry path on top. The contract under test: refresh
    // grants flow, and the post-401 retry carries a *newer* token.
    let token_hits = AtomicUsize::new(0);
    let api_hits = AtomicUsize::new(0);
    let refresh_bodies: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
    let refresh_bodies2 = refresh_bodies.clone();
    let first_api_token: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let first_api_token2 = first_api_token.clone();
    let second_api_token: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let second_api_token2 = second_api_token.clone();
    let server = MockServer::start(Arc::new(move |req: &RecordedRequest| {
        if req.path == "/api/token" {
            let hit = token_hits.fetch_add(1, Ordering::SeqCst);
            if hit > 0 {
                refresh_bodies2.lock().unwrap().push(req.body.clone());
            }
            return (
                200,
                vec![],
                format!(r#"{{"access_token": "at-{hit}", "expires_in": 30, "refresh_token": "rt"}}"#),
            );
        }
        if req.path == "/v1/me/top/artists" {
            let hit = api_hits.fetch_add(1, Ordering::SeqCst);
            let token = req.header("Authorization");
            if hit == 0 {
                *first_api_token2.lock().unwrap() = token;
                return (401, vec![], r#"{"error": "expired"}"#.into());
            }
            if hit == 1 {
                *second_api_token2.lock().unwrap() = token;
            }
        }
        (200, vec![], r#"{"items": [{"name": "A", "genres": ["techno"]}], "next": null}"#.into())
    }));
    let mut src = source_for(&server, Arc::new(MemorySecretStore::new()));
    let mut store = consented_store();
    src.exchange_code("c", "verifier-verifier-verifier-verifier-verifier-1234").unwrap();
    let report = src.sync(&mut store, SyncMode::Full).unwrap();
    assert!(report.retried_requests >= 1);
    let refreshes = refresh_bodies.lock().unwrap();
    assert!(refreshes.iter().all(|b| b.contains("grant_type=refresh_token")), "refresh grant used");
    assert!(!refreshes.is_empty(), "refresh grants flowed");
    let first = first_api_token.lock().unwrap().clone();
    let second = second_api_token.lock().unwrap().clone();
    assert_ne!(first, second, "the post-401 retry rode a newer token");
    assert!(second.is_some());
}

#[test]
fn incremental_sync_rides_the_cursor_and_skips_endpoints() {
    let recent_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
    let recent2 = recent_calls.clone();
    let server = MockServer::start(Arc::new(move |req: &RecordedRequest| {
        match (req.path.as_str(), req.method.as_str()) {
            ("/api/token", "POST") => (200, vec![], r#"{"access_token":"at","expires_in":3600}"#.into()),
            ("/v1/me/player/recently-played", "GET") => {
                recent2.lock().unwrap().push(req.query.clone());
                (200, vec![], format!(r#"{{"items": [{{"played_at": "2024-03-15T12:30:45Z", "track": {}}}], "next": null}}"#, track_json("T", "A")))
            }
            _ => (200, vec![], r#"{"items": [], "next": null}"#.into()),
        }
    }));
    let mut src = source_for(&server, Arc::new(MemorySecretStore::new()));
    let mut store = consented_store();
    src.exchange_code("c", "verifier-verifier-verifier-verifier-verifier-1234").unwrap();
    src.sync(&mut store, SyncMode::Full).unwrap();
    // Cursor stored from the served played_at timestamp.
    assert_eq!(
        store.cursor("spotify", "recently_played_after").unwrap().as_deref(),
        Some("1710505845000")
    );
    let paths_before = server.recorded().iter().filter(|r| r.path == "/v1/me/playlists").count();
    assert!(paths_before >= 1, "full sync walks playlists");

    let before = server.request_count();
    src.sync(&mut store, SyncMode::Incremental).unwrap();
    let incremental: Vec<RecordedRequest> = server
        .recorded()
        .into_iter()
        .skip(before)
        .filter(|r| r.path != "/api/token")
        .collect();
    assert!(
        !incremental.iter().any(|r| r.path == "/v1/me/playlists" || r.path == "/v1/me/tracks"),
        "incremental skips the heavy endpoints"
    );
    // The recently-played call rode the stored `after` cursor.
    let after_queries = recent_calls.lock().unwrap();
    let last = after_queries.last().unwrap();
    assert!(last.contains("after=1710505845000"), "incremental query: {last}");
}

#[test]
fn sync_without_consent_is_refused_before_any_network() {
    let server = MockServer::start_simple(200, "{}");
    let mut src = source_for(&server, Arc::new(MemorySecretStore::new()));
    let mut store = TasteStore::open_in_memory().unwrap();
    let err = src.sync(&mut store, SyncMode::Full).unwrap_err();
    assert!(matches!(err, TasteError::ConsentRequired { provider: "spotify" }));
    assert_eq!(server.request_count(), 0, "no request left the device");
}

#[test]
fn disconnect_purges_library_and_tokens() {
    let server = MockServer::start(combined_responder(
        |_| (200, vec![], r#"{"access_token":"at","expires_in":3600}"#.into()),
        ok_json_responder(),
    ));
    let secrets = Arc::new(MemorySecretStore::new());
    let mut src = source_for(&server, secrets.clone());
    let mut store = consented_store();
    src.exchange_code("c", "verifier-verifier-verifier-verifier-verifier-1234").unwrap();
    src.sync(&mut store, SyncMode::Full).unwrap();
    assert!(!secrets.is_empty());
    assert!(!store.events_for("spotify").unwrap().is_empty());

    src.disconnect(&mut store).unwrap();
    assert!(secrets.is_empty(), "tokens purged from the secret store");
    assert!(store.events_for("spotify").unwrap().is_empty());
    assert!(store.track_dna_jsons("spotify").unwrap().is_empty());
    assert_eq!(store.consent_for("spotify").unwrap(), Consent::default(), "consent row purged");
    assert!(store.profile().is_err() || store.profile().unwrap().is_some(), "profile is global, not per-source");
}
