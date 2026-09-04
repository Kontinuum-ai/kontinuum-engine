//! Graph-level behavior tests — the full pre-inversion suite, moved into
//! the first-party pack crate because the harness (kontinuum-core) contains
//! no instrument code (issue #51). Same fixtures, same assertions.

use kontinuum_core::fx::{Delay, Reverb, Saturate};
use kontinuum_core::mix::MUTE_FADE_MS;
use kontinuum_core::params;
use kontinuum_core::graph::VoiceFactory;
use kontinuum_core::graph::SampleTuning;
use kontinuum_core::{fnv1a64, AudioGraph, MixRole};
use kontinuum_instruments_core::registry;
use std::sync::Arc;
use kontinuum_schedule::{Event, RampCurve, TrackId};
use kontinuum_core::slice::SliceTable;


fn build_graph() -> AudioGraph {
    let mut g = AudioGraph::new(48_000);
    g.attach_with(0, &test_factory("kick"));
    g.attach_with(1, &test_factory("hat"));
    g.attach_with(2, &test_factory("bass"));
    g.attach_with(3, &test_factory("pad"));
    g.set_insert(0, 0, Box::new(Saturate::new(1.2)));
    g.set_send_fx(Box::new(Delay::new(48_000)), Box::new(Reverb::new(48_000)));
    g.snap_track_gain(0, 1.0);
    g.snap_track_gain(1, 0.8);
    g.snap_track_gain(2, 0.9);
    g.snap_track_gain(3, 0.7);
    g.set_track_send(1, 0, 0.3);
    g.set_track_send(3, 1, 0.4);
    g
}

fn four_bar_events() -> Vec<(u32, TrackId, Event)> {
    let bar_frames = 96_000u32;
    let mut ev = vec![];
    for bar in 0..4u32 {
        let base = bar * bar_frames;
        for beat in 0..4u32 {
            ev.push((
                base + beat * 24_000,
                0u8,
                Event::NoteOn { voice: (bar * 4 + beat) as u8, pitch: 60.0, velocity: 0.95, microtiming_ticks: 0 },
            ));
        }
        for e in 0..8u32 {
            let v = if e % 2 == 1 { 0.5 } else { 0.3 };
            ev.push((base + e * 12_000, 1, Event::NoteOn { voice: e as u8, pitch: 60.0, velocity: v, microtiming_ticks: 0 }));
            ev.push((base + e * 12_000 + 6000, 1, Event::NoteOff { voice: e as u8 }));
        }
        for (e, pitch) in [(0u32, 36.0f32), (2, 39.0), (4, 36.0), (6, 41.0)] {
            ev.push((base + e * 12_000, 2, Event::NoteOn { voice: 0, pitch, velocity: 0.9, microtiming_ticks: 0 }));
            ev.push((base + e * 12_000 + 11_000, 2, Event::NoteOff { voice: 0 }));
        }
        for (v, pitch) in [(0u8, 48.0f32), (1, 55.0), (2, 60.0)] {
            ev.push((base, 3, Event::NoteOn { voice: v, pitch, velocity: 0.6, microtiming_ticks: 0 }));
            ev.push((base + 95_000, 3, Event::NoteOff { voice: v }));
        }
        if bar == 2 {
            ev.push((base, 2, Event::ParamRamp { param: params::BASS_CUTOFF, target: 2400.0, duration_frames: 48_000, curve: RampCurve::Smooth }));
        }
        if bar == 3 {
            ev.push((base + 12_000, 1, Event::SampleTrigger { sample_id: 0, slice: 0, rate: 1.0 }));
        }
    }
    ev.sort_by_key(|(f, _, _)| *f);
    ev
}

fn hash_stereo(l: &[f32], r: &[f32]) -> u64 {
    let mut bytes = Vec::with_capacity(l.len() * 8);
    for (l, r) in l.iter().zip(r.iter()) {
        bytes.extend_from_slice(&l.to_bits().to_le_bytes());
        bytes.extend_from_slice(&r.to_bits().to_le_bytes());
    }
    fnv1a64(&bytes)
}

// -- #76 ducking tests ---------------------------------------------------
//
// Measurement (used by the envelope assertions below): the stereo mix
// is summed mono, band-limited by the crate's own one-pole LP
// (`fx::lp_coeff`), and split into 32 equal bins per beat; each bin
// holds the mean square energy phase-averaged across beats (beat 0 is
// skipped: it contains the source's own attack). Reported in dB. A deep
// bin right after the kick and a recovered bin before the next one is
// the pump.

const SR: u32 = 48_000;

