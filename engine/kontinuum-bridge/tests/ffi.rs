//! FFI roundtrip tests: CStrings through the C ABI from Rust, error paths,
//! null handling, and telemetry marshalling (issue #12).

use std::ffi::{CStr, CString};

use kontinuum_bridge::ffi::{
    kontinuum_abi_version, kontinuum_engine_apply_diff, kontinuum_engine_free,
    kontinuum_engine_new, kontinuum_engine_play, kontinuum_engine_render,
    kontinuum_engine_stop, kontinuum_engine_telemetry, kontinuum_last_error,
    kontinuum_set_track_mute, kontinuum_set_track_solo, kontinuum_string_free, TelemetryFFI,
    KONTINUUM_ABI_VERSION,
};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/loop-4track.ir.json");
const FRAMES: u32 = 512;

fn fixture_cstring() -> CString {
    let json = std::fs::read_to_string(FIXTURE).expect("read fixture");
    CString::new(json).expect("no NULs in fixture")
}

fn last_error() -> Option<String> {
    let p = kontinuum_last_error();
    if p.is_null() {
        return None;
    }
    // SAFETY: the bridge guarantees the returned pointer is a valid C string
    // until the next bridge call on this thread.
    Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
}

#[test]
fn abi_version_is_five() {
    assert_eq!(kontinuum_abi_version(), KONTINUUM_ABI_VERSION);
    assert_eq!(
        KONTINUUM_ABI_VERSION, 5,
        "5 = export_masters (#102); 4 = set_track_instrument (#97); 3 = history frames (#90)"
    );
}

#[test]
fn full_roundtrip_new_play_render_telemetry_free() {
    let session = fixture_cstring();
    // SAFETY: session is a valid C string; the returned pointer is freed below.
    let engine = unsafe { kontinuum_engine_new(48_000, session.as_ptr()) };
    assert!(!engine.is_null(), "engine must construct from the fixture");

    // SAFETY: live engine pointer.
    assert_eq!(unsafe { kontinuum_engine_play(engine) }, 0);

    let mut l = vec![0.0f32; FRAMES as usize];
    let mut r = vec![0.0f32; FRAMES as usize];
    for _ in 0..64 {
        // SAFETY: buffers outlive the call with FRAMES writable f32s each.
        unsafe { kontinuum_engine_render(engine, l.as_mut_ptr(), r.as_mut_ptr(), FRAMES) };
    }
    assert!(l.iter().chain(&r).all(|s| s.is_finite()), "FFI render must stay finite");
    let peak = l.iter().chain(&r).fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(peak > 0.01, "audio must flow through the C ABI, peak {peak}");

    // SAFETY: `out` is a valid TelemetryFFI location.
    let mut t = TelemetryFFI::default();
    assert_eq!(unsafe { kontinuum_engine_telemetry(engine, &mut t) }, 0);
    assert!(t.playing);
    assert!(t.has_active);
    assert!(t.playhead_bar > 0.0);
    assert_eq!(t.render_gaps, 0);
    assert!(
        t.queue_len >= 4,
        "pump keeps a 32-bar lookahead queued: {}",
        t.queue_len
    );

    // SAFETY: live engine pointer.
    assert_eq!(unsafe { kontinuum_engine_stop(engine) }, 0);
    // SAFETY: live engine pointer.
    let mut stopped = vec![1.0f32; FRAMES as usize];
    unsafe { kontinuum_engine_render(engine, stopped.as_mut_ptr(), stopped.as_mut_ptr(), FRAMES) };
    assert!(stopped.iter().all(|s| s.abs() < f32::EPSILON), "stopped engine is silent");

    // Diff through the ABI: replace the b_groove kick pattern for bar 8+.
    let diff = CString::new(
        r#"{"op":"replace_pattern","section":"c_break","track":"kick",
            "pattern":{"generator":"euclidean","k":8,"n":16,"rot":2}}"#,
    )
    .unwrap();
    // SAFETY: live engine + valid diff string.
    assert_eq!(unsafe { kontinuum_engine_apply_diff(engine, diff.as_ptr(), 8) }, 0);

    // SAFETY: single free of the owned pointer.
    unsafe { kontinuum_engine_free(engine) };
}

