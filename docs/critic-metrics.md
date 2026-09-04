# Critic metrics — exact definitions (#25)

Every number the critic produces, as implemented in `engine/kontinuum-analysis`
(`critic.rs`, `stems.rs`, `metrics.rs`, `filters.rs`, `verdict.rs`). All filters
are deterministic; the same master/stem stream always yields the same
snapshots. Consumers: the reward model (#26, `kontinuum-compose::reward`),
the kill-switch feed (#15, `kontinuum-supervision::critic_feed`), and the
composer context (#22).

## Signal conditioning

- **K-weighting** (ITU-R BS.1770): stage-1 high shelf, +3.99984 dB @
  1681.97 Hz, Q 0.70718 → stage-2 high-pass, 38.135 Hz, Q 0.50033 (RBJ
  biquads, `filters::KWeighter`). Identical math to
  `kontinuum-mastering::loudness` so offline and live numbers agree.
- **Analysis horizon**: 60 s rolling ring (`ANALYSIS_SECONDS`), 100 ms slots;
  values below ≈ −240.7 LUFS are floored so silence is a number, not a NaN.

## Master snapshot (`CriticSnapshot`, per push-block)

| Metric | Definition |
|---|---|
| `momentary_lufs` | −0.691 + 10·log₁₀(mean square of K-weighted L/R sum) over 400 ms (4 slots). |
| `short_term_lufs` | Same over 3 s (30 slots). |
| `integrated_lufs` | BS.1770 gated: absolute gate −70 LUFS, relative gate 10 LU below the ungated mean, over the trailing 60 s. |
| `crest_db` | short-term peak dBFS − short-term RMS dBFS (peak/RMS ratio). |
| `tilt_db_per_oct` | Least-squares slope of log-power vs log-frequency across the 6-band plan (below), 100 Hz–10 kHz region; negative = dark, positive = bright. |
| `centroid_hz` | Σ(fᵢ·Pᵢ)/ΣPᵢ over STFT bins, power-weighted (8192 Hanning window). |
| `sub_share` | Spectral power in 20–60 Hz ÷ total power (0..1), from the band plan. |
| `correlation` | Pearson correlation of L vs R over the short-term window (−1..1). |
| `width_db` | 20·log₁₀(RMS side / RMS mid), mid = (L+R)/2, side = (L−R)/2. |
| `true_peak_dbfs` | 4× linear-interpolated oversampling peak over the short-term window. |

**Band plan** (`metrics::BANDS`, shared by tilt/shares):
sub 20–60, bass 60–150, lowmid 150–400, mid 400–2k, himid 2k–6k, high 6k–16k Hz.

## Stem board (`StemBoardSnapshot`, per push-block)

Four fixed buses — Kick, Bass, Perc, Pad — each independently:

| Metric | Definition |
|---|---|
| `short_term_lufs` | 3 s K-weighted solo loudness of the stem. |
| `centroid_hz` | Power-weighted spectral centroid of the stem. |
| `transients_per_sec` | Spectral flux (1024/512 window/hop, > 3 kHz), peaks above mean + 3σ of the trailing flux ring, floor 10⁻³ of session max magnitude; peaks per second across the ring span. |

### Kick↔bass masking collision

Both stems band-passed 30–120 Hz (`MASK_LO_HZ`/`MASK_HI_HZ`, 2nd-order HP+LP,
Q 0.7071); mean-square per 30 ms slot gives envelope series x (kick), y (bass):

```
C = Σ min(x, y) / Σ max(x, y)   over the trailing ≤ 20 s
```

Scale-invariant, 0..1: → 1 when both are simultaneously loud (masking), → 0
when disjoint in time or an out-of-band bass drives y → 0. Consumed by the
auto-mixer (#27) and penalized by the reward model (#26).

## Verdict layer (`CriticVerdict::evaluate`)

`CriticVerdict` scores each axis one-sided against a versioned
`CriticTargets` fixture (per-genre), violations normalized by the target's
tolerance and clamped ≥ 0:

- **dynamics**: violation of `crest_floor_db` ÷ `crest_tolerance_db`
- **spectral**: |tilt − `tilt_target`| ÷ `tilt_tolerance`
- **low_end**: violation of `sub_share_cap` ÷ `sub_share_tolerance`
- **loudness**: |integrated − `integrated_target`| ÷ `loudness_tolerance`

Flags (each independently latched into the #15 kill-switch feed via
`critic_feed`, one fault window per evaluation when any is raised):
`dynamics_collapsed`, `spectral_imbalance`, `sub_rumble`,
`loudness_shortfall`, `loudness_excess`. Minimal-techno target values ship
as versioned JSON fixtures under `engine/kontinuum-analysis/fixtures/`
(hypotheses pending the #23 corpus, per #52/#28 — that caveat expires
when #23 closes).

## Warm-up

Every consumer gates on `CriticSnapshot.seconds` — readings before the
windows fill (400 ms / 3 s / 60 s respectively) are marked by the signal-time
field, not silently truncated.
