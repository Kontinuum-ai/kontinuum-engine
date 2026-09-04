//! Facade-level tests for the auto-mix engine: end-to-end convergence,
//! collision and masking scenarios, slew, silence, stability, determinism,
//! allocation freedom, and the telemetry contract.

use super::*;
use crate::fnv1a64;

const SR: u32 = 48_000;

struct Voice {
    freq: f32,
    amp: f32,
}

impl Voice {
    fn sample(&self, i: usize) -> f32 {
        self.amp * (std::f32::consts::TAU * self.freq * i as f32 / SR as f32).sin()
    }
}

fn rms(buf: &[f32]) -> f32 {
    (buf.iter().map(|s| s * s).sum::<f32>() / buf.len().max(1) as f32).sqrt()
}

/// Kick at −9 dBFS anchor, bass +5 dB hot, pad 6 dB quiet, perc mid.
fn rig() -> (AutoMixer, Vec<Voice>, Vec<u8>) {
    let mut mix = AutoMixer::new(SR);
    let (voices, tracks) = rig_inited(&mut mix);
    (mix, voices, tracks)
}

fn rig_inited(mix: &mut AutoMixer) -> (Vec<Voice>, Vec<u8>) {
    mix.set_role(0, MixRole::Kick);
    mix.set_role(1, MixRole::Bass);
    mix.set_role(2, MixRole::Perc);
    mix.set_role(3, MixRole::Pad);
    (
        vec![
            Voice { freq: 60.0, amp: 0.5 },
            Voice { freq: 55.0, amp: 0.9 },
            Voice { freq: 800.0, amp: 0.2 },
            Voice { freq: 220.0, amp: 0.05 },
        ],
        vec![0, 1, 2, 3],
    )
}

/// Render `seconds` of the rig; `kick_period > 0` keys a half-note kick.
fn run_rig(mix: &mut AutoMixer, voices: &[Voice], tracks: &[u8], seconds: f32, kick_period: usize) {
    let total = (SR as f32 * seconds) as usize;
    for i in (0..total).step_by(crate::BLOCK_FRAMES) {
        let n = crate::BLOCK_FRAMES.min(total - i);
        if kick_period > 0 && (i / kick_period) % 2 == 0 {
            mix.kick(1.0);
        }
        for (&ti, v) in tracks.iter().zip(voices.iter()) {
            let mut buf: Vec<f32> = (i..i + n).map(|k| v.sample(k)).collect();
            mix.process_track(ti, &mut buf);
        }
    }
}

#[test]
fn end_to_end_tracks_settle_inside_role_tolerance() {
    let (mut mix, voices, tracks) = rig();
    run_rig(&mut mix, &voices, &tracks, 14.0, 0);
    let tel = mix.telemetry();
    // Anchor: kick measured ≈ −9 dBFS. Every role must land within
    // tolerance of (anchor + role target).
    assert!((tel.track_gain_db[1]).abs() <= GAIN_CORRECTION_MAX_DB);
    assert!(tel.track_gain_db[1] < -1.0, "hot bass not pulled down: {}", tel.track_gain_db[1]);
    assert!(tel.track_gain_db[3] > 1.0, "quiet pad not lifted: {}", tel.track_gain_db[3]);
    assert!(tel.track_gain_db[0].abs() < 0.75, "kick anchor drifted: {}", tel.track_gain_db[0]);
}

#[test]
fn kick_bass_collision_bass_node_engages_and_is_bounded() {
    let (mut mix, voices, _) = rig();
    let seconds = 4.0;
    let total = (SR as f32 * seconds) as usize;
    let mut max_cut = 0.0f32;
    for i in (0..total).step_by(crate::BLOCK_FRAMES) {
        let n = crate::BLOCK_FRAMES.min(total - i);
        // Half-note kick key.
        if (i / (SR as usize / 2)) % 2 == 0 {
            mix.kick(1.0);
        }
        let mut buf: Vec<f32> = (i..i + n).map(|k| voices[1].sample(k)).collect();
        mix.process_track(1, &mut buf);
        max_cut = max_cut.max(mix.telemetry().bass_cut_db);
    }
    assert!(max_cut > 0.5, "bass node never engaged: {max_cut}");
    assert!(max_cut <= BASS_CUT_MAX_DB + 0.2, "bass cut over cap: {max_cut}");
    assert!(mix.telemetry().bass_node_active);
}

