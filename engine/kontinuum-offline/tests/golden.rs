//! Golden-regression foundation (issue #11): the fixture must render
//! bit-identically across runs, hash to a pinned constant, produce audible
//! finite audio, and survive a WAV write/re-read roundtrip.

use std::path::Path;

use kontinuum_ir::Session;
use kontinuum_offline::{
    parse_session, render_session, write_wav, RenderError, RenderOutput, DEFAULT_SAMPLE_RATE,
};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/loop-4track.ir.json");

/// Pinned FNV-1a of the fixture render (48 kHz) **on the canonical pinning
/// host** (aarch64 macOS, debug profile). Any DSP or mapping change that
/// shifts a single sample breaks this — update only with intent.
/// 0x64df4912bcc88bac: per-hit variation (#52 WS3) in the voice layer.
/// 0xa448b21cf76252c6: 808-correct hat bank + resonant clap (#74), AutoMixer
/// wired with role-based ducking (#76), auto-mix gain staging (#27) — all
/// deliberate sound changes, each bit-deterministic.
/// 0x0aafebdf601d677a: the #28 mastering chain replaced the in-core tanh
/// MasterChain as the render-path master (#82) — tilt/low/glue/clipper/
/// limiter at hypothesis targets, enabled by default.
/// 0xf9fbfc9a4a7158ae: #51 harness inversion — the 12 synth voices moved
/// from kontinuum-core to plugins/kontinuum-instruments-core. Identical DSP
/// source, different codegen layout: LLVM float-contraction decisions shift
/// ULPs, which the acid resonator's feedback amplifies into decorrelated
/// (but statistically identical — RMS within 0.03 dB, crest within 0.14 dB)
/// renders. The pin is therefore CODE-LAYOUT canonical as well as
/// host-canonical; the run-to-run determinism tests remain the portable
/// contract.
/// 0x874a4f4f2ff61686: #51 rebased onto main after the #104 musicality
/// pass — the kick's sub-octave weight dropped 0.5 → 0.22, a real audio
/// change that shipped ungated (#133 verifies it against the previous
/// weight; this caveat expires when that issue closes);
/// the rebase also folded in #97's patch voices and #101's UI counter
/// saturation, neither of which touches the render path.
///
/// WHY THE PIN IS HOST-CANONICAL, NOT PORTABLE (measured 2026-08-31): the
/// render path calls transcendentals (sin/exp/powf/tanh) that resolve to the
/// platform libm, whose results vary across OS build, toolchain and opt
/// profile. Identical source produced three distinct hashes:
///   debug, local aarch64 mac (rustc 1.87.0) = 0xa448b21cf76252c6 (this pin)
///   debug, macos-14 CI runner               = 0xdae2b77da3a52786
///   release, local mac == macos-14 CI       = 0xe39eb74171839eeb
/// A single constant therefore cannot gate across environments. CI runs the
/// portable guarantees (run-to-run determinism, finite/audible, WAV
/// roundtrip) and treats the hash as informational; the exact-pin assertion
/// is opt-in via `KONTINUUM_GOLDEN_PIN_CHECK=1` on the host that owns the
/// pin. The critic ratchet (#52, statistical profiles) is the CI-level
/// regression gate for sound quality.
const GOLDEN: u64 = 0x874a_4f4f_2ff6_1686;

fn render_fixture() -> RenderOutput {
    let session = parse_session(Path::new(FIXTURE)).expect("fixture parses");
    render_session(&session, DEFAULT_SAMPLE_RATE).expect("fixture renders")
}

#[test]
fn golden_hash_matches_pinned_constant() {
    if std::env::var_os("KONTINUUM_GOLDEN_PIN_CHECK").is_none() {
        eprintln!(
            "pin check skipped: the golden hash is host/toolchain/profile-specific \
             (see GOLDEN docs); set KONTINUUM_GOLDEN_PIN_CHECK=1 on the canonical \
             pinning host to assert it"
        );
        return;
    }
    assert_eq!(render_fixture().fnv_hash(), GOLDEN);
}

#[test]
fn renders_are_bit_identical_across_fresh_runs() {
    let a = render_fixture();
    let b = render_fixture();
    assert_eq!(a.fnv_hash(), b.fnv_hash());
    assert_eq!(a.left, b.left);
    assert_eq!(a.right, b.right);
}

#[test]
fn rendered_fixture_is_finite_and_audible() {
    let out = render_fixture();
    assert_eq!(out.left.len(), out.right.len());
    assert!(out.left.iter().chain(&out.right).all(|s| s.is_finite()));
    let peak = out.left.iter().chain(&out.right).fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(peak > 0.01, "render is silent: peak {peak}");
}

#[test]
fn wav_roundtrip_preserves_spec_and_length() {
    let out = render_fixture();
    let dir = std::env::temp_dir().join("kontinuum-offline-tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(format!("golden-{}.wav", std::process::id()));
    write_wav(&path, &out).expect("wav write");

    let mut reader = hound::WavReader::open(&path).expect("wav reopen");
    let spec = reader.spec();
    assert_eq!(spec.channels, 2);
    assert_eq!(spec.sample_rate, DEFAULT_SAMPLE_RATE);
    assert_eq!(spec.sample_format, hound::SampleFormat::Float);
    assert_eq!(spec.bits_per_sample, 32);
    assert_eq!(reader.samples::<f32>().count(), (out.left.len() + out.right.len()) as usize);
    drop(reader);
    std::fs::remove_file(&path).expect("cleanup");
}

#[test]
fn sessions_over_the_bar_ceiling_are_refused() {
    let doc = r#"{
        "version": 1, "seed": 1, "tempo_lane": [[0, 120.0]],
        "sections": [{"id": "a", "bars": 2049, "energy_curve": [0.5],
            "pattern_bindings": {"k": {"generator": "euclidean", "k": 4, "n": 16}}}],
        "tracks": [{"id": "k", "role": "kick", "instrument": {"kind": "kick"}}]
    }"#;
    let session: Session = serde_json::from_str(doc).expect("parse");
    let err = render_session(&session, DEFAULT_SAMPLE_RATE).expect_err("must refuse");
    assert!(matches!(err, RenderError::TooLong), "got: {err:?}");
}
