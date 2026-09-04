//! Real-time granular texture voice (issue #19, granular v0 — RT half).
//! A single-source grain cloud: grains are scheduled on a fixed hop, read
//! positions sweep the source front-to-back with seeded spray/pitch jitter,
//! and grains overlap-add through a Hann window. This is the playback twin
//! of `kontinuum_samples::granular::render_cloud` (the offline bed), sharing
//! its parameter vocabulary and sweep/jitter semantics.
//!
//! RT contract (crate rules): the grain table is a fixed-size array
//! allocated at construction, `render` allocates nothing and takes no locks,
//! and all configuration (`set_source`, `set_config`) is control-thread only.

use std::sync::Arc;

use super::{flush_denormal, NoiseGen};
use crate::{Voice, SILENCE_ABS};

/// Maximum simultaneous grains. At 200 ms grains and 200 grains/s the
/// overlap is ~40 — above the cap new grains steal the oldest slot instead
/// of allocating, which reads as a subtle dip, never a failure.
pub const MAX_GRAINS: usize = 32;

/// Fixed xorshift state for the per-voice spray/pitch jitter stream. A
/// dedicated constant (not the shared `NoiseGen::seeded()`) keeps granular
/// noise decorrelated from the hit-jitter voices.
const GRAIN_RNG_SEED: u32 = 0x4A12_9C3B;

/// Control-thread grain-cloud configuration. Defaults mirror the offline
/// `GrainSpec` defaults where those exist; the IR's granular slot params
/// land on these same ranges.
#[derive(Clone, Debug, PartialEq)]
pub struct GrainConfig {
    /// Grain size in ms (20..=200).
    pub grain_ms: f32,
    /// Grains per second (1..=200). Output level scales with overlap
    /// (grain_ms/1000 x density); balance with `level`.
    pub density: f32,
    /// Random read-position jitter per grain in +/- ms (0..=1000).
    pub spray_ms: f32,
    /// Random per-grain tuning jitter in +/- cents (0..=1200).
    pub pitch_jitter_cents: f32,
    /// Bed mix level 0..=1.
    pub level: f32,
}

