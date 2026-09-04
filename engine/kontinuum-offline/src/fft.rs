//! FFT-partitioned linear-phase master EQ for the premium render (#28).
//!
//! The real-time chain corrects spectral tilt with RBJ shelf biquads
//! ([`kontinuum_mastering::tilt::TiltEq`]); minimum-phase biquads smear
//! transients across the shelf's transition band. The premium path makes
//! the *same* tilt move (mirrored low/high shelf pair, pivot and gain
//! from the targets file, magnitude-identical shelf shapes) as a
//! **linear-phase FIR**: the digital magnitude response of the same two
//! RBJ shelves is sampled onto an FFT grid, and the inverse transform of
//! that zero-phase (real, even) spectrum is a symmetric impulse response
//! — perfectly linear phase, pre-ring included by design.
//!
//! FIR length / partition size tradeoff (48 kHz; offline latency is
//! free, cost is not):
//! - [`EQ_TAPS`] = 4096 (85 ms) sets the design resolution: 11.8 Hz bins,
//!   plenty for a ±3 dB shelf pivoting around the targets' ~700 Hz; a
//!   shorter FIR would quantize the shelf's low-frequency skirt.
//! - [`EQ_PARTITION`] = 1024 divides the FIR into 4 uniform partitions
//!   convolved overlap-save with 2048-point FFTs. Each input block costs
//!   one forward + one inverse FFT per channel regardless of partition
//!   count; 1024 keeps the frequency-domain delay line (4 × 2048 bins)
//!   tiny. A larger partition would only cut per-block overhead — not
//!   worth the scratch memory; a smaller one wastes FFT efficiency.
//! - Streaming latency is the FIR center delay (2048 frames ≈ 43 ms);
//!   `process` returns an input-aligned buffer, so it only costs scratch,
//!   never a monitoring problem.
//!
//! All arithmetic is f64 in a fixed operation order — bit-deterministic
//! per build, no `unsafe`, no RNG.

/// Linear-phase FIR length in taps (85 ms at 48 kHz).
pub const EQ_TAPS: usize = 4096;
/// Uniform partition size in frames (21 ms at 48 kHz).
pub const EQ_PARTITION: usize = 1024;
/// FIR center delay the EQ inserts; `process` compensates it.
pub const EQ_LATENCY_FRAMES: usize = EQ_TAPS / 2;

/// Complex sample for the FFT (own type — no external deps).
#[derive(Clone, Copy, Debug, Default)]
struct Cx {
    re: f64,
    im: f64,
}

impl Cx {
    fn mul(self, o: Cx) -> Cx {
        Cx { re: self.re * o.re - self.im * o.im, im: self.re * o.im + self.im * o.re }
    }
    fn conj(self) -> Cx {
        Cx { re: self.re, im: -self.im }
    }
}

/// Iterative radix-2 FFT with precomputed twiddles and bit-reversal
/// table. `n` is a crate constant (2 × [`EQ_PARTITION`]); the
/// constructor asserts it because every caller shares that constant.
struct Fft {
    n: usize,
    /// tw[k] = e^{-i·2πk/n} for the forward transform.
    tw: Vec<Cx>,
    rev: Vec<usize>,
}

impl Fft {
    fn new(n: usize) -> Self {
        assert!(n.is_power_of_two(), "FFT size must be a power of two");
        let mut tw = Vec::with_capacity(n);
        for k in 0..n {
            let a = -std::f64::consts::TAU * k as f64 / n as f64;
            tw.push(Cx { re: a.cos(), im: a.sin() });
        }
        let bits = n.trailing_zeros();
        let rev = (0..n).map(|i| i.reverse_bits() >> (usize::BITS - bits)).collect();
        Fft { n, tw, rev }
    }

