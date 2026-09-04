//! Blind A/B harness (#32): renders the loudness-matched (mix, master)
//! pair the listening protocol consumes, plus a deterministic manifest.
//!
//! Matching discipline: level differences masquerade as quality, so both
//! stimuli are gain-aligned to the *quieter* render's integrated LUFS
//! (attenuation only — nothing gets amplified into clipping) using the
//! crate's BS.1770 measurement. Uniform gain commutes exactly with the
//! gated loudness integral, so the pair lands within a float epsilon of
//! [`MATCH_TOLERANCE_LU`] — asserted by tests.
//!
//! With `--premium` the master comes from the premium chain
//! ([`premium_render`]); otherwise it is the real-time chain
//! ([`MasteringChain`]) rendered offline over the same mix — the exact
//! master a live session would have produced. The listener protocol
//! itself (randomization, survey tool) stays out of scope here.
//!
//! The manifest is plain serde JSON in struct declaration order — no
//! timestamps, no paths, no randomness — so two runs of the same session
//! serialize byte-identically and their hashes can be diffed in CI (#32
//! regression suite).

use std::path::Path;

use serde::Serialize;

use kontinuum_core::BLOCK_FRAMES;
use kontinuum_ir::Session;
use kontinuum_mastering::chain::MasteringChain;
use kontinuum_mastering::offline::{
    integrated_lufs, measure_loudness, normalize_to_target, true_peak_dbfs,
};
use kontinuum_mastering::targets::MasteringTargets;

use crate::premium::premium_render;
use crate::{render_session_with, write_wav, RenderError, RenderOptions, RenderOutput};

/// Blind-pair matching tolerance (LU) the protocol requires.
pub const MATCH_TOLERANCE_LU: f64 = 0.2;
/// Silent tail rendered after the mix so the RT chain's lookahead and
/// release drain before the latency trim (same discipline as premium).
const RT_MASTER_PAD_FRAMES: usize = 4096;

/// The offline-rendered RT chain over the mix, latency-trimmed 1:1.
///
/// The chain is driven in [`BLOCK_FRAMES`] tiles — the same call
/// boundaries `AudioGraph::render_block` uses live (`n =
/// (end - off).min(BLOCK_FRAMES)`, final partial tile included).
/// `MasteringChain::render` re-derives its per-call working point at the
/// top of every call (relax block coefficient, tilt slew,
/// `low.update_block()` — one step per call, no frame count — and the
/// glue seeker), so one call over the whole program would freeze them at
/// their initial working point (#119).
fn master_with_rt_chain(mix: &RenderOutput, targets: &MasteringTargets) -> RenderOutput {
    let mut chain = MasteringChain::new_with_targets(mix.sample_rate, targets);
    let mut l = mix.left.clone();
    let mut r = mix.right.clone();
    l.resize(l.len() + chain.latency_frames() + RT_MASTER_PAD_FRAMES, 0.0);
    r.resize(r.len() + chain.latency_frames() + RT_MASTER_PAD_FRAMES, 0.0);
    let len = l.len().min(r.len());
    let mut off = 0;
    while off < len {
        let n = (len - off).min(BLOCK_FRAMES);
        chain.render(&mut l[off..off + n], &mut r[off..off + n]);
        off += n;
    }
    let delay = chain.latency_frames();
    RenderOutput {
        left: l[delay..delay + mix.left.len()].to_vec(),
        right: r[delay..delay + mix.right.len()].to_vec(),
        sample_rate: mix.sample_rate,
    }
}

/// Per-file manifest entry: content hash + loudness/peak profile.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AbFile {
    pub name: &'static str,
    /// FNV-1a over the interleaved f32 bit patterns (LE), left first.
    pub fnv_hash: String,
    pub integrated_lufs: f64,
    pub short_term_peak_lufs: f64,
    pub lra_lu: f64,
    pub true_peak_dbfs: f64,
}

fn ab_file(name: &'static str, out: &RenderOutput) -> AbFile {
    let m = measure_loudness(&out.left, &out.right, out.sample_rate);
    AbFile {
        name,
        fnv_hash: format!("{:016x}", out.fnv_hash()),
        integrated_lufs: m.integrated_lufs,
        short_term_peak_lufs: m.short_term_peak_lufs,
        lra_lu: m.lra_lu,
        true_peak_dbfs: true_peak_dbfs(&out.left, &out.right),
    }
}