#[test]
fn masking_input_produces_bounded_carves() {
    let mut mix = AutoMixer::new(SR);
    mix.set_role(3, MixRole::Pad);
    mix.set_masking(3, 0, 1.0);
    mix.set_masking(3, 1, 0.9);
    let total = SR as usize;
    let mut max_cut = 0.0f32;
    for i in (0..total).step_by(crate::BLOCK_FRAMES) {
        let n = crate::BLOCK_FRAMES.min(total - i);
        let mut buf: Vec<f32> =
            (i..i + n).map(|k| 0.4 * (std::f32::consts::TAU * 300.0 * k as f32 / SR as f32).sin()).collect();
        mix.process_track(3, &mut buf);
        max_cut = max_cut.max(mix.telemetry().mask_cut_db[3]);
    }
    assert!(max_cut > 1.0, "mask input did not carve: {max_cut}");
    assert!(max_cut <= MASK_CUT_MAX_DB + 0.2, "mask carve over cap: {max_cut}");
    assert!(mix.telemetry().mask_active);
}

#[test]
fn parameter_moves_are_click_free() {
    let (mut mix, voices, _) = rig();
    mix.set_mask_band(1, 0, 40.0, 90.0);
    let total = SR as usize;
    let half = total / 2;
    let mut max_delta = 0.0f32;
    let mut prev_out = 0.0f32;
    for i in (0..total).step_by(crate::BLOCK_FRAMES) {
        let n = crate::BLOCK_FRAMES.min(total - i);
        if i >= half {
            mix.set_masking(1, 0, 1.0);
            mix.kick(1.0);
        }
        let mut buf: Vec<f32> = (i..i + n).map(|k| voices[1].sample(k)).collect();
        mix.process_track(1, &mut buf);
        for s in buf {
            max_delta = max_delta.max((s - prev_out).abs());
            prev_out = s;
        }
    }
    // 55 Hz tone's natural sample slew is ~0.0065; the slew-moved carve
    // adds ~0.001 and the #76 duck attack (5 ms one-pole toward the Bass
    // role's 0.9 full-key depth) ~0.0034 on the keying tile. A real click
    // (unslewed gain step) would be ~0.9.
    assert!(max_delta < 0.013, "audible click on parameter move: {max_delta}");
}

#[test]
fn silence_stays_exactly_silent() {
    let mut mix = AutoMixer::new(SR);
    mix.set_role(0, MixRole::Kick);
    mix.set_role(1, MixRole::Bass);
    mix.set_masking(1, 0, 1.0);
    mix.kick(1.0);
    mix.set_drum_drive(1.8);
    mix.set_harmonic_drive(1.6);
    let total = SR as usize / 2;
    for i in (0..total).step_by(crate::BLOCK_FRAMES) {
        let n = crate::BLOCK_FRAMES.min(total - i);
        let mut kt = vec![0.0f32; n];
        let mut bt = vec![0.0f32; n];
        mix.process_track(0, &mut kt);
        mix.process_track(1, &mut bt);
        let mut dl = vec![0.0f32; n];
        let mut dr = vec![0.0f32; n];
        let mut hl = vec![0.0f32; n];
        let mut hr = vec![0.0f32; n];
        mix.process_track(2, &mut dl);
        mix.process_drum_bus(&mut dl, &mut dr);
        mix.process_harmonic_bus(&mut hl, &mut hr);
        assert!(kt.iter().chain(bt.iter()).chain(dl.iter()).chain(dr.iter())
            .chain(hl.iter()).chain(hr.iter()).all(|s| *s == 0.0));
    }
    assert!(mix.telemetry().track_gain_db.iter().all(|g| *g == 0.0), "servo chased silence");
}

