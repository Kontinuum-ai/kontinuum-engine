//! Hand percussion voices: clap, snare, shaker. All deterministic (fixed-seed
//! noise, fixed envelopes) and self-muting below the silence threshold.

use kontinuum_core::voice::{decay_coeff, flush_denormal, HitJitter, HIT_VARIANTS, NoiseGen};
use kontinuum_core::{ParamId, Voice, SILENCE_ABS};

/// Clap bandpass centre as a direct frequency (Hz), issue #74. Lives here
/// until it migrates into `params.rs`; slot 22 is free in the shared table
/// and `VoicePool`/`AudioGraph` dispatch is generic over `ParamId`, so no
/// other file needs to change for this to reach the voice.
pub const CLAP_CENTER_HZ: ParamId = 22;

/// Clap bandpass resonance, bounded 0.5..8. Below ~0.5 the band is too wide
/// to smack; above ~8 the pole pair self-rings long enough to wash the tail.
pub const CLAP_RESONANCE_Q: ParamId = 23;

/// Two-pole biquad (RBJ Audio EQ Cookbook coefficients, Direct Form I),
/// hand-rolled — the crate has no DSP dependencies. Coefficients are
/// normalized by a0 at set time, so `tick` is multiply/add only and RT-safe.
///
/// For `w0 = 2π·f0/sr`, `cosw = cos(w0)`, `α = sin(w0)/(2·Q)`:
/// - highpass: `b = [(1+cosw)/2, -(1+cosw), (1+cosw)/2]` (12 dB/oct)
/// - bandpass, constant 0 dB peak gain: `b = [α, 0, -α]`
/// - both share `a = [1+α, -2·cosw, 1-α]`, everything divided by a0.
///
/// Shared by the clap (bandpass) and the hat (cascaded highpass sections).
pub(crate) struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad {
    pub(crate) const fn new() -> Self {
        Biquad { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0, x1: 0.0, x2: 0.0, y1: 0.0, y2: 0.0 }
    }

    /// Clear the delay lines (per hit, so renders stay bit-stable). Coefficients stay.
    pub(crate) fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }

    /// Keep f0 away from DC and Nyquist: there sin(w0) collapses, α → 0 and
    /// the poles land on the unit circle (self-oscillation, NaN-prone state).
    fn guard_f0(sr: f32, f0: f32) -> f32 {
        f0.clamp(10.0, sr * 0.45)
    }

    pub(crate) fn set_highpass(&mut self, sr: f32, f0: f32, q: f32) {
        let w0 = std::f32::consts::TAU * Self::guard_f0(sr, f0) / sr;
        let cosw = w0.cos();
        let alpha = w0.sin() / (2.0 * q);
        let a0 = 1.0 + alpha;
        self.b0 = (1.0 + cosw) / 2.0 / a0;
        self.b1 = -(1.0 + cosw) / a0;
        self.b2 = self.b0;
        self.a1 = -2.0 * cosw / a0;
        self.a2 = (1.0 - alpha) / a0;
    }

    pub(crate) fn set_bandpass(&mut self, sr: f32, f0: f32, q: f32) {
        let w0 = std::f32::consts::TAU * Self::guard_f0(sr, f0) / sr;
        let cosw = w0.cos();
        let alpha = w0.sin() / (2.0 * q);
        let a0 = 1.0 + alpha;
        self.b0 = alpha / a0;
        self.b1 = 0.0;
        self.b2 = -alpha / a0;
        self.a1 = -2.0 * cosw / a0;
        self.a2 = (1.0 - alpha) / a0;
    }

    pub(crate) fn tick(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2 - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = flush_denormal(y);
        y
    }
}

/// 808-style clap: three pre-echoes into a longer noise tail, run through a
/// resonant bandpass around 1.1 kHz (issue #74). The resonance is the whole
/// difference between "smack" and "shh".
pub struct Clap {
    sr: f32,
    decay_ms: f32,
    center_hz: f32,
    q: f32,
    env: f32,
    env_coeff: f32,
    burst: f32,
    burst_count: u8,
    frames_to_next_burst: u32,
    bp: Biquad,
    noise: NoiseGen,
    jitter: HitJitter,
    active: bool,
}

