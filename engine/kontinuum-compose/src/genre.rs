//! Genre specs v2 (issues #87/#88): how a named style shapes everything the
//! generator does — tempo band, groove and swing, the rack it is performed
//! by, the key it lives in, the structure and energy of its arrangement.
//!
//! History: an earlier revision carried only `max_concurrent` and `dev_bind`
//! probabilities, so "acid" and "ambient" fell through to a default rig and
//! produced byte-identical sessions. v1 added tempo/swing/groove/backbeat —
//! but kept one shared 4-track rig and a hardcoded F minor. v2 makes every
//! dimension in #87's table a per-genre fact:
//!
//! | dimension   | where it lives here                        |
//! |-------------|--------------------------------------------|
//! | tempo       | `bpm` (default) + `bpm_range` (tendency)   |
//! | swing       | `swing` range, drawn seeded per session    |
//! | groove      | `grooves` pool, drawn seeded per session   |
//! | rig         | `rack` — the voices the engine plays (#88) |
//! | patterns    | `hats` idiom + `bass_pool` archetypes       |
//! | key/scale   | `keys` tendency pool                       |
//! | structure   | `dev_count` / `breakdown_count` ranges      |
//! | energy      | `energy_bias` shift on every section curve  |
//!
//! Concurrency caps still encode restraint — "everything at once is a bug".
//! `dev_bind` probabilities are per binding *class* (see [`BindProbs`]); the
//! days of the hardcoded (bass, perc, pad) triple are over — sections bind
//! by class over whatever the rack contains.

use kontinuum_ir::schema::{InstrumentDef, MusicalKey, Sends};
use kontinuum_ir::TrackRole;

/// Tracks in the legacy full rig — still the cap reference for the widest
/// default rack.
pub(crate) const FULL_RIG: usize = 6;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Style {
    Microhouse,
    Minimal,
    Techno,
    DeepHouse,
    House,
    Acid,
    DubTechno,
    Ambient,
    Default,
}

/// What the *closed* hats do between the kicks. Open-hat voices (the rack
/// entry's instrument carries `open: true`) always play the offbeat-eighth
/// line — the "and" of every beat, THE house signature — regardless of this
/// field.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum HatIdiom {
    /// Straight 16ths with a velocity contour — the techno/house engine room.
    Sixteenths,
    /// Sparse, seeded placement: micro-detail rather than a pulse.
    Euclid,
}

/// How a rack entry participates in the arrangement. Section binding
/// generalizes over classes: a section asks for "the spine, the pulse, one
/// low voice" rather than for hardcoded track ids, so any rack — including
/// ambient's kickless one — binds correctly.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum BindClass {
    /// The groove's key source (the kick). Sections are built around it.
    Spine,
    /// What lands on beats 2 and 4 (clap/snare).
    Backbeat,
    /// The timekeeping surface between the kicks (hats, shaker).
    Pulse,
    /// The low end (bass, acid line).
    Low,
    /// Harmonic colour (pad, EP, stab, pluck).
    Harmony,
    /// Beatless atmosphere (sample texture).
    Texture,
}

impl BindClass {
    /// Dev-section overage is dropped harmonic-and-up: the groove spine is
    /// never the thing that gets dropped (issue #52 WS1). Lower rank = dropped
    /// first.
    pub(crate) fn drop_rank(self) -> u8 {
        match self {
            BindClass::Harmony => 0,
            BindClass::Texture => 1,
            BindClass::Low => 2,
            BindClass::Pulse => 3,
            BindClass::Backbeat | BindClass::Spine => 4,
        }
    }
}

/// The twelve engine voices (#88): every rack is built from this palette —
/// the same vocabulary the IR and the engine's plugin registry speak.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Voice {
    Kick,
    Hat,
    Shaker,
    Clap,
    /// Part of the 12-voice palette (the backbeat dispatch covers it); no
    /// built-in rack carries a snare today, but pack data (#51) can.
    #[allow(dead_code)]
    Snare,
    Bass,
    Acid,
    Pad,
    Ep,
    Pluck,
    Stab,
    Texture,
}

/// One slot in a genre's rack (#88): which voice, what it does in the
/// arrangement, and the mixer/FX defaults it ships with. Per-rack mixer
/// identity lives HERE — dub techno's long delay send is part of the rack,
/// not a knob the arrangement re-derives.
pub(crate) struct RackEntry {
    /// Engine track id — the canonical vocabulary sessions and the app's
    /// rack UI are keyed on (issue #89).
    pub id: &'static str,
    pub voice: Voice,
    pub role: TrackRole,
    pub class: BindClass,
    /// Extra dev-section draw *inside* the entry's class (1.0 = bound
    /// whenever its class is). Secondary colours — a second pulse voice, a
    /// stab behind the EP — sit below 1.0.
    pub chance: f32,
    pub inst: InstrumentDef,
    pub gain: f32,
    pub pan: f32,
    pub sends: Sends,
    /// Kick-sidechain duck depth; `None` = the engine's role default (#76).
    pub duck: Option<f32>,
    /// Sample-slot query for [`Voice::Texture`] entries (empty otherwise);
    /// materialized into the track's `SampleSlot` by the palette.
    pub sample_query: &'static str,
}

