//! Store contract (#19 storage half): migrations converge from fresh and
//! survive reopen, provenance is stamped on every import, hashes are stable
//! and catch tampering, and the f16 embedding blob round-trips bit-exactly.

use std::path::PathBuf;

use kontinuum_samples::{
    AudioEmbedding, CatalogDb, CatalogRow, EngineeredFeatures, SampleClass, StoreError,
    SAMPLE_PIPELINE_VERSION,
};

fn fixture() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/catalog.json"))
        .expect("fixture")
}

fn features() -> EngineeredFeatures {
    EngineeredFeatures {
        duration_s: 0.5,
        spectral_centroid_hz: 120.0,
        flatness: 0.1,
        pitch_hz: 48.0,
        transient_sharpness: 0.9,
        lufs: -7.0,
    }
}

fn row(id: &str, class: SampleClass) -> CatalogRow {
    CatalogRow {
        id: id.into(),
        path: "/libs/core/kick.wav".into(),
        features: features(),
        sample_rate: 48_000,
        class,
        pack: "core".into(),
        license: "CC0".into(),
        source_note: "synthesized".into(),
        tags: vec!["punchy".into()],
        embedding: None,
        embedding_dim: 0,
        pipeline_version: SAMPLE_PIPELINE_VERSION,
        integrity_hash: String::new(),
    }
    .with_integrity()
}

/// Unique per-test temp dir: process id + tag, no tempfile dependency.
fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kontinuum-store-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