#[test]
fn bar_history_frames_carry_measured_loudness_through_the_abi() {
    use kontinuum_bridge::ffi::{kontinuum_engine_ui_history, kontinuum_engine_ui_snapshot,
                               KontinuumBarFrameFFI, KontinuumTrackUiFFI, KontinuumUiSnapshotFFI};

    let session = fixture_cstring();
    // SAFETY: valid C string.
    let engine = unsafe { kontinuum_engine_new(48_000, session.as_ptr()) };
    assert!(!engine.is_null());
    // SAFETY: live engine pointer.
    assert_eq!(unsafe { kontinuum_engine_play(engine) }, 0);

    // ~10 s of render with snapshot polls so bars finalize against the meter.
    let mut l = vec![0.0f32; FRAMES as usize];
    let mut r = vec![0.0f32; FRAMES as usize];
    let chunks = (10.0 * 48_000.0 / FRAMES as f64) as usize;
    for i in 0..chunks {
        // SAFETY: buffers outlive the call with FRAMES writable f32s each.
        unsafe { kontinuum_engine_render(engine, l.as_mut_ptr(), r.as_mut_ptr(), FRAMES) };
        if i % 15 == 0 {
            let mut snap = KontinuumUiSnapshotFFI {
                bar: 0.0, beat_phase: 0.0, energy: 0.0, section_index: 0,
                bar_in_section: 0, section_bars: 0, playing: false,
                tracks: [KontinuumTrackUiFFI { onsets: 0, velocity: 0.0, pitch: 0.0 }; 12],
                current_masks: [0; 12],
            };
            // SAFETY: `snap` is a valid output location.
            assert_eq!(unsafe { kontinuum_engine_ui_snapshot(engine, &mut snap) }, 0);
        }
    }

    let mut out = [KontinuumBarFrameFFI {
        energy: 0.0, onsets: [0; 12], masks: [0; 12], last_velocity: [0.0; 12],
        rms: 0.0, peak: 0.0, section_index: 0,
    }; 12];
    // SAFETY: live engine; `out` holds 12 writable frames.
    let count = unsafe { kontinuum_engine_ui_history(engine, out.as_mut_ptr(), 12) };
    assert!(count >= 4, "history finalizes past the first bars: {count}");
    for f in &out[..count as usize] {
        assert!(f.rms.is_finite() && f.peak.is_finite(), "meter stays finite");
        assert!((0.0..=1.0).contains(&f.rms) && (0.0..=1.0).contains(&f.peak));
        assert!(f.rms <= f.peak + 1e-6, "rms cannot exceed peak: {} vs {}", f.rms, f.peak);
    }
    assert!(
        out[..count as usize].iter().any(|f| f.rms > 0.0),
        "the waveform draws measured audio, not just the section scalar"
    );

    // SAFETY: live engine pointer.
    unsafe { kontinuum_engine_stop(engine) };
    // SAFETY: single free of the owned pointer.
    unsafe { kontinuum_engine_free(engine) };
}

#[test]
fn bad_json_yields_null_and_last_error() {
    let bad = CString::new("{\"version\": \"nope\"}").unwrap();
    // SAFETY: valid C string.
    let engine = unsafe { kontinuum_engine_new(48_000, bad.as_ptr()) };
    assert!(engine.is_null(), "bad JSON must yield a null engine");
    let msg = last_error().expect("last_error must carry the parse failure");
    assert!(!msg.is_empty());
    assert!(msg.to_lowercase().contains("parse"), "unexpected message: {msg}");
}

#[test]
fn null_and_empty_arguments_are_contained() {
    // SAFETY: all-null arguments exercise the null paths of every export.
    unsafe {
        kontinuum_engine_free(std::ptr::null_mut());
        assert_eq!(kontinuum_engine_play(std::ptr::null_mut()), 1);
        assert_eq!(kontinuum_engine_stop(std::ptr::null_mut()), 1);
        assert_eq!(kontinuum_engine_apply_diff(std::ptr::null_mut(), std::ptr::null(), 0), 1);
        kontinuum_engine_render(
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            FRAMES,
        );
        let mut t = TelemetryFFI::default();
        assert_eq!(kontinuum_engine_telemetry(std::ptr::null_mut(), &mut t), 1);

        let session = fixture_cstring();
        assert!(
            kontinuum_engine_new(48_000, std::ptr::null()).is_null(),
            "null session string must yield null"
        );
        let engine = kontinuum_engine_new(48_000, session.as_ptr());
        assert!(!engine.is_null());
        assert_eq!(kontinuum_engine_apply_diff(engine, std::ptr::null(), 0), 2);
        assert!(last_error().is_some_and(|m| m.contains("null")));
        kontinuum_engine_free(engine);
        kontinuum_string_free(std::ptr::null_mut());
    }
}