/// Dev-section binding probabilities per binding class. Density nudges from
/// taste (`GenParams::density`) shift these, centred on 0.6.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct BindProbs {
    pub low: f32,
    pub pulse: f32,
    pub harmony: f32,
    pub texture: f32,
}

#[derive(Clone, Copy)]
pub(crate) struct GenreSpec {
    pub style: Style,
    /// Hard cap on concurrently bound tracks per section.
    pub max_concurrent: usize,
    pub dev_bind: BindProbs,
    /// Tempo the style sits at, BPM — the genre-typical default.
    pub bpm: f64,
    /// The band the style lives in. Generation runs at `bpm`; a caller-stated
    /// tempo still wins outright (`GenParams::bpm`, the #38 live-move
    /// contract). The range is the documented tendency the style packs
    /// (#56) serialize — carried as data, asserted against the default.
    #[allow(dead_code)]
    pub bpm_range: (f64, f64),
    /// Swing on the 16th grid, drawn seeded in this range per session.
    /// `(0.0, 0.0)` is straight time and means it.
    pub swing: (f32, f32),
    /// Groove-template pool (see [`crate::groove::ALL`]); drawn seeded per
    /// session. An explicit `GenParams::groove` pin outranks the draw.
    pub grooves: &'static [&'static str],
    /// The rig (#88): exactly the tracks this style is performed by.
    pub rack: &'static [RackEntry],
    pub hats: HatIdiom,
    /// Bass-archetype pool (see [`crate::bass::ALL`]); drawn seeded per
    /// session so the whole record rides one coherent low-end idiom. Empty =
    /// free energy-weighted draw (the legacy behaviour).
    pub bass_pool: &'static [&'static str],
    /// Key/scale tendencies. The seeded pick stamps `Session::key` and
    /// transposes the whole progression to the tonic.
    pub keys: &'static [MusicalKey],
    /// Section-structure tendencies: dev/breakdown count ranges (inclusive).
    pub dev_count: (u32, u32),
    pub breakdown_count: (u32, u32),
    /// Energy-curve tendency: shifts every drawn section energy. Ambient
    /// sits low, techno pushes.
    pub energy_bias: f32,
    /// Kick/bass downbeat collision policy (issue #17): microhouse keeps
    /// the sub off the kick, driving techno stacks them on purpose.
    pub downbeat_collision: kontinuum_ir::schema::DownbeatCollision,
    /// Chord colour for the 3-voice harmony material (issue #17): the
    /// modal/sus2/quartal tint the pad, EP and stab voicings take.
    pub harmony_color: crate::harmony::Extension,
    /// Kick-sidechain duck release τ in ms (issue #76).
    pub duck_release_ms: f32,
}

use Voice::{Acid, Bass, Clap, Ep, Hat, Kick, Pad, Pluck, Shaker, Stab, Texture};

const DUCK: Option<f32> = Some(1.0);

// -- Rack entries -----------------------------------------------------------
// Shared voice shapes live in the small constructors below so the rack
// tables read as racks, not parameter dumps. Per-genre deviations (deep's
// soft kick, dub's dark pad) are written inline in that genre's table — the
// deviation IS the identity.

const fn kick(id: &'static str, tune: f32, decay: f32, click: f32, class: BindClass) -> RackEntry {
    RackEntry {
        id,
        voice: Kick,
        role: TrackRole::Kick,
        class,
        chance: 1.0,
        inst: InstrumentDef::Kick(kontinuum_ir::schema::KickInstrument {
            kind: kontinuum_ir::schema::KickTag::Kick,
            tune_hz: tune,
            decay_ms: decay,
            click,
            drive: 0.3,
        }),
        gain: 1.0,
        pan: 0.0,
        sends: Sends { delay: 0.0, reverb: 0.05 },
        duck: None,
        sample_query: "",
    }
}

const fn bass(id: &'static str, cutoff: f32, gain: f32, chance: f32) -> RackEntry {
    RackEntry {
        id,
        voice: Bass,
        role: TrackRole::Bass,
        class: BindClass::Low,
        chance,
        inst: InstrumentDef::Bass(kontinuum_ir::schema::BassInstrument {
            kind: kontinuum_ir::schema::BassTag::Bass,
            cutoff_hz: cutoff,
            resonance: 0.3,
            wave: kontinuum_ir::schema::Wave::Saw,
            glide_ms: 40.0,
        }),
        gain,
        pan: 0.0,
        sends: Sends { delay: 0.0, reverb: 0.0 },
        duck: DUCK,
        sample_query: "",
    }
}

