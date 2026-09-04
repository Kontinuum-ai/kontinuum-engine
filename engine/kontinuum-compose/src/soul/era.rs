//! Era switching mid-session (issue #55: "artists change style — switchable
//! at a section boundary like any diff, without restart").
//!
//! Mechanism, on the world-morph machinery: the new era's blended layer view
//! re-voices the rig at the boundary section's start bar, and each changed
//! mix target gets a two-point `Smooth` crossfade lane (same
//! one-lane-per-(section,track) precedence as [`crate::world::morph`];
//! unchanged values write no lane at all). Past audio is untouched by
//! construction. `Session::souls` records the switch, so the session
//! document stays the single source of truth.

use std::collections::BTreeMap;

use kontinuum_ir::schema::{Session, SoulRef};

use super::blend::{blend, BlendInput};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoulSwitchError {
    /// `at_section` is past the end of the session.
    Section { index: usize, sections: usize },
    /// The stack does not name this pack (or the registry does not have it).
    UnknownSoul(String),
    /// The pack does not ship this era.
    UnknownEra { soul: String, era: String },
}

impl std::fmt::Display for SoulSwitchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SoulSwitchError::Section { index, sections } => {
                write!(f, "era switch section {index} out of range ({sections} sections)")
            }
            SoulSwitchError::UnknownSoul(id) => write!(f, "session stack has no soul `{id}`"),
            SoulSwitchError::UnknownEra { soul, era } => {
                write!(f, "soul `{soul}` has no era named `{era}`")
            }
        }
    }
}

impl std::error::Error for SoulSwitchError {}

/// Switches soul `id` to `era`, landing at the start of
/// `sections[at_section]`. `packs` is the host's loaded registry; every
/// stack entry must resolve in it. The whole stack re-blends with the new
/// era and applies with world-morph boundary mechanics.
pub fn set_era(
    session: &mut Session,
    packs: &BTreeMap<String, super::CreativeSoul>,
    id: &str,
    era: &str,
    at_section: usize,
) -> Result<(), SoulSwitchError> {
    if at_section >= session.sections.len() {
        return Err(SoulSwitchError::Section { index: at_section, sections: session.sections.len() });
    }
    let Some(pack) = packs.get(id) else { return Err(SoulSwitchError::UnknownSoul(id.to_string())) };
    if !pack.eras.contains_key(era) {
        return Err(SoulSwitchError::UnknownEra { soul: id.to_string(), era: era.to_string() });
    }
    let Some(refs) = session.souls.as_ref() else {
        return Err(SoulSwitchError::UnknownSoul(id.to_string()));
    };
    if !refs.iter().any(|r| r.id == id) {
        return Err(SoulSwitchError::UnknownSoul(id.to_string()));
    }

    let switched: Vec<SoulRef> = refs
        .iter()
        .map(|r| SoulRef {
            id: r.id.clone(),
            weight: r.weight,
            era: if r.id == id { Some(era.to_string()) } else { r.era.clone() },
        })
        .collect();
    let inputs: Vec<BlendInput<'_>> = switched
        .iter()
        .map(|r| BlendInput {
            soul: &packs[&r.id],
            weight: r.weight,
            era: r.era.as_deref().or(Some(super::load::DEFAULT_ERA)),
        })
        .collect();
    let blended = blend(&inputs).map_err(|e| match e {
        super::BlendError::UnknownEra { soul, era } => SoulSwitchError::UnknownEra { soul, era },
        _ => SoulSwitchError::UnknownSoul(id.to_string()),
    })?;

    morph_soul(session, &blended, at_section);
    session.souls = Some(switched);
    Ok(())
}

/// Applies `to` with world-morph boundary mechanics: palette re-voicing
/// lands at the section start bar; changed mix targets crossfade across the
/// boundary section (unchanged values write no lane).
pub(crate) fn morph_soul(session: &mut Session, to: &super::BlendedSoul, at_section: usize) {
    let from_values: Vec<(String, crate::world::MixValues)> = session
        .tracks
        .iter()
        .filter(|t| to.mix_profile.contains_key(&t.id))
        .map(|t| (t.id.clone(), crate::world::MixValues::of(t)))
        .collect();
    super::apply_to_tracks(&mut session.tracks, to);
    for (id, from_v) in from_values {
        let Some(target) = to.mix_profile.get(&id) else { continue };
        let Some(sec) = session.sections.get_mut(at_section) else { continue };
        let from_v = crate::world::morph::with_lane_endpoint(sec, &id, from_v);
        let to_override = crate::world::MixTargetOverride {
            gain: Some(target.gain),
            pan: Some(target.pan),
            send_delay: Some(target.send_delay),
            send_reverb: Some(target.send_reverb),
        };
        crate::world::morph::write_crossfade(sec, &id, from_v, to_override);
    }
}
