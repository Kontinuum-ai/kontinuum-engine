//! Premium offline render (#28): the same session log through a
//! higher-quality chain than the real-time path. Used for
//! exports/bookmarks (#31 share feature) and the A/B harness (#32).
//!
//! Chain (offline latency is free; quality is the point):
//! 1. **Linear-phase master EQ** — the RT tilt move rendered by
//!    FFT-partitioned convolution ([`crate::fft`]): identical shelf
//!    magnitudes, perfectly linear phase.
//! 2. **Loudness drive** — the gain that aims the program at the targets'
//!    integrated loudness is applied *before* peak control, solved against
//!    the limiter's own gain reduction ([`drive_into_limiter`]).
//! 3. **Saturation** — mastering's [`SoftClipper`] (the RT chain's stage
//!    4) ahead of peak control (#115): the last ~1 dB is traded for
//!    harmonic content instead of pure limiter gain reduction, which is
//!    what makes the loudness target reachable at all.
//! 4. **×8 oversampled true-peak limiting** — the signal is rate-
//!    converted ×2 (65-tap windowed-sinc pair below), then mastering's
//!    [`TruePeakLimiter`] — whose internal stage is ×4 — runs at the 2×
//!    rate: 2 × 4 = [`PREMIUM_OVERSAMPLE`] inter-sample estimate, double
//!    the RT chain's ×4. A final ×4 guard at the base rate enforces the
//!    ceiling on the decimator's own output (#112 — the ×2 down-converter
//!    reconstructs peaks the 2×-rate limiter never saw).
//! 5. **Residual loudness normalization** to the targets file
//!    ([`normalize_to_target`], BS.1770 measurement + true-peak ceiling) —
//!    a trim now, not the loudness move itself.
//! 6. **TPDF dither to 16-bit** ([`dither_tpdf_16`]), seeded from the
//!    session's IR master seed — no hidden entropy anywhere, so the same
//!    session + targets always produce a bit-identical WAV.
//!
//! Everything loudness/dither is mastering's own math, reused verbatim;
//! the only DSP added here is the ×2 rate converter that mastering's ×4
//! pair cannot express.
//!
//! The input is the **unmastered** mix: since #98 put the real-time
//! mastering chain inside `AudioGraph`, a plain `render_session` is already
//! mastered, and running the premium chain over that is a double-master.
//! `premium_render` bypasses the graph chain for exactly this reason.

use std::path::Path;

use kontinuum_core::fnv1a64;
use kontinuum_ir::Session;
use kontinuum_mastering::clipper::{CLIP_CEILING_DB, SoftClipper};
use kontinuum_mastering::limiter::TruePeakLimiter;
use kontinuum_mastering::offline::{
    dither_tpdf_16, integrated_lufs, measure_loudness, normalize_to_target, true_peak_dbfs,
    Dithered16, LoudnessMeasurement,
};
use kontinuum_mastering::targets::MasteringTargets;

use crate::fft::LinearPhaseTiltEq;
use crate::{
    parse_session, render_session_with, RenderError, RenderOptions, RenderOutput,
    DEFAULT_SAMPLE_RATE,
};

/// Effective inter-sample oversampling of the premium limiter stage.
pub const PREMIUM_OVERSAMPLE: usize = 8;

/// Silent tail (base-rate frames) appended before the ×8 limiter stage so
/// its lookahead/release and the decimator history drain — keeps the last
/// output samples fully valid.
const LIMIT_PAD_BASE: usize = 256;

/// Drive-solve passes: how many times the chain re-aims the pre-limiter
/// gain at the loudness target. The limiter's gain reduction is itself a
/// function of the drive, so one pass lands short; the secant steps
/// ([`drive_into_limiter`]) converge on the saturating curve in a handful.
const PREMIUM_DRIVE_PASSES: usize = 8;
/// Drive-solve stopping band (LU). Tighter than the targets' own
/// integrated tolerance, so the solve is never the binding constraint.
const DRIVE_TOLERANCE_LU: f64 = 0.05;
/// Below this loudness-per-drive slope (LU/dB) the curve counts as flat
/// and the solve jumps for the clamp instead of dividing by the slope.
const SECANT_MIN_SLOPE: f64 = 0.02;
/// Bound on the pre-limiter drive, matching `normalize_to_target`'s own
/// ±24 dB clamp — a pathological (near-silent) mix must not ask for
/// unbounded gain.
const MAX_DRIVE_DB: f64 = 24.0;