#[test]
fn long_run_sixty_seconds_stays_finite_and_bounded() {
    let (mut mix, voices, tracks) = rig();
    mix.set_masking(3, 0, 0.8);
    let total = SR as usize * 60;
    let kick_period = SR as usize / 2;
    let mut max_gain = 0.0f32;
    let mut min_gain = 0.0f32;
    let mut max_cut = 0.0f32;
    for i in (0..total).step_by(crate::BLOCK_FRAMES) {
        let n = crate::BLOCK_FRAMES.min(total - i);
        if (i / kick_period) % 2 == 0 {
            mix.kick(0.95);
        }
        // Deterministic slow wobble on the mask input.
        let overlap = 0.5 + 0.5 * (std::f32::consts::TAU * i as f32 / (SR as usize * 8) as f32).sin();
        mix.set_masking(3, 0, overlap);
        for (&ti, v) in tracks.iter().zip(voices.iter()) {
            let mut buf: Vec<f32> = (i..i + n).map(|k| v.sample(k)).collect();
            mix.process_track(ti, &mut buf);
        }
        let mut l: Vec<f32> = (i..i + n).map(|k| voices[0].sample(k) + voices[2].sample(k)).collect();
        let mut r = l.clone();
        mix.process_drum_bus(&mut l, &mut r);
        let tel = mix.telemetry();
        for &g in tel.track_gain_db.iter() {
            max_gain = max_gain.max(g);
            min_gain = min_gain.min(g);
            assert!(g.is_finite());
        }
        max_cut = max_cut.max(tel.bass_cut_db).max(tel.mask_cut_db[3]);
        assert!(l.iter().chain(r.iter()).all(|s| s.is_finite()));
    }
    assert!(max_gain <= GAIN_CORRECTION_MAX_DB + 1e-3);
    assert!(min_gain >= -GAIN_CORRECTION_MAX_DB - 1e-3);
    assert!(max_cut <= BASS_CUT_MAX_DB + 0.2, "carve over the largest cap: {max_cut}");
    assert!(mix.telemetry().track_gain_db.iter().all(|g| g.is_finite()));
}

#[test]
fn determinism_bit_identical_double_render() {
    fn scripted_render() -> Vec<f32> {
        let mut mix = AutoMixer::new(SR);
        let (voices, tracks) = rig_inited(&mut mix);
        let total = SR as usize * 2;
        let mut out: Vec<f32> = Vec::with_capacity(total * tracks.len());
        for i in (0..total).step_by(crate::BLOCK_FRAMES) {
            let n = crate::BLOCK_FRAMES.min(total - i);
            if (i / (SR as usize / 2)) % 2 == 0 {
                mix.kick(1.0);
            }
            mix.set_masking(3, 0, if i > total / 2 { 0.9 } else { 0.0 });
            for (&ti, v) in tracks.iter().zip(voices.iter()) {
                let mut buf: Vec<f32> = (i..i + n).map(|k| v.sample(k)).collect();
                mix.process_track(ti, &mut buf);
                out.extend_from_slice(&buf);
            }
            let mut l: Vec<f32> =
                (i..i + n).map(|k| voices[0].sample(k) + voices[2].sample(k)).collect();
            let mut r = l.clone();
            mix.process_drum_bus(&mut l, &mut r);
            out.extend_from_slice(&l);
        }
        out
    }
    let a = scripted_render();
    let b = scripted_render();
    let bits_a: Vec<u8> = a.iter().flat_map(|s| s.to_bits().to_le_bytes()).collect();
    let bits_b: Vec<u8> = b.iter().flat_map(|s| s.to_bits().to_le_bytes()).collect();
    assert_eq!(fnv1a64(&bits_a), fnv1a64(&bits_b), "renders diverged");
}

