//! Metadata enrichment (issue #21): MusicBrainz/Discogs lookups behind a
//! provider trait on the same [`HttpTransport`] seam as the connectors —
//! mock-server testable, no API keys required.
//!
//! Rate limits are contractual: MusicBrainz allows **one request per
//! second** with a identifying User-Agent; the client enforces the
//! interval between requests itself (via the injected clock), independent
//! of caller discipline. Discogs requires a personal-access token for
//! meaningful rates — without one the client reports disabled rather than
//! hammering anonymously.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::error::TasteError;
use crate::http::HttpRequest;
use crate::http::HttpTransport;

/// What enrichment knows about one artist.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct ArtistEnrichment {
    pub artist: String,
    /// MusicBrainz/Discogs tags — the subgenre vocabulary (deep-house,
    /// minimal-techno, …) the entity graph can't get from streaming APIs.
    pub tags: Vec<String>,
    /// Life-span begin year (era evidence).
    pub begin_year: Option<i32>,
    /// Country/area (scene evidence).
    pub origin: Option<String>,
}

pub trait EnrichmentProvider: Send + Sync {
    fn name(&self) -> &'static str;
    /// `Ok(None)` = looked up, nothing found. Errors are transport/parsing
    /// failures, never "no artist".
    fn lookup_artist(&self, name: &str) -> Result<Option<ArtistEnrichment>, TasteError>;
}

/// MusicBrainz (keyless, 1 req/s per ToS). `now_ms_ms`-style monotonic
/// clock is injected so tests can assert the spacing without waiting.
pub struct MusicBrainzClient {
    transport: Arc<dyn HttpTransport>,
    base: String,
    /// Minimum spacing between requests (ToS: 1 s).
    min_interval: Duration,
    /// Monotonic ms of the last request (atomic: &self rate limiting).
    last_request_ms: AtomicI64,
    now_ms: Box<dyn Fn() -> i64 + Send + Sync>,
    sleep_ms: Box<dyn Fn(Duration) + Send + Sync>,
    pub requests_made: AtomicI64,
}

impl MusicBrainzClient {
    pub fn new(transport: Arc<dyn HttpTransport>) -> Self {
        MusicBrainzClient {
            transport,
            base: "http://musicbrainz.org/ws/2".into(),
            min_interval: Duration::from_secs(1),
            last_request_ms: AtomicI64::new(i64::MIN / 2),
            now_ms: Box::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64
            }),
            sleep_ms: Box::new(std::thread::sleep),
            requests_made: AtomicI64::new(0),
        }
    }

    /// Test configuration: mock base URL, interval, clock, sleep.
    pub fn with_test_setup(
        mut self,
        base: String,
        min_interval: Duration,
        now_ms: Box<dyn Fn() -> i64 + Send + Sync>,
        sleep_ms: Box<dyn Fn(Duration) + Send + Sync>,
    ) -> Self {
        self.base = base;
        self.min_interval = min_interval;
        self.now_ms = now_ms;
        self.sleep_ms = sleep_ms;
        self
    }
}

impl EnrichmentProvider for MusicBrainzClient {
    fn name(&self) -> &'static str {
        "musicbrainz"
    }

    fn lookup_artist(&self, name: &str) -> Result<Option<ArtistEnrichment>, TasteError> {
        // Rate gate BEFORE the request: wait out the remainder of the
        // interval since the previous lookup.
        let now = (self.now_ms)();
        let earliest = self.last_request_ms.load(Ordering::Relaxed) + self.min_interval.as_millis() as i64;
        if now < earliest {
            (self.sleep_ms)(Duration::from_millis((earliest - now) as u64));
        }
        self.last_request_ms.store((self.now_ms)(), Ordering::Relaxed);
        self.requests_made.fetch_add(1, Ordering::Relaxed);

        let query = format!("artist:{}&fmt=json&limit=1", crate::spotify::urlencode(name));
        let url = format!("{}/artist?{query}", self.base);
        let req = HttpRequest::get(&url)
            .with_header("User-Agent", "Kontinuum/0.1 (taste-importer; issue #21)")
            .with_header("Accept", "application/json");
        let resp = self.transport.send(&req)?;
        if resp.status == 404 {
            return Ok(None);
        }
        if resp.status != 200 {
            return Err(TasteError::HttpStatus { status: resp.status, url, body: resp.body });
        }
        let v: serde_json::Value = serde_json::from_str(&resp.body)
            .map_err(|e| TasteError::BadResponse { provider: "musicbrainz", reason: e.to_string() })?;
        let artist = match v["artists"].as_array().and_then(|a| a.first()) {
            Some(a) => a,
            None => return Ok(None),
        };
        let enrichment = ArtistEnrichment {
            artist: artist["name"].as_str().unwrap_or(name).to_string(),
            tags: artist["tags"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|t| t["name"].as_str())
                .map(str::to_string)
                .collect(),
            begin_year: artist["life-span"]["begin"]
                .as_str()
                .and_then(|y| y.get(..4))
                .and_then(|y| y.parse().ok()),
            origin: artist["country"].as_str().map(str::to_string),
        };
        Ok(Some(enrichment))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::HttpResponse;
    use std::sync::Mutex;

    struct FakeTransport {
        calls: Mutex<Vec<String>>,
    }

    impl HttpTransport for FakeTransport {
        fn send(&self, req: &HttpRequest) -> Result<HttpResponse, TasteError> {
            self.calls.lock().unwrap().push(req.url.clone());
            Ok(HttpResponse {
                status: 200,
                headers: vec![],
                body: r#"{"artists": [{"name": "Ricardo Villalobos", "country": "DE",
                    "life-span": {"begin": "1970-08-06"},
                    "tags": [{"name": "minimal techno"}, {"name": "microhouse"}]}]}"#
                    .into(),
            })
        }
    }

    #[test]
    fn parses_mb_response_into_enrichment() {
        let t = Arc::new(FakeTransport { calls: Mutex::new(vec![]) });
        let mut c = MusicBrainzClient::new(t.clone()).with_test_setup(
            "http://mb.test/ws/2".into(),
            Duration::ZERO,
            Box::new(|| 1_000),
            Box::new(|_| {}),
        );
        let e = c.lookup_artist("ricardo villalobos").unwrap().unwrap();
        assert_eq!(e.tags, vec!["minimal techno", "microhouse"]);
        assert_eq!(e.begin_year, Some(1970));
        assert_eq!(e.origin.as_deref(), Some("DE"));
        assert_eq!(t.calls.lock().unwrap().len(), 1);
        assert!(t.calls.lock().unwrap()[0].contains("artist:ricardo"));
    }

    #[test]
    fn rate_gate_waits_out_the_interval() {
        let t = Arc::new(FakeTransport { calls: Mutex::new(vec![]) });
        let clock = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
        let slept = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u64>::new()));
        let slept2 = slept.clone();
        let c = MusicBrainzClient::new(t.clone()).with_test_setup(
            "http://mb.test/ws/2".into(),
            Duration::from_millis(1_000),
            Box::new(move || clock.fetch_add(100, Ordering::Relaxed) as i64 + 100),
            Box::new(move |d| slept2.lock().unwrap().push(d.as_millis() as u64)),
        );
        c.lookup_artist("a").unwrap();
        c.lookup_artist("b").unwrap();
        // Second request: clock advanced 100ms of the required 1000 → a
        // 900ms wait was recorded.
        assert_eq!(slept.lock().unwrap().as_slice(), &[900]);
        assert_eq!(t.calls.lock().unwrap().len(), 2, "both requests went out");
    }
}