/// A finished premium render: float master, 16-bit export payload, and
/// the loudness bookkeeping that produced them.
#[derive(Clone, Debug)]
pub struct PremiumRender {
    /// Premium-mastered float program (normalized to the targets file).
    pub master: RenderOutput,
    /// TPDF-dithered 16-bit export payload (what `write_wav16` stores).
    pub dithered: Dithered16,
    /// BS.1770 measurement of the final float master.
    pub measurement: LoudnessMeasurement,
    /// Loudness drive applied ahead of the limiter (dB) — the gain that
    /// aims the program at the targets' integrated loudness.
    ///
    /// Renamed from `gain_db`, which meant the *normalizer's* gain before
    /// the loudness move went upstream of the limiter. Reusing that name
    /// for a different quantity would have read as the old one.
    pub drive_db: f64,
    /// Extra peak trim to respect the targets' ceiling (dB, ≤ 0).
    pub ceiling_trim_db: f64,
}

impl PremiumRender {
    /// FNV-1a over the final 16-bit payload — the content fingerprint of
    /// a premium export, used by the golden regression and the CLI.
    pub fn content_hash(&self) -> u64 {
        let mut bytes = Vec::with_capacity(self.dithered.left.len() * 4);
        for (l, r) in self.dithered.left.iter().zip(self.dithered.right.iter()) {
            bytes.extend_from_slice(&l.to_le_bytes());
            bytes.extend_from_slice(&r.to_le_bytes());
        }
        fnv1a64(&bytes)
    }
}

/// 65-tap windowed-sinc ×2 converter (Blackman window, cutoff at the
/// base Nyquist = 0.25 of the 2× rate). Deterministic, f64 taps, f32
/// signal path — mirrors the style of mastering's `oversample` module.
const RESAMPLE_TAPS: usize = 65;
/// Group delay at the 2× rate: (taps − 1) / 2.
const RESAMPLE_DELAY_2X: usize = (RESAMPLE_TAPS - 1) / 2;
/// Group delay at the base rate: half of [`RESAMPLE_DELAY_2X`].
const RESAMPLE_DELAY_BASE: usize = RESAMPLE_DELAY_2X / 2;

fn resample_taps() -> [f64; RESAMPLE_TAPS] {
    let m = (RESAMPLE_TAPS - 1) as f64 / 2.0;
    let fc = 0.25;
    let mut taps = [0.0f64; RESAMPLE_TAPS];
    for (t, tap) in taps.iter_mut().enumerate() {
        let x = t as f64 - m;
        let s = if x.abs() < 1e-12 {
            2.0 * fc
        } else {
            (std::f64::consts::TAU * fc * x).sin() / (std::f64::consts::PI * x)
        };
        let ph = std::f64::consts::TAU * t as f64 / (RESAMPLE_TAPS - 1) as f64;
        let w = 0.42 - 0.5 * ph.cos() + 0.08 * (2.0 * ph).cos();
        *tap = s * w;
    }
    taps
}

/// Interpolate ×2 (zero-stuff by 2, windowed-sinc, per-phase unity DC).
/// Group delay: [`RESAMPLE_DELAY_2X`] at the 2× rate.
fn upsample2(x: &[f32]) -> Vec<f32> {
    let taps = resample_taps();
    let even_sum: f64 = (0..RESAMPLE_DELAY_2X + 1).map(|j| taps[2 * j]).sum();
    let odd_sum: f64 = (0..RESAMPLE_DELAY_2X).map(|j| taps[2 * j + 1]).sum();
    let mut out = vec![0.0f32; 2 * x.len()];
    let mut hist = [0.0f64; RESAMPLE_DELAY_2X + 1]; // x[m−32..=m], newest first
    for (m, slot) in out.chunks_mut(2).enumerate() {
        hist.copy_within(0..RESAMPLE_DELAY_2X, 1);
        hist[0] = x[m] as f64;
        let mut even = 0.0f64;
        let mut odd = 0.0f64;
        for (j, &h) in hist.iter().enumerate() {
            even += taps[2 * j] * h;
            if 2 * j + 1 < RESAMPLE_TAPS {
                odd += taps[2 * j + 1] * h;
            }
        }
        slot[0] = (even / even_sum) as f32;
        slot[1] = (odd / odd_sum) as f32;
    }
    out
}

