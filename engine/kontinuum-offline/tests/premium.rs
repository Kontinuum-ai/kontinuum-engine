//! Premium render + A/B regression checks (#28/#32): bit-identical
//! double renders, a stable golden content hash, byte-stable artifacts,
//! and a spec-correct 16-bit WAV export.

use std::path::Path;

use kontinuum_ir::Session;
use kontinuum_mastering::targets::MasteringTargets;
use kontinuum_offline::{
    parse_session, premium_golden_hash, premium_render, render_ab, write_ab, write_wav16,
    PremiumRender, DEFAULT_SAMPLE_RATE,
};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/loop-4track.ir.json");

fn fixture_session() -> Session {
    parse_session(Path::new(FIXTURE)).expect("fixture parses")
}

fn premium_fixture() -> PremiumRender {
    premium_render(&fixture_session(), DEFAULT_SAMPLE_RATE, &MasteringTargets::hypothesis())
        .expect("premium render")
}

#[test]
fn premium_double_render_is_bit_identical() {
    let a = premium_fixture();
    let b = premium_fixture();
    assert_eq!(a.master.left, b.master.left, "float master drifted");
    assert_eq!(a.master.right, b.master.right, "float master drifted");
    assert_eq!(a.dithered, b.dithered, "16-bit payload drifted");
    assert_eq!(a.content_hash(), b.content_hash());
}

#[test]
fn premium_golden_hash_is_stable_in_process() {
    let a = premium_golden_hash().expect("golden render a");
    let b = premium_golden_hash().expect("golden render b");
    assert_eq!(a, b, "golden premium hash must be stable across runs");
}

