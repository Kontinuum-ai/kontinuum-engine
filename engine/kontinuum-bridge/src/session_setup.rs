//! Session → [`AudioGraph`] wiring shared by the live engine facade (this
//! crate) and usable by the offline renderer. Track instruments attach
//! through the plugin registry (#51): the IR `"kind"` picks the plugin, the
//! plugin builds the voice factory and owns the param vocabulary. Sample
//! slots and custom patches have no synth plugin — they attach silent and
//! resolve later (PCM hot-load / #37 follow-up). IR gains snap, IR pan
//! −1..1 maps to mixer 0..1, sends wire, inserts build, and instrument
//! parameters push as immediate `ParamRamp`s.

use kontinuum_core::fx::{Chorus, Delay, FreqShifter, Phaser, Reverb, Saturate, Svf, TransientDesigner};
use kontinuum_core::graph::TrackSwap;
use kontinuum_core::params as core_params;
use kontinuum_core::{AudioGraph, InsertFx, ParamId};
use kontinuum_plugin_api::Registry;
use kontinuum_ir::schema::{InsertDef, InsertKind, InstrumentDef, Session, Track};
use kontinuum_ir::TrackRole;
use kontinuum_schedule::{Event, RampCurve};
use std::sync::Arc;

/// Applies every session-level mixer/instrument setting to the graph. Called
/// from `KontinuumEngine::new` and re-applied after accepted diffs (so
/// `SetInstrumentParam` lands without a restart).
pub fn apply_session_to_graph(graph: &mut AudioGraph, session: &Session, registry: &Registry) {
    let sr = graph.sample_rate();
    graph.set_send_fx(Box::new(Delay::new(sr)), Box::new(Reverb::new(sr)));
    // Issue #76: the session's groove template retimes the pump; per-track
    // duck depths ride the tracks below.
    graph.set_duck_release_ms(session.duck_release_ms);
    for (ti, track) in session.tracks.iter().enumerate() {
        let track_id = u8::try_from(ti).unwrap_or(u8::MAX);
        apply_track(graph, sr, track_id, track, registry);
    }
}

pub(crate) fn apply_track(
    graph: &mut AudioGraph,
    sr: u32,
    track_id: u8,
    track: &Track,
    registry: &Registry,
) {
    if let Some(id) = kind_id_for(track) {
        if let Some(factory) = registry.voice_factory(id) {
            graph.attach_with(track_id, &factory);
        } else {
            graph.attach_silent(track_id);
        }
    } else {
        graph.attach_silent(track_id);
    }
    // Issue #37/#97: a custom patch keeps the role's mix identity from the
    // attach above, but its sound is the patch itself — swap in the
    // PatchVoice pool when the graph compiles. A patch that fails to compile
    // never passes validation, so the role-voice attach is a belt-and-braces
    // fallback, not a live path.
    if let InstrumentDef::Custom(c) = &track.instrument {
        if let Ok(plan) = kontinuum_ir::compile::compile_patch(c) {
            graph.attach_patch(track_id, &plan);
        }
    }
    for (slot, insert) in track.inserts.iter().take(2).enumerate() {
        if let Some(fx) = build_insert(sr, insert) {
            graph.set_insert(track_id, slot, fx);
        }
    }
    // IR gain snaps straight onto the smoother; IR pan (−1..1) maps to the
    // equal-power mixer convention (0..1).
    graph.snap_track_gain(track_id, track.gain);
    graph.snap_track_pan(track_id, (track.pan + 1.0) * 0.5);
    graph.set_track_send(track_id, 0, track.sends.delay);
    graph.set_track_send(track_id, 1, track.sends.reverb);
    // Issue #76: an explicit IR duck depth overrides the role default the
    // attach above installed; `None` keeps that default.
    if let Some(depth) = track.duck_depth {
        graph.set_track_duck_depth(track_id, depth);
    }
    // Instrument values push through the plugin's own param vocabulary:
    // (name, value) pairs from the IR meet ParamSpec lookups from the plugin.
    if let Some(id) = kind_id_for(track) {
        if let Some(p) = registry.get(id) {
            let schema = p.params();
            for (name, value) in track.instrument.param_values() {
                if let Some(spec) = schema.iter().find(|s| s.name == name) {
                    graph.apply_event(
                        track_id,
                        Event::ParamRamp {
                            param: spec.param,
                            target: value,
                            duration_frames: 1,
                            curve: RampCurve::Linear,
                        },
                    );
                }
            }
        }
    }
}

/// The registry kind a track attaches as: the instrument definition when it
/// names a concrete machine; the role's fallback voice for sample/patch
/// tracks (mirrors kontinuum-offline's `kind_id_for` — the #97 PatchVoice
/// swap needs a role attach to give the strip its mix identity).
fn kind_id_for(track: &Track) -> Option<&'static str> {
    track.instrument.kind_id().or_else(|| match &track.instrument {
        InstrumentDef::Sample(_) | InstrumentDef::Custom(_) => Some(match track.role {
            TrackRole::Kick => "kick",
            TrackRole::Perc | TrackRole::Fx => "hat",
            TrackRole::Bass => "bass",
            TrackRole::Pad => "pad",
        }),
        _ => None,
    })
}