#[test]
fn render_path_is_allocation_free() {
    let (mut mix, voices, tracks) = rig();
    let mut buf = vec![0.0f32; crate::BLOCK_FRAMES];
    let mut l = vec![0.0f32; crate::BLOCK_FRAMES];
    let mut r = vec![0.0f32; crate::BLOCK_FRAMES];
    assert_no_alloc::assert_no_alloc(|| {
        for (i, (&ti, v)) in tracks.iter().zip(voices.iter()).enumerate() {
            for (k, s) in buf.iter_mut().enumerate() {
                *s = v.sample(i * 1000 + k);
            }
            mix.process_track(ti, &mut buf);
            if ti == 0 {
                mix.kick(1.0);
            }
            mix.set_masking(3, 0, 0.7);
        }
        mix.process_drum_bus(&mut l, &mut r);
        mix.process_harmonic_bus(&mut l, &mut r);
    });
}

#[test]
fn telemetry_serializes_round_trip() {
    let (mut mix, voices, _tracks) = rig();
    let mut buf: Vec<f32> = (0..crate::BLOCK_FRAMES).map(|k| voices[1].sample(k)).collect();
    mix.process_track(1, &mut buf);
    mix.kick(1.0);
    let tel = mix.telemetry();
    let json = serde_json::to_string(&tel).expect("serialize");
    let back: MixTelemetry = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, tel);
    assert!(json.contains("track_gain_db"));
}

#[test]
fn reset_returns_to_bypass_state() {
    let (mut mix, voices, tracks) = rig();
    run_rig(&mut mix, &voices, &tracks, 1.0, SR as usize);
    mix.reset();
    let tel = mix.telemetry();
    assert!(tel.track_gain_db.iter().all(|g| *g == 0.0));
    assert_eq!(tel.bass_cut_db, 0.0);
    assert_eq!(tel.tiles, 0);
    // Given a fresh mixer with the bass node detached: processing must
    // be bit-exact passthrough.
    mix.set_role(1, MixRole::Unassigned);
    let mut buf: Vec<f32> = (0..crate::BLOCK_FRAMES).map(|k| voices[1].sample(k)).collect();
    let reference: Vec<f32> = buf.clone();
    mix.process_track(1, &mut buf);
    assert!(buf.iter().zip(reference.iter()).all(|(a, b)| a.to_bits() == b.to_bits()),
        "post-reset track is not bit-exact bypass");
}

#[test]
fn duck_depth_setter_clamps_and_role_redefaults() {
    let mut mix = AutoMixer::new(SR);
    mix.set_role(1, MixRole::Bass);
    assert!((mix.duck_depth(1) - MixRole::Bass.duck_depth()).abs() < 1e-6);
    mix.set_duck_depth(1, 5.0);
    assert!((mix.duck_depth(1) - 1.0).abs() < 1e-6, "depth must clamp to the full range");
    mix.set_duck_depth(1, -1.0);
    assert_eq!(mix.duck_depth(1), 0.0);
    mix.set_duck_depth(1, f32::NAN);
    assert_eq!(mix.duck_depth(1), 0.0);
    // Role re-assignment re-defaults the depth.
    mix.set_role(1, MixRole::Perc);
    assert!((mix.duck_depth(1) - MixRole::Perc.duck_depth()).abs() < 1e-6);
}

