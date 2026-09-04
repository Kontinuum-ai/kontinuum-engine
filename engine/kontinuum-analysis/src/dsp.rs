//! Real-time-safe analysis primitives for the self-listening critic
//! (issue #25): slot storage, rolling spectra, transient flux and a
//! causal peak estimator. Filter primitives live in `crate::filters`.
//! All state is preallocated at construction: `push` paths allocate
//! nothing.

use crate::fft::{hanning, next_pow2, power_spectrum};
use crate::metrics::BANDS;

/// Fixed-capacity ring of per-slot f64 accumulations; oldest retained
/// value is index 0 of `tail`/`iter`.
pub struct SlotRing {
    data: Vec<f64>,
    pos: usize,
    filled: usize,
}

impl SlotRing {
    pub fn new(capacity: usize) -> Self {
        SlotRing { data: vec![0.0; capacity], pos: 0, filled: 0 }
    }

    pub fn push(&mut self, v: f64) {
        self.data[self.pos] = v;
        self.pos = (self.pos + 1) % self.data.len();
        if self.filled < self.data.len() {
            self.filled += 1;
        }
    }

    pub fn len(&self) -> usize {
        self.filled
    }

    pub fn is_empty(&self) -> bool {
        self.filled == 0
    }

    /// Chronological value; 0 = oldest retained.
    pub fn at(&self, c: usize) -> f64 {
        let cap = self.data.len();
        let start = (self.pos + cap - self.filled) % cap;
        self.data[(start + c) % cap]
    }

    /// Last `n` retained values in chronological order.
    pub fn tail(&self, n: usize) -> impl Iterator<Item = f64> + '_ {
        let from = self.filled.saturating_sub(n);
        (from..self.filled).map(move |c| self.at(c))
    }

    pub fn iter(&self) -> impl Iterator<Item = f64> + '_ {
        self.tail(self.filled)
    }
}

/// Rolling windowed spectrum over a mono feed: spectral centroid,
/// least-squares spectral tilt (dB/octave across the shared band plan)
/// and sub-band (20–60 Hz) energy share. One FFT per full window,
/// computed inside `push` on the analysis thread (≈ sr/window times per
/// second).
pub struct SpectralTracker {
    sr_hz: f64,
    window: usize,
    bins: usize,
    buf: Vec<f64>,
    pos: usize,
    win: Vec<f64>,
    re: Vec<f64>,
    im: Vec<f64>,
    /// Power-weighted centroid (Hz); 0.0 before the first full window.
    pub centroid_hz: f64,
    /// Least-squares slope of 10·log10(power) vs log2(frequency).
    pub tilt_db_per_oct: f64,
    /// Share of total power in 20–60 Hz.
    pub sub_share: f64,
}

const TILT_FLOOR_DB: f64 = 45.0;
const SUB_LO_HZ: f64 = 20.0;
const SUB_HI_HZ: f64 = 60.0;

impl SpectralTracker {
    /// `window` must be a power of two (radix-2 FFT).
    pub fn new(sample_rate: u32, window: usize) -> Self {
        let padded = next_pow2(window);
        let bins = padded / 2;
        SpectralTracker {
            sr_hz: sample_rate as f64,
            window: padded,
            bins,
            buf: vec![0.0; padded],
            pos: 0,
            win: hanning(padded),
            re: vec![0.0; padded],
            im: vec![0.0; padded],
            centroid_hz: 0.0,
            tilt_db_per_oct: 0.0,
            sub_share: 0.0,
        }
    }

    /// Feed one mono sample. Allocates nothing; recomputes the spectrum
    /// once per `window` samples (when the rolling buffer wraps).
    pub fn push(&mut self, mono: f64) {
        self.buf[self.pos] = mono;
        self.pos += 1;
        if self.pos == self.window {
            self.pos = 0;
            self.recompute();
        }
    }

    fn recompute(&mut self) {
        power_spectrum(&self.buf, &self.win, &mut self.re, &mut self.im);
        let mut total = 0.0f64;
        let mut weighted = 0.0f64;
        let mut sub = 0.0f64;
        // Band powers over the shared plan (crate::metrics::BANDS) for the
        // tilt fit: raw per-bin fits are dominated by Hann leakage decay,
        // aggregated bands are not.
        let mut band_power = [0.0f64; BANDS.len()];
        for k in 0..self.bins {
            let f = k as f64 * self.sr_hz / self.window as f64;
            let p = self.re[k] * self.re[k] + self.im[k] * self.im[k];
            total += p;
            weighted += f * p;
            if (SUB_LO_HZ..SUB_HI_HZ).contains(&f) {
                sub += p;
            }
            for (i, &(_, lo, hi)) in BANDS.iter().enumerate() {
                if f >= lo && f < hi {
                    band_power[i] += p;
                    break;
                }
            }
        }
        self.centroid_hz = if total > 0.0 { weighted / total } else { 0.0 };
        self.sub_share = if total > 0.0 { sub / total } else { 0.0 };
        self.tilt_db_per_oct = band_tilt(&band_power);
    }
}

