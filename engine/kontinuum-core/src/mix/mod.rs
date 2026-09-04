//! Auto-mix facade (issue #27): role-based gain staging, one kick-keyed
//! dynamic node on bass, a broadband kick-sidechain duck per track with
//! role-defaulted depth (#76 — the engine's single ducking implementation),
//! bounded mask carves fed by the #25 critic, and gentle per-bus glue
//! (drums / harmonic). Sits **before** [`crate::MasterChain`]; the loudness
//! war belongs to the mastering crate (#28) — no limiter here.
//!
//! Same rules as the rest of the engine: allocation-free render path,
//! deterministic, no `unsafe`, every adaptive parameter hard-bounded and
//! slewed, telemetry snapshot per processed tile.
//!
//! # Input contracts (for the #25 critic feed)
//!
//! - [`AutoMixer::set_masking`] — per (track, node) energy-overlap
//!   fraction in 0..1; see [`MaskNode`] for semantics. The bands default
//!   to #27's mud (200–500 Hz) and harshness (5–8 kHz) nodes and are
//!   configurable via [`AutoMixer::set_mask_band`].
//! - [`AutoMixer::kick`] — kick-onset sidechain key (velocity 0..1),
//!   fed from the kick track's NoteOn events. Keys every track's duck
//!   (depth gates audibility) and all bass nodes.
//! - Track levels are measured internally from the buffers passed to
//!   [`AutoMixer::process_track`]; when #25 ships per-track LUFS the
//!   measurement seam lives in `servo::LevelFollower`.

mod bus;
mod carve;
mod crossfade;
mod decks;
mod duck;
mod kill;
mod servo;
mod targets;

#[cfg(test)]
mod tests;

pub use bus::{BusChain, BUS_GR_CAP_DB, BUS_RATIO, DRIVE_DEFAULT, DRIVE_MAX, DRIVE_MIN};
pub use carve::{
    BassNode, MaskNode, BASS_BAND_HI_HZ, BASS_BAND_LO_HZ, BASS_CUT_MAX_DB, MASK_CUT_MAX_DB,
    MASK_ENGAGE, MASK_NODES_PER_TRACK,
};
pub use crossfade::{crossfade_frames, equal_power_gains, Crossfade};
pub use decks::{Deck, DeckMixer};
pub use duck::{DuckNode, DUCK_RELEASE_MAX_MS, DUCK_RELEASE_MIN_MS, DUCK_RELEASE_MS};
pub use kill::{KillFade, KillTelemetry, MUTE_FADE_MS, PANIC_FADE_MS};
pub use servo::{GAIN_CORRECTION_MAX_DB, GATE_DBFS, SERVO_SLEW_DB_PER_S};
pub use targets::{BusSide, MixRole, MixTelemetry};

use servo::{LevelFollower, TrackServo};

use crate::MAX_TRACKS;

/// Anchor follower time constant (ms) — slower than track levels, faster
/// than the servo, so the cascade stays stable.
const ANCHOR_TAU_MS: f32 = 1_000.0;

struct TrackChain {
    role: MixRole,
    servo: TrackServo,
    bass: Option<BassNode>,
    duck: DuckNode,
    masks: [MaskNode; MASK_NODES_PER_TRACK],
}

impl TrackChain {
    fn new(sample_rate: u32) -> Self {
        TrackChain {
            role: MixRole::Unassigned,
            servo: TrackServo::new(sample_rate as f32),
            bass: None,
            duck: DuckNode::new(sample_rate),
            masks: [
                MaskNode::new(sample_rate, 200.0, 500.0),
                MaskNode::new(sample_rate, 5_000.0, 8_000.0),
            ],
        }
    }
}

/// The auto-mix engine. Hosted by the graph one track at a time
/// (`process_track`, post-voice pre-pan), then per bus before master.
pub struct AutoMixer {
    sample_rate: u32,
    tracks: Box<[TrackChain]>,
    anchor: LevelFollower,
    anchor_db: f32,
    drums: BusChain,
    harmonic: BusChain,
    telemetry: MixTelemetry,
}

impl AutoMixer {
    pub fn new(sample_rate: u32) -> Self {
        let tracks = (0..MAX_TRACKS).map(|_| TrackChain::new(sample_rate)).collect();
        AutoMixer {
            sample_rate,
            tracks,
            anchor: LevelFollower::new(sample_rate as f32, ANCHOR_TAU_MS),
            anchor_db: 0.0,
            drums: BusChain::new(sample_rate),
            harmonic: BusChain::new(sample_rate),
            telemetry: MixTelemetry::default(),
        }
    }

    /// Assign a track's mix role. Bass tracks get the kick-keyed dynamic
    /// node; every track's duck depth re-defaults to the role's value
    /// (an explicit [`AutoMixer::set_duck_depth`] afterwards wins).
    /// Re-assigning resets the keyed node state (deterministic).
    pub fn set_role(&mut self, track: u8, role: MixRole) {
        if let Some(t) = self.tracks.get_mut(track as usize) {
            t.role = role;
            t.bass = if role == MixRole::Bass { Some(BassNode::new(self.sample_rate)) } else { None };
            t.duck.set_depth(role.duck_depth());
            t.duck.reset();
        }
    }

    pub fn role(&self, track: u8) -> MixRole {
        self.tracks.get(track as usize).map(|t| t.role).unwrap_or(MixRole::Unassigned)
    }