#[test]
fn broadband_duck_engages_on_non_bass_role_and_stays_bounded() {
    let mut mix = AutoMixer::new(SR);
    mix.set_role(2, MixRole::Perc);
    let tone = |i: usize| 0.5 * (std::f32::consts::TAU * 800.0 * i as f32 / SR as f32).sin();
    // Four keyed tiles (~5.3 ms ≈ one attack τ): the servo is still in
    // warmup, so its gain is exactly 1.0 and the ratio isolates the duck.
    mix.kick(1.0);
    let mut ratio = 1.0f32;
    let mut out = vec![];
    for tile in 0..4u32 {
        let input: Vec<f32> =
            (0..crate::BLOCK_FRAMES).map(|k| tone(tile as usize * crate::BLOCK_FRAMES + k)).collect();
        out = input.clone();
        mix.process_track(2, &mut out);
        ratio = rms(&out) / rms(&input);
    }
    assert!(ratio < 0.9, "perc-role track did not duck: ratio {ratio}");
    // Steady half-note key for 2 s: bounded by the Perc depth (0.5 → never
    // below −6 dB plus attack margin).
    let total = SR as usize * 2;
    let mut worst = 1.0f32;
    for i in (0..total).step_by(crate::BLOCK_FRAMES) {
        let n = crate::BLOCK_FRAMES.min(total - i);
        if (i / (SR as usize / 2)) % 2 == 0 {
            mix.kick(1.0);
        }
        let input: Vec<f32> = (i..i + n).map(tone).collect();
        let mut out = input.clone();
        mix.process_track(2, &mut out);
        worst = worst.min(rms(&out) / rms(&input));
    }
    assert!(worst > 0.4, "duck dug past its depth bound: ratio {worst}");
    assert!(out.iter().all(|s| s.is_finite()));
}

#[test]
fn duck_depth_param_reaches_unity_at_max() {
    let tone = |i: usize| 0.5 * (std::f32::consts::TAU * 300.0 * i as f32 / SR as f32).sin();
    // Hold the key open (a kick every tile) so the attenuation settles
    // exactly at depth: the applied gain bottoms at 1 − depth.
    let settled_ratio = |depth: f32| {
        let mut mix = AutoMixer::new(SR);
        mix.set_role(2, MixRole::Bass);
        mix.set_duck_depth(2, depth);
        let mut ratio = 1.0f32;
        for tile in 0..60u32 {
            mix.kick(1.0);
            let input: Vec<f32> =
                (0..crate::BLOCK_FRAMES).map(|k| tone(tile as usize * crate::BLOCK_FRAMES + k)).collect();
            let mut out = input.clone();
            mix.process_track(2, &mut out);
            ratio = rms(&out) / rms(&input);
        }
        ratio
    };
    let full = settled_ratio(1.0);
    assert!(full < 0.05, "depth 1.0 did not reach unity: ratio {full}");
    let half = settled_ratio(0.5);
    assert!((0.4..=0.6).contains(&half), "depth 0.5 did not halve the signal: ratio {half}");
    // Depth 0.9 settles at gain ≈ 0.1: deeper than half, never full.
    let bass_default = settled_ratio(MixRole::Bass.duck_depth());
    assert!(bass_default < 0.2 && bass_default > full, "unexpected settling at 0.9: {bass_default}");
}

#[test]
fn duck_release_setter_retimes_the_pump() {
    let tone = |i: usize| 0.5 * (std::f32::consts::TAU * 300.0 * i as f32 / SR as f32).sin();
    // One key hit, then watch the recovery across 10 tiles (13.3 ms): the
    // duck target is constant within a tile, so only the multi-tile key
    // decay separates the two release τ values.
    let tail_ratio = |release_ms: f32| {
        let mut mix = AutoMixer::new(SR);
        mix.set_role(2, MixRole::Pad);
        mix.set_duck_release_ms(release_ms);
        mix.kick(1.0);
        let mut ratio = 1.0f32;
        for tile in 0..10u32 {
            let input: Vec<f32> =
                (0..crate::BLOCK_FRAMES).map(|k| tone(tile as usize * crate::BLOCK_FRAMES + k)).collect();
            let mut out = input.clone();
            mix.process_track(2, &mut out);
            ratio = rms(&out) / rms(&input);
        }
        ratio
    };
    let fast = tail_ratio(DUCK_RELEASE_MIN_MS);
    let slow = tail_ratio(500.0);
    assert!(fast > slow + 0.1, "release setter had no effect: fast {fast} vs slow {slow}");
}
