//! C ABI for the iOS shell (issue #12). Every exported function:
//! - accepts nullable pointers and checks them,
//! - wraps its body in `catch_unwind` so no panic ever crosses the boundary
//!   (error paths report through return codes + [`kontinuum_last_error`]),
//! - keeps the RT render entry allocation- and lock-free.
//!
//! ABI version: [`kontinuum_abi_version`] returns 5 for this surface
//! (5 = `kontinuum_export_masters`, the #102 deliverable-export seam;
//! 4 = `kontinuum_engine_set_track_instrument`, the #97 library-preset
//! seam; 3 = per-bar RMS/peak/section_index on the history frames, issue
//! #90; 2 = per-track arrays widened to `MAX_TRACKS` + session track
//! descriptors, issue #89).
//!
//! Memory contract: `kontinuum_engine_new` returns an owned opaque pointer,
//! released exactly once with `kontinuum_engine_free`. `kontinuum_last_error`
//! returns a thread-local buffer owned by the bridge (valid until the next
//! bridge call on the same thread).

use std::cell::RefCell;
use std::ffi::{c_char, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;

use kontinuum_core::MAX_TRACKS;

use crate::engine::{EngineError, KontinuumEngine, MasteringTelemetryLite, Telemetry};
use kontinuum_mastering::OutputProfile;

/// Version of this C ABI. Bump on any signature/semantic change.
/// 2: mastering controls + telemetry surfaced (#82).
/// 3: desktop shell session surface (macOS phase 1) — supersedes our 2.
/// 4: kontinuum_engine_set_track_instrument, the #97 library-preset seam.
/// 5: kontinuum_export_masters, the #102 deliverable-export seam.
pub const KONTINUUM_ABI_VERSION: u32 = 5;

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_last_error(msg: String) {
    let msg = CString::new(msg)
        .unwrap_or_else(|_| CString::from(c"error message contained interior NUL"));
    LAST_ERROR.with(|slot| *slot.borrow_mut() = Some(msg));
}

fn last_error_ptr() -> *const c_char {
    LAST_ERROR.with(|slot| {
        match slot.borrow().as_ref() {
            Some(c) => c.as_ptr(),
            None => ptr::null(),
        }
    })
}

/// `Ok` from the closure, or `Err(())` if it panicked. Every FFI call funnels
/// through here for panic containment.
fn guarded<T>(f: impl FnOnce() -> T) -> Option<T> {
    catch_unwind(AssertUnwindSafe(f)).ok()
}

/// Null-checked `*const c_char` → owned Rust string.
///
/// # Safety
/// `p` must be null or point to a valid NUL-terminated UTF-8 C string.
unsafe fn opt_str(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: caller guarantees validity per the contract above.
    Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
}

/// `#[repr(C)]` telemetry mirror for Swift (see `ios/Kontinuum/KontinuumBridge.h`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct TelemetryFFI {
    pub playhead_bar: f64,
    pub playing: bool,
    pub queue_len: u32,
    pub active_bar: u32,
    pub has_active: bool,
    pub render_gaps: u64,
    pub invalid_diffs: u64,
}

/// Ground-truth UI snapshot per track (onsets drain since the last call).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct KontinuumTrackUiFFI {
    pub onsets: u32,
    pub velocity: f32,
    pub pitch: f32,
}

/// Ground-truth UI snapshot for the living interface (issue #33). Per-track
/// arrays cover `MAX_TRACKS` slots (issue #89: all 12 engine tracks reach
/// the UI; index = session track order).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct KontinuumUiSnapshotFFI {
    pub bar: f64,
    pub beat_phase: f64,
    pub energy: f32,
    pub section_index: u32,
    pub bar_in_section: u32,
    pub section_bars: u32,
    pub playing: bool,
    pub tracks: [KontinuumTrackUiFFI; MAX_TRACKS],
    pub current_masks: [u32; MAX_TRACKS],
}

impl From<Telemetry> for TelemetryFFI {
    fn from(t: Telemetry) -> Self {
        TelemetryFFI {
            playhead_bar: t.playhead_bar,
            playing: t.playing,
            queue_len: t.queue_len as u32,
            active_bar: t.active_block_bar.unwrap_or(0),
            has_active: t.active_block_bar.is_some(),
            render_gaps: t.render_gaps,
            invalid_diffs: t.invalid_diffs,
        }
    }
}

/// `#[repr(C)]` mastering working point for Swift (see
/// `ios/Kontinuum/KontinuumBridge.h`); mirrors `MasteringTelemetryLite`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MasteringTelemetryFFI {
    pub tilt_db: f32,
    pub glue_gr_db: f32,
    pub clipper_gr_db: f32,
    pub limiter_gr_db: f32,
    pub limiter_gr_alarm: bool,
    pub bypassed: bool,
}

impl From<MasteringTelemetryLite> for MasteringTelemetryFFI {
    fn from(m: MasteringTelemetryLite) -> Self {
        MasteringTelemetryFFI {
            tilt_db: m.tilt_db,
            glue_gr_db: m.glue_gr_db,
            clipper_gr_db: m.clipper_gr_db,
            limiter_gr_db: m.limiter_gr_db,
            limiter_gr_alarm: m.limiter_gr_alarm,
            bypassed: m.bypassed,
        }
    }
}

// -- ABI ---------------------------------------------------------------------

/// Returns the ABI version of this bridge (currently 4).
#[no_mangle]
pub extern "C" fn kontinuum_abi_version() -> u32 {
    KONTINUUM_ABI_VERSION
}

/// Creates an engine from session JSON. Returns null on parse/validation/
/// compile failure (or panic); the reason is in `kontinuum_last_error`.
///
/// # Safety
/// `session_json` must be null or a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn kontinuum_engine_new(
    sample_rate: u32,
    session_json: *const c_char,
) -> *mut KontinuumEngine {
    let result = guarded(|| {
        let Some(json) = (unsafe { opt_str(session_json) }) else {
            return Err(EngineError::SessionParse("session_json is null".into()));
        };
        KontinuumEngine::new(sample_rate, &json)
    });
    match result {
        Some(Ok(engine)) => Box::into_raw(Box::new(engine)),
        Some(Err(e)) => {
            set_last_error(e.to_string());
            ptr::null_mut()
        }
        None => {
            set_last_error("panic while constructing the engine".into());
            ptr::null_mut()
        }
    }
}

