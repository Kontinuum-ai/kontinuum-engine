//! Granular texture bed (issue #19, granular v0): a single-source grain
//! cloud. Grains are scheduled on a fixed hop, read positions sweep the
//! source once across the cloud with seeded spray/pitch jitter, and grains
//! overlap-add through a chosen window. Everything is derived from
//! `(seed, grain index)`, so the cloud is bit-reproducible.
//!
//! CPU: O(output_len × overlap) with overlap = density × grain length —
//! a 48 kHz, 4 s bed at 100 ms grains / 25 grains/s overlaps ~2.5×, i.e.
//! roughly 3 resampled reads per output sample. Budget 1–2 instances per
//! session (cost table #11 L3).

use serde::{Deserialize, Serialize};

use kontinuum_clock::stream;

use crate::schema::{bounds, check, RecipeError};

/// RNG purpose selector for grain draws.
const PURPOSE_GRAIN: u16 = 0x56;

/// L2 bounds + source resolution for the texture spec, called from
/// `schema::validate`.
pub(crate) fn validate_grain(spec: &GrainSpec, known_voices: &[&str]) -> Result<(), RecipeError> {
    if !known_voices.contains(&spec.source_voice.as_str()) {
        return Err(RecipeError::UnknownVoice(spec.source_voice.clone()));
    }
    check("grain_ms", spec.grain_ms, bounds::GRAIN_MS)?;
    check("density", spec.density, bounds::GRAIN_DENSITY)?;
    check("spray_ms", spec.spray_ms, bounds::GRAIN_SPRAY_MS)?;
    check("pitch_jitter_cents", spec.pitch_jitter_cents, bounds::GRAIN_PITCH_JITTER)?;
    check("level", spec.level, bounds::GRAIN_LEVEL)?;
    check("pitch", spec.pitch, bounds::GRAIN_PITCH)?;
    check("velocity", spec.velocity, bounds::GRAIN_VELOCITY)?;
    Ok(())
}

/// Grain amplitude window. Hann is the default: hard-zero edges mean no
/// window-boundary clicks at any hop.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrainWindow {
    #[default]
    Hann,
    Hamming,
    Triangle,
}

impl GrainWindow {
    /// Window gain at grain-relative position `t` in 0..1.
    pub fn gain(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            GrainWindow::Hann => 0.5 * (1.0 - (std::f32::consts::TAU * t).cos()),
            GrainWindow::Hamming => 0.54 - 0.46 * (std::f32::consts::TAU * t).cos(),
            GrainWindow::Triangle => 1.0 - (2.0 * t - 1.0).abs(),
        }
    }
}

/// Texture-bed spec (schema fragment). `source_voice` names a recipe voice
/// rendered once as the grain source.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrainSpec {
    /// Voice id — must exist in the recipe's `voices`.
    pub source_voice: String,
    /// Grain size in ms (20..=200).
    pub grain_ms: f32,
    /// Grains per second (1..=200). Output level scales with overlap
    /// (grain_ms/1000 × density); use `level` to balance the bed.
    pub density: f32,
    /// Random read-position jitter per grain in ±ms (0..=1000).
    #[serde(default)]
    pub spray_ms: f32,
    /// Random per-grain tuning jitter in ±cents (0..=1200).
    #[serde(default)]
    pub pitch_jitter_cents: f32,
    #[serde(default)]
    pub window: GrainWindow,
    /// Bed mix level 0..=1.
    #[serde(default = "d_level")]
    pub level: f32,
    /// Source-voice pitch (0..=127).
    #[serde(default = "d_pitch")]
    pub pitch: f32,
    /// Source-voice velocity (0..=1).
    #[serde(default = "d_velocity")]
    pub velocity: f32,
}

fn d_level() -> f32 {
    0.5
}
fn d_pitch() -> f32 {
    60.0
}
fn d_velocity() -> f32 {
    0.8
}