fn test_factory(kind: &str) -> VoiceFactory {
    registry().voice_factory(kind).expect("test kind")
}
const BEAT_FRAMES: usize = 24_000; // 120 BPM

fn kick_bass_graph() -> AudioGraph {
    let mut g = AudioGraph::new(SR);
    g.attach_with(0, &test_factory("kick"));
    g.attach_with(1, &test_factory("bass"));
    // Mute the kick's own signal so band measurements isolate the
    // ducked bass; keying happens at event dispatch and is unaffected
    // by strip gain.
    g.snap_track_gain(0, 0.0);
    g.snap_track_gain(1, 1.0);
    g
}

/// 4-on-the-floor kick + one sustained bass note (gate held for the
/// whole render, so the tone runs unbroken across every kick). The
/// bass is low-passed to near-sine: the envelope tests measure the
/// 30–150 Hz band anyway, and the click test needs a source whose
/// natural per-sample slew stays well under the duck's move.
fn kick_bass_events(beats: u32) -> Vec<(u32, TrackId, Event)> {
    let beat = BEAT_FRAMES as u32;
    let mut ev = vec![
        (
            0,
            1u8,
            Event::ParamRamp {
                param: params::BASS_CUTOFF,
                target: 120.0,
                duration_frames: 1,
                curve: RampCurve::Linear,
            },
        ),
        (
            0,
            1u8,
            Event::NoteOn { voice: 0, pitch: 36.0, velocity: 0.9, microtiming_ticks: 0 },
        ),
    ];
    for b in 0..beats {
        ev.push((
            b * beat,
            0u8,
            Event::NoteOn { voice: b as u8, pitch: 60.0, velocity: 0.95, microtiming_ticks: 0 },
        ));
    }
    ev
}

fn beat_envelope_db(g: &mut AudioGraph, events: &[(u32, TrackId, Event)], cutoff_hz: f32) -> [f64; 32] {
    let beat = BEAT_FRAMES;
    let beats = 8usize;
    let total = beat * beats;
    let mut l = vec![0.0f32; total];
    let mut r = vec![0.0f32; total];
    g.render_block(&mut l, &mut r, events, 0);
    let a = kontinuum_core::fx::lp_coeff(SR as f32, cutoff_hz) as f64;
    let mut lp = 0.0f64;
    let mut acc = [0.0f64; 32];
    let bin = beat / 32;
    for i in beat..total {
        let m = 0.5 * (l[i] as f64 + r[i] as f64);
        lp += a * (m - lp);
        acc[(i % beat) / bin] += lp * lp;
    }
    let n = (beats - 1) as f64;
    let mut db = [0.0f64; 32];
    for (k, e) in acc.iter().enumerate() {
        db[k] = 10.0 * (e / n / bin as f64).max(1e-12).log10();
    }
    db
}

fn envelope_range(db: &[f64; 32]) -> f64 {
    db.iter().cloned().fold(f64::MIN, f64::max) - db.iter().cloned().fold(f64::MAX, f64::min)
}

#[test]
fn bass_band_dips_after_each_kick_and_pumps_across_the_beat() {
    let mut g = kick_bass_graph();
    let events = kick_bass_events(8);
    let db = beat_envelope_db(&mut g, &events, 150.0);
    let range = envelope_range(&db);
    assert!(range > 8.0, "no audible bass-band pump across the beat: {range:.1} dB");
    // Direction: bins right after the kick sit well below the
    // late-beat recovery.
    let early = db[1..5].iter().sum::<f64>() / 4.0;
    let late = db[26..32].iter().sum::<f64>() / 6.0;
    assert!(late - early > 6.0, "bass band does not dip after the kick: early {early:.1} dB, late {late:.1} dB");
}

#[test]
fn role_keyed_ducking_hat_defaults_to_role_and_role_change_alters_it() {
    // Hats duck by default — the path is role-keyed, no kind allowlist.
    let mut perc_g = hat_rig(None);
    assert!(
        (perc_g.track_duck_depth(1) - MixRole::Perc.duck_depth()).abs() < 1e-6,
        "hat did not default to its role's duck depth"
    );
    let events = hat_events();
    let perc_db = beat_envelope_db(&mut perc_g, &events, 12_000.0);
    // Re-assign the same hat track to the Bass role: the duck must
    // deepen to the role default.
    let mut bass_g = hat_rig(Some(MixRole::Bass));
    assert!((bass_g.track_duck_depth(1) - MixRole::Bass.duck_depth()).abs() < 1e-6);
    let bass_db = beat_envelope_db(&mut bass_g, &events, 12_000.0);
    let perc_range = envelope_range(&perc_db);
    let bass_range = envelope_range(&bass_db);
    assert!(perc_range > 1.5, "hats are not ducked at all at role default: {perc_range:.1} dB");
    assert!(
        bass_range > perc_range + 4.0,
        "role change did not alter ducking: perc {perc_range:.1} dB vs bass {bass_range:.1} dB"
    );
}

