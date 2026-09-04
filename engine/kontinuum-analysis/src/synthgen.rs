//! Synthetic reference-track generator for the #23 corpus pipeline.
//!
//! This is FIXTURE MACHINERY, not a music generator: it renders short,
//! deterministic, obviously-synthetic loop tracks (kick / hat / clap /
//! bass / pad built from oscillators and filtered noise) whose arrangement
//! is PLANTED — section order, lengths, boundary treatments, tempo, key,
//! and groove are construction parameters. The corpus pipeline's
//! self-consistency check (segmentation F1, tempo/key recovery, fit
//! recovery, consumer loading) runs against these plants, where the truth
//! is known by construction. It is never used for real analysis targets
//! and nothing here is in the real-time path.
//!
//! Presets are referenced from `corpus/manifest.csv` rows with
//! `synthetic=true` via the `synth_spec` column.

use kontinuum_clock::Rng;

use crate::filters::{highpass_coeffs, lowpass_coeffs, Biquad};

/// Fixture render rate — low on purpose; the pipeline is sample-rate
/// agnostic and the fixtures only need to fit 22050 Hz Nyquist.
pub const SYNTH_SAMPLE_RATE: u32 = 22_050;

/// How a section ENTERS. `SilentBar` plants a near-silent first bar —
/// the "silence" boundary treatment the classifier must find.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    Normal,
    SilentBar,
}

/// How a section EXITS into the next one. These plant the boundary
/// treatments: `Riser` → filter sweep, `DrumFill` → fill, `FadeOut` and
/// `Normal` → hard cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    Normal,
    Riser,
    DrumFill,
    FadeOut,
}

#[derive(Clone, Copy, Debug)]
pub struct SynthSection {
    pub kind: &'static str,
    pub bars: u32,
    pub entry: Entry,
    pub exit: Exit,
}

/// A complete planted fixture track.
#[derive(Clone, Debug)]
pub struct SynthPreset {
    pub id: &'static str,
    pub track_id: &'static str,
    pub subgenre: &'static str,
    pub bpm: f64,
    /// Pitch class of the planted minor key, C = 0.
    pub root_pc: u8,
    pub seed: u64,
    /// Planted lateness of the off-8th hats, in ticks (120 per 16th).
    pub swing_ticks: f32,
    /// Planted off-8th hat velocity (the groove-family axis).
    pub hat_velocity: f32,
    pub sections: &'static [SynthSection],
}

/// The boundary type a (exit, entry) pair plants, in the detector's
/// vocabulary. Order matters nowhere here — the CLASSIFIER guesses it
/// from audio; this is the grading truth.
pub fn planted_boundary_type(prev: &SynthSection, next: &SynthSection) -> &'static str {
    match (prev.exit, next.entry) {
        (_, Entry::SilentBar) => "silence",
        (Exit::Riser, _) => "filter_sweep",
        (Exit::DrumFill, _) => "fill",
        _ => "hard_cut",
    }
}

/// Ground-truth annotation for one preset, in the corpus crate's
/// annotation format (bars on the planted grid).
pub fn planted_annotation(preset: &SynthPreset) -> kontinuum_corpus::SegmentationAnnotation {
    let mut sections = Vec::new();
    let mut bar = 0u32;
    for s in preset.sections {
        sections.push(kontinuum_corpus::AnnotatedSection {
            start_bar: bar,
            bars: s.bars,
            label: Some(s.kind.to_string()),
        });
        bar += s.bars;
    }
    kontinuum_corpus::SegmentationAnnotation {
        track_id: preset.track_id.to_string(),
        // Machine truth gets a 2-bar tolerance: the detected grid
        // quantizes boundaries to ±1 bar, and the novelty peak itself
        // lands ±1 around the plant. Hand annotations (real tracks) stay
        // strict at 1.
        tolerance_bars: 2,
        sections,
    }
}

/// Ground-truth transition types for one preset, aligned with the
/// interior boundary list (section i → i+1 for i in 0..n-1).
pub fn planted_transition_types(preset: &SynthPreset) -> Vec<&'static str> {
    preset
        .sections
        .windows(2)
        .map(|w| planted_boundary_type(&w[0], &w[1]))
        .collect()
}

