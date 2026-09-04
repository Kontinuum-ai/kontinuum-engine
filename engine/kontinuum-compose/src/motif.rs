//! Motif memory (issue #16): per-track pattern motifs stored with ids at
//! first introduction, and the transforms `reintro`/late sections request
//! when material "returns changed" — transpose, density-thin, re-voice,
//! half-time. This is the composed-vs-generated tell: a reintroduction is
//! never the section's first draw, it is remembered material reshaped.

use std::collections::BTreeMap;

use kontinuum_clock::Rng;
use kontinuum_ir::schema::{Pattern, Step, StepsPattern};

/// One remembered figure: the pattern a track first played, tagged with a
/// stable id (`motif_<track>_<n>`) and the section that introduced it.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredMotif {
    pub id: String,
    pub track: String,
    pub pattern: Pattern,
    pub intro_section: String,
}

/// Per-track motif store for one session.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MotifMemory {
    by_track: BTreeMap<String, Vec<StoredMotif>>,
}

impl MotifMemory {
    pub fn new() -> MotifMemory {
        MotifMemory::default()
    }

    /// Remembers a track's pattern as a motif the first time it appears;
    /// later introductions of the same track keep the original figure.
    /// Returns the motif id when a new motif was stored.
    pub fn observe(&mut self, track: &str, pattern: &Pattern, section: &str) -> Option<String> {
        let seen = self.by_track.entry(track.to_string()).or_default();
        if !seen.is_empty() {
            return None;
        }
        let id = format!("motif_{}_{}", track, seen.len());
        seen.push(StoredMotif {
            id: id.clone(),
            track: track.to_string(),
            pattern: pattern.clone(),
            intro_section: section.to_string(),
        });
        Some(id)
    }

    pub fn is_empty(&self) -> bool {
        self.by_track.values().all(Vec::is_empty)
    }

    /// The motif stored for `track`, if introduction already happened.
    pub fn motif_for(&self, track: &str) -> Option<&StoredMotif> {
        self.by_track.get(track)?.first()
    }
}

/// The reintro transform a section requests (#16): material returns
/// changed, never identical.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MotifTransform {
    /// Shift pitched patterns by the given semitones.
    Transpose(i32),
    /// Drop the weakest (quietest) steps' probability mass.
    DensityThin,
    /// Re-pitch pitched steps into a new register (fifth up, octave down).
    ReVoice,
    /// Keep the even 8ths only, double the gates.
    HalfTime,
}

/// Picks the seeded transform a late section requests. Deterministic per
/// (section, rng state). Unpitched figures (percussion) skip the pitch
/// transforms — a transpose on an unpitched hit is a no-op, and material
/// must return CHANGED.
pub fn request_transform(rng: &mut Rng) -> MotifTransform {
    match rng.below(4) {
        0 => MotifTransform::Transpose(if rng.chance(0.5) { -2 } else { 3 }),
        1 => MotifTransform::DensityThin,
        2 => MotifTransform::ReVoice,
        _ => MotifTransform::HalfTime,
    }
}

/// [`request_transform`], restricted to transforms that actually change
/// `pattern`.
pub fn request_transform_for(pattern: &Pattern, rng: &mut Rng) -> MotifTransform {
    let pitched = match pattern {
        Pattern::Steps(sp) => sp.steps.iter().any(|st| st.pitch.is_some()),
        Pattern::Euclidean(ep) => ep.pitch.is_some(),
        Pattern::ProbabilityMask(mp) => mp.pitch.is_some(),
    };
    if pitched {
        return request_transform(rng);
    }
    match rng.below(2) {
        0 => MotifTransform::DensityThin,
        _ => MotifTransform::HalfTime,
    }
}

/// Draws transforms until one actually changes the figure (a thin on an
/// all-loud bar is an identity — material must return CHANGED), falling
/// back to half-time which always reshapes the grid.
pub fn request_and_apply(pattern: &Pattern, rng: &mut Rng) -> Pattern {
    for _ in 0..4 {
        let t = request_transform_for(pattern, rng);
        let changed = transform(pattern, t);
        if changed != *pattern {
            return changed;
        }
    }
    transform(pattern, MotifTransform::HalfTime)
}

/// Applies a transform to a pattern, returning the changed figure.
/// Unpitched percussion ignores transpose/re-voice (no pitch to move) but
/// thinning and half-time apply to everything.
pub fn transform(pattern: &Pattern, t: MotifTransform) -> Pattern {
    match pattern {
        Pattern::Steps(sp) => {
            let mut steps = sp.steps.clone();
            match t {
                MotifTransform::Transpose(semitones) => transpose_steps(&mut steps, semitones),
                MotifTransform::DensityThin => thin_steps(&mut steps),
                MotifTransform::ReVoice => {
                    transpose_steps(&mut steps, 7);
                    for st in steps.iter_mut().filter(|s| s.pitch.is_some()) {
                        st.pitch = st.pitch.map(|p| (p - 12.0).max(24.0));
                    }
                }
                MotifTransform::HalfTime => half_time(&mut steps),
            }
            Pattern::Steps(StepsPattern { steps, repeats: sp.repeats })
        }
        Pattern::Euclidean(ep) => {
            let mut ep = ep.clone();
            match t {
                MotifTransform::Transpose(semitones) => shift_pitch(&mut ep.pitch, semitones),
                MotifTransform::DensityThin => ep.k = (ep.k / 2).max(1),
                MotifTransform::ReVoice => {
                    shift_pitch(&mut ep.pitch, 7);
                    ep.pitch = ep.pitch.map(|p| (p - 12.0).max(24.0));
                }
                MotifTransform::HalfTime => ep.k = (ep.k / 2).max(1),
            }
            Pattern::Euclidean(ep)
        }
        Pattern::ProbabilityMask(mp) => {
            let mut mp = mp.clone();
            if let MotifTransform::Transpose(n) = t {
                shift_pitch(&mut mp.pitch, n);
            }
            if matches!(t, MotifTransform::ReVoice) {
                shift_pitch(&mut mp.pitch, 7);
                mp.pitch = mp.pitch.map(|p| (p - 12.0).max(24.0));
            }
            Pattern::ProbabilityMask(mp)
        }
    }
}