/// Bandpass output makeup gain: the biquad keeps peak gain at 0 dB but the
/// noise band is narrow, so the broadband level drops hard. Reseat only —
/// final mix balance is #76's job.
const CLAP_GAIN: f32 = 4.0;

impl Clap {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let mut c = Clap {
            sr,
            decay_ms: 350.0,
            center_hz: 1100.0,
            q: 1.2,
            env: 0.0,
            env_coeff: 1.0,
            burst: 0.0,
            burst_count: 0,
            frames_to_next_burst: 0,
            bp: Biquad::new(),
            noise: NoiseGen::seeded(),
            jitter: HitJitter::new(),
            active: false,
        };
        c.update_coeffs();
        c
    }

    fn update_coeffs(&mut self) {
        self.env_coeff = decay_coeff(self.sr, self.decay_ms);
        self.bp.set_bandpass(self.sr, self.center_hz, self.q);
    }
}

impl Voice for Clap {
    fn note_on(&mut self, _pitch: f32, velocity: f32) {
        let j = self.jitter.next_hit((0.75, 1.2), 0.0, 0.1, (0.9, 1.1));
        self.env = velocity.clamp(0.0, 1.0) * j.amp;
        self.env_coeff = decay_coeff(self.sr, self.decay_ms * j.decay);
        self.bp.reset();
        self.bp.set_bandpass(self.sr, self.center_hz * j.tone, self.q);
        self.burst = 1.0;
        self.burst_count = 3;
        self.frames_to_next_burst = (0.011 * self.sr) as u32;
        self.noise = NoiseGen::seeded_at(HIT_VARIANTS[j.variant]);
        self.active = self.env > 0.0;
    }

    fn note_off(&mut self) {}

    fn is_active(&self) -> bool {
        self.active
    }

    fn render(&mut self, out: &mut [f32]) {
        // Pre-echo spacing: ~11 ms between the three leading bursts. The
        // countdown lives on the voice, not in the render call, so bursts 2
        // and 3 still fire when the graph slices into BLOCK_FRAMES chunks.
        let burst_gap = (0.011 * self.sr) as u32;
        for slot in out.iter_mut() {
            if !self.active {
                *slot = 0.0;
                continue;
            }
            let mut s = self.noise.next_f32() * (self.burst * 0.8 + self.env * 0.5);
            if self.burst_count > 0 {
                self.frames_to_next_burst -= 1;
                if self.frames_to_next_burst == 0 {
                    self.burst = 1.0;
                    self.burst_count -= 1;
                    self.frames_to_next_burst = burst_gap;
                } else {
                    self.burst = flush_denormal(self.burst * 0.985);
                }
            } else {
                self.burst = 0.0;
            }
            s = self.bp.tick(s);
            s *= self.env;
            self.env = flush_denormal(self.env * self.env_coeff);
            *slot = s * CLAP_GAIN;
            if self.env < SILENCE_ABS {
                self.active = false;
            }
        }
    }

    fn set_param(&mut self, param: ParamId, value: f32) {
        use kontinuum_core::params::*;
        match param {
            CLAP_DECAY_MS => {
                self.decay_ms = value.clamp(50.0, 1500.0);
                self.update_coeffs();
            }
            // Tone sweeps the bandpass centre over 600..1500 Hz; the default
            // 0.55 lands at ~1095 Hz, the "smack" region of the real clap.
            CLAP_TONE => {
                self.center_hz = 600.0 + value.clamp(0.0, 1.0) * 900.0;
                self.update_coeffs();
            }
            CLAP_CENTER_HZ => {
                self.center_hz = value.clamp(400.0, 2500.0);
                self.update_coeffs();
            }
            CLAP_RESONANCE_Q => {
                self.q = value.clamp(0.5, 8.0);
                self.update_coeffs();
            }
            _ => {}
        }
    }

    fn reset(&mut self) {
        *self = Self::new(self.sr as u32);
    }
}

/// Snare: two detuned tone rods over a bright noise bed.
pub struct Snare {
    sr: f32,
    tune_hz: f32,
    hit_tune_hz: f32,
    decay_ms: f32,
    snap: f32,
    phase: [f32; 2],
    env: f32,
    env_coeff: f32,
    noise_env: f32,
    noise_coeff: f32,
    hp_lp: f32,
    hp_a: f32,
    noise: NoiseGen,
    jitter: HitJitter,
    active: bool,
}

