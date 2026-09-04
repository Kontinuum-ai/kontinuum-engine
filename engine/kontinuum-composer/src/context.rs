//! Composer context builder (issue #22): one compact, versioned document
//! carrying everything a wake plans from — style card, taste-DNA summary,
//! SectionGraph position + energy, compressed active patterns, last critic
//! report, recent incidents, recent user instructions.
//!
//! Budget: the serialized document must stay ≤ [`TOKEN_BUDGET`] estimated
//! tokens for the lifetime of a session. The bound is structural — every
//! variable-length input is capped by [`ComposerContext::build`] — and is
//! regression-tested in CI with a pathological 2-hour session and
//! oversized inputs.
//!
//! Token counting uses [`estimate_tokens`], a deterministic chars/4
//! estimator (documented swap point: the T1 host substitutes its real
//! tokenizer's count; the budget test asserts on the estimator, which
//! over-counts CJK-heavy text and under-counts none of our formats, so the
//! real-model budget is never larger).

use kontinuum_ir::schema::Pattern;
use kontinuum_ir::Session;

/// Serialization format version; bump on any field change that a T1 model
/// prompt would need to re-learn.
pub const CONTEXT_FORMAT_VERSION: u32 = 1;

/// Estimated-token ceiling for the serialized document (issue #22: ≤ 2k).
pub const TOKEN_BUDGET: usize = 2_000;

/// Style-card clamp (chars). The blended soul card is prose; the model does
/// not need more than this to know the palette.
const MAX_STYLE_CARD_CHARS: usize = 480;
const MAX_TASTE_CHARS: usize = 320;
const MAX_CRITIC_CHARS: usize = 240;
const MAX_LINE_ITEMS: usize = 4;
const MAX_INSTRUCTIONS: usize = 8;
const MAX_ITEM_CHARS: usize = 120;
/// Pattern digests kept; the nearest sections to the playhead win.
const MAX_PATTERN_LINES: usize = 24;

/// Host-supplied narrative state. Strings, not structs: #21 taste-DNA,
/// #25/#26 critic and #15 incidents each own their real types in their own
/// crate; the composer context only needs their compact summaries. The
/// summaries come from the session host (iOS app / bridge) once those
/// crates are wired into the runtime loop.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ContextInputs<'a> {
    pub style_card: &'a str,
    pub taste_summary: &'a str,
    pub critic_report: &'a str,
    pub incidents: &'a [String],
    pub instructions: &'a [String],
}

/// Where the playhead sits in the SectionGraph, with the energy the playing
/// section carries.
#[derive(Clone, Debug, PartialEq)]
pub struct SectionPosition {
    pub id: String,
    pub start_bar: u32,
    pub bars: u32,
    pub energy: f32,
}

/// One active pattern, compressed to a digest line. Raw patterns stay in
/// the session; the model sees only what steers its next decision.
#[derive(Clone, Debug, PartialEq)]
pub struct PatternDigest {
    pub section: String,
    pub track: String,
    pub text: String,
}

/// The versioned composer context (issue #22).
#[derive(Clone, Debug, PartialEq)]
pub struct ComposerContext {
    pub version: u32,
    pub style_card: String,
    pub taste_summary: String,
    pub position: Option<SectionPosition>,
    pub patterns: Vec<PatternDigest>,
    pub critic_report: String,
    pub incidents: Vec<String>,
    pub instructions: Vec<String>,
    /// Bars in the session and the playhead, for the position header.
    pub current_bar: u32,
    pub total_bars: u32,
}

impl ComposerContext {
    /// Builds the context at a playhead. Every input is clamped here, so
    /// the serialized form cannot exceed the token budget no matter what
    /// the host supplies.
    pub fn build(session: &Session, current_bar: u32, inputs: ContextInputs<'_>) -> Self {
        let starts = session.section_start_bars();
        let position = session
            .sections
            .iter()
            .zip(starts.iter())
            .find(|&(sec, start)| current_bar < start + sec.bars)
            .map(|(sec, start)| SectionPosition {
                id: sec.id.clone(),
                start_bar: *start,
                bars: sec.bars,
                energy: sec.energy_curve.first().copied().unwrap_or(0.0),
            });

        let rank = |si: usize| -> (u8, u32) {
            let start = starts[si];
            let end = start + session.sections[si].bars;
            if current_bar >= start && current_bar < end {
                (0, 0)
            } else if current_bar < start {
                (1, start - current_bar)
            } else {
                (2, current_bar - end)
            }
        };
        let mut order: Vec<usize> = (0..session.sections.len()).collect();
        order.sort_by_key(|&si| rank(si));
        let mut patterns = Vec::new();
        for &si in &order {
            let sec = &session.sections[si];
            for (track, pattern) in &sec.pattern_bindings {
                if patterns.len() >= MAX_PATTERN_LINES {
                    break;
                }
                patterns.push(PatternDigest {
                    section: sec.id.clone(),
                    track: track.clone(),
                    text: compress_pattern(pattern),
                });
            }
            if patterns.len() >= MAX_PATTERN_LINES {
                break;
            }
        }

        ComposerContext {
            version: CONTEXT_FORMAT_VERSION,
            style_card: clamp_chars(inputs.style_card, MAX_STYLE_CARD_CHARS),
            taste_summary: clamp_chars(inputs.taste_summary, MAX_TASTE_CHARS),
            position,
            patterns,
            critic_report: clamp_chars(inputs.critic_report, MAX_CRITIC_CHARS),
            incidents: inputs
                .incidents
                .iter()
                .rev()
                .take(MAX_LINE_ITEMS)
                .rev()
                .map(|s| clamp_chars(s, MAX_ITEM_CHARS))
                .collect(),
            instructions: inputs
                .instructions
                .iter()
                .rev()
                .take(MAX_INSTRUCTIONS)
                .rev()
                .map(|s| clamp_chars(s, MAX_ITEM_CHARS))
                .collect(),
            current_bar,
            total_bars: session.total_bars() as u32,
        }
    }

