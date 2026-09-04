//! Facade tests for [`KontinuumEngine`]: render loop, boundary switching with
//! live diffs, gap accounting, and the RT alloc-free property (issue #10/#12).

use assert_no_alloc::assert_no_alloc;
use kontinuum_bridge::KontinuumEngine;
use kontinuum_core::fnv1a64;
use kontinuum_clock::TempoLane;

const SR: u32 = 48_000;
const CHUNK: usize = 512;
const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/loop-4track.ir.json");

fn fixture_json() -> String {
    std::fs::read_to_string(FIXTURE).expect("read fixture")
}

fn new_engine() -> KontinuumEngine {
    KontinuumEngine::new(SR, &fixture_json()).expect("engine from fixture")
}

fn render_seconds(engine: &mut KontinuumEngine, seconds: f64) -> (Vec<f32>, usize) {
    let frames = (seconds * f64::from(SR)) as usize;
    let mut mono_mix = Vec::with_capacity(frames);
    let mut l = [0.0f32; CHUNK];
    let mut r = [0.0f32; CHUNK];
    let mut chunks = 0usize;
    while chunks * CHUNK < frames {
        engine.render(&mut l, &mut r);
        mono_mix.extend_from_slice(&l);
        chunks += 1;
    }
    (mono_mix, chunks)
}

fn rms(buf: &[f32]) -> f32 {
    (buf.iter().map(|s| s * s).sum::<f32>() / buf.len().max(1) as f32).sqrt()
}

fn peak(buf: &[f32]) -> f32 {
    buf.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

#[test]
fn render_loop_advances_eight_seconds_in_sync_with_the_tempo_lane() {
    let mut engine = new_engine();
    engine.play();
    // Mid-session window first (lookahead assertions need queued blocks), then
    // the full 8 s window; every sample is checked for finiteness.
    let (first, first_chunks) = render_seconds(&mut engine, 4.5);

    let mid = engine.telemetry();
    assert!(mid.playing);
    assert!(mid.queue_len > 0, "lookahead must stay primed mid-session");
    assert!(mid.active_block_bar.is_some());
    assert_eq!(mid.render_gaps, 0, "no gaps while the session is running");

    let (rest, rest_chunks) = render_seconds(&mut engine, 3.5);
    let samples: Vec<f32> = first.into_iter().chain(rest).collect();
    assert_eq!(samples.len(), (first_chunks + rest_chunks) * CHUNK);
    assert!(
        samples.iter().all(|s| s.is_finite()),
        "NaN/inf in the render output"
    );
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(peak > 0.05, "8 s of playback must be audible, peak {peak}");

    let frames = ((first_chunks + rest_chunks) * CHUNK) as u64;
    assert_eq!(engine.playhead_frame(), frames, "playhead must track rendered frames exactly");

    // Note: `TempoLane::time_at_bar` integrates 60·L/bpm seconds for L bars,
    // i.e. one bar per beat (its own `seconds_per_bar_at` returns 240/bpm).
    // compile_session feeds bars through the same mapping, so the engine is
    // frame-coherent with the compiled blocks; the 4x wall-clock rate of the
    // "bar" unit is a pre-existing kontinuum-clock inconsistency, not a
    // bridge artifact. The bridge pins playhead_bar to the lane.
    let lane = TempoLane::constant(SR, 124.0).unwrap();
    let bar = engine.playhead_bar();
    assert!(
        (bar - lane.bar_at_frame(frames)).abs() < 1e-9,
        "playhead_bar must match the transport lane: {bar}"
    );
    assert_eq!(engine.telemetry().invalid_diffs, 0);
}

#[test]
fn stopped_engine_renders_silence_and_freezes_playhead() {
    let mut engine = new_engine();
    let mut l = [0.0f32; CHUNK];
    let mut r = [0.0f32; CHUNK];
    engine.render(&mut l, &mut r);
    assert!(l.iter().chain(&r).all(|s| s.abs() < f32::EPSILON));
    assert_eq!(engine.playhead_frame(), 0);
    assert!(!engine.is_playing());
    assert_eq!(engine.telemetry().playhead_bar, 0.0);
}

#[test]
fn boundary_switch_applies_diff_at_bar_eight_without_gap() {
    let mut engine = new_engine();
    engine.play();
    // Drive the playhead to mid-block (block 4..8 is sounding at bar 7.5).
    while engine.playhead_bar() < 7.5 {
        engine.render(&mut [0.0; CHUNK], &mut [0.0; CHUNK]);
    }
    let gaps_before = engine.telemetry().render_gaps;
    let active_before = engine.telemetry().active_block_bar;

    let diff = r#"{"op":"replace_pattern","section":"c_break","track":"kick",
        "pattern":{"generator":"euclidean","k":8,"n":16,"rot":2}}"#;
    let outcome = engine.apply_diff_json(diff, 8).expect("diff applies at bar 8");
    assert_eq!(outcome.applied, vec!["replace_pattern:kick@c_break".to_string()]);

    // Render past the boundary; the new block (bars 8..12) must activate.
    while engine.playhead_bar() < 9.5 {
        engine.render(&mut [0.0; CHUNK], &mut [0.0; CHUNK]);
    }
    let t = engine.telemetry();
    assert_eq!(t.active_block_bar, Some(8), "new block must be live past bar 8");
    assert_ne!(t.active_block_bar, active_before);
    assert_eq!(t.render_gaps, gaps_before, "boundary switch must not gap");
    assert!(t.queue_len > 0, "remaining blocks must be queued after refill");
}

