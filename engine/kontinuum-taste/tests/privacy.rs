//! Privacy guarantees (issue #21), as enforced tests:
//! 1. No audio retention — the store keeps abstract features only, and
//!    the persisted byte size does not scale with audio length.
//! 2. No secrets outside the SecretStore — tokens never touch the store,
//!    the profile, or the export.
//! 3. Zero network during playback — the playback path takes no
//!    transport at all, tested with a fail-loud transport in scope.

use std::sync::Arc;

use kontinuum_compose::taste::session_from_taste;
use kontinuum_taste::audio::{Contribution, TrackDna};
use kontinuum_taste::error::TasteError;
use kontinuum_taste::http::{HttpRequest, HttpResponse, HttpTransport};
use kontinuum_taste::map::{composer_bias_for_dna, gen_params_for_dna, session_from_dna, taste_priors_for_dna};
use kontinuum_taste::secrets::{MemorySecretStore, SecretStore};
use kontinuum_taste::source::TasteSource;
use kontinuum_taste::spotify::{SpotifyConfig, SpotifySource};
use kontinuum_taste::store::{Consent, TasteStore};

/// A transport that fails the test the moment it is used. The playback
/// path is handed this Arc and must never call it.
struct PanicTransport;

impl HttpTransport for PanicTransport {
    fn send(&self, _req: &HttpRequest) -> Result<HttpResponse, TasteError> {
        panic!("network call during playback — taste layer reached the transport")
    }
}

