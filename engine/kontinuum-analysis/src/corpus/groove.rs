//! Groove / microtiming statistics for the corpus pipeline (issue #5's
//! percussive-band method): onset times are compared against the detected
//! 16th grid, and the per-slot mean offsets and strengths become the
//! groove observation the corpus fitters cluster into templates.

use crate::corpus::onsets::Onset;
use crate::corpus::tempo::BeatGrid;
use kontinuum_corpus::GrooveObservation;

/// Ticks per 16th in the IR's tick domain.
const TICKS_PER_16TH: f64 = 120.0;

/// Minimum onset evidence before groove stats mean anything.
const MIN_ONSETS: usize = 32;

/// Builds the per-16th microtiming/velocity/swing observation, or `None`
/// when the track's onset evidence is too thin (the schema's documented
/// "did not converge" case).
pub fn observe(grid: &BeatGrid, onsets: &[Onset]) -> Option<GrooveObservation> {
    if onsets.len() < MIN_ONSETS {
        return None;
    }
    let sixteenth = grid.beat_sec() / 4.0;
    let mut offset_sum = [0.0f64; 16];
    let mut strength_sum = [0.0f64; 16];
    let mut counts = [0u32; 16];
    let mut all_offsets: Vec<f64> = Vec::with_capacity(onsets.len());
    let mut downbeat_sum = 0.0f64;
    let mut downbeat_count = 0u32;
    for o in onsets {
        let rel = (o.time_sec - grid.first_beat_sec) / sixteenth;
        if rel < 0.0 {
            continue;
        }
        let slot = ((rel.round() as i64).rem_euclid(16)) as usize;
        let offset_ticks = (rel - rel.round()) * TICKS_PER_16TH;
        let offset_ticks = offset_ticks.clamp(-TICKS_PER_16TH, TICKS_PER_16TH);
        offset_sum[slot] += offset_ticks;
        strength_sum[slot] += o.strength;
        counts[slot] += 1;
        all_offsets.push(offset_ticks);
        if slot % 4 == 0 {
            downbeat_sum += offset_ticks;
            downbeat_count += 1;
        }
    }
    let total: u32 = counts.iter().sum();
    if (total as usize) < MIN_ONSETS {
        return None;
    }

    // Detection-lag debias: the onset pipeline measures every hit a
    // constant amount early/late (frame quantization + filter delay).
    // The reference is the DOWNBEAT slots' mean offset — kicks land dead
    // on the grid there — falling back to the overall median when the
    // downbeat slots are sparsely populated.
    let bias = if downbeat_count >= 8 {
        downbeat_sum / f64::from(downbeat_count)
    } else {
        all_offsets.sort_by(f64::total_cmp);
        all_offsets[all_offsets.len() / 2]
    };

    let mut microtiming = [0.0f32; 16];
    let mut velocity = [0.0f32; 16];
    for i in 0..16 {
        if counts[i] > 0 {
            microtiming[i] = (offset_sum[i] / counts[i] as f64 - bias) as f32;
            velocity[i] = (strength_sum[i] / counts[i] as f64) as f32;
        }
    }
    let max_v = velocity.iter().cloned().fold(1e-9f32, f32::max);
    for v in velocity.iter_mut() {
        *v = (*v / max_v).clamp(0.0, 1.0);
    }

    // Swing: mean lateness of the off-8th slots (16th indices 2, 6, 10,
    // 14 — the "and" between beats), expressed 0..=1 of a 16th.
    let off8th: [usize; 4] = [2, 6, 10, 14];
    let swing = off8th.iter().map(|&i| f64::from(microtiming[i])).sum::<f64>() / 4.0
        / TICKS_PER_16TH;
    Some(GrooveObservation {
        swing: swing.clamp(0.0, 1.0) as f32,
        velocity_profile: velocity,
        microtiming_profile: microtiming,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn onset(t: f64, s: f64) -> Onset {
        Onset { time_sec: t, strength: s }
    }

    #[test]
    fn thin_evidence_is_none() {
        let grid = BeatGrid { bpm: 120.0, first_beat_sec: 0.0 };
        assert!(observe(&grid, &[]).is_none());
    }

    #[test]
    fn planted_swing_and_velocity_recover() {
        let grid = BeatGrid { bpm: 120.0, first_beat_sec: 0.0 };
        let sixteenth = 0.125;
        let mut onsets = Vec::new();
        for beat in 0..64u32 {
            // Kick dead on the beat, strong.
            onsets.push(onset(beat as f64 * 4.0 * sixteenth, 1.0));
            // Off-8th hat, weaker, planted 18 ticks late.
            let late = 18.0 / 120.0 * sixteenth;
            onsets.push(onset(beat as f64 * 4.0 * sixteenth + 2.0 * sixteenth + late, 0.5));
        }
        let g = observe(&grid, &onsets).expect("enough onsets");
        assert!((g.swing - 18.0 / 120.0).abs() < 0.02, "swing {}", g.swing);
        assert!(g.velocity_profile[0] > 0.9, "kick slot velocity {}", g.velocity_profile[0]);
        assert!(g.velocity_profile[2] < 0.7, "hat slot velocity {}", g.velocity_profile[2]);
        assert!((g.microtiming_profile[0]).abs() < 3.0);
        assert!((g.microtiming_profile[2] - 18.0).abs() < 3.0, "hat lateness {}", g.microtiming_profile[2]);
    }
}