/// Releases an engine. Null is ignored; double-free is the caller's bug
/// (the Swift handle is `close`-idempotent to make that impossible).
///
/// # Safety
/// `engine` must be null or a pointer from `kontinuum_engine_new` not yet freed.
#[no_mangle]
pub unsafe extern "C" fn kontinuum_engine_free(engine: *mut KontinuumEngine) {
    if engine.is_null() {
        return;
    }
    // SAFETY: per contract, sole owner; drop on the caller's thread.
    let _ = guarded(|| unsafe { drop(Box::from_raw(engine)) });
}

/// Starts the transport. 0 = ok, 1 = null engine, 2 = internal panic.
///
/// # Safety
/// `engine` must be null or a live engine pointer.
#[no_mangle]
pub unsafe extern "C" fn kontinuum_engine_play(engine: *mut KontinuumEngine) -> i32 {
    match guarded(|| unsafe { engine.as_mut() }.map(KontinuumEngine::play)) {
        Some(Some(())) => 0,
        Some(None) => 1,
        None => 2,
    }
}

/// Stops the transport (render then outputs silence). 0/1/2 as `…_play`.
///
/// # Safety
/// `engine` must be null or a live engine pointer.
#[no_mangle]
pub unsafe extern "C" fn kontinuum_engine_stop(engine: *mut KontinuumEngine) -> i32 {
    match guarded(|| unsafe { engine.as_mut() }.map(KontinuumEngine::stop)) {
        Some(Some(())) => 0,
        Some(None) => 1,
        None => 2,
    }
}

/// Control thread: mute/unmute one track (click-free kill fade; safe while
/// playing). 0 = ok, 1 = null engine, 2 = internal panic.
///
/// # Safety
/// `engine` must be null or a live engine pointer.
#[no_mangle]
pub unsafe extern "C" fn kontinuum_set_track_mute(
    engine: *mut KontinuumEngine,
    track: u8,
    muted: bool,
) -> i32 {
    match guarded(|| unsafe { engine.as_mut() }.map(|e| e.set_track_mute(track, muted))) {
        Some(Some(())) => 0,
        Some(None) => 1,
        None => 2,
    }
}

/// Control thread: mastering controls (#82) — bypass (bit-exact A/B /
/// kill-switch first rung), tilt target (dB, clamped ±3) and section
/// energy (0 = full intensity, 1 = breakdown). Safe while playing.
/// 0 = ok, 1 = null engine, 2 = internal panic.
///
/// # Safety
/// `engine` must be null or a live engine pointer.
#[no_mangle]
pub unsafe extern "C" fn kontinuum_set_mastering(
    engine: *mut KontinuumEngine,
    bypassed: bool,
    tilt_db: f32,
    section_energy: f32,
) -> i32 {
    match guarded(|| unsafe { engine.as_mut() }.map(|e| {
        e.set_mastering_bypass(bypassed);
        e.set_mastering_tilt(tilt_db);
        e.set_mastering_section_energy(section_energy);
    })) {
        Some(Some(())) => 0,
        Some(None) => 1,
        None => 2,
    }
}

fn output_profile_from_code(code: u32) -> Option<OutputProfile> {
    match code {
        0 => Some(OutputProfile::Full),
        1 => Some(OutputProfile::SmallSpeaker),
        _ => None,
    }
}

/// Control thread: speaker-aware output profile (#82) — `profile` 0 =
/// Full, 1 = SmallSpeaker (built-in speaker). 0 = ok, 1 = null engine,
/// 2 = invalid profile value or internal panic.
///
/// # Safety
/// `engine` must be null or a live engine pointer.
#[no_mangle]
pub unsafe extern "C" fn kontinuum_set_mastering_output_profile(
    engine: *mut KontinuumEngine,
    profile: u32,
) -> i32 {
    if engine.is_null() {
        return 1;
    }
    let Some(parsed) = output_profile_from_code(profile) else {
        set_last_error(format!(
            "unknown output profile {profile} (0 = Full, 1 = SmallSpeaker)"
        ));
        return 2;
    };
    match guarded(|| unsafe { engine.as_mut() }.map(|e| e.set_mastering_output_profile(parsed))) {
        Some(Some(())) => 0,
        Some(None) => 1,
        None => 2,
    }
}

/// Control thread: solo one track (every other strip fades out; safe while
/// playing). 0 = ok, 1 = null engine, 2 = internal panic.
///
/// # Safety
/// `engine` must be null or a live engine pointer.
#[no_mangle]
pub unsafe extern "C" fn kontinuum_set_track_solo(
    engine: *mut KontinuumEngine,
    track: u8,
    solo: bool,
) -> i32 {
    match guarded(|| unsafe { engine.as_mut() }.map(|e| e.set_track_solo(track, solo))) {
        Some(Some(())) => 0,
        Some(None) => 1,
        None => 2,
    }
}

/// Non-RT: applies one diff at a musical boundary. 0 = ok, 1 = null pointer,
/// 2 = rejected (parse/validate/apply — see `kontinuum_last_error`), 3 = panic.
///
/// # Safety
/// `engine` must be null or a live engine pointer; `diff_json` null or a valid
/// C string.
#[no_mangle]
pub unsafe extern "C" fn kontinuum_engine_apply_diff(
    engine: *mut KontinuumEngine,
    diff_json: *const c_char,
    at_bar: u32,
) -> i32 {
    if engine.is_null() {
        return 1;
    }
    let result = guarded(|| {
        let Some(json) = (unsafe { opt_str(diff_json) }) else {
            return Err(EngineError::DiffParse("diff_json is null".into()));
        };
        unsafe { &mut *engine }.apply_diff_json(&json, at_bar)
    });
    match result {
        Some(Ok(_)) => 0,
        Some(Err(e)) => {
            set_last_error(e.to_string());
            2
        }
        None => {
            set_last_error("panic while applying diff".into());
            3
        }
    }
}