const MT_SECTIONS: &[SynthSection] = &[
    SynthSection { kind: "intro", bars: 8, entry: Entry::Normal, exit: Exit::DrumFill },
    SynthSection { kind: "build", bars: 8, entry: Entry::Normal, exit: Exit::Riser },
    SynthSection { kind: "drop", bars: 16, entry: Entry::Normal, exit: Exit::Normal },
    SynthSection { kind: "break", bars: 8, entry: Entry::SilentBar, exit: Exit::DrumFill },
    SynthSection { kind: "groove", bars: 16, entry: Entry::Normal, exit: Exit::Normal },
    SynthSection { kind: "outro", bars: 8, entry: Entry::Normal, exit: Exit::FadeOut },
];

const MH_SECTIONS: &[SynthSection] = &[
    SynthSection { kind: "intro", bars: 12, entry: Entry::Normal, exit: Exit::Riser },
    SynthSection { kind: "drop", bars: 16, entry: Entry::Normal, exit: Exit::Normal },
    SynthSection { kind: "break", bars: 8, entry: Entry::SilentBar, exit: Exit::DrumFill },
    SynthSection { kind: "groove", bars: 12, entry: Entry::Normal, exit: Exit::Normal },
    SynthSection { kind: "outro", bars: 8, entry: Entry::Normal, exit: Exit::FadeOut },
];

/// The in-repo fixture corpus: 6 minimal-techno + 6 microhouse tracks
/// (three groove families each — straight / swung / pushed — across varied
/// tempo/key). These are the rows `corpus/manifest.csv` points at. Six per
/// subgenre (not fewer) so the fitted transition-type conditionals carry
/// enough evidence mass for the sampled-majority validation.
pub const PRESETS: &[SynthPreset] = &[
    SynthPreset {
        id: "mt-a", track_id: "syn-mt-a", subgenre: "minimal-techno",
        bpm: 128.0, root_pc: 9, seed: 101, swing_ticks: 0.0, hat_velocity: 0.75,
        sections: MT_SECTIONS,
    },
    SynthPreset {
        id: "mt-b", track_id: "syn-mt-b", subgenre: "minimal-techno",
        bpm: 126.0, root_pc: 6, seed: 102, swing_ticks: 18.0, hat_velocity: 0.5,
        sections: MT_SECTIONS,
    },
    SynthPreset {
        id: "mt-c", track_id: "syn-mt-c", subgenre: "minimal-techno",
        bpm: 130.0, root_pc: 4, seed: 103, swing_ticks: 9.0, hat_velocity: 0.62,
        sections: MT_SECTIONS,
    },
    SynthPreset {
        id: "mt-d", track_id: "syn-mt-d", subgenre: "minimal-techno",
        bpm: 129.0, root_pc: 11, seed: 104, swing_ticks: 0.0, hat_velocity: 0.7,
        sections: MT_SECTIONS,
    },
    SynthPreset {
        id: "mt-e", track_id: "syn-mt-e", subgenre: "minimal-techno",
        bpm: 127.0, root_pc: 2, seed: 105, swing_ticks: 15.0, hat_velocity: 0.55,
        sections: MT_SECTIONS,
    },
    SynthPreset {
        id: "mt-f", track_id: "syn-mt-f", subgenre: "minimal-techno",
        bpm: 131.0, root_pc: 0, seed: 106, swing_ticks: 7.0, hat_velocity: 0.66,
        sections: MT_SECTIONS,
    },
    SynthPreset {
        id: "mh-a", track_id: "syn-mh-a", subgenre: "microhouse",
        bpm: 124.0, root_pc: 2, seed: 201, swing_ticks: 0.0, hat_velocity: 0.8,
        sections: MH_SECTIONS,
    },
    SynthPreset {
        id: "mh-b", track_id: "syn-mh-b", subgenre: "microhouse",
        bpm: 122.0, root_pc: 11, seed: 202, swing_ticks: 16.0, hat_velocity: 0.52,
        sections: MH_SECTIONS,
    },
    SynthPreset {
        id: "mh-c", track_id: "syn-mh-c", subgenre: "microhouse",
        bpm: 125.0, root_pc: 7, seed: 203, swing_ticks: 7.0, hat_velocity: 0.68,
        sections: MH_SECTIONS,
    },
    SynthPreset {
        id: "mh-d", track_id: "syn-mh-d", subgenre: "microhouse",
        bpm: 126.0, root_pc: 4, seed: 204, swing_ticks: 0.0, hat_velocity: 0.75,
        sections: MH_SECTIONS,
    },
    SynthPreset {
        id: "mh-e", track_id: "syn-mh-e", subgenre: "microhouse",
        bpm: 121.0, root_pc: 9, seed: 205, swing_ticks: 14.0, hat_velocity: 0.5,
        sections: MH_SECTIONS,
    },
    SynthPreset {
        id: "mh-f", track_id: "syn-mh-f", subgenre: "microhouse",
        bpm: 123.0, root_pc: 6, seed: 206, swing_ticks: 8.0, hat_velocity: 0.64,
        sections: MH_SECTIONS,
    },
];