#[test]
fn rejected_diff_counts_invalid_and_keeps_playing() {
    let mut engine = new_engine();
    engine.play();
    // Section `a_intro` starts at bar 0 < at_bar 8 → InPast.
    let diff = r#"{"op":"replace_pattern","section":"a_intro","track":"kick",
        "pattern":{"generator":"euclidean","k":1,"n":16}}"#;
    assert!(engine.apply_diff_json(diff, 8).is_err());
    assert_eq!(engine.telemetry().invalid_diffs, 1);

    let garbage = "not json";
    assert!(matches!(
        engine.apply_diff_json(garbage, 0),
        Err(kontinuum_bridge::EngineError::DiffParse(_))
    ));
    assert_eq!(engine.telemetry().invalid_diffs, 1, "parse failures are not apply rejections");

    // Stream keeps flowing.
    engine.render(&mut [0.0; CHUNK], &mut [0.0; CHUNK]);
    assert!(engine.is_playing());
}

#[test]
fn session_end_loops_with_the_pump_and_never_goes_silent() {
    let short = r#"{
        "version": 1, "seed": 1,
        "tempo_lane": [[0, 124.0]],
        "sections": [{"id": "a", "bars": 4, "energy_curve": [0.5],
            "pattern_bindings": {"k": {"generator": "euclidean", "k": 4, "n": 16}}}],
        "tracks": [{"id": "k", "role": "kick", "instrument": {"kind": "kick"}}]
    }"#;
    let mut engine = KontinuumEngine::new(SR, short).expect("short session");
    engine.play();
    // Without the pump the 4-bar session would starve at 7.742 s; with it the
    // stream loops and stays gapless past two session lengths.
    let total_seconds = 18.0;
    let chunks_per_second = SR as usize / CHUNK;
    let mut l = [0.0f32; CHUNK];
    let mut r = [0.0f32; CHUNK];
    let mut audible_after_end = false;
    let mut chunks = 0usize;
    while (chunks * CHUNK) as f64 / f64::from(SR) < total_seconds {
        engine.render(&mut l, &mut r);
        if (chunks * CHUNK) as f64 / f64::from(SR) > 9.0 && l.iter().any(|s| s.abs() > 0.01) {
            audible_after_end = true;
        }
        chunks += 1;
        if chunks % chunks_per_second == 0 {
            engine.pump();
        }
    }
    let end_frame = TempoLane::constant(SR, 124.0).unwrap().frame_of_bar(4.0) as usize;
    assert!(end_frame > 0);
    assert!(audible_after_end, "looped lap must be audible past the session end");
    assert!(
        engine.telemetry().render_gaps == 0,
        "no gaps across the loop once the pump is live"
    );
}