fn transpose_steps(steps: &mut [Step], semitones: i32) {
    for st in steps.iter_mut().filter(|s| s.pitch.is_some()) {
        st.pitch = st.pitch.map(|p| p + semitones as f32);
    }
}

fn shift_pitch(pitch: &mut Option<f32>, semitones: i32) {
    *pitch = pitch.map(|p| p + semitones as f32);
}

/// Density-thin: the quiet tier retreats (velocity, not probability —
/// the per-bar ghost count is #17's budget, not ours), so the figure
/// stays recognizable but breathes.
fn thin_steps(steps: &mut [Step]) {
    let Some(ceiling) = steps.iter().map(|s| s.velocity).fold(None::<f32>, |m, v| {
        Some(m.map_or(v, |m: f32| m.max(v)))
    }) else {
        return;
    };
    for st in steps.iter_mut() {
        if st.probability >= 1.0 && st.velocity < ceiling * 0.75 {
            st.velocity = (st.velocity * 0.75).clamp(0.02, 0.33);
        }
    }
}

/// Half-time: keep the quarter-note skeleton (snapped — swung positions
/// never sit exactly on the grid) and ring the survivors.
fn half_time(steps: &mut Vec<Step>) {
    const EIGHTH: u32 = 240;
    let on_quarter = |pos: u32| ((pos + EIGHTH / 2) / EIGHTH) % 2 == 0;
    let fallback = steps.iter().max_by(|a, b| a.velocity.total_cmp(&b.velocity)).cloned();
    steps.retain(|st| on_quarter(st.position));
    if steps.is_empty() {
        // A fully swung figure has no on-grid survivor; keep its loudest
        // hit (re-snapped) so the transformed motif never returns empty.
        if let Some(mut strongest) = fallback {
            strongest.position = (strongest.position / EIGHTH) * EIGHTH;
            steps.push(strongest);
        }
    }
    for st in steps.iter_mut() {
        st.gate = Some((st.gate.unwrap_or(0.5) * 2.0).min(4.0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(pitches: &[(u32, f32, f32)]) -> Pattern {
        Pattern::Steps(StepsPattern {
            steps: pitches
                .iter()
                .map(|(pos, pitch, vel)| Step {
                    position: *pos,
                    velocity: *vel,
                    probability: 1.0,
                    microtiming_ticks: 0,
                    ratchet: 1,
                    accent: false,
                    gate: Some(0.25),
                    pitch: Some(*pitch),
                })
                .collect(),
            repeats: 1,
        })
    }

    #[test]
    fn first_observe_stores_and_second_is_kept() {
        let mut m = MotifMemory::new();
        let p = pattern(&[(0, 36.0, 0.8)]);
        assert!(m.observe("bass", &p, "dev_0").is_some());
        assert!(m.observe("bass", &pattern(&[(0, 38.0, 0.8)]), "dev_1").is_none());
        assert_eq!(m.motif_for("bass").expect("stored").pattern, p);
    }

    #[test]
    fn transpose_moves_pitch_density_thin_quiets_the_tail_half_time_thins_the_grid() {
        let p = pattern(&[(0, 36.0, 0.8), (240, 43.0, 0.8), (360, 48.0, 0.4)]);
        let Pattern::Steps(t) = transform(&p, MotifTransform::Transpose(3)) else {
            panic!("steps");
        };
        assert_eq!(t.steps[0].pitch, Some(39.0));

        let Pattern::Steps(t) = transform(&p, MotifTransform::DensityThin) else {
            panic!("steps");
        };
        assert!(t.steps[2].velocity < 0.4, "quiet tail thinned");
        assert_eq!(t.steps[2].probability, 1.0, "thinning never mints ghosts");
        assert_eq!(t.steps[0].velocity, 0.8, "the figure's head keeps its weight");

        let Pattern::Steps(t) = transform(&p, MotifTransform::HalfTime) else {
            panic!("steps");
        };
        assert_eq!(t.steps.len(), 2, "the quarter skeleton survives the 8th-grid cut");
        assert_eq!(t.steps[0].gate, Some(0.5));
    }

    #[test]
    fn revoice_opens_a_fifth_and_drops_an_octave() {
        let p = pattern(&[(0, 48.0, 0.8)]);
        let Pattern::Steps(t) = transform(&p, MotifTransform::ReVoice) else {
            panic!("steps");
        };
        assert_eq!(t.steps[0].pitch, Some(43.0));
    }
}