#[test]
fn premium_wav16_roundtrip_preserves_spec_and_length() {
    let render = premium_fixture();
    let dir = std::env::temp_dir().join("kontinuum-offline-premium-tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(format!("premium-{}.wav", std::process::id()));
    write_wav16(&path, &render.dithered, render.master.sample_rate).expect("wav write");

    let mut reader = hound::WavReader::open(&path).expect("wav reopen");
    let spec = reader.spec();
    assert_eq!(spec.channels, 2);
    assert_eq!(spec.sample_rate, DEFAULT_SAMPLE_RATE);
    assert_eq!(spec.bits_per_sample, 16);
    assert_eq!(spec.sample_format, hound::SampleFormat::Int);
    assert_eq!(
        reader.samples::<i16>().count(),
        (render.dithered.left.len() + render.dithered.right.len()) as usize
    );
    drop(reader);
    std::fs::remove_file(&path).expect("cleanup");
}

/// True-peak ceiling: the one absolute guarantee this chain owes.
///
/// This test used to also assert loudness, against an expectation it
/// computed as `target + ceiling_trim_db` — i.e. it derived the answer
/// from whatever the chain had just done, so it passed no matter how far
/// under target the export landed. That half is gone; the loudness
/// invariants live in the tests below, which assert against fixed
/// references rather than the chain's own output.
#[test]
fn premium_master_respects_the_true_peak_ceiling() {
    let render = premium_fixture();
    let targets = MasteringTargets::hypothesis();
    let tp = kontinuum_mastering::offline::true_peak_dbfs(
        &render.master.left,
        &render.master.right,
    );
    // Platform libm drift moves the normalized convergence point by ~1e-6 dB
    // across hosts/profiles (same root cause as the golden-pin host-canonical
    // note): the ceiling assertion carries a 1e-3 dB tolerance — far below
    // audibility, far above drift.
    assert!(
        tp <= targets.ceiling_dbtp + 1e-3,
        "premium master true peak {tp} dBTP"
    );
}

#[test]
fn ab_artifacts_are_byte_stable_across_writes() {
    let targets = MasteringTargets::hypothesis();
    let dir = std::env::temp_dir().join("kontinuum-offline-ab-tests");
    let first = dir.join("a");
    let second = dir.join("b");
    let pair_a = render_ab(&fixture_session(), DEFAULT_SAMPLE_RATE, &targets, true)
        .expect("ab render a");
    let pair_b = render_ab(&fixture_session(), DEFAULT_SAMPLE_RATE, &targets, true)
        .expect("ab render b");
    assert_eq!(pair_a.manifest, pair_b.manifest, "manifest must be identical");
    write_ab(&first, &pair_a).expect("write a");
    write_ab(&second, &pair_b).expect("write b");
    for name in ["mix.wav", "master.wav", "manifest.json"] {
        let a = std::fs::read(first.join(name)).expect("read a");
        let b = std::fs::read(second.join(name)).expect("read b");
        assert_eq!(a, b, "{name} bytes drifted");
    }
}

/// The test that #98 needed and did not have (#111).
///
/// Every other premium assertion here is *relational* — "respects the
/// ceiling", "normalizes as far as it reaches" — and every one of them
/// passes on a double-mastered chain, because they derive the expectation
/// from whatever the chain did.
///
/// `ceiling_trim_db` is the chain conceding that the true-peak ceiling
/// stopped it reaching the loudness it aimed for. A healthy chain concedes
/// almost nothing, because the loudness move happens *into* the limiter
/// and the limiter hands back a program already sitting on the ceiling.
/// A chain fed an already-limited mix has no crest factor left to trade,
/// so it asks for gain and gives every dB of it back: on main the fixture
/// conceded 9.13 dB and every test above still passed.
#[test]
fn premium_chain_is_not_starved_of_headroom() {
    let render = premium_fixture();
    assert!(
        render.ceiling_trim_db > -1.0,
        "ceiling trim {:.2} dB after {:.2} dB of loudness drive: the chain is \
         giving back what it asked for, which is what mastering an already- \
         mastered mix looks like",
        render.ceiling_trim_db,
        render.drive_db,
    );
}

/// The premium chain's input must be the raw mix, not the graph master.
/// Pins the seam #98 moved directly, so a future move of the mastering
/// chain shows up here rather than as a quiet 9 dB in the exports.
#[test]
fn premium_input_is_the_unmastered_mix() {
    use kontinuum_offline::{render_session, render_session_with, RenderOptions};
    let s = fixture_session();
    let raw = render_session_with(&s, DEFAULT_SAMPLE_RATE, &RenderOptions::unmastered())
        .expect("mix");
    let mastered = render_session(&s, DEFAULT_SAMPLE_RATE).expect("mastered");
    assert_ne!(
        raw.left, mastered.left,
        "the unmastered and mastered renders are identical — the graph \
         master is not actually being bypassed, so every downstream chain \
         double-masters"
    );
    let raw_tp = kontinuum_mastering::offline::true_peak_dbfs(&raw.left, &raw.right);
    let mastered_tp =
        kontinuum_mastering::offline::true_peak_dbfs(&mastered.left, &mastered.right);
    assert!(
        raw_tp > mastered_tp,
        "raw mix peaks at {raw_tp} dBTP, mastered at {mastered_tp} dBTP: the \
         raw mix should be the hotter, unlimited one"
    );
}

/// A premium export is the high-quality version of the same session's
/// real-time master, so it must be at least as loud — with margin. Under
/// the #111 double-master the premium export landed 0.36 dB from the RT
/// render, i.e. the entire premium chain was buying nothing.
#[test]
fn premium_master_is_clearly_louder_than_the_realtime_render() {
    use kontinuum_mastering::offline::integrated_lufs;
    use kontinuum_offline::render_session;
    let s = fixture_session();
    let rt = render_session(&s, DEFAULT_SAMPLE_RATE).expect("rt render");
    let rt_lufs = integrated_lufs(&rt.left, &rt.right, DEFAULT_SAMPLE_RATE);
    let premium_lufs = premium_fixture().measurement.integrated_lufs;
    assert!(
        premium_lufs - rt_lufs >= 3.0,
        "premium {premium_lufs:.2} LUFS vs real-time {rt_lufs:.2} LUFS: the \
         premium chain is buying less than 3 LU over the live path, so it is \
         not reaching the mastering targets it exists to reach"
    );
}

/// The A/B harness compares a mix against a master. Both legs went
/// through `render_session` before #111, which made the "mix" leg a master
/// and the "master" leg a double-master — two versions of the same
/// processing, blind-tested against each other.
#[test]
fn ab_mix_leg_is_genuinely_unmastered() {
    use kontinuum_mastering::offline::true_peak_dbfs;
    use kontinuum_offline::{render_session_with, RenderOptions};
    let s = fixture_session();
    let targets = MasteringTargets::hypothesis();
    let pair = render_ab(&s, DEFAULT_SAMPLE_RATE, &targets, true).expect("ab render");
    let raw = render_session_with(&s, DEFAULT_SAMPLE_RATE, &RenderOptions::unmastered())
        .expect("mix");
    // The pair is loudness-matched, so compare shape, not level: the
    // unmastered leg keeps the raw mix's crest factor, the master does not.
    let crest = |l: &[f32], r: &[f32]| {
        let rms = (l.iter().chain(r.iter()).map(|s| (s * s) as f64).sum::<f64>()
            / (l.len() + r.len()) as f64)
            .sqrt();
        true_peak_dbfs(l, r) - 20.0 * rms.log10()
    };
    let raw_crest = crest(&raw.left, &raw.right);
    let mix_crest = crest(&pair.mix.left, &pair.mix.right);
    let master_crest = crest(&pair.master.left, &pair.master.right);
    assert!(
        (mix_crest - raw_crest).abs() < 0.5,
        "A/B mix leg crest {mix_crest:.2} dB vs raw mix {raw_crest:.2} dB: the \
         'unmastered' stimulus has been through a master"
    );
    assert!(
        master_crest < mix_crest - 1.0,
        "A/B master crest {master_crest:.2} dB vs mix {mix_crest:.2} dB: the \
         two stimuli are not meaningfully different processing"
    );
}

/// A targets profile with a ceiling other than −1 dBTP must be honoured.
///
/// `TruePeakLimiter` enforces its own fixed `CEILING_DBTP` (−1 dBTP) and
/// never reads `targets.ceiling_dbtp`. When the loudness solve measured
/// the limiter's raw output, a profile asking for −3 dBTP converged
/// against −1 and then lost the difference to a trim the solve had never
/// seen — the shipped hypothesis profile happens to use −1.0, so the
/// fixture could not expose it.
#[test]
fn premium_honours_a_non_default_ceiling() {
    let mut targets = MasteringTargets::hypothesis();
    targets.ceiling_dbtp = -3.0;
    let render = premium_render(&fixture_session(), DEFAULT_SAMPLE_RATE, &targets)
        .expect("premium render at -3 dBTP");
    let tp = kontinuum_mastering::offline::true_peak_dbfs(
        &render.master.left,
        &render.master.right,
    );
    assert!(
        tp <= targets.ceiling_dbtp + 1e-3,
        "true peak {tp} dBTP exceeds the requested -3.0 dBTP ceiling"
    );
    // And the solve must have accounted for it rather than converging at
    // -1 dBTP and paying the 2 dB afterwards.
    assert!(
        render.ceiling_trim_db > -1.0,
        "ceiling trim {:.2} dB: the solve converged against the limiter's \
         own ceiling, not the requested one",
        render.ceiling_trim_db
    );
}

/// The −8.5 LUFS target must actually be reachable (#115).
///
/// A pure true-peak limiter asymptotes at −9.4 LUFS on this fixture: past
/// ~16 dB of drive, extra gain converts into gain reduction and the target
/// stays ~0.9 dB out of reach. With the saturation stage ahead of the
/// limiter, the issue's drive sweep must land inside the target's own
/// integrated tolerance (±0.5 LU) across 16–24 dB of drive — asserted
/// against the fixed `MasteringTargets::hypothesis()` numbers, not
/// against whatever the chain produced — with the ceiling still enforced.
#[test]
fn premium_reaches_the_loudness_target_across_the_drive_sweep() {
    use kontinuum_mastering::offline::{integrated_lufs, true_peak_dbfs};
    use kontinuum_offline::{premium_master_with_drive, render_session_with, PremiumDrive, RenderOptions};
    let s = fixture_session();
    let targets = MasteringTargets::hypothesis();
    let mix = render_session_with(&s, DEFAULT_SAMPLE_RATE, &RenderOptions::unmastered())
        .expect("mix");
    let lo = targets.integrated_lufs - targets.tolerances.integrated_lufs;
    let hi = targets.integrated_lufs + targets.tolerances.integrated_lufs;

    let mut results = Vec::new();
    let mut off_target = Vec::new();
    for drive in [16.0, 18.0, 20.0, 22.0, 24.0] {
        let out = premium_master_with_drive(mix.clone(), s.seed, &targets, PremiumDrive::Fixed(drive));
        let lufs = integrated_lufs(&out.master.left, &out.master.right, DEFAULT_SAMPLE_RATE);
        let tp = true_peak_dbfs(&out.master.left, &out.master.right);
        results.push(format!(
            "drive {drive:4.1} dB -> {lufs:.2} LUFS, {tp:+.2} dBTP (trim {:.2} dB)",
            out.ceiling_trim_db
        ));
        if !(lo..=hi).contains(&lufs) {
            off_target.push(format!("drive {drive:4.1} dB: {lufs:.2} LUFS"));
        }
        assert!(
            tp <= targets.ceiling_dbtp + 1e-3,
            "drive {drive:4.1} dB true peak {tp} dBTP exceeds the ceiling"
        );
    }
    eprintln!("premium drive sweep:\n{}", results.join("\n"));
    assert!(
        off_target.is_empty(),
        "drive sweep misses the {lo:.1}..{hi:.1} LUFS window:\n{}",
        off_target.join("\n")
    );
}

/// The solved premium chain — what a delivery actually runs — must land
/// inside the targets' own integrated tolerance (#115), asserted against
/// the fixed `MasteringTargets::hypothesis()` numbers. This is the
/// assertion #111 deliberately removed a self-referential version of:
/// the reference here is the targets file, not the chain's behavior.
///
/// The loudness is measured off the returned master samples, not read
/// from `render.measurement`: the struct is the chain's own
/// bookkeeping, and a refactor that moves the measurement to an earlier
/// stage (pre-limiter, pre-trim) would keep it agreeing with itself
/// while the export drifts. The bookkeeping is instead pinned to the
/// audio it claims to describe.
#[test]
fn premium_render_lands_on_the_loudness_target() {
    use kontinuum_mastering::offline::integrated_lufs;
    let render = premium_fixture();
    let targets = MasteringTargets::hypothesis();
    let lufs = integrated_lufs(
        &render.master.left,
        &render.master.right,
        DEFAULT_SAMPLE_RATE,
    );
    let tol = targets.tolerances.integrated_lufs;
    assert!(
        (targets.integrated_lufs - lufs).abs() <= tol,
        "premium render landed at {lufs:.2} LUFS, target {} ± {tol}",
        targets.integrated_lufs
    );
    assert!(
        (render.measurement.integrated_lufs - lufs).abs() < 1e-6,
        "the chain's own measurement ({}) does not describe the samples it \
         returned ({lufs:.2} LUFS) — the measurement point moved",
        render.measurement.integrated_lufs
    );
}

/// Premium stems must be gain-referenced to the mix, not solved on their own.
///
/// `premium_master` solves a loudness drive from whatever it is handed, so
/// mastering each stem with it aims every stem at the same integrated
/// loudness — the mix balance, discarded. On this fixture an independent
/// solve moves track 3 by +14.5 dB and track 1 by +6.1 dB; the two sit
/// 0.65 dB apart in the mix. `PremiumDrive::Fixed` is the seam that stops
/// it (#121 tracks the residual, which is per-stem limiting).
#[test]
fn premium_stems_share_the_mix_drive_rather_than_solving_their_own() {
    use kontinuum_offline::{
        premium_master, premium_master_with_drive, render_session_with, PremiumDrive,
        RenderOptions,
    };
    let s = fixture_session();
    let targets = MasteringTargets::hypothesis();
    let tracks = s.tracks.len();
    let mix = render_session_with(&s, DEFAULT_SAMPLE_RATE, &RenderOptions::unmastered())
        .expect("mix");
    let mix_drive = premium_master(mix, s.seed, &targets).drive_db;

    let mut solo_drives = Vec::new();
    for i in 0..tracks {
        let stem = render_session_with(&s, DEFAULT_SAMPLE_RATE, &RenderOptions::stem(tracks, i))
            .expect("stem");
        // Referenced to the mix: the drive is the mix's, every time.
        let shared = premium_master_with_drive(
            stem.clone(),
            s.seed,
            &targets,
            PremiumDrive::Fixed(mix_drive),
        );
        assert!(
            (shared.drive_db - mix_drive).abs() < 1e-9,
            "stem {i} drive {} != mix drive {mix_drive}",
            shared.drive_db
        );
        solo_drives.push(premium_master(stem, s.seed, &targets).drive_db);
    }

    // And the thing being prevented is real: solved independently, the
    // per-stem drives fan out by many dB.
    let lo = solo_drives.iter().cloned().fold(f64::INFINITY, f64::min);
    let hi = solo_drives.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    assert!(
        hi - lo > 3.0,
        "independent per-stem drives spread only {:.2} dB ({lo:.2}..{hi:.2}); if this \
         is now flat the fixture no longer demonstrates the hazard and this test is \
         not guarding anything",
        hi - lo
    );
}

/// A float premium stem keeps the mix gain the per-stem limiter used to
/// take (#121).
///
/// The shared drive (#118) fixed what goes *in* to every stem's chain; the
/// ×8 limiter and ceiling trim still re-shaped what comes *out*, engaging
/// by crest factor. Measured on the fixture: the same drive landed as
/// +6.73 dB of applied gain on track 0 and +2.64 dB on track 1. With peak
/// control bypassed the applied gain is the drive itself, so the spread
/// collapses to measurement noise — that is the balance coming back.
#[test]
fn premium_float_stems_keep_the_gain_the_limiter_used_to_take() {
    use kontinuum_mastering::offline::{integrated_lufs, true_peak_dbfs};
    use kontinuum_offline::{
        premium_master, premium_master_peaks_bypassed, premium_master_with_drive,
        render_session_with, PremiumDrive, RenderOptions,
    };
    let s = fixture_session();
    let targets = MasteringTargets::hypothesis();
    let tracks = s.tracks.len();
    let mix = render_session_with(&s, DEFAULT_SAMPLE_RATE, &RenderOptions::unmastered())
        .expect("mix");
    let drive = premium_master(mix, s.seed, &targets).drive_db;

    let mut free_lo = f64::INFINITY;
    let mut free_hi = f64::NEG_INFINITY;
    let mut limited_lo = f64::INFINITY;
    let mut limited_hi = f64::NEG_INFINITY;
    let mut hottest_free_tp = f64::NEG_INFINITY;
    for i in 0..tracks {
        let stem = render_session_with(&s, DEFAULT_SAMPLE_RATE, &RenderOptions::stem(tracks, i))
            .expect("stem");
        let raw_lufs = integrated_lufs(&stem.left, &stem.right, DEFAULT_SAMPLE_RATE);
        let applied = |out: &PremiumRender| {
            integrated_lufs(&out.master.left, &out.master.right, DEFAULT_SAMPLE_RATE) - raw_lufs
        };
        let free = premium_master_peaks_bypassed(stem.clone(), s.seed, &targets, drive);
        let limited =
            premium_master_with_drive(stem, s.seed, &targets, PremiumDrive::Fixed(drive));
        assert_eq!(free.drive_db, drive, "stem {i} must receive the mix's drive");
        assert_eq!(free.ceiling_trim_db, 0.0, "no peak control ran, so no trim");

        let free_gain = applied(&free);
        let limited_gain = applied(&limited);
        free_lo = free_lo.min(free_gain);
        free_hi = free_hi.max(free_gain);
        limited_lo = limited_lo.min(limited_gain);
        limited_hi = limited_hi.max(limited_gain);
        hottest_free_tp = hottest_free_tp.max(true_peak_dbfs(
            &free.master.left,
            &free.master.right,
        ));
    }

    // Bypassed peak control is pure gain (the shipped hypothesis tilts by
    // 0 dB), so every stem applies the same gain to within the loudness
    // measurement's precision.
    assert!(
        free_hi - free_lo < 0.1,
        "float stems still spread {:.2} dB of applied gain ({free_lo:.2}..{free_hi:.2}): \
         the per-stem peak stage is not actually bypassed",
        free_hi - free_lo
    );
    // The hazard is real: the limited path's spread is many dB. If this ever
    // goes flat the fixture no longer demonstrates what #121 fixed.
    assert!(
        limited_hi - limited_lo > 3.0,
        "limited stems spread only {:.2} dB ({limited_lo:.2}..{limited_hi:.2}); the \
         fixture no longer demonstrates the crest-factor hazard",
        limited_hi - limited_lo
    );
    // And the point of float: with peak control off, a stem really does
    // leave the 0 dBFS neighborhood the limiter used to enforce.
    assert!(
        hottest_free_tp > targets.ceiling_dbtp,
        "no float stem exceeded the ceiling ({hottest_free_tp:.2} dBTP), so the \
         bypass is not being exercised by this fixture"
    );
}

/// A 16/24-bit deliverable cannot hold samples above full scale, so its
/// premium stems keep the full chain (#121) and stay independently legal:
/// every stem respects the targets' true-peak ceiling.
#[test]
fn premium_int_stems_respect_the_true_peak_ceiling() {
    use kontinuum_mastering::offline::true_peak_dbfs;
    use kontinuum_offline::{
        premium_master, premium_master_with_drive, render_session_with, PremiumDrive,
        RenderOptions,
    };
    let s = fixture_session();
    let targets = MasteringTargets::hypothesis();
    let tracks = s.tracks.len();
    let mix = render_session_with(&s, DEFAULT_SAMPLE_RATE, &RenderOptions::unmastered())
        .expect("mix");
    let drive = premium_master(mix, s.seed, &targets).drive_db;

    for i in 0..tracks {
        let stem = render_session_with(&s, DEFAULT_SAMPLE_RATE, &RenderOptions::stem(tracks, i))
            .expect("stem");
        let out = premium_master_with_drive(stem, s.seed, &targets, PremiumDrive::Fixed(drive));
        assert!(
            out.ceiling_trim_db <= 0.0,
            "stem {i} ceiling trim {} dB must never be gain",
            out.ceiling_trim_db
        );
        let tp = true_peak_dbfs(&out.master.left, &out.master.right);
        assert!(
            tp <= targets.ceiling_dbtp + 1e-3,
            "limited stem {i} true peak {tp} dBTP exceeds the {:.1} dBTP ceiling",
            targets.ceiling_dbtp
        );
    }
}