const fn closed_hat(id: &'static str, decay: f32, tone: f32, gain: f32, pan: f32) -> RackEntry {
    RackEntry {
        id,
        voice: Hat,
        role: TrackRole::Perc,
        class: BindClass::Pulse,
        chance: 1.0,
        inst: InstrumentDef::Hat(kontinuum_ir::schema::HatInstrument {
            kind: kontinuum_ir::schema::HatTag::Hat,
            decay_ms: decay,
            tone,
            open: false,
        }),
        gain,
        pan,
        sends: Sends { delay: 0.2, reverb: 0.1 },
        duck: DUCK,
        sample_query: "",
    }
}

const fn open_hat(id: &'static str, decay: f32, gain: f32, pan: f32, class: BindClass) -> RackEntry {
    RackEntry {
        id,
        voice: Hat,
        role: TrackRole::Perc,
        class,
        chance: 1.0,
        inst: InstrumentDef::Hat(kontinuum_ir::schema::HatInstrument {
            kind: kontinuum_ir::schema::HatTag::Hat,
            decay_ms: decay,
            tone: 0.5,
            open: true,
        }),
        gain,
        pan,
        sends: Sends { delay: 0.15, reverb: 0.18 },
        duck: DUCK,
        sample_query: "",
    }
}

const fn shaker(id: &'static str, gain: f32, pan: f32, chance: f32) -> RackEntry {
    RackEntry {
        id,
        voice: Shaker,
        role: TrackRole::Perc,
        class: BindClass::Pulse,
        chance,
        inst: InstrumentDef::Shaker(kontinuum_ir::schema::ShakerInstrument {
            kind: kontinuum_ir::schema::ShakerTag::Shaker,
            decay_ms: 70.0,
            tone: 0.5,
        }),
        gain,
        pan,
        sends: Sends { delay: 0.1, reverb: 0.08 },
        duck: DUCK,
        sample_query: "",
    }
}

const fn clap(id: &'static str, decay: f32, tone: f32, gain: f32) -> RackEntry {
    RackEntry {
        id,
        voice: Clap,
        role: TrackRole::Perc,
        class: BindClass::Backbeat,
        chance: 1.0,
        inst: InstrumentDef::Clap(kontinuum_ir::schema::ClapInstrument {
            kind: kontinuum_ir::schema::ClapTag::Clap,
            decay_ms: decay,
            tone,
        }),
        gain,
        pan: 0.0,
        sends: Sends { delay: 0.1, reverb: 0.3 },
        duck: DUCK,
        sample_query: "",
    }
}

const fn stab(id: &'static str, cutoff: f32, decay: f32, detune: f32, delay: f32, gain: f32, chance: f32) -> RackEntry {
    RackEntry {
        id,
        voice: Stab,
        role: TrackRole::Pad,
        class: BindClass::Harmony,
        chance,
        inst: InstrumentDef::Stab(kontinuum_ir::schema::StabInstrument {
            kind: kontinuum_ir::schema::StabTag::Stab,
            cutoff_hz: cutoff,
            decay_ms: decay,
            detune_cents: detune,
        }),
        gain,
        pan: -0.15,
        sends: Sends { delay, reverb: 0.25 },
        duck: DUCK,
        sample_query: "",
    }
}

const fn ep(id: &'static str, decay: f32, depth: f32, gain: f32, chance: f32) -> RackEntry {
    RackEntry {
        id,
        voice: Ep,
        role: TrackRole::Pad,
        class: BindClass::Harmony,
        chance,
        inst: InstrumentDef::Ep(kontinuum_ir::schema::EpInstrument {
            kind: kontinuum_ir::schema::EpTag::Ep,
            decay_ms: decay,
            depth,
        }),
        gain,
        pan: -0.2,
        sends: Sends { delay: 0.1, reverb: 0.35 },
        duck: DUCK,
        sample_query: "",
    }
}

const fn pluck(id: &'static str, damping: f32, bright: f32, gain: f32, chance: f32) -> RackEntry {
    RackEntry {
        id,
        voice: Pluck,
        role: TrackRole::Pad,
        class: BindClass::Harmony,
        chance,
        inst: InstrumentDef::Pluck(kontinuum_ir::schema::PluckInstrument {
            kind: kontinuum_ir::schema::PluckTag::Pluck,
            damping,
            bright,
        }),
        gain,
        pan: 0.3,
        sends: Sends { delay: 0.2, reverb: 0.3 },
        duck: DUCK,
        sample_query: "",
    }
}

const fn pad(id: &'static str, attack: f32, release: f32, cutoff: f32, gain: f32, chance: f32) -> RackEntry {
    RackEntry {
        id,
        voice: Pad,
        role: TrackRole::Pad,
        class: BindClass::Harmony,
        chance,
        inst: InstrumentDef::Pad(kontinuum_ir::schema::PadInstrument {
            kind: kontinuum_ir::schema::PadTag::Pad,
            attack_ms: attack,
            release_ms: release,
            detune_cents: 12.0,
            cutoff_hz: cutoff,
        }),
        gain,
        pan: -0.2,
        sends: Sends { delay: 0.1, reverb: 0.35 },
        duck: DUCK,
        sample_query: "",
    }
}

