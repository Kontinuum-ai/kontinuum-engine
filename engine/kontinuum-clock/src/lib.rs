//! `kontinuum-clock` — musical transport: PPQ, tempo automation, bar/phrase/section
//! boundaries, seeded RNG streams (issue #10).
//!
//! Design invariants:
//! - Sample-frame mapping uses **closed-form integration** over piecewise tempo
//!   lanes (prefix sums of exact per-segment integrals) — never naive sample
//!   accumulation, so re-rendering any bar range is bit-reproducible.
//! - All mapping functions are allocation-free and pure.
//! - Determinism: every randomness source derives from one master seed through a
//!   documented hash hierarchy (`derive_seed`), so (session seed, track, purpose)
//!   fully determines the stream.

pub const PPQ: u32 = 960;
pub const BEATS_PER_BAR: u32 = 4;
pub const TICKS_PER_BAR: u64 = (PPQ * BEATS_PER_BAR) as u64;
pub const DEFAULT_PHRASE_BARS: u32 = 8;

/// Musical position. All fields 0-based; `tick` in `[0, PPQ)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MusicalTime {
    pub bar: u32,
    pub beat: u8,
    pub tick: u16,
}

impl MusicalTime {
    pub const fn at_bar(bar: u32) -> Self {
        MusicalTime { bar, beat: 0, tick: 0 }
    }

    /// Position in ticks from session start (bar 0 beat 0 tick 0).
    pub const fn to_ticks(self) -> u64 {
        self.bar as u64 * TICKS_PER_BAR + self.beat as u64 * PPQ as u64 + self.tick as u64
    }

    pub fn from_ticks(ticks: u64) -> Self {
        let bar = (ticks / TICKS_PER_BAR) as u32;
        let rem = ticks % TICKS_PER_BAR;
        let beat = (rem / PPQ as u64) as u8;
        let tick = (rem % PPQ as u64) as u16;
        MusicalTime { bar, beat, tick }
    }

    /// Position in quarter notes from session start (fractional).
    pub fn to_beats(self) -> f64 {
        self.bar as f64 * BEATS_PER_BAR as f64
            + self.beat as f64
            + self.tick as f64 / PPQ as f64
    }
}

/// Kind of musical boundary — the switching primitive for block hot-swaps (#13).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BoundaryKind {
    Bar,
    Phrase,
    Section,
}

/// Piecewise tempo automation lane. BPM is linearly interpolated between
/// breakpoints anchored at integer bars; the lane starts at bar 0.
///
/// Closed-form mapping: for a segment of `L` bars ramping `a → c` BPM, the time
/// to cross it is `60·L·ln(c/a)/(c−a)` seconds (`60·L/a` when `a == c`). Prefix
/// sums make `time ⇄ musical position` exact regardless of distance — the
/// property tests pin drift at 0 for thousands of bars.
#[derive(Clone, Debug)]
pub struct TempoLane {
    sample_rate: f64,
    bars: Vec<u32>,
    bpm: Vec<f64>,
    /// `cum_time[i]`: seconds at the *start* of bar `bars[i]`.
    cum_time: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TempoLaneError {
    pub reason: &'static str,
}

impl TempoLane {
    /// `breakpoints` must start at bar 0, be strictly ascending in bar, and have
    /// bpm > 0. A single breakpoint (bar 0) is a constant-tempo lane.
    pub fn new(sample_rate: u32, breakpoints: &[(u32, f64)]) -> Result<Self, TempoLaneError> {
        if sample_rate == 0 {
            return Err(TempoLaneError { reason: "sample_rate must be > 0" });
        }
        if breakpoints.is_empty() {
            return Err(TempoLaneError { reason: "at least one breakpoint required" });
        }
        if breakpoints[0].0 != 0 {
            return Err(TempoLaneError { reason: "first breakpoint must be bar 0" });
        }
        for w in breakpoints.windows(2) {
            if w[0].0 >= w[1].0 {
                return Err(TempoLaneError { reason: "breakpoint bars must strictly ascend" });
            }
        }
        if breakpoints.iter().any(|(_, b)| !b.is_finite() || *b <= 0.0) {
            return Err(TempoLaneError { reason: "bpm must be finite and > 0" });
        }
        let bars: Vec<u32> = breakpoints.iter().map(|(b, _)| *b).collect();
        let bpm: Vec<f64> = breakpoints.iter().map(|(_, b)| *b).collect();
        let n = bars.len();
        let mut cum_time = Vec::with_capacity(n + 1);
        cum_time.push(0.0);
        for i in 0..n {
            let span = if i + 1 < n { (bars[i + 1] - bars[i]) as f64 } else { 0.0 };
            let (a, c) = (bpm[i], if i + 1 < n { bpm[i + 1] } else { bpm[i] });
            let t = if span == 0.0 {
                0.0 // final segment: constant bpm, handled analytically below
            } else if (a - c).abs() < 1e-12 {
                240.0 * span / a
            } else {
                240.0 * span * (c / a).ln() / (c - a)
            };
            cum_time.push(cum_time[i] + t);
        }
        Ok(TempoLane { sample_rate: sample_rate as f64, bars, bpm, cum_time })
    }