#[test]
fn duck_edges_are_click_free() {
    let mut g = kick_bass_graph();
    let events = kick_bass_events(3);
    let total = BEAT_FRAMES * 3;
    let mut l = vec![0.0f32; total];
    let mut r = vec![0.0f32; total];
    g.render_block(&mut l, &mut r, &events, 0);
    // Skip the bass voice's own attack (first beat); from there the
    // output is the low-passed bass tone shaped by the slewed duck,
    // with attack + release edges at each remaining kick.
    let mut max_delta = 0.0f32;
    let mut prev = 0.5 * (l[BEAT_FRAMES] + r[BEAT_FRAMES]);
    for i in BEAT_FRAMES..total {
        let m = 0.5 * (l[i] + r[i]);
        max_delta = max_delta.max((m - prev).abs());
        prev = m;
    }
    // Near-sine 65 Hz source slew ≈ 8e-3 per sample; the 5 ms duck
    // attack adds ≤ ~4e-3 at full amp. An unslewed depth-0.9 gain step
    // would be ≥ 0.3 at the signal peak.
    assert!(max_delta < 0.02, "click on duck edge: {max_delta}");
}

#[test]
fn single_ducking_path_ad_hoc_strip_duck_is_gone() {
    // Grep-level guard: the retired ad-hoc strip duck must stay retired —
    // the AutoMixer's duck node is the one live ducking implementation.
    let src = include_str!("../../../engine/kontinuum-core/src/graph.rs");
    // Needles assembled from fragments so this assertion cannot match
    // its own source text.
    let strip_duck = concat!("strip", ".duck");
    let duck_amount = concat!("DUCK", "_AMOUNT");
    let duck_release = concat!("DUCK", "_RELEASE", "_MS");
    assert!(!src.contains(strip_duck), "ad-hoc strip duck is live again");
    assert!(!src.contains(duck_amount), "global duck depth const is back");
    assert!(!src.contains(duck_release), "fixed duck release const is back");
    // And the live path is the mixer's.
    assert!(
        src.contains(concat!("mixer", ".process_track")) && src.contains(concat!("mixer", ".kick")),
        "AutoMixer is not wired into the render path"
    );
}

#[test]
fn master_tap_drains_exactly_what_was_rendered() {
    let mut g = build_graph();
    let mut tap = g.attach_master_tap(48_000);
    let empty: Vec<(u32, TrackId, Event)> = vec![];
    let mut l = [0.0f32; 64];
    let mut r = [0.0f32; 64];
    // Render some audible material through track 0.
    g.snap_track_gain(0, 1.0);
    for bar in 0..4u64 {
        let ev = [(0u32, 0u8, Event::NoteOn { voice: 0, pitch: 36.0, velocity: 0.9, microtiming_ticks: 0 })];
        g.render_block(&mut l, &mut r, &ev, bar * 64);
        for f in 0..64 {
            g.render_block(&mut l, &mut r, &empty, bar * 64 + f as u64 + 1);
        }
    }
    let (mut dl, mut dr) = (Vec::new(), Vec::new());
    let frames = tap.drain_stereo(&mut dl, &mut dr);
    assert!(frames > 0, "tap must capture rendered audio");
    assert_eq!(dl.len(), dr.len());
    assert!(dl.iter().any(|&s| s != 0.0), "tap captured silence only");
    assert_eq!(tap.dropped_frames(), 0, "capacity 1 s: nothing dropped");
}

#[test]
fn master_tap_never_blocks_and_counts_overruns() {
    let mut g = build_graph();
    // Minimum capacity (one tile) — render far more than that without
    // draining: the RT side must drop, never block or corrupt.
    let mut tap = g.attach_master_tap(64);
    let empty: Vec<(u32, TrackId, Event)> = vec![];
    let mut l = [0.0f32; 64];
    let mut r = [0.0f32; 64];
    for bar in 0..16u64 {
        g.render_block(&mut l, &mut r, &empty, bar * 64);
    }
    assert!(tap.dropped_frames() > 0, "overruns must be counted");
    let (mut dl, mut dr) = (Vec::new(), Vec::new());
    let n = tap.drain_stereo(&mut dl, &mut dr);
    assert_eq!(n, 64, "ring holds exactly one tile after overruns");
}