const fn acid_line() -> RackEntry {
    RackEntry {
        id: "acid",
        voice: Acid,
        role: TrackRole::Bass,
        class: BindClass::Low,
        chance: 1.0,
        inst: InstrumentDef::Acid(kontinuum_ir::schema::AcidInstrument {
            kind: kontinuum_ir::schema::AcidTag::Acid,
            cutoff_hz: 750.0,
            resonance: 0.55,
            env_amt: 3.0,
            glide_ms: 60.0,
        }),
        gain: 0.95,
        pan: 0.0,
        sends: Sends { delay: 0.08, reverb: 0.06 },
        duck: DUCK,
        sample_query: "",
    }
}

const fn texture() -> RackEntry {
    RackEntry {
        id: "texture",
        voice: Texture,
        role: TrackRole::Fx,
        class: BindClass::Texture,
        chance: 0.9,
        inst: InstrumentDef::Sample(kontinuum_ir::schema::SampleSlot {
            kind: kontinuum_ir::schema::SampleTag::Sample,
            query: None,
            id: None,
            recipe_hash: None,
            transpose: None,
            fine: None,
            stretch: None,
            choke_group: None,
            granular: None,
        }),
        gain: 0.4,
        pan: 0.0,
        sends: Sends { delay: 0.3, reverb: 0.5 },
        duck: DUCK,
        sample_query: "atmospheric texture",
    }
}

// -- The racks (#88) ---------------------------------------------------------
// One table per style, in performance order (spine first). These are the
// issue's suggested racks, tuned into track parameters.

/// The legacy full rig: the default (unnamed-genre) rack, unchanged so
/// sessions without a style keep their sound.
static DEFAULT_RACK: [RackEntry; 6] = [
    kick("kick", 52.0, 300.0, 0.4, BindClass::Spine),
    clap("clap", 280.0, 0.6, 0.72),
    closed_hat("perc", 55.0, 0.55, 0.62, 0.3),
    open_hat("ohat", 90.0, 0.5, -0.25, BindClass::Pulse),
    bass("bass", 900.0, 0.9, 1.0),
    pad("pad", 600.0, 1200.0, 2400.0, 0.7, 1.0),
];

/// minimal techno: kick · hat · sub bass · sparse stab.
static MINIMAL_RACK: [RackEntry; 4] = [
    kick("kick", 56.0, 250.0, 0.55, BindClass::Spine),
    closed_hat("perc", 45.0, 0.62, 0.6, 0.4),
    bass("bass", 700.0, 0.95, 1.0),
    stab("stab", 2200.0, 320.0, 8.0, 0.2, 0.45, 0.55),
];

/// techno: kick · hat · clap · sub bass · dark stab.
static TECHNO_RACK: [RackEntry; 5] = [
    kick("kick", 52.0, 300.0, 0.4, BindClass::Spine),
    closed_hat("perc", 55.0, 0.55, 0.62, 0.3),
    clap("clap", 240.0, 0.68, 0.72),
    bass("bass", 800.0, 0.9, 1.0),
    stab("stab", 1600.0, 500.0, 12.0, 0.15, 0.5, 0.7),
];

/// deep house: soft kick · open hat · shaker · sub bass · EP chords · stab.
static DEEP_HOUSE_RACK: [RackEntry; 6] = [
    kick("kick", 52.0, 180.0, 0.25, BindClass::Spine),
    open_hat("perc", 90.0, 0.5, 0.25, BindClass::Pulse),
    shaker("shaker", 0.5, -0.3, 0.9),
    bass("bass", 800.0, 0.9, 1.0),
    ep("ep", 1600.0, 2.2, 0.6, 1.0),
    stab("stab", 2400.0, 380.0, 10.0, 0.25, 0.45, 0.6),
];

/// house: kick · open hat · clap · bass · EP.
static HOUSE_RACK: [RackEntry; 5] = [
    kick("kick", 52.0, 180.0, 0.4, BindClass::Spine),
    open_hat("perc", 90.0, 0.5, 0.25, BindClass::Pulse),
    clap("clap", 300.0, 0.62, 0.72),
    bass("bass", 900.0, 0.9, 1.0),
    ep("ep", 1400.0, 2.4, 0.6, 0.85),
];

/// microhouse: tight kick · hat · shaker · sub bass · micro-pluck.
static MICROHOUSE_RACK: [RackEntry; 5] = [
    kick("kick", 56.0, 250.0, 0.55, BindClass::Spine),
    closed_hat("perc", 45.0, 0.62, 0.6, 0.4),
    shaker("shaker", 0.5, -0.35, 0.85),
    bass("bass", 700.0, 0.95, 1.0),
    pluck("pluck", 0.35, 0.65, 0.42, 0.6),
];

/// acid: kick · hat · the 303 line front and center.
static ACID_RACK: [RackEntry; 3] = [
    kick("kick", 54.0, 280.0, 0.5, BindClass::Spine),
    closed_hat("perc", 50.0, 0.6, 0.55, 0.3),
    acid_line(),
];