impl Default for GrainConfig {
    fn default() -> Self {
        GrainConfig {
            grain_ms: 80.0,
            density: 25.0,
            spray_ms: 0.0,
            pitch_jitter_cents: 0.0,
            level: 0.8,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Grain {
    /// Read position in the source buffer (frames).
    read: f32,
    /// Resample rate for this grain (source frames per output frame).
    speed: f32,
    /// Frames played since spawn.
    age: usize,
    /// Total grain length in frames (at the voice's rate).
    len: usize,
    alive: bool,
}

pub struct GranularVoice {
    sr: f32,
    sample: Option<Arc<[f32]>>,
    sample_sr: f32,
    cfg: GrainConfig,
    /// Pitch-derived playback rate for the current note.
    rate: f32,
    amp: f32,
    gate: bool,
    active: bool,
    grains: [Grain; MAX_GRAINS],
    /// Round-robin spawn slot: a full table steals in fixed order, which
    /// keeps the cloud deterministic.
    next_slot: usize,
    /// Frames until the next grain spawns.
    hop_counter: usize,
    hop_frames: usize,
    grain_len_frames: usize,
    /// Read-position sweep cursor (source frames); advances one hop per
    /// grain so the cloud traverses the source once per output time.
    sweep_pos: f32,
    /// Highest spawnable read position (one grain length from the end).
    sweep_max: f32,
    spray_frames: f32,
    rng: NoiseGen,
}

impl GranularVoice {
    pub fn new(sample_rate: u32) -> Self {
        let mut v = GranularVoice {
            sr: sample_rate as f32,
            sample: None,
            sample_sr: sample_rate as f32,
            cfg: GrainConfig::default(),
            rate: 1.0,
            amp: 0.0,
            gate: false,
            active: false,
            grains: [Grain::default(); MAX_GRAINS],
            next_slot: 0,
            hop_counter: 0,
            hop_frames: 1,
            grain_len_frames: 1,
            sweep_pos: 0.0,
            sweep_max: 0.0,
            spray_frames: 0.0,
            rng: NoiseGen::seeded_at(GRAIN_RNG_SEED),
        };
        v.retime();
        v
    }

    /// Swap the grain source (control thread). Position state resets on the
    /// next `note_on`.
    pub fn set_source(&mut self, data: Arc<[f32]>, sample_rate: u32) {
        self.sample = Some(data);
        self.sample_sr = sample_rate as f32;
    }

    /// Apply a new cloud configuration (control thread); re-derives the
    /// hop/grain lengths. Takes effect on the next `note_on` (and the hop
    /// immediately — both are plain field writes).
    pub fn set_config(&mut self, cfg: GrainConfig) {
        self.cfg = cfg;
        self.retime();
    }

    /// Derive frame-domain timing from `cfg`. Shared by `new`/`set_config`
    /// so the render path only reads precomputed integers.
    fn retime(&mut self) {
        self.grain_len_frames =
            ((self.cfg.grain_ms / 1000.0) * self.sr).round().clamp(16.0, 1.0e6) as usize;
        self.hop_frames = (self.sr / self.cfg.density.max(1.0)).round().max(1.0) as usize;
        self.spray_frames = (self.cfg.spray_ms / 1000.0) * self.sr;
    }

    fn spawn_grain(&mut self) {
        let Some(source_len) = self.sample.as_ref().map(|s| s.len()) else {
            return;
        };
        if source_len < 16 {
            return;
        }
        // Sweep advances one hop of source per grain: the cloud reads the
        // source at output speed, front to back, matching the offline bed.
        self.sweep_pos = (self.sweep_pos + self.hop_frames as f32 * self.rate).min(self.sweep_max);
        let read = (self.sweep_pos
            + self.rng.range_f32(-self.spray_frames, self.spray_frames))
        .clamp(0.0, self.sweep_max);
        let jitter_mul =
            (self.rng.range_f32(-self.cfg.pitch_jitter_cents, self.cfg.pitch_jitter_cents)
                / 1200.0)
                .exp2();
        let slot = self.next_slot;
        self.next_slot = (self.next_slot + 1) % MAX_GRAINS;
        self.grains[slot] = Grain {
            read,
            speed: self.rate * jitter_mul * self.sample_sr / self.sr,
            age: 0,
            len: self.grain_len_frames,
            alive: true,
        };
    }
}

impl Voice for GranularVoice {
    fn note_on(&mut self, pitch: f32, velocity: f32) {
        // Fixed reset: identical notes are identical. The jitter stream is
        // re-seeded per note like HitJitter's per-hit determinism.
        self.rng = NoiseGen::seeded_at(GRAIN_RNG_SEED);
        self.rate = ((pitch.clamp(12.0, 108.0) - 60.0) / 12.0).exp2();
        self.amp = velocity.clamp(0.0, 1.0) * self.cfg.level;
        self.grains = [Grain::default(); MAX_GRAINS];
        self.next_slot = 0;
        self.hop_counter = 0;
        self.sweep_pos = 0.0;
        self.sweep_max = self
            .sample
            .as_ref()
            .map_or(0.0, |s| (s.len().saturating_sub(2)) as f32);
        self.gate = true;
        self.active = self.amp > 0.0 && self.sample.is_some();
    }

    /// Stop emitting; live grains finish their window and the voice goes
    /// inactive on its own.
    fn note_off(&mut self) {
        self.gate = false;
    }

    fn is_active(&self) -> bool {
        self.active
    }

    fn render(&mut self, out: &mut [f32]) {
        if !self.active {
            out.fill(0.0);
            return;
        }
        for slot in out.iter_mut() {
            // Grain scheduling: one spawn per hop while gated.
            if self.gate {
                if self.hop_counter == 0 {
                    self.spawn_grain();
                }
                self.hop_counter = (self.hop_counter + 1) % self.hop_frames;
            }
            let mut v = 0.0f32;
            for g in self.grains.iter_mut() {
                if !g.alive {
                    continue;
                }
                let t = g.age as f32 / g.len as f32;
                // Hann window: hard-zero edges, no boundary clicks.
                let win = 0.5 * (1.0 - (std::f32::consts::TAU * t).cos());
                let i = g.read as usize;
                if let Some(s) = self.sample.as_ref() {
                    if i + 1 < s.len() {
                        let frac = g.read - i as f32;
                        v += (s[i] + (s[i + 1] - s[i]) * frac) * win;
                    }
                }
                g.read += g.speed;
                g.age += 1;
                if g.age >= g.len {
                    g.alive = false;
                }
            }
            v *= self.amp;
            if v.abs() < SILENCE_ABS {
                v = 0.0;
            }
            *slot = v;
            if !self.gate && self.grains.iter().all(|g| !g.alive) {
                self.active = false;
            }
        }
        self.amp = flush_denormal(self.amp);
    }

    fn set_param(&mut self, _param: crate::ParamId, _value: f32) {}

    fn reset(&mut self) {
        self.grains = [Grain::default(); MAX_GRAINS];
        self.gate = false;
        self.active = false;
        self.sweep_pos = 0.0;
        self.hop_counter = 0;
        self.amp = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BLOCK_FRAMES;

    fn sine_source() -> Arc<[f32]> {
        let n = 48_000; // 1 s
        (0..n)
            .map(|i| (std::f32::consts::TAU * 220.0 * i as f32 / 48_000.0).sin() * 0.5)
            .collect::<Vec<_>>()
            .into()
    }

    fn voice() -> GranularVoice {
        let mut v = GranularVoice::new(48_000);
        v.set_source(sine_source(), 48_000);
        v
    }

    fn render_note(v: &mut GranularVoice, frames: usize) -> Vec<f32> {
        v.note_on(60.0, 0.9);
        let mut out = Vec::with_capacity(frames);
        let mut buf = [0.0f32; BLOCK_FRAMES];
        while out.len() < frames {
            v.render(&mut buf);
            out.extend_from_slice(&buf);
        }
        v.note_off();
        // Drain until inactive (bounded: grain tails are <= 200 ms).
        let mut guard = 0;
        while v.is_active() {
            v.render(&mut buf);
            out.extend_from_slice(&buf);
            guard += 1;
            assert!(guard < 1000, "grain voice never went inactive");
        }
        out
    }

    #[test]
    fn cloud_is_deterministic_per_note() {
        let run = || render_note(&mut voice(), 48_000);
        let a = run();
        let b = run();
        assert_eq!(a.len(), b.len());
        assert!(
            a.iter().zip(b.iter()).all(|(x, y)| x.to_bits() == y.to_bits()),
            "same note must render bit-identically"
        );
        let peak = a.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.01, "silent cloud: {peak}");
        assert!(a.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn note_off_stops_emission_and_tail_drains_to_exact_zero() {
        let mut v = voice();
        v.note_on(60.0, 0.9);
        let mut buf = [0.0f32; BLOCK_FRAMES];
        for _ in 0..50 {
            v.render(&mut buf); // ~50 ms of gated emission
        }
        v.note_off();
        let mut guard = 0;
        while v.is_active() {
            v.render(&mut buf);
            guard += 1;
            assert!(guard < 1000);
        }
        let mut tail = [1.0f32; BLOCK_FRAMES];
        v.render(&mut tail);
        assert!(tail.iter().all(|&s| s == 0.0), "tail must be exactly zero");
    }

    #[test]
    fn zero_velocity_and_missing_source_stay_silent_but_safe() {
        let mut v = voice();
        v.note_on(60.0, 0.0);
        assert!(!v.is_active());
        let mut buf = [0.5f32; BLOCK_FRAMES];
        v.render(&mut buf);
        assert!(buf.iter().all(|&s| s == 0.0));

        let mut bare = GranularVoice::new(48_000);
        bare.note_on(60.0, 1.0);
        assert!(!bare.is_active(), "no source: silent");
        bare.render(&mut buf);
        assert!(buf.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn grain_table_is_bounded_when_density_outruns_capacity() {
        let mut v = voice();
        v.set_config(GrainConfig {
            grain_ms: 200.0,   // 9600 frames
            density: 200.0,    // 240-frame hop -> ~40 overlapping grains
            spray_ms: 100.0,
            pitch_jitter_cents: 50.0,
            level: 1.0,
        });
        v.note_on(60.0, 1.0);
        let mut buf = [0.0f32; BLOCK_FRAMES];
        for _ in 0..50 {
            v.render(&mut buf); // ~50 ms of gated emission
        }
        v.note_off();
        let mut guard = 0;
        while v.is_active() {
            v.render(&mut buf);
            guard += 1;
            assert!(guard < 10_000, "oversubscribed cloud never settled");
        }
        let mut tail = [1.0f32; BLOCK_FRAMES];
        v.render(&mut tail);
        assert!(tail.iter().all(|&s| s == 0.0));
    }
}
