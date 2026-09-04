//! Pack pipeline contract: −1 dBTP normalization preserves dynamics, the
//! `.kpack` container round-trips bit-exactly with equal catalog rows,
//! every corruption mode is rejected, and builds are byte-deterministic.

use std::path::Path;

use kontinuum_samples::{
    analyze_features, build_pack, ingest_dir, load_pack, normalize_to_target_peak, CatalogRow,
    IngestedSample, PackError, PackMeta, SampleClass, TARGET_DBTP, SAMPLE_PIPELINE_VERSION,
};

fn meta() -> PackMeta {
    PackMeta {
        pack: "test-pack".into(),
        license: "CC0".into(),
        source: "synthesized in-test".into(),
    }
}

fn target_peak() -> f32 {
    10f32.powf(TARGET_DBTP / 20.0)
}

fn sine(freq: f32, sr: u32, secs: f32) -> Vec<f32> {
    (0..(sr as f32 * secs) as usize)
        .map(|i| (std::f32::consts::TAU * freq * i as f32 / sr as f32).sin())
        .collect()
}

fn decaying_noise(sr: u32, secs: f32) -> Vec<f32> {
    // LCG noise with an exponential decay: hot attack, noisy body.
    let n = (sr as f32 * secs) as usize;
    let mut state = 0x1234_5678u32;
    (0..n)
        .map(|i| {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let noise = (state as f32 / u32::MAX as f32) * 2.0 - 1.0;
            noise * (-(i as f32) / (0.05 * sr as f32)).exp()
        })
        .collect()
}

fn ingested(id: &str, class: SampleClass, pcm: Vec<f32>, sr: u32) -> IngestedSample {
    IngestedSample {
        id: id.into(),
        class,
        sample_rate: sr,
        features: analyze_features(&pcm, sr),
        pcm,
    }
}

fn kit() -> Vec<IngestedSample> {
    let sr = 48_000;
    vec![
        ingested("kick.round.01", SampleClass::Kick, sine(55.0, sr, 0.3), sr),
        ingested("hat.tick.01", SampleClass::Hat, decaying_noise(sr, 0.08), sr),
    ]
}

/// The row convention for container entries: no filesystem path, provenance
/// straight from the pack meta.
fn row_for(sample: &IngestedSample, meta: &PackMeta) -> CatalogRow {
    CatalogRow {
        id: sample.id.clone(),
        path: String::new(),
        features: sample.features,
        sample_rate: sample.sample_rate,
        class: sample.class,
        pack: meta.pack.clone(),
        license: meta.license.clone(),
        source_note: meta.source.clone(),
        tags: vec![],
        embedding: None,
        embedding_dim: 0,
        pipeline_version: SAMPLE_PIPELINE_VERSION,
        integrity_hash: String::new(),
    }
    .with_integrity()
}

