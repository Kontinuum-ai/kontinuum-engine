//! First-party instrument pack (issue #51): the original 12 synth voices
//! (+ the IR-unreachable wavetable/FM-perc/texture core kinds) as explicit
//! plugin registrations. The harness itself contains zero instrument code —
//! `kontinuum-core` builds and boots without this crate.
//!
//! Sampler and the #37 patch interpreter stay harness built-ins (they can
//! express or host everything else); these native plugins exist for
//! efficiency and quality.

use kontinuum_core::{MixRole, ParamId, Voice};
use kontinuum_plugin_api::{InstrumentPlugin, ParamSpec, Registry};

mod bass;
mod ep;
mod fmperc;
mod hand;
mod hat;
mod kick;
mod melodic;
mod pad;
mod texture;
mod wavetable;

pub use bass::Bass;
pub use ep::Ep;
pub use fmperc::{FmPerc, FmPreset};
pub use hand::{Clap, Shaker, Snare};
pub use hat::Hat;
pub use kick::Kick;
pub use melodic::{Acid, Pluck, Stab};
pub use pad::Pad;
pub use texture::Texture;
pub use wavetable::{WavetableSet, WavetableVoice};

const PERC_COST: f32 = 0.6;
const BASS_COST: f32 = 2.0;
const PAD_COST: f32 = 3.0;
const PERC_STEM: usize = 2;
const PAD_STEM: usize = 3;