#[test]
fn render_path_is_allocation_free() {
    let mut engine = new_engine();
    engine.play();
    // Warm up outside the guard: first call drains the queue and activates
    // block 0 (Vec warm-up + event-list ownership transfer).
    for _ in 0..2 {
        engine.render(&mut [0.0; CHUNK], &mut [0.0; CHUNK]);
    }
    // Steady-state RT callback: no allocation. (Block swaps deallocate the
    // outgoing block; assert_no_alloc only guards allocation, and swaps are
    // avoided inside the guard anyway by staying well inside block 0.)
    let mut l = [0.0f32; CHUNK];
    let mut r = [0.0f32; CHUNK];
    for _ in 0..4 {
        assert_no_alloc(|| engine.render(&mut l, &mut r));
    }
    assert!(engine.playhead_frame() == 6 * CHUNK as u64);
}

#[test]
fn invalid_sessions_are_rejected_with_formatted_errors() {
    // Out-of-range gain (L2 bounds) must surface as a formatted error list.
    let bad = r#"{
        "version": 1, "seed": 1,
        "tempo_lane": [[0, 124.0]],
        "sections": [{"id": "a", "bars": 4, "energy_curve": [0.5],
            "pattern_bindings": {"k": {"generator": "euclidean", "k": 4, "n": 16}}}],
        "tracks": [{"id": "k", "role": "kick", "instrument": {"kind": "kick"}, "gain": 9.0}]
    }"#;
    let err = match KontinuumEngine::new(SR, bad) {
        Err(e) => e,
        Ok(_) => panic!("invalid session must be rejected"),
    };
    let msg = err.to_string();
    assert!(msg.contains("session failed validation"), "{msg}");
}

#[test]
fn pump_keeps_playback_alive_across_the_session_loop() {
    let mut engine = new_engine();
    engine.play();
    let total_seconds = 75.0; // 16-bar session ≈ 31 s; run well past its end
    let chunks_per_second = SR as usize / CHUNK;
    let mut l = [0.0f32; CHUNK];
    let mut r = [0.0f32; CHUNK];
    let mut chunks = 0usize;
    while (chunks * CHUNK) as f64 / f64::from(SR) < total_seconds {
        engine.render(&mut l, &mut r);
        chunks += 1;
        if chunks % chunks_per_second == 0 {
            engine.pump(); // the UI timer cadence
            assert!(engine.telemetry().queue_len > 0, "queue starved at chunk {chunks}");
        }
    }
    let t = engine.telemetry();
    assert!(t.playhead_bar > 16.0, "playhead must be past the session end: {}", t.playhead_bar);
    assert_eq!(t.render_gaps, 0, "no gaps allowed across the loop with a live pump");
    assert!(t.queue_len > 0);
}

#[test]
fn ui_snapshot_streams_masks_and_energy() {
    let mut engine = new_engine();
    engine.play();
    let chunks_per_second = SR as usize / CHUNK;
    let mut l = [0.0f32; CHUNK];
    let mut r = [0.0f32; CHUNK];
    let mut last = engine.ui_snapshot();
    for i in 0..(chunks_per_second * 14) {
        engine.render(&mut l, &mut r);
        if i % (chunks_per_second / 10) == 0 {
            last = engine.ui_snapshot();
        }
    }
    assert!(last.bar > 4.0, "playhead advanced: {}", last.bar);
    assert!(last.energy > 0.0 && last.energy <= 1.0, "energy in range: {}", last.energy);
    assert!(last.tracks[0].onsets > 0 || !last.current_masks.is_empty(),
        "kick activity must be visible");
    let hist = engine.ui_history_copy_owned(64);
    assert!(!hist.is_empty());
    let with_hits = hist.iter().filter(|f| f.onsets.iter().any(|o| *o > 0)).count();
    assert!(with_hits > 0, "history bars must carry onsets");
    let with_masks = hist.iter().filter(|f| f.masks.iter().any(|m| *m != 0)).count();
    assert!(with_masks > 0, "history bars must carry masks");
}