impl Snare {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let mut s = Snare {
            sr,
            tune_hz: 185.0,
            hit_tune_hz: 185.0,
            decay_ms: 220.0,
            snap: 0.6,
            phase: [0.0; 2],
            env: 0.0,
            env_coeff: 1.0,
            noise_env: 0.0,
            noise_coeff: 1.0,
            hp_lp: 0.0,
            hp_a: 1.0,
            noise: NoiseGen::seeded(),
            jitter: HitJitter::new(),
            active: false,
        };
        s.update_coeffs();
        s
    }

    fn update_coeffs(&mut self) {
        self.env_coeff = decay_coeff(self.sr, self.decay_ms);
        self.noise_coeff = decay_coeff(self.sr, self.decay_ms * 0.55);
        self.hp_a = 1.0 - (-std::f32::consts::TAU * 1200.0 / self.sr).exp();
    }
}

impl Voice for Snare {
    fn note_on(&mut self, _pitch: f32, velocity: f32) {
        let j = self.jitter.next_hit((0.75, 1.2), 25.0, 0.1, (0.9, 1.1));
        self.hit_tune_hz = self.tune_hz * j.pitch;
        self.phase = [0.0; 2];
        self.env = velocity.clamp(0.0, 1.0) * j.amp;
        self.noise_env = self.env * (0.4 + self.snap * 0.8) * j.tone;
        self.env_coeff = decay_coeff(self.sr, self.decay_ms * j.decay);
        self.hp_lp = 0.0;
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
            self.phase[0] += self.hit_tune_hz / self.sr;
            self.phase[1] += self.hit_tune_hz * 1.78 / self.sr;
            for p in self.phase.iter_mut() {
                if *p >= 1.0 {
                    *p -= 1.0;
                }
            }
            let tone = (std::f32::consts::TAU * self.phase[0]).sin() * 0.6
                + (std::f32::consts::TAU * self.phase[1]).sin() * 0.35;
            let mut s = tone * self.env + self.noise.next_f32() * self.noise_env;
            self.hp_lp += self.hp_a * (s - self.hp_lp);
            s -= self.hp_lp;
            self.env = flush_denormal(self.env * self.env_coeff);
            self.noise_env = flush_denormal(self.noise_env * self.noise_coeff);
            *slot = s * 0.9;
            if self.env < SILENCE_ABS && self.noise_env < SILENCE_ABS {
                self.active = false;
            }
        }
    }

    fn set_param(&mut self, param: ParamId, value: f32) {
        use kontinuum_core::params::*;
        match param {
            SNARE_TUNE_HZ => self.tune_hz = value.clamp(120.0, 420.0),
            SNARE_DECAY_MS => {
                self.decay_ms = value.clamp(60.0, 900.0);
                self.update_coeffs();
            }
            SNARE_SNAP => {
                self.snap = value.clamp(0.0, 1.0);
                self.update_coeffs();
            }
            _ => {}
        }
    }

    fn reset(&mut self) {
        *self = Self::new(self.sr as u32);
    }
}

/// Shaker: slow-attack bright noise, short body — the groove glue.
pub struct Shaker {
    sr: f32,
    decay_ms: f32,
    tone: f32,
    env: f32,
    attack_ramp: f32,
    env_coeff: f32,
    hp_lp: f32,
    hp_a: f32,
    noise: NoiseGen,
    jitter: HitJitter,
    active: bool,
}

impl Shaker {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let mut s = Shaker {
            sr,
            decay_ms: 90.0,
            tone: 0.6,
            env: 0.0,
            attack_ramp: 0.0,
            env_coeff: 1.0,
            hp_lp: 0.0,
            hp_a: 1.0,
            noise: NoiseGen::seeded(),
            jitter: HitJitter::new(),
            active: false,
        };
        s.update_coeffs();
        s
    }

    fn update_coeffs(&mut self) {
        self.env_coeff = decay_coeff(self.sr, self.decay_ms);
        let cutoff = 5500.0 + self.tone.clamp(0.0, 1.0) * 6000.0;
        self.hp_a = 1.0 - (-std::f32::consts::TAU * cutoff / self.sr).exp();
    }
}

