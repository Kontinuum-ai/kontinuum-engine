//! Voice-kind table for the fitter (issue #75): which parameters are
//! fitted per kind, with bounds lifted verbatim from the voices'
//! `set_param` clamps in `kontinuum-core`. Because the fitter searches
//! exactly those clamp intervals (and applies params via `set_param`,
//! which clamps again), **any fit result is a valid IR by construction**
//! — no second validation pass exists or is needed.
//!
//! Only parameters the `kontinuum-ir` schema can carry are fitted, so the
//! printed `InstrumentDef` round-trips losslessly:
//! * kick — `tune_hz` / `decay_ms` / `click` / `drive` (all four slots).
//! * hat — `decay_ms` / `tone`. The circuit's `HAT_OPEN` is a 0..1 float
//!   in the voice but a `bool` in the IR (and `HAT_NOISE_MIX` has no slot
//!   yet), so the fitter pins `open = 0.0` (closed hat) and leaves
//!   `noise_mix` at the voice default 0.1.
//! * clap — `decay_ms` / `tone`. `CLAP_CENTER_HZ` / `CLAP_RESONANCE_Q`
//!   (ParamIds 22/23) have no IR slots, so they stay at the voice
//!   defaults (1100 Hz / Q 1.2) and `tone` sweeps the band centre over
//!   600–1500 Hz exactly as the IR's `tone` is defined to.
//!
//! Fitted params are applied in table order AFTER the pinned ones;
//! `set_param` clamping makes order safe.

use kontinuum_core::params::{
    CLAP_DECAY_MS, CLAP_TONE, HAT_DECAY_MS, HAT_TONE, KICK_CLICK, KICK_DECAY_MS, KICK_DRIVE,
    KICK_TUNE_HZ,
};
use kontinuum_core::ParamId;
use kontinuum_ir::{
    ClapInstrument, ClapTag, HatInstrument, HatTag, InstrumentDef, KickInstrument, KickTag,
};

/// One fittable parameter: `set_param` id, its clamp interval, and the
/// voice's `new()` default (restart 0 starts from the defaults).
#[derive(Clone, Copy, Debug)]
pub struct ParamSpec {
    pub id: ParamId,
    pub name: &'static str,
    pub lo: f32,
    pub hi: f32,
    pub default: f32,
}

/// Drum kinds the fitter drives. Bounds below are the exact `set_param`
/// clamps (kick.rs / hat.rs / hand.rs); defaults are those constructors'.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoiceKind {
    Kick,
    Hat,
    Clap,
}

impl VoiceKind {
    pub fn parse(s: &str) -> Option<VoiceKind> {
        match s {
            "kick" => Some(VoiceKind::Kick),
            "hat" => Some(VoiceKind::Hat),
            "clap" => Some(VoiceKind::Clap),
            _ => None,
        }
    }