/// Least-squares spectral tilt (dB/octave) over the band plan: band level
/// 10·log10(power) vs log2(geometric center frequency), bands within
/// −45 dB of the loudest band only, 0.0 when fewer than two bands carry
/// enough energy (sparse spectra have no meaningful slope).
fn band_tilt(band_power: &[f64]) -> f64 {
    let levels: Vec<f64> =
        band_power.iter().map(|p| 10.0 * (p + 1e-30).log10()).collect();
    let max = levels.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let mut sx = 0.0;
    let mut sy = 0.0;
    let mut sxx = 0.0;
    let mut sxy = 0.0;
    let mut count = 0.0;
    for (i, level) in levels.iter().enumerate() {
        if *level < max - TILT_FLOOR_DB {
            continue;
        }
        let (_, lo, hi) = BANDS[i];
        let x = 0.5 * (lo * hi).log2();
        sx += x;
        sy += *level;
        sxx += x * x;
        sxy += x * *level;
        count += 1.0;
    }
    let denom = count * sxx - sx * sx;
    if count >= 2.0 && denom.abs() > 1e-9 {
        (count * sxy - sx * sy) / denom
    } else {
        0.0
    }
}

/// Spectral-flux transient detector — same shape as `metrics::analyze`
/// (1024-pt Hann window on a 512 hop, flux summed above 3 kHz, peaks over
/// mean + 1.5σ of the trailing flux ring with a noise floor at 10⁻³ of
/// the session max magnitude). Rolling variant: the last `FLUX_WINDOW`
/// samples are re-analysed every `FLUX_HOP` pushes.
pub struct FluxTracker {
    sr_hz: f64,
    buf: Vec<f64>,
    pos: usize,
    since_hop: usize,
    win: Vec<f64>,
    re: Vec<f64>,
    im: Vec<f64>,
    prev_mag: Vec<f64>,
    have_prev: bool,
    max_mag: f64,
    flux: SlotRing,
}

const FLUX_WINDOW: usize = 1024;
const FLUX_HOP: usize = 512;
const FLUX_MIN_HZ: f64 = 3000.0;

impl FluxTracker {
    /// `ring` = how many hop results the trailing statistics cover.
    pub fn new(sample_rate: u32, ring: usize) -> Self {
        let bins = FLUX_WINDOW / 2;
        FluxTracker {
            sr_hz: sample_rate as f64,
            buf: vec![0.0; FLUX_WINDOW],
            pos: 0,
            since_hop: 0,
            win: hanning(FLUX_WINDOW),
            re: vec![0.0; FLUX_WINDOW],
            im: vec![0.0; FLUX_WINDOW],
            prev_mag: vec![0.0; bins],
            have_prev: false,
            max_mag: 0.0,
            flux: SlotRing::new(ring),
        }
    }

    /// Feed one mono sample. Allocates nothing; one FFT per `FLUX_HOP`.
    pub fn push(&mut self, mono: f64) {
        self.buf[self.pos] = mono;
        self.pos = (self.pos + 1) % FLUX_WINDOW;
        self.since_hop += 1;
        if self.since_hop == FLUX_HOP {
            self.since_hop = 0;
            self.measure();
        }
    }

    fn measure(&mut self) {
        power_spectrum(&self.buf, &self.win, &mut self.re, &mut self.im);
        let mut flux_now = 0.0f64;
        for k in 0..self.prev_mag.len() {
            let f = k as f64 * self.sr_hz / FLUX_WINDOW as f64;
            let mag = (self.re[k] * self.re[k] + self.im[k] * self.im[k]).sqrt();
            if mag > self.max_mag {
                self.max_mag = mag;
            }
            if f > FLUX_MIN_HZ {
                if self.have_prev {
                    flux_now += (mag - self.prev_mag[k]).max(0.0);
                }
                self.prev_mag[k] = mag;
            }
        }
        if self.have_prev {
            self.flux.push(flux_now);
        }
        self.have_prev = true;
    }

    /// Peaks per second across the trailing flux ring (0.0 while cold).
    /// Rolling variant: mean + 3σ — tighter than the offline `metrics`
    /// 1.5σ because the long ring makes pure-noise local maxima a
    /// counting problem otherwise.
    pub fn transients_per_sec(&self) -> f64 {
        let n = self.flux.len();
        if n < 3 {
            return 0.0;
        }
        let flux: Vec<f64> = self.flux.iter().collect();
        let mean = flux.iter().sum::<f64>() / n as f64;
        let var = flux.iter().map(|f| (f - mean) * (f - mean)).sum::<f64>() / n as f64;
        let threshold = (mean + 3.0 * var.sqrt()).max(self.max_mag * 1e-3);
        let peaks = (1..n - 1)
            .filter(|&i| {
                flux[i] > threshold && flux[i] >= flux[i - 1] && flux[i] >= flux[i + 1]
            })
            .count();
        let seconds = n as f64 * FLUX_HOP as f64 / self.sr_hz;
        peaks as f64 / seconds.max(1e-9)
    }
}