fn write_wav(path: &Path, pcm: &[f32], sr: u32) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: sr,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec).expect("wav writer");
    for s in pcm {
        writer.write_sample(*s).expect("write");
    }
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("kontinuum-pack-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

#[test]
fn normalize_hits_minus_one_dbtp_and_preserves_ratios() {
    let mut pcm = vec![0.8, -0.4, 0.2, -0.05];
    normalize_to_target_peak(&mut pcm, target_peak());
    let peak = pcm.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!((peak - target_peak()).abs() < 1e-6, "peak {peak} missed −1 dBTP");
    let before = 0.8 / 0.4;
    let after = pcm[0] / -pcm[1];
    assert!(
        (before - after).abs() / before < 1e-5,
        "scalar gain must keep sample ratios: {before} vs {after}"
    );

    let mut silent = vec![0.0; 64];
    normalize_to_target_peak(&mut silent, target_peak());
    assert!(silent.iter().all(|s| *s == 0.0), "silence stays silent, no NaN");

    let mut already = vec![target_peak(), -target_peak() / 2.0];
    normalize_to_target_peak(&mut already, target_peak());
    assert_eq!(already[0], target_peak(), "at-target material is untouched");
}

#[test]
fn features_stay_in_bounds_for_hot_tonal_noisy_and_silent_input() {
    let sr = 48_000u32;
    for (name, pcm) in [
        ("tone", sine(1000.0, sr, 0.2)),
        ("noise", decaying_noise(sr, 0.2)),
        ("silence", vec![0.0; sr as usize / 2]),
    ] {
        let f = analyze_features(&pcm, sr);
        assert!(f.duration_s > 0.0, "{name}");
        assert!(f.spectral_centroid_hz >= 20.0, "{name} centroid {}", f.spectral_centroid_hz);
        assert!((0.0..=1.0).contains(&f.flatness), "{name} flatness {}", f.flatness);
        assert_eq!(f.pitch_hz, 0.0, "{name}: pitch is pipeline-filled later");
        assert!((0.0..=1.0).contains(&f.transient_sharpness), "{name}");
        assert!(f.lufs.is_finite(), "{name}");
    }
    let silent = analyze_features(&[0.0; 4800], sr);
    assert_eq!(silent.spectral_centroid_hz, 20.0, "silence floors at the 20 Hz bound");
    assert_eq!(silent.lufs, -120.0, "silence floors at the documented loudness bound");

    let tone = analyze_features(&sine(1000.0, sr, 0.2), sr);
    assert!(
        (500.0..2000.0).contains(&tone.spectral_centroid_hz),
        "1 kHz sine centroid lands near it: {}",
        tone.spectral_centroid_hz
    );
    let noise = analyze_features(&decaying_noise(sr, 0.2), sr);
    assert!(
        noise.flatness > tone.flatness,
        "noise is flatter ({}) than a tone ({})",
        noise.flatness,
        tone.flatness
    );
    // Sharpness is the largest envelope RISE: a hit preceded by silence
    // jumps the envelope in one step; a slow swell creeps up.
    let hit = analyze_features(&[vec![0.0; sr as usize / 100], decaying_noise(sr, 0.2)].concat(), sr);
    let swell: Vec<f32> = decaying_noise(sr, 0.2)
        .iter()
        .enumerate()
        .map(|(i, s)| s * i as f32 / (0.2 * sr as f32))
        .collect();
    let slow = analyze_features(&swell, sr);
    assert!(
        hit.transient_sharpness > slow.transient_sharpness * 2.0,
        "silent-then-hit ({}) must out-sharp a swell ({})",
        hit.transient_sharpness,
        slow.transient_sharpness
    );
}

#[test]
fn pack_round_trips_bit_identically_with_equal_catalog_rows() {
    let samples = kit();
    let bytes = build_pack(&samples, &meta());
    let pack = load_pack(&bytes).expect("load");

    assert_eq!(pack.manifest.pack, "test-pack");
    assert_eq!(pack.manifest.license, "CC0");
    assert_eq!(pack.manifest.source, "synthesized in-test");
    assert_eq!(pack.entries.len(), 2);

    let manifest_meta = PackMeta {
        pack: pack.manifest.pack.clone(),
        license: pack.manifest.license.clone(),
        source: pack.manifest.source.clone(),
    };
    let mut built_rows: Vec<CatalogRow> =
        samples.iter().map(|s| row_for(s, &manifest_meta)).collect();
    built_rows.sort_by(|a, b| a.id.cmp(&b.id));
    let loaded_rows: Vec<CatalogRow> = pack
        .entries
        .iter()
        .map(|e| CatalogRow {
            id: e.meta.id.clone(),
            path: String::new(),
            features: e.meta.features,
            sample_rate: e.meta.sample_rate,
            class: e.meta.class,
            pack: manifest_meta.pack.clone(),
            license: manifest_meta.license.clone(),
            source_note: manifest_meta.source.clone(),
            tags: vec![],
            embedding: None,
            embedding_dim: 0,
            pipeline_version: SAMPLE_PIPELINE_VERSION,
            integrity_hash: String::new(),
        })
        .map(|r| r.with_integrity())
        .collect();
    assert_eq!(built_rows, loaded_rows, "both sides describe the same rows");

    for entry in &pack.entries {
        let source = samples.iter().find(|s| s.id == entry.meta.id).expect("source");
        assert_eq!(source.pcm.len(), entry.pcm.len());
        assert!(
            source.pcm.iter().zip(entry.pcm.iter()).all(|(a, b)| a.to_bits() == b.to_bits()),
            "entry `{}` PCM is not bit-identical",
            entry.meta.id
        );
    }
}

#[test]
fn corruption_is_rejected_in_every_mode() {
    let bytes = build_pack(&kit(), &meta());
    let last = bytes.len() - 1;

    let mut bad = bytes.clone();
    bad[0] ^= 0xff;
    assert!(matches!(load_pack(&bad), Err(PackError::Header(_))), "bad magic");

    let mut bad = bytes.clone();
    bad[4] = 2; // version u16 LE: 1 → 2
    assert!(matches!(load_pack(&bad), Err(PackError::Header(_))), "bad version");

    let truncated = &bytes[..bytes.len() / 2];
    assert!(load_pack(truncated).is_err(), "truncated container");

    // Flip one bit inside the last entry's PCM (before the trailer).
    let mut bad = bytes.clone();
    bad[last - 12] ^= 0x01;
    assert!(matches!(load_pack(&bad), Err(PackError::Integrity(_))), "corrupted PCM");

    let mut bad = bytes.clone();
    bad[last] ^= 0x01;
    assert!(matches!(load_pack(&bad), Err(PackError::Integrity(_))), "corrupted trailer");
}

#[test]
fn same_input_builds_identical_bytes_and_hashes() {
    let samples = kit();
    let a = build_pack(&samples, &meta());
    let b = build_pack(&samples, &meta());
    assert_eq!(a, b, "same samples + meta → identical container bytes");

    let rows_a: Vec<String> = samples.iter().map(|s| row_for(s, &meta()).integrity_hash).collect();
    let rows_b: Vec<String> = samples.iter().map(|s| row_for(s, &meta()).integrity_hash).collect();
    assert_eq!(rows_a, rows_b, "integrity hashes are stable");
    assert!(rows_a.iter().all(|h| h.len() == 16), "hex-encoded u64");
}

#[test]
fn ingest_dir_walks_sorted_rejects_unknown_and_ignores_non_wav() {
    let dir = temp_dir("ingest");
    let sr = 48_000u32;
    // Created out of order; output must come back sorted by id.
    std::fs::create_dir_all(dir.join("texture")).expect("dir");
    std::fs::create_dir_all(dir.join("kick")).expect("dir");
    write_wav(&dir.join("kick/a-hot.wav"), &sine(50.0, sr, 0.1), sr);
    write_wav(&dir.join("texture/zzz-bed.wav"), &sine(220.0, sr, 0.1), sr);
    write_wav(&dir.join("kick/m-quiet.wav"), &sine(60.0, sr, 0.1), sr);
    std::fs::write(dir.join("kick/notes.txt"), "ignore me").expect("stray file");
    std::fs::write(dir.join("README.md"), "# pack").expect("top-level file");

    let samples = ingest_dir(&dir).expect("ingest");
    let ids: Vec<&str> = samples.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec!["kick.a-hot", "kick.m-quiet", "texture.zzz-bed"], "sorted ids");

    let hot = &samples[0];
    let peak = hot.pcm.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!((peak - target_peak()).abs() < 1e-6, "ingest normalizes to −1 dBTP: {peak}");
    assert_eq!(hot.class, SampleClass::Kick);
    assert!(hot.features.spectral_centroid_hz >= 20.0);

    std::fs::create_dir_all(dir.join("bells")).expect("unknown class dir");
    assert!(matches!(ingest_dir(&dir), Err(PackError::Manifest(_))), "unknown subdir rejected");
    let _ = std::fs::remove_dir_all(&dir);
}
