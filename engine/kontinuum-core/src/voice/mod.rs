//! Harness-side voice infrastructure: the `Voice` trait lives in the crate
//! root; this module holds the built-in voices (#51: patch interpreter and
//! sampler are harness built-ins) plus the shared deterministic DSP utils
//! the instrument plugins (kontinuum-instruments-core) import.
//!
//! All voices are deterministic: `note_on` resets to fixed state, output
//! hard-mutes below [`crate::SILENCE_ABS`] so renders are bit-stable, and
//! tails flush denormals.

pub mod choke;
pub mod granular;
pub mod patch;
pub mod sampler;

pub use choke::{CHOKE_GROUP_HATS, ChokeState};
pub use granular::{GrainConfig, GranularVoice, MAX_GRAINS};
pub use patch::{PatchVoice, SampleBank};
pub use sampler::Sampler;

use crate::DENORMAL_FLOOR;

/// Exponential coefficient for a one-pole reaching -60 dB after `ms`.
pub fn decay_coeff(sample_rate: f32, ms: f32) -> f32 {
    (-1000.0 / (ms.max(0.05) * sample_rate.max(1.0))).exp()
}

pub fn midi_to_hz(pitch: f32) -> f32 {
    440.0 * ((pitch - 69.0) / 12.0).exp2()
}

/// Anti-aliased falling saw generator with PolyBLEP correction. Advances
/// `phase` by `dt` and returns the sample in -1..1. Call per sample.
pub fn poly_blep_saw(phase: &mut f32, dt: f32) -> f32 {
    *phase += dt;
    if *phase >= 1.0 {
        *phase -= 1.0;
    }
    let mut value = 1.0 - 2.0 * *phase;
    value += poly_blep(*phase, dt);
    value
}

pub fn poly_blep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let x = t / dt - 1.0;
        -(x * x) - 2.0 * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt + 1.0;
        x * x + 2.0 * x + 1.0
    } else {
        0.0
    }
}

pub fn flush_denormal(x: f32) -> f32 {
    if x.abs() < DENORMAL_FLOOR {
        0.0
    } else {
        x
    }
}

/// Fixed-seed xorshift32 noise. Reset per note: identical notes are identical.
pub struct NoiseGen {
    state: u32,
}

impl NoiseGen {
    pub const fn seeded() -> Self {
        NoiseGen { state: 0x9E37_79B9 }
    }

    /// Explicit state — the round-robin hit variants must not share a state.
    pub const fn seeded_at(state: u32) -> Self {
        NoiseGen { state: if state == 0 { 1 } else { state } }
    }

    pub fn range_f32(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (self.next_f32() * 0.5 + 0.5) * (hi - lo)
    }

    pub fn next_f32(&mut self) -> f32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        (x >> 8) as f32 * (2.0 / 16_777_216.0) - 1.0
    }
}

/// Round-robin noise seeds: consecutive hits draw different fixed noise
/// states, the per-hit "variants" of issue #52 workstream 3.
pub const HIT_VARIANTS: [u32; 6] = [
    0x1F2E_3D4C, 0x9A8B_7C6D, 0x0F1E_2D3C, 0x7465_8798, 0x2B3C_4D5E, 0xE5D4_C3B2,
];

/// Per-hit modulation drawn from a voice's [`HitJitter`] stream.
pub struct HitMod {
    pub amp: f32,
    pub pitch: f32,
    pub decay: f32,
    pub tone: f32,
    pub variant: usize,
}

/// Deterministic per-voice jitter stream (issue #52 WS3): no two hits
/// identical. The stream lives in the voice instance, advances per trigger,
/// and stays deterministic because pool slot assignment is deterministic.
pub struct HitJitter {
    rng: NoiseGen,
    variant: usize,
}

impl HitJitter {
    pub const fn new() -> Self {
        HitJitter { rng: NoiseGen::seeded_at(0x6A09_E667), variant: 0 }
    }

    /// Modulation for the next hit: amplitude in `amp`, pitch spread ±`cents`,
    /// decay ±`decay` around 1.0, tone in `tone`.
    pub fn next_hit(
        &mut self,
        amp: (f32, f32),
        cents: f32,
        decay: f32,
        tone: (f32, f32),
    ) -> HitMod {
        self.variant = (self.variant + 1) % HIT_VARIANTS.len();
        HitMod {
            amp: self.rng.range_f32(amp.0, amp.1),
            pitch: (self.rng.range_f32(-cents, cents) / 1200.0).exp2(),
            decay: self.rng.range_f32(1.0 - decay, 1.0 + decay),
            tone: self.rng.range_f32(tone.0, tone.1),
            variant: self.variant,
        }
    }
}