/// Decimate ×2 (windowed-sinc anti-alias at the base Nyquist, unity DC).
/// Group delay: [`RESAMPLE_DELAY_2X`] at the 2× rate.
fn downsample2(u: &[f32]) -> Vec<f32> {
    let taps = resample_taps();
    let sum: f64 = taps.iter().sum();
    let n = u.len() / 2;
    let mut out = vec![0.0f32; n];
    let mut hist = [0.0f64; RESAMPLE_TAPS]; // u[2m − t], newest first
    for (m, slot) in out.iter_mut().enumerate() {
        // Evaluate on the even sub-sample so hist[t] == u[2m − t] and the
        // symmetric taps' center (32) lands exactly on the 2× grid.
        hist.copy_within(0..RESAMPLE_TAPS - 1, 1);
        hist[0] = u[2 * m] as f64;
        let acc: f64 = hist.iter().zip(taps.iter()).map(|(s, t)| s * t).sum::<f64>() / sum;
        *slot = acc as f32;
        // Queue the odd sub-sample behind it for the next output.
        hist.copy_within(0..RESAMPLE_TAPS - 1, 1);
        hist[0] = u[2 * m + 1] as f64;
    }
    out
}

/// Head silence (base-rate frames) the ×8 stage inserts: up-converter
/// delay + the limiter's own (2× rate) latency + down-converter delay.
fn limit_stage_delay(limiter_latency_2x: usize) -> usize {
    RESAMPLE_DELAY_BASE + limiter_latency_2x.div_ceil(2) + RESAMPLE_DELAY_BASE
}

/// ×8 oversampled true-peak limiting: ×2 up → [`TruePeakLimiter`] at the
/// 2× rate (internal ×4) → ×2 down → final [`TruePeakLimiter`] guard at
/// the base rate, latency-compensated to the input.
///
/// The trailing guard is the ceiling guarantee (#112). The ×2 decimator
/// after the 2×-rate limiter reconstructs inter-sample peaks the limiter
/// never saw: measured on the fixture mix across the 0→24 dB drive sweep,
/// the decimated output sits at −0.76 dBTP at every drive 0–20 dB (0.39 dB
/// over the limiter's −1.15 dBTP working point) and +0.11 dBTP at 24 dB
/// (1.26 dB over) — a fixed guard band sized for that worst case would tax
/// every master 1.3 dB of loudness, so the overshoot is enforced away
/// where it appears instead, on the base-rate signal the guarantee is
/// actually about. The guard's ×4 estimate is the same view
/// `true_peak_dbfs` measures, so what it enforces is what gets tested.
fn limit_x8(left: &[f32], right: &[f32], sample_rate: u32) -> (Vec<f32>, Vec<f32>) {
    let n = left.len().min(right.len());
    let mut up_l = upsample2(&left[..n]);
    let mut up_r = upsample2(&right[..n]);
    let tail = 2 * LIMIT_PAD_BASE;
    up_l.resize(up_l.len() + tail, 0.0);
    up_r.resize(up_r.len() + tail, 0.0);

    let mut lim = TruePeakLimiter::new(2 * sample_rate);
    let mut lim_l = Vec::with_capacity(up_l.len());
    let mut lim_r = Vec::with_capacity(up_r.len());
    for i in 0..up_l.len() {
        let (l, r) = lim.tick(up_l[i], up_r[i]);
        lim_l.push(l);
        lim_r.push(r);
    }

    let down_l = downsample2(&lim_l);
    let down_r = downsample2(&lim_r);

    let mut guard = TruePeakLimiter::new(sample_rate);
    let mut guarded_l = Vec::with_capacity(down_l.len());
    let mut guarded_r = Vec::with_capacity(down_r.len());
    for i in 0..down_l.len() {
        let (l, r) = guard.tick(down_l[i], down_r[i]);
        guarded_l.push(l);
        guarded_r.push(r);
    }

    // `latency_frames()` over-reports a TruePeakLimiter by one frame: the
    // ×4 oversampler's UP and DOWN latency constants each round the
    // filters' true 15.5-frame group delay up to 16. One limiter's
    // half-frame residual washes out in the decimator's group delay; two
    // in series (limiter + guard) lose a whole frame, so compensate the
    // true total.
    let delay =
        limit_stage_delay(lim.latency_frames()) + guard.latency_frames() - 1;
    let take = n.min(guarded_l.len().saturating_sub(delay));
    let mut out_l = vec![0.0f32; n];
    let mut out_r = vec![0.0f32; n];
    out_l[..take].copy_from_slice(&guarded_l[delay..delay + take]);
    out_r[..take].copy_from_slice(&guarded_r[delay..delay + take]);
    (out_l, out_r)
}

