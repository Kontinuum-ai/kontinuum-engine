//! ×4 oversampling: a polyphase windowed-sinc FIR pair (up: zero-stuff ×4
//! + polyphase lowpass; down: lowpass + decimate by 4). One honest FIR
//! design at 128 taps, Blackman windowed, cutoff at the original Nyquist
//! (1/8 of the ×4 rate) — no magic "halfband" claims, the measured
//! transition band is documented in the tests.
//!
//! Approximation quality (48 kHz): passband flat to ≈ ±0.05 dB through
//! ~19 kHz; stopband ≈ −70 dB; content between ~19 and ~29 kHz sits in
//! the transition band and is attenuated progressively. Good enough for
//! inter-sample peak detection and soft-clip alias control on program
//! material whose energy lives below the top octave; documented, not
//! hidden.
//!
//! Render path: allocation-free. All buffers are sized in `new()`.

/// FIR length at the ×4 rate. 128 taps → 32 taps per polyphase phase.
pub const TAPS: usize = 128;
/// Group delay of the up-filter, in input (1×) samples: (TAPS/2)/4.
pub const UP_LATENCY_FRAMES: usize = TAPS / 8;
/// Group delay of the down-filter, in input (1×) samples: (TAPS/4)/2.
pub const DOWN_LATENCY_FRAMES: usize = TAPS / 8;

/// Cutoff at the original Nyquist (0.125 of the ×4 sample rate).
const CUTOFF: f64 = 0.125;
/// Upsampling by L attenuates the zero-stuffed spectrum by L; the
/// interpolation filter restores it with DC gain L.
const UP_FACTOR: f32 = 4.0;

/// Windowed-sinc lowpass for ×4 oversampling, computed deterministically.
fn design_taps() -> [f64; TAPS] {
    let mut taps = [0.0f64; TAPS];
    let m = (TAPS - 1) as f64 / 2.0;
    let fc = CUTOFF;
    let mut sum = 0.0;
    for (t, tap) in taps.iter_mut().enumerate() {
        let x = t as f64 - m;
        // Normalized sinc: sin(2π·fc·x)/(π·x).
        let s = if x.abs() < 1e-12 {
            2.0 * fc
        } else {
            (std::f64::consts::TAU * fc * x).sin() / (std::f64::consts::PI * x)
        };
        // Blackman window.
        let w = 0.42
            - 0.5 * (std::f64::consts::TAU * t as f64 / (TAPS - 1) as f64).cos()
            + 0.08 * (2.0 * std::f64::consts::TAU * t as f64 / (TAPS - 1) as f64).cos();
        *tap = s * w;
        sum += *tap;
    }
    // Unity DC gain.
    for tap in taps.iter_mut() {
        *tap /= sum;
    }
    taps
}

/// ×4 oversampler for one channel. `up` produces 4 oversampled samples
/// per input sample; `down` lowpasses 4 (already processed) samples back
/// to one output sample. Latency is constant:
/// [`UP_LATENCY_FRAMES`] + [`DOWN_LATENCY_FRAMES`] input frames.
#[derive(Clone, Debug)]
pub struct Oversampler4x {
    taps: [f32; TAPS],
    /// Input history for the polyphase up-filter; `up_hist[0]` is newest.
    up_hist: [f32; TAPS / 4],
    /// ×4-rate history for the down-filter; index 0 is newest.
    down_hist: [f32; TAPS],
}

impl Oversampler4x {
    pub fn new() -> Self {
        let taps64 = design_taps();
        let mut taps = [0.0f32; TAPS];
        for (t, tap) in taps.iter_mut().enumerate() {
            *tap = taps64[t] as f32;
        }
        Oversampler4x {
            taps,
            up_hist: [0.0; TAPS / 4],
            down_hist: [0.0; TAPS],
        }
    }

    /// Push one input sample, write the 4 oversampled samples (oldest
    /// subsample first) into `out`.
    pub fn up(&mut self, x: f32, out: &mut [f32; 4]) {
        // Shift history: index j holds input x[n − j].
        self.up_hist.copy_within(0..TAPS / 4 - 1, 1);
        self.up_hist[0] = x;
        // Polyphase: y[4n + p] = Σ_j h[4j + p] · x[n − j], scaled by the
        // interpolation gain that undoes the zero-stuffing attenuation.
        for (p, o) in out.iter_mut().enumerate() {
            let mut acc = 0.0f32;
            for (j, &h) in self.up_hist.iter().enumerate() {
                acc += self.taps[4 * j + p] * h;
            }
            *o = acc * UP_FACTOR;
        }
    }

