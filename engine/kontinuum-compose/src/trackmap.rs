//! Track map (issue #16): a printable SVG rendering of the SectionGraph —
//! one band per section, labeled with kind + bars, the energy curve drawn
//! as a polyline over the band, and transitions marked at their edge.
//! Deterministic per session: no time, no RNG, pure function of the
//! sections. Feeds #33's living UI later.

use kontinuum_ir::schema::Section;

/// Canvas geometry: 40 px per bar, bands 64 px tall.
const PX_PER_BAR: f32 = 12.0;
const BAND_H: f32 = 64.0;
const CURVE_H: f32 = 48.0;
const LABEL_H: f32 = 14.0;

/// Renders the session's SectionGraph to SVG text. Width scales with the
/// longest session; band fill intensity tracks the section's energy.
pub fn render_svg(sections: &[Section]) -> String {
    let total_bars: u32 = sections.iter().map(|s| s.bars).sum();
    let width = (total_bars.max(1) as f32 * PX_PER_BAR).ceil();
    let height = BAND_H + LABEL_H + 8.0;
    let mut out = String::with_capacity(sections.len() * 512 + 256);
    out.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\">\n"
    ));
    out.push_str(&format!(
        "<rect width=\"{width}\" height=\"{height}\" fill=\"#101014\"/>\n"
    ));
    out.push_str(&format!(
        "<text x=\"4\" y=\"{LABEL_H}\" fill=\"#e8e8ec\" font-family=\"monospace\" font-size=\"12\">bars {total_bars}</text>\n"
    ));
    let mut x = 0.0f32;
    for sec in sections {
        let w = sec.bars as f32 * PX_PER_BAR;
        let energy = sec.energy_curve.first().copied().unwrap_or(0.5);
        // Energy tints the band: dark = low, warm = driving.
        let r = (60.0 + 160.0 * energy) as u8;
        let g = (40.0 + 90.0 * energy) as u8;
        let b = (80.0 + 40.0 * energy) as u8;
        out.push_str(&format!(
            "<rect x=\"{x}\" y=\"{LABEL_H}\" width=\"{w}\" height=\"{BAND_H}\" fill=\"#{r:02x}{g:02x}{b:02x}\" stroke=\"#26262c\"/>\n"
        ));
        out.push_str(&format!(
            "<text x=\"{}\" y=\"{}\" fill=\"#f2f2f5\" font-family=\"monospace\" font-size=\"11\">{} · {}b</text>\n",
            x + 3.0,
            LABEL_H + 14.0,
            sec.id,
            sec.bars
        ));
        // The energy curve as a polyline over the band.
        if sec.energy_curve.len() > 1 {
            let points: Vec<String> = sec
                .energy_curve
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    let px = x + (i as f32 + 0.5) / sec.energy_curve.len() as f32 * w;
                    let py = LABEL_H + BAND_H - e * CURVE_H;
                    format!("{px:.1},{py:.1}")
                })
                .collect();
            out.push_str(&format!(
                "<polyline points=\"{}\" fill=\"none\" stroke=\"#ffd166\" stroke-width=\"1.5\"/>\n",
                points.join(" ")
            ));
        }
        // Transition marks: in-boundary above, out-boundary below.
        if let Some(t) = sec.transition_in.as_ref() {
            out.push_str(&transition_mark(x, LABEL_H + 2.0, &kind_label(t.kind)));
        }
        if let Some(t) = sec.transition_out.as_ref() {
            out.push_str(&transition_mark(x + w - 4.0, LABEL_H + BAND_H - 8.0, &kind_label(t.kind)));
        }
        x += w;
    }
    out.push_str("</svg>\n");
    out
}

fn kind_label(kind: kontinuum_ir::schema::TransitionKind) -> &'static str {
    use kontinuum_ir::schema::TransitionKind::*;
    match kind {
        FilterSweep => "sweep",
        MuteChoreo => "mute",
        Fill => "fill",
        SilenceDrop => "drop",
        Riser => "riser",
        ReverbThrow => "throw",
    }
}

fn transition_mark(x: f32, y: f32, label: &str) -> String {
    format!(
        "<text x=\"{x}\" y=\"{y}\" fill=\"#8ecae6\" font-family=\"monospace\" font-size=\"9\">{label}</text>\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use kontinuum_ir::schema::{Section, Transition, TransitionKind};

    fn section(id: &str, bars: u32, energy: &[f32]) -> Section {
        Section {
            id: id.into(),
            bars,
            energy_curve: energy.to_vec(),
            density_curve: Vec::new(),
            brightness_curve: Vec::new(),
            transition_in: None,
            transition_out: Some(Transition {
                kind: TransitionKind::Riser,
                bars: 2,
                params: serde_json::Value::Null,
            }),
            pattern_bindings: Default::default(),
            automation: Default::default(),
        }
    }

    #[test]
    fn svg_is_deterministic_and_well_formed() {
        let sections = [section("intro", 8, &[0.3, 0.45]), section("dev_0", 24, &[0.5, 0.7])];
        let a = render_svg(&sections);
        let b = render_svg(&sections);
        assert_eq!(a, b, "same sections must render byte-identical SVG");
        assert!(a.starts_with("<svg") && a.trim_end().ends_with("</svg>"));
        assert!(a.contains("intro · 8b"));
        assert!(a.contains("dev_0 · 24b"));
        assert_eq!(a.matches("<rect").count(), 3, "backdrop + two bands");
    }

    #[test]
    fn empty_and_single_value_curves_render_without_polylines() {
        let sections = [section("outro", 8, &[0.4])];
        let svg = render_svg(&sections);
        assert!(!svg.contains("<polyline"));
    }
}
