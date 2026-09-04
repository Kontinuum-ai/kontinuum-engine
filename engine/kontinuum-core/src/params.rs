//! Shared `ParamId` constants and routing rules.
//!
//! Routing convention used by [`crate::graph::AudioGraph`]:
//! - `< FX_PARAM_BASE` → instrument voices on the track
//! - `>= FX_PARAM_BASE` → insert FX on the track
//! - `ROUTE_*` → dedicated track smoothers (gain/pan/sends)

use crate::ParamId;

pub const PARAM_TABLE_LEN: usize = 128;
pub const FX_PARAM_BASE: ParamId = 64;

pub const ROUTE_TRACK_GAIN: ParamId = 90;
pub const ROUTE_TRACK_PAN: ParamId = 91;
pub const ROUTE_SEND_DELAY: ParamId = 92;
pub const ROUTE_SEND_REVERB: ParamId = 93;

pub const KICK_TUNE_HZ: ParamId = 0;
pub const KICK_DECAY_MS: ParamId = 1;
pub const KICK_CLICK: ParamId = 2;
pub const KICK_DRIVE: ParamId = 3;

pub const HAT_DECAY_MS: ParamId = 16;
pub const HAT_OPEN: ParamId = 17;
pub const HAT_TONE: ParamId = 18;

pub const BASS_GLIDE_MS: ParamId = 32;
pub const BASS_CUTOFF: ParamId = 33;
pub const BASS_RESONANCE: ParamId = 34;
pub const BASS_WAVE: ParamId = 35;
pub const BASS_ENV_AMT: ParamId = 36;
pub const BASS_ATTACK_MS: ParamId = 37;
pub const BASS_RELEASE_MS: ParamId = 38;

pub const PAD_DETUNE_CENTS: ParamId = 48;
pub const PAD_CUTOFF: ParamId = 49;
pub const PAD_ATTACK_MS: ParamId = 50;
pub const PAD_RELEASE_MS: ParamId = 51;

pub const DELAY_TIME_FRAMES: ParamId = 64;
pub const DELAY_FEEDBACK: ParamId = 65;
pub const DELAY_TONE: ParamId = 66;
pub const DELAY_MIX: ParamId = 67;
pub const REVERB_SIZE: ParamId = 68;
pub const REVERB_DAMP: ParamId = 69;
pub const REVERB_WET: ParamId = 70;
pub const CLAP_DECAY_MS: ParamId = 20;
pub const CLAP_TONE: ParamId = 21;
pub const SNARE_TUNE_HZ: ParamId = 24;
pub const SNARE_DECAY_MS: ParamId = 25;
pub const SNARE_SNAP: ParamId = 26;
pub const SHAKER_DECAY_MS: ParamId = 28;
pub const SHAKER_TONE: ParamId = 29;
pub const ACID_CUTOFF: ParamId = 40;
pub const ACID_RESONANCE: ParamId = 41;
pub const ACID_ENV_AMT: ParamId = 42;
pub const ACID_GLIDE_MS: ParamId = 43;
pub const PLUCK_DAMPING: ParamId = 52;
pub const PLUCK_BRIGHT: ParamId = 53;
pub const STAB_CUTOFF: ParamId = 56;
pub const STAB_DECAY_MS: ParamId = 57;
pub const STAB_DETUNE: ParamId = 58;

pub const EP_DECAY_MS: ParamId = 60;
pub const EP_DEPTH: ParamId = 61;

// Sound roster v2 (#30). Free bands under FX_PARAM_BASE: 4-15 and 44-47.
pub const WAV_POSITION: ParamId = 4;
pub const WAV_DETUNE_CENTS: ParamId = 5;
pub const WAV_OSC2_LEVEL: ParamId = 6;
pub const WAV_SUB: ParamId = 7;
pub const WAV_CUTOFF: ParamId = 8;
pub const WAV_RELEASE_MS: ParamId = 9;

pub const FM_RATIO: ParamId = 10;
pub const FM_INDEX: ParamId = 11;
pub const FM_FEEDBACK: ParamId = 12;
pub const FM_DECAY_MS: ParamId = 13;
pub const FM_PRESET: ParamId = 14;

pub const TEX_MODE: ParamId = 44;
pub const TEX_DENSITY: ParamId = 45;
pub const TEX_GRAIN_MS: ParamId = 46;
pub const TEX_TONE: ParamId = 47;

pub const FILTER_CUTOFF: ParamId = 72;
pub const FILTER_RESONANCE: ParamId = 73;
pub const FILTER_TYPE: ParamId = 74;
pub const SATURATE_DRIVE: ParamId = 75;

pub const CHORUS_RATE: ParamId = 76;
pub const CHORUS_DEPTH: ParamId = 77;
pub const CHORUS_MIX: ParamId = 78;

pub const PHASER_RATE: ParamId = 80;
pub const PHASER_DEPTH: ParamId = 81;
pub const PHASER_FEEDBACK: ParamId = 82;
pub const PHASER_MIX: ParamId = 83;

pub const SHIFT_HZ: ParamId = 88;
pub const SHIFT_MIX: ParamId = 89;

// FX v2 remainder (#30). Bands 84-87 and 94-127 were free under
// FX_PARAM_BASE; 96-99 routes past ROUTE_* (90-93).
/// Tape-delay loop color: all three at 0 keep the clean digital loop
/// (bit-exact with the pre-tape path).
pub const TAPE_WOW: ParamId = 84;
pub const TAPE_FLUTTER: ParamId = 85;
pub const TAPE_SAT: ParamId = 86;
/// Transient designer attack/sustain: 0.5 = neutral, <0.5 cuts,
/// >0.5 boosts.
pub const TRANSIENT_ATTACK: ParamId = 87;
pub const TRANSIENT_SUSTAIN: ParamId = 94;
pub const TRANSIENT_MIX: ParamId = 95;
/// Phaser stage count selector: 0 = 4 stages (default), 1 = 8 stages.
pub const PHASER_STAGES: ParamId = 96;
