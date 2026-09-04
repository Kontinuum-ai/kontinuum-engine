//! Render adapter (issue #75): one note-on of a named voice kind at
//! candidate params, rendered through the real `kontinuum-core` voices
//! into mono f32. Every render constructs a FRESH voice — the per-hit
//! jitter/noise streams are deterministic per instance, so a fresh
//! instance at the same params always renders a bit-identical hit (the
//! same property the core crate's determinism tests pin). This is what
//! makes the fitting landscape well-defined and the whole fitter
//! reproducible.

use kontinuum_instruments_core::{Clap, Hat, Kick};
use kontinuum_core::{ParamId, Voice};

use super::VoiceKind;

// Mirrors `kontinuum_core::voice::hat::HAT_NOISE_MIX` (slot 19): the const
// lives in a private module of core, so the value is pinned here by id.
const HAT_NOISE_MIX: ParamId = 19;

/// Renders one hit of `kind` at `params` (real units, `VoiceKind::params()`
/// order) for `frames` frames at `sample_rate`. Pinned params (no IR slot)
/// are applied first, fitted params second; the voice self-mutes and the
/// remainder stays exactly zero.
pub fn render_note(kind: VoiceKind, params: &[f32], sample_rate: u32, frames: usize) -> Vec<f32> {
    let mut voice: Box<dyn Voice> = match kind {
        VoiceKind::Kick => Box::new(Kick::new(sample_rate)),
        VoiceKind::Hat => Box::new(Hat::new(sample_rate)),
        VoiceKind::Clap => Box::new(Clap::new(sample_rate)),
    };
    apply_pinned(kind, voice.as_mut());
    for (spec, &value) in kind.params().iter().zip(params.iter()) {
        voice.set_param(spec.id, value);
    }
    voice.note_on(60.0, 1.0);
    let mut out = vec![0.0f32; frames];
    voice.render(&mut out);
    out
}

/// Params the fitter does not search (no IR slot) — pinned to the closed /
/// default values documented in [`super::VoiceKind`].
fn apply_pinned(kind: VoiceKind, voice: &mut dyn Voice) {
    let pinned: &[(ParamId, f32)] = match kind {
        VoiceKind::Kick | VoiceKind::Clap => &[],
        VoiceKind::Hat => &[
            (kontinuum_core::params::HAT_OPEN, 0.0),
            (HAT_NOISE_MIX, 0.1),
        ],
    };
    for &(id, value) in pinned {
        voice.set_param(id, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    fn defaults(kind: VoiceKind) -> Vec<f32> {
        kind.params().iter().map(|p| p.default).collect()
    }

    #[test]
    fn same_params_render_bit_identically() {
        for kind in [VoiceKind::Kick, VoiceKind::Hat, VoiceKind::Clap] {
            let p = defaults(kind);
            let a = render_note(kind, &p, SR, 24_000);
            let b = render_note(kind, &p, SR, 24_000);
            assert_eq!(a.len(), b.len());
            assert!(
                a.iter().zip(b.iter()).all(|(x, y)| x.to_bits() == y.to_bits()),
                "{kind:?} renders diverged"
            );
            assert!(a.iter().any(|&s| s != 0.0), "{kind:?} rendered silence");
        }
    }

    #[test]
    fn decay_param_changes_the_rendered_length() {
        let short = render_note(VoiceKind::Hat, &[45.0, 0.4], SR, 96_000);
        let long = render_note(VoiceKind::Hat, &[400.0, 0.4], SR, 96_000);
        let last_nonzero = |x: &[f32]| x.iter().rposition(|&s| s != 0.0).unwrap_or(0);
        assert!(last_nonzero(&long) > last_nonzero(&short) * 2);
    }

    #[test]
    fn voice_self_mutes_inside_the_buffer() {
        let x = render_note(VoiceKind::Kick, &defaults(VoiceKind::Kick), SR, 96_000);
        let end = x.iter().rposition(|&s| s != 0.0).unwrap();
        assert!(x[end + 1..].iter().all(|&s| s == 0.0));
    }
}
