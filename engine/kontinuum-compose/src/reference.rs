//! Reference-track analysis (Phase R1, research doc §2.2): decode an audio
//! file, estimate BPM + energy + onset density, and adapt the generator.
//! File import only — YouTube URLs are the server pipeline (out of scope here).

use hound::{SampleFormat, WavReader};
use serde::{Deserialize, Serialize};


use kontinuum_ir::schema::{EpInstrument, EpTag, Pattern, Session};

use crate::taste::{session_from_taste, TasteProfile};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ReferenceAnalysis {
    pub bpm: f64,
    /// Mean RMS 0..1 across the file.
    pub energy: f64,
    /// Onsets per second (density proxy).
    pub onset_rate: f64,
    pub duration_seconds: f64,
}

pub fn decode_wav_mono(path: &str) -> Result<(Vec<f32>, u32), String> {
    let reader = WavReader::open(path).map_err(|e| e.to_string())?;
    let spec = reader.spec();
    let sr = spec.sample_rate;
    let mut mono = Vec::new();
    match spec.sample_format {
        SampleFormat::Float => {
            for sample in reader.into_samples::<f32>() {
                mono.push(sample.map_err(|e| e.to_string())?);
            }
        }
        _ => {
            for sample in reader.into_samples::<i32>() {
                // IntoSamples<i32> normalizes any int depth to i32.
                let s = sample.map_err(|e| e.to_string())?;
                mono.push(s as f32 / 2_147_483_648.0);
            }
        }
    }
    Ok((mono, sr))
}

/// Mono f32 -> half-wave-rectified onset envelope at `env_hz`.
fn onset_envelope(samples: &[f32], sr: u32, env_hz: u32) -> Vec<f32> {
    let win = (sr / env_hz).max(1) as usize;
    let mut env = Vec::with_capacity(samples.len() / win + 1);
    let mut i = 0;
    while i + win <= samples.len() {
        let rms = (samples[i..i + win].iter().map(|s| s * s).sum::<f32>() / win as f32).sqrt();
        env.push(rms);
        i += win;
    }
    env.windows(2)
        .map(|w| (w[1] - w[0]).max(0.0))
        .collect()
}

/// BPM estimate: autocorrelation peak of the onset envelope in 60..200 BPM.
fn estimate_bpm(env: &[f64], env_hz: f64) -> f64 {
    if env.len() < 8 {
        return 124.0;
    }
    let mut best_bpm = 124.0f64;
    let mut best_score = -1.0f64;
    let mut bpm = 60.0f64;
    while bpm <= 200.0 {
        let lag = (env_hz * 60.0 / bpm).round() as usize;
        if lag >= 2 && lag < env.len() {
            let mut score = 0.0;
            for i in 0..env.len() - lag {
                score += env[i] * env[i + lag];
            }
            // Prefer slower multiples on ties (half-time ambiguity).
            if bpm < 150.0 {
                score *= 1.05;
            }
            if score > best_score {
                best_score = score;
                best_bpm = bpm;
            }
        }
        bpm += 0.5;
    }
    (best_bpm * 2.0 % 1.0 > 0.0).then_some(best_bpm).unwrap_or(best_bpm)
}

pub fn analyze_wav(path: &str) -> Result<ReferenceAnalysis, String> {
    let (samples, sr) = decode_wav_mono(path)?;
    analyze_samples(&samples, sr)
}

/// Analyze already-decoded mono PCM (the device path: Swift decodes any
/// container with AVAudioFile and hands frames across the FFI).
pub fn analyze_samples(samples: &[f32], sr: u32) -> Result<ReferenceAnalysis, String> {
    if samples.len() < sr as usize / 2 {
        return Err("file too short for analysis (need >= 0.5 s)".into());
    }
    let env_hz = 100u32;
    let env_f32 = onset_envelope(&samples, sr, env_hz);
    let env: Vec<f64> = env_f32.iter().map(|e| f64::from(*e)).collect();
    let bpm = estimate_bpm(&env, f64::from(env_hz));
    let mean_rms = samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32;
    let energy = (mean_rms.sqrt() * 2.5).clamp(0.05, 1.0) as f64;
    let onsets = env.iter().filter(|e| **e > 0.02).count() as f64;
    let onset_rate = (onsets / env_hz as f64).clamp(0.5, 20.0);
    Ok(ReferenceAnalysis {
        bpm,
        energy,
        onset_rate,
        duration_seconds: samples.len() as f64 / sr as f64,
    })
}

