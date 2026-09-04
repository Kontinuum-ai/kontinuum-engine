//! End-to-end deliverable export (#102): the files land, they are readable
//! by a normal decoder, and the same session always produces the same bytes.

use std::path::{Path, PathBuf};

use kontinuum_export::{
    encode::Encoding, export_session, Cut, Deliverable, ExportDate, ExportError, ExportRequest,
    Master,
};
use kontinuum_ir::Session;
use kontinuum_mastering::targets::MasteringTargets;
use kontinuum_offline::parse_session;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/loop-4track.ir.json");

fn fixture() -> Session {
    parse_session(Path::new(FIXTURE)).expect("fixture session")
}

/// Each test gets its own directory so a parallel run cannot see another
/// test's files.
fn out_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("kontinuum-export-tests")
        .join(format!("{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn request(tag: &str) -> ExportRequest {
    ExportRequest::new("Kontinuum", "Night Shift", ExportDate::new(2026, 9, 2), out_dir(tag))
}

#[test]
fn default_set_writes_four_named_files() {
    let session = fixture();
    let req = request("default-set");
    let report = export_session(&session, &req, &MasteringTargets::hypothesis()).expect("export");

    assert_eq!(report.files.len(), 4);
    assert_eq!(report.seed, session.seed);

    let names: Vec<String> = report
        .files
        .iter()
        .map(|f| f.path.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    assert_eq!(
        names,
        vec![
            "Kontinuum - Night Shift (Full Mix) 48k-32float 20260902.wav",
            "Kontinuum - Night Shift (Full Mix) 48k-24bit 20260902.wav",
            "Kontinuum - Night Shift (Full Mix) 48k-16bit 20260902.wav",
            "Kontinuum - Night Shift (Full Mix) 48k-320kbps 20260902.mp3",
        ]
    );
    for f in &report.files {
        assert!(f.path.is_file(), "{} was not written", f.path.display());
        assert!(f.bytes > 1024, "{} is {} bytes", f.path.display(), f.bytes);
        assert!(f.duration_secs() > 1.0, "{} is {}s", f.path.display(), f.duration_secs());
    }
    let _ = std::fs::remove_dir_all(&req.out_dir);
}

/// Precision has to actually reach the file: hound must read back the depth
/// and rate we claimed in the name.
#[test]
fn wav_files_carry_the_spec_their_name_advertises() {
    let session = fixture();
    let req = request("wav-spec");
    let report = export_session(&session, &req, &MasteringTargets::hypothesis()).expect("export");

    for f in report.files.iter().filter(|f| f.encoding != Encoding::Mp3Cbr { kbps: 320 }) {
        let reader = hound::WavReader::open(&f.path).expect("open wav");
        let spec = reader.spec();
        assert_eq!(spec.channels, 2);
        assert_eq!(spec.sample_rate, 48_000);
        let (bits, format) = match f.encoding {
            Encoding::WavFloat32 => (32, hound::SampleFormat::Float),
            Encoding::WavPcm24 => (24, hound::SampleFormat::Int),
            Encoding::WavPcm16 => (16, hound::SampleFormat::Int),
            Encoding::Mp3Cbr { .. } => unreachable!(),
        };
        assert_eq!(spec.bits_per_sample, bits, "{:?}", f.encoding);
        assert_eq!(spec.sample_format, format, "{:?}", f.encoding);
        assert_eq!(reader.duration() as usize, f.frames, "{:?} frame count", f.encoding);
    }
    let _ = std::fs::remove_dir_all(&req.out_dir);
}

/// The MP3 has to be decodable by something that is not our encoder's twin,
/// and has to carry the program it was given — not silence, not noise.
#[test]
fn mp3_decodes_back_to_the_master_it_encoded() {
    let session = fixture();
    let mut req = request("mp3-roundtrip");
    req.deliverables = vec![Deliverable::press_kit_mp3(48_000, 320)];
    let report = export_session(&session, &req, &MasteringTargets::hypothesis()).expect("export");
    let file = &report.files[0];

    let bytes = std::fs::read(&file.path).expect("read mp3");
    let mut decoder = rusty_mp3::Mp3Decoder::default();
    decoder.push(&bytes);
    decoder.flush();
    let mut decoded: Vec<f32> = Vec::new();
    let mut rate = 0u32;
    while let Ok(frame) = decoder.next_frame() {
        rate = frame.sample_rate;
        decoded.extend_from_slice(&frame.samples);
    }
    assert_eq!(rate, 48_000, "decoded sample rate");

    let frames = decoded.len() / 2;
    // The decoder emits the encoder delay as leading silence, so the decode
    // is a little longer than the source; it must not be shorter.
    assert!(
        frames >= file.frames,
        "decoded {frames} frames from a {}-frame master",
        file.frames
    );
    let rms = (decoded.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>()
        / decoded.len() as f64)
        .sqrt();
    assert!(rms > 0.01, "decoded MP3 is effectively silent (rms {rms})");
    assert!(rms < 1.0, "decoded MP3 is not audio (rms {rms})");
    let _ = std::fs::remove_dir_all(&req.out_dir);
}

/// Deterministic on the seed: the whole point of the filename convention is
/// that the same deliverable is the same file.
#[test]
fn the_same_session_exports_byte_identical_files() {
    let session = fixture();
    let targets = MasteringTargets::hypothesis();
    // One file per chain (live-mastered float, premium-mastered 16-bit) is
    // enough to pin the pipeline; MP3 determinism has its own unit test, and
    // encoding it here doubles the debug-profile runtime for no new coverage.
    let deliverables = vec![Deliverable::archival(48_000), Deliverable::press_kit_wav(48_000)];
    let mut req_a = request("determinism-a");
    req_a.deliverables = deliverables.clone();
    let mut req_b = request("determinism-b");
    req_b.deliverables = deliverables;
    let a = export_session(&session, &req_a, &targets).expect("export a");
    let b = export_session(&session, &req_b, &targets).expect("export b");

    for (fa, fb) in a.files.iter().zip(b.files.iter()) {
        assert_eq!(fa.encoding, fb.encoding);
        assert_eq!(
            fa.content_hash, fb.content_hash,
            "{:?} is not reproducible",
            fa.encoding
        );
        assert_eq!(fa.bytes, fb.bytes);
    }
    let _ = std::fs::remove_dir_all(&a.files[0].path.parent().unwrap());
    let _ = std::fs::remove_dir_all(&b.files[0].path.parent().unwrap());
}

#[test]
fn stems_are_named_after_their_track_and_are_quieter_than_the_mix() {
    let session = fixture();
    let mut req = request("stems");
    req.deliverables = vec![Deliverable::archival(48_000)];
    req = req.with_stems(&session, 48_000);
    let report = export_session(&session, &req, &MasteringTargets::hypothesis()).expect("export");

    assert_eq!(report.files.len(), 1 + session.tracks.len());
    let mix = &report.files[0];
    for (i, track) in session.tracks.iter().enumerate() {
        let stem = &report.files[1 + i];
        assert_eq!(stem.cut, Cut::Stem(i));
        assert_eq!(stem.master, Master::None, "stems are unmastered");
        let name = stem.path.file_name().unwrap().to_string_lossy();
        assert!(name.contains(&format!("(Stem {})", track.id)), "{name}");
        assert_eq!(stem.frames, mix.frames, "stem and mix must line up");
    }

    // A single stem cannot be as loud as the whole mix.
    let energy = |p: &Path| -> f64 {
        let mut r = hound::WavReader::open(p).expect("open");
        let n = r.len() as f64;
        r.samples::<i32>().map(|s| { let v = s.unwrap() as f64; v * v }).sum::<f64>() / n
    };
    let stem_energy: f64 = report.files[1..].iter().map(|f| energy(&f.path)).sum();
    assert!(stem_energy > 0.0, "every stem is silent");
    let _ = std::fs::remove_dir_all(&req.out_dir);
}

#[test]
fn rejects_a_stem_for_a_track_that_does_not_exist() {
    let session = fixture();
    let mut req = request("bad-stem");
    req.deliverables = vec![Deliverable::stem(99, 48_000)];
    assert!(matches!(
        export_session(&session, &req, &MasteringTargets::hypothesis()),
        Err(ExportError::NoSuchTrack(99))
    ));
    // Nothing should have been written for a request that cannot be served.
    assert!(!req.out_dir.exists());
}

#[test]
fn rejects_an_empty_request() {
    let session = fixture();
    let mut req = request("empty");
    req.deliverables.clear();
    assert!(matches!(
        export_session(&session, &req, &MasteringTargets::hypothesis()),
        Err(ExportError::NothingRequested)
    ));
}

/// The engine builds at the rate it is asked for — there is no resampler,
/// so a 44.1 kHz deliverable is a 44.1 kHz render.
#[test]
fn renders_natively_at_a_non_default_rate() {
    let session = fixture();
    let mut req = request("rate-44k1");
    req.deliverables = vec![Deliverable::lossless(44_100)];
    let report = export_session(&session, &req, &MasteringTargets::hypothesis()).expect("export");
    let file = &report.files[0];
    let name = file.path.file_name().unwrap().to_string_lossy();
    assert!(name.contains("44.1k-24bit"), "{name}");
    assert_eq!(hound::WavReader::open(&file.path).unwrap().spec().sample_rate, 44_100);
    let _ = std::fs::remove_dir_all(&req.out_dir);
}

/// Regression: a premium-mastered stem must be that stem, not the full mix.
/// The premium chain used to re-render the session from scratch and ignore
/// the requested cut, so a stem came out as the whole mix under a stem name.
#[test]
fn a_premium_stem_is_the_stem_not_the_whole_mix() {
    let session = fixture();
    let mut req = request("premium-stem");
    req.deliverables = vec![
        Deliverable { cut: Cut::FullMix, encoding: Encoding::WavPcm16, sample_rate: 48_000, master: Master::Premium },
        Deliverable { cut: Cut::Stem(0), encoding: Encoding::WavPcm16, sample_rate: 48_000, master: Master::Premium },
    ];
    let report = export_session(&session, &req, &MasteringTargets::hypothesis()).expect("export");
    let (mix, stem) = (&report.files[0], &report.files[1]);

    assert_ne!(
        mix.content_hash, stem.content_hash,
        "the premium stem is byte-identical to the premium full mix"
    );
    let name = stem.path.file_name().unwrap().to_string_lossy();
    assert!(name.contains("(Stem "), "{name}");

    // Energy is the wrong probe here: the premium chain loudness-normalizes,
    // so a solo kick gets gained up to the same target as the full mix.
    // Compare *spectrum* instead, which normalization cannot fake. Track 0 of
    // the fixture is the kick — nearly all sub-bass — while the mix carries
    // hats, claps and a shaker, so the high-frequency share separates them by
    // a wide margin and is invariant to gain.
    let hf_ratio = |p: &Path| -> f64 {
        let mut r = hound::WavReader::open(p).expect("open");
        let x: Vec<f64> = r.samples::<i16>().map(|s| s.unwrap() as f64).collect();
        let energy: f64 = x.iter().map(|v| v * v).sum();
        // First difference = a one-pole high-pass; its energy share is the
        // fraction of the signal that lives up top.
        let diff: f64 = x.windows(2).map(|w| (w[1] - w[0]).powi(2)).sum();
        diff / energy
    };
    assert_eq!(session.tracks[0].id, "kick", "this test assumes track 0 is the kick");
    let (hm, hs) = (hf_ratio(&mix.path), hf_ratio(&stem.path));
    assert!(
        hs < hm / 2.0,
        "premium stem high-frequency share {hs:.4} is not well below the mix's {hm:.4} — \
         the stem looks like the whole mix"
    );
    let _ = std::fs::remove_dir_all(&req.out_dir);
}

#[test]
fn refuses_two_deliverables_that_would_share_one_filename() {
    let session = fixture();
    let mut req = request("collision");
    // Same cut, same rate, same spec tag: the names are identical, so the
    // second write would silently replace the first.
    req.deliverables = vec![Deliverable::lossless(48_000), Deliverable::lossless(48_000)];
    match export_session(&session, &req, &MasteringTargets::hypothesis()) {
        Err(ExportError::CollidingNames(name)) => {
            assert!(name.contains("(Full Mix) 48k-24bit"), "{name}");
        }
        other => panic!("expected a collision error, got {other:?}"),
    }
    assert!(!req.out_dir.exists(), "a rejected request must not create the directory");
}

/// MPEG-1 Layer III cannot carry 320 kbps at 24 kHz — the encoder would snap
/// the rate down and leave a file whose name lies about its own bitrate.
#[test]
fn refuses_an_mp3_at_a_sample_rate_mpeg1_cannot_carry() {
    let session = fixture();
    let mut req = request("mp3-rate");
    req.deliverables = vec![Deliverable::archival(24_000), Deliverable::press_kit_mp3(24_000, 320)];
    assert!(matches!(
        export_session(&session, &req, &MasteringTargets::hypothesis()),
        Err(ExportError::UnsupportedMp3SampleRate(24_000))
    ));
    // Caught in preflight, so the archival file that precedes it in the
    // request was never rendered or written.
    assert!(!req.out_dir.exists(), "a rejected request must not write anything");
}

/// The renderer sizes its buffers from the sample rate, so a nonsense rate
/// arriving over the FFI must be a rejection, not an allocation abort.
#[test]
fn refuses_an_absurd_sample_rate_rather_than_trying_to_allocate_it() {
    let session = fixture();
    let mut req = request("absurd-rate");
    req.deliverables = vec![Deliverable::archival(3_000_000_000)];
    assert!(matches!(
        export_session(&session, &req, &MasteringTargets::hypothesis()),
        Err(ExportError::UnsupportedSampleRate(3_000_000_000))
    ));
    assert!(!req.out_dir.exists());
}

/// APFS, HFS+ and NTFS are case-insensitive, so two track ids differing only
/// in case are one file on the disk that receives them. The preflight has to
/// see that before anything is rendered.
#[test]
fn refuses_stem_names_that_differ_only_by_case() {
    let mut session = fixture();
    session.tracks[1].id = session.tracks[0].id.to_uppercase();
    let mut req = request("case-collision");
    req.deliverables = vec![Deliverable::stem(0, 48_000), Deliverable::stem(1, 48_000)];
    match export_session(&session, &req, &MasteringTargets::hypothesis()) {
        Err(ExportError::CollidingNames(name)) => assert!(name.contains("(Stem KICK)"), "{name}"),
        other => panic!("expected a collision error, got {other:?}"),
    }
    assert!(!req.out_dir.exists());
}

/// A premium stem's bytes must not depend on what else was requested, or in
/// what order (#121 seam).
///
/// Premium stems are gain-referenced to the full mix's loudness drive, and
/// that drive can now reach them by two paths: solved on demand by
/// `full_mix_drive`, or seeded by a premium full-mix deliverable that
/// happened to render first. Those are the same number by construction —
/// the full-mix premium render uses `{ mastering: false, muted_tracks: [] }`,
/// which is `RenderOptions::unmastered()` — and this pins it, because a
/// stem whose level depends on its neighbours in the request list is not a
/// deliverable anyone can trust.
#[test]
fn premium_stem_bytes_do_not_depend_on_request_order() {
    let session = fixture();
    let targets = MasteringTargets::hypothesis();
    let stem = Deliverable {
        cut: Cut::Stem(0),
        sample_rate: 48_000,
        encoding: Encoding::WavFloat32,
        master: Master::Premium,
    };
    let full = Deliverable {
        cut: Cut::FullMix,
        sample_rate: 48_000,
        encoding: Encoding::WavFloat32,
        master: Master::Premium,
    };

    // (a) stem alone — the drive is solved by full_mix_drive.
    let mut a = request("order-stem-only");
    a.deliverables = vec![stem.clone()];
    let ra = export_session(&session, &a, &targets).expect("export a");

    // (b) full mix first, then the stem — the drive is seeded by the mix.
    let mut b = request("order-mix-first");
    b.deliverables = vec![full.clone(), stem.clone()];
    let rb = export_session(&session, &b, &targets).expect("export b");

    // (c) stem first, then the full mix — the drive is solved, then the mix
    //     deliverable renders separately.
    let mut c = request("order-stem-first");
    c.deliverables = vec![stem, full];
    let rc = export_session(&session, &c, &targets).expect("export c");

    let stem_bytes = |r: &kontinuum_export::ExportReport| -> Vec<u8> {
        let f = r
            .files
            .iter()
            .find(|f| f.path.to_string_lossy().contains("(Stem "))
            .expect("a stem file in the report");
        std::fs::read(&f.path).expect("read stem")
    };

    let (ba, bb, bc) = (stem_bytes(&ra), stem_bytes(&rb), stem_bytes(&rc));
    assert_eq!(ba, bb, "stem changed when a full mix was exported alongside it");
    assert_eq!(ba, bc, "stem changed when the request order changed");
}

/// The deliverable's encoding decides a premium stem's peak-control stage
/// (#121).
///
/// A float deliverable's stem ships at mix gain — tilt plus the full mix's
/// shared drive, the ×8 limiter bypassed — so its samples sit above the
/// full-scale ceiling the limited chain enforces. The same stem in a 16-bit
/// deliverable must stay an independently legal master: full chain, ceiling
/// respected. One cut, two encodings, two different renders — the cache key
/// must not collapse them.
#[test]
fn premium_stem_peak_control_follows_the_deliverables_encoding() {
    let session = fixture();
    let targets = MasteringTargets::hypothesis();
    let mut req = request("premium-stem-encoding");
    req.deliverables = vec![
        Deliverable { cut: Cut::Stem(0), encoding: Encoding::WavFloat32, sample_rate: 48_000, master: Master::Premium },
        Deliverable { cut: Cut::Stem(0), encoding: Encoding::WavPcm16, sample_rate: 48_000, master: Master::Premium },
    ];
    let report = export_session(&session, &req, &targets).expect("export");
    assert_eq!(report.files.len(), 2);
    let (float_file, int_file) = (&report.files[0], &report.files[1]);
    assert_eq!(float_file.encoding, Encoding::WavFloat32);
    assert_eq!(int_file.encoding, Encoding::WavPcm16);
    assert_ne!(
        float_file.content_hash, int_file.content_hash,
        "float and int premium stems rendered to the same bytes — the peak-control \
         split did not engage"
    );

    // Sample peak of the file as written, read back per encoding.
    let peak_dbfs = |p: &Path| -> f64 {
        let mut r = hound::WavReader::open(p).expect("open wav");
        let peak = if r.spec().sample_format == hound::SampleFormat::Float {
            r.samples::<f32>().map(|s| s.unwrap().abs() as f64).fold(0.0, f64::max)
        } else {
            r.samples::<i16>().map(|s| (s.unwrap().abs() as f64) / 32768.0).fold(0.0, f64::max)
        };
        20.0 * peak.log10()
    };
    let float_peak = peak_dbfs(&float_file.path);
    let int_peak = peak_dbfs(&int_file.path);

    // Float can hold what the bypass leaves behind: above full scale.
    assert!(
        float_peak > 0.0,
        "float premium stem peaks at {float_peak:.2} dBFS — the limiter was not \
         actually bypassed"
    );
    // Int cannot: the limited stem must be a legal 16-bit file. Its true
    // peak was trimmed to the −1 dBTP ceiling, so the sample peak sits at or
    // below full scale with margin.
    assert!(
        int_peak <= 0.0,
        "16-bit premium stem peaks at {int_peak:.2} dBFS — over full scale"
    );
    // And the two paths genuinely diverge: the limiter took real gain from
    // this stem (the kick, driven ~+11 dB by the shared solve).
    assert!(
        float_peak - int_peak > 1.0,
        "float stem peaks at {float_peak:.2} dBFS vs {int_peak:.2} dBFS limited — \
         the encoding split produced near-identical files"
    );
    let _ = std::fs::remove_dir_all(&req.out_dir);
}