    /// Fittable parameters, in application order.
    pub fn params(self) -> &'static [ParamSpec] {
        match self {
            VoiceKind::Kick => &[
                ParamSpec { id: KICK_TUNE_HZ, name: "tune_hz", lo: 20.0, hi: 200.0, default: 50.0 },
                ParamSpec { id: KICK_DECAY_MS, name: "decay_ms", lo: 10.0, hi: 2000.0, default: 430.0 },
                ParamSpec { id: KICK_CLICK, name: "click", lo: 0.0, hi: 1.0, default: 0.55 },
                ParamSpec { id: KICK_DRIVE, name: "drive", lo: 0.2, hi: 8.0, default: 2.2 },
            ],
            // Pinned: HAT_OPEN = 0.0, HAT_NOISE_MIX = 0.1 (voice default).
            VoiceKind::Hat => &[
                ParamSpec { id: HAT_DECAY_MS, name: "decay_ms", lo: 5.0, hi: 2000.0, default: 45.0 },
                ParamSpec { id: HAT_TONE, name: "tone", lo: 0.0, hi: 1.0, default: 0.4 },
            ],
            // Pinned: CLAP_CENTER_HZ / CLAP_RESONANCE_Q untouched (voice
            // defaults 1100 Hz / 1.2); `tone` maps to 600..1500 Hz.
            VoiceKind::Clap => &[
                ParamSpec { id: CLAP_DECAY_MS, name: "decay_ms", lo: 50.0, hi: 1500.0, default: 350.0 },
                ParamSpec { id: CLAP_TONE, name: "tone", lo: 0.0, hi: 1.0, default: 0.55 },
            ],
        }
    }

    /// The fitter's normalized search box is always the unit hypercube;
    /// `to_normalized`/`from_normalized` map to real param units.
    pub fn from_normalized(self, x: &[f64]) -> Vec<f32> {
        self.params()
            .iter()
            .zip(x.iter())
            .map(|(p, &t)| (p.lo as f64 + t * (p.hi - p.lo) as f64) as f32)
            .collect()
    }

    pub fn to_normalized(self, params: &[f32]) -> Vec<f64> {
        self.params()
            .iter()
            .zip(params.iter())
            .map(|(p, &v)| {
                ((v - p.lo) / (p.hi - p.lo)).clamp(0.0, 1.0) as f64
            })
            .collect()
    }

    /// The fit result as an IR `InstrumentDef` (the artifact that ships).
    pub fn to_instrument_def(self, params: &[f32]) -> InstrumentDef {
        match self {
            VoiceKind::Kick => InstrumentDef::Kick(KickInstrument {
                kind: KickTag::Kick,
                tune_hz: params[0],
                decay_ms: params[1],
                click: params[2],
                drive: params[3],
            }),
            VoiceKind::Hat => InstrumentDef::Hat(HatInstrument {
                kind: HatTag::Hat,
                decay_ms: params[0],
                tone: params[1],
                open: false,
            }),
            VoiceKind::Clap => InstrumentDef::Clap(ClapInstrument {
                kind: ClapTag::Clap,
                decay_ms: params[0],
                tone: params[1],
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_match_the_set_param_clamps() {
        // Spot-check the documented clamps: kick drive 0.2..8, hat decay
        // 5..2000, clap decay 50..1500 — all bounds live in the table and
        // nowhere else.
        let kick = VoiceKind::Kick.params();
        assert_eq!((kick[3].lo, kick[3].hi), (0.2, 8.0));
        let hat = VoiceKind::Hat.params();
        assert_eq!((hat[0].lo, hat[0].hi), (5.0, 2000.0));
        let clap = VoiceKind::Clap.params();
        assert_eq!((clap[0].lo, clap[0].hi), (50.0, 1500.0));
    }

    #[test]
    fn normalization_round_trips() {
        for kind in [VoiceKind::Kick, VoiceKind::Hat, VoiceKind::Clap] {
            let raw: Vec<f32> = kind.params().iter().map(|p| p.default).collect();
            let back = kind.from_normalized(&kind.to_normalized(&raw));
            for (a, b) in raw.iter().zip(back.iter()) {
                assert!((a - b).abs() < 1e-4, "{kind:?}: {a} vs {b}");
            }
        }
    }

    #[test]
    fn defaults_serialize_to_valid_ir_json() {
        for kind in [VoiceKind::Kick, VoiceKind::Hat, VoiceKind::Clap] {
            let raw: Vec<f32> = kind.params().iter().map(|p| p.default).collect();
            let def = kind.to_instrument_def(&raw);
            let json = serde_json::to_string(&def).unwrap();
            let back: InstrumentDef = serde_json::from_str(&json).unwrap();
            assert_eq!(back, def, "{kind:?} IR round-trip");
        }
    }

    #[test]
    fn parse_rejects_unknown_kinds() {
        assert_eq!(VoiceKind::parse("kick"), Some(VoiceKind::Kick));
        assert_eq!(VoiceKind::parse("snare"), None);
    }
}
