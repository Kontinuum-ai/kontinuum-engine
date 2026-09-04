//! Stem mute-sets (#102): a muted track must leave no trace on the render.

use std::path::Path;

use kontinuum_offline::{parse_session, render_session_with, RenderOptions};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/loop-4track.ir.json");

/// The fixture automates `pad.send_reverb` — which is the whole point: it is
/// the case that used to leak.
#[test]
fn a_muted_track_cannot_reach_a_stem_through_its_send_automation() {
    let session = parse_session(Path::new(FIXTURE)).expect("fixture");
    let pad = session
        .tracks
        .iter()
        .position(|t| t.id == "pad")
        .expect("fixture has a pad track");
    assert!(
        session.sections.iter().any(|s| s.automation.contains_key("pad")),
        "this test is meaningless unless the fixture automates the pad"
    );

    // Same stem, rendered from a session where the pad's automation lanes do
    // not exist at all. A muted pad must be indistinguishable from an absent
    // one: zeroing its sends at graph build time is not enough on its own,
    // because the automation puts them straight back mid-render and sends
    // tap pre-mute.
    let mut stripped = session.clone();
    for section in &mut stripped.sections {
        section.automation.remove("pad");
    }

    let keep = if pad == 0 { 1 } else { 0 };
    let options = RenderOptions::stem(session.tracks.len(), keep);
    let with_pad_automation = render_session_with(&session, 48_000, &options).expect("render");
    let without = render_session_with(&stripped, 48_000, &options).expect("render");

    assert_eq!(
        with_pad_automation.left, without.left,
        "a muted pad's send automation changed the left channel of another track's stem"
    );
    assert_eq!(with_pad_automation.right, without.right, "…and the right channel");
}

/// The full mix must be untouched by the suppression: it only applies to
/// tracks the caller muted, and the mix mutes nothing.
#[test]
fn the_full_mix_still_hears_send_automation() {
    let session = parse_session(Path::new(FIXTURE)).expect("fixture");
    let mut stripped = session.clone();
    for section in &mut stripped.sections {
        section.automation.remove("pad");
    }
    let mix = render_session_with(&session, 48_000, &RenderOptions::mix()).expect("render");
    let without = render_session_with(&stripped, 48_000, &RenderOptions::mix()).expect("render");
    assert_ne!(
        mix.left, without.left,
        "removing the pad's automation changed nothing, so the lane is not reaching the mix"
    );
}

/// Muting is not silencing: a muted kick still keys the #76 sidechain duck,
/// which is what keeps a stem coherent with the mix it came from.
#[test]
fn a_muted_kick_still_ducks_the_other_stems() {
    let session = parse_session(Path::new(FIXTURE)).expect("fixture");
    let kick = session.tracks.iter().position(|t| t.id == "kick").expect("kick");
    let bass = session.tracks.iter().position(|t| t.id == "bass").expect("bass");

    let bass_stem = render_session_with(
        &session,
        48_000,
        &RenderOptions::stem(session.tracks.len(), bass),
    )
    .expect("render");

    // The same bass stem from a session with no kick track at all has no
    // duck key, so it must differ.
    let mut no_kick = session.clone();
    no_kick.tracks.remove(kick);
    for section in &mut no_kick.sections {
        section.pattern_bindings.remove("kick");
        section.automation.remove("kick");
    }
    let new_bass = no_kick.tracks.iter().position(|t| t.id == "bass").expect("bass");
    let undurked = render_session_with(
        &no_kick,
        48_000,
        &RenderOptions::stem(no_kick.tracks.len(), new_bass),
    )
    .expect("render");

    assert_ne!(
        bass_stem.left, undurked.left,
        "the bass stem is identical with and without a kick, so the duck is not keying"
    );
}