/// What the RT thread must attach after an accepted `SwapInstrument` diff
/// (issue #37): a compiled patch plan for `custom`, the registry factory for
/// concrete machines, the role fallback for sample slots. A patch that fails
/// to compile falls back to a silent strip — the diff path already rejects
/// that case, so this is belt-and-braces.
pub(crate) fn swap_for(track: &Track, registry: &Registry) -> TrackSwap {
    if let InstrumentDef::Custom(c) = &track.instrument {
        return kontinuum_ir::compile::compile_patch(c)
            .map(|plan| TrackSwap::Patch(Arc::new(plan)))
            .unwrap_or(TrackSwap::Silent);
    }
    match kind_id_for(track).and_then(|id| registry.voice_factory(id)) {
        Some(factory) => TrackSwap::Factory(factory),
        None => TrackSwap::Silent,
    }
}

/// Builds the insert FX the core graph can host. `drive` → `Saturate`,
/// `filter` → the SVF adapter, and the #30 FX v2 kinds → their core
/// implementations. `Delay`/`Reverb`/`Compressor` stay bus-class (the
/// graph hosts them once via `set_send_fx`, not per track).
fn build_insert(sr: u32, def: &InsertDef) -> Option<Box<dyn InsertFx>> {
    let amount = |key: &str, default: f32| -> f32 {
        def.params.get(key).and_then(|v| v.as_f64()).map_or(default, |v| v as f32)
    };
    match def.kind {
        InsertKind::Drive => Some(Box::new(Saturate::new(amount("amount", 1.2)))),
        InsertKind::Filter => Some(Box::new(SvfAsInsert::new(
            sr,
            amount("cutoff_hz", 1200.0),
            amount("resonance", 0.3),
        ))),
        InsertKind::Chorus => {
            let mut fx = Chorus::new(sr);
            fx.set_param(core_params::CHORUS_RATE, amount("rate_hz", 0.6));
            fx.set_param(core_params::CHORUS_DEPTH, amount("depth", 0.5));
            Some(Box::new(fx))
        }
        InsertKind::Phaser => {
            let mut fx = Phaser::new(sr);
            fx.set_param(core_params::PHASER_RATE, amount("rate_hz", 0.4));
            fx.set_param(core_params::PHASER_DEPTH, amount("depth", 0.6));
            fx.set_param(core_params::PHASER_FEEDBACK, amount("feedback", 0.5));
            fx.set_param(core_params::PHASER_STAGES, amount("stages", 0.0));
            Some(Box::new(fx))
        }
        InsertKind::FreqShifter => {
            let mut fx = FreqShifter::new(sr);
            fx.set_param(core_params::SHIFT_HZ, amount("shift_hz", 0.0));
            Some(Box::new(fx))
        }
        InsertKind::Transient => {
            let mut fx = TransientDesigner::new(sr);
            fx.set_param(core_params::TRANSIENT_ATTACK, amount("attack", 0.5));
            fx.set_param(core_params::TRANSIENT_SUSTAIN, amount("sustain", 0.5));
            Some(Box::new(fx))
        }
        InsertKind::Delay | InsertKind::Reverb | InsertKind::Compressor => {
            let _ = sr;
            None
        }
    }
}

/// Minimal `Svf` adapter so the `filter` insert kind (used by the fixture's
/// `filter_sweep` transitions) has a host-side placeholder that still runs.
struct SvfAsInsert {
    svf: Svf,
}

impl SvfAsInsert {
    fn new(sr: u32, cutoff_hz: f32, resonance: f32) -> Self {
        SvfAsInsert { svf: Svf::new(sr, cutoff_hz, resonance) }
    }
}

impl InsertFx for SvfAsInsert {
    fn render(&mut self, io: &mut [f32]) {
        for s in io.iter_mut() {
            *s = self.svf.process_lowpass(*s);
        }
    }

    fn set_param(&mut self, param: ParamId, value: f32) {
        match param {
            core_params::FILTER_CUTOFF => self.svf.set_cutoff(value),
            core_params::FILTER_RESONANCE => self.svf.set_resonance(value),
            _ => {}
        }
    }

    fn reset(&mut self) {
        self.svf.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #76: the IR's per-track duck depth and the session's template
    /// release must land on the graph — this is the wiring the acceptance
    /// measurement rides.
    #[test]
    fn ir_duck_parameters_reach_the_graph() {
        let session: Session = serde_json::from_str(
            r#"{
            "version": 1, "seed": 7, "duck_release_ms": 300.0,
            "tempo_lane": [[0, 124.0]],
            "sections": [{"id": "a", "bars": 4, "energy_curve": [0.5]}],
            "tracks": [
                {"id": "kick", "role": "kick", "instrument": {"kind": "kick"}},
                {"id": "bass", "role": "bass", "instrument": {"kind": "bass"},
                 "duck_depth": 1.0},
                {"id": "pad", "role": "pad", "instrument": {"kind": "pad"}}
            ]
        }"#,
        )
        .expect("parse");
        let registry = kontinuum_instruments_core::registry();
        let mut graph = AudioGraph::new(48_000);
        apply_session_to_graph(&mut graph, &session, &registry);
        assert_eq!(
            graph.track_duck_depth(1),
            1.0,
            "explicit IR depth must override the role default"
        );
        // No IR depth: the attach's role default (MixRole::Pad → 0.85) stays.
        assert!((graph.track_duck_depth(2) - 0.85).abs() < 1e-6);
        assert_eq!(
            graph.track_duck_release_ms(0),
            300.0,
            "the session's template release must retime the pump"
        );
    }
}
