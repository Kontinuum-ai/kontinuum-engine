//! Render metrics — the Rust port of `scripts/analysis/ab-profile.py`
//! (issue #52 WS4): band shares, spectral centroid, crest, short-term
//! dynamics, spectral-flux transient density, and per-hit strength
//! variation. Window sizes and thresholds mirror the Python exactly so the
//! two implementations can be cross-checked.
//!
//! Three definitions were corrected after the numbers they produced turned
//! out to describe the measurement rather than the music (see the comments at
//! each site): short-term dynamics is a percentile spread instead of
//! max-over-min, the centroid is magnitude-weighted instead of
//! power-weighted, and the transient noise floor is scaled by the band the
//! flux is actually measured in. Profiles derived against the old
//! definitions are not comparable with these.

use crate::dsp::PeakProbe;
use crate::fft::{hanning, next_pow2, power_spectrum};

/// Analysis bands, (label, lo Hz, hi Hz) — same as the Python.
pub const BANDS: [(&str, f64, f64); 6] = [
    ("sub", 20.0, 60.0),
    ("bass", 60.0, 150.0),
    ("lowmid", 150.0, 400.0),
    ("mid", 400.0, 2000.0),
    ("himid", 2000.0, 6000.0),
    ("high", 6000.0, 16000.0),
];

const STFT_WINDOW: usize = 8192;
const STFT_HOP: usize = 4096;
const FLUX_WINDOW: usize = 1024;
const FLUX_HOP: usize = 512;
const FLUX_MIN_HZ: f64 = 3000.0;

/// One measured render.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Metrics {
    pub rms_dbfs: f64,
    pub peak_dbfs: f64,
    /// 4× interpolated peak estimate over the whole render (dBFS) — the
    /// live critic's definition (`dsp::PeakProbe`). `peak_dbfs` is blind
    /// to inter-sample peaks; the profile ceiling guarding the export
    /// chain's −1 dBTP promise (#112) needs this one.
    #[serde(default)]
    pub true_peak_dbfs: f64,
    pub crest_db: f64,
    /// 5th-to-95th percentile spread of the 400 ms RMS windows, in dB.
    pub short_term_dyn_db: f64,
    /// Magnitude-weighted spectral centroid (librosa convention).
    pub centroid_hz: f64,
    /// Share of total spectral energy per band, 0..1, in [`BANDS`] order.
    pub band_shares: Vec<f64>,
    pub transients_per_sec: f64,
    pub hit_cv: f64,
}