#[test]
fn schema_migrates_fresh_and_survives_reopen() {
    let db = CatalogDb::open_in_memory().expect("migrate fresh");
    db.insert(&row("kick.punch.01", SampleClass::Kick)).expect("insert");
    assert_eq!(db.rows().expect("rows").len(), 1);
    assert_eq!(db.verify_integrity().expect("verify"), 1);

    let dir = temp_dir("reopen");
    let path = dir.join("catalog.sqlite");
    CatalogDb::open(&path).expect("migrate on-disk");
    let db = CatalogDb::open(&path).expect("reopen skips migration");
    db.insert(&row("hat.closed.01", SampleClass::Hat)).expect("insert");
    drop(db);
    let db = CatalogDb::open(&path).expect("reopen again");
    let ids: Vec<String> = db.rows().expect("rows").into_iter().map(|r| r.id).collect();
    assert_eq!(ids, vec!["hat.closed.01".to_string()], "on-disk rows survive reopen");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn import_json_counts_rows_and_hashes_stay_stable_on_reimport() {
    let db = CatalogDb::open_in_memory().expect("open");
    let n = db.import_json(&fixture(), "CC0", "core library v1").expect("import");
    assert_eq!(n, 20, "the shipped fixture has 20 samples");
    let first: Vec<(String, String)> =
        db.rows().expect("rows").into_iter().map(|r| (r.id, r.integrity_hash)).collect();

    let n2 = db.import_json(&fixture(), "CC0", "core library v1").expect("re-import");
    assert_eq!(n2, 20, "INSERT OR REPLACE keeps the count flat");
    let second: Vec<(String, String)> =
        db.rows().expect("rows").into_iter().map(|r| (r.id, r.integrity_hash)).collect();
    assert_eq!(first, second, "same content → same ids and hashes");
    assert_eq!(db.verify_integrity().expect("verify"), 20);
}

#[test]
fn imported_rows_carry_mandatory_provenance_and_defaults() {
    let db = CatalogDb::open_in_memory().expect("open");
    db.import_json(&fixture(), "CC0", "core library v1").expect("import");
    let r = &db.rows().expect("rows")[0];
    assert_eq!(r.license, "CC0");
    assert_eq!(r.source_note, "core library v1");
    assert_eq!(r.sample_rate, 48_000, "documented synthesized default");
    assert_eq!(r.path, "", "JSON-domain rows have no filesystem path");
    assert_eq!(r.pipeline_version, SAMPLE_PIPELINE_VERSION);
}

#[test]
fn verify_integrity_catches_tampered_rows() {
    let db = CatalogDb::open_in_memory().expect("open");

    // Content edited without a re-stamp: the stored hash goes stale.
    let mut edited = row("kick.punch.01", SampleClass::Kick);
    edited.pack = "someone-edited-content".into();
    db.insert(&edited).expect("insert");
    assert!(matches!(
        db.verify_integrity(),
        Err(StoreError::Integrity { id }) if id == "kick.punch.01"
    ), "content edits without a re-stamp are corruption");

    // A hand-written hash is stored as told — and then judged.
    let mut tampered = row("hat.closed.01", SampleClass::Hat);
    tampered.integrity_hash = "deadbeefdeadbeef".into();
    db.insert(&tampered).expect("insert");
    assert!(matches!(
        db.verify_integrity(),
        Err(StoreError::Integrity { id }) if id == "hat.closed.01"
    ), "rows sort id-ascending, so the hat reports first");
}

#[test]
fn f16_embedding_blob_round_trips_bit_exactly() {
    // All exactly f16-representable; 2^-24 is f16's smallest subnormal
    // (blob bits 0x0001) and 65504.0 is its largest finite.
    let vector = vec![
        0.0,
        -0.0,
        1.0,
        -2.5,
        0.33325195,
        6.1035156e-5,  // smallest normal 2^-14
        5.9604645e-8,  // smallest subnormal 2^-24
        -5.9604645e-8, // negative subnormal
        65504.0,
    ];
    let mut r = row("pad.warm.01", SampleClass::Pad);
    r.embedding = Some(vector.clone());
    r.embedding_dim = vector.len() as u32;
    let r = r.with_integrity();

    let db = CatalogDb::open_in_memory().expect("open");
    db.insert(&r).expect("insert");
    let stored = db.embedding_row("pad.warm.01").expect("query").expect("present");
    let back = stored.vector();
    assert_eq!(back.len(), vector.len());
    for (a, b) in vector.iter().zip(back.iter()) {
        assert_eq!(a.to_bits(), b.to_bits(), "{a} did not survive f16 storage");
    }
    assert!(db.embedding_row("no-such-id").expect("query").is_none());
}

#[test]
fn catalog_view_matches_the_document_domain() {
    let db = CatalogDb::open_in_memory().expect("open");
    db.import_json(&fixture(), "CC0", "core library v1").expect("import");
    let catalog = db.catalog().expect("catalog");
    assert_eq!(catalog.len(), 20);
    let kick = catalog.iter().find(|s| s.id == "kick.punch.01").expect("kick row");
    assert_eq!(kick.class, SampleClass::Kick);
    assert_eq!(kick.features.spectral_centroid_hz, 95.0, "fixture value preserved");
    assert_eq!(kick.features.duration_s, 0.35);
    assert_eq!(kick.pack, "core-synth");
    assert_eq!(kick.tags, vec!["punchy".to_string(), "four-floor".to_string(), "analog".to_string()]);
    assert!(kick.embedding.is_none(), "fixture ships without embeddings");
}

#[test]
fn database_newer_than_this_build_is_a_typed_error() {
    let dir = temp_dir("newer");
    let path = dir.join("future.sqlite");
    {
        let conn = rusqlite::Connection::open(&path).expect("raw open");
        conn.execute_batch("PRAGMA user_version = 99;").expect("stamp future version");
    }
    assert!(matches!(
        CatalogDb::open(&path),
        Err(StoreError::Version { found: 99 })
    ), "a newer schema must never be silently downgraded");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn query_log_records_searches_and_mines_gaps() {
    let db = CatalogDb::open_in_memory().expect("open");
    db.import_json(&fixture(), "CC0", "core library v1").expect("import");

    let ask = |terms: &str, class: Option<SampleClass>| kontinuum_samples::SampleQuery {
        text_terms: terms.split_whitespace().map(str::to_string).collect(),
        class,
        duration_range: None,
        target_centroid_hz: None,
    };

    // A search the library serves well, asked twice...
    let served = db
        .run_and_log_query(&ask("punchy four-floor kick", Some(SampleClass::Kick)), "2026-09-04T00:00:00Z")
        .expect("run");
    assert!(!served.used_fallback, "fixture kick should clear the floor");
    assert!(!served.candidates.is_empty());
    db.run_and_log_query(&ask("punchy kick", Some(SampleClass::Kick)), "2026-09-04T00:01:00Z")
        .expect("run");
    // ...and two poorly-served asks for material the library lacks.
    for ts in ["2026-09-04T00:02:00Z", "2026-09-04T00:03:00Z", "2026-09-04T00:04:00Z"] {
        db.run_and_log_query(&ask("vinyl crackle dusty loop", Some(SampleClass::Texture)), ts)
            .expect("run");
    }
    assert_eq!(db.query_log_len().expect("count"), 5);

    let gaps = db.worst_served_queries(2, 10).expect("mine");
    assert!(!gaps.is_empty(), "mining must return the repeated asks");
    let weakest = &gaps[0];
    assert_eq!(weakest.terms, "vinyl crackle dusty loop", "worst-served first");
    assert_eq!(weakest.asks, 3);

    // Singletons stay out at the min-asks floor.
    assert!(gaps.iter().all(|g| g.asks >= 2));
    assert!(
        gaps.iter().all(|g| g.avg_top_score <= 1.0),
        "scores are cosine-bounded"
    );
}
