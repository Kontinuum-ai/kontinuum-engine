//! Harmony & melody vocabulary (issue #46): scales, progression templates,
//! chord voicings. Pure Rust, zero dependencies, deterministic from seed.
//! The generator becomes chord-aware: bass follows roots, pads voice triads
//! and 7ths, stabs land on chord tones.

use kontinuum_clock::stream;
use kontinuum_ir::schema::{Pattern, Section};
use serde::{Deserialize, Serialize};

/// A chord: absolute MIDI root + quality.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Chord {
    /// Root as absolute MIDI note (octave 1-2 for bass material).
    pub root: u8,
    pub minor: bool,
}

impl Chord {
    /// Chord tones: minor [0,3,7,(10)] or major [0,4,7,(11)].
    pub fn tones(&self, seventh: bool) -> Vec<u8> {
        let third = if self.minor { 3 } else { 4 };
        let seventh_iv = if self.minor { 10 } else { 11 };
        if seventh {
            vec![0, third, 7, seventh_iv]
        } else {
            vec![0, third, 7]
        }
    }

    /// Voicing an octave-plus above the root (pad register).
    pub fn voicing(&self, seventh: bool) -> Vec<f32> {
        self.voicing_ext(if seventh { Extension::Seventh } else { Extension::Triad })
    }

    /// Voicing with an explicit color extension (#46, extended by #17):
    /// sus2 replaces the third with a major second, quartal stacks
    /// fourths — the deeper, less committed colors.
    pub fn voicing_ext(&self, ext: Extension) -> Vec<f32> {
        let base = f32::from(self.root + 24);
        let intervals: Vec<u8> = match ext {
            Extension::Triad => self.tones(false),
            Extension::Seventh => self.tones(true),
            Extension::Ninth => {
                let mut v = self.tones(true);
                v.push(14);
                v
            }
            Extension::Sus2 => vec![0, 2, 7],
            Extension::Quartal => vec![0, 5, 10],
        };
        intervals.iter().map(|&iv| base + f32::from(iv)).collect()
    }
}

/// Chord color: plain triad, seventh, ninth, sus2, or quartal (#46, #17).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Extension {
    Triad,
    Seventh,
    Ninth,
    Sus2,
    Quartal,
}

/// Natural-minor pitch classes rooted at C (58): the session key vocabulary.
/// Progressions and voicings must stay diatonic to this set.
pub const NATURAL_MINOR: [u8; 7] = [0, 2, 3, 5, 7, 8, 10];

/// Pitch classes of the natural minor scale at `root_pc` (0..12).
pub fn minor_scale(root_pc: u8) -> [u8; 7] {
    NATURAL_MINOR.map(|iv| (root_pc + iv) % 12)
}

/// Four-chord minor progressions, relative to a tonic (i - VI - III - VII
/// family), hand-picked to be musically valid; the seed picks and shuffles.
/// Roots land in the bass octave (tonic at MIDI `24 + tonic_pc`).
///
/// `darkness` (0..1) biases the draw toward the templates carrying more minor
/// chords. It is not a hard filter: every template stays reachable at every
/// setting, the odds just move. The tonic is the genre's key tendency
/// (issue #87): F minor for minimal techno, D minor for deep house, and so on.
pub fn progression(seed: u64, darkness: f32, tonic_pc: u8) -> Vec<Chord> {
    let mut rng = stream(seed, 0x4C, 0x01);
    let root = |off: u8| 24 + (tonic_pc + off) % 12;
    let templates: Vec<Vec<(u8, bool)>> = vec![
        // i - VI - III - VII
        vec![(0, true), (8, false), (3, false), (10, false)],
        // i - VII - VI - VII
        vec![(0, true), (10, false), (8, false), (10, false)],
        // i - III - iv - VI (two minor chords: the darkest of the three)
        vec![(0, true), (3, false), (5, true), (8, false)],
    ];
    // Weight by minor content, raised to a power that darkness controls.
    let weights: Vec<f64> = templates
        .iter()
        .map(|t| {
            let minor_share = t.iter().filter(|(_, mi)| *mi).count() as f64 / t.len().max(1) as f64;
            let bias = 1.0 + 3.0 * f64::from(darkness.clamp(0.0, 1.0));
            (0.25 + minor_share).powf(bias)
        })
        .collect();
    let total: f64 = weights.iter().sum();
    let mut draw = rng.next_f32() as f64 * total;
    let mut pick = templates.len() - 1;
    for (i, w) in weights.iter().enumerate() {
        if draw < *w {
            pick = i;
            break;
        }
        draw -= w;
    }
    let mut t: Vec<Chord> = templates[pick]
        .iter()
        .map(|&(off, mi)| Chord { root: root(off), minor: mi })
        .collect();
    // Seeded starting rotation (progressions are loops).
    let rot = rng.below(4) as usize;
    t.rotate_left(rot);
    t
}