impl Metrics {
    /// Analyzes a non-interleaved stereo render.
    pub fn analyze(left: &[f32], right: &[f32], sample_rate: u32) -> Metrics {
        assert_eq!(left.len(), right.len(), "stereo length mismatch");
        let n = left.len();
        let mono: Vec<f64> = left
            .iter()
            .zip(right.iter())
            .map(|(l, r)| ((*l + *r) * 0.5) as f64)
            .collect();
        let peak = left
            .iter()
            .chain(right.iter())
            .fold(0.0f64, |m, s| m.max((*s as f64).abs()));
        let rms = (mono.iter().map(|s| s * s).sum::<f64>() / n.max(1) as f64).sqrt();
        let rms_db = 20.0 * rms.max(1e-12).log10();
        let peak_db = 20.0 * peak.max(1e-12).log10();
        let crest = peak_db - rms_db;
        let mut tp_l = PeakProbe::new();
        let mut tp_r = PeakProbe::new();
        let mut tp = 0.0f64;
        for (&l, &r) in left.iter().zip(right.iter()) {
            tp = tp.max(tp_l.push(l)).max(tp_r.push(r));
        }
        let true_peak_db = 20.0 * tp.max(1e-12).log10();

        // Windowed spectra: band shares + centroid.
        let padded = next_pow2(STFT_WINDOW);
        let win = hanning(STFT_WINDOW);
        let bins = padded / 2;
        let freqs: Vec<f64> =
            (0..bins).map(|k| k as f64 * sample_rate as f64 / padded as f64).collect();
        // Two accumulators on purpose: band shares are energy shares, so they
        // are power-weighted (|X|²); the spectral centroid follows the usual
        // magnitude convention (|X|, as in librosa). Weighting the centroid by
        // power squares the low end's advantage and pins the number in the low
        // hundreds of Hz for any bass-heavy master — a ceiling on that value
        // reads as "be darker" no matter how bright the track actually is.
        let mut spec = vec![0.0f64; bins];
        let mut mag = vec![0.0f64; bins];
        let mut re = vec![0.0f64; padded];
        let mut im = vec![0.0f64; padded];
        let mut scratch = vec![0.0f64; STFT_WINDOW];
        let mut pos = 0usize;
        while pos + STFT_WINDOW <= n {
            scratch.copy_from_slice(&mono[pos..pos + STFT_WINDOW]);
            power_spectrum(&scratch, &win, &mut re, &mut im);
            for k in 0..bins {
                let p = re[k] * re[k] + im[k] * im[k];
                spec[k] += p;
                mag[k] += p.sqrt();
            }
            pos += STFT_HOP;
        }
        let total: f64 = spec.iter().sum();
        let mag_total: f64 = mag.iter().sum();
        let centroid = if mag_total > 0.0 {
            (0..bins).map(|k| freqs[k] * mag[k]).sum::<f64>() / mag_total
        } else {
            0.0
        };
        let band_shares: Vec<f64> = BANDS
            .iter()
            .map(|&(_, lo, hi)| {
                if total <= 0.0 {
                    0.0
                } else {
                    let band: f64 = (0..bins)
                        .filter(|&k| freqs[k] >= lo && freqs[k] < hi)
                        .map(|k| spec[k])
                        .sum();
                    band / total
                }
            })
            .collect();

        // Short-term dynamics: spread of the 400 ms RMS windows, measured as
        // the 5th-to-95th percentile range.
        //
        // This used to be max-over-min, which is a single-outlier statistic:
        // the quietest window in a file is almost always window 0 (the lead-in
        // before the first hit), so the number described the fade-in and not
        // the arrangement. A reference track with a silent intro scored ~60 dB
        // that way, and chasing that figure pushes a generator to scatter
        // near-silent passages through the whole record.
        let w400 = (0.4 * sample_rate as f64) as usize;
        let mut st: Vec<f64> = Vec::new();
        if w400 > 0 && n > w400 {
            let mut acc = 0.0f64;
            let mut count = 0usize;
            for &s in mono.iter() {
                acc += s * s;
                count += 1;
                if count == w400 {
                    st.push((acc / w400 as f64).sqrt());
                    acc = 0.0;
                    count = 0;
                }
            }
        }
        let dyn_db = if st.is_empty() {
            0.0
        } else {
            let mut db: Vec<f64> = st.iter().map(|v| 20.0 * v.max(1e-12).log10()).collect();
            db.sort_by(|a, b| a.partial_cmp(b).expect("RMS dB values are finite"));
            percentile(&db, 0.95) - percentile(&db, 0.05)
        };

        // Transients: spectral flux above 3 kHz, threshold mean+1.5σ, local maxima.
        let fpadded = next_pow2(FLUX_WINDOW);
        let fbins = fpadded / 2;
        let fwin = hanning(FLUX_WINDOW);
        let mut flux: Vec<f64> = Vec::new();
        let mut prev_mag = vec![0.0f64; fbins];
        let mut max_mag = 0.0f64;
        let mut hi_mag_sum = 0.0f64;
        let mut all_mag_sum = 0.0f64;
        let mut fre = vec![0.0f64; fpadded];
        let mut fim = vec![0.0f64; fpadded];
        let mut fscratch = vec![0.0f64; FLUX_WINDOW];
        let mut fpos = 0usize;
        let mut have_prev = false;
        while fpos + FLUX_WINDOW <= n {
            fscratch.copy_from_slice(&mono[fpos..fpos + FLUX_WINDOW]);
            power_spectrum(&fscratch, &fwin, &mut fre, &mut fim);
            let mut flux_now = 0.0f64;
            for k in 0..fbins {
                let f = k as f64 * sample_rate as f64 / fpadded as f64;
                let bin_mag = (fre[k] * fre[k] + fim[k] * fim[k]).sqrt();
                all_mag_sum += bin_mag;
                // The floor must be scaled by the band the flux is measured in.
                // Taking `max` over every bin let the kick set the threshold for
                // hat detection, so the more low end a mix had, the fewer of its
                // hats it could see.
                if f > FLUX_MIN_HZ {
                    max_mag = max_mag.max(bin_mag);
                    hi_mag_sum += bin_mag;
                    if have_prev {
                        flux_now += (bin_mag - prev_mag[k]).max(0.0);
                    }
                    prev_mag[k] = bin_mag;
                }
            }
            if have_prev {
                flux.push(flux_now);
            }
            have_prev = true;
            fpos += FLUX_HOP;
        }
        // A band with nothing in it has no transients in it. Below this share
        // everything above 3 kHz is leakage and float quantization noise, whose
        // frame-to-frame churn is pure flux and would otherwise read as a dense
        // hit pattern. Measured margins: a bare f32 sine sits at 5.7e-4, the
        // dullest real render here (kick alone) at 7.8e-2, a full mix at
        // 2.8e-1..4.5e-1 — the gate has an order of magnitude either side.
        const HI_BAND_MIN_SHARE: f64 = 5e-3;
        let (density, cv) = if hi_mag_sum <= all_mag_sum * HI_BAND_MIN_SHARE {
            (0.0, 0.0)
        } else {
            transient_stats(&flux, n as f64 / sample_rate as f64, max_mag * 1e-3)
        };

        Metrics {
            rms_dbfs: rms_db,
            peak_dbfs: peak_db,
            true_peak_dbfs: true_peak_db,
            crest_db: crest,
            short_term_dyn_db: dyn_db,
            centroid_hz: centroid,
            band_shares,
            transients_per_sec: density,
            hit_cv: cv,
        }
    }