/// Looks a preset up by its `synth_spec` id.
pub fn preset_by_id(id: &str) -> Option<&'static SynthPreset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// Renders the planted track as mono f32 (-1..1-ish). Deterministic:
/// same preset, same samples, always.
pub fn render(preset: &SynthPreset) -> Vec<f32> {
    let sr = f64::from(SYNTH_SAMPLE_RATE);
    let beat = 60.0 / preset.bpm;
    let bar = 4.0 * beat;
    let total_bars: u32 = preset.sections.iter().map(|s| s.bars).sum();
    let n = (total_bars as f64 * bar * sr).ceil() as usize;
    let mut out = vec![0.0f32; n];

    let mut rng = Rng::from_seed(preset.seed);
    // Tiny per-track level trim so the fixtures are not bit-identical in
    // loudness; ±4 % is far below every detection threshold used.
    let trim = 0.96 + 0.08 * f64::from(rng.next_f32());

    // Bass: root pitch class in octave 2 (C2 = midi 36). Chord: root in
    // octave 3 plus the minor triad (+3, +7 semitones).
    let root_hz = midi_hz(f64::from(36 + preset.root_pc));
    // The minor third gets doubled an octave up: it carries the mode, and
    // real stab voicings lean on it.
    let chord: [f64; 4] = [
        midi_hz(f64::from(48 + preset.root_pc)),
        midi_hz(f64::from(51 + preset.root_pc)),
        midi_hz(f64::from(55 + preset.root_pc)),
        midi_hz(f64::from(63 + preset.root_pc)),
    ];

    // Section boundary timeline in seconds.
    let mut sec_start_bar = 0u32;
    let mut section_spans = Vec::new();
    for s in preset.sections {
        let start = f64::from(sec_start_bar) * bar;
        let dur = f64::from(s.bars) * bar;
        section_spans.push((start, dur, s));
        sec_start_bar += s.bars;
    }

    let level_of = |kind: &str| match kind {
        "intro" => 0.55,
        "build" => 0.72,
        "drop" => 1.0,
        "break" => 0.5,
        "groove" => 0.85,
        _ => 0.6,
    };

    let mut noise = Rng::from_seed(preset.seed ^ 0x5eed);
    let mut white = move || noise.next_f32() * 2.0 - 1.0;

    // --- drums -------------------------------------------------------
    for &(start, dur, s) in &section_spans {
        let level = level_of(s.kind);
        if s.kind == "break" {
            continue; // pad-only; the fill below still plants its roll
        }
        // Fade multiplier across the section (outro decay).
        let fade = |t: f64| match s.exit {
            Exit::FadeOut => (1.0 - t / dur).max(0.06),
            _ => 1.0,
        };
        let end = start + dur;
        // Kick on every beat.
        let mut t = start;
        while t < end {
            let local = t - start;
            add_kick(&mut out, sr, t, 0.9 * level * fade(local) * trim);
            t += beat;
        }
        // Off-8th hats, planted late by swing_ticks.
        let sixteenth = beat / 4.0;
        let swing_sec = f64::from(preset.swing_ticks) / 120.0 * sixteenth;
        let mut t = start + beat * 0.5 + swing_sec;
        while t < end {
            let local = t - start;
            add_hat(&mut out, sr, t, 0.5 * f64::from(preset.hat_velocity) * level * fade(local) * trim, &mut white);
            t += beat;
        }
        // Clap on beats 2 and 4 (drop / groove only).
        if matches!(s.kind, "drop" | "groove") {
            let mut t = start + beat;
            while t < end {
                add_clap(&mut out, sr, t, 0.65 * level * fade(t - start) * trim, &mut white);
                t += 2.0 * beat;
            }
        }
        // Drum fill: a 16th snare roll with rising level in the last bar
        // (planted "fill" boundary treatment). Fills are meant to stand
        // out — the roll peaks near the section level.
        if s.exit == Exit::DrumFill {
            let fill_start = end - bar;
            let roll_n = 16;
            for i in 0..roll_n {
                let t = fill_start + f64::from(i) * bar / f64::from(roll_n);
                let ramp = 0.4 + 0.6 * f64::from(i) / f64::from(roll_n);
                add_snare(&mut out, sr, t, 1.1 * ramp * level * trim, &mut white);
            }
        }
    }

    // --- bass + stabs (drop / groove), pad (all non-break kinds) -----
    for &(start, dur, s) in &section_spans {
        let level = level_of(s.kind);
        let end = start + dur;
        let fade = |t: f64| match s.exit {
            Exit::FadeOut => (1.0 - t / dur).max(0.06),
            _ => 1.0,
        };
        let silent_bar = s.entry == Entry::SilentBar;
        if matches!(s.kind, "drop" | "groove") && !silent_bar {
            let mut t = start;
            let eighth = beat / 2.0;
            let mut i = 0u32;
            while t < end {
                let oct = if i % 8 == 7 { 2.0 } else { 1.0 };
                add_bass(&mut out, sr, t, root_hz * oct, eighth, 0.5 * level * fade(t - start) * trim);
                t += eighth;
                i += 1;
            }
            let mut t = start + eighth;
            let mut i = 0u32;
            while t < end {
                if i % 2 == 0 {
                    add_stab(&mut out, sr, t, &chord, 0.28 * level * fade(t - start) * trim);
                }
                t += eighth;
                i += 1;
            }
        }
        if !matches!(s.kind, "drop" | "groove") || silent_bar {
            // Pad: dark in intro, RAMPING OPEN through the build (the
            // brightness ramp a build IS), mid in break, falling in outro.
            let (cutoff_start, cutoff_end, amp) = match s.kind {
                "intro" => (500.0, 500.0, 0.3 * level),
                "build" => (600.0, 4_000.0, 0.34 * level),
                "break" => (1_400.0, 1_400.0, 0.35),
                "outro" => (900.0, 900.0, 0.4 * level),
                _ => (1_200.0, 1_200.0, 0.3),
            };
            add_pad(&mut out, sr, start, dur, &chord, cutoff_start, cutoff_end, amp * trim, silent_bar);
        }
    }

    // --- riser (planted filter-sweep treatment): last two bars --------
    for &(start, dur, s) in &section_spans {
        if s.exit == Exit::Riser {
            add_riser(&mut out, sr, start + dur - 2.0 * bar, 2.0 * bar, trim, &mut white);
        }
    }

    // Tiny seeded wobble so tracks differ beyond their plants while
    // staying far from any detection threshold.
    for x in out.iter_mut() {
        *x = (*x as f64 * (1.0 + 0.02 * f64::from(rng.next_f32() - 0.5))) as f32;
    }
    out
}