    /// Compact serialization the planner prompt embeds. Deterministic:
    /// same context, same bytes.
    pub fn serialize(&self) -> String {
        let mut out = String::with_capacity(1_400);
        out.push_str(&format!("ctx v{} bar {}/{}", self.version, self.current_bar, self.total_bars));
        if let Some(pos) = &self.position {
            out.push_str(&format!(
                " sec \"{}\" bars {}..{} energy {:.2}",
                pos.id,
                pos.start_bar,
                pos.start_bar + pos.bars,
                pos.energy
            ));
        }
        out.push('\n');
        if !self.style_card.is_empty() {
            out.push_str(&format!("style: {}\n", self.style_card));
        }
        if !self.taste_summary.is_empty() {
            out.push_str(&format!("taste: {}\n", self.taste_summary));
        }
        if !self.patterns.is_empty() {
            out.push_str("patterns:\n");
            for p in &self.patterns {
                out.push_str(&format!(" {}@{}: {}\n", p.track, p.section, p.text));
            }
        }
        if !self.critic_report.is_empty() {
            out.push_str(&format!("critic: {}\n", self.critic_report));
        }
        if !self.incidents.is_empty() {
            out.push_str(&format!("incidents: {}\n", self.incidents.join(" | ")));
        }
        if !self.instructions.is_empty() {
            out.push_str("instructions:\n");
            for i in &self.instructions {
                out.push_str(&format!(" - {i}\n"));
            }
        }
        out
    }

    /// Estimated tokens of [`Self::serialize`].
    pub fn estimated_tokens(&self) -> usize {
        estimate_tokens(&self.serialize())
    }
}

/// Deterministic token estimator: ~4 chars per token, rounded up. This is
/// the CI budget gate's counter, not a model tokenizer — documented swap
/// point for the T1 host (see module docs).
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

/// True when the document fits the wake budget.
pub fn within_budget(text: &str) -> bool {
    estimate_tokens(text) <= TOKEN_BUDGET
}