/// Render a validated session through the premium chain: mix →
/// linear-phase tilt EQ → loudness drive → ×8 limiter → residual
/// normalization to `targets` → TPDF dither seeded from the session's IR
/// master seed.
///
pub fn premium_render(
    session: &Session,
    sample_rate: u32,
    targets: &MasteringTargets,
) -> Result<PremiumRender, RenderError> {
    // The graph's own #98 mastering chain is bypassed: this function *is*
    // the mastering chain for the premium path, and feeding it an already
    // limited mix defeats it. Measured on the fixture session before this
    // bypass existed, the premium normalizer asked for +9.50 dB and the
    // ceiling trim immediately took −9.13 dB back, landing 9.1 dB under the
    // −8.5 LUFS target — the double-master signature.
    let mix = render_session_with(session, sample_rate, &RenderOptions::unmastered())?;
    Ok(premium_master(mix, session.seed, targets))
}

/// The premium chain over an **already rendered, unmastered** mix (#102).
///
/// Split out of [`premium_render`] so a caller that has rendered a specific
/// cut — a mute-set, an instrumental — can master exactly that cut instead
/// of silently getting the full mix. `seed` is the session's, and it is the
/// only entropy in the chain: the dither is seeded from it, so the same mix
/// and seed always produce the same 16-bit payload.
///
/// The mix must be unmastered ([`RenderOptions::unmastered`]); handing this
/// a graph-mastered render is the double-master described above.
pub fn premium_master(
    mix: RenderOutput,
    seed: u64,
    targets: &MasteringTargets,
) -> PremiumRender {
    premium_master_with_drive(mix, seed, targets, PremiumDrive::Solve)
}

/// Where the premium chain's loudness drive comes from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PremiumDrive {
    /// Solve the drive from this render — what a full-mix delivery wants.
    Solve,
    /// Apply a drive solved elsewhere (dB).
    ///
    /// This is what a **stem** wants. A stem is a decomposition of the mix,
    /// so it has to be gain-referenced to the mix: solving each stem
    /// independently aims every one of them at the same integrated
    /// loudness, which is exactly the mix balance being thrown away. On the
    /// fixture, an independent solve moves one track +14.5 dB and another
    /// +6.1 dB — two stems 0.65 dB apart in the mix come out 9 dB apart.
    Fixed(f64),
}

/// The premium chain with explicit control over the loudness drive.
/// See [`PremiumDrive`]; [`premium_master`] is this with
/// [`PremiumDrive::Solve`].
pub fn premium_master_with_drive(
    mix: RenderOutput,
    seed: u64,
    targets: &MasteringTargets,
    drive: PremiumDrive,
) -> PremiumRender {
    let sample_rate = mix.sample_rate;

    // Same tilt parameters the RT chain reads from the targets file
    // (see MasteringChain::new_with_targets): pivot + centi-dB gain.
    let eq = LinearPhaseTiltEq::new(
        sample_rate,
        targets.tilt_hz,
        (targets.tilt_cdb / 100.0) as f32,
    );
    let (eq_l, eq_r) = eq.process(&mix.left, &mix.right);
    let (lim_l, lim_r, drive_db) = match drive {
        PremiumDrive::Solve => drive_into_limiter(&eq_l, &eq_r, sample_rate, targets),
        PremiumDrive::Fixed(db) => {
            let db = db.clamp(-MAX_DRIVE_DB, MAX_DRIVE_DB);
            let (l, r) = limit_at_drive(&eq_l, &eq_r, sample_rate, db, targets.ceiling_dbtp);
            (l, r, db)
        }
    };

    // Ceiling enforcement only. The loudness move already happened
    // upstream of the limiter, and the limiter leaves the program sitting
    // on the ceiling — so a second loudness normalization here could only
    // ask for gain and hand every dB of it straight back as a trim. Aim
    // `normalize_to_target` at the loudness the program already has and
    // it reduces to exactly the true-peak guard that is still wanted:
    // gain 0, trim = the limiter's own residual overshoot (#112).
    let limited_lufs = integrated_lufs(&lim_l, &lim_r, sample_rate);
    let norm = normalize_to_target(
        &lim_l,
        &lim_r,
        sample_rate,
        limited_lufs,
        targets.ceiling_dbtp,
    );
    let dithered = dither_tpdf_16(&norm.left, &norm.right, seed);
    let measurement = measure_loudness(&norm.left, &norm.right, sample_rate);
    PremiumRender {
        master: RenderOutput { left: norm.left, right: norm.right, sample_rate },
        dithered,
        measurement,
        drive_db,
        ceiling_trim_db: norm.ceiling_trim_db,
    }
}

