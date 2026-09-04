//! State fingerprint — the credit-assignment key (#24): a compact, hashable
//! description of what was musically true when a signal fired.
//!
//! ## Granularity study scaffolding
//!
//! The granularity dial trades generalization against resolution:
//!
//! - **Too fine** → nearly every fingerprint is unique, so learned scores
//!   never transfer across sessions (no generalization; the learner memorizes).
//! - **Too coarse** → everything shares a key and preferences mush into an
//!   average that fits nothing.
//!
//! Three selectable levels, ordered by how many dimensions they key on:
//!
//! | Level    | Keyed dimensions                                                    | Bins |
//! |----------|---------------------------------------------------------------------|------|
//! | `Coarse` | section kind, energy, tempo                                         | 4    |
//! | `Mid`    | + density, brightness, palette id, groove template                  | 8    |
//! | `Fine`   | + bass archetype, dominant sample classes                           | 16   |
//!
//! Continuous buckets are *nested* (4 ⊂ 8 ⊂ 16 uniform bins), so coarsening is
//! integer division: a fine fingerprint coarsens exactly to what the raw state
//! would have produced at the coarse level. The granularity study (#24) picks
//! the winning level via offline replay; nothing else in the crate assumes a
//! particular choice.

use crate::signal::Signal;
use serde::{Deserialize, Serialize};

/// Fingerprint granularity (see module docs for the trade-off rationale).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Granularity {
    Coarse,
    Mid,
    Fine,
}

impl Granularity {
    /// Number of uniform bins for continuous dimensions at this level.
    pub fn buckets(self) -> u8 {
        match self {
            Granularity::Coarse => 4,
            Granularity::Mid => 8,
            Granularity::Fine => 16,
        }
    }

    /// Does this level key on everything `level` keys on?
    fn includes(self, level: Granularity) -> bool {
        self >= level
    }
}

/// Arc role of the section the listener was in. IR sections are free-form ids
/// (opaque to the engine); the caller maps id → role, this crate stays
/// self-contained.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionKind {
    Intro,
    Build,
    Peak,
    Break,
    Outro,
    /// Steady main body of the session.
    Body,
}

/// The raw musical state at time t, before bucketing. This is the boundary
/// input; fingerprints are derived from it deterministically.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MusicalState {
    pub section_kind: SectionKind,
    /// 0..=1
    pub energy: f32,
    /// 0..=1
    pub density: f32,
    /// 0..=1 (minor/dark ↔ bright tonal material)
    pub brightness: f32,
    pub bpm: f32,
    /// Engine palette id (opaque; id → name lives in the transparency layer).
    pub palette_id: u32,
    /// Groove template index.
    pub groove_template: u16,
    /// Bass archetype index.
    pub bass_archetype: u16,
    /// Top dominant sample classes, 0 = unused slot.
    pub dominant_sample_classes: [u8; 4],
}

/// Compact, hashable fingerprint at a chosen granularity. Fields *not* keyed
/// at the fingerprint's granularity are `None`; equality therefore treats
/// unkeyed dimensions as wildcards. Bin values are indices into the uniform
/// grid for `granularity`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StateFingerprint {
    pub granularity: Granularity,
    pub section_kind: Option<SectionKind>,
    pub energy_bucket: Option<u8>,
    pub density_bucket: Option<u8>,
    pub brightness_bucket: Option<u8>,
    pub palette_id: Option<u32>,
    pub groove_template: Option<u16>,
    pub bass_archetype: Option<u16>,
    pub tempo_bucket: Option<u8>,
    pub dominant_sample_classes: Option<[u8; 4]>,
}

/// Bucket a value in 0..=1 into `bins` uniform bins.
fn bucket01(v: f32, bins: u8) -> u8 {
    if !v.is_finite() {
        return 0;
    }
    ((v.clamp(0.0, 1.0) * bins as f32).floor() as u8).min(bins - 1)
}

/// Bucket a BPM into `bins` uniform bins over the 60..200 range.
fn bucket_bpm(bpm: f32, bins: u8) -> u8 {
    if !bpm.is_finite() {
        return 0;
    }
    let t = (bpm.clamp(60.0, 200.0) - 60.0) / 140.0;
    ((t * bins as f32).floor() as u8).min(bins - 1)
}

/// Center of bucket `b` (in `bins` uniform bins) as a 0..=1 value.
pub fn bucket_center(b: u8, bins: u8) -> f32 {
    (b as f32 + 0.5) / bins as f32
}

impl MusicalState {
    /// Derive the fingerprint at `g`.
    pub fn fingerprint(&self, g: Granularity) -> StateFingerprint {
        let bins = g.buckets();
        StateFingerprint {
            granularity: g,
            section_kind: g.includes(Granularity::Coarse).then_some(self.section_kind),
            energy_bucket: g.includes(Granularity::Coarse).then(|| bucket01(self.energy, bins)),
            tempo_bucket: g.includes(Granularity::Coarse).then(|| bucket_bpm(self.bpm, bins)),
            density_bucket: g.includes(Granularity::Mid).then(|| bucket01(self.density, bins)),
            brightness_bucket: g.includes(Granularity::Mid).then(|| bucket01(self.brightness, bins)),
            palette_id: g.includes(Granularity::Mid).then_some(self.palette_id),
            groove_template: g.includes(Granularity::Mid).then_some(self.groove_template),
            bass_archetype: g.includes(Granularity::Fine).then_some(self.bass_archetype),
            dominant_sample_classes: g
                .includes(Granularity::Fine)
                .then_some(self.dominant_sample_classes),
        }
    }
}