/// Truncates on a char boundary, marking the cut so the model can tell.
fn clamp_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// One-line pattern digest. Compression contract: shape + the numbers a
/// composer reasons about (onset density, rotation, probability, velocity);
/// step lists collapse to their onset count.
fn compress_pattern(pattern: &Pattern) -> String {
    match pattern {
        Pattern::Euclidean(e) => format!(
            "euc k={} n={} r={} v={:.2} p={:.2}",
            e.k, e.n, e.rot, e.velocity, e.probability
        ),
        Pattern::ProbabilityMask(m) => {
            format!("pmask d={:.2} v={:.2} p={:.2}", m.density, m.velocity, m.probability)
        }
        Pattern::Steps(s) => {
            let hits = s.steps.len();
            let vel = s.steps.first().map(|st| st.velocity).unwrap_or(0.8);
            format!("steps hits={hits} v={vel:.2}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kontinuum_compose::arrangement::{generate_session, GenParams};

    fn session() -> Session {
        generate_session(&GenParams { seed: 7, target_bars: 32, ..Default::default() })
    }

    fn inputs<'a>(
        style: &'a str,
        taste: &'a str,
        critic: &'a str,
        incidents: &'a [String],
        instructions: &'a [String],
    ) -> ContextInputs<'a> {
        ContextInputs {
            style_card: style,
            taste_summary: taste,
            critic_report: critic,
            incidents,
            instructions,
        }
    }

    #[test]
    fn context_captures_position_energy_and_tracks() {
        let s = session();
        // The grammar (#16) draws section lengths, so locate the section
        // under the playhead instead of assuming a fixed layout.
        let starts = s.section_start_bars();
        let (idx, &start) = starts
            .iter()
            .enumerate()
            .find(|&(i, &start)| 12 >= start && 12 < start + s.sections[i].bars)
            .expect("bar 12 inside the session");
        let live = &s.sections[idx];
        let ctx = ComposerContext::build(&s, 12, inputs("deep house card", "warm loopy", "", &[], &[]));
        let pos = ctx.position.expect("bar 12 is inside the session");
        assert_eq!(pos.id, live.id);
        assert_eq!(pos.start_bar, start);
        assert_eq!(pos.bars, live.bars);
        assert!(pos.energy > 0.0, "energy state is carried");
        assert!(!ctx.patterns.is_empty(), "active patterns are digested");
        assert!(
            ctx.patterns.iter().any(|p| p.section == live.id),
            "the live section's patterns come first"
        );
        assert_eq!(ctx.version, CONTEXT_FORMAT_VERSION);
    }

    #[test]
    fn serialization_is_versioned_and_deterministic() {
        let s = session();
        let ctx = ComposerContext::build(&s, 10, inputs("card", "taste", "critic ok", &[], &[]));
        let a = ctx.serialize();
        let b = ctx.serialize();
        assert_eq!(a, b);
        // The grammar (#16) draws the layout, so the header values are
        // derived from the session rather than hardcoded.
        assert!(
            a.starts_with(&format!("ctx v1 bar 10/{}", s.total_bars())),
            "header carries bar + total",
        );
        let live = ctx.position.as_ref().map(|p| p.id.clone()).unwrap_or_default();
        assert!(a.contains(&format!("sec \"{live}\"")), "the section under the playhead is named");
        assert!(a.contains("style: card"));
        assert!(a.contains("taste: taste"));
        assert!(a.contains("patterns:"));
        assert!(a.contains("critic: critic ok"));
    }

    #[test]
    fn recent_instructions_and_incidents_are_capped_and_ordered() {
        let s = session();
        let instructions: Vec<String> = (0..20).map(|i| format!("instruction {i}")).collect();
        let incidents: Vec<String> = (0..10).map(|i| format!("incident {i}")).collect();
        let ctx = ComposerContext::build(&s, 4, inputs("", "", "", &incidents, &instructions));
        assert_eq!(ctx.instructions.len(), MAX_INSTRUCTIONS);
        assert_eq!(ctx.incidents.len(), MAX_LINE_ITEMS);
        // Recent wins: the last 8 instructions survive, oldest dropped.
        assert_eq!(ctx.instructions.first().unwrap(), "instruction 12");
        assert_eq!(ctx.instructions.last().unwrap(), "instruction 19");
        assert_eq!(ctx.incidents.last().unwrap(), "incident 9");
    }

    #[test]
    fn long_inputs_are_clamped() {
        let s = session();
        let huge = "x".repeat(10_000);
        let ctx = ComposerContext::build(
            &s,
            4,
            inputs(&huge, &huge, &huge, &[], &[]),
        );
        assert!(ctx.style_card.chars().count() <= MAX_STYLE_CARD_CHARS);
        assert!(ctx.taste_summary.chars().count() <= MAX_TASTE_CHARS);
        assert!(ctx.critic_report.chars().count() <= MAX_CRITIC_CHARS);
    }

    #[test]
    fn pattern_digests_compress_the_real_generators() {
        let s = session();
        let ctx = ComposerContext::build(&s, 4, inputs("", "", "", &[], &[]));
        for p in &ctx.patterns {
            assert!(
                p.text.starts_with("euc ")
                    || p.text.starts_with("steps ")
                    || p.text.starts_with("pmask "),
                "digest form for {}@{}: {}",
                p.track,
                p.section,
                p.text
            );
        }
    }

    #[test]
    fn token_budget_holds_for_a_two_hour_session_with_fat_inputs() {
        // Pathological case from the acceptance criteria: 2-hour session
        // (≈ 2h × 60m × 126bpm / 4 beats ≈ 3.8k bars — halved here for test
        // runtime, which is still far past anything the budget could ride),
        // oversized style card, a backlog of incidents and instructions.
        let s = generate_session(&GenParams {
            seed: 3,
            target_bars: 1_900,
            ..Default::default()
        });
        let style = "x".repeat(10_000);
        let instructions: Vec<String> = (0..200).map(|i| format!("steer the vibe {i}")).collect();
        let incidents: Vec<String> = (0..100).map(|i| format!("clipping on bar {i}")).collect();
        let ctx = ComposerContext::build(
            &s,
            900,
            inputs(&style, &style, &style, &incidents, &instructions),
        );
        let text = ctx.serialize();
        assert!(
            within_budget(&text),
            "context must stay under {TOKEN_BUDGET} estimated tokens, got {}",
            ctx.estimated_tokens()
        );
        assert_eq!(ctx.estimated_tokens(), estimate_tokens(&text));
    }

    #[test]
    fn estimator_is_deterministic_and_monotone() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
        assert!(estimate_tokens(&"y".repeat(399)) < estimate_tokens(&"y".repeat(401)));
    }

    #[test]
    fn past_the_end_session_still_builds() {
        let s = session();
        let ctx = ComposerContext::build(&s, 999, inputs("", "", "", &[], &[]));
        assert!(ctx.position.is_none());
        assert!(!ctx.patterns.is_empty(), "the whole session is now history; still digest it");
    }
}