/// The premium chain with the peak-control stage **bypassed** — what a
/// premium stem in a float deliverable is (#121).
///
/// The ×8 limiter and the ceiling trim engage by crest factor, so even with
/// the mix's shared drive in, a sparse percussive stem gives several dB back
/// that a dense sustained one keeps: measured on the fixture, the same drive
/// landed as +6.73 dB on track 0 and +2.64 dB on track 1. For a stem set
/// that destroys the mix balance a second time, after [`PremiumDrive::Fixed`]
/// made the drive shared. A float WAV can carry the samples above 0 dBFS
/// that skipping peak control leaves behind, so a float deliverable's stem
/// ships at mix gain instead: tilt EQ, the given drive as pure gain,
/// nothing else. An int deliverable cannot hold those samples, so its stems
/// keep the full chain ([`premium_master_with_drive`]).
///
/// `drive_db` is the full mix's drive (the same ±[`MAX_DRIVE_DB`] clamp the
/// [`PremiumDrive::Fixed`] arm applies) — solving loudness here would aim
/// the stem at the mix's target, which is the balance again.
pub fn premium_master_peaks_bypassed(
    mix: RenderOutput,
    seed: u64,
    targets: &MasteringTargets,
    drive_db: f64,
) -> PremiumRender {
    let sample_rate = mix.sample_rate;
    // Same tilt the limited chain applies (see premium_master_with_drive).
    let eq = LinearPhaseTiltEq::new(
        sample_rate,
        targets.tilt_hz,
        (targets.tilt_cdb / 100.0) as f32,
    );
    let (eq_l, eq_r) = eq.process(&mix.left, &mix.right);
    let drive_db = drive_db.clamp(-MAX_DRIVE_DB, MAX_DRIVE_DB);
    let g = 10.0f64.powf(drive_db / 20.0) as f32;
    let left: Vec<f32> = eq_l.iter().map(|s| s * g).collect();
    let right: Vec<f32> = eq_r.iter().map(|s| s * g).collect();
    let dithered = dither_tpdf_16(&left, &right, seed);
    let measurement = measure_loudness(&left, &right, sample_rate);
    PremiumRender {
        master: RenderOutput { left, right, sample_rate },
        dithered,
        measurement,
        drive_db,
        // No peak-control stage ran, so there is no trim to report.
        ceiling_trim_db: 0.0,
    }
}

/// How far below the signal's own true peak the saturation threshold
/// floats (dB). Only the top of the peaks is traded for harmonic content
/// (#115) — and because the threshold tracks the peak, the shaped
/// waveform is invariant to the drive: the drive solve stays a pure scale
/// problem, and the loudness-vs-drive curve saturates the same way the
/// pure limiter's did, one crest-reduction step higher.
///
/// 10.3 is a measurement, not a taste call: on the fixture the curve's
/// asymptote sits at −9.28 LUFS with a 4 dB shave and −8.09 with 12, so
/// 10.3 puts the flat top of the curve on the −8.5 target (measured
/// −8.51…−8.68 across drive 16–24; the solve converges at 21.2 dB of
/// drive delivering −8.53 LUFS) without pushing past it.
const CLIP_SHAVE_DB: f64 = 10.3;