/// Versioned targets snapshot embedded in the manifest.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AbTargets {
    pub name: String,
    pub integrated_lufs: f64,
    pub ceiling_dbtp: f64,
    pub tilt_hz: f64,
    pub tilt_cdb: f64,
}

/// Deterministic manifest for one A/B render pair.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AbManifest {
    pub schema_version: u32,
    pub engine_version: &'static str,
    pub premium: bool,
    pub sample_rate: u32,
    pub session_seed: u64,
    pub targets: AbTargets,
    /// Integrated LUFS both stimuli were gain-aligned to.
    pub match_target_lufs: f64,
    pub files: AbFiles,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AbFiles {
    pub mix: AbFile,
    pub master: AbFile,
}

/// The loudness-matched pair plus its manifest.
#[derive(Clone, Debug)]
pub struct AbPair {
    pub mix: RenderOutput,
    pub master: RenderOutput,
    pub manifest: AbManifest,
}

/// Render the A/B pair for a session: (a) the unmastered mix, (b) the
/// mastered version (`premium` selects the premium chain vs the RT
/// chain), both loudness-matched within [`MATCH_TOLERANCE_LU`].
pub fn render_ab(
    session: &Session,
    sample_rate: u32,
    targets: &MasteringTargets,
    premium: bool,
) -> Result<AbPair, RenderError> {
    // Unmastered, deliberately: this is the "mix" side of a mix-vs-master
    // comparison, and since #98 put the mastering chain inside AudioGraph a
    // plain `render_session` is already mastered. Rendering it that way made
    // the RT arm compare mastered against double-mastered, and the premium
    // arm compare mastered against premium-mastered — neither is the
    // question #32 asks.
    let mix = render_session_with(session, sample_rate, &RenderOptions::unmastered())?;
    let master_raw = if premium {
        premium_render(session, sample_rate, targets)?.master
    } else {
        master_with_rt_chain(&mix, targets)
    };

    let mix_lufs = integrated_lufs(&mix.left, &mix.right, sample_rate);
    let master_lufs = integrated_lufs(&master_raw.left, &master_raw.right, sample_rate);
    if !mix_lufs.is_finite() || !master_lufs.is_finite() {
        return Err(RenderError::Silent(
            "a silent render has no loudness to match".into(),
        ));
    }
    // Stage 1 — peak-legalize each stimulus at its own loudness: the raw
    // mix may exceed full scale, and the ceiling trim must happen BEFORE
    // matching or it shifts one side after the fact.
    let mix_own =
        normalize_to_target(&mix.left, &mix.right, sample_rate, mix_lufs, targets.ceiling_dbtp);
    let master_own = normalize_to_target(
        &master_raw.left,
        &master_raw.right,
        sample_rate,
        master_lufs,
        targets.ceiling_dbtp,
    );
    // Stage 2 — attenuate the louder legal stimulus down to the quieter
    // one: uniform gain moves the gated integrated loudness exactly, no
    // trim fires (peaks only go down), so the pair lands on one level.
    let match_target = mix_own.integrated_lufs.min(master_own.integrated_lufs);
    let mix_m = normalize_to_target(
        &mix_own.left,
        &mix_own.right,
        sample_rate,
        match_target,
        targets.ceiling_dbtp,
    );
    let master_m = normalize_to_target(
        &master_own.left,
        &master_own.right,
        sample_rate,
        match_target,
        targets.ceiling_dbtp,
    );

    let mix_out = RenderOutput { left: mix_m.left, right: mix_m.right, sample_rate };
    let master_out =
        RenderOutput { left: master_m.left, right: master_m.right, sample_rate };
    let manifest = AbManifest {
        schema_version: 1,
        engine_version: env!("CARGO_PKG_VERSION"),
        premium,
        sample_rate,
        session_seed: session.seed,
        targets: AbTargets {
            name: targets.name.clone(),
            integrated_lufs: targets.integrated_lufs,
            ceiling_dbtp: targets.ceiling_dbtp,
            tilt_hz: targets.tilt_hz,
            tilt_cdb: targets.tilt_cdb,
        },
        match_target_lufs: match_target,
        files: AbFiles {
            mix: ab_file("mix.wav", &mix_out),
            master: ab_file("master.wav", &master_out),
        },
    };
    Ok(AbPair { mix: mix_out, master: master_out, manifest })
}

