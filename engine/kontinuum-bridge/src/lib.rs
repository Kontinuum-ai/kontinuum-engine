//! `kontinuum-bridge` — the live engine facade and its C FFI surface
//! (issues #10/#12).
//!
//! Layout:
//! - [`engine`] — [`engine::KontinuumEngine`], the whole live engine in one
//!   `Send` object: session, tempo lane, [`AudioGraph`](kontinuum_core::AudioGraph)
//!   and the RT block queue; `render` is the audio-thread entry, the rest is
//!   non-RT control.
//! - [`queue`] — [`queue::prepared_queue`]: split SPSC rings carrying
//!   compiled blocks *and* their pre-merged event lists, so block activation
//!   on the RT thread never allocates.
//! - [`ffi`] — the C ABI consumed by the iOS shell (`ios/Kontinuum`): every
//!   call is null-checked and wrapped in `catch_unwind`; panics never cross
//!   the boundary.
//!
//! Thread contract (honest scope, issue #12): iOS calls `kontinuum_engine_render`
//! from the AVAudioSourceNode render thread and the control functions from the
//! main thread. The engine serializes them at the FFI layer of the app (one
//! `KontinuumEngineHandle` owned by the controller; `render` only touches the
//! RT path). Cross-thread `&mut` aliasing is the Swift side's contract to
//! uphold — see `ios/Kontinuum/AudioEngineController.swift`.

pub mod engine;
pub mod ffi;
pub mod queue;
pub mod session_setup;

pub use engine::{ApplyOutcome, EngineError, KontinuumEngine, SafetyCounters, Telemetry};
pub use queue::{PreparedBlock, PreparedConsumer, PreparedProducer, prepared_queue};
pub use session_setup::apply_session_to_graph;

/// Compile-time `Send` proof for the engine (it crosses to the render thread).
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<KontinuumEngine>();
};
