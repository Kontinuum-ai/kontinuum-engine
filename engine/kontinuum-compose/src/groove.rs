//! Named groove templates (#17): (microtiming bias, swing, jitter, velocity
//! contour) bundles — the hand-made groove vocabulary. A seed picks one per
//! session; corpus-extracted grooves arrive via #23 as a [`GrooveBank`]
//! (loaded from the corpus crate's groove-templates artifact) and are
//! applied through the same timing path.

use kontinuum_clock::Rng;
use kontinuum_corpus::GrooveTemplatesArtifact;
use kontinuum_ir::schema::Step;

/// Microtiming bias in ticks (±12 = push/pull), jitter sigma in ticks.
pub struct Groove {
    pub name: &'static str,
    pub bias_ticks: i16,
    pub swing: f32,
    pub jitter_ticks: f32,
    /// Velocity multiplier applied to off-8th steps (downbeat-heavy vs pushed).
    pub offbeat_gain: f32,
}

pub const STRAIGHT_MACHINE: Groove =
    Groove { name: "straight-machine", bias_ticks: 0, swing: 0.0, jitter_ticks: 1.0, offbeat_gain: 1.0 };
pub const MPC: Groove =
    Groove { name: "mpc-ish", bias_ticks: -8, swing: 0.10, jitter_ticks: 3.0, offbeat_gain: 0.92 };
pub const DRUNK_SHUFFLE: Groove =
    Groove { name: "drunk-shuffle", bias_ticks: 12, swing: 0.18, jitter_ticks: 4.0, offbeat_gain: 0.85 };
pub const PUSHED_HATS: Groove =
    Groove { name: "pushed-hats", bias_ticks: 10, swing: 0.0, jitter_ticks: 2.0, offbeat_gain: 1.08 };
pub const LAID_BACK: Groove =
    Groove { name: "laid-back", bias_ticks: -12, swing: 0.08, jitter_ticks: 2.0, offbeat_gain: 0.95 };
pub const TENSE: Groove =
    Groove { name: "tense", bias_ticks: 4, swing: 0.05, jitter_ticks: 1.0, offbeat_gain: 1.0 };

pub const ALL: [&Groove; 6] =
    [&STRAIGHT_MACHINE, &MPC, &DRUNK_SHUFFLE, &PUSHED_HATS, &LAID_BACK, &TENSE];

/// Seeded pick. `None` name = free draw; a matching name pins the template.
pub fn pick(name: Option<&str>, rng: &mut Rng) -> &'static Groove {
    name.and_then(|n| ALL.iter().copied().find(|g| g.name == n))
        .unwrap_or_else(|| ALL[rng.below(ALL.len() as u64) as usize])
}

/// Closest template by swing amount (issue #21 audio DNA → groove knob).
/// Ties take the earlier template in [`ALL`], so the mapping is total and
/// deterministic. Pushed-hats and straight-machine both swing 0 — the
/// mapping carries only the timing feel; offbeat gain stays the template's.
pub fn nearest_swing(swing: f32) -> &'static Groove {
    ALL.iter()
        .copied()
        .min_by(|a, b| {
            let da = (a.swing - swing).abs();
            let db = (b.swing - swing).abs();
            da.total_cmp(&db)
        })
        .unwrap_or(&STRAIGHT_MACHINE)
}

impl Groove {
    fn as_active(&self) -> ActiveGroove {
        ActiveGroove { bias_ticks: self.bias_ticks, jitter_ticks: self.jitter_ticks, offbeat_gain: self.offbeat_gain }
    }

    /// Applies bias + jitter + offbeat contour to already-swing-corrected
    /// steps. Deterministic in the caller's stream.
    pub fn apply(&self, steps: &mut [Step], rng: &mut Rng) {
        self.as_active().apply(steps, rng);
    }
}

/// The timing bundle actually applied to steps, shared by the hand-made
/// vocabulary and corpus-extracted templates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ActiveGroove {
    bias_ticks: i16,
    jitter_ticks: f32,
    offbeat_gain: f32,
}

impl ActiveGroove {
    pub fn from_static(g: &Groove) -> Self {
        g.as_active()
    }

    pub fn from_corpus(g: &CorpusGroove) -> Self {
        ActiveGroove { bias_ticks: g.bias_ticks, jitter_ticks: g.jitter_ticks, offbeat_gain: g.offbeat_gain }
    }

    /// The recorded bundle (issue #17): the session IR's `pattern_engine`
    /// reads the resolved push/pull and jitter through these.
    pub fn bias_ticks(&self) -> i16 {
        self.bias_ticks
    }