#[test]
fn master_tap_does_not_affect_output() {
    let with_tap_out = {
        let mut g = build_graph();
        let mut tap = g.attach_master_tap(4_096);
        let mut l = [0.0f32; 64];
        let mut r = [0.0f32; 64];
        g.snap_track_gain(0, 1.0);
        let mut out = Vec::new();
        for bar in 0..8u64 {
            let ev = [(0u32, 0u8, Event::NoteOn { voice: 0, pitch: 36.0, velocity: 0.9, microtiming_ticks: 0 })];
            g.render_block(&mut l, &mut r, &ev, bar * 64);
            out.extend_from_slice(&l);
            let _ = tap.drain_stereo(&mut Vec::new(), &mut Vec::new());
        }
        out
    };
    let without_tap_out = {
        let mut g = build_graph();
        let mut l = [0.0f32; 64];
        let mut r = [0.0f32; 64];
        g.snap_track_gain(0, 1.0);
        let mut out = Vec::new();
        for bar in 0..8u64 {
            let ev = [(0u32, 0u8, Event::NoteOn { voice: 0, pitch: 36.0, velocity: 0.9, microtiming_ticks: 0 })];
            g.render_block(&mut l, &mut r, &ev, bar * 64);
            out.extend_from_slice(&l);
        }
        out
    };
    assert_eq!(with_tap_out, without_tap_out, "the tap must be read-only");
}

#[test]
fn mute_is_bit_exact_silence_at_graph_level() {
    let mut g = build_graph();
    let mut l = [0.0f32; 64];
    let mut r = [0.0f32; 64];
    // 1s of kick hits every 0.484s (~23232 frames), like the facade test.
    let empty: Vec<(u32, TrackId, Event)> = vec![];
    let mut frame = 0u64;
    while frame < 48_000 {
        let ev = [(0u32, 0u8, Event::NoteOn { voice: 0, pitch: 36.0, velocity: 0.9, microtiming_ticks: 0 })];
        g.render_block(&mut l, &mut r, &ev, frame);
        for f in 1..2323u64 {
            g.render_block(&mut l, &mut r, &empty, frame + f);
        }
        frame += 2323;
    }
    g.set_track_mute(0, true);
    g.set_track_mute(1, true);
    g.set_track_mute(2, true);
    g.set_track_mute(3, true);
    let mut leaked = Vec::new();
    for f in 0..48_000u64 {
        l.fill(0.0);
        r.fill(0.0);
        g.render_block(&mut l, &mut r, &empty, f);
        if f >= 384 && l.iter().any(|&s| s != 0.0) {
            leaked.push(f);
        }
    }
    assert!(leaked.is_empty(), "graph-level mute leaks {} frames", leaked.len());
}

#[test]
fn stem_taps_capture_kick_on_the_kick_bus() {
    let mut g = build_graph();
    let mut taps = g.attach_stem_taps(48_000);
    let mut l = [0.0f32; 64];
    let mut r = [0.0f32; 64];
    let ev = [(0u32, 0u8, Event::NoteOn { voice: 0, pitch: 36.0, velocity: 0.9, microtiming_ticks: 0 })];
    g.render_block(&mut l, &mut r, &ev, 0);
    let empty: Vec<(u32, TrackId, Event)> = vec![];
    for f in 0..8u64 {
        g.render_block(&mut l, &mut r, &empty, f + 1);
    }
    let mut kick = Vec::new();
    let frames = taps.drain(0, &mut kick);
    assert!(frames > 0 && kick.iter().any(|&s| s != 0.0), "kick bus captured the hit");
    let mut pad = Vec::new();
    taps.drain(3, &mut pad);
    // Pad track was silent; its ring may be empty or zero-only.
    assert!(pad.iter().all(|&s| s == 0.0), "pad bus stayed silent");
}

#[test]
fn zipper_noise_below_threshold_on_gain_move() {
    let mut g = build_graph();
    g.snap_track_gain(0, 0.0);
    g.set_track_gain(0, 1.0);
    let mut l = [0.0f32; 1];
    let mut r = [0.0f32; 1];
    let empty: Vec<(u32, TrackId, Event)> = vec![];
    let mut max_step = 0.0f32;
    let mut prev = g.track_gain_value(0);
    for _ in 0..4800 {
        g.render_block(&mut l, &mut r, &empty, 0);
        let v = g.track_gain_value(0);
        max_step = max_step.max((v - prev).abs());
        prev = v;
    }
    // 20 ms one-pole at 48 kHz: worst per-frame step is ~0.001; an
    // unsmoothed jump would be 1.0. Threshold 0.01 = 1% of full scale/frame.
    assert!(max_step < 0.01, "zipper: max control step {max_step}");
    assert!((prev - 1.0).abs() < 0.05, "gain did not settle: {prev}");
}

