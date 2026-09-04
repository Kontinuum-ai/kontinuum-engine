//! Structural segmentation for the corpus pipeline: bar-level novelty
//! boundary detection, then honest role labeling and boundary-type
//! classification. The detector's label vocabulary matches what the
//! corpus fitters and the #16 planner map:
//! sections `intro/build/drop/break/groove/outro`,
//! boundaries `silence/filter_sweep/fill/hard_cut`.
//!
//! Honesty (documented for #23): detected labels are HEURISTIC ROLES, not
//! human truths. "drop" means "full-energy arrival after a build/intro",
//! "groove" means "other full-energy sections". Detected sections never
//! label themselves "reintro" — that mapping stays with the planner.

use crate::corpus::features::BarFeatures;

/// Shortest section the detector will emit, in bars (4/4 dance floor:
/// sections shorter than this are production accidents, not structure).
pub const MIN_SECTION_BARS: u32 = 4;

#[derive(Clone, Debug, PartialEq)]
pub struct DetectedSection {
    pub start_bar: u32,
    pub bars: u32,
    pub kind: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BoundaryEvent {
    /// Bar index where the NEXT section starts.
    pub bar: u32,
    pub kind: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Segmentation {
    pub sections: Vec<DetectedSection>,
    pub boundaries: Vec<BoundaryEvent>,
}

/// Weighted feature novelty between consecutive bars.
fn novelty(smooth: &[BarFeatures], i: usize, max_density: f64) -> f64 {
    let d = |f: fn(&BarFeatures) -> f64, w: f64| w * (f(&smooth[i]) - f(&smooth[i - 1])).abs();
    d(|f| f.energy, 1.0)
        + d(|f| f.brightness, 0.75)
        + d(|f| f.density, 0.5 / max_density.max(1.0))
}

fn smooth3(feats: &[BarFeatures]) -> Vec<BarFeatures> {
    (0..feats.len())
        .map(|i| {
            let lo = i.saturating_sub(1);
            let hi = (i + 2).min(feats.len());
            let n = (hi - lo) as f64;
            let mean = |get: fn(&BarFeatures) -> f64| {
                feats[lo..hi].iter().map(|f| get(f)).sum::<f64>() / n
            };
            BarFeatures {
                energy: mean(|f| f.energy),
                density: mean(|f| f.density),
                brightness: mean(|f| f.brightness),
                flux: mean(|f| f.flux),
            }
        })
        .collect()
}

/// Segments the bar-feature curve. Deterministic: no randomness anywhere.
pub fn segment(feats: &[BarFeatures]) -> Segmentation {
    let n = feats.len();
    if n < 2 * MIN_SECTION_BARS as usize {
        return single_section(n, feats);
    }
    let smooth = smooth3(feats);
    let max_density = feats.iter().map(|f| f.density).fold(1e-9, f64::max);

    let mut novelties: Vec<(usize, f64)> = Vec::with_capacity(n);
    for i in 1..n {
        novelties.push((i, novelty(&smooth, i, max_density)));
    }
    let mean = novelties.iter().map(|(_, v)| *v).sum::<f64>() / novelties.len() as f64;
    let std = (novelties.iter().map(|(_, v)| (v - mean).powi(2)).sum::<f64>() / novelties.len() as f64).sqrt();
    let threshold = (mean + 0.8 * std).max(0.10);

    let mut ranked: Vec<(usize, f64)> = Vec::new();
    for &(i, v) in &novelties {
        let raw_energy_step = (feats[i].energy - feats[i - 1].energy).abs();
        // A fill roll is boundary evidence on its own: a local flux spike
        // nominates the boundary even when the global novelty is modest.
        let lo = i.saturating_sub(12);
        let mut local_flux: Vec<f64> = feats[lo..i].iter().map(|f| f.flux).collect();
        local_flux.sort_by(f64::total_cmp);
        let local_median_flux = local_flux[local_flux.len() / 2];
        let spike_bar = if i >= 2 { feats[i - 1].flux.max(feats[i - 2].flux) } else { feats[i - 1].flux };
        let flux_spike = spike_bar >= 1.6 * local_median_flux.max(0.15);
        // Strong feature change, or a fill spike that coincides with SOME
        // feature change — bare 8-bar periodicity produces flux spikes
        // inside otherwise flat sections.
        let strong = v > threshold || raw_energy_step > 0.18;
        if strong || (flux_spike && v > 0.3 * threshold) {
            ranked.push((i, v));
        }
    }
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));