/// Control thread: replaces one track's instrument from plain instrument
/// JSON (issue #97). The bridge is catalog-agnostic: the caller resolves a
/// catalog entry to `{"kind": "kick", …}` or `{"kind": "custom", "patch":
/// …}` first. The change is validated against the whole session, becomes
/// audible on the track's next notes, and shows up in
/// `kontinuum_engine_export_session`. Like `kontinuum_engine_load_sample`,
/// this touches the graph — keep the transport stopped across the call.
/// Returns 0 = ok, 1 = null engine, 2 = unknown track (or null `track`),
/// 3 = invalid instrument (parse/validation/panic — see
/// `kontinuum_last_error`).
///
/// # Safety
/// `engine` must be null or a live engine pointer; `track` and
/// `instrument_json` null or valid C strings.
#[no_mangle]
pub unsafe extern "C" fn kontinuum_engine_set_track_instrument(
    engine: *mut KontinuumEngine,
    track: *const c_char,
    instrument_json: *const c_char,
) -> i32 {
    if engine.is_null() {
        return 1;
    }
    let result = guarded(|| {
        let Some(track_id) = (unsafe { opt_str(track) }) else {
            return Err(EngineError::UnknownTrack("<null track>".into()));
        };
        let Some(json) = (unsafe { opt_str(instrument_json) }) else {
            return Err(EngineError::InstrumentParse("instrument_json is null".into()));
        };
        unsafe { &mut *engine }.set_track_instrument(&track_id, &json)
    });
    match result {
        Some(Ok(())) => 0,
        Some(Err(e)) => {
            let code = if matches!(e, EngineError::UnknownTrack(_)) { 2 } else { 3 };
            set_last_error(e.to_string());
            code
        }
        None => {
            set_last_error("panic while setting the track instrument".into());
            3
        }
    }
}

/// Non-RT: keeps the block queue primed ahead of the playhead (looping the
/// session). Call from a UI timer while playing — 1 Hz is plenty. 0 = ok,
/// 1 = null engine, 2 = internal panic.
///
/// # Safety
/// `engine` must be null or a live engine pointer.
#[no_mangle]
pub unsafe extern "C" fn kontinuum_engine_pump(engine: *mut KontinuumEngine) -> i32 {
    match guarded(|| unsafe { engine.as_mut() }.map(KontinuumEngine::pump)) {
        Some(Some(())) => 0,
        Some(None) => 1,
        None => 2,
    }
}

/// Control thread: ground-truth UI snapshot (drains per-track onset counters).
/// 0 = ok, 1 = null pointer, 2 = panic.
///
/// # Safety
/// `engine` must be null or a live engine pointer; `out` must be null or a
/// valid `KontinuumUiSnapshotFFI` location.
#[no_mangle]
pub unsafe extern "C" fn kontinuum_engine_ui_snapshot(
    engine: *mut KontinuumEngine,
    out: *mut KontinuumUiSnapshotFFI,
) -> i32 {
    if engine.is_null() || out.is_null() {
        return 1;
    }
    let result = guarded(|| unsafe { &mut *engine }.ui_snapshot());
    match result {
        Some(snap) => {
            let mut tracks =
                [KontinuumTrackUiFFI { onsets: 0, velocity: 0.0, pitch: 0.0 }; MAX_TRACKS];
            for (i, t) in snap.tracks.iter().enumerate().take(MAX_TRACKS) {
                tracks[i] = KontinuumTrackUiFFI {
                    onsets: t.onsets,
                    velocity: t.velocity,
                    pitch: t.pitch,
                };
            }
            let mut current_masks = [0u32; MAX_TRACKS];
            for (i, m) in snap.current_masks.iter().enumerate().take(MAX_TRACKS) {
                current_masks[i] = *m;
            }
            unsafe {
                *out = KontinuumUiSnapshotFFI {
                    bar: snap.bar,
                    beat_phase: snap.beat_phase,
                    energy: snap.energy,
                    section_index: snap.section_index as u32,
                    bar_in_section: snap.bar_in_section,
                    section_bars: snap.section_bars,
                    playing: snap.playing,
                    tracks,
                    current_masks,
                };
            }
            0
        }
        None => 2,
    }
}

/// Finalized bar frame for the living UI's waveform/code stream. Per-track
/// arrays cover `MAX_TRACKS` slots (issue #89); `rms`/`peak`/`section_index`
/// carry the bar's measured loudness and compose position (issue #90).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct KontinuumBarFrameFFI {
    pub energy: f32,
    pub onsets: [u32; MAX_TRACKS],
    pub masks: [u32; MAX_TRACKS],
    pub last_velocity: [f32; MAX_TRACKS],
    /// Mixed-output RMS of this bar's audio, metered on the RT path (0..1).
    pub rms: f32,
    /// Mixed-output peak of this bar's audio (0..1).
    pub peak: f32,
    /// Section the bar belongs to; the UI ticks a boundary where it flips.
    pub section_index: u32,
}

/// Control thread: copy finalized bar history (oldest first). Returns count.
///
/// # Safety
/// `engine` null-or-live; `out` valid for `max` entries (or null with max 0).
#[no_mangle]
pub unsafe extern "C" fn kontinuum_engine_ui_history(
    engine: *mut KontinuumEngine,
    out: *mut KontinuumBarFrameFFI,
    max: u32,
) -> u32 {
    if engine.is_null() || out.is_null() || max == 0 {
        return 0;
    }
    let result = guarded(|| unsafe { &*engine }.ui_history_copy_owned(max as usize));
    match result {
        Some(frames) => {
            for (i, f) in frames.iter().enumerate() {
                let mut onsets = [0u32; MAX_TRACKS];
                let mut masks = [0u32; MAX_TRACKS];
                let mut last_velocity = [0.0f32; MAX_TRACKS];
                for (j, v) in f.onsets.iter().enumerate().take(MAX_TRACKS) {
                    onsets[j] = *v;
                }
                for (j, m) in f.masks.iter().enumerate().take(MAX_TRACKS) {
                    masks[j] = *m;
                }
                for (j, v) in f.last_velocity.iter().enumerate().take(MAX_TRACKS) {
                    last_velocity[j] = *v;
                }
                unsafe {
                    *out.add(i) = KontinuumBarFrameFFI {
                        energy: f.energy,
                        onsets,
                        masks,
                        last_velocity,
                        rms: f.rms,
                        peak: f.peak,
                        section_index: f.section_index,
                    };
                }
            }
            frames.len() as u32
        }
        None => 0,
    }
}