#[test]
fn determinism_bit_identical_across_graphs_and_runs() {
    let events = four_bar_events();
    let total = 4 * 96_000;
    let mut hashes = vec![];
    for _ in 0..2 {
        let mut g = build_graph();
        for _ in 0..2 {
            g.reset();
            let mut l = vec![0.0f32; total];
            let mut r = vec![0.0f32; total];
            g.render_block(&mut l, &mut r, &events, 0);
            hashes.push(hash_stereo(&l, &r));
        }
    }
    assert!(hashes.windows(2).all(|w| w[0] == w[1]), "renders diverged: {hashes:?}");
}

#[test]
fn render_path_is_allocation_free() {
    let mut g = build_graph();
    let events = four_bar_events();
    let mut l = vec![0.0f32; 4800];
    let mut r = vec![0.0f32; 4800];
    let empty: Vec<(u32, TrackId, Event)> = vec![];
    assert_no_alloc::assert_no_alloc(|| g.render_block(&mut l, &mut r, &empty, 0));
    assert_no_alloc::assert_no_alloc(|| g.render_block(&mut l, &mut r, &events, 0));
}

#[test]
fn voice_stealing_caps_pool_and_stays_finite() {
    let mut g = build_graph();
    let mut ev: Vec<(u32, TrackId, Event)> = (0..16u8)
        .map(|i| {
            (
                (i as u32) * 100,
                0u8,
                Event::NoteOn { voice: i, pitch: 60.0, velocity: 1.0, microtiming_ticks: 0 },
            )
        })
        .collect();
    ev.push((2000, 0, Event::NoteOff { voice: 99 }));
    let mut l = vec![0.0f32; 4800];
    let mut r = vec![0.0f32; 4800];
    g.render_block(&mut l, &mut r, &ev, 0);
    assert!(g.track_active_voices(0) <= 8);
    assert!(l.iter().chain(r.iter()).all(|s| s.is_finite()));
}

#[test]
fn dense_render_smoke() {
    let mut g = build_graph();
    let events = four_bar_events();
    let total = 4 * 96_000;
    let mut l = vec![0.0f32; total];
    let mut r = vec![0.0f32; total];
    g.render_block(&mut l, &mut r, &events, 0);
    assert!(l.iter().chain(r.iter()).all(|s| s.is_finite()));
    let peak = l.iter().chain(r.iter()).fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(peak > 0.05, "dense render silent: peak {peak}");
    assert!(peak <= 1.0, "master limiter violated: peak {peak}");
}

#[test]
fn solo_silence_matches_mute_bit_for_bit_and_unsolo_restores() {
    let chunk = 12_000usize;
    let fade = (MUTE_FADE_MS * 0.001 * SR as f32).ceil() as usize;

    let (mut m, events) = two_source_graph();
    let (mut s, s_events) = two_source_graph();
    // Control: same fixture with the wavetable gated before the warmup,
    // so its fade is long dead — the control renders the pad alone.
    let (mut c, c_events) = two_source_graph();
    c.set_track_mute(1, true);
    let (pre, _) = render_span(&mut m, &events, 0, chunk);
    let (s_pre, _) = render_span(&mut s, &s_events, 0, chunk);
    let (_, _) = render_span(&mut c, &c_events, 0, chunk);
    assert!(pre.iter().fold(0.0f32, |m, s| m.max(s.abs())) > 0.05, "fixture silent");
    assert!(
        pre.iter().zip(s_pre.iter()).all(|(a, b)| a.to_bits() == b.to_bits()),
        "fixtures diverged before the gate"
    );

    // Muting the wavetable vs soloing the pad must produce identical
    // mixes frame for frame: the same KillFade ramp, times the same
    // gains, on the same voices.
    m.set_track_mute(1, true);
    s.set_track_solo(0, true);
    assert!(s.track_solo(0) && !s.track_solo(1));
    let (muted, _) = render_span(&mut m, &[], chunk as u64, chunk);
    let (soloed, _) = render_span(&mut s, &[], chunk as u64, chunk);
    assert!(
        muted.iter().zip(soloed.iter()).all(|(a, b)| a.to_bits() == b.to_bits()),
        "solo silence diverged from mute silence"
    );
    // Once the solo ramp lands, the mix is the soloed track alone at
    // its bit-exact passthrough level.
    let (control, _) = render_span(&mut c, &[], chunk as u64, chunk);
    assert!(
        soloed[fade..]
            .iter()
            .zip(control[fade..].iter())
            .all(|(a, b)| a.to_bits() == b.to_bits()),
        "solo tail is not the exact soloed-track signal"
    );

    // Releasing the gate restores the audio identically on both paths.
    m.set_track_mute(1, false);
    s.set_track_solo(0, false);
    assert!(!s.track_solo(0));
    let (m_back, _) = render_span(&mut m, &[], (chunk * 2) as u64, chunk);
    let (s_back, _) = render_span(&mut s, &[], (chunk * 2) as u64, chunk);
    assert!(m_back.iter().any(|v| *v != 0.0), "mute release stayed silent");
    assert!(
        m_back.iter().zip(s_back.iter()).all(|(a, b)| a.to_bits() == b.to_bits()),
        "unsolo restore diverged from unmute restore"
    );
}