fn midi_hz(midi: f64) -> f64 {
    440.0 * f64::exp2((midi - 69.0) / 12.0)
}

fn add_kick(out: &mut [f32], sr: f64, at: f64, amp: f64) {
    let start = (at * sr) as usize;
    let len = (0.35 * sr) as usize;
    let mut phase = 0.0f64;
    for i in 0..len.min(out.len().saturating_sub(start)) {
        let t = f64::from(i as u32) / sr;
        // Pitch envelope 90→42 Hz with a fast decay: the sweep stays
        // below the chord register, so it cannot pollute pitch-class
        // statistics the way a wide 170 Hz sweep would.
        let f = 42.0 + 48.0 * f64::exp(-t * 200.0);
        phase += 2.0 * std::f64::consts::PI * f / sr;
        out[start + i] += (amp * f64::exp(-t * 9.0) * phase.sin()) as f32;
    }
}

fn add_hat(out: &mut [f32], sr: f64, at: f64, amp: f64, white: &mut impl FnMut() -> f32) {
    let start = (at * sr) as usize;
    let len = (0.05 * sr) as usize;
    let mut hp = Biquad::identity();
    hp.set_coeffs(highpass_coeffs(sr, 5_000.0, std::f64::consts::FRAC_1_SQRT_2));
    let mut lp = Biquad::identity();
    lp.set_coeffs(lowpass_coeffs(sr, 11_000.0, std::f64::consts::FRAC_1_SQRT_2));
    for i in 0..len.min(out.len().saturating_sub(start)) {
        let t = f64::from(i as u32) / sr;
        let x = hp.tick(white()) as f64;
        let x = lp.tick(x as f32) as f64;
        out[start + i] += (amp * f64::exp(-t * 60.0) * x) as f32;
    }
}