impl Voice for Shaker {
    fn note_on(&mut self, _pitch: f32, velocity: f32) {
        let j = self.jitter.next_hit((0.7, 1.25), 0.0, 0.15, (0.92, 1.08));
        self.env = velocity.clamp(0.0, 1.0) * j.amp;
        self.env_coeff = decay_coeff(self.sr, self.decay_ms * j.decay);
        let cutoff = (5500.0 + self.tone.clamp(0.0, 1.0) * 6000.0) * j.tone;
        self.hp_a = 1.0 - (-std::f32::consts::TAU * cutoff / self.sr).exp();
        self.attack_ramp = 0.0;
        self.hp_lp = 0.0;
        self.noise = NoiseGen::seeded_at(HIT_VARIANTS[j.variant]);
        self.active = self.env > 0.0;
    }

    fn note_off(&mut self) {}

    fn is_active(&self) -> bool {
        self.active
    }

    fn render(&mut self, out: &mut [f32]) {
        // ~6 ms soft attack: the shaker "chhk" rather than a click.
        let attack_frames = 0.006 * self.sr;
        for slot in out.iter_mut() {
            if !self.active {
                *slot = 0.0;
                continue;
            }
            self.attack_ramp = (self.attack_ramp + 1.0).min(attack_frames);
            let shape = self.attack_ramp / attack_frames;
            self.hp_lp += self.hp_a * (self.noise.next_f32() - self.hp_lp);
            let s = (self.noise.next_f32() * 0.5 + self.hp_lp * 0.4 - self.hp_lp) * shape * self.env;
            self.env = flush_denormal(self.env * self.env_coeff);
            *slot = s * 2.2;
            if self.env < SILENCE_ABS {
                self.active = false;
            }
        }
    }

    fn set_param(&mut self, param: ParamId, value: f32) {
        use kontinuum_core::params::*;
        match param {
            SHAKER_DECAY_MS => {
                self.decay_ms = value.clamp(20.0, 600.0);
                self.update_coeffs();
            }
            SHAKER_TONE => {
                self.tone = value.clamp(0.0, 1.0);
                self.update_coeffs();
            }
            _ => {}
        }
    }

