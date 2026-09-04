//! Issue #76 acceptance: the kick-sidechain pump must actually move the
//! record. A fresh render of each four-to-the-floor style is scanned for a
//! steady 16-beat groove window where the beat-phase-averaged band envelope
//! (32 bins across one beat) swings more than 10 dB in BOTH the sub band
//! (30–100 Hz) and the mid band (400–2k) — the reference patch's numbers
//! the issue pins as the bar (11.5 / 14.1 dB). Arrangement sections strip
//! down on purpose (breakdowns, outro), so the record passes when it
//! CONTAINS such a window, not when every section does.

use kontinuum_analysis::pump_window_ranges;
use kontinuum_compose::arrangement::{generate_session, GenParams};
use kontinuum_ir::{validate_session, Session};
use kontinuum_offline::{render_session, RenderOutput, DEFAULT_SAMPLE_RATE};

/// Window length for the groove scan (beats): long enough to average the
/// phase profile over a full section stretch, short enough to sit inside
/// one arrangement block.
const WINDOW_BEATS: usize = 16;
/// Slide step (beats): a bar, so the scan covers every section stretch.
const HOP_BEATS: usize = 4;
/// The issue's bar: sub AND mid ranges over one beat exceed 10 dB.
const ACCEPTANCE_RANGE_DB: f64 = 10.0;

fn render_genre(genre: &str) -> (Session, RenderOutput) {
    let params = GenParams {
        seed: 7,
        target_bars: 32,
        intensity: 0.75,
        genre: Some(genre.to_string()),
        ..GenParams::default()
    };
    let session = generate_session(&params);
    validate_session(&session).expect("generated session must validate");
    let out = render_session(&session, DEFAULT_SAMPLE_RATE).expect("render");
    (session, out)
}

#[test]
fn house_and_deep_house_pump_over_10_db_in_sub_and_mid() {
    for genre in ["house", "deep-house"] {
        let (session, out) = render_genre(genre);
        let mono: Vec<f32> = out
            .left
            .iter()
            .zip(&out.right)
            .map(|(l, r)| (l + r) * 0.5)
            .collect();
        let bpm = session.tempo_lane.first().map(|(_, b)| *b).unwrap_or(120.0);
        let windows = pump_window_ranges(
            &mono,
            DEFAULT_SAMPLE_RATE,
            bpm,
            WINDOW_BEATS,
            HOP_BEATS,
        );
        assert!(!windows.is_empty(), "{genre}: render too short to scan");
        let best = windows
            .iter()
            .copied()
            .max_by(|a, b| a.0.min(a.1).total_cmp(&b.0.min(b.1)))
            .expect("non-empty");
        assert!(
            best.0 > ACCEPTANCE_RANGE_DB && best.1 > ACCEPTANCE_RANGE_DB,
            "{genre}: no groove window cleared {ACCEPTANCE_RANGE_DB} dB in both bands \
             (best: sub {:.1} dB, mid {:.1} dB across {} windows)",
            best.0,
            best.1,
            windows.len()
        );
    }
}