#[test]
fn no_lap_is_ever_the_same() {
    let short = r#"{
        "version": 1, "seed": 99,
        "tempo_lane": [[0, 124.0]],
        "sections": [{"id": "a", "bars": 4, "energy_curve": [0.5, 0.6, 0.5, 0.6],
            "pattern_bindings": {"k": {"generator": "euclidean", "k": 4, "n": 16, "rot": 0, "velocity": 0.8}}},
            {"id": "b", "bars": 4, "energy_curve": [0.6, 0.7, 0.6, 0.7],
            "pattern_bindings": {"k": {"generator": "euclidean", "k": 4, "n": 16, "rot": 1, "velocity": 0.9},
                                  "p": {"steps": [{"position": 0, "velocity": 0.5, "pitch": 55.0, "gate": 3.5}]}}}],
        "tracks": [{"id": "k", "role": "kick", "instrument": {"kind": "kick"}},
                   {"id": "p", "role": "pad", "instrument": {"kind": "pad", "attack_ms": 200.0, "release_ms": 400.0}}]
    }"#;
    let mut engine = KontinuumEngine::new(SR, short).expect("engine");
    engine.play();
    let chunks_per_second = SR as usize / CHUNK;
    let mut l = [0.0f32; CHUNK];
    let mut r = [0.0f32; CHUNK];
    let mut hashes = vec![];
    let mut frames_this_lap = 0usize;
    let lap_seconds = 8.0f64;
    for _ in 0..(chunks_per_second * lap_seconds as usize * 3) {
        engine.render(&mut l, &mut r);
        frames_this_lap += CHUNK;
        engine.pump();
        if frames_this_lap >= chunks_per_second * 8 {
            let bytes: Vec<u8> = l.iter().chain(r.iter()).flat_map(|s| s.to_le_bytes()).collect();
            hashes.push(fnv1a64(&bytes));
            frames_this_lap = 0;
        }
    }
    // Session is ~15.5 s; three 8 s windows span laps 0, 1, 2. The variation
    // pass must make each lap's tail unique.
    assert!(hashes.len() >= 3, "expected 3 windows, got {}", hashes.len());
    assert_ne!(hashes[0], hashes[1], "lap 1 must differ from lap 0");
    assert_ne!(hashes[1], hashes[2], "lap 2 must differ from lap 1");
}

#[test]
fn mute_drives_the_track_to_exact_silence_and_back() {
    let single = r#"{
        "version": 1, "seed": 3,
        "tempo_lane": [[0, 124.0]],
        "sections": [{"id": "a", "bars": 4, "energy_curve": [0.5],
            "pattern_bindings": {"k": {"generator": "euclidean", "k": 4, "n": 16}}}],
        "tracks": [{"id": "k", "role": "kick", "instrument": {"kind": "kick"}}]
    }"#;
    let mut engine = KontinuumEngine::new(SR, single).expect("single-track session");
    engine.play();

    // render_block accumulates onto the caller's buffer, so every callback
    // gets a fresh zeroed buffer — the contract a real host honors too.
    fn render_fresh(engine: &mut KontinuumEngine) -> Vec<f32> {
        let mut window = Vec::with_capacity(SR as usize);
        for _ in 0..SR as usize / CHUNK {
            let mut l = [0.0f32; CHUNK];
            let mut r = [0.0f32; CHUNK];
            engine.render(&mut l, &mut r);
            window.extend_from_slice(&l);
        }
        window
    }

    let before = render_fresh(&mut engine);
    assert!(rms(&before) > 0.001, "kick must be audible: {}", rms(&before));

    engine.set_track_mute(0, true);
    assert!(engine.track_muted(0));
    let muted = render_fresh(&mut engine);
    // The 8 ms kill fade fits inside the first 512-frame chunk; from frame
    // 512 on the strip's gain path is bit-exact zero. The mastering chain
    // (#82) adds a ≤ 104-frame lookahead shift (measured: last nonzero
    // sample at frame 578, peak residue 6.6e-4), so chunk 1 carries that
    // tail and from chunk 2 on the output is exactly silent again.
    let residue = muted[CHUNK..].iter().fold(0.0f32, |a, s| a.max(s.abs()));
    let last_nonzero = muted.iter().rposition(|s| s.abs() > 0.0);
    assert!(
        muted[CHUNK..CHUNK * 2].iter().all(|s| s.abs() < 1e-3),
        "muted output must decay within one chunk of the fade (residue {residue}, last nonzero {last_nonzero:?})"
    );
    assert!(muted[CHUNK * 2..].iter().all(|s| *s == 0.0), "muted output must be exactly silent");

    engine.set_track_mute(0, false);
    assert!(!engine.track_muted(0));
    let back = render_fresh(&mut engine);
    assert!(rms(&back) > 0.001, "unmute must restore the signal, rms {}", rms(&back));
}