/// Solve the pre-limiter drive that lands the program on the targets'
/// integrated loudness, then return the limited signal and the drive used.
///
/// The loudness move has to happen **before** peak control, not after it.
/// `normalize_to_target` is pure gain plus a hard true-peak trim, so run
/// downstream of a limiter it can never exceed (ceiling − crest factor):
/// it asks for the gain and hands the same number straight back as
/// `ceiling_trim_db`, and the export lands however far under target the
/// mix's crest factor happens to put it (#111 — 6.2 dB on the fixture).
/// Gaining *into* the limiter makes the limiter do the peak work, which
/// is what a limiter is for.
///
/// The limiter's gain reduction depends on the drive, so this is a fixed
/// point: measure, correct, re-limit, at most [`PREMIUM_DRIVE_PASSES`]
/// times. The correction steps along the secant through the last two
/// (drive, loudness) points once two exist: the curve is monotone but
/// saturating, and plain error accumulation crawls up its flat top one
/// shrinking correction at a time. Where the curve is flat the secant
/// step blows past the [`MAX_DRIVE_DB`] clamp, which stops the loop the
/// same way it always has — the delivered loudness is then the chain's
/// asymptote, which is the honest answer. Deterministic — the same input
/// always takes the same path — so the bit-exactness contract holds.
fn drive_into_limiter(
    left: &[f32],
    right: &[f32],
    sample_rate: u32,
    targets: &MasteringTargets,
) -> (Vec<f32>, Vec<f32>, f64) {
    let measured = integrated_lufs(left, right, sample_rate);
    if !measured.is_finite() {
        // Silence has no loudness to aim at; limit it as-is.
        let (l, r) = limit_x8(left, right, sample_rate);
        return (l, r, 0.0);
    }
    let mut drive_db = (targets.integrated_lufs - measured).clamp(-MAX_DRIVE_DB, MAX_DRIVE_DB);
    let mut out = limit_at_drive(left, right, sample_rate, drive_db, targets.ceiling_dbtp);
    let mut hit = integrated_lufs(&out.0, &out.1, sample_rate);
    // The point before the current one, for the secant slope.
    let mut prev: Option<(f64, f64)> = None;
    for _ in 1..PREMIUM_DRIVE_PASSES {
        if !hit.is_finite() {
            break;
        }
        let err = targets.integrated_lufs - hit;
        if err.abs() <= DRIVE_TOLERANCE_LU {
            break;
        }
        let step = match prev {
            Some((prev_drive, prev_hit))
                if (drive_db - prev_drive).abs() > 1e-9 && (hit - prev_hit).abs() > 1e-9 =>
            {
                let slope = (hit - prev_hit) / (drive_db - prev_drive);
                if slope > SECANT_MIN_SLOPE {
                    err / slope
                } else {
                    // Flat: the drive is doing nothing for loudness.
                    err.signum() * MAX_DRIVE_DB
                }
            }
            _ => err,
        };
        let next = (drive_db + step).clamp(-MAX_DRIVE_DB, MAX_DRIVE_DB);
        if next == drive_db {
            // The clamp is holding, or the correction rounded to nothing:
            // more passes cannot move it, so stop rather than spin.
            break;
        }
        prev = Some((drive_db, hit));
        drive_db = next;
        out = limit_at_drive(left, right, sample_rate, drive_db, targets.ceiling_dbtp);
        hit = integrated_lufs(&out.0, &out.1, sample_rate);
    }
    (out.0, out.1, drive_db)
}

