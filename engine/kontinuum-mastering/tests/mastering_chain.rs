//! Cross-stage behavior tests for the mastering chain (#28): the safety
//! guarantees (ceiling, alarm, unity, no-clicks, determinism) that span
//! multiple stages, plus the offline export helpers.

use kontinuum_mastering::offline::{
    integrated_lufs, measure_loudness, normalize_to_target, true_peak_dbfs,
};
use kontinuum_mastering::{MasteringChain, OutputProfile};

const SR: u32 = 48_000;

fn sine(freq_hz: f32, amp: f32, i: usize) -> f32 {
    amp * (std::f32::consts::TAU * freq_hz * i as f32 / SR as f32).sin()
}

fn render_all(chain: &mut MasteringChain, left: &mut [f32], right: &mut [f32]) {
    // Drive through 64-frame blocks like the engine does.
    let n = left.len().min(right.len());
    let mut pos = 0;
    while pos < n {
        let end = (pos + 64).min(n);
        chain.render(&mut left[pos..end], &mut right[pos..end]);
        pos = end;
    }
}

fn rms_db(v: &[f32]) -> f64 {
    let ms = v.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>() / v.len().max(1) as f64;
    10.0 * ms.max(1e-24).log10()
}

#[test]
fn unity_passthrough_neutral_is_silence_safe_and_transparent() {
    // Neutral settings = bypassed chain: bit-exact for silence and for a
    // moderate program; the live chain keeps silence exactly silent too.
    let mut chain = MasteringChain::new(SR);
    chain.set_bypassed(true);

    let mut silence = vec![0.0f32; SR as usize];
    let mut silence_r = silence.clone();
    render_all(&mut chain, &mut silence, &mut silence_r);
    assert!(silence.iter().all(|s| s.abs() < 1e-9), "bypassed silence moved");

    let mut moderate: Vec<f32> = (0..SR as usize).map(|i| sine(997.0, 0.25, i)).collect();
    let mut moderate_r = moderate.clone();
    let in_db = rms_db(&moderate);
    render_all(&mut chain, &mut moderate, &mut moderate_r);
    let out_db = rms_db(&moderate);
    assert_eq!(moderate, moderate_r);
    assert!(
        (out_db - in_db).abs() < 0.1,
        "bypassed moderate input changed by {} dB",
        (out_db - in_db).abs()
    );

    // Live (not bypassed) chain on silence: envelopes rest at zero, so
    // the output must remain silent — graceful degradation on empty input.
    let mut chain = MasteringChain::new(SR);
    let mut quiet = vec![0.0f32; SR as usize];
    let mut quiet_r = quiet.clone();
    render_all(&mut chain, &mut quiet, &mut quiet_r);
    assert!(quiet.iter().all(|s| s.abs() < 1e-9), "live chain hisses on silence");
}

#[test]
fn hot_material_never_exceeds_minus_one_dbtp() {
    // Several frequencies including inter-sample-peak-prone ones, driven
    // 5.5 dB over the ceiling.
    let mut chain = MasteringChain::new(SR);
    let n = SR as usize;
    let mut all_l = Vec::with_capacity(4 * n);
    let mut all_r = Vec::with_capacity(4 * n);
    for &freq in &[997.0f32, 1_999.0, 7_317.0, 15_971.0] {
        let seg_l: Vec<f32> = (0..n).map(|i| sine(freq, 1.5, i)).collect();
        let seg_r: Vec<f32> = (0..n).map(|i| sine(freq + 0.7, 1.5, i)).collect();
        all_l.extend_from_slice(&seg_l);
        all_r.extend_from_slice(&seg_r);
    }
    render_all(&mut chain, &mut all_l, &mut all_r);
    let tp = true_peak_dbfs(&all_l, &all_r);
    assert!(
        tp <= -1.0,
        "true peak {tp} dBFS exceeds the −1.0 dBTP ceiling"
    );
}

#[test]
fn sustained_over_limit_input_raises_the_gr_alarm() {
    // Stage-level: the limiter owns the sustained-GR alarm (#15 feed).
    let mut lim = kontinuum_mastering::limiter::TruePeakLimiter::new(SR);
    let mut latched = false;
    for i in 0..2 * SR as usize {
        let (l, _) = lim.tick(1.5, 1.5);
        assert!(l.abs() <= 1.5);
        if lim.alarm() {
            latched = true;
            assert!(i >= (kontinuum_mastering::limiter::GR_ALARM_SUSTAIN_S * SR as f32) as usize);
            break;
        }
    }
    assert!(latched, "sustained over-limit must latch the alarm");
    lim.reset();
    assert!(!lim.alarm(), "reset must clear the alarm");
}