    // Strongest-first greedy selection, keeping the minimum section
    // length on both sides of every accepted boundary.
    let min = MIN_SECTION_BARS as usize;
    let mut accepted: Vec<usize> = Vec::new();
    for &(i, _) in &ranked {
        if i < min || n - i < min {
            continue;
        }
        if accepted.iter().any(|&a| (a as i64 - i as i64).abs() < min as i64) {
            continue;
        }
        accepted.push(i);
    }
    accepted.sort_unstable();

    let mut sections = Vec::new();
    let mut boundaries: Vec<BoundaryEvent> = accepted
        .iter()
        .map(|&i| BoundaryEvent { bar: i as u32, kind: classify_boundary(feats, &smooth, i) })
        .collect();
    // A silence dip's own recovery must not read as a second boundary:
    // drop any boundary within 8 bars after a silence-classified one
    // (half-bar dropouts inside breakdowns are the same artifact).
    let mut kept: Vec<BoundaryEvent> = Vec::new();
    for b in boundaries {
        if kept.last().is_some_and(|prev: &BoundaryEvent| {
            prev.kind == "silence" && b.bar - prev.bar < 8
        }) {
            continue;
        }
        kept.push(b);
    }
    boundaries = kept;

    let mut start = 0usize;
    for b in &boundaries {
        let bar = b.bar as usize;
        sections.push(DetectedSection {
            start_bar: start as u32,
            bars: (bar - start) as u32,
            kind: "groove",
        });
        start = bar;
    }
    sections.push(DetectedSection {
        start_bar: start as u32,
        bars: (n - start) as u32,
        kind: "groove",
    });
    label_sections(&mut sections, feats);
    Segmentation { sections, boundaries }
}

fn single_section(n: usize, feats: &[BarFeatures]) -> Segmentation {
    let kind = if n == 0 {
        "groove"
    } else {
        let e = feats.iter().map(|f| f.energy).sum::<f64>() / n as f64;
        if e < 0.45 { "break" } else { "groove" }
    };
    Segmentation {
        sections: vec![DetectedSection { start_bar: 0, bars: n as u32, kind }],
        boundaries: Vec::new(),
    }
}

/// Boundary type at bar `i` (start of the new section). Order is the
/// classifier's honesty: a silence dip wins over everything, then a FILL
/// (transient density spike — checked before sweep because a roll also
/// brightens the mix, while a true sweep is smooth noise with no spike),
/// then a filter sweep (a sustained brightness RISE into the boundary —
/// sustained and monotone, so hat-level flicker cannot fake one).
fn classify_boundary(raw: &[BarFeatures], smooth: &[BarFeatures], i: usize) -> &'static str {
    let n = raw.len();
    let next_window = &raw[i..(i + 2).min(n)];
    if next_window.iter().map(|f| f.energy).fold(f64::INFINITY, f64::min) < 0.08 {
        return "silence";
    }
    let prev_len = i.min(8);
    if prev_len >= 3 {
        // Spike: the strongest of the last two bars before the boundary
        // (the ±1-bar grid tolerance puts the roll in either one), against
        // the summed onset strength (flux) of the preceding bars — flux
        // survives peak-merging that undercounts a dense roll.
        let spike = raw[i - 1].flux.max(raw[i - 2].flux);
        let mut baseline: Vec<f64> = raw[i - prev_len..i - 2].iter().map(|f| f.flux).collect();
        baseline.sort_by(f64::total_cmp);
        let median = baseline[baseline.len() / 2];
        // The 0.15 floor keeps fill detection alive for drumless sections
        // (a breakdown roll spikes against a ~0 baseline).
        if spike >= 1.6 * median.max(0.15) {
            return "fill";
        }
    }
    let w = i.min(6);
    if w >= 4 {
        // The sweep PEAKS at the section change, and the ±1-bar grid
        // tolerance puts that peak on either side — so the rise is
        // measured into the first bar of the next section, not just up
        // to the boundary.
        let peak = smooth[i.min(n - 1)].brightness.max(smooth[(i + 1).min(n - 1)].brightness);
        let rise = peak - smooth[i - w].brightness;
        let steps = smooth[i - w..i].windows(2);
        let rising_steps = steps.filter(|s| s[1].brightness >= s[0].brightness - 0.005).count();
        // A sweep enters a section that does not collapse: an outro fade
        // also brightens (its kick drops out), but its energy FALLS.
        let not_collapsing = raw[i - 1].energy >= 0.9 * raw[i - w].energy;
        if rise >= 0.08 && rising_steps >= (w as f64 * 0.7) as usize && not_collapsing {
            return "filter_sweep";
        }
    }
    "hard_cut"
}