#[test]
fn solo_isolates_one_track_and_releases_the_others() {
    // Two engines run the fixture deterministically side by side so the solo
    // window, the release window and the reference all cover the same bars
    // (inside b_groove, bars 4-8, where all eight tracks play).
    let mut full_engine = new_engine();
    let mut solo_engine = new_engine();
    full_engine.play();
    solo_engine.play();
    let _ = render_seconds(&mut full_engine, 9.0);
    let _ = render_seconds(&mut solo_engine, 9.0);

    // Mastering (#82) is a loudness maximizer: glue makeup lifts the soloed
    // (much quieter) mix nearly back to the full level, masking exactly the
    // level relationships this test verifies, and re-enabling the chain
    // resets its seekers (cold vs warm chains diverge for seconds). Solo
    // gating is a mix-path contract, so every level comparison here runs
    // with the chain bypassed; mastering behavior has its own tests.
    full_engine.set_mastering_bypass(true);
    solo_engine.set_mastering_bypass(true);

    let (full_pre, _) = render_seconds(&mut full_engine, 4.0);
    let level_full = rms(&full_pre);

    solo_engine.set_track_solo(0, true);
    assert!(solo_engine.track_solo(0));
    let (isolated, _) = render_seconds(&mut solo_engine, 4.0);
    let level_isolated = rms(&isolated);
    assert!(
        level_isolated < level_full * 0.85,
        "soloing the kick must drop the mix: {level_isolated} vs {level_full}"
    );
    assert!(peak(&isolated) > 0.01, "the soloed track must stay audible: {}", peak(&isolated));

    let (full_post, _) = render_seconds(&mut full_engine, 4.0);
    let level_full_post = rms(&full_post);

    solo_engine.set_track_solo(0, false);
    assert!(!solo_engine.track_solo(0));
    let (released, _) = render_seconds(&mut solo_engine, 4.0);
    let level_released = rms(&released);
    // The kick dominates this fixture, so with the sub-octave at 0.22 the
    // soloed level sits within a few percent of the reopened mix; the reference
    // match below is what proves the solo actually released.
    assert!(
        level_released > level_isolated * 0.95,
        "clearing the solo must bring the mix back toward its level: {level_released} vs {level_isolated}"
    );
    // The reopen fade plus the reverb tail accumulated during the solo make
    // an exact match impossible; the level must still land on the reference.
    assert!(
        (level_released - level_full_post).abs() < level_full_post * 0.15,
        "released mix must match the un-soloed reference: {level_released} vs {level_full_post}"
    );
}

#[test]
fn live_playback_honors_compiler_automation_lanes() {
    // The fixture automates the pad's reverb send in b_groove (bars 4-8) and
    // c_break (bars 8-12). A lane-less clone must sound identical through the
    // un-automated intro and diverge once the lanes land — if live retargeting
    // were dropped, both engines would render bit-identically everywhere.
    let mut variant: serde_json::Value = serde_json::from_str(&fixture_json()).expect("fixture is JSON");
    for section in variant["sections"].as_array_mut().expect("sections") {
        section.as_object_mut().expect("section object").remove("automation");
    }
    let stripped = serde_json::to_string(&variant).expect("reserialize");

    let mut with_lanes = new_engine();
    let mut without_lanes = KontinuumEngine::new(SR, &stripped).expect("lane-less session");
    with_lanes.play();
    without_lanes.play();

    let (laned, _) = render_seconds(&mut with_lanes, 15.0);
    let (plain, _) = render_seconds(&mut without_lanes, 15.0);
    assert_eq!(laned.len(), plain.len());

    let intro = SR as usize * 7; // bars 0..4 have no automation at all
    let intro_diff = laned[..intro].iter().zip(&plain[..intro]).fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
    assert!(intro_diff < 1e-6, "un-automated bars must be untouched: {intro_diff}");

    let groove_diff = laned[intro..].iter().zip(&plain[intro..]).fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
    assert!(groove_diff > 1e-4, "live playback must honor the pad's send_reverb lanes: {groove_diff}");
}