#[test]
fn mute_and_solo_combine_multiplicatively() {
    let chunk = 12_000usize;
    let fade = (MUTE_FADE_MS * 0.001 * SR as f32).ceil() as usize;

    // Only the pad is audible (the soloable strip rides at zero gain):
    // soloing it silences the whole mix, and the pad's own mute must
    // not be able to leak through the separate solo gate.
    let (mut g, events) = two_source_graph();
    g.snap_track_gain(1, 0.0);
    let (_, _) = render_span(&mut g, &events, 0, chunk);
    g.set_track_solo(1, true);
    let (silenced, _) = render_span(&mut g, &[], chunk as u64, chunk);
    assert!(silenced[fade..].iter().all(|v| *v == 0.0), "solo did not silence the other track");

    g.set_track_mute(0, true);
    let (_, _) = render_span(&mut g, &[], (chunk * 2) as u64, chunk);
    // Un-muting under the active solo must stay silent: the mute fade
    // reopens but the separate solo fade is still closed, and the gain
    // is the product of both.
    g.set_track_mute(0, false);
    let (still_silent, _) = render_span(&mut g, &[], (chunk * 3) as u64, chunk);
    assert!(
        still_silent.iter().all(|v| *v == 0.0),
        "unmute leaked through the active solo"
    );

    g.set_track_solo(1, false);
    let (back, _) = render_span(&mut g, &[], (chunk * 4) as u64, chunk);
    assert!(back[fade..].iter().any(|v| *v != 0.0), "clearing the solo stayed silent");
}

#[test]
fn solo_transitions_follow_the_solo_count() {
    let chunk = 12_000usize;
    let fade = (MUTE_FADE_MS * 0.001 * SR as f32).ceil() as usize;

    // Control: same fixture, never gated — the bit-exact baseline the
    // gated graph must converge to once its fades reopen.
    let (mut c, events) = two_source_graph();
    c.snap_track_gain(1, 0.0);
    let (_, _) = render_span(&mut c, &events, 0, chunk);
    let mut control: Vec<Vec<f32>> = Vec::new();
    for k in 1..5u64 {
        let (mono, _) = render_span(&mut c, &[], k * chunk as u64, chunk);
        control.push(mono);
    }

    let (mut g, events) = two_source_graph();
    g.snap_track_gain(1, 0.0);
    let (_, _) = render_span(&mut g, &events, 0, chunk);

    // 0 -> 1: the playing pad is not soloed, so it fades out.
    g.set_track_solo(1, true);
    let (first, _) = render_span(&mut g, &[], chunk as u64, chunk);
    assert!(first[fade..].iter().all(|v| *v == 0.0), "first solo did not close the rest");

    // 1 -> 2: soloing the pad reopens exactly its own gate.
    g.set_track_solo(0, true);
    let (second, _) = render_span(&mut g, &[], (chunk * 2) as u64, chunk);
    assert!(
        second[fade..]
            .iter()
            .zip(control[1][fade..].iter())
            .all(|(a, b)| a.to_bits() == b.to_bits()),
        "second solo did not restore the soloed track"
    );

    // 2 -> 1: releasing one solo must not touch the other soloed track.
    g.set_track_solo(1, false);
    assert!(g.track_solo(0) && !g.track_solo(1));
    let (third, _) = render_span(&mut g, &[], (chunk * 3) as u64, chunk);
    assert!(
        third.iter().zip(control[2].iter()).all(|(a, b)| a.to_bits() == b.to_bits()),
        "un-soloing a sibling disturbed the remaining solo"
    );

    // 1 -> 0: the last release opens every gate.
    g.set_track_solo(0, false);
    assert!(!g.track_solo(0));
    let (fourth, _) = render_span(&mut g, &[], (chunk * 4) as u64, chunk);
    assert!(
        fourth.iter().zip(control[3].iter()).all(|(a, b)| a.to_bits() == b.to_bits()),
        "clearing the last solo did not restore passthrough"
    );
}

// -- #19 sample trigger slice playback ------------------------------------

