//! TR-808 hi-hat circuit (issue #74): six Schmitt-trigger-style square
//! oscillators at the 808's mutually-irrational low frequencies, summed and
//! hard-highpassed around 7 kHz so only the dense inharmonic cloud of upper
//! harmonics above the corner is audible — the fundamentals (205–800 Hz) sit
//! ~120 dB down and are never heard. Open/closed differ only in VCA decay
//! time on the same bank, as in the circuit.

use std::sync::Arc;

use kontinuum_core::voice::ChokeState;
use kontinuum_core::voice::{decay_coeff, flush_denormal, HitJitter, HIT_VARIANTS, NoiseGen};
use super::hand::Biquad;
use kontinuum_core::{ParamId, Voice, SILENCE_ABS};

/// The 808's six square-oscillator frequencies (Hz). Deliberately mutually
/// irrational: their upper harmonics interleave into a never-repeating
/// metallic beat pattern. Public so the #75 fitter can read and target the
/// bank; per-oscillator ParamIds still need `params.rs` slots (see report).
pub const BANK_FREQS: [f32; 6] = [205.3, 254.3, 304.4, 369.6, 522.7, 800.0];

/// Noise/square balance (issue #74): the fraction of the pre-filter signal
/// that is white noise. 0.0 = pure square bank (the 808 circuit), 1.0 = pure
/// noise (the legacy model this replaces). Default 0.1 keeps a trace of grit;
/// noise mixed pre-filter so it takes the same spectral shape as the bank.
/// Lives here until it migrates into `params.rs` — slot 19 is free in the
/// shared table and dispatch is generic over `ParamId`.
pub const HAT_NOISE_MIX: ParamId = 19;

/// Rescales the summed ±1 squares: after the 7 kHz wall only sparse upper
/// harmonics survive (~0.15 RMS of the raw bank), so the filtered result
/// needs reseating. Level only — final mix balance is #76's job.
const BANK_GAIN: f32 = 0.35;

/// Output makeup gain after the VCA.
const OUT_GAIN: f32 = 2.0;

/// `open` extends the VCA decay by up to this much (ms). The circuit's
/// open/closed footswitch is purely an envelope-time difference on the same
/// oscillator bank and filter — no separate voice, no extra scaling fudge.
const OPEN_EXTRA_MS: f32 = 480.0;

/// Butterworth Q staggering for two cascaded 2-pole highpasses: 4 poles
/// total, 24 dB/oct. At the 7 kHz corner that puts 205 Hz about 120 dB down
/// — the two-stage one-pole of the old model could not do this.
const HP_STAGES_Q: [f32; 2] = [0.5412, 1.3066];

/// Choke fade length: ~10 ms at the voice's rate, matching the offline
/// renderer's choke ceiling.
const CHOKE_FADE_MS: f32 = 10.0;

pub struct Hat {
    sr: f32,
    decay_ms: f32,
    open: f32,
    tone: f32,
    noise_mix: f32,
    phases: [f32; 6],
    env: f32,
    env_coeff: f32,
    hp: [Biquad; 2],
    noise: NoiseGen,
    jitter: HitJitter,
    active: bool,
    choke: Option<(Arc<ChokeState>, u8)>,
    /// Epoch this voice's current note owns; 0 = no choke assignment.
    epoch: u64,
    group: u8,
    fading: bool,
    fade_pos: usize,
    fade_len: usize,
}