/// Voicing engine (issue #17): picks the inversion of `voicing` whose
/// movement from `previous` is smallest, octave-shifted so the chord stays
/// near the previous register (minimal voice-leading / inversion
/// continuity). `None` previous keeps the voicing as built.
pub fn voice_lead(voicing: &[f32], previous: Option<&[f32]>) -> Vec<f32> {
    let Some(prev) = previous else { return voicing.to_vec() };
    if voicing.is_empty() {
        return Vec::new();
    }
    let prev_mean = prev.iter().sum::<f32>() / prev.len() as f32;
    (0..voicing.len())
        .map(|r| {
            let rotated: Vec<f32> =
                voicing[r..].iter().chain(voicing[..r].iter()).copied().collect();
            let mean = rotated.iter().sum::<f32>() / rotated.len() as f32;
            let shift = ((prev_mean - mean) / 12.0).round() * 12.0;
            rotated.iter().map(|p| (*p + shift).clamp(36.0, 84.0)).collect::<Vec<f32>>()
        })
        .min_by(|a, b| {
            let cost = |cand: &[f32]| -> f32 {
                cand.iter().zip(prev).map(|(x, p)| (x - p).abs()).sum()
            };
            cost(a).total_cmp(&cost(b))
        })
        .unwrap_or_else(|| voicing.to_vec())
}

/// Snap a section's bass and pad material onto its chord (chord-following).
///
/// `melody` holds the ids of single-note voices (bass, acid): their material
/// is *transposed*, not snapped per step — the builder writes a motif as
/// intervals above a placeholder root, and the whole figure moves together so
/// the shape survives the chord change. Rewriting each step's pitch class
/// individually (and injecting a fifth on every third step, as an earlier
/// revision did) flattens any motif back into a root pulse with noise on top.
/// `poly` holds the chord voices (pad, ep, stab, pluck): each step takes its
/// tone from the section's chord voicing.
///
/// `color` is the 3-voice chord color (issue #17's sus2/quartal vocabulary —
/// the genre spec carries the style's tint); 4-voice stays a seventh and
/// 5+ the ninth colour (#46). Returns the voicing actually used, so the
/// caller can thread inversion continuity across sections.
pub fn retune_section(
    section: &mut Section,
    chord: &Chord,
    melody: &[&str],
    poly: &[&str],
    color: Extension,
    previous: Option<&[f32]>,
) -> Vec<f32> {
    let mut used_voicing: Vec<f32> = Vec::new();
    for id in melody {
        if let Some(Pattern::Steps(st)) = section.pattern_bindings.get_mut(*id) {
            if let Some(first) = st.steps.first().and_then(|s| s.pitch) {
                let root_pc = i64::from(chord.root).rem_euclid(12);
                let first_pc = (first.round() as i64).rem_euclid(12);
                // Nearest transposition in (-6..=5) keeps the line in register
                // instead of walking it up an octave over a progression.
                let mut shift = (root_pc - first_pc).rem_euclid(12);
                if shift > 5 {
                    shift -= 12;
                }
                for step in st.steps.iter_mut() {
                    if let Some(p) = step.pitch.as_mut() {
                        *p += shift as f32;
                    }
                }
            }
        }
    }
    for id in poly {
        if let Some(Pattern::Steps(st)) = section.pattern_bindings.get_mut(*id) {
            // The step count is the voicing request: three voices take the
            // style's colour, four a seventh, five the ninth colour (#46).
            let ext = match st.steps.len() {
                0..=3 => color,
                4 => Extension::Seventh,
                _ => Extension::Ninth,
            };
            let voicing = voice_lead(&chord.voicing_ext(ext), previous);
            for (i, step) in st.steps.iter_mut().enumerate() {
                if let Some(p) = step.pitch.as_mut() {
                    *p = voicing[i % voicing.len()];
                }
            }
            used_voicing = voicing;
        }
    }
    used_voicing
}

/// Assign progression chords cyclically over the session's sections. The
/// tonic (pitch class 0..12) is the genre's key tendency; `melody`/`poly`
/// are the session's melody and chord-voice track ids (see
/// [`retune_section`]); `color` is the style's 3-voice chord tint (issue
/// #17). Voicings carry inversion continuity across section boundaries.
pub fn apply_progression(
    sections: &mut [Section],
    seed: u64,
    darkness: f32,
    tonic_pc: u8,
    melody: &[&str],
    poly: &[&str],
    color: Extension,
) {
    let prog = progression(seed, darkness, tonic_pc);
    let mut previous: Option<Vec<f32>> = None;
    for (i, section) in sections.iter_mut().enumerate() {
        let chord = &prog[i % prog.len()];
        let used = retune_section(section, chord, melody, poly, color, previous.as_deref());
        if !used.is_empty() {
            previous = Some(used);
        }
    }
}