/// Two DC halves: the rendered level names the region being played.
fn step_sample() -> Arc<[f32]> {
    let mut data = vec![0.25f32; 48_000];
    for v in data[24_000..].iter_mut() {
        *v = 0.75;
    }
    data.into()
}

fn sample_trigger(track: u8, slice: u16, rate: f32) -> Vec<(u32, TrackId, Event)> {
    // The frame-64 event closes the first dispatch span, so the frame-0
    // trigger applies after the first tile (voice 99 is outside the map).
    vec![
        (0, track, Event::SampleTrigger { sample_id: 0, slice, rate }),
        (64, track, Event::NoteOff { voice: 99 }),
    ]
}

#[test]
fn sample_trigger_plays_the_requested_slice() {
    let voiced_of = |sample: Arc<[f32]>, slices: SliceTable, slice: u16| -> Vec<f32> {
        let mut g = AudioGraph::new(SR);
        // Slice-content identity is a sampler contract: bit-exact voiced
        // streams need the bypassed path (the chain's group delay would
        // otherwise reorder the filtered tail).
        g.set_mastering_bypass(true);
        g.attach_sampler_with_slices(0, sample, SR, slices, SampleTuning::default());
        g.snap_track_gain(0, 1.0);
        let events = sample_trigger(0, slice, 1.0);
        let (mono, _) = render_span(&mut g, &events, 0, 96_000);
        let voiced: Vec<f32> = mono.into_iter().filter(|v| *v != 0.0).collect();
        assert!(!voiced.is_empty(), "slice trigger rendered silence");
        voiced
    };

    // Reference: a buffer that is ONLY the second region's content,
    // played as the full-buffer slice. Both runs start at the same
    // frame with the same amplitude and produce the same samples, so
    // the mix chain (including the gain servo) behaves identically and
    // the voiced stretches must be bit-identical.
    let reference_voiced =
        voiced_of(vec![0.75f32; 48_000].into(), Arc::from(Vec::new()), 0);

    let mut data = vec![0.25f32; 48_000];
    for v in data[24_000..].iter_mut() {
        *v = 0.75;
    }
    let sliced = voiced_of(data.into(), vec![0, 24_000].into(), 1);

    let n = sliced.len().min(reference_voiced.len());
    assert!(n > 12_000, "slice render suspiciously short: {n}");
    assert!(
        sliced[..n]
            .iter()
            .zip(reference_voiced[..n].iter())
            .all(|(a, b)| a.to_bits() == b.to_bits()),
        "slice 1 did not play the second region's content"
    );
    // The region is 24_000 frames and the round-robin offset eats into
    // it, so a full pass can never reach the boundary length.
    assert!(sliced.len() < 24_000, "slice overran its boundary: {}", sliced.len());
}

#[test]
fn sample_trigger_out_of_range_slice_clamps_to_the_last() {
    let build = || {
        let mut g = AudioGraph::new(SR);
        g.attach_sampler_with_slices(0, step_sample(), SR, vec![0, 24_000].into(), SampleTuning::default());
        g.snap_track_gain(0, 1.0);
        g
    };
    let voiced = |slice: u16| -> Vec<f32> {
        let mut g = build();
        let events = sample_trigger(0, slice, 1.0);
        let (mono, _) = render_span(&mut g, &events, 0, 96_000);
        let voiced: Vec<f32> = mono.into_iter().filter(|v| *v != 0.0).collect();
        assert!(!voiced.is_empty(), "clamped slice silent");
        voiced
    };
    let clamped = voiced(u16::MAX);
    let last = voiced(1);
    assert_eq!(clamped.len(), last.len(), "clamp changed the region length");
    assert!(
        clamped.iter().zip(last.iter()).all(|(a, b)| a.to_bits() == b.to_bits()),
        "clamp missed the last slice"
    );
}

#[test]
fn slice_zero_without_a_table_is_the_full_buffer_one_shot() {
    // Reference: the pre-slice path — a note-on over the same buffer.
    // Span dispatch applies an event at the END of the span it opens,
    // so the mid-voice gate release needs its own span closer.
    let note_events: Vec<(u32, TrackId, Event)> = vec![
        (0, 0, Event::NoteOn { voice: 0, pitch: 60.0, velocity: 1.0, microtiming_ticks: 0 }),
        (64, 0, Event::NoteOff { voice: 0 }),
        (128, 0, Event::NoteOff { voice: 99 }),
    ];
    let run = |events: &[(u32, TrackId, Event)]| {
        let mut g = AudioGraph::new(SR);
        g.attach_sampler(0, step_sample(), SR);
        g.snap_track_gain(0, 1.0);
        render_span(&mut g, events, 0, 49_000).0
    };
    let sliced = run(&sample_trigger(0, 0, 1.0));
    let one_shot = run(&note_events);
    assert!(
        sliced.iter().zip(one_shot.iter()).all(|(a, b)| a.to_bits() == b.to_bits()),
        "slice 0 with an empty table diverged from the full-buffer one-shot"
    );
}