    pub fn jitter_ticks(&self) -> f32 {
        self.jitter_ticks
    }

    pub fn offbeat_gain(&self) -> f32 {
        self.offbeat_gain
    }

    /// Applies bias + jitter + offbeat contour to already-swing-corrected
    /// steps (see [`Groove::apply`]).
    pub fn apply(&self, steps: &mut [Step], rng: &mut Rng) {
        self.apply_tilted(Tilt::Full, steps, rng);
    }

    /// Per-track microtiming (issue #17): the push/pull bias is tilted by
    /// the track's role — pulse voices ride the full groove, the low end
    /// half of it (a locked low end with a pushed/pulled top is the feel),
    /// and the backbeat stays on the grid (a late clap reads as a mistake).
    /// Jitter follows the same tilt; the offbeat velocity contour always
    /// applies in full.
    pub fn apply_tilted(&self, tilt: Tilt, steps: &mut [Step], rng: &mut Rng) {
        let is_offbeat = |pos: u32| (pos / 480) % 2 == 1;
        let (bias, jitter) = tilt.scaled(self.bias_ticks, self.jitter_ticks);
        for st in steps.iter_mut() {
            let jitter = rng.range_f32(-jitter, jitter);
            let shifted = st.microtiming_ticks as f32 + bias as f32 + jitter;
            st.microtiming_ticks = shifted.clamp(-120.0, 120.0) as i16;
            if is_offbeat(st.position) {
                st.velocity = (st.velocity * self.offbeat_gain).clamp(0.0, 1.0);
            }
        }
    }
}

/// Per-track bias tilt (see [`ActiveGroove::apply_tilted`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tilt {
    /// Pulse voices: the full bundle.
    Full,
    /// Low voices: half bias and jitter — locked but breathing.
    Half,
    /// Backbeat: on the grid, offbeat contour only.
    None,
}

impl Tilt {
    fn scaled(&self, bias: i16, jitter: f32) -> (i16, f32) {
        match self {
            Tilt::Full => (bias, jitter),
            Tilt::Half => ((bias as f32 * 0.5).round() as i16, jitter * 0.5),
            Tilt::None => (0, 0.0),
        }
    }
}

/// A corpus-extracted groove (#23): the artifact's swing/velocity/microtiming
/// profiles reduced to the same timing bundle the hand-made templates use.
#[derive(Clone, Debug, PartialEq)]
pub struct CorpusGroove {
    pub name: String,
    pub bias_ticks: i16,
    /// Clamped swing 0..0.5; the session's swing (drawn in
    /// [`crate::arrangement`]) stays authoritative, this records the fit.
    pub swing: f32,
    pub jitter_ticks: f32,
    pub offbeat_gain: f32,
    /// Mean of the fit's velocity profile — the energy-fit key.
    pub mean_velocity: f32,
    /// Tracks clustered into the template (from the artifact).
    pub members: u32,
}

impl CorpusGroove {
    /// Derives the timing bundle from one artifact template: bias = mean
    /// microtiming, jitter = its spread, offbeat gain = the offbeat/onbeat
    /// velocity ratio, all clamped to the hand-made vocabulary's envelope.
    pub fn from_template(t: &kontinuum_corpus::GrooveTemplate) -> CorpusGroove {
        let mean = |v: &[f32; 16]| v.iter().sum::<f32>() / 16.0;
        let micro_mean = mean(&t.microtiming_profile);
        let micro_var = t.microtiming_profile.iter().map(|m| (m - micro_mean).powi(2)).sum::<f32>() / 16.0;
        let onbeat: f32 = t.velocity_profile.iter().enumerate().filter(|(i, _)| i % 2 == 0).map(|(_, v)| v).sum();
        let offbeat: f32 = t.velocity_profile.iter().enumerate().filter(|(i, _)| i % 2 == 1).map(|(_, v)| v).sum();
        let offbeat_gain =
            if onbeat.abs() < f32::EPSILON { 1.0 } else { (offbeat / onbeat).clamp(0.5, 1.5) };
        CorpusGroove {
            name: t.name.clone(),
            bias_ticks: (micro_mean.round() as i16).clamp(-120, 120),
            swing: t.swing.clamp(0.0, 0.5),
            jitter_ticks: micro_var.sqrt().clamp(0.0, 12.0),
            offbeat_gain,
            mean_velocity: mean(&t.velocity_profile),
            members: t.members,
        }
    }