impl Hat {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let mut h = Hat {
            sr,
            decay_ms: 45.0,
            tone: 0.4,
            noise_mix: 0.1,
            open: 0.0,
            phases: [0.0; 6],
            env: 0.0,
            env_coeff: 1.0,
            hp: [Biquad::new(), Biquad::new()],
            noise: NoiseGen::seeded(),
            jitter: HitJitter::new(),
            active: false,
            choke: None,
            epoch: 0,
            group: 0,
            fading: false,
            fade_pos: 0,
            fade_len: ((CHOKE_FADE_MS / 1000.0) * sample_rate as f32).round().max(1.0) as usize,
        };
        h.update_coeffs();
        h
    }

    fn update_coeffs(&mut self) {
        let effective = self.decay_ms + self.open.clamp(0.0, 1.0) * OPEN_EXTRA_MS;
        self.set_hit_coeffs(effective, 1.0);
    }

    /// Per-hit coefficients: decay and tone carry the hit's jitter.
    fn set_hit_coeffs(&mut self, effective_ms: f32, tone_mul: f32) {
        self.env_coeff = decay_coeff(self.sr, effective_ms);
        // Tone is the highpass corner: 5..10 kHz, default 0.4 = the 808's 7 kHz.
        let corner = (5000.0 + self.tone.clamp(0.0, 1.0) * 5000.0) * tone_mul;
        for (stage, &q) in self.hp.iter_mut().zip(HP_STAGES_Q.iter()) {
            stage.set_highpass(self.sr, corner, q);
        }
    }

    /// Join a choke group. Voices sharing `state` in the same non-zero
    /// group fade each other out on retrigger.
    pub fn set_choke(&mut self, state: Arc<ChokeState>, group: u8) {
        self.choke = Some((state, group));
        self.group = group;
    }
}

impl Voice for Hat {
    fn note_on(&mut self, _pitch: f32, velocity: f32) {
        if let Some((state, group)) = &self.choke {
            self.epoch = state.trigger(*group);
        }
        self.fading = false;
        self.fade_pos = 0;
        let j = self.jitter.next_hit((0.68, 1.25), 0.0, 0.15, (0.92, 1.08));
        self.phases = [0.0; 6];
        for stage in self.hp.iter_mut() {
            stage.reset();
        }
        self.env = velocity.clamp(0.0, 1.0) * j.amp;
        let effective = self.decay_ms * j.decay + self.open.clamp(0.0, 1.0) * OPEN_EXTRA_MS;
        self.set_hit_coeffs(effective, j.tone);
        self.noise = NoiseGen::seeded_at(HIT_VARIANTS[j.variant]);
        self.active = self.env > 0.0;
    }

    fn note_off(&mut self) {}

    fn is_active(&self) -> bool {
        self.active
    }

    fn render(&mut self, out: &mut [f32]) {
        for slot in out.iter_mut() {
            if !self.active {
                *slot = 0.0;
                continue;
            }
            let mut gain = 1.0;
            if self.fading {
                if self.fade_pos >= self.fade_len {
                    self.active = false;
                    *slot = 0.0;
                    continue;
                }
                self.fade_pos += 1;
                gain = 1.0 - self.fade_pos as f32 / self.fade_len as f32;
            } else if self.epoch > 0 {
                if let Some((state, group)) = &self.choke {
                    if state.current(*group) != self.epoch {
                        self.fading = true;
                        self.fade_pos = 0;
                    }
                }
            }
            let mut bank = 0.0f32;
            let sr = self.sr;
            for (phase, &f) in self.phases.iter_mut().zip(BANK_FREQS.iter()) {
                *phase += f / sr;
                if *phase >= 1.0 {
                    *phase -= 1.0;
                }
                bank += if *phase < 0.5 { 1.0 } else { -1.0 };
            }
            let src = bank * BANK_GAIN * (1.0 - self.noise_mix)
                + self.noise.next_f32() * self.noise_mix;
            let s = self.hp[0].tick(src);
            let s = self.hp[1].tick(s) * self.env * OUT_GAIN * gain;
            self.env = flush_denormal(self.env * self.env_coeff);
            *slot = s;
            if self.env < SILENCE_ABS {
                self.active = false;
            }
        }
    }