const KICK_PARAMS: [ParamSpec; 4] = [
    ParamSpec { name: "tune_hz", param: 0, range: (30.0, 120.0), default: 48.0, cost_weight: 0.4 },
    ParamSpec { name: "decay_ms", param: 1, range: (50.0, 1500.0), default: 300.0, cost_weight: 0.2 },
    ParamSpec { name: "click", param: 2, range: (0.0, 1.0), default: 0.4, cost_weight: 0.2 },
    ParamSpec { name: "drive", param: 3, range: (0.0, 1.0), default: 0.2, cost_weight: 0.2 },
];
const HAT_PARAMS: [ParamSpec; 3] = [
    ParamSpec { name: "decay_ms", param: 16, range: (5.0, 2000.0), default: 60.0, cost_weight: 0.4 },
    ParamSpec { name: "tone", param: 18, range: (0.0, 1.0), default: 0.6, cost_weight: 0.3 },
    // `open` is a bool in the IR; surfaced as 0/1 on the control route.
    ParamSpec { name: "open", param: 17, range: (0.0, 1.0), default: 0.0, cost_weight: 0.3 },
];
const CLAP_PARAMS: [ParamSpec; 2] = [
    ParamSpec { name: "decay_ms", param: 20, range: (50.0, 1500.0), default: 350.0, cost_weight: 0.5 },
    ParamSpec { name: "tone", param: 21, range: (0.0, 1.0), default: 0.6, cost_weight: 0.5 },
];
const SNARE_PARAMS: [ParamSpec; 3] = [
    ParamSpec { name: "tune_hz", param: 24, range: (120.0, 420.0), default: 185.0, cost_weight: 0.34 },
    ParamSpec { name: "decay_ms", param: 25, range: (60.0, 900.0), default: 220.0, cost_weight: 0.33 },
    ParamSpec { name: "snap", param: 26, range: (0.0, 1.0), default: 0.6, cost_weight: 0.33 },
];
const SHAKER_PARAMS: [ParamSpec; 2] = [
    ParamSpec { name: "decay_ms", param: 28, range: (20.0, 600.0), default: 90.0, cost_weight: 0.5 },
    ParamSpec { name: "tone", param: 29, range: (0.0, 1.0), default: 0.6, cost_weight: 0.5 },
];
const BASS_PARAMS: [ParamSpec; 4] = [
    ParamSpec { name: "cutoff_hz", param: 33, range: (40.0, 8000.0), default: 900.0, cost_weight: 0.4 },
    ParamSpec { name: "resonance", param: 34, range: (0.0, 1.0), default: 0.2, cost_weight: 0.2 },
    ParamSpec { name: "glide_ms", param: 32, range: (0.0, 1000.0), default: 30.0, cost_weight: 0.2 },
    // Wave enum surfaced as the control-route value (0 = saw, 1 = square).
    ParamSpec { name: "wave", param: 35, range: (0.0, 1.0), default: 0.0, cost_weight: 0.2 },
];
const ACID_PARAMS: [ParamSpec; 4] = [
    ParamSpec { name: "cutoff_hz", param: 40, range: (60.0, 8000.0), default: 700.0, cost_weight: 0.3 },
    ParamSpec { name: "resonance", param: 41, range: (0.0, 1.0), default: 0.2, cost_weight: 0.2 },
    ParamSpec { name: "env_amt", param: 42, range: (0.0, 4.0), default: 2.6, cost_weight: 0.3 },
    ParamSpec { name: "glide_ms", param: 43, range: (0.0, 1000.0), default: 30.0, cost_weight: 0.2 },
];
const PAD_PARAMS: [ParamSpec; 4] = [
    ParamSpec { name: "attack_ms", param: 50, range: (1.0, 10000.0), default: 400.0, cost_weight: 0.25 },
    ParamSpec { name: "release_ms", param: 51, range: (10.0, 20000.0), default: 1200.0, cost_weight: 0.25 },
    ParamSpec { name: "detune_cents", param: 48, range: (-100.0, 100.0), default: 10.0, cost_weight: 0.25 },
    ParamSpec { name: "cutoff_hz", param: 49, range: (40.0, 16000.0), default: 3000.0, cost_weight: 0.25 },
];
const EP_PARAMS: [ParamSpec; 2] = [
    ParamSpec { name: "decay_ms", param: 60, range: (200.0, 6000.0), default: 1400.0, cost_weight: 0.5 },
    ParamSpec { name: "depth", param: 61, range: (0.0, 6.0), default: 2.4, cost_weight: 0.5 },
];
const PLUCK_PARAMS: [ParamSpec; 2] = [
    ParamSpec { name: "damping", param: 52, range: (0.0, 1.0), default: 0.5, cost_weight: 0.5 },
    ParamSpec { name: "bright", param: 53, range: (0.0, 1.0), default: 0.5, cost_weight: 0.5 },
];
const STAB_PARAMS: [ParamSpec; 3] = [
    ParamSpec { name: "cutoff_hz", param: 56, range: (200.0, 12000.0), default: 2600.0, cost_weight: 0.34 },
    ParamSpec { name: "decay_ms", param: 57, range: (60.0, 2000.0), default: 420.0, cost_weight: 0.33 },
    ParamSpec { name: "detune_cents", param: 58, range: (0.0, 40.0), default: 11.0, cost_weight: 0.33 },
];
/// Sound roster v2 (#30): control-route schemas for the wavetable, FM-perc
/// and texture voices so IR documents (and packs authored from them) can
/// address every voice parameter by name. Param ids match `params.rs`.
const WAVETABLE_PARAMS: [ParamSpec; 6] = [
    ParamSpec { name: "position", param: 4, range: (0.0, 1.0), default: 0.5, cost_weight: 0.15 },
    ParamSpec { name: "detune_cents", param: 5, range: (0.0, 50.0), default: 14.0, cost_weight: 0.15 },
    ParamSpec { name: "osc2_level", param: 6, range: (0.0, 1.0), default: 0.8, cost_weight: 0.15 },
    ParamSpec { name: "sub", param: 7, range: (0.0, 1.0), default: 0.35, cost_weight: 0.15 },
    ParamSpec { name: "cutoff_hz", param: 8, range: (100.0, 12000.0), default: 6000.0, cost_weight: 0.2 },
    ParamSpec { name: "release_ms", param: 9, range: (20.0, 8000.0), default: 220.0, cost_weight: 0.2 },
];
const FMPERC_PARAMS: [ParamSpec; 5] = [
    ParamSpec { name: "ratio", param: 10, range: (0.25, 8.0), default: 1.0, cost_weight: 0.2 },
    ParamSpec { name: "index", param: 11, range: (0.0, 8.0), default: 3.0, cost_weight: 0.2 },
    ParamSpec { name: "feedback", param: 12, range: (0.0, 1.0), default: 0.3, cost_weight: 0.2 },
    ParamSpec { name: "decay_ms", param: 13, range: (20.0, 3000.0), default: 320.0, cost_weight: 0.2 },
    // 0 = metallic, 1 = tom, 2 = bell (IR FmPercPreset route value).
    ParamSpec { name: "preset", param: 14, range: (0.0, 2.0), default: 0.0, cost_weight: 0.2 },
];
const TEXTURE_PARAMS: [ParamSpec; 4] = [
    // 0 = granulated bed, 1 = vinyl/tape crackle.
    ParamSpec { name: "crackle", param: 44, range: (0.0, 1.0), default: 0.0, cost_weight: 0.25 },
    ParamSpec { name: "density", param: 45, range: (0.0, 0.05), default: 0.002, cost_weight: 0.25 },
    ParamSpec { name: "grain_ms", param: 46, range: (2.0, 200.0), default: 30.0, cost_weight: 0.25 },
    ParamSpec { name: "tone", param: 47, range: (0.0, 1.0), default: 0.5, cost_weight: 0.25 },
];

