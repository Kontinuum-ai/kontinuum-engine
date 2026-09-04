//! Live DJ performance facade (issue #38 steps 1–3): beat-locked moves on a
//! running [`ArrangementEngine`].
//!
//! Everything funnels through [`ArrangementEngine::apply_diff`], so the diff
//! pipeline stays the only write path and the engine's block cache keeps all
//! bars before a move's boundary bit-identical. Audibility of a one-shot
//! follows the 4-bar block grid: the block containing the landing bar is the
//! first that can carry it (#13's boundary switching).
//!
//! Landing rules (issue #13 boundary choice, quantized to section
//! boundaries): see [`landing_bar`]. Armed one-shots record the bar they will
//! land on so a surface can render countdowns (issue #33 scope).

use kontinuum_clock::DEFAULT_PHRASE_BARS;
use kontinuum_ir::schema::MusicalKey;
use kontinuum_ir::{ApplyError, ApplyReport, IrDiff, Pattern, Session};

use crate::engine::ArrangementEngine;

/// Quantization grid for armed one-shots (bar-domain twin of
/// `kontinuum_clock::BoundaryKind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quantize {
    /// Land on the next bar.
    Bar,
    /// Land on the next phrase boundary ([`DEFAULT_PHRASE_BARS`] grid).
    Phrase,
}

/// Length of a loop request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopLength {
    /// Hold the groove for half the playing section's bars.
    Half,
    /// Replay the whole section once more.
    Full,
}

/// Which live move is armed; selects the row of the landing-rule table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveMoveKind {
    /// `SetTempo`: lands at the next section boundary (≥ the request bar).
    Tempo,
    /// `SetKey`: session-level hint — lands immediately, no bar target.
    Key,
    /// Loop / `ExtendSection`: becomes audible at the first boundary
    /// strictly after the request (the playing section's end).
    Loop,
}

/// Landing-rule table: `(current_bar, section_start_bars, kind)` → the bar
/// the move lands on, or `None` when no boundary qualifies (key moves and a
/// request past the last boundary).
pub fn landing_bar(kind: LiveMoveKind, current_bar: u32, section_starts: &[u32]) -> Option<u32> {
    match kind {
        LiveMoveKind::Key => None,
        LiveMoveKind::Tempo => section_starts.iter().copied().find(|b| *b >= current_bar),
        LiveMoveKind::Loop => section_starts.iter().copied().find(|b| *b > current_bar),
    }
}

/// First bar/phrase boundary strictly after `current_bar`.
pub fn quantized_bar(quantize: Quantize, current_bar: u32) -> u32 {
    match quantize {
        Quantize::Bar => current_bar.saturating_add(1),
        Quantize::Phrase => (current_bar / DEFAULT_PHRASE_BARS)
            .saturating_add(1)
            .saturating_mul(DEFAULT_PHRASE_BARS),
    }
}

/// A triggered one-shot pattern: what fires, where, and on which grid.
#[derive(Clone, Debug, PartialEq)]
pub struct OneShot {
    pub track: String,
    pub pattern: Pattern,
    pub quantize: Quantize,
}

/// Confirmation that an armed one-shot will land on a known bar.
#[derive(Clone, Debug, PartialEq)]
pub struct ArmedAction {
    pub track: String,
    pub landing_bar: u32,
    pub quantize: Quantize,
}

/// A one-shot that reached its landing bar and was applied.
#[derive(Clone, Debug, PartialEq)]
pub struct LandedOneShot {
    pub track: String,
    pub landing_bar: u32,
    pub section: String,
    pub report: ApplyReport,
}

/// A loop request that was applied.
#[derive(Clone, Debug, PartialEq)]
pub struct LoopApplied {
    pub diff: IrDiff,
    pub section: String,
    pub extra_bars: u32,
    /// Bar where the loop becomes audible: the playing section's end.
    pub landing_bar: u32,
    pub report: ApplyReport,
}

/// A tempo/key move confirmation: the boundary it landed on plus the report.
#[derive(Clone, Debug, PartialEq)]
pub struct MoveLanded {
    pub landing_bar: u32,
    pub report: ApplyReport,
}

struct ArmedOneShot {
    track: String,
    pattern: Pattern,
    landing_bar: u32,
}

/// Performance facade over a running [`ArrangementEngine`] (issue #38 step
/// 2): armed one-shots, loops, and tempo/key moves, quantized by the landing
/// rules. Pure bookkeeping plus diff application — no audio threads.
pub struct DjDeck {
    armed: Vec<ArmedOneShot>,
}

impl Default for DjDeck {
    fn default() -> Self {
        DjDeck { armed: Vec::new() }
    }
}

impl DjDeck {
    pub fn new() -> Self {
        DjDeck::default()
    }