#[test]
fn sample_trigger_sequences_render_bit_identically() {
    let slices: SliceTable = vec![0, 12_000, 24_000].into();
    let events: Vec<(u32, TrackId, Event)> = vec![
        (0, 0, Event::SampleTrigger { sample_id: 0, slice: 0, rate: 1.0 }),
        (6_000, 0, Event::SampleTrigger { sample_id: 0, slice: 2, rate: 1.25 }),
        (12_000, 0, Event::SampleTrigger { sample_id: 0, slice: 1, rate: 0.5 }),
    ];
    let run = || {
        let mut g = AudioGraph::new(SR);
        g.attach_sampler_with_slices(0, step_sample(), SR, Arc::clone(&slices), SampleTuning::default());
        g.snap_track_gain(0, 1.0);
        render_span(&mut g, &events, 0, 48_000).0
    };
    let a = run();
    let b = run();
    assert_eq!(a.len(), b.len());
    assert!(a.iter().zip(b.iter()).all(|(x, y)| x.to_bits() == y.to_bits()));
}


    fn hat_rig(role: Option<MixRole>) -> AudioGraph {
        let mut g = AudioGraph::new(SR);
        g.attach_with(0, &test_factory("kick"));
        g.attach_with(1, &test_factory("hat"));
        if let Some(r) = role {
            g.set_track_role(1, r);
        }
        g.snap_track_gain(0, 0.0);
        g.snap_track_gain(1, 1.0);
        g
    }

    /// transient.
    fn hat_events() -> Vec<(u32, TrackId, Event)> {
        let beat = BEAT_FRAMES as u32;
        let mut ev = vec![(
            0,
            1u8,
            Event::ParamRamp {
                param: params::HAT_DECAY_MS,
                target: 3_000.0,
                duration_frames: 1,
                curve: RampCurve::Linear,
            },
        )];
        for b in 0..8u32 {
            ev.push((
                b * beat,
                0u8,
                Event::NoteOn { voice: b as u8, pitch: 60.0, velocity: 0.95, microtiming_ticks: 0 },
            ));
            for half in 0..2u32 {
                ev.push((
                    b * beat + half * (beat / 2),
                    1u8,
                    Event::NoteOn {
                        voice: (b * 2 + half) as u8,
                        pitch: 60.0,
                        velocity: 0.8,
                        microtiming_ticks: 0,
                    },
                ));
            }
        }
        ev.sort_by_key(|(f, _, _)| *f);
        ev
    }

    /// cannot preserve.
    fn two_source_graph() -> (AudioGraph, Vec<(u32, TrackId, Event)>) {
        let mut g = AudioGraph::new(SR);
        g.set_mastering_bypass(true);
        g.attach_with(0, &test_factory("pad"));
        g.attach_with(1, &test_factory("wavetable"));
        g.snap_track_gain(0, 0.8);
        g.snap_track_gain(1, 0.8);
        let events: Vec<(u32, TrackId, Event)> = vec![
            (
                0,
                0,
                Event::ParamRamp {
                    param: params::PAD_ATTACK_MS,
                    target: 5.0,
                    duration_frames: 1,
                    curve: RampCurve::Linear,
                },
            ),
            (1, 0, Event::NoteOn { voice: 0, pitch: 55.0, velocity: 0.8, microtiming_ticks: 0 }),
            (1, 1, Event::NoteOn { voice: 0, pitch: 60.0, velocity: 0.8, microtiming_ticks: 0 }),
            (64, 0, Event::NoteOff { voice: 99 }),
        ];
        (g, events)
    }

    /// mono-summed output plus its worst per-sample delta.
    fn render_span(
        g: &mut AudioGraph,
        events: &[(u32, TrackId, Event)],
        start: u64,
        frames: usize,
    ) -> (Vec<f32>, f32) {
        let mut l = vec![0.0f32; frames];
        let mut r = vec![0.0f32; frames];
        g.render_block(&mut l, &mut r, events, start);
        let mut max_delta = 0.0f32;
        for w in l.windows(2) {
            max_delta = max_delta.max((w[1] - w[0]).abs());
        }
        let mono = l.iter().zip(r.iter()).map(|(l, r)| 0.5 * (l + r)).collect();
        (mono, max_delta)
    }