/// Same assignment over an explicit progression table — the Creative Soul
/// harmony layer (issue #55) supplies the vocabulary; the seed still picks
/// the table and the starting rotation so a table never hard-pins a take.
pub fn apply_progression_with(
    sections: &mut [Section],
    seed: u64,
    templates: &[Vec<Chord>],
    melody: &[&str],
    poly: &[&str],
    color: Extension,
) {
    if templates.is_empty() {
        return;
    }
    let mut rng = stream(seed, 0x4C, 0x01);
    let mut prog = templates[rng.below(templates.len() as u64) as usize].clone();
    let rot = rng.below(prog.len().max(1) as u64) as usize;
    prog.rotate_left(rot);
    let mut previous: Option<Vec<f32>> = None;
    for (i, section) in sections.iter_mut().enumerate() {
        let chord = &prog[i % prog.len()];
        let used = retune_section(section, chord, melody, poly, color, previous.as_deref());
        if !used.is_empty() {
            previous = Some(used);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const F: u8 = 5;

    #[test]
    fn progression_is_deterministic_and_valid() {
        let a = progression(7, 0.7, F);
        let b = progression(7, 0.7, F);
        assert_eq!(a, b);
        assert_eq!(a.len(), 4);
        assert!(a.iter().any(|c| c.minor) && a.iter().any(|c| !c.minor),
                "progression mixes minor and major chords");
    }

    #[test]
    fn voicing_stays_in_register() {
        let c = Chord { root: 29, minor: true };
        let v = c.voicing(true);
        assert_eq!(v.len(), 4);
        assert!(v.iter().all(|p| (52.0..=80.0).contains(p)), "pad register: {:?}", v);
    }

    #[test]
    fn ninth_extends_the_voicing_by_one_diatonic_tone() {
        let c = Chord { root: 29, minor: true };
        let seventh = c.voicing_ext(Extension::Seventh);
        let ninth = c.voicing_ext(Extension::Ninth);
        assert_eq!(ninth.len(), seventh.len() + 1);
        assert_eq!(ninth[..seventh.len()], seventh[..], "ninth appends to the seventh voicing");
        assert_eq!(ninth[ninth.len() - 1], f32::from(c.root + 24 + 14));
        // Triad path unchanged.
        assert_eq!(c.voicing_ext(Extension::Triad).len(), 3);
    }

    #[test]
    fn sus2_and_quartal_are_the_issue_17_colors() {
        let c = Chord { root: 29, minor: true };
        // Sus2 replaces the third with a major second.
        let sus2 = c.voicing_ext(Extension::Sus2);
        assert_eq!(sus2.len(), 3);
        assert_eq!(sus2[1] - sus2[0], 2.0, "the second sits in the third's place");
        assert_eq!(sus2[2] - sus2[0], 7.0, "the fifth stays");
        // Quartal stacks fourths: root, fourth, flat seventh.
        let quartal = c.voicing_ext(Extension::Quartal);
        assert_eq!(quartal.len(), 3);
        assert_eq!(quartal[1] - quartal[0], 5.0);
        assert_eq!(quartal[2] - quartal[1], 5.0);
        // Both stay in the pad register.
        for p in sus2.iter().chain(&quartal) {
            assert!((52.0..=80.0).contains(p));
        }
    }

    #[test]
    fn voice_lead_picks_the_nearest_inversion() {
        let c = Chord { root: 24, minor: true };
        let prev = c.voicing_ext(Extension::Triad);
        // A chord a fifth up, voiced from root: the nearest inversion must
        // move less than the root position would.
        let next = Chord { root: 31, minor: false };
        let root_position = next.voicing_ext(Extension::Triad);
        let led = voice_lead(&root_position, Some(&prev));
        let cost = |v: &[f32]| v.iter().zip(&prev).map(|(x, p)| (x - p).abs()).sum::<f32>();
        assert!(cost(&led) <= cost(&root_position) + 1e-4, "never worse than root position");
        for p in &led {
            assert!((36.0..=84.0).contains(p), "register clamp: {p}");
        }
        assert_eq!(voice_lead(&root_position, None), root_position, "no previous, no move");
        assert!(voice_lead(&[], Some(&prev)).is_empty());
    }

    #[test]
    fn tonic_transposes_the_whole_progression() {
        // Same seed, different tonic: identical shape, transposed roots.
        let f_minor = progression(7, 0.7, F);
        let g_minor = progression(7, 0.7, 7);
        assert_eq!(f_minor.len(), g_minor.len());
        for (f, g) in f_minor.iter().zip(&g_minor) {
            assert_eq!(f.minor, g.minor);
            let step = (i16::from(g.root) - i16::from(f.root)).rem_euclid(12);
            assert_eq!(step, 2, "G is a whole step above F");
        }
    }

    #[test]
    fn minor_scale_is_diatonic() {
        let f_minor = minor_scale(F); // F = pc 5
        assert!(f_minor.contains(&5) && f_minor.contains(&8) && f_minor.contains(&0));
        // Every progression chord root sits in the tonic's minor scale — for
        // every shipped tonic, not just F.
        for tonic in 0..12u8 {
            for chord in progression(7, 0.7, tonic) {
                assert!(
                    minor_scale(tonic).contains(&(chord.root % 12)),
                    "chord root {} not diatonic to tonic {tonic}",
                    chord.root
                );
            }
        }
    }
}