/// Apply `drive_db` of gain, the saturation stage, the ×8 true-peak
/// limiter, then the requested ceiling.
///
/// The ceiling belongs *inside* the solve, not after it. `TruePeakLimiter`
/// enforces its own fixed [`kontinuum_mastering::limiter::CEILING_DBTP`]
/// (−1 dBTP) and never reads `targets.ceiling_dbtp`, so a profile asking
/// for a lower ceiling would otherwise converge against −1 dBTP and then
/// lose the whole difference to a trim the solve never measured: the same
/// "asks for gain, hands it back" failure this function exists to remove,
/// just moved one stage later. Trimming here means every iteration is
/// measured on the signal that will actually be returned.
///
/// Saturation runs ahead of the limiter (#115): a pure true-peak limiter
/// asymptotes at −9.4 LUFS on the fixture — past ~16 dB of drive, extra
/// gain converts into gain reduction, not loudness, and the −8.5 target
/// stays ~0.9 dB out of reach. Mastering's own [`SoftClipper`] (the RT
/// chain's stage 4, ×4 oversampled at the base rate) shaves the peaks
/// statically first, so the last ~1 dB is traded for harmonic content
/// instead of pure gain reduction, and the flat top of the curve moves
/// up into the target window (measured on the fixture at
/// [`CLIP_SHAVE_DB`] = 10.3: −8.51…−8.68 LUFS across drive 16–24, where
/// the limiter alone plateaued at −9.4).
fn limit_at_drive(
    left: &[f32],
    right: &[f32],
    sample_rate: u32,
    drive_db: f64,
    ceiling_dbtp: f64,
) -> (Vec<f32>, Vec<f32>) {
    // Saturation BEFORE the drive gain: the shave is measured against the
    // program's own peak, so [`CLIP_SHAVE_DB`] is a real top-of-program
    // depth. Ahead of the gain it also stays drive-invariant — the drive
    // only scales the clipped shape — which keeps the loudness solve a
    // pure gain problem.
    let (cl, cr) = soft_clip_at_peak_shave(left, right, sample_rate);
    let g = 10.0f64.powf(drive_db / 20.0) as f32;
    let dl: Vec<f32> = cl.iter().map(|s| s * g).collect();
    let dr: Vec<f32> = cr.iter().map(|s| s * g).collect();
    let (mut ll, mut rr) = limit_x8(&dl, &dr, sample_rate);
    let tp = true_peak_dbfs(&ll, &rr);
    if tp.is_finite() && tp > ceiling_dbtp {
        let t = 10.0f64.powf((ceiling_dbtp - tp) / 20.0) as f32;
        for s in ll.iter_mut().chain(rr.iter_mut()) {
            *s *= t;
        }
    }
    (ll, rr)
}

/// [`SoftClipper`] with its fixed ceiling floated to sit
/// [`CLIP_SHAVE_DB`] below the signal's own true peak: scale the signal
/// so that threshold lands on the clipper's −1.2 dBFS ceiling, clip, then
/// scale back. Below the knee the curve has unity slope, so the scale
/// round-trip is transparent there; above it, exactly the top
/// [`CLIP_SHAVE_DB`] of the peaks is shaved into the quadratic knee.
fn soft_clip_at_peak_shave(
    left: &[f32],
    right: &[f32],
    sample_rate: u32,
) -> (Vec<f32>, Vec<f32>) {
    let n = left.len().min(right.len());
    let tp = true_peak_dbfs(left, right);
    let threshold = (tp - CLIP_SHAVE_DB) as f32;
    // The clipper clips at its own fixed ceiling; map our threshold onto it.
    let k = (10.0f64.powf((CLIP_CEILING_DB as f64 - threshold as f64) / 20.0)) as f32;
    let mut clip = SoftClipper::new(sample_rate);
    let mut cl = Vec::with_capacity(n);
    let mut cr = Vec::with_capacity(n);
    for i in 0..n {
        let (l, r) = clip.tick(left[i] * k, right[i] * k);
        cl.push(l / k);
        cr.push(r / k);
    }
    (cl, cr)
}

/// Write a premium (or plain) 16-bit dithered payload as PCM WAV.
pub fn write_wav16(
    path: &Path,
    dithered: &Dithered16,
    sample_rate: u32,
) -> Result<(), RenderError> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for (l, r) in dithered.left.iter().zip(dithered.right.iter()) {
        writer.write_sample(*l)?;
        writer.write_sample(*r)?;
    }
    writer.finalize()?;
    Ok(())
}

/// Convenience: parse a session JSON file → premium render at
/// [`DEFAULT_SAMPLE_RATE`] → write a 16-bit WAV.
pub fn premium_render_to_wav(
    session_json_path: &Path,
    out_wav_path: &Path,
    targets: &MasteringTargets,
) -> Result<(), RenderError> {
    let session = parse_session(session_json_path)?;
    let render = premium_render(&session, DEFAULT_SAMPLE_RATE, targets)?;
    write_wav16(out_wav_path, &render.dithered, render.master.sample_rate)
}

/// Fixture session for the golden regression (same file the #11 golden
/// test pins; path is baked at compile time like `tests/golden.rs`).
pub const GOLDEN_FIXTURE: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/loop-4track.ir.json");