/// Render one deterministic grain cloud over `duration_frames`. The source
/// is swept front-to-back across the cloud; each grain is linear-interp
/// resampled at its jittered pitch and windowed before overlap-add.
pub fn render_cloud(
    source: &[f32],
    sample_rate: u32,
    spec: &GrainSpec,
    duration_frames: usize,
    seed: u64,
) -> Vec<f32> {
    let mut out = vec![0.0f32; duration_frames];
    if source.len() < 16 || duration_frames == 0 {
        return out;
    }
    let grain_len = ((spec.grain_ms / 1000.0) * sample_rate as f32).round() as usize;
    let grain_len = grain_len.clamp(16, source.len());
    let hop = (sample_rate as f32 / spec.density).round().max(1.0) as usize;
    let n_grains = duration_frames / hop + 1;
    let spray_frames = (spec.spray_ms / 1000.0) * sample_rate as f32;
    // Sweep span leaves room for one full grain inside the source.
    let sweep = (source.len() - grain_len) as f32;

    for i in 0..n_grains {
        // Deterministic per-grain stream: low bits spread the track byte,
        // high bits offset the purpose so grain counts above 256 stay
        // independent.
        let mut rng = stream(seed, (i & 0xff) as u8, PURPOSE_GRAIN + (i >> 8) as u16);
        let start = (i * hop).min(duration_frames.saturating_sub(1));
        let base = i as f32 / n_grains as f32 * sweep;
        let read = (base + rng.range_f32(-spray_frames, spray_frames)).clamp(0.0, sweep);
        let rate = (rng.range_f32(-spec.pitch_jitter_cents, spec.pitch_jitter_cents)
            / 1200.0)
            .exp2();
        for j in 0..grain_len {
            let frame = start + j;
            if frame >= duration_frames {
                break;
            }
            let pos = read + j as f32 * rate;
            if pos < 0.0 || pos >= (source.len() - 1) as f32 {
                continue;
            }
            let idx = pos as usize;
            let frac = pos - idx as f32;
            let s = source[idx] + (source[idx + 1] - source[idx]) * frac;
            out[frame] += s * spec.window.gain(j as f32 / grain_len as f32);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> GrainSpec {
        GrainSpec {
            source_voice: "pad".into(),
            grain_ms: 60.0,
            density: 25.0,
            spray_ms: 40.0,
            pitch_jitter_cents: 30.0,
            window: GrainWindow::Hann,
            level: 0.5,
            pitch: 60.0,
            velocity: 0.8,
        }
    }

    /// A warm-ish source: decaying noise-free sine, 0.25 s at 48 kHz.
    fn source() -> Vec<f32> {
        (0..12_000)
            .map(|i| (std::f32::consts::TAU * 220.0 * i as f32 / 48_000.0).sin() * 0.5)
            .collect()
    }

    #[test]
    fn windows_have_zero_edges_and_unit_center() {
        assert_eq!(GrainWindow::Hann.gain(0.0), 0.0);
        assert_eq!(GrainWindow::Hann.gain(1.0), 0.0);
        assert_eq!(GrainWindow::Triangle.gain(0.0), 0.0);
        assert!((GrainWindow::Hann.gain(0.5) - 1.0).abs() < 1e-6);
        assert!((GrainWindow::Triangle.gain(0.5) - 1.0).abs() < 1e-6);
        assert!(GrainWindow::Hamming.gain(0.0) > 0.0, "hamming keeps a floor");
    }

    #[test]
    fn cloud_is_deterministic_and_fills_the_duration() {
        let a = render_cloud(&source(), 48_000, &spec(), 96_000, 42);
        let b = render_cloud(&source(), 48_000, &spec(), 96_000, 42);
        assert_eq!(a.len(), 96_000);
        assert!(a.iter().zip(b.iter()).all(|(x, y)| x.to_bits() == y.to_bits()));
        let peak = a.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.01, "silent cloud: {peak}");
        assert!(a.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn seed_changes_the_cloud_but_jitter_free_is_seed_stable() {
        let a = render_cloud(&source(), 48_000, &spec(), 48_000, 1);
        let b = render_cloud(&source(), 48_000, &spec(), 48_000, 2);
        assert_ne!(a, b, "spray/pitch jitter must be seed-dependent");
        let mut still = spec();
        still.spray_ms = 0.0;
        still.pitch_jitter_cents = 0.0;
        let c = render_cloud(&source(), 48_000, &still, 48_000, 1);
        let d = render_cloud(&source(), 48_000, &still, 48_000, 2);
        assert_eq!(c, d, "no jitter: the bed must not depend on the seed");
    }

    #[test]
    fn density_sets_the_overlap_texture() {
        let mut sparse = spec();
        sparse.density = 5.0;
        let mut dense = spec();
        dense.density = 100.0;
        let energy = |x: &[f32]| x.iter().map(|s| s * s).sum::<f32>();
        assert!(energy(&render_cloud(&source(), 48_000, &dense, 48_000, 3))
            > energy(&render_cloud(&source(), 48_000, &sparse, 48_000, 3)));
    }

    #[test]
    fn degenerate_inputs_stay_silent_and_sized() {
        assert_eq!(render_cloud(&[], 48_000, &spec(), 4800, 1), vec![0.0; 4800]);
        assert!(render_cloud(&source(), 48_000, &spec(), 0, 1).is_empty());
    }
}