    /// Per-track duck depth 0..1 (clamped; 1 = duck to unity at full key).
    /// This is the per-track mix parameter seam for issue #76: the IR
    /// `Track.duck_depth` lands here through the graph's
    /// `set_track_duck_depth` (absent = the role's default applies).
    pub fn set_duck_depth(&mut self, track: u8, depth: f32) {
        if let Some(t) = self.tracks.get_mut(track as usize) {
            t.duck.set_depth(depth);
        }
    }

    pub fn duck_depth(&self, track: u8) -> f32 {
        self.tracks.get(track as usize).map(|t| t.duck.depth()).unwrap_or(0.0)
    }

    /// Release τ (ms) of every track's duck recovery — the groove/genre
    /// template seam (#76). Core has no template plumbing, so hosts retime
    /// the pump from the template here.
    pub fn set_duck_release_ms(&mut self, ms: f32) {
        let sr = self.sample_rate as f32;
        for t in self.tracks.iter_mut() {
            t.duck.set_release_ms(sr, ms);
        }
    }

    /// The configured duck release τ (clamped), ms — per-track; the setter
    /// above retimes every track at once.
    pub fn duck_release_ms(&self, track: u8) -> f32 {
        self.tracks.get(track as usize).map(|t| t.duck.release_ms()).unwrap_or(0.0)
    }

    /// Reconfigure a mask node's band (Hz), clamped to the sane range with
    /// a minimum width of 10 Hz.
    pub fn set_mask_band(&mut self, track: u8, node: usize, lo_hz: f32, hi_hz: f32) {
        if let Some(t) = self.tracks.get_mut(track as usize) {
            if let Some(m) = t.masks.get_mut(node) {
                m.set_band(self.sample_rate, lo_hz, hi_hz);
            }
        }
    }

    /// #25 critic input: energy overlap 0..1 for (track, node). RT-safe.
    pub fn set_masking(&mut self, track: u8, node: usize, overlap: f32) {
        if let Some(t) = self.tracks.get_mut(track as usize) {
            if let Some(m) = t.masks.get_mut(node) {
                m.set_overlap(overlap);
            }
        }
    }

    /// Kick-onset sidechain key (velocity 0..1) — keys every track's duck
    /// (depth gates audibility) and all bass nodes.
    pub fn kick(&mut self, velocity: f32) {
        for t in self.tracks.iter_mut() {
            t.duck.key_hit(velocity);
            if let Some(b) = t.bass.as_mut() {
                b.key_hit(velocity);
            }
        }
    }

    /// Process one track tile (mono, post-voice/insert, pre-pan): measure,
    /// carve, then apply the staged gain.
    pub fn process_track(&mut self, track: u8, io: &mut [f32]) {
        let Some(t) = self.tracks.get_mut(track as usize) else { return };
        let level_db = t.servo.measure(io);
        if t.role == MixRole::Kick && level_db > GATE_DBFS {
            for &s in io.iter() {
                self.anchor.push(s);
            }
            let anchor_db = self.anchor.level_db();
            if anchor_db > GATE_DBFS {
                self.anchor_db = anchor_db;
            }
        }
        let anchor_db = self.anchor_db;
        if let Some(b) = t.bass.as_mut() {
            b.process(io);
        }
        for m in t.masks.iter_mut() {
            m.process(io);
        }
        t.duck.process(io);
        if t.role != MixRole::Unassigned {
            let dt = io.len() as f32 / self.sample_rate as f32;
            t.servo.update(dt, anchor_db + t.role.target_db());
        }
        let (gain_db, at_bound) = (t.servo.gain_db(), t.servo.at_bound());
        let bass_cut_db = t.bass.as_ref().map(BassNode::cut_db).unwrap_or(0.0);
        for slot in io.iter_mut() {
            *slot *= t.servo.tick_gain();
        }
        let idx = track as usize;
        self.telemetry.track_gain_db[idx] = gain_db;
        if t.bass.is_some() {
            self.telemetry.bass_cut_db = bass_cut_db;
        }
        self.telemetry.mask_cut_db[idx] =
            t.masks.iter().map(|m| m.cut_db()).fold(0.0f32, f32::max);
        self.telemetry.any_gain_at_bound |= at_bound;
        self.telemetry.bass_node_active |= t.bass.as_ref().is_some_and(BassNode::is_active);
        self.telemetry.mask_active |= t.masks.iter().any(MaskNode::is_active);
        self.telemetry.tiles += 1;
    }

    /// Per-bus glue: drums (kick + perc) before the mix bus.
    pub fn process_drum_bus(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.drums.process(left, right);
        self.telemetry.drum_gr_db = self.drums.gr_db();
    }

    /// Per-bus glue: harmonic (bass + pads) before the mix bus.
    pub fn process_harmonic_bus(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.harmonic.process(left, right);
        self.telemetry.harmonic_gr_db = self.harmonic.gr_db();
    }

    pub fn telemetry(&self) -> MixTelemetry {
        self.telemetry
    }

    pub fn set_drum_drive(&mut self, drive: f32) {
        self.drums.set_drive(drive);
    }

    pub fn set_harmonic_drive(&mut self, drive: f32) {
        self.harmonic.set_drive(drive);
    }

    pub fn reset(&mut self) {
        for t in self.tracks.iter_mut() {
            t.servo.reset();
            if let Some(b) = t.bass.as_mut() {
                b.reset();
            }
            t.duck.reset();
            for m in t.masks.iter_mut() {
                m.reset();
            }
        }
        self.anchor.reset();
        self.anchor_db = 0.0;
        self.drums.reset();
        self.harmonic.reset();
        self.telemetry = MixTelemetry::default();
    }
}