#[test]
fn pathological_input_is_absorbed_upstream_without_alarm() {
    // The glue stage's bounded absorption means even an absurd input
    // (amplitude 20) reaches the limiter tamed: ceiling holds, output
    // stays finite, and the limiter alarm does NOT fire — the mix
    // carries loudness, not the limiter (#28 policy). The alarm latch
    // itself is the limiter stage's contract, tested separately.
    let mut chain = MasteringChain::new(SR);
    let n = 2 * SR as usize;
    let mut l: Vec<f32> = (0..n).map(|i| sine(997.0, 20.0, i)).collect();
    let mut r: Vec<f32> = (0..n).map(|i| sine(997.0, 20.0, i)).collect();
    render_all(&mut chain, &mut l, &mut r);
    assert!(l.iter().chain(r.iter()).all(|s| s.is_finite()));
    assert!(true_peak_dbfs(&l, &r) <= -1.0, "pathological input breached the ceiling");
    assert!(!chain.limiter_alarm(), "glue must absorb the pathology upstream");
}

#[test]
fn tilt_eq_steers_a_tilted_stimulus_toward_flat() {
    // Bright stimulus: 6 kHz +12 dB relative to 150 Hz, in both channels.
    // The high/low ratio is immune to broadband gain stages (glue/limiter
    // move both together), so it isolates the tilt response.
    fn goertzel(buf: &[f32], freq: f32) -> f64 {
        let k = (buf.len() as f64 * freq as f64 / SR as f64).round();
        let w = std::f64::consts::TAU * k / buf.len() as f64;
        let coeff = 2.0 * w.cos();
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for &x in buf {
            let s0 = x as f64 + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        (s1 * s1 + s2 * s2 - coeff * s1 * s2).sqrt()
    }

    fn band_ratio(chain: &mut MasteringChain) -> f64 {
        let n = 2 * SR as usize;
        let mut l = Vec::with_capacity(n);
        let mut r = Vec::with_capacity(n);
        for i in 0..n {
            let lo = 0.25 * sine(150.0, 1.0, i);
            let hi = 1.0 * sine(6_000.0, 1.0, i);
            l.push(lo + hi);
            r.push(lo + hi);
        }
        render_all(chain, &mut l, &mut r);
        // Goertzel magnitudes on the settled tail.
        let tail = &l[l.len() - SR as usize / 2..];
        let lo_mag = goertzel(tail, 150.0);
        let hi_mag = goertzel(tail, 6_000.0);
        20.0 * (hi_mag / lo_mag.max(1e-12)).log10()
    }

    let mut neutral = MasteringChain::new(SR);
    neutral.set_bypassed(true);
    let before = band_ratio(&mut neutral);

    let mut chain = MasteringChain::new(SR);
    chain.set_tilt_target_db(-3.0);
    // Advance the 5 s mastering slew to its target with silence, then
    // measure the ratio shift on the actual stimulus.
    let blocks = (30.0 * SR as f64 / 64.0) as usize;
    let mut l = vec![0.0f32; 64];
    let mut r = vec![0.0f32; 64];
    for _ in 0..blocks {
        chain.render(&mut l, &mut r);
    }
    let after = band_ratio(&mut chain);

    // Expected: −3 dB high shelf / +3 dB low shelf moves the pair by
    // roughly −6 dB (each shelf contributes ~3 dB this far from pivot).
    let delta = after - before;
    assert!(
        (-7.5..=-3.0).contains(&delta),
        "tilt must darken the bright pair: before {before:.2} after {after:.2}"
    );
}

#[test]
fn small_speaker_profile_changes_output_and_respects_the_tilt_cap() {
    // Same stimulus as the tilt test: the high/low band ratio isolates the
    // tilt response from broadband gain stages. The profile must audibly
    // brighten the master (v0: tilt + low-relax knobs only) while the
    // applied tilt target stays inside the stage's ±3 dB cap.
    fn goertzel(buf: &[f32], freq: f32) -> f64 {
        let k = (buf.len() as f64 * freq as f64 / SR as f64).round();
        let w = std::f64::consts::TAU * k / buf.len() as f64;
        let coeff = 2.0 * w.cos();
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for &x in buf {
            let s0 = x as f64 + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        (s1 * s1 + s2 * s2 - coeff * s1 * s2).sqrt()
    }

    fn band_ratio(chain: &mut MasteringChain) -> f64 {
        let n = 2 * SR as usize;
        let mut l = Vec::with_capacity(n);
        let mut r = Vec::with_capacity(n);
        for i in 0..n {
            let lo = 0.25 * sine(150.0, 1.0, i);
            let hi = 1.0 * sine(6_000.0, 1.0, i);
            l.push(lo + hi);
            r.push(lo + hi);
        }
        render_all(chain, &mut l, &mut r);
        let tail = &l[l.len() - SR as usize / 2..];
        let lo_mag = goertzel(tail, 150.0);
        let hi_mag = goertzel(tail, 6_000.0);
        20.0 * (hi_mag / lo_mag.max(1e-12)).log10()
    }

    // Neutral host tilt + the profile offset: the target lands at +2 dB.
    let mut full = MasteringChain::new(SR);
    full.set_output_profile(OutputProfile::Full);
    let before = band_ratio(&mut full);

    let mut small = MasteringChain::new(SR);
    small.set_output_profile(OutputProfile::SmallSpeaker);
    assert!(
        (small.telemetry().tilt_db - 2.0).abs() < 1e-5,
        "neutral + SmallSpeaker must tilt +2 dB: {}",
        small.telemetry().tilt_db
    );

    // Cap: a host tilt already at +3 must not be pushed past ±3 by the
    // profile offset.
    small.set_tilt_target_db(3.0);
    assert!(
        (small.telemetry().tilt_db - 3.0).abs() < 1e-5,
        "tilt cap breached by the profile offset: {}",
        small.telemetry().tilt_db
    );

    // Advance the 5 s mastering slew to its target with silence, so the
    // measured ratio reflects the settled profile, not the slew.
    let blocks = (30.0 * SR as f64 / 64.0) as usize;
    let mut l = vec![0.0f32; 64];
    let mut r = vec![0.0f32; 64];
    for _ in 0..blocks {
        small.render(&mut l, &mut r);
    }

    // And the rendered output must actually change: brighten relative to
    // the Full profile on the same stimulus.
    let after = band_ratio(&mut small);
    let delta = after - before;
    assert!(
        delta > 0.5,
        "SmallSpeaker profile must brighten the pair: before {before:.2} after {after:.2}"
    );
    assert!(after.is_finite() && before.is_finite());
}

#[test]
fn loudness_measurement_matches_known_amplitude_sine() {
    // 997 Hz stereo sine at 0.5 amplitude: expected integrated LUFS =
    // −0.691 + 20·log10(0.5) ≈ −6.71 (K-weighting ≈ 0 dB at 997 Hz).
    let n = 5 * SR as usize;
    let l: Vec<f32> = (0..n).map(|i| sine(997.0, 0.5, i)).collect();
    let r: Vec<f32> = (0..n).map(|i| sine(997.0, 0.5, i)).collect();
    let measured = integrated_lufs(&l, &r, SR);
    let expected = -0.691 + 20.0 * (0.5f64).log10();
    assert!(
        (measured - expected).abs() <= 0.5,
        "measured {measured:.3} vs expected {expected:.3} LUFS"
    );

    // Silence gates to −∞ rather than reporting a fake number.
    let silent = integrated_lufs(&vec![0.0; n], &vec![0.0; n], SR);
    assert_eq!(silent, f64::NEG_INFINITY);
    let full = measure_loudness(&l, &r, SR);
    assert!(
        (full.short_term_peak_lufs - expected).abs() <= 0.5,
        "short-term peak {full:?}"
    );
}

#[test]
fn normalize_pass_hits_target_and_respects_ceiling() {
    let n = 5 * SR as usize;
    let l: Vec<f32> = (0..n).map(|i| sine(997.0, 0.25, i)).collect();
    let r: Vec<f32> = (0..n).map(|i| sine(997.0, 0.25, i)).collect();
    let out = normalize_to_target(&l, &r, SR, -14.0, -1.0);
    assert!(
        (out.integrated_lufs - (-14.0)).abs() <= 0.5,
        "normalized to {} LUFS",
        out.integrated_lufs
    );
    assert!(true_peak_dbfs(&out.left, &out.right) <= -1.0 + 1e-6, "ceiling breached");
    // Silence passes through untouched rather than exploding.
    let silent = normalize_to_target(&vec![0.0; 64], &vec![0.0; 64], SR, -14.0, -1.0);
    assert_eq!(silent.gain_db, 0.0);
    assert!(silent.left.iter().all(|s| s.abs() < 1e-9));
}

#[test]
fn determinism_same_input_same_output() {
    let n = SR as usize / 2;
    let mut a = MasteringChain::new(SR);
    let mut b = MasteringChain::new(SR);
    let mut la: Vec<f32> = (0..n).map(|i| sine(440.0, 0.9, i)).collect();
    let mut ra: Vec<f32> = (0..n).map(|i| sine(441.3, 0.9, i)).collect();
    let mut lb = la.clone();
    let mut rb = ra.clone();
    render_all(&mut a, &mut la, &mut ra);
    render_all(&mut b, &mut lb, &mut rb);
    assert_eq!(la, lb, "left channels diverged");
    assert_eq!(ra, rb, "right channels diverged");
    assert_eq!(a.telemetry(), b.telemetry());
}

#[test]
fn parameter_moves_do_not_click() {
    let mut chain = MasteringChain::new(SR);
    let n = 3 * SR as usize;
    let mut l = Vec::with_capacity(n);
    let mut r = Vec::with_capacity(n);
    let mut max_step = 0.0f32;
    let mut prev = 0.0f32;
    for i in 0..n {
        // Move every adaptive target mid-stream.
        if i == n / 3 {
            chain.set_tilt_target_db(3.0);
            chain.set_section_energy(0.0);
        }
        if i == 2 * n / 3 {
            chain.set_tilt_target_db(-3.0);
            chain.set_section_energy(1.0);
        }
        let x = sine(500.0, 0.5, i);
        let mut xl = vec![x];
        let mut xr = vec![x * 0.9];
        render_all(&mut chain, &mut xl, &mut xr);
        let y = xl[0];
        max_step = max_step.max((y - prev).abs());
        prev = y;
        l.push(y);
        r.push(xr[0]);
    }
    // Natural per-sample slew of a 500 Hz sine at 0.5 ≈ 0.033; clicks
    // from coefficient jumps would dwarf it.
    assert!(max_step < 0.08, "parameter move clicked: step {max_step}");
    assert!(l.iter().chain(r.iter()).all(|s| s.is_finite()));
}

#[test]
fn section_energy_relaxes_processing() {
    let mut chain = MasteringChain::new(SR);
    // Loud program engages glue + clipper.
    let n = SR as usize;
    let mut l: Vec<f32> = (0..n).map(|i| sine(120.0, 1.1, i)).collect();
    let mut r: Vec<f32> = (0..n).map(|i| sine(120.0, 1.1, i)).collect();
    render_all(&mut chain, &mut l, &mut r);
    let hot = chain.telemetry();

    // Breakdown: quiet section signal + relaxation commanded.
    chain.set_section_energy(0.0);
    let mut bl = vec![0.05f32; 2 * n];
    let mut br = vec![0.05f32; 2 * n];
    render_all(&mut chain, &mut bl, &mut br);
    let relaxed = chain.telemetry();
    assert!(relaxed.section_relax > 0.9, "relax must follow the breakdown");
    assert!(
        relaxed.glue_gr_db < hot.glue_gr_db.max(0.5),
        "glue must see through breakdowns: hot {} relaxed {}",
        hot.glue_gr_db,
        relaxed.glue_gr_db
    );
    assert!(
        relaxed.clipper_drive_db <= hot.clipper_drive_db + 0.05,
        "clipper drive must unwind in breakdowns"
    );
}

#[test]
fn latency_is_positive_and_stable() {
    let mut chain = MasteringChain::new(SR);
    let lat = chain.latency_frames();
    // Up FIR + 1.5 ms lookahead + down FIR.
    assert!(lat >= 72, "lookahead missing: {lat}");
    chain.set_bypassed(true);
    assert_eq!(chain.latency_frames(), 0);
}