macro_rules! instrument_plugin {
    ($name:ident, $id:literal, $display:literal, $voice:ty, $capacity:expr, $role:expr, $stem:expr, $cost:expr, $params:expr, $make:expr) => {
        pub struct $name;
        impl InstrumentPlugin for $name {
            fn kind_id(&self) -> &'static str { $id }
            fn display_name(&self) -> &'static str { $display }
            fn params(&self) -> &'static [ParamSpec] { &$params }
            fn cost(&self) -> f32 { $cost }
            fn mix_role(&self) -> MixRole { $role }
            fn stem_index(&self) -> usize { $stem }
            fn pool_capacity(&self) -> usize { $capacity }
            fn make_voice(&self, sample_rate: u32) -> Box<dyn Voice> {
                let make: fn(u32) -> $voice = $make;
                Box::new(make(sample_rate))
            }
        }
    };
}

instrument_plugin!(KickPlugin, "kick", "Kick", Kick, 8, MixRole::Kick, 0, 1.0, KICK_PARAMS, |sr| Kick::new(sr));
/// Hats carry the per-strip choke wiring (#14): every hat voice joins
/// CHOKE_GROUP_HATS via the shared choke state, so open/closed cuts land
/// regardless of pool slot — harness behavior the plugin must reproduce.
pub struct HatPlugin;
impl InstrumentPlugin for HatPlugin {
    fn kind_id(&self) -> &'static str { "hat" }
    fn display_name(&self) -> &'static str { "Hat" }
    fn params(&self) -> &'static [ParamSpec] { &HAT_PARAMS }
    fn cost(&self) -> f32 { PERC_COST }
    fn mix_role(&self) -> MixRole { MixRole::Perc }
    fn stem_index(&self) -> usize { PERC_STEM }
    fn pool_capacity(&self) -> usize { 16 }
    fn make_voice(&self, sample_rate: u32) -> Box<dyn Voice> {
        use kontinuum_core::voice::ChokeState;
        let mut v = Hat::new(sample_rate);
        let choke = ChokeState::shared();
        v.set_choke(std::sync::Arc::clone(&choke), kontinuum_core::voice::CHOKE_GROUP_HATS);
        Box::new(v)
    }
}
instrument_plugin!(ClapPlugin, "clap", "Clap", Clap, 8, MixRole::Perc, PERC_STEM, PERC_COST, CLAP_PARAMS, |sr| Clap::new(sr));
instrument_plugin!(SnarePlugin, "snare", "Snare", Snare, 8, MixRole::Perc, PERC_STEM, PERC_COST, SNARE_PARAMS, |sr| Snare::new(sr));
instrument_plugin!(ShakerPlugin, "shaker", "Shaker", Shaker, 16, MixRole::Perc, PERC_STEM, PERC_COST, SHAKER_PARAMS, |sr| Shaker::new(sr));
instrument_plugin!(BassPlugin, "bass", "Bass", Bass, 4, MixRole::Bass, 1, BASS_COST, BASS_PARAMS, |sr| Bass::new(sr));
instrument_plugin!(AcidPlugin, "acid", "Acid", Acid, 4, MixRole::Bass, 1, BASS_COST, ACID_PARAMS, |sr| Acid::new(sr));
instrument_plugin!(EpPlugin, "ep", "Electric Piano", Ep, 8, MixRole::Pad, PAD_STEM, PAD_COST, EP_PARAMS, |sr| Ep::new(sr));
instrument_plugin!(PadPlugin, "pad", "Pad", Pad, 8, MixRole::Pad, PAD_STEM, PAD_COST, PAD_PARAMS, |sr| Pad::new(sr));
instrument_plugin!(PluckPlugin, "pluck", "Pluck", Pluck, 8, MixRole::Pad, PAD_STEM, PAD_COST, PLUCK_PARAMS, |sr| Pluck::new(sr));
instrument_plugin!(StabPlugin, "stab", "Stab", Stab, 8, MixRole::Pad, PAD_STEM, PAD_COST, STAB_PARAMS, |sr| Stab::new(sr));