    /// Arms a one-shot pattern (fill, riser, stop), quantized to the next bar
    /// or phrase boundary. Returns where it will land; the move fires on
    /// [`DjDeck::tick`] once the playhead reaches that bar.
    pub fn arm_one_shot(
        &mut self,
        shot: OneShot,
        current_bar: u32,
        session: &Session,
    ) -> Result<ArmedAction, ApplyError> {
        let OneShot { track, pattern, quantize } = shot;
        let landing_bar = quantized_bar(quantize, current_bar);
        if u64::from(landing_bar) >= session.total_bars() {
            return Err(ApplyError::Invalid(format!(
                "one-shot landing bar {landing_bar} is beyond the session ({} bars)",
                session.total_bars()
            )));
        }
        let track: String = track;
        self.armed.push(ArmedOneShot {
            track: track.clone(),
            pattern,
            landing_bar,
        });
        Ok(ArmedAction { track, landing_bar, quantize })
    }

    /// Fires every armed one-shot whose landing bar has arrived, targeting
    /// the section playing at `current_bar` (in-flight sections are editable;
    /// bars before the diff boundary stay cached and bit-identical).
    pub fn tick(
        &mut self,
        current_bar: u32,
        engine: &mut ArrangementEngine,
    ) -> Vec<Result<LandedOneShot, ApplyError>> {
        let armed = std::mem::take(&mut self.armed);
        let mut landed = Vec::new();
        let mut remaining = Vec::new();
        for action in armed {
            if action.landing_bar > current_bar {
                remaining.push(action);
                continue;
            }
            landed.push(fire_one_shot(action, current_bar, engine));
        }
        self.armed = remaining;
        landed
    }

    /// Loops the playing section: instead of ending at its boundary it keeps
    /// playing for `extra_bars` more bars (its own pattern bindings repeat).
    /// Applies immediately — the section is in flight, so the IR accepts it —
    /// and becomes audible at the section's end ([`LoopApplied::landing_bar`]).
    /// The appended bars reuse the section's content-addressed phrase
    /// expansion, and blocks before the diff boundary stay cached, so looping
    /// is cache-stable (pinned in `engine.rs` tests).
    pub fn loop_current_section(
        &mut self,
        length: LoopLength,
        current_bar: u32,
        engine: &mut ArrangementEngine,
    ) -> Result<LoopApplied, ApplyError> {
        let session = engine.current_session();
        let starts = session.section_start_bars();
        let si = playing_section(&starts, current_bar)
            .ok_or_else(|| ApplyError::Invalid(format!("no section is playing at bar {current_bar}")))?;
        let section = &session.sections[si];
        let section_id = section.id.clone();
        let extra_bars = match length {
            LoopLength::Half => (section.bars / 2).max(1),
            LoopLength::Full => section.bars,
        };
        let diff = IrDiff::ExtendSection { id: section_id.clone(), extra_bars };
        let landing_bar = starts[si].saturating_add(section.bars);
        let report = engine.apply_diff(&diff, current_bar)?;
        Ok(LoopApplied {
            diff,
            section: section_id,
            extra_bars,
            landing_bar,
            report,
        })
    }

    /// Tempo move: lands at the next section boundary (≥ `current_bar`),
    /// never mid-section. The tempo lane slews linearly from the previous
    /// breakpoint into the landing boundary (existing `TempoLane` ramp).
    pub fn set_tempo(
        &mut self,
        bpm: f64,
        current_bar: u32,
        engine: &mut ArrangementEngine,
    ) -> Result<MoveLanded, ApplyError> {
        let starts = engine.current_session().section_start_bars();
        let landing_bar = landing_bar(LiveMoveKind::Tempo, current_bar, &starts).ok_or_else(|| {
            ApplyError::Invalid(format!("no section boundary at or after bar {current_bar}"))
        })?;
        let report = engine.apply_diff(&IrDiff::SetTempo { bpm }, landing_bar)?;
        Ok(MoveLanded { landing_bar, report })
    }

    /// Key move: session-level hint, applies immediately (no bar target).
    pub fn set_key(
        &mut self,
        key: MusicalKey,
        current_bar: u32,
        engine: &mut ArrangementEngine,
    ) -> Result<MoveLanded, ApplyError> {
        let report = engine.apply_diff(&IrDiff::SetKey { key }, current_bar)?;
        Ok(MoveLanded { landing_bar: current_bar, report })
    }
}

fn playing_section(section_starts: &[u32], current_bar: u32) -> Option<usize> {
    section_starts.iter().rposition(|b| *b <= current_bar)
}

fn fire_one_shot(
    action: ArmedOneShot,
    current_bar: u32,
    engine: &mut ArrangementEngine,
) -> Result<LandedOneShot, ApplyError> {
    let session = engine.current_session();
    let starts = session.section_start_bars();
    let si = playing_section(&starts, current_bar)
        .ok_or_else(|| ApplyError::Invalid(format!("no section is playing at bar {current_bar}")))?;
    let section = session.sections[si].id.clone();
    let diff = IrDiff::ReplacePattern {
        section: section.clone(),
        track: action.track.clone(),
        pattern: action.pattern,
    };
    let report = engine.apply_diff(&diff, current_bar)?;
    Ok(LandedOneShot {
        track: action.track,
        landing_bar: action.landing_bar,
        section,
        report,
    })
}