/// dub techno: kick · hat · sub bass · dark pad · stab through a long delay.
/// The pad sits ahead of the stab: it is the breakdown voice, and the first
/// harmony entry is what breakdowns bind.
static DUB_RACK: [RackEntry; 5] = [
    kick("kick", 50.0, 320.0, 0.3, BindClass::Spine),
    closed_hat("perc", 60.0, 0.5, 0.55, 0.0),
    bass("bass", 600.0, 0.9, 1.0),
    pad("pad", 1200.0, 2400.0, 1500.0, 0.5, 0.8),
    // The long delay send is the style's identity — echo space, not a mix
    // afterthought.
    stab("stab", 1400.0, 900.0, 8.0, 0.6, 0.5, 0.85),
];

/// ambient: NO kick · pad · EP · pluck · texture sample. Beatless.
static AMBIENT_RACK: [RackEntry; 4] = [
    pad("pad", 2400.0, 6000.0, 1800.0, 0.6, 1.0),
    ep("ep", 3000.0, 1.8, 0.45, 0.85),
    pluck("pluck", 0.7, 0.4, 0.4, 0.8),
    texture(),
];

const MICROHOUSE: GenreSpec = GenreSpec {
    style: Style::Microhouse,
    max_concurrent: 5,
    // Micro-percussion is the genre: this style is sparse underneath and busy
    // on top, so the hats are the last thing to go, not the first.
    dev_bind: BindProbs { low: 0.9, pulse: 0.95, harmony: 0.45, texture: 0.3 },
    bpm: 125.0,
    bpm_range: (120.0, 128.0),
    swing: (0.10, 0.18),
    grooves: &["mpc-ish", "tense"],
    rack: &MICROHOUSE_RACK,
    hats: HatIdiom::Euclid,
    bass_pool: &["offbeat-eighths", "call-response"],
    keys: &[MusicalKey::FMinor, MusicalKey::EMinor, MusicalKey::GMinor],
    dev_count: (2, 3),
    breakdown_count: (0, 1),
    energy_bias: -0.03,
    downbeat_collision: kontinuum_ir::schema::DownbeatCollision::Avoid,
    harmony_color: crate::harmony::Extension::Sus2,
    duck_release_ms: 160.0,
};

const MINIMAL: GenreSpec = GenreSpec {
    style: Style::Minimal,
    max_concurrent: 4,
    dev_bind: BindProbs { low: 0.9, pulse: 0.92, harmony: 0.4, texture: 0.3 },
    bpm: 128.0,
    bpm_range: (124.0, 134.0),
    swing: (0.02, 0.06),
    grooves: &["tense", "straight-machine"],
    rack: &MINIMAL_RACK,
    hats: HatIdiom::Euclid,
    bass_pool: &["rolling-16ths", "offbeat-eighths"],
    keys: &[MusicalKey::FMinor, MusicalKey::GMinor],
    dev_count: (2, 4),
    breakdown_count: (0, 1),
    energy_bias: 0.0,
    downbeat_collision: kontinuum_ir::schema::DownbeatCollision::Avoid,
    harmony_color: crate::harmony::Extension::Quartal,
    duck_release_ms: 150.0,
};

const TECHNO: GenreSpec = GenreSpec {
    style: Style::Techno,
    max_concurrent: 5,
    dev_bind: BindProbs { low: 0.95, pulse: 0.9, harmony: 0.55, texture: 0.4 },
    bpm: 132.0,
    bpm_range: (128.0, 140.0),
    swing: (0.0, 0.0),
    grooves: &["straight-machine", "pushed-hats"],
    rack: &TECHNO_RACK,
    hats: HatIdiom::Sixteenths,
    bass_pool: &["rolling-16ths", "offbeat-eighths"],
    keys: &[MusicalKey::GMinor, MusicalKey::AMinor, MusicalKey::FMinor],
    dev_count: (2, 4),
    breakdown_count: (0, 2),
    energy_bias: 0.02,
    downbeat_collision: kontinuum_ir::schema::DownbeatCollision::Allow,
    harmony_color: crate::harmony::Extension::Triad,
    duck_release_ms: 140.0,
};

const DEEP_HOUSE: GenreSpec = GenreSpec {
    style: Style::DeepHouse,
    max_concurrent: FULL_RIG,
    dev_bind: BindProbs { low: 0.95, pulse: 0.9, harmony: 0.85, texture: 0.4 },
    bpm: 122.0,
    bpm_range: (118.0, 126.0),
    swing: (0.12, 0.18),
    grooves: &["laid-back", "mpc-ish"],
    rack: &DEEP_HOUSE_RACK,
    hats: HatIdiom::Sixteenths,
    bass_pool: &["offbeat-eighths", "dub-sub"],
    keys: &[MusicalKey::DMinor, MusicalKey::AMinor, MusicalKey::GMinor],
    dev_count: (2, 4),
    breakdown_count: (0, 1),
    energy_bias: -0.01,
    downbeat_collision: kontinuum_ir::schema::DownbeatCollision::DuckOnly,
    harmony_color: crate::harmony::Extension::Seventh,
    duck_release_ms: 220.0,
};