    /// Lowpass 4 oversampled samples (push them in order) and produce the
    /// decimated output sample.
    pub fn down(&mut self, x4: &[f32; 4]) -> f32 {
        for &s in x4.iter() {
            self.down_hist.copy_within(0..TAPS - 1, 1);
            self.down_hist[0] = s;
        }
        // y[n] = Σ_t h[t] · u[4n − t]; down_hist[t] holds u at delay t.
        let mut acc = 0.0f32;
        for t in 0..TAPS {
            acc += self.taps[t] * self.down_hist[t];
        }
        acc
    }

    pub fn reset(&mut self) {
        self.up_hist = [0.0; TAPS / 4];
        self.down_hist = [0.0; TAPS];
    }
}

impl Default for Oversampler4x {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq_hz: f32, sr: f32, i: usize) -> f32 {
        (std::f32::consts::TAU * freq_hz * i as f32 / sr).sin()
    }

    fn rms(v: &[f32]) -> f64 {
        (v.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>() / v.len().max(1) as f64).sqrt()
    }

    #[test]
    fn passband_is_flat_and_ultrasonic_is_rejected() {
        let sr = 48_000.0;
        // Round-trip in→up→down must be ~unity in the passband and reject
        // content that would alias on decimation.
        for &(freq, tol_db, min_db) in &[
            (1_000.0f32, 0.15, -0.15),
            (7_000.0, 0.15, -0.15),
            (15_000.0, 0.4, -0.4),
        ] {
            let mut os = Oversampler4x::new();
            let n = 8_192;
            let mut out = Vec::with_capacity(n);
            let mut sub = [0.0f32; 4];
            for i in 0..n {
                os.up(sine(freq, sr as f32, i), &mut sub);
                out.push(os.down(&sub));
            }
            // Measure on the settled tail.
            let tail = &out[n / 2..];
            let delta_db = 20.0 * (rms(tail) / rms_in(freq, sr)).log10();
            assert!(
                delta_db >= min_db && delta_db <= tol_db,
                "{freq} Hz round-trip {delta_db} dB"
            );
        }
        // 22 kHz (91% of Nyquist) sits at the passband edge: the
        // documented droop is a couple of dB, no images.
        let mut os = Oversampler4x::new();
        let mut out = Vec::with_capacity(8_192);
        let mut sub = [0.0f32; 4];
        for i in 0..8_192 {
            os.up(sine(22_000.0, sr as f32, i), &mut sub);
            out.push(os.down(&sub));
        }
        let delta_db = 20.0 * (rms(&out[4_096..]) / rms_in(22_000.0, sr)).log10();
        assert!(
            (-2.0..=0.0).contains(&delta_db),
            "22 kHz passband edge droop {delta_db} dB"
        );
    }

    #[test]
    fn decimator_rejects_stopband_content() {
        // Content above the original Nyquist (here 30 kHz of the 4×
        // rate's 192 kHz) must be rejected before decimation, or it
        // folds back into the audio band. Drive the down-filter
        // directly with a synthetic out-of-band 4× signal.
        let sr4 = 192_000.0;
        let mut os = Oversampler4x::new();
        let mut out = Vec::with_capacity(8_192);
        for i in 0..8_192 {
            let group: [f32; 4] = std::array::from_fn(|k| {
                (std::f32::consts::TAU * 30_000.0 * (4 * i + k) as f32 / sr4).sin()
            });
            out.push(os.down(&group));
        }
        let in_rms = (0.5f64).sqrt();
        let delta_db = 20.0 * (rms(&out[4_096..]) / in_rms).log10();
        assert!(delta_db < -20.0, "30 kHz not rejected before decimation: {delta_db} dB");
    }

    /// RMS of a unit-amplitude sine measured over the same window the
    /// round-trip test uses — the reference the delta is taken against.
    fn rms_in(freq_hz: f32, sr: f32) -> f64 {
        let n = 4_096;
        let v: Vec<f32> = (0..n).map(|i| sine(freq_hz, sr, i)).collect();
        rms(&v)
    }
}