/// Copies `src` into a fixed NUL-terminated buffer, truncating to fit.
fn fill_c_string<const N: usize>(dst: &mut [c_char; N], src: &str) {
    let bytes = src.as_bytes();
    let n = bytes.len().min(N - 1);
    for (i, b) in bytes[..n].iter().enumerate() {
        dst[i] = *b as c_char;
    }
    dst[n] = 0;
}

/// One track descriptor for the loaded session (issue #89): the canonical
/// engine identity the UI derives its lanes from. `id`/`name` are
/// NUL-terminated UTF-8 in fixed buffers. Mirrors `KontinuumTrackDescriptor`
/// in `KontinuumBridge.h`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct KontinuumTrackDescriptorFFI {
    /// Session track index (order of `session.tracks`).
    pub index: u32,
    /// Voice kind: 0 kick, 1 bass, 2 perc, 3 pad, 4 fx.
    pub voice: u32,
    /// NUL-terminated engine track id (canonical vocabulary).
    pub id: [c_char; 32],
    /// NUL-terminated display name derived from the instrument kind.
    pub name: [c_char; 48],
}

const TRACK_ID_MAX: usize = 32;
const TRACK_NAME_MAX: usize = 48;

/// Control thread: descriptors for the loaded session's tracks, in session
/// track order. Writes at most `max` entries; returns how many were written
/// (0 on null/panic).
///
/// # Safety
/// `engine` null-or-live; `out` valid for `max` entries (or null with max 0).
#[no_mangle]
pub unsafe extern "C" fn kontinuum_engine_track_descriptors(
    engine: *mut KontinuumEngine,
    out: *mut KontinuumTrackDescriptorFFI,
    max: u32,
) -> u32 {
    if engine.is_null() || out.is_null() || max == 0 {
        return 0;
    }
    let result = guarded(|| unsafe { &*engine }.track_descriptors());
    match result {
        Some(descriptors) => {
            let count = descriptors.len().min(max as usize);
            for (i, d) in descriptors.iter().take(count).enumerate() {
                let mut id = [0 as c_char; TRACK_ID_MAX];
                let mut name = [0 as c_char; TRACK_NAME_MAX];
                fill_c_string(&mut id, &d.id);
                fill_c_string(&mut name, &d.name);
                // SAFETY: `out` is valid for `max` entries; i < count <= max.
                unsafe {
                    *out.add(i) = KontinuumTrackDescriptorFFI {
                        index: d.index as u32,
                        voice: d.voice as u32,
                        id,
                        name,
                    };
                }
            }
            count as u32
        }
        None => 0,
    }
}

/// Control thread: export the current session (saved composition) as pretty
/// JSON. Returns a malloc'd C string (free with `kontinuum_string_free`) or
/// null on panic.
///
/// # Safety
/// `engine` must be null or a live engine pointer.
#[no_mangle]
pub unsafe extern "C" fn kontinuum_engine_export_session(
    engine: *mut KontinuumEngine,
) -> *mut c_char {
    if engine.is_null() {
        return ptr::null_mut();
    }
    match guarded(|| unsafe { &*engine }.export_session_json()) {
        Some(json) => match CString::new(json) {
            Ok(c) => c.into_raw(),
            Err(_) => ptr::null_mut(),
        },
        None => ptr::null_mut(),
    }
}

/// Control thread: generate a session from a taste profile JSON. Returns a
/// malloc'd session JSON (valid, compilable) or null (see kontinuum_last_error).
///
/// # Safety
/// `profile_json` must be null or a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn kontinuum_generate_session_from_taste(
    profile_json: *const c_char,
    seed: u64,
) -> *mut c_char {
    if profile_json.is_null() {
        return ptr::null_mut();
    }
    let result = guarded(|| -> Result<String, String> {
        let json = unsafe { opt_str(profile_json) }.ok_or("profile_json is null".to_string())?;
        let profile: kontinuum_compose::taste::TasteProfile =
            serde_json::from_str(&json).map_err(|e| e.to_string())?;
        let session = kontinuum_compose::taste::session_from_taste(&profile, seed);
        kontinuum_ir::validate_session(&session)
            .map_err(|errs| errs.iter().map(|e| e.code.to_string()).collect::<Vec<_>>().join(","))?;
        serde_json::to_string_pretty(&session).map_err(|e| e.to_string())
    });
    match result {
        Some(Ok(json)) => match CString::new(json) {
            Ok(c) => c.into_raw(),
            Err(_) => ptr::null_mut(),
        },
        Some(Err(e)) => {
            set_last_error(e);
            ptr::null_mut()
        }
        None => {
            set_last_error("panic while generating session".into());
            ptr::null_mut()
        }
    }
}

/// Control thread: load a mono f32 PCM buffer as the sample for `track`'s
/// sampler pool. Transport must be stopped. 0 = ok, 1 = null, 2 = playing,
/// 3 = panic.
///
/// # Safety
/// `engine` null-or-live; `pcm` valid for `frames` f32s (or null with 0).
#[no_mangle]
pub unsafe extern "C" fn kontinuum_engine_load_sample(
    engine: *mut KontinuumEngine,
    track: u32,
    pcm: *const f32,
    frames: u32,
    sample_rate: u32,
) -> i32 {
    if engine.is_null() {
        return 1;
    }
    let result = guarded(|| {
        let slice = if pcm.is_null() || frames == 0 {
            &[][..]
        } else {
            unsafe { std::slice::from_raw_parts(pcm, frames as usize) }
        };
        unsafe { &mut *engine }.load_sample(track as u8, slice, sample_rate)
    });
    match result {
        Some(Ok(())) => 0,
        Some(Err(e)) => {
            set_last_error(e.to_string());
            2
        }
        None => 3,
    }
}