const HOUSE: GenreSpec = GenreSpec {
    style: Style::House,
    max_concurrent: 5,
    dev_bind: BindProbs { low: 0.95, pulse: 0.9, harmony: 0.7, texture: 0.4 },
    bpm: 124.0,
    bpm_range: (120.0, 128.0),
    swing: (0.08, 0.15),
    grooves: &["mpc-ish", "drunk-shuffle"],
    rack: &HOUSE_RACK,
    hats: HatIdiom::Sixteenths,
    bass_pool: &["offbeat-eighths", "rolling-16ths"],
    keys: &[MusicalKey::AMinor, MusicalKey::CMinor, MusicalKey::FMinor],
    dev_count: (2, 4),
    breakdown_count: (0, 1),
    energy_bias: 0.01,
    downbeat_collision: kontinuum_ir::schema::DownbeatCollision::DuckOnly,
    harmony_color: crate::harmony::Extension::Seventh,
    duck_release_ms: 170.0,
};

const ACID: GenreSpec = GenreSpec {
    style: Style::Acid,
    max_concurrent: 3,
    dev_bind: BindProbs { low: 1.0, pulse: 0.9, harmony: 0.25, texture: 0.2 },
    bpm: 130.0,
    bpm_range: (125.0, 138.0),
    swing: (0.0, 0.04),
    grooves: &["straight-machine", "tense"],
    rack: &ACID_RACK,
    hats: HatIdiom::Sixteenths,
    bass_pool: &["acid-slide"],
    keys: &[MusicalKey::FMinor, MusicalKey::GMinor, MusicalKey::CMinor],
    dev_count: (2, 4),
    breakdown_count: (0, 1),
    energy_bias: 0.02,
    downbeat_collision: kontinuum_ir::schema::DownbeatCollision::Allow,
    harmony_color: crate::harmony::Extension::Triad,
    duck_release_ms: 140.0,
};

const DUB_TECHNO: GenreSpec = GenreSpec {
    style: Style::DubTechno,
    max_concurrent: 5,
    dev_bind: BindProbs { low: 0.95, pulse: 0.9, harmony: 0.75, texture: 0.4 },
    bpm: 120.0,
    bpm_range: (116.0, 126.0),
    swing: (0.0, 0.03),
    grooves: &["straight-machine", "laid-back"],
    rack: &DUB_RACK,
    hats: HatIdiom::Sixteenths,
    bass_pool: &["dub-sub"],
    keys: &[MusicalKey::CMinor, MusicalKey::GMinor, MusicalKey::FMinor],
    dev_count: (2, 3),
    breakdown_count: (0, 1),
    energy_bias: -0.02,
    downbeat_collision: kontinuum_ir::schema::DownbeatCollision::DuckOnly,
    harmony_color: crate::harmony::Extension::Quartal,
    duck_release_ms: 200.0,
};

const AMBIENT: GenreSpec = GenreSpec {
    style: Style::Ambient,
    max_concurrent: 4,
    dev_bind: BindProbs { low: 0.0, pulse: 0.0, harmony: 0.95, texture: 0.85 },
    bpm: 84.0,
    bpm_range: (72.0, 96.0),
    swing: (0.0, 0.0),
    grooves: &["straight-machine"],
    rack: &AMBIENT_RACK,
    hats: HatIdiom::Euclid,
    bass_pool: &[],
    keys: &[MusicalKey::AMinor, MusicalKey::DMinor, MusicalKey::EMinor],
    dev_count: (1, 3),
    breakdown_count: (1, 2),
    energy_bias: -0.15,
    downbeat_collision: kontinuum_ir::schema::DownbeatCollision::DuckOnly,
    harmony_color: crate::harmony::Extension::Ninth,
    duck_release_ms: 300.0,
};

const DEFAULT: GenreSpec = GenreSpec {
    style: Style::Default,
    max_concurrent: FULL_RIG,
    dev_bind: BindProbs { low: 0.9, pulse: 0.9, harmony: 0.6, texture: 0.4 },
    bpm: 126.0,
    bpm_range: (120.0, 132.0),
    swing: (0.04, 0.08),
    grooves: &["straight-machine"],
    rack: &DEFAULT_RACK,
    hats: HatIdiom::Sixteenths,
    bass_pool: &[],
    keys: &[MusicalKey::FMinor],
    dev_count: (2, 4),
    breakdown_count: (0, 2),
    energy_bias: 0.0,
    downbeat_collision: kontinuum_ir::schema::DownbeatCollision::DuckOnly,
    harmony_color: crate::harmony::Extension::Seventh,
    duck_release_ms: 160.0,
};