    fn reset(&mut self) {
        *self = Self::new(self.sr as u32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kontinuum_core::BLOCK_FRAMES;

    const SR: u32 = 48_000;

    fn render_clap(velocity: f32) -> Vec<f32> {
        let mut clap = Clap::new(SR);
        clap.note_on(60.0, velocity);
        let mut buf = [0.0f32; BLOCK_FRAMES];
        let mut out = Vec::new();
        while clap.is_active() {
            clap.render(&mut buf);
            out.extend_from_slice(&buf);
        }
        out
    }

    fn mean_power(x: &[f32], from_frame: usize, to_frame: usize) -> f32 {
        let w = &x[from_frame..to_frame];
        w.iter().map(|v| v * v).sum::<f32>() / w.len() as f32
    }

    fn frame_at(ms: f32) -> usize {
        (ms * 0.001 * SR as f32) as usize
    }

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
    fn clap_lifecycle_peak_and_silent_tail() {
        let mut clap = Clap::new(SR);
        assert!(!clap.is_active());
        let x = render_clap(1.0);
        assert!(!clap.is_active());
        let peak = x.iter().fold(0.0f32, |m, &v| m.max(v.abs()));
        assert!(peak > 0.05 && peak < 2.0, "clap peak out of range: {peak}");
        let mut tail = [1.0f32; BLOCK_FRAMES];
        clap.render(&mut tail);
        assert!(tail.iter().all(|&s| s == 0.0), "tail must be exactly zero");
    }

    #[test]
    fn clap_is_deterministic() {
        let ra = render_clap(0.9);
        let rb = render_clap(0.9);
        assert_eq!(ra.len(), rb.len());
        assert!(ra.iter().zip(rb.iter()).all(|(x, y)| x.to_bits() == y.to_bits()));
    }

    /// The three ~11 ms pre-echoes into a longer tail are the clap's
    /// fingerprint and must survive the new bandpass: short-window power must
    /// re-energize at ~11 and ~22 ms against the decayed valley before each.
    #[test]
    fn clap_keeps_three_pre_echoes_and_long_tail() {
        let x = render_clap(1.0);
        assert!(x.len() as f32 > 0.15 * SR as f32, "tail too short for a clap");

        let burst2 = mean_power(&x, frame_at(11.0), frame_at(12.5));
        let valley2 = mean_power(&x, frame_at(7.5), frame_at(10.5));
        assert!(burst2 > valley2 * 1.4, "second pre-echo buried: {burst2} vs {valley2}");

        let burst3 = mean_power(&x, frame_at(22.0), frame_at(23.5));
        let valley3 = mean_power(&x, frame_at(18.5), frame_at(21.5));
        assert!(burst3 > valley3 * 1.4, "third pre-echo buried: {burst3} vs {valley3}");
    }

    /// Band centre proven by narrowband probe-power ratios on the tail window
    /// (Q=4 probes): energy at 1100 Hz must dominate bands well clear on both
    /// sides. Zero-crossing spacing is deliberately not used here: on noise it
    /// estimates the f^2-weighted spectral moment (measured ~8.5 samples for
    /// this band), not the spectral peak — it stays valid for the pure-tone
    /// ring tests above. A broadband tail scores ~0.36 / ~3.2 on these ratios.
    #[test]
    fn clap_tail_energy_is_concentrated_at_the_bandpass_center() {
        let x = render_clap(1.0);
        let tail = &x[x.len() * 2 / 5..];
        let probe = |f0: f32| {
            let mut b = Biquad::new();
            b.set_bandpass(SR as f32, f0, 4.0);
            tail.iter().map(|&v| { let y = b.tick(v); y * y }).sum::<f32>()
        };
        let center = probe(1100.0);
        assert!(center > 0.0);
        assert!(probe(400.0) / center < 0.15, "tail leaks below the band");
        assert!(probe(3500.0) / center < 0.8, "tail leaks above the band");
        assert!(probe(800.0) / center > 0.2, "no in-band content near the centre");
    }

    /// Resonance is provable on the filter itself: an impulse into the biquad
    /// rings for ~Q/(pi·f0) seconds, so higher Q must ring far longer, and the
    /// ring must oscillate at the centre frequency (spacing ~ sr/(2·f0)).
    #[test]
    fn biquad_bandpass_rings_longer_at_higher_q() {
        let ring_samples_above = |q: f32| {
            let mut bq = Biquad::new();
            bq.set_bandpass(SR as f32, 1100.0, q);
            let mut count = 0usize;
            let mut y = bq.tick(1.0);
            for _ in 0..SR as usize {
                if y.abs() > 1e-3 {
                    count += 1;
                }
                y = bq.tick(0.0);
            }
            count
        };
        let short = ring_samples_above(0.5);
        let long = ring_samples_above(8.0);
        assert!(long > 200, "Q=8 does not resonate: {long} samples");
        assert!(long > 4 * short, "Q does not lengthen the ring: {short} vs {long}");
    }

    #[test]
    fn biquad_bandpass_rings_at_center_frequency() {
        let mut bq = Biquad::new();
        bq.set_bandpass(SR as f32, 1100.0, 4.0);
        let mut ring = Vec::new();
        let mut y = bq.tick(1.0);
        for _ in 0..250 {
            ring.push(y);
            y = bq.tick(0.0);
        }
        let spacing = mean_zero_crossing_spacing(&ring);
        let expected = SR as f32 / (2.0 * 1100.0);
        assert!(
            (spacing - expected).abs() / expected < 0.12,
            "ring not at centre: spacing {spacing:.1}, expected {expected:.1}"
        );
    }

    #[test]
    fn clap_params_clamp_to_bounds() {
        let render_with = |param: ParamId, value: f32| {
            let mut c = Clap::new(SR);
            c.set_param(param, value);
            c.note_on(60.0, 1.0);
            let mut buf = [0.0f32; BLOCK_FRAMES];
            let mut out = Vec::new();
            while c.is_active() {
                c.render(&mut buf);
                out.extend_from_slice(&buf);
            }
            out
        };
        let cases = [
            (kontinuum_core::params::CLAP_DECAY_MS, 99_999.0, 1500.0),
            (kontinuum_core::params::CLAP_TONE, 9.0, 1.0),
            (CLAP_CENTER_HZ, -50.0, 400.0),
            (CLAP_RESONANCE_Q, 100.0, 8.0),
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
}