/// Control thread: analyze a WAV reference file and generate an adapted
/// session. Malloc'd session JSON or null (see kontinuum_last_error).
///
/// # Safety
/// `path` must be null or a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn kontinuum_generate_session_from_reference(
    path: *const c_char,
    seed: u64,
) -> *mut c_char {
    if path.is_null() {
        return ptr::null_mut();
    }
    let result = guarded(|| -> Result<String, String> {
        let p = unsafe { opt_str(path) }.ok_or("path is null".to_string())?;
        let session = kontinuum_compose::reference::session_from_reference_wav(&p, seed)?;
        kontinuum_ir::validate_session(&session).map_err(|errs| {
            errs.iter().map(|e| e.code.to_string()).collect::<Vec<_>>().join(",")
        })?;
        serde_json::to_string_pretty(&session).map_err(|e| e.to_string())
    });
    match result {
        Some(Ok(json)) => match CString::new(json) {
            Ok(c) => c.into_raw(),
            Err(_) => ptr::null_mut(),
        },
        Some(Err(e)) => {
            set_last_error(e);
            ptr::null_mut()
        }
        None => {
            set_last_error("panic during reference analysis".into());
            ptr::null_mut()
        }
    }
}

/// Control thread: analyze decoded mono PCM and generate an adapted session.
/// Malloc'd session JSON or null (see kontinuum_last_error).
///
/// # Safety
/// `pcm` must be null or valid for `frames` f32s; `sample_rate` > 0.
#[no_mangle]
pub unsafe extern "C" fn kontinuum_generate_session_from_reference_pcm(
    pcm: *const f32,
    frames: u32,
    sample_rate: u32,
    seed: u64,
) -> *mut c_char {
    if pcm.is_null() || frames == 0 || sample_rate == 0 {
        return ptr::null_mut();
    }
    let result = guarded(|| -> Result<String, String> {
        let slice = unsafe { std::slice::from_raw_parts(pcm, frames as usize) };
        let session =
            kontinuum_compose::reference::session_from_reference_samples(slice, sample_rate, seed)?;
        kontinuum_ir::validate_session(&session).map_err(|errs| {
            errs.iter().map(|e| e.code.to_string()).collect::<Vec<_>>().join(",")
        })?;
        serde_json::to_string_pretty(&session).map_err(|e| e.to_string())
    });
    match result {
        Some(Ok(json)) => match CString::new(json) {
            Ok(c) => c.into_raw(),
            Err(_) => ptr::null_mut(),
        },
        Some(Err(e)) => {
            set_last_error(e);
            ptr::null_mut()
        }
        None => {
            set_last_error("panic during reference analysis".into());
            ptr::null_mut()
        }
    }
}

/// RT render entry (audio thread): no locks, no allocation, no logging.
/// On internal panic the buffer is zero-filled so garbage never reaches the
/// speaker.
///
/// # Safety
/// `engine` must be null or a live engine pointer; `out_l`/`out_r` must be
/// null or each valid for `frames` writable `f32`s. In the Swift shell both
/// buffers have exactly `frames` frames (non-interleaved stereo).
#[no_mangle]
pub unsafe extern "C" fn kontinuum_engine_render(
    engine: *mut KontinuumEngine,
    out_l: *mut f32,
    out_r: *mut f32,
    frames: u32,
) {
    if engine.is_null() || out_l.is_null() || out_r.is_null() || frames == 0 {
        return;
    }
    let n = frames as usize;
    let result = guarded(|| {
        // SAFETY: caller guarantees `frames` writable frames per channel.
        let l = unsafe { std::slice::from_raw_parts_mut(out_l, n) };
        let r = unsafe { std::slice::from_raw_parts_mut(out_r, n) };
        unsafe { &mut *engine }.render(l, r);
    });
    if result.is_none() {
        // f32 bit pattern 0x00 is +0.0 — silence the damaged buffer.
        // SAFETY: same writability contract as above.
        unsafe {
            ptr::write_bytes(out_l, 0, n);
            ptr::write_bytes(out_r, 0, n);
        }
    }
}

/// Reads a telemetry snapshot. 0 = ok, 1 = null pointer, 2 = internal panic.
///
/// # Safety
/// `engine` must be null or a live engine pointer; `out` must be null or a
/// valid `TelemetryFFI` location.
#[no_mangle]
pub unsafe extern "C" fn kontinuum_engine_telemetry(
    engine: *mut KontinuumEngine,
    out: *mut TelemetryFFI,
) -> i32 {
    if engine.is_null() || out.is_null() {
        return 1;
    }
    let snapshot = guarded(|| TelemetryFFI::from(unsafe { &*engine }.telemetry()));
    match snapshot {
        Some(t) => {
            // SAFETY: `out` is non-null and valid for a TelemetryFFI write.
            unsafe { *out = t };
            0
        }
        None => 2,
    }
}

/// Reads the mastering working point (#82). 0 = ok, 1 = null pointer,
/// 2 = internal panic.
///
/// # Safety
/// `engine` must be null or a live engine pointer; `out` must be null or a
/// valid `MasteringTelemetryFFI` location.
#[no_mangle]
pub unsafe extern "C" fn kontinuum_mastering_telemetry(
    engine: *mut KontinuumEngine,
    out: *mut MasteringTelemetryFFI,
) -> i32 {
    if engine.is_null() || out.is_null() {
        return 1;
    }
    let snapshot =
        guarded(|| MasteringTelemetryFFI::from(unsafe { &*engine }.telemetry().mastering));
    match snapshot {
        Some(m) => {
            // SAFETY: `out` is non-null and valid for a
            // MasteringTelemetryFFI write.
            unsafe { *out = m };
            0
        }
        None => 2,
    }
}