    /// In-place forward DIT transform; `buf.len()` must equal `n`.
    fn forward(&self, buf: &mut [Cx]) {
        for (i, &r) in self.rev.iter().enumerate() {
            if i < r {
                buf.swap(i, r);
            }
        }
        let mut len = 2;
        while len <= self.n {
            let half = len / 2;
            let stride = self.n / len;
            for start in (0..self.n).step_by(len) {
                for j in 0..half {
                    let w = self.tw[j * stride];
                    let u = buf[start + j];
                    let v = buf[start + j + half].mul(w);
                    buf[start + j] = Cx { re: u.re + v.re, im: u.im + v.im };
                    buf[start + j + half] = Cx { re: u.re - v.re, im: u.im - v.im };
                }
            }
            len *= 2;
        }
    }

    /// In-place inverse transform (conjugate trick, 1/n scaled).
    fn inverse(&self, buf: &mut [Cx]) {
        for c in buf.iter_mut() {
            *c = c.conj();
        }
        self.forward(buf);
        let scale = 1.0 / self.n as f64;
        for c in buf.iter_mut() {
            let re = c.re * scale;
            *c = Cx { re, im: -c.im * scale }.conj();
        }
    }
}

/// Magnitude of a second-order section `b0 + b1·z⁻¹ + b2·z⁻²` at
/// z = e^{jw}, with z⁻¹ = co − j·si.
fn poly2_mag(b0: f64, b1: f64, b2: f64, co: f64, si: f64) -> f64 {
    let re = b0 + b1 * co + b2 * (co * co - si * si);
    let im = -(b1 * si + b2 * 2.0 * co * si);
    (re * re + im * im).sqrt()
}

/// Digital magnitude of an RBJ biquad (`[b0, b1, b2, a1, a2]`, a0 = 1).
fn biquad_mag(c: &[f64; 5], w: f64) -> f64 {
    let (si, co) = w.sin_cos();
    let num = poly2_mag(c[0], c[1], c[2], co, si);
    let den = poly2_mag(1.0, c[3], c[4], co, si);
    num / den.max(1e-30)
}

/// Stereo-linked linear-phase tilt EQ. Built from the targets file's
/// pivot/tilt (same mapping as `MasteringChain::new_with_targets`), so
/// premium and RT renders make the same corrective move; only the phase
/// behavior differs (this one is linear by construction).
pub struct LinearPhaseTiltEq {
    /// Frequency-domain FIR partitions, K × fft_n, taps starting at bin 0.
    partitions: Vec<Vec<Cx>>,
    /// Processing transform (2 × [`EQ_PARTITION`] points).
    fft: Fft,
}

impl LinearPhaseTiltEq {
    /// Design the EQ for `sample_rate`, tilting by `tilt_db` around
    /// `pivot_hz` (positive brightens). The tilt is clamped to the RT
    /// chain's ±3 dB bound; non-finite input falls back to neutral.
    pub fn new(sample_rate: u32, pivot_hz: f64, tilt_db: f32) -> Self {
        let g = if tilt_db.is_finite() { tilt_db } else { 0.0 }
            .clamp(-kontinuum_mastering::tilt::TILT_MAX_DB, kontinuum_mastering::tilt::TILT_MAX_DB)
            as f64;
        let sr = sample_rate as f64;
        let f0 = pivot_hz.max(20.0).min(sr * 0.45);
        // Same shelf shapes as the RT tilt (tilt.rs): mirrored pair, Q 0.707.
        let q = 0.707;
        let low = kontinuum_mastering::filters::low_shelf_coeffs(sr, f0, -g, q);
        let high = kontinuum_mastering::filters::high_shelf_coeffs(sr, f0, g, q);

        // Zero-phase spectrum: sample the shelf-pair magnitude onto the
        // FFT grid (real and even by construction — w and 2π−w agree).
        let mut spec = vec![Cx::default(); EQ_TAPS];
        for (k, s) in spec.iter_mut().enumerate() {
            let w = std::f64::consts::TAU * k as f64 / EQ_TAPS as f64;
            s.re = biquad_mag(&low, w) * biquad_mag(&high, w);
        }
        // Multiply by (−1)^k: circular time shift by n/2, centering the
        // symmetric impulse response at tap n/2 (linear-phase delay).
        for (k, s) in spec.iter_mut().enumerate() {
            if k & 1 == 1 {
                s.re = -s.re;
            }
        }
        let fft = Fft::new(EQ_TAPS);
        fft.inverse(&mut spec);

        // Split the centered impulse response into uniform partitions and
        // pre-transform each into the frequency domain. The processing
        // transform is a different size (2 × partition) than the design
        // transform (EQ_TAPS points) — hence the second instance.
        let mut partitions = Vec::with_capacity(EQ_TAPS / EQ_PARTITION);
        let proc_fft = Fft::new(2 * EQ_PARTITION);
        for part in spec.chunks(EQ_PARTITION) {
            let mut block = vec![Cx::default(); 2 * EQ_PARTITION];
            for (i, h) in part.iter().enumerate() {
                block[i] = *h;
            }
            proc_fft.forward(&mut block);
            partitions.push(block);
        }
        LinearPhaseTiltEq { partitions, fft: proc_fft }
    }