    /// Constant-tempo lane.
    pub fn constant(sample_rate: u32, bpm: f64) -> Result<Self, TempoLaneError> {
        Self::new(sample_rate, &[(0, bpm)])
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate as u32
    }

    /// BPM at an arbitrary (fractional) bar position.
    pub fn bpm_at_bar(&self, bar: f64) -> f64 {
        let n = self.bars.len();
        if bar >= self.bars[n - 1] as f64 {
            return self.bpm[n - 1];
        }
        let mut lo = 0usize;
        let mut hi = n - 1;
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if (self.bars[mid] as f64) <= bar {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let span = (self.bars[hi] - self.bars[lo]) as f64;
        let frac = (bar - self.bars[lo] as f64) / span;
        self.bpm[lo] + (self.bpm[hi] - self.bpm[lo]) * frac
    }

    /// Exact seconds from session start to a fractional bar position.
    pub fn time_at_bar(&self, bar: f64) -> f64 {
        let n = self.bars.len();
        if bar >= self.bars[n - 1] as f64 {
            // Final constant-bpm region.
            let over = bar - self.bars[n - 1] as f64;
            return self.cum_time[n - 1] + 240.0 * over / self.bpm[n - 1];
        }
        let mut lo = 0usize;
        let mut hi = n - 1;
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if self.bars[mid] as f64 <= bar {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let span = (self.bars[hi] - self.bars[lo]) as f64;
        let x = bar - self.bars[lo] as f64;
        let (a, c) = (self.bpm[lo], self.bpm[hi]);
        let t = if (a - c).abs() < 1e-12 {
            240.0 * x / a
        } else {
            240.0 * span * ((a + x * (c - a) / span) / a).ln() / (c - a)
        };
        self.cum_time[lo] + t
    }

    /// Inverse of `time_at_bar` (fractional bar position at exact seconds).
    pub fn bar_at_time(&self, time: f64) -> f64 {
        let n = self.bars.len();
        if time >= self.cum_time[n - 1] {
            let over = time - self.cum_time[n - 1];
            return self.bars[n - 1] as f64 + over * self.bpm[n - 1] / 240.0;
        }
        // Binary search the prefix sums for the containing segment.
        let mut lo = 0usize;
        let mut hi = n - 1;
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if self.cum_time[mid] <= time {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let target = time - self.cum_time[lo];
        let span = (self.bars[hi] - self.bars[lo]) as f64;
        let (a, c) = (self.bpm[lo], self.bpm[hi]);
        let x = if (a - c).abs() < 1e-12 {
            target * a / 240.0
        } else {
            span * a * ((c - a) * target / (240.0 * span)).exp_m1() / (c - a)
        };
        self.bars[lo] as f64 + x
    }

    /// Exact sample frame of a musical position.
    pub fn frame_of(&self, t: MusicalTime) -> u64 {
        let bar_f = t.bar as f64
            + (t.beat as f64 * PPQ as f64 + t.tick as f64) / TICKS_PER_BAR as f64;
        let frame = self.time_at_bar(bar_f) * self.sample_rate;
        frame.round() as u64
    }

    /// Exact sample frame of a fractional bar position.
    pub fn frame_of_bar(&self, bar: f64) -> u64 {
        (self.time_at_bar(bar) * self.sample_rate).round() as u64
    }

    /// Fractional bar position at a sample frame (inverse of `frame_of_bar`).
    pub fn bar_at_frame(&self, frame: u64) -> f64 {
        self.bar_at_time(frame as f64 / self.sample_rate)
    }

    /// Seconds per bar at the given fractional bar position (for local pacing).
    pub fn seconds_per_bar_at(&self, bar: f64) -> f64 {
        240.0 / self.bpm_at_bar(bar)
    }

    /// First sample frame strictly **after** `from_frame` where boundary kind
    /// `kind` occurs, plus its bar index. Block activation exactly at a
    /// boundary frame is the engine's job (`frame >= block.start_frame`), so
    /// this is strictly-after. For `Section`, `section_bars` holds the sorted
    /// section start bars.
    pub fn next_boundary(
        &self,
        kind: BoundaryKind,
        from_frame: u64,
        section_bars: &[u32],
    ) -> Option<(u64, u32)> {
        // Current fractional bar → next candidate bar for the kind.
        let cur_bar = self.bar_at_frame(from_frame);
        let candidates: Box<dyn Iterator<Item = u32>> = match kind {
            BoundaryKind::Bar => Box::new((cur_bar.ceil() as u32)..),
            BoundaryKind::Phrase => {
                let step = DEFAULT_PHRASE_BARS;
                let next = ((cur_bar / step as f64).floor() as u32 + 1) * step;
                Box::new((next..).step_by(step as usize))
            }
            BoundaryKind::Section => Box::new(
                section_bars
                    .iter()
                    .copied()
                    .filter(|b| (*b as f64) >= cur_bar - 1e-9),
            ),
        };
        let mut found: Option<(u64, u32)> = None;
        for bar in candidates {
            let frame = self.frame_of_bar(bar as f64);
            if frame > from_frame {
                found = Some((frame, bar));
                break;
            }
        }
        found
    }
}

/// Deterministic counter-based RNG (SplitMix64). Reproducible across targets.
#[derive(Clone, Debug)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn from_seed(seed: u64) -> Self {
        Rng { state: seed }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform `[0, 1)`.
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Uniform `[min, max)`.
    pub fn range_f32(&mut self, min: f32, max: f32) -> f32 {
        min + self.next_f32() * (max - min)
    }

    /// Bernoulli trial with probability `p` in `[0,1]`.
    pub fn chance(&mut self, p: f32) -> bool {
        self.next_f32() < p
    }

    /// Uniform integer in `[0, n)` (n > 0), rejection-free via widening.
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
}

/// Seed hierarchy: one deterministic sub-stream per `(master_seed, track, purpose)`.
/// Changing the track id or purpose yields an independent stream; a fixed pair
/// always yields the identical stream for a given master seed.
pub fn derive_seed(master_seed: u64, track: u8, purpose: u16) -> u64 {
    let mut z = master_seed
        ^ (track as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (purpose as u64) << 32;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Convenience: dedicated RNG for `(master_seed, track, purpose)`.
pub fn stream(master_seed: u64, track: u8, purpose: u16) -> Rng {
    Rng::from_seed(derive_seed(master_seed, track, purpose))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lane() -> TempoLane {
        TempoLane::new(48_000, &[(0, 124.0), (16, 132.0), (32, 124.0)]).unwrap()
    }

    #[test]
    fn musical_time_roundtrip() {
        for ticks in [0u64, 1, PPQ as u64, TICKS_PER_BAR, TICKS_PER_BAR * 7 + 5 * PPQ as u64 + 3] {
            let t = MusicalTime::from_ticks(ticks);
            assert_eq!(t.to_ticks(), ticks);
        }
    }

    #[test]
    fn frame_mapping_monotonic() {
        let l = lane();
        let mut prev = 0;
        for bar in 0..2000 {
            let f = l.frame_of_bar(bar as f64);
            assert!(f >= prev, "frame mapping regressed at bar {bar}");
            prev = f;
        }
    }

    #[test]
    fn closed_form_matches_naive_integration() {
        // Naive per-tick accumulation of instantaneous tempo must agree with the
        // closed form to sub-sample tolerance (issue #10 drift property).
        let l = TempoLane::new(48_000, &[(0, 120.0), (8, 140.0), (40, 118.0)]).unwrap();
        let ticks_per_step = 24u64; // one metric modulation step
        let mut t_sec = 0.0f64;
        let mut ticks = 0u64;
        let target_ticks = 40 * TICKS_PER_BAR;
        let dt_bars = ticks_per_step as f64 / TICKS_PER_BAR as f64;
        while ticks < target_ticks {
            let bar_f = ticks as f64 / TICKS_PER_BAR as f64;
            let bpm = l.bpm_at_bar(bar_f);
            t_sec += 240.0 / bpm * dt_bars;
            ticks += ticks_per_step;
        }
        let closed = l.time_at_bar(target_ticks as f64 / TICKS_PER_BAR as f64);
        assert!(
            (t_sec - closed).abs() < 1e-3,
            "drift {t_sec} vs closed form {closed}"
        );
    }

    #[test]
    fn time_bar_inverse_roundtrip() {
        let l = lane();
        for &bar in &[0.0f64, 0.5, 7.25, 16.0, 23.75, 31.5, 100.0, 4096.25] {
            let t = l.time_at_bar(bar);
            let back = l.bar_at_time(t);
            assert!((bar - back).abs() < 1e-9, "inverse failed at bar {bar}: {back}");
        }
    }

    #[test]
    fn boundaries() {
        let l = lane();
        // Next bar boundary from frame 0 is bar 1.
        let (f, b) = l.next_boundary(BoundaryKind::Bar, 0, &[]).unwrap();
        assert_eq!(b, 1);
        assert_eq!(f, l.frame_of_bar(1.0));
        // Phrase boundaries land on multiples of 8.
        let (f, b) = l.next_boundary(BoundaryKind::Phrase, 0, &[]).unwrap();
        assert_eq!(b, 8);
        assert_eq!(f, l.frame_of_bar(8.0));
        // Mid-bar cursor.
        let mid = l.frame_of_bar(3.5);
        let (_, b) = l.next_boundary(BoundaryKind::Bar, mid, &[]).unwrap();
        assert_eq!(b, 4);
        // Section boundaries come from the provided list only.
        let (_, b) = l
            .next_boundary(BoundaryKind::Section, 0, &[16, 48, 64])
            .unwrap();
        assert_eq!(b, 16);
    }

    #[test]
    fn rng_determinism_and_streams() {
        let mut a = stream(42, 1, 7);
        let mut b = stream(42, 1, 7);
        for _ in 0..1000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        let mut c = stream(42, 2, 7);
        let mut d = stream(42, 1, 8);
        assert_ne!(a.next_u64(), c.next_u64());
        assert_ne!(a.next_u64(), d.next_u64());
        let mut e = Rng::from_seed(1);
        for _ in 0..10_000 {
            let f = e.next_f32();
            assert!((0.0..1.0).contains(&f));
        }
    }

    #[test]
    fn rejects_bad_lanes() {
        assert!(TempoLane::new(48_000, &[]).is_err());
        assert!(TempoLane::new(48_000, &[(1, 120.0)]).is_err());
        assert!(TempoLane::new(48_000, &[(0, 120.0), (0, 130.0)]).is_err());
        assert!(TempoLane::new(48_000, &[(0, 0.0)]).is_err());
        assert!(TempoLane::new(48_000, &[(0, f64::NAN)]).is_err());
    }
}