fn add_clap(out: &mut [f32], sr: f64, at: f64, amp: f64, white: &mut impl FnMut() -> f32) {
    let start = (at * sr) as usize;
    let len = (0.12 * sr) as usize;
    let mut hp = Biquad::identity();
    hp.set_coeffs(highpass_coeffs(sr, 1_000.0, std::f64::consts::FRAC_1_SQRT_2));
    let mut lp = Biquad::identity();
    lp.set_coeffs(lowpass_coeffs(sr, 3_000.0, std::f64::consts::FRAC_1_SQRT_2));
    for i in 0..len.min(out.len().saturating_sub(start)) {
        let t = f64::from(i as u32) / sr;
        let x = hp.tick(white()) as f64;
        let x = lp.tick(x as f32) as f64;
        out[start + i] += (amp * f64::exp(-t * 28.0) * x) as f32;
    }
}

fn add_snare(out: &mut [f32], sr: f64, at: f64, amp: f64, white: &mut impl FnMut() -> f32) {
    let start = (at * sr) as usize;
    let len = (0.08 * sr) as usize;
    let mut hp = Biquad::identity();
    hp.set_coeffs(highpass_coeffs(sr, 1_400.0, std::f64::consts::FRAC_1_SQRT_2));
    for i in 0..len.min(out.len().saturating_sub(start)) {
        let t = f64::from(i as u32) / sr;
        let x = hp.tick(white()) as f64;
        out[start + i] += (amp * f64::exp(-t * 40.0) * x) as f32;
    }
}

fn add_bass(out: &mut [f32], sr: f64, at: f64, hz: f64, dur: f64, amp: f64) {
    let start = (at * sr) as usize;
    let len = (dur * sr) as usize;
    let mut lp = Biquad::identity();
    lp.set_coeffs(lowpass_coeffs(sr, 320.0, std::f64::consts::FRAC_1_SQRT_2));
    for i in 0..len.min(out.len().saturating_sub(start)) {
        let t = f64::from(i as u32) / sr;
        let ph = t * hz;
        let saw = 2.0 * (ph - (ph + 0.5).floor());
        let env = (t * 30.0).min(1.0) * f64::exp(-t * 3.0).max(0.25);
        out[start + i] += (amp * env * lp.tick((0.8 * saw) as f32) as f64) as f32;
    }
}

fn add_stab(out: &mut [f32], sr: f64, at: f64, chord: &[f64], amp: f64) {
    let start = (at * sr) as usize;
    let len = (0.25 * sr) as usize;
    let mut lp = Biquad::identity();
    lp.set_coeffs(lowpass_coeffs(sr, 2_600.0, std::f64::consts::FRAC_1_SQRT_2));
    for i in 0..len.min(out.len().saturating_sub(start)) {
        let t = f64::from(i as u32) / sr;
        let mut s = 0.0f64;
        for (v, &hz) in chord.iter().enumerate() {
            let det = 1.0 + 0.004 * f64::from(v as i32 - 1);
            let ph = t * hz * det;
            s += 2.0 * (ph - (ph + 0.5).floor()) - 1.0;
        }
        let env = f64::exp(-t * 9.0);
        out[start + i] += (amp * env * lp.tick((0.3 * s) as f32) as f64) as f32;
    }
}

