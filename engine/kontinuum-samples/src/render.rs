//! Deterministic recipe renderer (issue #53, step 1): recipe + seed → PCM.
//! Every hit is a fresh voice instance rendered to silence at its frame
//! offset, so hits never interact and the render is a pure function of the
//! document. Randomness comes only from the seeded humanization stream.

use kontinuum_core::voice::Sampler as SamplerVoice;
use kontinuum_core::{Voice, SILENCE_ABS};
use kontinuum_clock::stream;
use kontinuum_ir::InstrumentDef;

use crate::choke::{self, HitPart};
use crate::expr;
use crate::granular;
use crate::schema::{
    validate, bounds, ChainKind, ChainStep, RecipeError, RecipeHit, RenderedSample, SampleRecipe,
    SliceMode,
};

/// RNG purpose selector for hit humanization.
const PURPOSE_HUMANIZE: u16 = 0x53;
/// Timing jitter window: ±6 ms, the edge of "played" before it reads sloppy.
const TIMING_JITTER_MS: f32 = 6.0;
/// Velocity jitter window: ±0.1 around the written velocity.
const VELOCITY_JITTER: f32 = 0.1;

/// Renders a validated recipe. Same document → bit-identical output.
pub fn render_recipe(recipe: &SampleRecipe) -> Result<RenderedSample, RecipeError> {
    validate(recipe)?;
    let sr = recipe.sample_rate.max(8000);
    let tail_ms = recipe.tail_ms.unwrap_or(1000.0).clamp(bounds::TAIL_MS.0, bounds::TAIL_MS.1);
    let end_ms = recipe
        .hits
        .iter()
        .map(|h| h.at_ms)
        .fold(0.0f32, f32::max)
        + tail_ms;
    let total = ((end_ms / 1000.0) * sr as f32).ceil() as usize + 1;
    let mut pcm = vec![0.0f32; total];

    let mut parts: Vec<HitPart> = Vec::with_capacity(recipe.hits.len());
    for (hi, hit) in recipe.hits.iter().enumerate() {
        let (mut at_ms, mut velocity) = humanize(&recipe.seed, hi, hit);
        let mut pitch = hit.pitch;
        if let Some(spec) = &hit.expression {
            let sel = expr::select_hit(recipe.seed, hi, spec, hit.velocity);
            velocity = sel.velocity;
            pitch += sel.pitch_offset;
            at_ms = (at_ms + sel.timing_ms).max(0.0);
        }
        parts.push(render_part(recipe, hit, at_ms, velocity, pitch, sr)?);
    }
    choke::mix(&mut pcm, &mut parts, sr);

    if let Some(texture) = &recipe.texture {
        let instrument = recipe
            .voices
            .iter()
            .find(|v| v.id == texture.source_voice)
            .map(|v| &v.instrument)
            .ok_or_else(|| RecipeError::UnknownVoice(texture.source_voice.clone()))?;
        let source = render_hit(voice_for(instrument, sr), texture.velocity, texture.pitch, sr);
        let cloud = granular::render_cloud(&source, sr, texture, total, recipe.seed);
        for (slot, s) in pcm.iter_mut().zip(cloud.iter()) {
            *slot += texture.level * s;
        }
    }

    let slices = match &recipe.slice {
        Some(spec) => match spec.mode {
            SliceMode::Transient => kontinuum_core::slice::detect_slices(
                &pcm,
                sr,
                spec.max_slices as usize,
                spec.sensitivity,
            ),
            SliceMode::FixedMs => {
                let interval = spec.interval_ms.unwrap_or(500.0);
                let step = ((interval / 1000.0) * sr as f32).round() as usize;
                (0..pcm.len()).step_by(step.max(1)).collect()
            }
        },
        None => vec![0],
    };

    Ok(RenderedSample {
        pcm,
        sample_rate: sr,
        slices,
        tags: recipe.tags.clone(),
        hash: crate::schema::recipe_hash(recipe),
    })
}

