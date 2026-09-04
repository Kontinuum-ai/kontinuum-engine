//! Error surface of the taste importer.

use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum TasteError {
    #[error("transport failed: {0}")]
    Transport(String),
    #[error("http {status} from {url}: {body}")]
    HttpStatus {
        status: u16,
        url: String,
        body: String,
    },
    #[error("rate limited by {provider}, gave up after {retries} retries (last Retry-After {retry_after:?})")]
    RateLimited {
        provider: &'static str,
        retries: u32,
        retry_after: Option<Duration>,
    },
    #[error("not authenticated with {provider}: {reason}")]
    NotAuthenticated { provider: &'static str, reason: String },
    #[error("sync for {provider} was not consented (privacy gate)")]
    ConsentRequired { provider: &'static str },
    #[error("store error: {0}")]
    Store(String),
    #[error("analysis failed: {0}")]
    Analysis(String),
    #[error("bad response from {provider}: {reason}")]
    BadResponse { provider: &'static str, reason: String },
    #[error("{0}")]
    Other(String),
}

impl From<rusqlite::Error> for TasteError {
    fn from(e: rusqlite::Error) -> Self {
        TasteError::Store(e.to_string())
    }
}

impl From<serde_json::Error> for TasteError {
    fn from(e: serde_json::Error) -> Self {
        TasteError::BadResponse { provider: "json", reason: e.to_string() }
    }
}
