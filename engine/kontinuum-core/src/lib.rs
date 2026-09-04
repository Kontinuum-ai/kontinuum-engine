//! `kontinuum-core` — fixed-topology DSP graph, voice pools, instruments, FX,
//! mixer (issues #10/#14). This module tree is the contract; implementations
//! live in the submodules.
//!
//! Hard rules: f32 samples, 64-frame internal blocks, every audible parameter
//! behind a one-pole smoother, no allocation on the render path, denormals
//! flushed to zero on aarch64.

pub const BLOCK_FRAMES: usize = 64;
pub const MAX_TRACKS: usize = 12;
pub const INSERT_SLOTS_PER_TRACK: usize = 2;

pub type ParamId = u16;

/// Mono synth voice. One note at a time; polyphony comes from the voice pool.
pub trait Voice: Send {
    fn note_on(&mut self, pitch: f32, velocity: f32);
    /// One-shot playback of one slice of the voice's sample table: starts at
    /// the slice's frame offset, stops at the next boundary, never loops;
    /// `rate_mul` multiplies the pitch-derived rate. Out-of-range slice
    /// indices clamp to the last slice. Default no-op for voices without a
    /// sample table.
    fn note_on_slice(&mut self, _pitch: f32, _velocity: f32, _slice: u16, _rate_mul: f32) {}
    fn note_off(&mut self);
    fn is_active(&self) -> bool;
    /// Render `out.len()` mono frames; the voice must be silent-after-release.
    fn render(&mut self, out: &mut [f32]);
    /// RT-safe parameter set (already-smoothed at the mixer level too).
    fn set_param(&mut self, param: ParamId, value: f32);
    fn reset(&mut self);
}

/// Per-track insert effect (2 slots per track in the fixed topology).
pub trait InsertFx: Send {
    /// In-place mono processing of the track signal pre-send.
    fn render(&mut self, io: &mut [f32]);
    fn set_param(&mut self, param: ParamId, value: f32);
    fn reset(&mut self);
}

/// Send-bus effects (delay + reverb) processing stereo mixes of summed sends.
pub trait BusFx: Send {
    fn render(&mut self, left: &mut [f32], right: &mut [f32]);
    fn set_param(&mut self, param: ParamId, value: f32);
    fn reset(&mut self);
}

/// One-pole parameter smoother. Every audible parameter must pass through one
/// (10–50 ms per param class) — zipper noise is CI-tested.
#[derive(Clone, Debug)]
pub struct Smoother {
    current: f32,
    target: f32,
    coeff: f32,
}

impl Smoother {
    /// `ms` smoothing time at `sample_rate`; the one-pole coefficient is derived
    /// from the -60 dB settling approximation `coeff = exp(-1 / (ms·sr/1000))`.
    pub fn new(sample_rate: f32, ms: f32) -> Self {
        let coeff = (-1000.0 / (ms.max(0.01) * sample_rate)).exp();
        Smoother { current: 0.0, target: 0.0, coeff }
    }

    pub fn set_target(&mut self, value: f32) {
        self.target = value;
    }

    pub fn target(&self) -> f32 {
        self.target
    }

    pub fn value(&self) -> f32 {
        self.current
    }

    /// Advance one frame; returns the smoothed value. `coeff` is the
    /// per-frame retention factor (exp(-1000/(ms·sr))), so the step is its
    /// complement — ~-60 dB of glide energy per `ms` of smoothing time.
    pub fn tick(&mut self) -> f32 {
        self.current += (1.0 - self.coeff) * (self.target - self.current);
        self.current
    }

    pub fn snap(&mut self, value: f32) {
        self.current = value;
        self.target = value;
    }

    pub fn settled(&self) -> bool {
        (self.target - self.current).abs() < 1e-4
    }
}

/// Flush denormals to zero by setting FPCR.FZ on aarch64 (issue #10, per #2).
/// Call once on the audio thread before the render loop; no-op elsewhere.
pub fn enable_denormal_protection() {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        let mut fpcr: u64;
        core::arch::asm!(
            "mrs {0}, fpcr",
            out(reg) fpcr,
            options(nomem, nostack)
        );
        fpcr |= 1 << 24; // FZ bit
        core::arch::asm!(
            "msr fpcr, {0}",
            in(reg) fpcr,
            options(nomem, nostack)
        );
    }
}

/// Deterministic FNV-1a 64-bit — sample hashing for golden renders without a
/// crypto dependency.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}

/// Sub-silence threshold (-90 dBFS): voices hard-mute below this so tails are
/// exactly zero and renders stay bit-stable.
pub const SILENCE_ABS: f32 = 3.16e-5;

/// Values under this magnitude flush to zero (denormal protection).
pub const DENORMAL_FLOOR: f32 = 1e-20;

pub mod fx;
pub mod graph;
pub mod master;
pub mod mix;
pub mod params;
pub mod slice;
pub mod pool;
pub mod voice;

pub use graph::{AudioGraph, VoiceFactory};
pub use master::MasterChain;
pub use mix::{
    crossfade_frames, equal_power_gains, AutoMixer, Crossfade, Deck, DeckMixer, KillTelemetry,
    MixRole, MixTelemetry,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoother_settles_and_never_overshoots() {
        let mut s = Smoother::new(48_000.0, 20.0);
        s.set_target(1.0);
        let mut prev = 0.0f32;
        for i in 0..100_000 {
            let v = s.tick();
            assert!(v >= prev - 1e-6, "smoother undershot at {i}");
            assert!((0.0..=1.0).contains(&v));
            prev = v;
        }
        assert!(s.settled());
    }

    #[test]
    fn fnv_reference_vectors() {
        assert_eq!(fnv1a64(b""), 0xcbf29ce484222325);
        assert_eq!(fnv1a64(b"a"), 0xaf63dc4c8601ec8c);
    }
}