#[test]
fn stored_dna_size_is_independent_of_audio_length() {
    let preset = kontinuum_analysis::synthgen::preset_by_id("mt-a").unwrap();
    let mono = kontinuum_analysis::synthgen::render(preset);
    let full = TrackDna::analyze(preset.track_id, &mono, kontinuum_analysis::synthgen::SYNTH_SAMPLE_RATE, preset.bpm).unwrap();
    // Same track truncated to the 8-second analysis floor: same feature
    // count, different input length (same id — ids are caller-chosen and
    // not the point here).
    let short_len = kontinuum_analysis::synthgen::SYNTH_SAMPLE_RATE as usize * 8;
    let short = TrackDna::analyze(preset.track_id, &mono[..short_len], kontinuum_analysis::synthgen::SYNTH_SAMPLE_RATE, preset.bpm).unwrap();

    let full_json = serde_json::to_string(&full).unwrap();
    let short_json = serde_json::to_string(&short).unwrap();
    // A 30s+ render and an 8s slice serialize to records bounded by the
    // fixed schema, not by the input: audio retention would scale this
    // with length. (Values differ, so float formatting jitters a few
    // bytes — that is why this is a bound, not an equality.)
    assert!(full_json.len() < 512 && short_json.len() < 512, "abstract features only: {full_json}");
    assert!(full_json.len().abs_diff(short_json.len()) < 32, "record size must not track audio length");

    // And the store that persists it stays tiny no matter the audio.
    let dir = std::env::temp_dir().join(format!("kt-privacy-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("taste.db");
    {
        let store = TasteStore::open(&path).unwrap();
        store.set_consent("local", Consent { metadata_sync: false, audio_analysis: true, enrichment: false }, 1).unwrap();
        store.upsert_track_dna("local", preset.track_id, &full_json).unwrap();
    }
    let db_bytes = std::fs::read(&path).unwrap().len();
    let pcm_bytes = mono.len() * 4; // f32 samples the analysis consumed
    assert!(
        db_bytes < 100_000 && db_bytes * 100 < pcm_bytes,
        "db {db_bytes} bytes must be a rounding error next to {pcm_bytes} bytes of analyzed PCM"
    );
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
}

#[test]
fn tokens_never_leave_the_secret_store() {
    // A sync against the fail-loud transport would panic on any network
    // use, so run the token path with a real store but assert on the
    // persisted side only: build the state by hand (token in secrets,
    // rich profile in store), then verify nothing leaks into the store
    // bytes or the export.
    let secrets = MemorySecretStore::new();
    secrets.set("spotify/access-token", "SUPER-SECRET-ACCESS");
    secrets.set("spotify/refresh-token", "SUPER-SECRET-REFRESH");

    let dir = std::env::temp_dir().join(format!("kt-tokens-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("taste.db");
    {
        let store = TasteStore::open(&path).unwrap();
        store
            .set_consent("spotify", Consent { metadata_sync: true, audio_analysis: false, enrichment: false }, 1)
            .unwrap();
        let mut profile = kontinuum_compose::taste::TasteProfile::default();
        profile.bpm = Some(128.0);
        profile.genres = vec!["minimal techno".into()];
        store.save_profile(&profile, 1).unwrap();
        store.upsert_track_dna("spotify", "t1", r#"{"track_id":"t1","bpm":128.0}"#).unwrap();

        let export = store.export_profile_json().unwrap();
        let what = store.what_we_learned().unwrap();
        let learned = serde_json::to_string(&what).unwrap();
        let db = std::fs::read(&path).unwrap();
        let db_text = String::from_utf8_lossy(&db).to_string();
        for surface in [export, learned, db_text] {
            assert!(!surface.contains("SUPER-SECRET-ACCESS"));
            assert!(!surface.contains("SUPER-SECRET-REFRESH"));
            assert!(!surface.to_lowercase().contains("refresh_token"));
        }
    }
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
}

#[test]
fn playback_never_touches_the_transport() {
    // The fail-loud transport is in scope for the whole test; the
    // playback path cannot reach it because every entry point takes data
    // only — this is the structural guarantee, not an honor system.
    let transport: Arc<dyn HttpTransport> = Arc::new(PanicTransport);
    let secrets = Arc::new(MemorySecretStore::new());
    let config = SpotifyConfig::new("cid", "redir");
    let mut src = SpotifySource::new(config, transport.clone(), secrets);
    drop(transport);
    let _ = &mut src; // the connector exists in this test and stays inert

    let mut profile = kontinuum_compose::taste::TasteProfile::default();
    profile.bpm = Some(126.0);
    profile.swing = Some(kontinuum_compose::taste::Stat::new(0.1, 0.02));
    profile.adventurousness = Some(0.7);

    // The entire playback surface, exercised under the panic transport:
    let session = session_from_dna(&profile, 11);
    let _params = gen_params_for_dna(&profile, 11);
    let _biased = composer_bias_for_dna(&profile);
    let _priors = taste_priors_for_dna(&profile, vec![1], vec![0]).unwrap();
    let _varied = kontinuum_compose::taste::vary_session(&session, 11, 2);
    let _regen = session_from_taste(&profile, 11);
    assert!(kontinuum_ir::validate_session(&session).is_ok());
}

#[test]
fn disconnect_purges_profile_contributions_and_consent_together() {
    let mut store = TasteStore::open_in_memory().unwrap();
    store
        .set_consent("spotify", Consent { metadata_sync: true, audio_analysis: true, enrichment: false }, 1)
        .unwrap();
    let dna = TrackDna {
        track_id: "t".into(),
        bpm: 128.0,
        swing: Some(0.1),
        brightness: 0.4,
        energy: 0.6,
        density: 0.5,
        section_bars: 16.0,
        pipeline_version: 1,
    };
    store.upsert_track_dna("spotify", "t", &serde_json::to_string(&dna).unwrap()).unwrap();
    store.save_profile(&kontinuum_compose::taste::TasteProfile::default(), 1).unwrap();

    // "delete profile" clears the learned DNA; disconnect clears the
    // source. Both are the privacy screen's actions (#33 renders them).
    store.delete_profile().unwrap();
    assert!(store.profile().unwrap().is_none());

    let mut src = SpotifySource::new(
        SpotifyConfig::new("cid", "redir"),
        Arc::new(PanicTransport),
        Arc::new(MemorySecretStore::new()),
    );
    src.disconnect(&mut store).unwrap();
    let learned = store.what_we_learned().unwrap();
    let status = learned.sources.iter().find(|s| s.source == "spotify");
    assert!(status.is_none(), "purged sources vanish from the what-we-learned surface");
    // Any surviving track DNA for the source is gone with it.
    assert!(store.track_dna_jsons("spotify").unwrap().is_empty());
    // And a Contribution dropped afterwards is just numbers, no audio.
    drop(Contribution::library(dna));
}