/// Resolves a style name (as passed in [`crate::GenParams::genre`]) to its
/// spec. Unknown names fall back to the default rig. Match order matters:
/// "minimal techno" contains "techno", "dub techno" contains "techno", and
/// "microhouse" contains "house" — the more specific style wins.
pub(crate) fn spec_for(genre: Option<&str>) -> GenreSpec {
    let Some(g) = genre else { return DEFAULT };
    let g = g.to_lowercase();
    if g.contains("microhouse") || g.contains("micro") {
        MICROHOUSE
    } else if g.contains("minimal") {
        MINIMAL
    } else if g.contains("dub") {
        DUB_TECHNO
    } else if g.contains("acid") {
        ACID
    } else if g.contains("techno") || g.contains("tech") {
        TECHNO
    } else if g.contains("deep") {
        DEEP_HOUSE
    } else if g.contains("house") {
        HOUSE
    } else if g.contains("ambient") {
        AMBIENT
    } else {
        DEFAULT
    }
}

/// True when [`spec_for`] recognises the name as one of the shipped styles
/// (rather than falling through to the default rig), so callers can tell
/// "this style owns its tempo" from "nothing here matched".
pub(crate) fn names_a_style(genre: &str) -> bool {
    let g = genre.to_lowercase();
    ["micro", "minimal", "techno", "tech", "deep", "house", "acid", "dub", "ambient"]
        .iter()
        .any(|k| g.contains(k))
}

impl GenreSpec {
    /// Harmony felt-not-heard (issue #52 WS2): short chord gates, low pad
    /// levels — the mid budget belongs to the low end and micro-detail.
    pub(crate) fn restrained_harmony(self) -> bool {
        matches!(self.style, Style::Microhouse | Style::Minimal)
    }