/// Control thread: render a session document to deliverable master files
/// (#102) and return a JSON report (free with `kontinuum_string_free`), or
/// null with the reason in `kontinuum_last_error`.
///
/// `session_json` is a session document — the same shape
/// `kontinuum_engine_export_session` hands out, so a host exports what is
/// playing by saving the session first. Export is a pure function of that
/// document: it never touches the live engine, and because the engine is
/// deterministic on the session seed, the files are what the listener heard.
///
/// `spec_json` is a `kontinuum_export::ExportSpec`:
/// ```json
/// {"artist":"Kontinuum","title":"Night Shift","year":2026,"month":9,"day":2,
///  "outDir":"/…/Documents/Exports","presets":["archival","pressKitMp3"],
///  "sampleRate":48000,"mp3Kbps":320,"stems":false}
/// ```
/// Omitting `presets` asks for the default four-file set.
///
/// **This call blocks for as long as the render takes** — seconds to minutes,
/// several renders plus an MP3 encode. Call it from a background queue, never
/// from the audio thread and never from the main thread.
///
/// # Safety
/// `session_json` and `spec_json` must be null or valid NUL-terminated C
/// strings.
#[no_mangle]
pub unsafe extern "C" fn kontinuum_export_masters(
    session_json: *const c_char,
    spec_json: *const c_char,
) -> *mut c_char {
    let result = guarded(|| -> Result<String, String> {
        let session_src =
            unsafe { opt_str(session_json) }.ok_or("session_json is null".to_string())?;
        let spec_src = unsafe { opt_str(spec_json) }.ok_or("spec_json is null".to_string())?;
        let session: kontinuum_ir::Session =
            serde_json::from_str(&session_src).map_err(|e| format!("session_json: {e}"))?;
        let spec: kontinuum_export::ExportSpec =
            serde_json::from_str(&spec_src).map_err(|e| format!("spec_json: {e}"))?;
        let request = spec.into_request(session.tracks.len());
        let targets = kontinuum_mastering::MasteringTargets::hypothesis();
        let report = kontinuum_export::export_session(&session, &request, &targets)
            .map_err(|e| e.to_string())?;
        let json = kontinuum_export::ExportReportJson::from_report(&report, &session);
        serde_json::to_string(&json).map_err(|e| e.to_string())
    });
    match result {
        Some(Ok(json)) => match CString::new(json) {
            Ok(c) => c.into_raw(),
            Err(_) => {
                set_last_error("export report contained interior NUL".to_string());
                ptr::null_mut()
            }
        },
        Some(Err(e)) => {
            set_last_error(e);
            ptr::null_mut()
        }
        None => {
            set_last_error("export panicked".to_string());
            ptr::null_mut()
        }
    }
}

/// Returns the last error message on this thread (null if none). The buffer is
/// owned by the bridge and stays valid until the next bridge call on the same
/// thread.
#[no_mangle]
pub extern "C" fn kontinuum_last_error() -> *const c_char {
    last_error_ptr()
}

/// Frees a string previously handed out by this bridge (reserved for future
/// string-returning calls; `kontinuum_last_error` buffers are bridge-owned).
///
/// # Safety
/// `s` must be null or a pointer produced by this bridge's CString exports.
#[no_mangle]
pub unsafe extern "C" fn kontinuum_string_free(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    // SAFETY: per contract, the pointer came from CString::into_raw here.
    let _ = guarded(|| unsafe { drop(CString::from_raw(s)) });
}

// -- test hooks --------------------------------------------------------------