#[test]
fn rejected_diff_sets_error_code_and_message() {
    let session = fixture_cstring();
    // SAFETY: valid C string.
    let engine = unsafe { kontinuum_engine_new(48_000, session.as_ptr()) };
    assert!(!engine.is_null());
    let bad_diff = CString::new(
        r#"{"op":"replace_pattern","section":"missing","track":"kick",
            "pattern":{"generator":"euclidean","k":4,"n":16}}"#,
    )
    .unwrap();
    // SAFETY: live engine + valid diff string.
    let code = unsafe { kontinuum_engine_apply_diff(engine, bad_diff.as_ptr(), 0) };
    assert_eq!(code, 2, "unknown section must be a rejected diff");
    assert!(last_error().is_some_and(|m| m.contains("unknown section")));
    // SAFETY: live engine pointer.
    unsafe { kontinuum_engine_free(engine) };
}

#[test]
fn mute_and_solo_roundtrip_through_the_c_abi() {
    // Two tracks: soloing the kick must silence the hat while the kick stays
    // audible; muting both must leave the stream bit-exact silent.
    let two_tracks = CString::new(
        r#"{
            "version": 1, "seed": 5,
            "tempo_lane": [[0, 124.0]],
            "sections": [{"id": "a", "bars": 4, "energy_curve": [0.5],
                "pattern_bindings": {"kick": {"generator": "euclidean", "k": 4, "n": 16},
                                     "hat": {"generator": "euclidean", "k": 7, "n": 16}}}],
            "tracks": [{"id": "kick", "role": "kick", "instrument": {"kind": "kick"}},
                       {"id": "hat", "role": "perc", "instrument": {"kind": "hat"}}]
        }"#,
    )
    .unwrap();

    // SAFETY: null engine exercises the null path.
    unsafe {
        assert_eq!(kontinuum_set_track_mute(std::ptr::null_mut(), 0, true), 1);
        assert_eq!(kontinuum_set_track_solo(std::ptr::null_mut(), 0, true), 1);
    }

    // SAFETY: valid C string.
    let engine = unsafe { kontinuum_engine_new(48_000, two_tracks.as_ptr()) };
    assert!(!engine.is_null(), "engine must construct from the inline session");
    // SAFETY: live engine pointer.
    assert_eq!(unsafe { kontinuum_engine_play(engine) }, 0);

    let mut l = vec![0.0f32; FRAMES as usize];
    let mut r = vec![0.0f32; FRAMES as usize];
    // render accumulates onto the caller's buffer: zero per callback.
    let mut render_rms = |n: u32| -> f32 {
        let mut sum_sq = 0.0f64;
        for _ in 0..n {
            l.iter_mut().for_each(|s| *s = 0.0);
            r.iter_mut().for_each(|s| *s = 0.0);
            // SAFETY: buffers outlive the call with FRAMES writable f32s each.
            unsafe { kontinuum_engine_render(engine, l.as_mut_ptr(), r.as_mut_ptr(), FRAMES) };
            sum_sq += l.iter().chain(&r).map(|s| (*s as f64) * (*s as f64)).sum::<f64>();
        }
        (sum_sq / (2 * FRAMES * n) as f64).sqrt() as f32
    };
    let window = 125; // ~1.3 s: a bar is ~1.9 s at 124 bpm, so averages are stable
    let full = render_rms(window);
    assert!(full > 0.001, "two-track mix must be audible: {full}");

    // SAFETY: live engine pointer.
    assert_eq!(unsafe { kontinuum_set_track_solo(engine, 0, true) }, 0);
    let soloed = render_rms(window);
    assert!(soloed < full, "soloing the kick must silence the hat: {soloed} vs {full}");
    assert!(soloed > 0.0005, "the soloed kick must stay audible: {soloed}");

    // SAFETY: live engine pointer.
    assert_eq!(unsafe { kontinuum_set_track_solo(engine, 0, false) }, 0);
    // SAFETY: live engine pointer.
    assert_eq!(unsafe { kontinuum_set_track_mute(engine, 0, true) }, 0);
    // SAFETY: live engine pointer.
    assert_eq!(unsafe { kontinuum_set_track_mute(engine, 1, true) }, 0);
    // The 8 ms fades finish inside the first 512-frame callback. The
    // mastering chain (#82) then replays the fade tail through its ≤
    // 104-frame lookahead during chunk 1; from chunk 2 on the stream is
    // bit-exact silent.
    let mut exact_silent = true;
    for chunk in 0..32 {
        l.iter_mut().for_each(|s| *s = 0.0);
        r.iter_mut().for_each(|s| *s = 0.0);
        // SAFETY: buffers outlive the call with FRAMES writable f32s each.
        unsafe { kontinuum_engine_render(engine, l.as_mut_ptr(), r.as_mut_ptr(), FRAMES) };
        if chunk > 1 {
            exact_silent &= l.iter().chain(&r).all(|s| *s == 0.0);
        }
    }
    assert!(exact_silent, "muting every track must fade the mix to exact silence");

    // SAFETY: single free of the owned pointer.
    unsafe { kontinuum_engine_free(engine) };
}
