//! The stable seam between the engine harness and instrument plugins
//! (issue #51). The harness (graph, pools, bridge, offline) depends only on
//! this crate; instruments live in plugin crates (first-party:
//! `kontinuum-instruments-core`) and register explicitly — no life-before-
//! main magic, no dlopen (iOS: native code ships with the binary).
//!
//! RT contract for [`InstrumentPlugin::make_voice`]: the returned voice must
//! be allocation-free after construction, deterministic (identical triggers
//! → identical samples), and hard-mute below `kontinuum_core::SILENCE_ABS`.

use kontinuum_core::{MixRole, ParamId, Voice};

/// One declarative parameter: the single source of truth for validation,
/// defaults, the control-route ParamId, and (via `cost_weight`) the
/// estimator. Replaces the hand-written per-kind bounds/params/validator
/// matches this architecture retires.
#[derive(Clone, Copy, Debug)]
pub struct ParamSpec {
    pub name: &'static str,
    /// Control-route id (see `kontinuum_core::params`).
    pub param: ParamId,
    pub range: (f32, f32),
    pub default: f32,
    /// Relative CPU weight of this parameter's stage (0.0 = free). The
    /// estimator sums these per active voice; v0 keeps it coarse.
    pub cost_weight: f32,
}

/// A first-party instrument: DSP voice factory + everything the harness
/// needs to route, mix, budget and validate it — declaratively.
pub trait InstrumentPlugin: Send + Sync {
    /// Stable id, == the IR `"kind"` discriminant (e.g. "kick").
    fn kind_id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn params(&self) -> &'static [ParamSpec];
    /// Per-voice CPU weight for the compile-time budget (#11's table).
    fn cost(&self) -> f32;
    fn mix_role(&self) -> MixRole;
    /// Stem-tap bus (kick 0, bass 1, perc 2, pad 3).
    fn stem_index(&self) -> usize;
    /// Voice pool capacity for the strip.
    fn pool_capacity(&self) -> usize;
    /// Build one voice. Called on the control thread at attach time; the
    /// returned voice then lives on the RT thread.
    fn make_voice(&self, sample_rate: u32) -> Box<dyn Voice>;
}

/// Explicit registration table. Constructed per app target; lookup is by
/// the IR `"kind"` discriminant.
pub struct Registry {
    plugins: Vec<std::sync::Arc<dyn InstrumentPlugin>>,
}

impl Registry {
    /// Empty harness: compiles, boots, plays silence (#51).
    pub fn empty() -> Self {
        Registry { plugins: Vec::new() }
    }

    pub fn new(plugins: Vec<Box<dyn InstrumentPlugin>>) -> Self {
        Registry { plugins: plugins.into_iter().map(std::sync::Arc::from).collect() }
    }

    pub fn get(&self, kind_id: &str) -> Option<&dyn InstrumentPlugin> {
        self.plugins.iter().find(|p| p.kind_id() == kind_id).map(|p| p.as_ref())
    }

    pub fn iter(&self) -> impl Iterator<Item = &dyn InstrumentPlugin> {
        self.plugins.iter().map(|p| p.as_ref())
    }

    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }
}

impl Registry {
    /// Build the harness-side factory for `kind_id`: a `'static`, cloneable
    /// voice source carrying the plugin's routing metadata. The graph
    /// consumes this struct — it never sees the trait.
    pub fn voice_factory(&self, kind_id: &str) -> Option<kontinuum_core::graph::VoiceFactory> {
        let plugin: std::sync::Arc<dyn InstrumentPlugin> =
            self.plugins.iter().find(|p| p.kind_id() == kind_id)?.clone();
        Some(kontinuum_core::graph::VoiceFactory {
            kind_id: plugin.kind_id(),
            capacity: plugin.pool_capacity(),
            stem: plugin.stem_index(),
            role: plugin.mix_role(),
            make: std::sync::Arc::new(move |sr| plugin.make_voice(sr)),
        })
    }
}