#[cfg(test)]
pub fn test_panic_path() {
    let result: Option<Result<(), EngineError>> =
        guarded(|| -> Result<(), EngineError> { panic!("simulated engine panic") });
    assert!(result.is_none(), "panic must be contained to None");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_containment_maps_panic_to_none() {
        test_panic_path();
    }

    #[test]
    fn composer_test_paths_produce_valid_reports() {
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/loop-4track.ir.json"
        );
        let session = std::fs::read_to_string(fixture).expect("fixture readable");
        let session_c = CString::new(session.clone()).unwrap();
        let prompt_c = CString::new("darker and sparser").unwrap();
        let model_c = CString::new("qwen3.5-2b").unwrap();

        unsafe {
            let envelope = kontinuum_composer_envelope(session_c.as_ptr(), prompt_c.as_ptr(), model_c.as_ptr());
            assert!(!envelope.is_null(), "envelope builds");
            let body = CStr::from_ptr(envelope).to_string_lossy().into_owned();
            kontinuum_string_free(envelope);
            assert!(body.contains("\"messages\""), "envelope is a chat body");
            assert!(body.contains("\"temperature\":0.0"), "deterministic decoding is policy");

            let report = kontinuum_composer_test_ondevice(session_c.as_ptr(), prompt_c.as_ptr());
            assert!(!report.is_null(), "on-device test succeeds");
            let text = CStr::from_ptr(report).to_string_lossy().into_owned();
            kontinuum_string_free(report);
            let v: serde_json::Value = serde_json::from_str(&text).expect("report is JSON");
            assert_eq!(v["backend"], "heuristic");
            assert_eq!(v["valid"], true, "heuristic plan applies cleanly: {text}");

            // A report body is NOT a plan response: apply must reject it with a
            // clear error, never silently treat it as diffs.
            let not_a_plan = CString::new(text.clone()).unwrap();
            let rejected = kontinuum_composer_apply_response(
                session_c.as_ptr(),
                not_a_plan.as_ptr(),
                prompt_c.as_ptr(),
            );
            assert!(rejected.is_null(), "non-plan bodies are rejected");
            let err = CStr::from_ptr(kontinuum_last_error()).to_string_lossy().into_owned();
            assert!(err.contains("not a plan"), "rejection explains itself: {err}");

            // And a genuine PlanResponse body applies through the cloud-first
            // ladder, proving the Swift transport round-trip end to end.
            let plan = serde_json::json!({
                "diffs": ["{\"op\":\"set_section_energy\",\"id\":\"b_groove\",\"energy\":[0.5,0.6]}"],
                "notes": "cloud test", "backend_id": "", "latency_hint_ms": 0
            });
            let plan_c = CString::new(plan.to_string()).unwrap();
            let applied = kontinuum_composer_apply_response(
                session_c.as_ptr(),
                plan_c.as_ptr(),
                prompt_c.as_ptr(),
            );
            assert!(!applied.is_null(), "a valid plan response applies");
            let applied_text = CStr::from_ptr(applied).to_string_lossy().into_owned();
            kontinuum_string_free(applied);
            let v: serde_json::Value = serde_json::from_str(&applied_text).expect("report JSON");
            assert_eq!(v["backend"], "cloud-http", "the cloud rung served the plan");
        }
    }

    #[test]
    fn last_error_roundtrip_and_clear() {
        set_last_error("boom 1".into());
        let p = last_error_ptr();
        assert!(!p.is_null());
        let msg = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
        assert_eq!(msg, "boom 1");
        // Interior NUL is sanitized, not a panic.
        set_last_error("bad\0nul".into());
        assert!(!last_error_ptr().is_null());
    }

    #[test]
    fn descriptors_and_snapshot_cover_all_twelve_tracks() {
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/loop-4track.ir.json"
        );
        let session = std::fs::read_to_string(fixture).expect("fixture readable");
        let session_c = CString::new(session).unwrap();
        let engine = unsafe { kontinuum_engine_new(48_000, session_c.as_ptr()) };
        assert!(!engine.is_null(), "fixture engine constructs");

        // Descriptors follow session track order; ids/voices come from the
        // session, not a UI-side table.
        let mut out = [KontinuumTrackDescriptorFFI {
            index: 0,
            voice: 0,
            id: [0 as c_char; TRACK_ID_MAX],
            name: [0 as c_char; TRACK_NAME_MAX],
        }; MAX_TRACKS];
        let count = unsafe { kontinuum_engine_track_descriptors(engine, out.as_mut_ptr(), MAX_TRACKS as u32) };
        assert_eq!(count as usize, 8, "every fixture track gets a descriptor");
        let id_of = |i: usize| {
            let raw = out[i].id;
            let len = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
            String::from_utf8(raw[..len].iter().map(|&c| c as u8).collect()).unwrap()
        };
        let voice_of = |i: usize| out[i].voice;
        assert_eq!(id_of(0), "kick");
        assert_eq!(id_of(1), "hat");
        assert_eq!(id_of(2), "bass");
        assert_eq!(id_of(3), "pad");
        assert_eq!(voice_of(0), 0, "kick voice");
        assert_eq!(voice_of(1), 2, "perc voice");
        assert_eq!(voice_of(2), 1, "bass voice");
        assert_eq!(voice_of(3), 3, "pad voice");
        assert_eq!(out[7].index, 7);

        // The snapshot carries all MAX_TRACKS slots: the kick (index 0) and
        // the hat (index 1) land at their session indices, not truncated
        // into an 8-slot mirror of a different index space.
        unsafe { kontinuum_engine_play(engine) };
        let mut l = [0.0f32; 512];
        let mut r = [0.0f32; 512];
        for _ in 0..180 {
            unsafe { kontinuum_engine_render(engine, l.as_mut_ptr(), r.as_mut_ptr(), 512) };
        }
        let mut snap = KontinuumUiSnapshotFFI {
            bar: 0.0,
            beat_phase: 0.0,
            energy: 0.0,
            section_index: 0,
            bar_in_section: 0,
            section_bars: 0,
            playing: false,
            tracks: [KontinuumTrackUiFFI { onsets: 0, velocity: 0.0, pitch: 0.0 }; MAX_TRACKS],
            current_masks: [0; MAX_TRACKS],
        };
        let code = unsafe { kontinuum_engine_ui_snapshot(engine, &mut snap) };
        assert_eq!(code, 0);
        let total_onsets: u32 = snap.tracks.iter().map(|t| t.onsets).sum();
        assert!(total_onsets > 0, "render produced onsets: {total_onsets}");
        assert!(
            snap.current_masks.iter().any(|m| *m != 0),
            "hit masks arrive through the widened 12-slot array"
        );
        unsafe {
            kontinuum_engine_stop(engine);
            kontinuum_engine_free(engine);
        }
    }

    #[test]
    fn telemetry_ffi_layout_matches_serde_snapshot() {
        let t = Telemetry {
            playhead_bar: 7.5,
            playing: true,
            queue_len: 3,
            active_block_bar: Some(4),
            render_gaps: 1,
            invalid_diffs: 2,
            mastering: MasteringTelemetryLite::default(),
        };
        let f = TelemetryFFI::from(t);
        assert_eq!((f.playhead_bar, f.playing, f.queue_len), (7.5, true, 3));
        assert_eq!((f.active_bar, f.has_active), (4, true));
        assert_eq!((f.render_gaps, f.invalid_diffs), (1, 2));
        let none = TelemetryFFI::from(Telemetry {
            active_block_bar: None,
            playhead_bar: 0.0,
            playing: false,
            queue_len: 0,
            render_gaps: 0,
            invalid_diffs: 0,
            mastering: MasteringTelemetryLite::default(),
        });
        assert!(!none.has_active && none.active_bar == 0);
    }
}

// -- Composer settings + test (issues #36/#22/#3) ----------------------------
//
// The Settings surface drives these from Swift: build the provider envelope
// here (all planning logic stays in Rust), let Swift's URLSession do the
// byte transport (the #36 "URLSession at the FFI boundary" seam), then apply
// the provider's response through the same validated-diff gate every plan
// takes. Nothing here touches the audio path.

/// One composer test result, serialized as JSON for the Settings surface.
#[derive(serde::Serialize)]
struct ComposerTestReport {
    backend: String,
    applied: usize,
    rejected: usize,
    repairs: u32,
    latency_ms: u128,
    valid: bool,
    notes: String,
}

/// A backend that replays one precomputed response, then reports exhaustion.
/// Lets the wake ladder treat a Settings test exactly like a live cloud call.
struct RawResponseBackend {
    response: Option<kontinuum_composer::PlanResponse>,
}

impl kontinuum_composer::ComposerBackend for RawResponseBackend {
    fn name(&self) -> &str {
        "cloud-http"
    }

    fn set_timeout_ms(&mut self, _timeout_ms: u64) {}

    fn plan(
        &mut self,
        _request: &kontinuum_composer::PlanRequest,
    ) -> Result<kontinuum_composer::PlanResponse, kontinuum_composer::BackendError> {
        self.response
            .take()
            .ok_or_else(|| kontinuum_composer::BackendError::Transport("response already consumed".into()))
            .map(|mut r| {
                if r.backend_id.is_empty() {
                    r.backend_id = "cloud-http".into();
                }
                r
            })
    }
}

fn parse_session_json(json: &str) -> Result<kontinuum_ir::schema::Session, String> {
    serde_json::from_str(json).map_err(|e| format!("session JSON: {e}"))
}