#[test]
fn mastering_bypass_toggles_to_bit_exact_passthrough() {
    // Two engines, same fixture, same render schedule: one runs the chain
    // enabled then bypassed, the reference runs bypassed throughout. The
    // chain is feed-forward, so after the toggle the streams must agree
    // bit-for-bit from that point on.
    let mut live = new_engine();
    let mut reference = new_engine();
    live.play();
    reference.play();
    reference.set_mastering_bypass(true);
    assert!(reference.telemetry().mastering.bypassed);
    assert!(!live.telemetry().mastering.bypassed);

    let (live_enabled, _) = render_seconds(&mut live, 2.0);
    let (ref_enabled, _) = render_seconds(&mut reference, 2.0);
    assert!(
        live_enabled.iter().zip(&ref_enabled).any(|(a, b)| a.to_bits() != b.to_bits()),
        "enabled chain must process the mix (streams identical)"
    );

    live.set_mastering_bypass(true);
    let (live_after, _) = render_seconds(&mut live, 2.0);
    let (ref_after, _) = render_seconds(&mut reference, 2.0);
    assert_eq!(live_after.len(), ref_after.len());
    let live_bits: Vec<u32> = live_after.iter().map(|s| s.to_bits()).collect();
    let ref_bits: Vec<u32> = ref_after.iter().map(|s| s.to_bits()).collect();
    assert_eq!(live_bits, ref_bits, "bypass must be bit-exact passthrough");
}

#[test]
fn enabled_mastering_keeps_output_finite_bounded_and_alloc_free() {
    let mut engine = new_engine();
    engine.play();
    // Warm up outside the guard (queue drain + block activation), as in
    // render_path_is_allocation_free.
    for _ in 0..2 {
        engine.render(&mut [0.0; CHUNK], &mut [0.0; CHUNK]);
    }
    let mut l = [0.0f32; CHUNK];
    let mut r = [0.0f32; CHUNK];
    let mut peak = 0.0f32;
    let mut audible = false;
    for _ in 0..64 {
        assert_no_alloc(|| engine.render(&mut l, &mut r));
        for s in l.iter().chain(r.iter()) {
            assert!(s.is_finite(), "NaN/inf through the mastering chain");
            if s.abs() > 0.0 {
                audible = true;
            }
            peak = peak.max(s.abs());
        }
    }
    assert!(audible, "enabled mastering went silent");
    assert!(peak <= 1.0, "peak {peak} exceeds 0 dBFS through the chain");
    let m = engine.telemetry().mastering;
    assert!(!m.bypassed, "chain must be enabled by default");
    assert!(!m.limiter_gr_alarm, "program material must not latch the alarm");
}

#[test]
fn enabled_mastering_renders_bit_identically_across_engines() {
    let mut a = new_engine();
    let mut b = new_engine();
    a.play();
    b.play();
    let (la, _) = render_seconds(&mut a, 4.0);
    let (lb, _) = render_seconds(&mut b, 4.0);
    assert_eq!(la.len(), lb.len());
    let bits_a: Vec<u32> = la.iter().map(|s| s.to_bits()).collect();
    let bits_b: Vec<u32> = lb.iter().map(|s| s.to_bits()).collect();
    assert_eq!(bits_a, bits_b, "two identical runs diverged through mastering");
}