/// Instrument rack chosen by the analysis (plugin-style loading): the
/// reference's features decide which machines the generated session uses.
/// Sparse/quiet references load a minimal rack; dense/loud ones load the
/// full percussion and harmony set. Low energy swaps the saw pad for the
/// FM electric piano (deep character).
pub fn rack_for_analysis(a: &ReferenceAnalysis) -> Vec<&'static str> {
    let mut rack = vec!["kick", "bass"];
    if a.onset_rate > 3.0 {
        rack.push("hat");
    }
    if a.onset_rate > 7.0 {
        rack.push("perc");
    }
    if a.energy > 0.35 {
        rack.push("pad");
    }
    if a.onset_rate > 9.0 {
        rack.push("stab");
    }
    rack
}

fn adapt_session_to_rack(session: &mut Session, rack: &[&'static str], a: &ReferenceAnalysis) {
    use kontinuum_ir::schema::InstrumentDef;
    let keep: Vec<&str> = rack.to_vec();
    session.tracks.retain(|t| keep.contains(&t.id.as_str()));
    session.tracks.iter_mut().for_each(|t| {
        // Low-energy references get the EP on the pad track: deep character.
        if t.id == "pad" && a.energy < 0.55 {
            t.instrument = InstrumentDef::Ep(EpInstrument {
                kind: EpTag::Ep,
                decay_ms: 1600.0,
                depth: 2.2,
            });
        }
    });
    for sec in session.sections.iter_mut() {
        let ids: Vec<String> = keep.iter().map(|s| s.to_string()).collect();
        sec.pattern_bindings.retain(|k, _| ids.contains(k));
        if sec.energy_curve.iter().any(|e| *e > 0.6) {
            if let Some(pat) = sec.pattern_bindings.get_mut("hat") {
                // Dense references: open the hats up a touch.
                if let Pattern::Euclidean(e) = pat {
                    e.velocity = (e.velocity + 0.1).min(1.0);
                }
            }
        }
    }
}

pub fn taste_from_reference(a: &ReferenceAnalysis) -> TasteProfile {
    TasteProfile {
        bpm: Some(a.bpm.clamp(60.0, 200.0)),
        energy: a.energy.clamp(0.1, 1.0) as f32,
        darkness: 0.7,
        density: (a.onset_rate / 12.0).clamp(0.25, 1.0) as f32,
        variation: 0.55,
        genres: vec!["techno".into()],
        ..Default::default()
    }
}

/// The one-call integration: reference file in, adapted session out.
pub fn session_from_reference_wav(path: &str, seed: u64) -> Result<Session, String> {
    let (samples, sr) = decode_wav_mono(path)?;
    session_from_reference_samples(&samples, sr, seed)
}

/// One-call integration for decoded PCM.
pub fn session_from_reference_samples(
    samples: &[f32],
    sr: u32,
    seed: u64,
) -> Result<Session, String> {
    let analysis = analyze_samples(samples, sr)?;
    let mut session = session_from_taste(&taste_from_reference(&analysis), seed);
    let rack = rack_for_analysis(&analysis);
    adapt_session_to_rack(&mut session, &rack, &analysis);
    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hound::{SampleFormat, WavSpec};

    #[test]
    fn a_120bpm_click_track_estimates_near_120() {
        // Synthesize a 1 kHz-decaying click every 0.5 s (120 BPM), 8 s.
        let sr = 48_000u32;
        let spec = WavSpec { channels: 1, sample_rate: sr, bits_per_sample: 32, sample_format: SampleFormat::Float };
        let mut writer = hound::WavWriter::create("/tmp/ref-test.wav", spec).unwrap();
        let click_len = (sr as f32 * 0.05) as usize;
        let mut samples = vec![0.0f32; sr as usize * 8];
        let mut t = 0usize;
        while t < samples.len() {
            for i in 0..click_len {
                let env = (-(i as f32) / (0.01 * sr as f32)).exp();
                samples[t + i] += (std::f32::consts::TAU * 800.0 * i as f32 / sr as f32).sin() * env * 0.8;
            }
            t += sr as usize / 2; // 120 BPM
        }
        for s in &samples {
            writer.write_sample(*s).unwrap();
        }
        drop(writer);
        let a = analyze_wav("/tmp/ref-test.wav").unwrap();
        assert!((a.bpm - 120.0).abs() < 4.0, "bpm off: {}", a.bpm);
        assert!(a.energy > 0.1);
    }

    #[test]
    fn reference_generates_a_valid_session() {
        let a = ReferenceAnalysis { bpm: 126.0, energy: 0.7, onset_rate: 6.0, duration_seconds: 200.0 };
        let session = session_from_taste(&taste_from_reference(&a), 42);
        assert!(kontinuum_ir::validate_session(&session).is_ok());
    }
}