fn composer_report_json(
    report: kontinuum_composer::ComposerReport,
    latency_ms: u128,
) -> Result<String, String> {
    let out = ComposerTestReport {
        backend: report.backend,
        applied: report.applied.len(),
        rejected: report.rejected,
        repairs: report.repairs,
        latency_ms,
        valid: !report.applied.is_empty(),
        notes: report.notes,
    };
    serde_json::to_string(&out).map_err(|e| format!("report serialization: {e}"))
}

/// Builds the OpenAI-compatible chat envelope for a composer cloud test.
/// Swift POSTs the returned body with URLSession and feeds the raw HTTP
/// response to `kontinuum_composer_apply_response`. Returns a malloc'd C
/// string (free with `kontinuum_string_free`); NULL on error (see
/// `kontinuum_last_error`).
///
/// # Safety
/// All three arguments must be null or valid NUL-terminated C strings.
#[no_mangle]
pub unsafe extern "C" fn kontinuum_composer_envelope(
    session_json: *const c_char,
    prompt: *const c_char,
    model: *const c_char,
) -> *mut c_char {
    let result = guarded(|| {
        let (Some(json), Some(prompt), Some(model)) = (
            unsafe { opt_str(session_json) },
            unsafe { opt_str(prompt) },
            unsafe { opt_str(model) },
        ) else {
            return Err("null argument".to_string());
        };
        let session = parse_session_json(&json)?;
        let request = kontinuum_composer::wake::build_plan_request(
            &session,
            0,
            kontinuum_composer::wake::Steering { style: "techno", prompt: &prompt, taste_json: "{}", style_card: "" },
        );
        kontinuum_composer::openai::build_request_body(&model, &request)
    });
    match result {
        Some(Ok(body)) => match CString::new(body) {
            Ok(c) => c.into_raw(),
            Err(_) => ptr::null_mut(),
        },
        Some(Err(e)) => {
            set_last_error(e);
            ptr::null_mut()
        }
        None => {
            set_last_error("panic while building the composer envelope".into());
            ptr::null_mut()
        }
    }
}

/// Runs the on-device composer test: one validated-diff wake on a scratch
/// engine through the heuristic backend. Returns a ComposerTestReport JSON
/// (malloc'd, free with `kontinuum_string_free`); NULL on error.
///
/// # Safety
/// Both arguments must be null or valid NUL-terminated C strings.
#[no_mangle]
pub unsafe extern "C" fn kontinuum_composer_test_ondevice(
    session_json: *const c_char,
    prompt: *const c_char,
) -> *mut c_char {
    let result = guarded(|| {
        let (Some(json), Some(prompt)) = (unsafe { opt_str(session_json) }, unsafe { opt_str(prompt) })
        else {
            return Err("null argument".to_string());
        };
        let session = parse_session_json(&json)?;
        let mut engine = kontinuum_compose::engine::ArrangementEngine::new(session, 48_000);
        let request = kontinuum_composer::wake::build_plan_request(
            engine.current_session(),
            0,
            kontinuum_composer::wake::Steering { style: "techno", prompt: &prompt, taste_json: "{}", style_card: "" },
        );
        let mut heuristic = kontinuum_composer::OnDeviceHeuristicBackend;
        let selector = kontinuum_composer::BackendSelector::default();
        let mut ladder = selector.ladder(&mut heuristic, None);
        let started = std::time::Instant::now();
        let report = kontinuum_composer::wake::run_wake(&mut engine, &mut ladder, &request);
        composer_report_json(report, started.elapsed().as_millis())
    });
    match result {
        Some(Ok(json)) => match CString::new(json) {
            Ok(c) => c.into_raw(),
            Err(_) => ptr::null_mut(),
        },
        Some(Err(e)) => {
            set_last_error(e);
            ptr::null_mut()
        }
        None => {
            set_last_error("panic during the on-device composer test".into());
            ptr::null_mut()
        }
    }
}

/// Applies a provider's raw HTTP response body through the validated-diff
/// gate on a scratch engine — cloud-first ladder, heuristic floor. Returns a
/// ComposerTestReport JSON; NULL on error.
///
/// # Safety
/// All three arguments must be null or valid NUL-terminated C strings.
#[no_mangle]
pub unsafe extern "C" fn kontinuum_composer_apply_response(
    session_json: *const c_char,
    response_body: *const c_char,
    prompt: *const c_char,
) -> *mut c_char {
    let result = guarded(|| {
        let (Some(json), Some(body), Some(prompt)) = (
            unsafe { opt_str(session_json) },
            unsafe { opt_str(response_body) },
            unsafe { opt_str(prompt) },
        ) else {
            return Err("null argument".to_string());
        };
        let payload = kontinuum_composer::openai::extract_response(&body);
        let response: kontinuum_composer::PlanResponse = serde_json::from_str(&payload)
            .map_err(|e| format!("provider response is not a plan: {e}"))?;
        let session = parse_session_json(&json)?;
        let mut engine = kontinuum_compose::engine::ArrangementEngine::new(session, 48_000);
        let request = kontinuum_composer::wake::build_plan_request(
            engine.current_session(),
            0,
            kontinuum_composer::wake::Steering { style: "techno", prompt: &prompt, taste_json: "{}", style_card: "" },
        );
        let mut heuristic = kontinuum_composer::OnDeviceHeuristicBackend;
        let mut cloud = RawResponseBackend { response: Some(response) };
        let selector = kontinuum_composer::BackendSelector { prefer_on_device: false, ..Default::default() };
        let mut ladder =
            selector.ladder(&mut heuristic, Some(&mut cloud as &mut dyn kontinuum_composer::ComposerBackend));
        let started = std::time::Instant::now();
        let report = kontinuum_composer::wake::run_wake(&mut engine, &mut ladder, &request);
        composer_report_json(report, started.elapsed().as_millis())
    });
    match result {
        Some(Ok(json)) => match CString::new(json) {
            Ok(c) => c.into_raw(),
            Err(_) => ptr::null_mut(),
        },
        Some(Err(e)) => {
            set_last_error(e);
            ptr::null_mut()
        }
        None => {
            set_last_error("panic while applying the composer response".into());
            ptr::null_mut()
        }
    }
}