    pub fn apply(&self, steps: &mut [Step], rng: &mut Rng) {
        ActiveGroove::from_corpus(self).apply(steps, rng);
    }
}

/// A loaded groove-templates artifact: the named corpus vocabulary for one
/// subgenre, selected by name pin or energy fit.
#[derive(Clone, Debug, PartialEq)]
pub struct GrooveBank {
    pub subgenre: String,
    /// `TrackObservation`s fitted (0 = placeholder fit).
    pub corpus_size: u32,
    grooves: Vec<CorpusGroove>,
}

impl GrooveBank {
    /// Parses and version-gates a groove-templates artifact from JSON text.
    pub fn load_json(text: &str) -> Result<GrooveBank, kontinuum_corpus::CorpusError> {
        Ok(Self::from_artifact(&kontinuum_corpus::load_groove(text)?))
    }

    pub fn from_artifact(a: &GrooveTemplatesArtifact) -> GrooveBank {
        GrooveBank {
            subgenre: a.subgenre.clone(),
            corpus_size: a.corpus_size,
            grooves: a.templates.iter().map(CorpusGroove::from_template).collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.grooves.is_empty()
    }

    /// Seeded selection. `Some(name)` pins a template (unknown names fall
    /// through); `None` picks the template whose mean velocity sits nearest
    /// the energy curve's target, with a seeded chance to take the
    /// runner-up so sessions vary. `None` result only when the bank is
    /// empty — callers fall back to the hand-made vocabulary.
    pub fn pick(&self, name: Option<&str>, energy: f32, rng: &mut Rng) -> Option<&CorpusGroove> {
        if let Some(g) = name.and_then(|n| self.grooves.iter().find(|g| g.name == n)) {
            return Some(g);
        }
        let target = 0.4 + 0.5 * energy.clamp(0.0, 1.0);
        let mut by_fit: Vec<&CorpusGroove> = self.grooves.iter().collect();
        by_fit.sort_by(|a, b| {
            (a.mean_velocity - target)
                .abs()
                .total_cmp(&(b.mean_velocity - target).abs())
                .then_with(|| a.name.cmp(&b.name))
        });
        match by_fit.as_slice() {
            [] => None,
            [only] => Some(only),
            [nearest, runner_up, ..] => {
                if rng.chance(0.3) { Some(*runner_up) } else { Some(*nearest) }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kontinuum_clock::stream;

    fn sample_steps() -> Vec<Step> {
        [0, 480, 960, 1440]
            .iter()
            .map(|&pos|             Step {
                position: pos,
                velocity: 0.8,
                probability: 1.0,
                microtiming_ticks: 0,
                ratchet: 1,
                pitch: None,
                gate: None,
                accent: false,
            })
            .collect()
    }

    #[test]
    fn templates_are_distinct_and_named() {
        for (i, a) in ALL.iter().enumerate() {
            for b in ALL.iter().skip(i + 1) {
                assert_ne!(a.name, b.name);
                assert_ne!(
                    (a.bias_ticks, a.swing, a.jitter_ticks, a.offbeat_gain),
                    (b.bias_ticks, b.swing, b.jitter_ticks, b.offbeat_gain),
                    "{} and {} identical",
                    a.name,
                    b.name
                );
            }
        }
    }

    #[test]
    fn apply_is_deterministic_and_bounded() {
        for g in ALL {
            let mut a = sample_steps();
            let mut b = sample_steps();
            g.apply(&mut a, &mut stream(9, 1, 1));
            g.apply(&mut b, &mut stream(9, 1, 1));
            assert_eq!(a, b, "{} non-deterministic", g.name);
            for st in &a {
                assert!((-120..=120).contains(&st.microtiming_ticks));
                assert!((0.0..=1.0).contains(&st.velocity));
            }
        }
    }

    #[test]
    fn drunk_shuffle_pushes_and_lays_back_pulls() {
        let mut pushed = sample_steps();
        DRUNK_SHUFFLE.apply(&mut pushed, &mut stream(3, 1, 1));
        assert!(pushed.iter().all(|s| s.microtiming_ticks > -120));
        let mean: f32 = pushed.iter().map(|s| s.microtiming_ticks as f32).sum::<f32>() / 4.0;
        assert!(mean > 0.0, "drunk-shuffle must push: {mean}");
        let mut back = sample_steps();
        LAID_BACK.apply(&mut back, &mut stream(3, 1, 1));
        let mean_back: f32 = back.iter().map(|s| s.microtiming_ticks as f32).sum::<f32>() / 4.0;
        assert!(mean_back < 0.0, "laid-back must pull: {mean_back}");
    }

    #[test]
    fn pick_resolves_by_name_or_draws() {
        let mut rng = stream(1, 1, 1);
        assert_eq!(pick(Some("tense"), &mut rng).name, "tense");
        let mut rng2 = stream(1, 1, 1);
        let drawn = pick(None, &mut rng2);
        assert!(ALL.iter().any(|g| g.name == drawn.name), "draw must be one of the six");
    }

    #[test]
    fn per_track_tilt_scales_bias_and_locks_the_backbeat() {
        for g in ALL {
            let groove = ActiveGroove::from_static(g);
            let mut full = sample_steps();
            groove.apply_tilted(Tilt::Full, &mut full, &mut stream(21, 1, 1));
            let mut half = sample_steps();
            groove.apply_tilted(Tilt::Half, &mut half, &mut stream(21, 1, 1));
            let mut none = sample_steps();
            groove.apply_tilted(Tilt::None, &mut none, &mut stream(21, 1, 1));
            let mean = |steps: &[Step]| -> f32 {
                steps.iter().map(|s| s.microtiming_ticks as f32).sum::<f32>() / steps.len() as f32
            };
            let (f, h, n) = (mean(&full), mean(&half), mean(&none));
            assert_eq!(n, 0.0, "{}: backbeat stays on the grid", g.name);
            assert!((h - f / 2.0).abs() < 1.5, "{}: low end rides half the bias ({h} vs {f})", g.name);
            // The offbeat velocity contour always applies, even untilted.
            let untilted_offbeat = none.iter().filter(|s| s.position == 480).map(|s| s.velocity).sum::<f32>();
            assert!(untilted_offbeat > 0.0);
        }
    }

    fn template(name: &str, swing: f32, vel: f32, micro: f32) -> kontinuum_corpus::GrooveTemplate {
        kontinuum_corpus::GrooveTemplate {
            name: name.into(),
            swing,
            velocity_profile: [vel; 16],
            microtiming_profile: [micro; 16],
            members: 2,
        }
    }

    fn artifact() -> kontinuum_corpus::GrooveTemplatesArtifact {
        kontinuum_corpus::GrooveTemplatesArtifact {
            artifact_version: kontinuum_corpus::ARTIFACT_VERSION,
            corpus_size: 3,
            subgenre: "minimal-techno".into(),
            templates: vec![template("t0", 0.0, 0.4, -4.0), template("t1", 0.1, 0.8, 3.0)],
        }
    }

    #[test]
    fn corpus_template_derives_the_timing_bundle() {
        let g = CorpusGroove::from_template(&template("t0", 0.3, 0.6, -4.0));
        assert_eq!(g.bias_ticks, -4);
        assert!((g.jitter_ticks - 0.0).abs() < 1e-4, "flat profile has no jitter");
        assert_eq!(g.offbeat_gain, 1.0, "flat velocity profile is neither pushed nor pulled");
        assert!((g.swing - 0.3).abs() < 1e-6);
    }

    #[test]
    fn bank_applies_and_stays_bounded() {
        let bank = GrooveBank::from_artifact(&artifact());
        let mut rng = stream(2, 1, 1);
        let g = bank.pick(None, 0.9, &mut rng).expect("bank has templates");
        let mut steps = sample_steps();
        g.apply(&mut steps, &mut stream(2, 1, 2));
        for st in &steps {
            assert!((-120..=120).contains(&st.microtiming_ticks));
            assert!((0.0..=1.0).contains(&st.velocity));
        }
    }

    #[test]
    fn bank_pick_pins_names_and_fits_energy() {
        let bank = GrooveBank::from_artifact(&artifact());
        let mut rng = stream(3, 1, 1);
        assert_eq!(bank.pick(Some("t1"), 0.0, &mut rng).expect("pin").name, "t1");
        // t0's mean velocity 0.4 is the low-energy target; t1's 0.8 the hot one.
        assert_eq!(bank.pick(None, 0.0, &mut rng).expect("fit").name, "t0");
        assert_eq!(bank.pick(None, 1.0, &mut rng).expect("fit").name, "t1");
    }

    #[test]
    fn empty_bank_picks_none() {
        let mut a = artifact();
        a.templates.clear();
        let bank = GrooveBank::from_artifact(&a);
        let mut rng = stream(4, 1, 1);
        assert!(bank.is_empty());
        assert!(bank.pick(None, 0.5, &mut rng).is_none());
    }
}