/// Causal 4× peak estimate: cubic-Hermite interpolation over each sample
/// pair using only past samples (tangent at the segment end is a forward
/// difference). An estimate with small documented bias — mastering's
/// polyphase `Oversampler4x` stays the authoritative true-peak measure.
#[derive(Clone, Copy, Debug, Default)]
pub struct PeakProbe {
    hist: [f32; 2],
}

impl PeakProbe {
    pub fn new() -> Self {
        PeakProbe::default()
    }

    /// Feed sample `x` (= x_i); returns the largest |amplitude| among
    /// x_{i−1}, x_i and three interior interpolation points.
    pub fn push(&mut self, x: f32) -> f64 {
        let a = self.hist[0];
        let b = self.hist[1];
        let c = x;
        self.hist = [b, c];
        let m0 = (c - a) * 0.5; // central difference at x_{i−1}
        let m1 = c - b; // forward difference at x_i (causal)
        let mut peak = (b as f64).abs().max((c as f64).abs());
        for t in [0.25f64, 0.5, 0.75] {
            let (t2, t3) = (t * t, t * t * t);
            let p = (2.0 * t3 - 3.0 * t2 + 1.0) * b as f64
                + (t3 - 2.0 * t2 + t) * m0 as f64
                + (-2.0 * t3 + 3.0 * t2) * c as f64
                + (t3 - t2) * m1 as f64;
            peak = peak.max(p.abs());
        }
        peak
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(hz: f64, amp: f64, sr: u32, i: usize) -> f64 {
        amp * (std::f64::consts::TAU * hz * i as f64 / sr as f64).sin()
    }

    #[test]
    fn slot_ring_keeps_chronological_tail() {
        let mut r = SlotRing::new(4);
        for v in 0..6i32 {
            r.push(v as f64);
        }
        let got: Vec<f64> = r.iter().collect();
        assert_eq!(got, vec![2.0, 3.0, 4.0, 5.0]);
        assert_eq!(r.tail(2).collect::<Vec<_>>(), vec![4.0, 5.0]);
    }

    #[test]
    fn spectral_tracker_reads_centroid_sub_and_tilt_sign() {
        let sr = 48_000u32;
        // On-bin sub tone: 6 bins × 48000/8192 = 35.15625 Hz.
        let mut sub = SpectralTracker::new(sr, 8192);
        for i in 0..sr as usize {
            sub.push(sine(48000.0 * 6.0 / 8192.0, 0.8, sr, i));
        }
        assert!(sub.sub_share > 0.9, "35 Hz must be sub: {}", sub.sub_share);
        assert!(sub.centroid_hz < 45.0, "sub centroid {}", sub.centroid_hz);
        // A single tone is spectrally too sparse for a slope fit.
        assert_eq!(sub.tilt_db_per_oct, 0.0, "sparse spectrum must read flat");

        // Two shaped stacks: one tone at each band-plan center, amplitudes
        // stepping ±6 dB per octave of center spacing. A falling spectrum
        // (loud lows) must tilt NEGATIVE (power decreases with frequency).
        let centers = [34.64, 94.87, 244.95, 894.43, 3464.10, 9797.96];
        let cum_oct = [0.0, 1.457, 2.825, 4.695, 6.648, 8.148];
        let mut low_heavy = SpectralTracker::new(sr, 8192);
        let mut high_heavy = SpectralTracker::new(sr, 8192);
        for i in 0..sr as usize {
            let t = i as f64 / sr as f64;
            let (mut lo, mut hi) = (0.0f64, 0.0f64);
            for (k, &f) in centers.iter().enumerate() {
                let a_low = 0.5 * 2.0f64.powf(-cum_oct[k]); // falls 6 dB/oct
                let a_high = 0.5 * 2.0f64.powf(-(8.148 - cum_oct[k])); // rises
                let phase = std::f64::consts::TAU * f * t;
                lo += a_low * phase.sin();
                hi += a_high * phase.sin();
            }
            low_heavy.push(lo);
            high_heavy.push(hi);
        }
        assert!(low_heavy.tilt_db_per_oct < -4.0, "low-heavy tilt {}", low_heavy.tilt_db_per_oct);
        assert!(high_heavy.tilt_db_per_oct > 4.0, "high-heavy tilt {}", high_heavy.tilt_db_per_oct);
        assert!(low_heavy.centroid_hz < high_heavy.centroid_hz / 2.0,
            "centroids {} vs {}", low_heavy.centroid_hz, high_heavy.centroid_hz);
    }

    #[test]
    fn peak_probe_tracks_sine_amplitude() {
        let sr = 48_000u32;
        let mut p = PeakProbe::new();
        let mut max = 0.0f64;
        for i in 0..sr as usize {
            max = max.max(p.push(sine(997.0, 0.9, sr, i) as f32));
        }
        let err_db = 20.0 * (max / 0.9).log10();
        assert!(err_db.abs() < 0.2, "interpolated peak off by {err_db} dB");
    }
}