impl StateFingerprint {
    /// Coarsen to `to` (must not be finer than the current granularity; that
    /// is a no-op — information cannot be re-derived). Nesting guarantees the
    /// result equals fingerprinting the raw state at `to`: bucket values
    /// integer-divide, dimensions not keyed at `to` drop to `None`.
    pub fn coarsen(self, to: Granularity) -> Self {
        if to >= self.granularity {
            return self;
        }
        let ratio = self.granularity.buckets() / to.buckets();
        let down = |b: Option<u8>| b.map(|b| b / ratio);
        let keyed_mid = to.includes(Granularity::Mid);
        StateFingerprint {
            granularity: to,
            section_kind: self.section_kind,
            energy_bucket: down(self.energy_bucket),
            tempo_bucket: down(self.tempo_bucket),
            density_bucket: if keyed_mid { down(self.density_bucket) } else { None },
            brightness_bucket: if keyed_mid { down(self.brightness_bucket) } else { None },
            palette_id: if keyed_mid { self.palette_id } else { None },
            groove_template: if keyed_mid { self.groove_template } else { None },
            bass_archetype: None,
            dominant_sample_classes: None,
        }
    }
}

/// Result of attribution: the state at t plus, when known, the state one
/// phrase earlier (skips react late — the listener may be rejecting what
/// started a phrase ago, not what played at t).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Attribution {
    pub current: StateFingerprint,
    pub previous: Option<StateFingerprint>,
}

impl Attribution {
    /// 1–2 fingerprints for learner observation: current first, previous
    /// second (learners apply a half-weight to the previous phrase).
    pub fn fingerprints(&self) -> Vec<StateFingerprint> {
        match self.previous {
            Some(prev) => vec![self.current, prev],
            None => vec![self.current],
        }
    }
}

/// Attribute a signal to the state at t and (when available and distinct) the
/// state at t−1 phrase.
pub fn attribute(signal: &Signal, previous_phrase: Option<StateFingerprint>) -> Attribution {
    let current = signal.state_fingerprint;
    let previous = previous_phrase.filter(|p| *p != current);
    Attribution { current, previous }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::SignalKind;

    fn state(density: f32, palette: u32) -> MusicalState {
        MusicalState {
            section_kind: SectionKind::Build,
            energy: 0.9,
            density,
            brightness: 0.25,
            bpm: 126.0,
            palette_id: palette,
            groove_template: 2,
            bass_archetype: 5,
            dominant_sample_classes: [7, 3, 11, 0],
        }
    }

    #[test]
    fn granularity_selects_keyed_dimensions() {
        let s = state(0.9, 7);
        let coarse = s.fingerprint(Granularity::Coarse);
        assert!(coarse.section_kind.is_some());
        assert!(coarse.energy_bucket.is_some() && coarse.tempo_bucket.is_some());
        assert!(coarse.density_bucket.is_none() && coarse.palette_id.is_none());
        assert!(coarse.dominant_sample_classes.is_none());
        let mid = s.fingerprint(Granularity::Mid);
        assert!(mid.density_bucket.is_some() && mid.palette_id.is_some());
        assert!(mid.bass_archetype.is_none());
        let fine = s.fingerprint(Granularity::Fine);
        assert!(fine.bass_archetype.is_some() && fine.dominant_sample_classes.is_some());
        // SectionKind must stay hashable/exhaustive-friendly for map keys.
        let mut m = std::collections::BTreeMap::new();
        m.insert(SectionKind::Peak, 1);
        assert_eq!(m[&SectionKind::Peak], 1);
    }

    #[test]
    fn coarsening_is_nested_integer_division() {
        let s = state(0.9, 7);
        let fine = s.fingerprint(Granularity::Fine);
        let coarse_via_div = fine.coarsen(Granularity::Coarse);
        let coarse_direct = s.fingerprint(Granularity::Coarse);
        assert_eq!(coarse_via_div, coarse_direct);
        let mid_via_div = fine.coarsen(Granularity::Mid);
        assert_eq!(mid_via_div, s.fingerprint(Granularity::Mid));
        // Coarsening to the same-or-finer level is a no-op.
        assert_eq!(fine.coarsen(Granularity::Fine), fine);
    }

    #[test]
    fn equal_states_hash_equal_at_same_granularity() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let a = state(0.9, 7).fingerprint(Granularity::Mid);
        let b = state(0.9, 7).fingerprint(Granularity::Mid);
        let mut ha = DefaultHasher::new();
        let mut hb = DefaultHasher::new();
        a.hash(&mut ha);
        b.hash(&mut hb);
        assert_eq!(ha.finish(), hb.finish());
        // Different state → different key at fine level.
        assert_ne!(a, state(0.1, 7).fingerprint(Granularity::Mid));
    }

    #[test]
    fn attribution_returns_two_fingerprints_when_previous_differs() {
        let cur = state(0.9, 7).fingerprint(Granularity::Fine);
        let prev = state(0.2, 1).fingerprint(Granularity::Fine);
        let signal = Signal::new(1_000, SignalKind::Skip, cur);
        let both = attribute(&signal, Some(prev));
        assert_eq!(both.fingerprints(), vec![cur, prev]);
        // Same-state previous collapses to one attribution.
        let one = attribute(&signal, Some(cur));
        assert_eq!(one.fingerprints(), vec![cur]);
        // Missing previous-phrase state → one attribution.
        assert_eq!(attribute(&signal, None).fingerprints(), vec![cur]);
    }

    #[test]
    fn fingerprint_jsonl_roundtrip() {
        let fp = state(0.9, 7).fingerprint(Granularity::Fine);
        let line = serde_json::to_string(&fp).unwrap();
        let back: StateFingerprint = serde_json::from_str(&line).unwrap();
        assert_eq!(back, fp);
    }
}