/// Golden-regression entry point (#32): premium-render the fixture
/// session and return the content hash of the final 16-bit payload —
/// the fingerprint of the premium export path. Deterministic: repeated
/// calls in the same build return the same value.
pub fn premium_golden_hash() -> Result<u64, RenderError> {
    let session = parse_session(Path::new(GOLDEN_FIXTURE))?;
    let targets = MasteringTargets::hypothesis();
    let render = premium_render(&session, DEFAULT_SAMPLE_RATE, &targets)?;
    Ok(render.content_hash())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kontinuum_mastering::limiter::CEILING_DBTP;
    use kontinuum_mastering::offline::true_peak_dbfs;

    /// Harsher than the RT limiter tests (1.5 sine there): a hot square
    /// wave — flat-topped, maximal inter-sample crest — layered with a
    /// sine, sustained forever.
    fn harsh_frame(i: usize, sr: u32) -> (f32, f32) {
        let sq = if (std::f64::consts::TAU * 997.0 * i as f64 / sr as f64).sin() >= 0.0 {
            1.0
        } else {
            -1.0
        };
        let sn = (std::f64::consts::TAU * 3_111.0 * i as f64 / sr as f64).sin();
        let x = 1.5 * sq as f32 + 0.6 * sn as f32;
        (x, x * 0.98)
    }

    #[test]
    fn x8_limiter_never_exceeds_ceiling() {
        let sr = 48_000u32;
        let n = sr as usize;
        let in_l: Vec<f32> = (0..n).map(|i| harsh_frame(i, sr).0).collect();
        let in_r: Vec<f32> = (0..n).map(|i| harsh_frame(i, sr).1).collect();
        let (out_l, out_r) = limit_x8(&in_l, &in_r, sr);
        // Skip the limiter's startup (first 100 ms).
        let tp = true_peak_dbfs(&out_l[n / 4..], &out_r[n / 4..]);
        assert!(
            tp <= CEILING_DBTP as f64,
            "×8 limiter true peak {tp} dB exceeds {CEILING_DBTP} dBTP"
        );
        // …and it actually worked (the input was far over the ceiling).
        let in_tp = true_peak_dbfs(&in_l[n / 4..], &in_r[n / 4..]);
        assert!(in_tp > 0.0, "test input must be over the ceiling: {in_tp}");
    }

    #[test]
    fn x8_limiter_stage_is_latency_aligned() {
        let sr = 48_000u32;
        let at = 4_096;
        let impulse: Vec<f32> =
            (0..16_384).map(|i| if i == at { 1.0 } else { 0.0 }).collect();
        let (out_l, _) = limit_x8(&impulse, &impulse, sr);
        let peak = out_l
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .map(|(i, _)| i);
        assert_eq!(peak, Some(at), "×8 stage must not shift the signal");
    }

    /// The ×8 stage's **own** output must sit under the ceiling across the
    /// whole drive sweep (#112) — not just after the downstream
    /// `normalize_to_target` trim that used to mask the overshoot.
    ///
    /// Before the post-decimator guard this measured −0.76 dBTP at every
    /// drive 0–20 dB (0.24 dB over the −1.0 promise) and +0.11 dBTP at
    /// 24 dB: the ×2 decimator reconstructs inter-sample peaks the 2×-rate
    /// limiter never saw (see `limit_x8` for the measured numbers).
    #[test]
    fn x8_limiter_own_output_respects_ceiling_across_drive_sweep() {
        let sr = 48_000u32;
        let session = parse_session(Path::new(GOLDEN_FIXTURE)).expect("fixture parses");
        let mix = render_session_with(&session, sr, &RenderOptions::unmastered())
            .expect("unmastered fixture mix");
        let mut results = Vec::new();
        let mut over = Vec::new();
        for drive in [0.0, 4.0, 8.0, 12.0, 16.0, 20.0, 24.0] {
            let g = 10.0f64.powf(drive / 20.0) as f32;
            let dl: Vec<f32> = mix.left.iter().map(|s| s * g).collect();
            let dr: Vec<f32> = mix.right.iter().map(|s| s * g).collect();
            let (out_l, out_r) = limit_x8(&dl, &dr, sr);
            let tp = true_peak_dbfs(&out_l, &out_r);
            results.push(format!("drive {drive:4.1} dB -> {tp:+.2} dBTP"));
            if tp > CEILING_DBTP as f64 + 1e-3 {
                over.push(format!("drive {drive:4.1} dB: {tp:+.2} dBTP"));
            }
        }
        eprintln!("×8 stage own-output sweep:\n{}", results.join("\n"));
        assert!(
            over.is_empty(),
            "×8 stage output exceeds {CEILING_DBTP} dBTP:\n{}",
            over.join("\n")
        );
    }
}