    /// The section-binding probability for a class.
    pub(crate) fn bind_prob(self, class: BindClass) -> f32 {
        match class {
            BindClass::Low => self.dev_bind.low,
            BindClass::Pulse => self.dev_bind.pulse,
            BindClass::Harmony => self.dev_bind.harmony,
            BindClass::Texture => self.dev_bind.texture,
            BindClass::Spine | BindClass::Backbeat => 1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The app's genre strip — one spec each, all distinct.
    const APP_GENRES: [&str; 8] = [
        "minimal techno",
        "techno",
        "deep house",
        "house",
        "microhouse",
        "acid",
        "dub techno",
        "ambient",
    ];

    #[test]
    fn genre_resolution() {
        assert_eq!(spec_for(Some("microhouse")).style, Style::Microhouse);
        assert_eq!(spec_for(Some("Minimal Techno")).style, Style::Minimal);
        assert_eq!(spec_for(Some("techno")).style, Style::Techno);
        assert_eq!(spec_for(Some("deep house")).style, Style::DeepHouse);
        assert_eq!(spec_for(Some("acid")).style, Style::Acid);
        assert_eq!(spec_for(Some("dub techno")).style, Style::DubTechno);
        assert_eq!(spec_for(Some("ambient")).style, Style::Ambient);
        assert_eq!(spec_for(None).style, Style::Default);
        assert_eq!(spec_for(Some("??")).style, Style::Default);
    }

    #[test]
    fn every_app_genre_resolves_to_its_own_spec() {
        let specs: Vec<GenreSpec> = APP_GENRES.map(|g| spec_for(Some(g))).to_vec();
        for (i, a) in specs.iter().enumerate() {
            for b in specs.iter().skip(i + 1) {
                assert_ne!(a.style, b.style, "two strip genres share a spec");
            }
        }
    }

    /// The failure this guards: named genres rendering byte-identical audio
    /// because the spec carried nothing a listener can hear. Every pair of
    /// built-in specs must differ in at least one audible dimension.
    #[test]
    fn every_style_is_musically_distinct_from_every_other() {
        let specs = [MICROHOUSE, MINIMAL, TECHNO, HOUSE, DEEP_HOUSE, ACID, DUB_TECHNO, AMBIENT, DEFAULT];
        for (i, a) in specs.iter().enumerate() {
            for b in specs.iter().skip(i + 1) {
                let fingerprint = |s: &GenreSpec| {
                    (
                        s.bpm.to_bits(),
                        s.swing,
                        s.grooves,
                        s.rack.iter().map(|e| e.id).collect::<Vec<_>>(),
                        s.keys,
                        s.hats == HatIdiom::Sixteenths,
                        s.energy_bias.to_bits(),
                    )
                };
                assert_ne!(
                    fingerprint(a),
                    fingerprint(b),
                    "{:?} and {:?} share tempo, swing, groove pool, rack, keys and energy bias",
                    a.style,
                    b.style
                );
            }
        }
    }

    #[test]
    fn house_and_deep_house_do_not_collapse_into_each_other() {
        let house = spec_for(Some("house"));
        let deep = spec_for(Some("deep house"));
        assert_ne!(house.bpm, deep.bpm);
        assert_ne!(house.swing, deep.swing);
    }

    #[test]
    fn straight_styles_do_not_swing() {
        assert_eq!(TECHNO.swing, (0.0, 0.0), "techno is straight time");
        assert_eq!(AMBIENT.swing, (0.0, 0.0), "beatless music has no swing");
        assert!(HOUSE.swing.0 > 0.05, "house shuffles");
        assert!(DEEP_HOUSE.swing.0 > 0.05, "deep house shuffles");
    }

    #[test]
    fn racks_follow_the_issue_88_tables() {
        let ids = |spec: &GenreSpec| spec.rack.iter().map(|e| e.id).collect::<Vec<_>>();
        // minimal techno: kick · hat · sub bass · sparse stab
        assert_eq!(ids(&MINIMAL), vec!["kick", "perc", "bass", "stab"]);
        // ambient: no kick, no drums at all — pad · ep · pluck · texture.
        assert_eq!(ids(&AMBIENT), vec!["pad", "ep", "pluck", "texture"]);
        assert!(!AMBIENT.rack.iter().any(|e| e.voice == Kick), "ambient is beatless");
        // acid: the 303 front and center.
        assert_eq!(ids(&ACID), vec!["kick", "perc", "acid"]);
        assert_eq!(ACID.rack[2].voice, Voice::Acid);
        // dub techno: the stab ships its long delay send.
        let dub_stab = DUB_RACK.iter().find(|e| e.id == "stab").unwrap();
        assert!(dub_stab.sends.delay >= 0.5, "dub techno's delay send is identity");
        // The default rig keeps its six tracks and ids.
        assert_eq!(ids(&DEFAULT).len(), FULL_RIG);
    }

    #[test]
    fn every_rack_is_well_formed() {
        for spec in [MICROHOUSE, MINIMAL, TECHNO, DEEP_HOUSE, HOUSE, ACID, DUB_TECHNO, AMBIENT, DEFAULT] {
            assert!(!spec.rack.is_empty());
            let mut ids: Vec<&str> = spec.rack.iter().map(|e| e.id).collect();
            ids.sort_unstable();
            ids.dedup();
            assert_eq!(ids.len(), spec.rack.len(), "{:?}: duplicate rack ids", spec.style);
            assert!(
                spec.rack.iter().any(|e| matches!(e.class, BindClass::Harmony | BindClass::Pulse)),
                "{:?}: some section must have something to bind",
                spec.style
            );
            for e in spec.rack {
                assert!((0.0..=1.0).contains(&e.chance), "{:?}/{}: chance", spec.style, e.id);
                assert!((0.0..=2.0).contains(&e.gain), "{:?}/{}: gain", spec.style, e.id);
                assert!((-1.0..=1.0).contains(&e.pan), "{:?}/{}: pan", spec.style, e.id);
            }
            // The default tempo sits inside the style's own band.
            assert!(
                spec.bpm >= spec.bpm_range.0 && spec.bpm <= spec.bpm_range.1,
                "{:?}: default bpm outside its range",
                spec.style
            );
        }
    }

    #[test]
    fn acid_and_ambient_never_collapse_again() {
        // Issue #87's smoking gun: same seed, byte-identical sessions.
        let a = crate::arrangement::generate_session(&crate::arrangement::GenParams {
            genre: Some("acid".into()),
            seed: 42,
            ..crate::arrangement::GenParams::default()
        });
        let b = crate::arrangement::generate_session(&crate::arrangement::GenParams {
            genre: Some("ambient".into()),
            seed: 42,
            ..crate::arrangement::GenParams::default()
        });
        assert_ne!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap(),
            "acid and ambient must never render the same session"
        );
    }

    #[test]
    fn generated_sessions_carry_the_duck_parameters() {
        // Issue #76: the rig ships per-role duck depths and the genre
        // template's pump release, so the engine can duck by role with a
        // per-style release instead of one hard-wired constant.
        use crate::arrangement::{generate_session, GenParams};
        for genre in [Some("house"), Some("deep-house"), Some("techno"), None] {
            let session = generate_session(&GenParams {
                genre: genre.map(str::to_string),
                ..GenParams::default()
            });
            let expected_release = crate::genre::spec_for(genre.as_deref()).duck_release_ms;
            assert_eq!(
                session.duck_release_ms, expected_release,
                "genre {genre:?} must carry its template release"
            );
            for track in &session.tracks {
                match track.role {
                    kontinuum_ir::TrackRole::Kick => {
                        assert_eq!(
                            track.duck_depth, None,
                            "the key source stays on the engine's 0.0 role default"
                        );
                    }
                    role => {
                        let depth = track.duck_depth.expect("rig tracks ship an explicit depth");
                        assert!((0.0..=1.0).contains(&depth), "depth outside the full range");
                        if matches!(
                            role,
                            kontinuum_ir::TrackRole::Bass
                                | kontinuum_ir::TrackRole::Pad
                                | kontinuum_ir::TrackRole::Perc
                        ) {
                            assert_eq!(depth, 1.0, "the bed ducks to unity at full key");
                        }
                    }
                }
            }
        }
    }
}