    fn set_param(&mut self, param: ParamId, value: f32) {
        use kontinuum_core::params::*;
        match param {
            HAT_DECAY_MS => {
                self.decay_ms = value.clamp(5.0, 2000.0);
                self.update_coeffs();
            }
            HAT_TONE => {
                self.tone = value.clamp(0.0, 1.0);
                self.update_coeffs();
            }
            HAT_OPEN => {
                self.open = value.clamp(0.0, 1.0);
                self.update_coeffs();
            }
            HAT_NOISE_MIX => {
                self.noise_mix = value.clamp(0.0, 1.0);
            }
            _ => {}
        }
    }

    fn reset(&mut self) {
        let choke = self.choke.take();
        *self = Self::new(self.sr as u32);
        if let Some((state, group)) = choke {
            self.set_choke(state, group);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kontinuum_core::BLOCK_FRAMES;

    const SR: u32 = 48_000;

    /// Render one hit until the voice self-mutes, BLOCK_FRAMES at a time.
    fn render_hit<V: Voice>(voice: &mut V, velocity: f32) -> Vec<f32> {
        voice.note_on(60.0, velocity);
        let mut buf = [0.0f32; kontinuum_core::BLOCK_FRAMES];
        let mut out = Vec::new();
        while voice.is_active() {
            voice.render(&mut buf);
            out.extend_from_slice(&buf);
        }
        out
    }

    /// Mean spacing between strict sign changes, in samples.
    fn mean_zero_crossing_spacing(x: &[f32]) -> f32 {
        let mut crossings = 0usize;
        for w in x.windows(2) {
            if (w[0] < 0.0) != (w[1] < 0.0) && w[1] != 0.0 {
                crossings += 1;
            }
        }
        if crossings == 0 { f32::INFINITY } else { x.len() as f32 / crossings as f32 }
    }

    #[test]
    fn hat_bank_freqs_are_the_808_set() {
        assert_eq!(BANK_FREQS, [205.3, 254.3, 304.4, 369.6, 522.7, 800.0]);
    }

    #[test]
    fn closed_hat_lifecycle_peak_and_silent_tail() {
        let mut hat = Hat::new(SR);
        assert!(!hat.is_active());
        let x = render_hit(&mut hat, 1.0);
        assert!(!hat.is_active());
        assert!(x.len() < 4000 * kontinuum_core::BLOCK_FRAMES, "hat never went idle");
        let peak = x.iter().fold(0.0f32, |m, &v| m.max(v.abs()));
        assert!(peak > 0.02 && peak < 1.6, "hat peak out of range: {peak}");
        let mut tail = [1.0f32; kontinuum_core::BLOCK_FRAMES];
        hat.render(&mut tail);
        assert!(tail.iter().all(|&s| s == 0.0), "tail must be exactly zero");
    }

    #[test]
    fn hat_is_deterministic() {
        let mut a = Hat::new(SR);
        let mut b = Hat::new(SR);
        let ra = render_hit(&mut a, 0.8);
        let rb = render_hit(&mut b, 0.8);
        assert_eq!(ra.len(), rb.len());
        assert!(ra.iter().zip(rb.iter()).all(|(x, y)| x.to_bits() == y.to_bits()));
    }

    /// Windowed Goertzel power at `freq`: a single-bin DFT with f64
    /// accumulators and a Hann window (sidelobes -31 dB), enough to tell a
    /// spectral line from the floor between lines without an FFT dependency.
    fn goertzel_power(x: &[f32], freq: f32, sr: f32) -> f64 {
        let n = x.len();
        let w = std::f64::consts::TAU * freq as f64 / sr as f64;
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (i, &v) in x.iter().enumerate() {
            let s = v as f64 * (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos());
            let t = w * i as f64;
            re += s * t.cos();
            im -= s * t.sin();
        }
        let coherent = n as f64 * 0.5;
        (re * re + im * im) / (coherent * coherent)
    }

    /// THE discriminator (issue #74 acceptance: "discrete inharmonic peaks
    /// rather than a smooth noise floor"). Probe power at the exact
    /// odd-harmonic partial frequencies of the six bank oscillators in
    /// 6.9..8 kHz must dominate the between-partial gaps. The old signature
    /// cannot pass: its squares sat at 3.5-6.4 kHz, below this probe region,
    /// and its noise bed was flat across these very bins (the white-noise
    /// control below scores ~1.0 by construction).
    #[test]
    fn hat_shows_discrete_inharmonic_peaks_above_the_corner() {
        let x = render_hit(&mut Hat::new(SR), 1.0);
        let window = &x[..x.len().min(8192)];

        let mut partials: Vec<f32> = BANK_FREQS
            .iter()
            .flat_map(|&f| (1..=97usize).step_by(2).map(move |n| f * n as f32))
            .filter(|&p| p > 6900.0 && p < 8000.0)
            .collect();
        partials.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert!(partials.len() >= 8, "partial grid too sparse: {partials:?}");
        let nulls: Vec<f32> = partials
            .windows(2)
            .filter(|w| w[1] - w[0] > 80.0)
            .map(|w| (w[0] + w[1]) / 2.0)
            .collect();
        assert!(nulls.len() >= 3, "no clean gaps between partials");

        let bin_average = |bins: &[f32]| {
            bins.iter().map(|&f| goertzel_power(window, f, SR as f32)).sum::<f64>()
                / bins.len() as f64
        };
        let on_peaks = bin_average(&partials);
        let on_floor = bin_average(&nulls);
        assert!(
            on_peaks / on_floor > 4.0,
            "no comb structure above the corner: peaks {on_peaks:.3e} vs floor {on_floor:.3e}"
        );

        // Control: highpassed white noise must NOT show the comb. Single-window
        // Goertzel estimates are chi-square-noisy (one 8192 window put a
        // pure-noise ratio near 5), so average 32 independent windows — the
        // expected ratio concentrates at 1.0 and the threshold stays honest.
        let windows: usize = 32;
        let mut noise = NoiseGen::seeded();
        let noise_ref: Vec<f32> =
            (0..window.len() * windows).map(|_| noise.next_f32()).collect();
        let mut hp_a = Biquad::new();
        let mut hp_b = Biquad::new();
        hp_a.set_highpass(SR as f32, 7000.0, 0.5412);
        hp_b.set_highpass(SR as f32, 7000.0, 1.3066);
        let mut noise_peaks = 0.0f64;
        let mut noise_floor = 0.0f64;
        for w in 0..windows {
            let seg: Vec<f32> = noise_ref[w * window.len()..(w + 1) * window.len()]
                .iter()
                .map(|&v| hp_b.tick(hp_a.tick(v)))
                .collect();
            noise_peaks += partials
                .iter()
                .map(|&f| goertzel_power(&seg, f, SR as f32))
                .sum::<f64>()
                / partials.len() as f64;
            noise_floor += nulls
                .iter()
                .map(|&f| goertzel_power(&seg, f, SR as f32))
                .sum::<f64>()
                / nulls.len() as f64;
        }
        let (noise_peaks, noise_floor) = (noise_peaks / windows as f64, noise_floor / windows as f64);
        assert!(
            noise_peaks / noise_floor < 2.0,
            "control noise shows a comb: {noise_peaks:.3e} vs {noise_floor:.3e}"
        );
    }

    /// Circuit gate: the surviving energy must sit above the corner. Measured
    /// with the crate's own Biquad (two HP sections at 7 kHz, the analysis
    /// twin of the production filter): at least 30% of total power passes,
    /// and below 2 kHz — where the 205-800 Hz fundamentals live — less than
    /// 1% survives. (Old model also highpassed, so this gates the circuit,
    /// it does not discriminate signatures; the autocorr test discriminates.)
    #[test]
    fn hat_energy_concentrated_above_the_corner() {
        let x = render_hit(&mut Hat::new(SR), 1.0);
        let window = &x[..x.len().min(8192)];
        let total: f32 = window.iter().map(|v| v * v).sum();
        assert!(total > 0.0);

        let mut hp_a = Biquad::new();
        let mut hp_b = Biquad::new();
        hp_a.set_highpass(SR as f32, 7000.0, 0.5412);
        hp_b.set_highpass(SR as f32, 7000.0, 1.3066);
        let above: f32 = window
            .iter()
            .map(|&v| {
                let y = hp_b.tick(hp_a.tick(v));
                y * y
            })
            .sum();
        assert!(above / total > 0.3, "energy above corner too weak: {}", above / total);

        let mut hp_low = Biquad::new();
        hp_low.set_highpass(SR as f32, 2000.0, std::f32::consts::FRAC_1_SQRT_2);
        let above_2k: f32 = window
            .iter()
            .map(|&v| {
                let y = hp_low.tick(v);
                y * y
            })
            .sum();
        assert!(1.0 - above_2k / total < 0.01, "fundamentals leaked: {}", 1.0 - above_2k / total);
    }

    /// Character gate: the voice is a high-frequency instrument. Zero-crossing
    /// density gives a dominant-frequency estimate; noise controls cross far
    /// more often, partials near 7 kHz set a regular ~3.4-sample spacing.
    #[test]
    fn hat_zero_crossing_density_sits_above_the_corner_region() {
        let x = render_hit(&mut Hat::new(SR), 1.0);
        let window = &x[..x.len().min(4096)];
        let spacing = mean_zero_crossing_spacing(window);
        let f_est = SR as f32 / (2.0 * spacing);
        assert!(f_est > 5500.0, "hat dominant frequency too low: {f_est:.0} Hz");
    }

    /// Open/closed is a pure envelope-time difference on the same bank.
    #[test]
    fn open_hat_rings_longer_than_closed() {
        let mut closed = Hat::new(SR);
        let mut open = Hat::new(SR);
        open.set_param(kontinuum_core::params::HAT_OPEN, 1.0);
        let n_closed = render_hit(&mut closed, 1.0).len();
        let n_open = render_hit(&mut open, 1.0).len();
        assert!(
            n_open > n_closed * 4,
            "open hat not much longer: closed {n_closed}, open {n_open}"
        );
    }

    #[test]
    fn hat_params_clamp_to_bounds() {
        let render_with = |param: ParamId, value: f32| {
            let mut h = Hat::new(SR);
            h.set_param(param, value);
            render_hit(&mut h, 1.0)
        };
        let cases = [
            (kontinuum_core::params::HAT_DECAY_MS, 50_000.0, 2000.0),
            (kontinuum_core::params::HAT_TONE, 7.0, 1.0),
            (kontinuum_core::params::HAT_OPEN, -3.0, 0.0),
            (HAT_NOISE_MIX, 42.0, 1.0),
        ];
        for (param, out_of_range, at_bound) in cases {
            let a = render_with(param, out_of_range);
            let b = render_with(param, at_bound);
            assert_eq!(a.len(), b.len(), "{param}");
            assert!(
                a.iter().zip(b.iter()).all(|(x, y)| x.to_bits() == y.to_bits()),
                "{param} did not clamp {out_of_range} to {at_bound}"
            );
        }
    }

    fn hat_with(state: Arc<ChokeState>, group: u8) -> Hat {
        let mut h = Hat::new(SR);
        h.set_choke(state, group);
        h
    }

    #[test]
    fn same_group_hats_choke_within_10ms() {
        let state = ChokeState::shared();
        let mut open = hat_with(Arc::clone(&state), 1);
        open.set_param(kontinuum_core::params::HAT_OPEN, 1.0);
        let mut closed = hat_with(Arc::clone(&state), 1);
        open.note_on(60.0, 1.0);
        closed.note_on(60.0, 1.0);

        let mut buf = [0.0f32; BLOCK_FRAMES];
        open.render(&mut buf);
        assert!(buf.iter().any(|&s| s != 0.0), "open hat sounds before the choke");

        // The closed hit chokes the open one; the fade finishes inside 10 ms
        // (480 frames at 48 kHz), so a later block is exactly silent.
        let fade_len = ((CHOKE_FADE_MS / 1000.0) * SR as f32).round() as usize;
        let mut silenced = [1.0f32; 512];
        for _ in 0..fade_len / BLOCK_FRAMES {
            open.render(&mut buf);
        }
        open.render(&mut buf[..fade_len % BLOCK_FRAMES]);
        open.render(&mut silenced);
        assert!(silenced.iter().all(|&s| s == 0.0), "choked hat must hit exact zero");
        assert!(!open.is_active(), "choked open hat must leave the pool");

        open.note_on(60.0, 1.0);
        let mut fresh = [0.0f32; BLOCK_FRAMES];
        open.render(&mut fresh);
        assert!(fresh.iter().any(|&s| s != 0.0), "a new note escapes the stale epoch");
    }

    #[test]
    fn a_fresh_hit_is_never_choked_by_its_own_trigger() {
        let state = ChokeState::shared();
        let mut wired = hat_with(Arc::clone(&state), 1);
        wired.note_on(60.0, 1.0);
        // Control: identical hit with no choke wired at all. The voice holds
        // the current epoch, so the gain path must be bit-exact passthrough.
        let mut bare = Hat::new(SR);
        bare.note_on(60.0, 1.0);

        let mut buf_w = [0.0f32; BLOCK_FRAMES];
        let mut buf_b = [0.0f32; BLOCK_FRAMES];
        let mut blocks = 0usize;
        while wired.is_active() || bare.is_active() {
            wired.render(&mut buf_w);
            bare.render(&mut buf_b);
            assert!(
                buf_w.iter().zip(buf_b.iter()).all(|(w, b)| w.to_bits() == b.to_bits()),
                "un-choked hit diverged from the bare control at block {blocks}"
            );
            blocks += 1;
            assert!(blocks < 4000, "hat never went idle");
        }
    }

    #[test]
    fn other_choke_groups_are_unaffected() {
        let run = |sibling: Option<u8>| -> Vec<f32> {
            let state = ChokeState::shared();
            let mut hat = hat_with(Arc::clone(&state), 1);
            hat.note_on(60.0, 1.0);
            let mut out = [0.0f32; BLOCK_FRAMES];
            hat.render(&mut out);
            if let Some(group) = sibling {
                let mut other = hat_with(Arc::clone(&state), group);
                other.note_on(60.0, 1.0);
            }
            let mut tail = [0.0f32; 1024];
            hat.render(&mut tail);
            tail.to_vec()
        };
        // An other-group retrigger must leave the hat's tail bit-identical
        // to an uninterrupted run.
        let alone = run(None);
        let other_group = run(Some(2));
        assert!(
            alone.iter().zip(other_group.iter()).all(|(a, b)| a.to_bits() == b.to_bits()),
            "an other-group retrigger disturbed the hat"
        );
        // Control that the mechanism is live: a same-group retrigger chokes
        // the tail to exact zero inside the fade budget.
        let same_group = run(Some(1));
        assert!(
            same_group[480..].iter().all(|v| *v == 0.0),
            "same-group retrigger did not choke"
        );
    }

    #[test]
    fn hat_choke_is_deterministic() {
        let run = || {
            let state = ChokeState::shared();
            let mut a = hat_with(Arc::clone(&state), 1);
            let mut b = hat_with(Arc::clone(&state), 1);
            a.note_on(60.0, 1.0);
            let mut out = [0.0f32; BLOCK_FRAMES];
            a.render(&mut out);
            b.note_on(60.0, 1.0);
            let mut tail = [0.0f32; 1024];
            a.render(&mut tail);
            tail
        };
        let x = run();
        let y = run();
        assert!(x.iter().zip(y.iter()).all(|(p, q)| p.to_bits() == q.to_bits()));
    }
}