/// Wavetable needs its shared, lazily-built table set; the plugin carries it.
pub struct WavetablePlugin;
impl InstrumentPlugin for WavetablePlugin {
    fn kind_id(&self) -> &'static str { "wavetable" }
    fn display_name(&self) -> &'static str { "Wavetable" }
    fn params(&self) -> &'static [ParamSpec] { &WAVETABLE_PARAMS }
    fn cost(&self) -> f32 { PAD_COST }
    fn mix_role(&self) -> MixRole { MixRole::Pad }
    fn stem_index(&self) -> usize { PAD_STEM }
    fn pool_capacity(&self) -> usize { 8 }
    fn make_voice(&self, _sample_rate: u32) -> Box<dyn Voice> {
        Box::new(WavetableVoice::with_set(WavetableSet::shared(), _sample_rate))
    }
}

instrument_plugin!(FmPercPlugin, "fmperc", "FM Perc", FmPerc, 8, MixRole::Perc, PERC_STEM, PERC_COST, FMPERC_PARAMS, |sr| FmPerc::new(sr));
instrument_plugin!(TexturePlugin, "texture", "Texture", Texture, 2, MixRole::Pad, PAD_STEM, PAD_COST, TEXTURE_PARAMS, |sr| Texture::new(sr));

/// The first-party registration table (issue #51: explicit, no
/// life-before-main magic). App targets pass this to the harness.
pub fn registry() -> Registry {
    Registry::new(vec![
        Box::new(KickPlugin),
        Box::new(HatPlugin),
        Box::new(ClapPlugin),
        Box::new(SnarePlugin),
        Box::new(ShakerPlugin),
        Box::new(BassPlugin),
        Box::new(AcidPlugin),
        Box::new(EpPlugin),
        Box::new(PadPlugin),
        Box::new(PluckPlugin),
        Box::new(StabPlugin),
        Box::new(WavetablePlugin),
        Box::new(FmPercPlugin),
        Box::new(TexturePlugin),
    ])
}

/// Control-route id lookup for introspection surfaces (unused by the RT
/// path, which receives `ParamSpec::param` directly).
pub fn param_route(kind_id: &str, name: &str) -> Option<ParamId> {
    registry().get(kind_id)?.params().iter().find(|p| p.name == name).map(|p| p.param)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_exposes_all_kinds_with_distinct_ids() {
        let reg = registry();
        assert_eq!(reg.len(), 14);
        let mut ids: Vec<_> = reg.iter().map(|p| p.kind_id()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 14, "kind ids must be unique");
    }

    #[test]
    fn every_param_spec_stays_inside_its_declared_range() {
        for plugin in registry().iter() {
            for p in plugin.params() {
                assert!(p.range.0 <= p.default && p.default <= p.range.1,
                    "{}.{} default {} outside {:?}", plugin.kind_id(), p.name, p.default, p.range);
            }
        }
    }

    #[test]
    fn every_voice_constructs_and_renders_silent_until_triggered() {
        for plugin in registry().iter() {
            let mut voice = plugin.make_voice(48_000);
            assert!(!voice.is_active(), "{} must start idle", plugin.kind_id());
            let mut buf = [0.0f32; 64];
            voice.render(&mut buf);
            assert!(buf.iter().all(|s| *s == 0.0), "{} must be silent pre-trigger", plugin.kind_id());
        }
    }
}