    /// Filter a stereo buffer, returning an output aligned 1:1 with the
    /// input (FIR center delay compensated; the response to the signal
    /// ending is rendered by zero-padding the tail).
    pub fn process(&self, left: &[f32], right: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let n = left.len().min(right.len());
        let mut out_l = vec![0.0f32; n];
        let mut out_r = vec![0.0f32; n];
        if n == 0 {
            return (out_l, out_r);
        }
        let p = EQ_PARTITION;
        let fft_n = 2 * p;
        let k_count = self.partitions.len();
        // The zero-phase-aligned output[i] is the raw convolution value at
        // i + n/2, so half a FIR length of silent tail must be convolved.
        let blocks = (n + EQ_LATENCY_FRAMES + p - 1) / p;
        let mut out_os_l = vec![0.0f64; blocks * p];
        let mut out_os_r = vec![0.0f64; blocks * p];
        // Frequency-domain delay line of the [prev | cur] input windows;
        // partition k pairs with the window from k blocks ago.
        let mut hist_l: Vec<Vec<Cx>> =
            (0..k_count).map(|_| vec![Cx::default(); fft_n]).collect();
        let mut hist_r: Vec<Vec<Cx>> =
            (0..k_count).map(|_| vec![Cx::default(); fft_n]).collect();
        let mut win = vec![Cx::default(); fft_n];
        let mut acc = vec![Cx::default(); fft_n];

        for b in 0..blocks {
            for (hist, input) in [(&mut hist_l, left), (&mut hist_r, right)] {
                for slot in win.iter_mut() {
                    *slot = Cx::default();
                }
                // Overlap-save window = [previous block | current block];
                // its second half then yields exactly y[bP .. bP + P).
                for i in 0..p {
                    let prev = b * p + i;
                    if prev >= p && prev < n + p {
                        win[i].re = input[prev - p] as f64;
                    }
                    if prev < n {
                        win[p + i].re = input[prev] as f64;
                    }
                }
                self.fft.forward(&mut win);
                hist[b % k_count].copy_from_slice(&win);
            }

            for slot in acc.iter_mut() {
                *slot = Cx::default();
            }
            for (k, part) in self.partitions.iter().enumerate() {
                let past = (b + k_count - k) % k_count;
                for (i, slot) in acc.iter_mut().enumerate() {
                    let term = part[i].mul(hist_l[past][i]);
                    slot.re += term.re;
                    slot.im += term.im;
                }
            }
            self.fft.inverse(&mut acc);
            for (i, dst) in out_os_l[b * p..(b + 1) * p].iter_mut().enumerate() {
                *dst = acc[p + i].re;
            }

            // Same accumulation for the right channel (partitions shared).
            for slot in acc.iter_mut() {
                *slot = Cx::default();
            }
            for (k, part) in self.partitions.iter().enumerate() {
                let past = (b + k_count - k) % k_count;
                for (i, slot) in acc.iter_mut().enumerate() {
                    let term = part[i].mul(hist_r[past][i]);
                    slot.re += term.re;
                    slot.im += term.im;
                }
            }
            self.fft.inverse(&mut acc);
            for (i, dst) in out_os_r[b * p..(b + 1) * p].iter_mut().enumerate() {
                *dst = acc[p + i].re;
            }
        }

        for i in 0..n {
            out_l[i] = out_os_l[i + EQ_LATENCY_FRAMES] as f32;
            out_r[i] = out_os_r[i + EQ_LATENCY_FRAMES] as f32;
        }
        (out_l, out_r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_roundtrips_an_impulse() {
        let fft = Fft::new(16);
        let mut buf = vec![Cx::default(); 16];
        buf[3].re = 2.0;
        let orig = buf.clone();
        fft.forward(&mut buf);
        // Impulse → unit-magnitude spectrum (rotating phase).
        for c in &buf {
            let mag = (c.re * c.re + c.im * c.im).sqrt();
            assert!((mag - 2.0).abs() < 1e-12, "flat spectrum magnitude: {mag}");
        }
        fft.inverse(&mut buf);
        for (a, b) in buf.iter().zip(orig.iter()) {
            assert!((a.re - b.re).abs() < 1e-12 && (a.im - b.im).abs() < 1e-12);
        }
    }

    #[test]
    fn eq_impulse_response_is_symmetric_pre_ring_included() {
        // An impulse through the EQ must come back as the (centered,
        // symmetric) linear-phase FIR itself: out[D − i] == out[D + i].
        let sr = 48_000u32;
        let eq = LinearPhaseTiltEq::new(sr, 700.0, 2.0);
        let (peak_idx, len) = (4096, 16_384);
        let impulse: Vec<f32> =
            (0..len).map(|i| if i == peak_idx { 1.0 } else { 0.0 }).collect();
        let (out_l, _) = eq.process(&impulse, &impulse);

        let peak = out_l[peak_idx] as f64;
        assert!(peak > 0.0, "impulse must survive: {peak}");
        // Tails far from the shelf's transition pre-ring just as long as
        // they ring after — that symmetry IS linear phase.
        let tol = peak.abs() * 1e-6 + 1e-12;
        for i in 1..=1500 {
            let pre = out_l[peak_idx - i] as f64;
            let post = out_l[peak_idx + i] as f64;            assert!(
                (pre - post).abs() <= tol,
                "asymmetric at ±{i}: pre {pre:e} vs post {post:e}"
            );
        }
    }

    #[test]
    fn eq_tilt_is_clamped_and_neutral_passes_dc() {
        // The FIR must absorb the same ±3 dB bound the RT chain enforces;
        // verified through the DC gain on a long constant input. The mean
        // excludes the final FIR length where the window slides past the
        // end of the constant.
        let sr = 48_000u32;
        let neutral = LinearPhaseTiltEq::new(sr, 700.0, 0.0);
        let ones = vec![1.0f32; sr as usize];
        let (out, _) = neutral.process(&ones, &ones);
        let settled = &out[out.len() / 2..out.len() - EQ_LATENCY_FRAMES];
        let mean = settled.iter().map(|s| *s as f64).sum::<f64>() / settled.len() as f64;
        assert!((mean - 1.0).abs() < 1e-6, "neutral tilt must pass DC: {mean}");

        let dark = LinearPhaseTiltEq::new(sr, 700.0, -50.0);
        let (out, _) = dark.process(&ones, &ones);
        let settled = &out[out.len() / 2..out.len() - EQ_LATENCY_FRAMES];
        let mean = settled.iter().map(|s| *s as f64).sum::<f64>() / settled.len() as f64;
        // Mirrored tilt pair: a −3 dB tilt boosts DC by +3 dB (and cuts
        // the top octave by −3) — same as the RT TiltEq at full negative.
        let expect = 10.0f64.powf(3.0 / 20.0);
        assert!((mean - expect).abs() < 5e-4, "−50 dB request → +3 dB DC: {mean} vs {expect}");
    }
}
