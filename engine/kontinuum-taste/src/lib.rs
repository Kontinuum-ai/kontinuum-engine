//! Taste importer (issue #21): pluggable streaming sources → the canonical
//! musical DNA ([`kontinuum_compose::taste::TasteProfile`]).
//!
//! Layer map:
//! - [`http`] / [`pkce`] / [`secrets`] — the seams. No blocking third-party
//!   stack: the [`http::HttpTransport`] trait is host-injected (URLSession
//!   at the iOS FFI boundary, per the #22/#36 convention); the bundled
//!   [`http::TcpTransport`] speaks plain HTTP/1.1 over `std::net`, which is
//!   what the mock-server tests (and any non-TLS host) run on.
//! - [`spotify`] — the Spotify connector: Auth Code + PKCE, token refresh,
//!   paginated playlist/saved/top/recently-played pulls, 429/5xx backoff,
//!   incremental cursors, full purge on disconnect.
//! - [`model`] — the metadata taste model: entity graph weighted
//!   saved > playlisted > recently-played with a ~90-day recency decay,
//!   genre-mix / era / scene outputs and the adventurousness score.
//! - [`enrich`] — MusicBrainz/Discogs enrichment behind a rate-limited,
//!   keyless (MusicBrainz) provider trait on the same transport seam.
//! - [`audio`] — per-track DNA from the #5 on-device analysis subset and
//!   the weighted mean + dispersion aggregation into user DNA. Pinned
//!   references weigh more. Abstract features only.
//! - [`store`] — the on-device SQLite home: consent, sync cursors, library
//!   events, track DNA, learned profile. Purge, export and the
//!   "what we learned about you" data surface (#33's view reads this).
//! - [`map`] — DNA → generation: `GenParams`, groove, composer
//!   exploration budget (#24/#26) and `TastePriors` (#24).
//!
//! Privacy invariants (enforced in [`privacy`] tests): tokens live only in
//! the [`secrets::SecretStore`]; the store keeps abstract features and
//! never audio; playback runs with no transport in scope at all.

pub mod audio;
pub mod enrich;
pub mod error;
pub mod http;
pub mod map;
pub mod model;
pub mod pkce;
pub mod secrets;
pub mod source;
pub mod spotify;
pub mod store;

pub use error::TasteError;
pub use source::{SyncMode, SyncReport, TasteSource};
pub use store::TasteStore;