/// Role labels, applied left to right (each rule looks at its neighbor).
/// The tail is an outro only when it actually declines — an energetic
/// tail is still groove.
fn label_sections(sections: &mut [DetectedSection], feats: &[BarFeatures]) {
    if sections.is_empty() {
        return;
    }
    let mean_e = |s: &DetectedSection| -> f64 {
        let a = s.start_bar as usize;
        (a..a + s.bars as usize).map(|b| feats[b].energy).sum::<f64>() / s.bars.max(1) as f64
    };
    let first = mean_e(&sections[0]);
    sections[0].kind = if first < 0.75 { "intro" } else { "groove" };
    let last = sections.len() - 1;
    for i in 1..sections.len() {
        let e = mean_e(&sections[i]);
        let prev_kind = sections[i - 1].kind;
        let prev_e = mean_e(&sections[i - 1]);
        sections[i].kind = if i == last && e < prev_e * 0.9 {
            "outro"
        } else if e < 0.45 {
            "break"
        } else if prev_kind == "intro" {
            if e < 0.75 { "build" } else { "drop" }
        } else if prev_kind == "build" {
            "drop"
        } else if prev_kind == "outro" {
            "outro"
        } else {
            "groove"
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(energy: f64, density: f64, brightness: f64) -> BarFeatures {
        BarFeatures { energy, density, brightness, flux: density * 0.1 }
    }

    #[test]
    fn a_flat_track_is_one_section() {
        let feats = vec![bar(0.8, 8.0, 0.5); 32];
        let seg = segment(&feats);
        assert_eq!(seg.sections.len(), 1);
        assert!(seg.boundaries.is_empty());
    }

    #[test]
    fn energy_steps_are_found_and_labeled() {
        // intro 8 (quiet) → drop 16 (loud) → break 8 (silent first bar,
        // then quiet) → groove 8 (mid) → outro 8 (declining) — 48 bars.
        let mut feats = Vec::new();
        feats.extend(std::iter::repeat(bar(0.4, 8.0, 0.3)).take(8));
        feats.extend(std::iter::repeat(bar(1.0, 12.0, 0.7)).take(16));
        feats.push(bar(0.0, 0.0, 0.7));
        feats.extend(std::iter::repeat(bar(0.35, 2.0, 0.5)).take(7));
        feats.extend(std::iter::repeat(bar(0.8, 10.0, 0.6)).take(8));
        for i in 0..8u32 {
            feats.push(bar(0.45 - f64::from(i) * 0.045, 6.0, 0.5));
        }
        let seg = segment(&feats);
        let kinds: Vec<&str> = seg.sections.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, vec!["intro", "drop", "break", "groove", "outro"]);
        // Boundary times drift ±1 bar with the smoothing window — the
        // pipeline grades them through the corpus crate's tolerant F1.
        let truth = kontinuum_corpus::SegmentationAnnotation {
            track_id: "test".into(),
            tolerance_bars: 1,
            sections: [0u32, 8, 24, 32, 40]
                .iter()
                .map(|&start_bar| kontinuum_corpus::AnnotatedSection { start_bar, bars: 8, label: None })
                .collect(),
        };
        let bars: Vec<u32> = seg.boundaries.iter().map(|b| b.bar).collect();
        assert_eq!(kontinuum_corpus::boundary_f1(&bars, &truth).f1, 1.0, "detected {bars:?}");
        assert_eq!(seg.boundaries[1].kind, "silence");
    }
}