/// Write the pair files and manifest into `dir`: `mix.wav`,
/// `master.wav` (32-bit float), `manifest.json`.
pub fn write_ab(dir: &Path, pair: &AbPair) -> Result<(), RenderError> {
    std::fs::create_dir_all(dir)?;
    write_wav(&dir.join("mix.wav"), &pair.mix)?;
    write_wav(&dir.join("master.wav"), &pair.master)?;
    let json = serde_json::to_string_pretty(&pair.manifest)?;
    std::fs::write(dir.join("manifest.json"), json + "\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::premium::GOLDEN_FIXTURE;
    use crate::{render_session_with, RenderOptions, DEFAULT_SAMPLE_RATE};

    /// Sample-wise agreement the retiled RT leg owes the live render.
    /// The fixture's residual cadence drift (graph event spans vs uniform
    /// tiles) measures ~1.3e-4; the one-call bug this pins diverged at
    /// full scale (#119).
    const RT_LEG_EQUIVALENCE_TOLERANCE: f32 = 1e-3;

    fn fixture_session() -> Session {
        crate::parse_session(Path::new(GOLDEN_FIXTURE)).expect("fixture parses")
    }

    #[test]
    fn ab_pair_is_loudness_matched_within_tolerance() {
        let session = fixture_session();
        let targets = MasteringTargets::hypothesis();
        let pair = render_ab(&session, DEFAULT_SAMPLE_RATE, &targets, true)
            .expect("premium ab render");
        let delta = (pair.manifest.files.mix.integrated_lufs
            - pair.manifest.files.master.integrated_lufs)
            .abs();
        assert!(
            delta <= MATCH_TOLERANCE_LU,
            "matched pair {delta} LU apart (mix {}, master {})",
            pair.manifest.files.mix.integrated_lufs,
            pair.manifest.files.master.integrated_lufs
        );
        // ...and neither stimulus was pushed over the ceiling by matching.
        assert!(pair.manifest.files.mix.true_peak_dbfs <= targets.ceiling_dbtp);
        assert!(pair.manifest.files.master.true_peak_dbfs <= targets.ceiling_dbtp);
    }

    #[test]
    fn rt_leg_matches_the_live_mastered_render() {
        let session = fixture_session();
        let targets = MasteringTargets::hypothesis();
        // The live reference: the graph's own chain, driven once per
        // BLOCK_FRAMES tile across the compiled event spans (#98).
        let live = render_session_with(&session, DEFAULT_SAMPLE_RATE, &RenderOptions::mix())
            .expect("live mastered render");
        // The harness leg: the same unmastered mix through the standalone
        // chain, retiled to the same boundaries.
        let mix = render_session_with(&session, DEFAULT_SAMPLE_RATE, &RenderOptions::unmastered())
            .expect("unmastered mix");
        let rt = master_with_rt_chain(&mix, &targets);
        assert_eq!(
            live.left.len(),
            rt.left.len(),
            "legs must cover the same program"
        );
        assert_eq!(
            live.right.len(),
            rt.right.len(),
            "legs must cover the same program"
        );
        // The graph passes the chain's processing latency through to its
        // output (the host compensates live); the harness leg trims it, so
        // align on `latency_frames` before comparing.
        let latency = MasteringChain::new_with_targets(mix.sample_rate, &targets).latency_frames();
        let n = rt.left.len() - latency;
        let max_diff = live.left[latency..]
            .iter()
            .zip(&rt.left[..n])
            .chain(live.right[latency..].iter().zip(&rt.right[..n]))
            .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(
            max_diff < RT_LEG_EQUIVALENCE_TOLERANCE,
            "RT leg diverges from the live master by {max_diff}: the A/B 'real-time' \
             leg is not what AudioGraph would have rendered"
        );
    }

    #[test]
    fn manifest_is_stable_across_runs_both_modes() {
        let session = fixture_session();
        let targets = MasteringTargets::hypothesis();
        for premium in [false, true] {
            let a = render_ab(&session, DEFAULT_SAMPLE_RATE, &targets, premium)
                .expect("ab render a");
            let b = render_ab(&session, DEFAULT_SAMPLE_RATE, &targets, premium)
                .expect("ab render b");
            let json_a = serde_json::to_string_pretty(&a.manifest).expect("serialize");
            let json_b = serde_json::to_string_pretty(&b.manifest).expect("serialize");
            assert_eq!(json_a, json_b, "manifest must be byte-stable (premium {premium})");
            assert_ne!(
                a.manifest.files.mix.fnv_hash, a.manifest.files.master.fnv_hash,
                "mastering must audibly change the file (premium {premium})"
            );
        }
    }
}