fn add_pad(
    out: &mut [f32],
    sr: f64,
    at: f64,
    dur: f64,
    chord: &[f64],
    cutoff_start: f64,
    cutoff_end: f64,
    amp: f64,
    silent_bar: bool,
) {
    let start = (at * sr) as usize;
    let len = (dur * sr) as usize;
    let mut lp = Biquad::identity();
    let mut recompute_in = 0usize;
    for i in 0..len.min(out.len().saturating_sub(start)) {
        if recompute_in == 0 {
            let progress = f64::from(i as u32) / sr / dur;
            let cutoff = cutoff_start + (cutoff_end - cutoff_start) * progress;
            lp.set_coeffs(lowpass_coeffs(sr, cutoff, std::f64::consts::FRAC_1_SQRT_2));
            recompute_in = 64;
        }
        recompute_in -= 1;
        let t = f64::from(i as u32) / sr;
        // The planted silence bar: one bar at -50 dB, then the pad.
        let gate = if silent_bar && t < dur * 0.25 { 0.003 } else { 1.0 };
        let mut s = 0.0f64;
        for &hz in chord {
            let ph = t * hz;
            s += (2.0 * std::f64::consts::PI * ph).sin();
        }
        let env = (t * 0.8).min(1.0) * ((dur - t) * 0.8).clamp(0.0, 1.0);
        out[start + i] += (amp * gate * env * lp.tick((0.25 * s) as f32) as f64) as f32;
    }
}

fn add_riser(out: &mut [f32], sr: f64, at: f64, dur: f64, trim: f64, white: &mut impl FnMut() -> f32) {
    let start = (at.max(0.0) * sr) as usize;
    let len = (dur * sr) as usize;
    let mut lp = Biquad::identity();
    let mut recompute_in = 0usize;
    for i in 0..len.min(out.len().saturating_sub(start)) {
        if recompute_in == 0 {
            let progress = f64::from(i as u32) / sr / dur;
            lp.set_coeffs(
                lowpass_coeffs(sr, 400.0 * f64::exp2(progress * 4.3), std::f64::consts::FRAC_1_SQRT_2),
            );
            recompute_in = 64;
        }
        recompute_in -= 1;
        let t = f64::from(i as u32) / sr;
        // Real risers peak near the full mix — quiet ones vanish under
        // the hats and no spectral detector can see them.
        let amp = 0.6 * (t / dur) * trim;
        out[start + i] += (amp * lp.tick(white()) as f64) as f32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_have_unique_ids_and_positive_length() {
        for p in PRESETS {
            assert!(preset_by_id(p.id).is_some());
            let bars: u32 = p.sections.iter().map(|s| s.bars).sum();
            assert!(bars >= 32, "{}: fixture too short to segment", p.id);
        }
        assert_eq!(PRESETS.len(), 12);
    }

    #[test]
    fn render_is_deterministic_and_non_silent() {
        let p = preset_by_id("mt-a").unwrap();
        let a = render(p);
        let b = render(p);
        assert_eq!(a, b);
        let rms = (a.iter().map(|x| x * x).sum::<f32>() / a.len() as f32).sqrt();
        assert!(rms > 0.05, "fixture must have real signal, rms={rms}");
        assert_eq!(
            a.len(),
            (256.0 * (60.0 / 128.0) * f64::from(SYNTH_SAMPLE_RATE)) as usize
        );
    }

    #[test]
    fn planted_truth_is_consistent() {
        for p in PRESETS {
            let ann = planted_annotation(p);
            let types = planted_transition_types(p);
            assert_eq!(types.len(), ann.sections.len() - 1, "{}", p.id);
            // Every fixture exercises all four boundary treatments.
            for t in ["silence", "filter_sweep", "fill", "hard_cut"] {
                assert!(types.contains(&t), "{}: missing {t} plant", p.id);
            }
        }
    }
}