/// Render one hit (voice + chain) into a mixable part. Voice lookup is
/// re-checked instead of assumed so the render path stays panic-free.
fn render_part(
    recipe: &SampleRecipe,
    hit: &RecipeHit,
    at_ms: f32,
    velocity: f32,
    pitch: f32,
    sr: u32,
) -> Result<HitPart, RecipeError> {
    let instrument = recipe
        .voices
        .iter()
        .find(|v| v.id == hit.voice)
        .map(|v| &v.instrument)
        .ok_or_else(|| RecipeError::UnknownVoice(hit.voice.clone()))?;
    let start = ((at_ms / 1000.0) * sr as f32).round() as usize;
    let mut buffer = render_hit(voice_for(instrument, sr), velocity, pitch, sr);
    apply_chain(&mut buffer, sr, chain_of(recipe, hit));
    Ok(HitPart { start, group: hit.choke_group, buf: buffer })
}

fn chain_of<'a>(recipe: &'a SampleRecipe, hit: &RecipeHit) -> &'a [ChainStep] {
    recipe
        .voices
        .iter()
        .find(|v| v.id == hit.voice)
        .map(|v| v.chain.as_slice())
        .unwrap_or(&[])
}

/// Seeded, per-hit timing and velocity drift — the "played, not placed"
/// quality. Deterministic in (seed, hit index).
fn humanize(seed: &u64, hit_index: usize, hit: &RecipeHit) -> (f32, f32) {
    let mut rng = stream(*seed, hit_index as u8, PURPOSE_HUMANIZE);
    let dt = rng.range_f32(-TIMING_JITTER_MS, TIMING_JITTER_MS);
    let dv = rng.range_f32(-VELOCITY_JITTER, VELOCITY_JITTER);
    (
        (hit.at_ms + dt).max(0.0),
        (hit.velocity + dv).clamp(0.05, 1.0),
    )
}

/// Fresh voice instance per hit: no stealing, no cross-hit state. Synth
/// kinds construct through the first-party plugin registry (#51); sample
/// slots and custom patches fall back to the sampler voice (the pack-render
/// path has no patch evaluator yet, #37 follow-up).
fn voice_for(def: &InstrumentDef, sr: u32) -> Box<dyn Voice> {
    static REGISTRY: std::sync::LazyLock<kontinuum_plugin_api::Registry> =
        std::sync::LazyLock::new(|| kontinuum_instruments_core::registry());
    if let Some(id) = def.kind_id() {
        if let Some(plugin) = REGISTRY.get(id) {
            let mut voice = plugin.make_voice(sr);
            let schema = plugin.params();
            for (name, value) in def.param_values() {
                if let Some(spec) = schema.iter().find(|s| s.name == name) {
                    voice.set_param(spec.param, value);
                }
            }
            return voice;
        }
    }
    Box::new(SamplerVoice::new(sr))
}

/// Render one hit to silence (capped at the recipe tail ceiling).
fn render_hit(mut voice: Box<dyn Voice>, velocity: f32, pitch: f32, sr: u32) -> Vec<f32> {
    voice.note_on(pitch, velocity);
    let mut out = Vec::new();
    let cap = sr as usize * bounds::TAIL_MS.1 as usize / 1000;
    let mut scratch = vec![0.0f32; 64];
    while voice.is_active() && out.len() < cap {
        for slot in scratch.iter_mut() {
            *slot = 0.0;
        }
        voice.render(&mut scratch);
        out.extend_from_slice(&scratch);
    }
    if out.is_empty() {
        out.resize(64, 0.0);
    }
    out
}

/// Per-hit processing chain, in document order.
fn apply_chain(buffer: &mut [f32], sr: u32, chain: &[ChainStep]) {
    for step in chain {
        let mix = step.mix.clamp(0.0, 1.0);
        match step.kind {
            ChainKind::Drive => {
                for s in buffer.iter_mut() {
                    *s = mix * ((*s * step.amount).tanh()) + (1.0 - mix) * *s;
                }
            }
            ChainKind::Lowpass | ChainKind::Highpass => {
                let fc = step.amount.clamp(20.0, sr as f32 * 0.45);
                let a = 1.0 - (-std::f32::consts::TAU * fc / sr as f32).exp();
                let mut lp = 0.0f32;
                for s in buffer.iter_mut() {
                    lp += a * (*s - lp);
                    *s = if step.kind == ChainKind::Lowpass { lp } else { *s - lp };
                }
            }
        }
    }
}

/// Rendered buffers hard-mute below the core silence threshold; a fully
/// silent hit (e.g. zero velocity writes) stays a valid empty slot.
pub fn is_silent(buffer: &[f32]) -> bool {
    buffer.iter().all(|s| s.abs() < SILENCE_ABS)
}