#[test]
fn debug_mute_residue() {
    let single = r#"{
        "version": 1, "seed": 3,
        "tempo_lane": [[0, 124.0]],
        "sections": [{"id": "a", "bars": 4, "energy_curve": [0.5],
            "pattern_bindings": {"k": {"generator": "euclidean", "k": 4, "n": 16}}}],
        "tracks": [{"id": "k", "role": "kick", "instrument": {"kind": "kick"}}]
    }"#;
    let mut engine = KontinuumEngine::new(SR, single).expect("engine");
    engine.play();
    let _ = render_seconds(&mut engine, 1.0);
    engine.set_track_mute(0, true);
    let (muted, _) = render_seconds(&mut engine, 1.0);
    let worst = muted.iter().enumerate().fold((0usize, 0.0f32), |(wi, wv), (i, s)| {
        if s.abs() > wv.abs() { (i, *s) } else { (wi, wv) }
    });
    println!("worst sample: idx {} val {worst:?} (chunk {})", worst.0, worst.0 / CHUNK);
    for (i, s) in muted.iter().enumerate() {
        if s.abs() > 0.0 && i >= CHUNK {
            println!("first nonzero past chunk 0: idx {i} val {s}");
            break;
        }
    }
}

#[test]
fn sample_attaches_while_playing_at_a_block_boundary() {
    let single = r#"{
        "version": 1, "seed": 3,
        "tempo_lane": [[0, 124.0]],
        "sections": [{"id": "a", "bars": 4, "energy_curve": [0.5],
            "pattern_bindings": {"k": {"generator": "euclidean", "k": 4, "n": 16}}}],
        "tracks": [{"id": "k", "role": "kick", "instrument": {"kind": "kick"}}]
    }"#;
    let mut plain = KontinuumEngine::new(SR, single).expect("session");
    let mut loaded = KontinuumEngine::new(SR, single).expect("session");

    fn render_second(engine: &mut KontinuumEngine) -> Vec<f32> {
        let mut window = Vec::with_capacity(SR as usize);
        for _ in 0..SR as usize / CHUNK {
            let mut l = [0.0f32; CHUNK];
            let mut r = [0.0f32; CHUNK];
            engine.render(&mut l, &mut r);
            window.extend_from_slice(&l);
        }
        window
    }

    plain.play();
    loaded.play();

    // Identical engines render bit-identically up to the attach point.
    let plain_first = render_second(&mut plain);
    let loaded_first = render_second(&mut loaded);
    assert_eq!(
        plain_first.iter().map(|s| s.to_bits()).collect::<Vec<_>>(),
        loaded_first.iter().map(|s| s.to_bits()).collect::<Vec<_>>(),
        "engines must be bit-identical before the attach"
    );
    assert!(rms(&loaded_first) > 0.001, "kick must be audible");

    // Attach a Nyquist square burst WHILE PLAYING (#53 step 3b): accepted
    // (the old path errored with "stop the transport"), applied by the audio
    // thread at the next block boundary.
    let burst: Vec<f32> = (0..1920).map(|i| if i % 2 == 0 { 0.9 } else { -0.9 }).collect();
    loaded.load_sample(0, &burst, SR).expect("attach must be accepted while playing");

    let plain_second = render_second(&mut plain);
    let loaded_second = render_second(&mut loaded);
    assert!(
        plain_second.iter().zip(&loaded_second).any(|(a, b)| a.to_bits() != b.to_bits()),
        "the loaded sample must take over the track after the boundary"
    );

    // The square burst dominates frame-to-frame deltas: sample-to-sample
    // jumps of ~2·0.9·velocity versus the kick's smooth decaying tone.
    let max_delta = |w: &[f32]| {
        w.windows(2).map(|p| (p[0] - p[1]).abs()).fold(0.0f32, f32::max)
    };
    let burst_frames = |w: &[f32]| w.windows(2).filter(|p| (p[0] - p[1]).abs() > 0.4).count();
    let after_delta = max_delta(&loaded_second);
    assert!(after_delta > 0.4, "burst must be audible through the chain: {after_delta}");
    assert!(
        burst_frames(&loaded_second) > burst_frames(&plain_second) * 5 + 10,
        "sampler must dominate the track (burst frames {} vs kick {})",
        burst_frames(&loaded_second),
        burst_frames(&plain_second)
    );

    // Queue-full degrades to an error, never a block or panic.
    for _ in 0..16 {
        let _ = loaded.load_sample(0, &burst, SR);
    }
    for _ in 0..4 {
        let _ = render_second(&mut loaded);
    }
}