    pub fn band(&self, label: &str) -> f64 {
        BANDS
            .iter()
            .position(|(l, _, _)| *l == label)
            .and_then(|i| self.band_shares.get(i))
            .copied()
            .unwrap_or(0.0)
    }
}

/// Linear-interpolated percentile of an ascending slice (`q` in 0..=1).
fn percentile(sorted: &[f64], q: f64) -> f64 {
    match sorted.len() {
        0 => 0.0,
        1 => sorted[0],
        n => {
            let pos = q.clamp(0.0, 1.0) * (n - 1) as f64;
            let lo = pos.floor() as usize;
            let hi = pos.ceil() as usize;
            sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo as f64)
        }
    }
}

/// A hit has to be this many times the median flux to count. Stationary
/// content — a held tone, quantization noise in an empty band — has a flux
/// series that wobbles around its own median, and `mean + 1.5σ` alone marks
/// the top few percent of any such wobble as "hits", so a steady sine used to
/// read as a drum pattern. A real onset dwarfs the median.
const PEAK_OVER_MEDIAN: f64 = 3.0;

fn transient_stats(flux: &[f64], seconds: f64, noise_floor: f64) -> (f64, f64) {
    if flux.len() < 3 {
        return (0.0, 0.0);
    }
    let mean = flux.iter().sum::<f64>() / flux.len() as f64;
    let var = flux.iter().map(|f| (f - mean) * (f - mean)).sum::<f64>() / flux.len() as f64;
    let std = var.sqrt();
    let mut sorted = flux.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("flux values are finite"));
    let median = percentile(&sorted, 0.5);
    let threshold = (mean + 1.5 * std).max(noise_floor).max(median * PEAK_OVER_MEDIAN);
    let peaks: Vec<f64> = (1..flux.len() - 1)
        .filter(|&i| flux[i] > threshold && flux[i] >= flux[i - 1] && flux[i] >= flux[i + 1])
        .map(|i| flux[i])
        .collect();
    if peaks.len() <= 3 {
        return (0.0, 0.0);
    }
    let pmean = peaks.iter().sum::<f64>() / peaks.len() as f64;
    let pvar = peaks.iter().map(|p| (p - pmean) * (p - pmean)).sum::<f64>() / peaks.len() as f64;
    let cv = if pmean > 0.0 { pvar.sqrt() / pmean } else { 0.0 };
    (peaks.len() as f64 / seconds.max(1e-9), cv)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(done: &mut Vec<f32>, f: impl Fn(usize) -> f32, n: usize) -> Vec<f32> {
        let v: Vec<f32> = (0..n).map(f).collect();
        done.extend(v.iter());
        v
    }

    #[test]
    fn sine_lands_in_its_band_with_right_centroid() {
        let sr = 48_000u32;
        let n = sr as usize;
        // On-bin tone (16 cycles per 1024-frame): no window-slide beating, so
        // the signal is genuinely static to the analyzer.
        let freq_hz = 750.0f32;
        let mut unused = Vec::new();
        let l = render(&mut unused, |i| 0.6 * (std::f32::consts::TAU * freq_hz * i as f32 / sr as f32).sin(), n);
        let r = l.clone();
        let m = Metrics::analyze(&l, &r, sr);
        assert!(m.band("mid") > 0.9, "750 Hz must sit in mid: {}", m.band("mid"));
        assert!((700.0..=800.0).contains(&m.centroid_hz), "centroid {}", m.centroid_hz);
        assert!(m.hit_cv < 0.2, "steady sine must not read as varied hits: {}", m.hit_cv);
        assert!(m.transients_per_sec < 1.0, "steady sine transients: {}", m.transients_per_sec);
        assert!(m.short_term_dyn_db < 3.0, "steady sine dyn: {}", m.short_term_dyn_db);
        // Same amplitude-tracking contract as the live critic's snapshot
        // test: interpolated estimate ≈ the on-bin tone's sample peak.
        let expect_db = 20.0 * 0.6f64.log10();
        assert!(
            (m.true_peak_dbfs - expect_db).abs() < 0.5,
            "true peak {} vs sample peak {expect_db}",
            m.true_peak_dbfs
        );
    }

    #[test]
    fn drum_loop_reads_varied_hits_and_dynamics() {
        let sr = 48_000u32;
        let n = sr as usize * 2;
        // Alternating loud kick-ish thumps and quiet ticks with decay tails.
        let mut l = vec![0.0f32; n];
        let beat = sr as usize / 2;
        for (k, chunk) in l.chunks_mut(beat).enumerate() {
            let amp = if k % 2 == 0 { 0.9 } else { 0.25 };
            for (i, s) in chunk.iter_mut().enumerate() {
                let t = i as f32 / sr as f32;
                *s = amp * (-t * 30.0).exp() * (2.0 * std::f32::consts::PI * 80.0 * t).sin();
                if k % 4 == 1 && i % 64 == 0 {
                    *s += 0.3 * ((i % 128) as f32 / 128.0);
                }
            }
        }
        let r = l.clone();
        let m = Metrics::analyze(&l, &r, sr);
        assert!(m.short_term_dyn_db > 10.0, "drum loop dyn: {}", m.short_term_dyn_db);
        assert!(m.crest_db > 8.0, "drum loop crest: {}", m.crest_db);
    }

    #[test]
    fn silence_is_quiet_and_safe() {
        let m = Metrics::analyze(&vec![0.0; 48_000], &vec![0.0; 48_000], 48_000);
        assert!(m.rms_dbfs < -200.0);
        assert_eq!(m.transients_per_sec, 0.0);
    }
}

