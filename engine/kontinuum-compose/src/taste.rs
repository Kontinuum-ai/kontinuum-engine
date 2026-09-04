//! Taste import (issue #21): streaming metadata + on-device audio analysis
//! → [`TasteProfile`] → a generated session.
//!
//! The profile **is** the canonical musical DNA (v2): one versioned struct
//! shared by the importer, the composer context (#22 consumes
//! [`TasteProfile::summary`]) and the learner ladder (#24 expands its
//! points into `TastePriors` bands). v1 fields keep their names and JSON
//! shape; v2 adds audio-derived and entity-graph fields behind serde
//! defaults, so a v1 document deserializes as the v2 default profile.
//!
//! Reality of the platforms (PLAN §4): Spotify removed audio-features and
//! audio-analysis for new apps (Nov 2024) and exposes no full-track audio;
//! SoundCloud is metadata-first too. So metadata-only sources fill the
//! genre/era/scene fields; audio-derived fields (swing, brightness,
//! dispersion) fill in from user files and pinned references through the
//! importer crate. Every field lands in `docs/dna-mapping.md`.

use std::collections::BTreeMap;

use crate::arrangement::GenParams;
use kontinuum_ir::schema::Session;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// DNA schema version. Bump on any field change that breaks the meaning of
/// a stored profile; readers gate on it like the corpus artifacts do.
pub const DNA_VERSION: u32 = 2;

/// A measured dimension: weighted mean plus dispersion (population std
/// dev). Dispersion is the point of the pair — wide taste means wide
/// generation bounds — so a mean alone never carries a learned field.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Stat {
    pub mean: f32,
    pub dispersion: f32,
}

impl Stat {
    pub fn new(mean: f32, dispersion: f32) -> Self {
        Stat { mean, dispersion }
    }
}

/// The distilled musical identity of a listener (or a playlist).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TasteProfile {
    /// Schema version of this document ([`DNA_VERSION`]). Documents that
    /// predate versioning (no field) deserialize at the reader's current
    /// version with v2 defaults — the compatible-evolution contract with
    /// #22/#24.
    pub dna_version: u32,
    /// Target tempo in BPM, or `None` to take the genre's own tempo.
    ///
    /// Optional on purpose: callers that name a genre and nothing else (the
    /// iOS app sends `{"genres": ["techno"]}`) must not silently pin every
    /// style to one tempo, which a plain `f64` with a struct default does.
    pub bpm: Option<f64>,
    /// 0..1 — overall energy of the arrangement.
    pub energy: f32,
    /// 0..1 — minor/major and dark-vs-bright key material.
    pub darkness: f32,
    /// 0..1 — event density per bar.
    pub density: f32,
    /// 0..1 — how much pattern variation between sections.
    pub variation: f32,
    /// Free-form provenance: which genres drove the profile.
    pub genres: Vec<String>,
    /// BPM spread across the evidence (population std dev), when tempo
    /// metadata or audio analysis exists. Wide spread = the listener's
    /// library does not live at one speed.
    pub tempo_dispersion: Option<f64>,
    /// Normalized genre-mix distribution (weights sum to ≈1, sorted by
    /// weight descending). The entity-graph output behind `genres`.
    pub genre_mix: Vec<(String, f32)>,
    /// Groove/shuffle stats from analyzed audio, 0..1.
    pub swing: Option<Stat>,
    /// Spectral tilt (high-band share) from analyzed audio, 0..1.
    pub brightness: Option<Stat>,
    /// Catalog diversity, 0..1: how evenly the listener spreads across
    /// genres/artists (normalized Shannon entropy of the genre mix).
    pub adventurousness: Option<f32>,
    /// Era weights (decade → weight, e.g. "1990s"), from release years
    /// and enrichment.
    pub era_weights: Vec<(String, f32)>,
    /// Scene/label weights (label or scene name → weight), from
    /// enrichment and playlist curation.
    pub scene_weights: Vec<(String, f32)>,
    /// Preferred section length in bars, from analyzed audio.
    pub section_bars: Option<Stat>,
}

impl Default for TasteProfile {
    fn default() -> Self {
        TasteProfile {
            dna_version: DNA_VERSION,
            bpm: None,
            energy: 0.7,
            darkness: 0.7,
            density: 0.6,
            variation: 0.5,
            genres: vec![],
            tempo_dispersion: None,
            genre_mix: vec![],
            swing: None,
            brightness: None,
            adventurousness: None,
            era_weights: vec![],
            scene_weights: vec![],
            section_bars: None,
        }
    }
}

impl TasteProfile {
    /// Compact one-line summary for the composer context (#22's
    /// `ContextInputs.taste_summary`, clamped there to 320 chars).
    /// Deterministic: same profile, same bytes.
    pub fn summary(&self) -> String {
        let mut out = String::with_capacity(160);
        if let Some(bpm) = self.bpm {
            let spread = self.tempo_dispersion.unwrap_or(0.0);
            out.push_str(&format!("bpm {bpm:.0}±{spread:.0} "));
        }
        out.push_str(&format!(
            "energy {:.2} density {:.2} dark {:.2} var {:.2}",
            self.energy, self.density, self.darkness, self.variation
        ));
        if let Some(s) = self.swing {
            out.push_str(&format!(" swing {:.2}±{:.2}", s.mean, s.dispersion));
        }
        if let Some(s) = self.brightness {
            out.push_str(&format!(" bright {:.2}", s.mean));
        }
        if let Some(a) = self.adventurousness {
            out.push_str(&format!(" adv {a:.2}"));
        }
        if !self.genres.is_empty() {
            let top: Vec<&str> = self.genres.iter().take(3).map(|s| s.as_str()).collect();
            out.push_str(&format!(" [{}]", top.join(", ")));
        }
        out
    }

    /// Weighted genre map, descending. Empty when nothing was learned.
    pub fn genre_mix_map(&self) -> BTreeMap<String, f32> {
        self.genre_mix.iter().cloned().collect()
    }

    /// Public wrapper over the genre-nudge table: the importer crate builds
    /// profiles from typed entity graphs and needs the same seasoning the
    /// raw-JSON path gets.
    pub fn apply_genre_nudge(&mut self, genre: &str) {
        apply_genre(genre, self);
    }
}

/// Genre keyword → parameter nudges. Additive; defaults win when nothing
/// matches. Deliberately coarse and auditable (issue #33: trust surfaces show
/// exactly this mapping to the user).
///
/// Tempo has one home: `genre::spec_for` — every style it names (all eight
/// app genres since #87) has its bpm reset above. What remains here is the
/// seasoning for genres the spec does not carry, plus per-genre density/
/// darkness nudges that flow into `GenParams` and from there into binding
/// odds, fills and progression bias.
fn apply_genre(genre: &str, p: &mut TasteProfile) {
    let g = genre.to_lowercase();
    let has = |k: &str| g.contains(k);
    if crate::genre::names_a_style(&g) {
        p.bpm = None;
    }
    if has("minimal") {
        p.density = (p.density - 0.15).max(0.2);
        p.variation = (p.variation - 0.1).max(0.2);
        p.darkness = (p.darkness + 0.1).min(1.0);
    }
    if has("techno") {
        p.darkness = (p.darkness + 0.15).min(1.0);
        p.energy = (p.energy + 0.05).min(1.0);
    }
    if has("deep") && has("house") {
        p.darkness = (p.darkness + 0.05).min(1.0);
        p.density = (p.density + 0.05).min(1.0);
    }
    if has("house") && !has("deep") {
        p.energy = (p.energy + 0.1).min(1.0);
    }
    if has("microhouse") || has("micro") {
        p.density = (p.density + 0.1).min(1.0);
    }
    if has("ambient") {
        p.energy = (p.energy - 0.3).max(0.1);
        p.density = (p.density - 0.25).max(0.1);
        p.darkness = (p.darkness + 0.05).min(1.0);
    }
    if has("acid") {
        p.density = (p.density + 0.15).min(1.0);
        p.variation = (p.variation + 0.15).min(1.0);
    }
    if has("dub") && !crate::genre::names_a_style(&g) {
        p.variation = (p.variation + 0.1).min(1.0);
    }
}

/// Build a profile from Spotify-style metadata JSON: expects `{ items: [{ track:
/// { artists: [{name}], duration_ms, popularity } }] }` (saved tracks /
/// playlist items) and optionally top-level `{ genres: [...] }` (artist obj).
pub fn from_spotify_metadata(v: &Value) -> TasteProfile {
    let mut p = TasteProfile::default();
    if let Some(genres) = v.get("genres").and_then(|g| g.as_array()) {
        for g in genres {
            if let Some(s) = g.as_str() {
                p.genres.push(s.to_string());
                apply_genre(s, &mut p);
            }
        }
    }
    let mut durations = vec![];
    collect_spotify_tracks(v, &mut durations, &mut p);
    if !durations.is_empty() {
        let avg = durations.iter().sum::<f64>() / durations.len() as f64;
        // Long DJ-style tracks → steadier energy; short radio edits → denser.
        if avg > 330.0 {
            p.variation = (p.variation - 0.1).max(0.2);
        } else if avg < 210.0 {
            p.density = (p.density + 0.1).min(1.0);
        }
    }
    p.genres.sort();
    p.genres.dedup();
    p
}

fn collect_spotify_tracks(v: &Value, durations: &mut Vec<f64>, p: &mut TasteProfile) {
    if let Some(items) = v.get("items").and_then(|i| i.as_array()) {
        for item in items {
            if let Some(track) = item.get("track") {
                if let Some(ms) = track.get("duration_ms").and_then(|d| d.as_f64()) {
                    durations.push(ms / 1000.0);
                }
                for artist in track
                    .get("artists")
                    .and_then(|a| a.as_array())
                    .into_iter()
                    .flatten()
                {
                    if let Some(name) = artist.get("name").and_then(|n| n.as_str()) {
                        apply_genre(name, p);
                    }
                }
            }
        }
    }
}

/// Build a profile from SoundCloud-style metadata JSON: `{ collection: [{
/// track: { genre, duration, bpm } }] }` or a track object with `genre`/`bpm`.
pub fn from_soundcloud_metadata(v: &Value) -> TasteProfile {
    let mut p = TasteProfile::default();
    let mut bpms = vec![];
    if let Some(genre) = v.get("genre").and_then(|g| g.as_str()) {
        p.genres.push(genre.to_string());
        apply_genre(genre, &mut p);
    }
    if let Some(bpm) = v.get("bpm").and_then(|b| b.as_f64()) {
        if (60.0..200.0).contains(&bpm) {
            bpms.push(bpm);
        }
    }
    if let Some(collection) = v.get("collection").and_then(|c| c.as_array()) {
        for item in collection {
            if let Some(track) = item.get("track") {
                if let Some(genre) = track.get("genre").and_then(|g| g.as_str()) {
                    p.genres.push(genre.to_string());
                    apply_genre(genre, &mut p);
                }
                if let Some(bpm) = track.get("bpm").and_then(|b| b.as_f64()) {
                    if (60.0..200.0).contains(&bpm) {
                        bpms.push(bpm);
                    }
                }
            }
        }
    }
    if !bpms.is_empty() {
        p.bpm = Some(bpms.iter().sum::<f64>() / bpms.len() as f64);
    }
    p.genres.sort();
    p.genres.dedup();
    p
}

/// Profile → GenParams, shared by both session entry points.
///
/// Every field of the profile is carried across. `density`, `variation` and
/// `darkness` used to be computed by [`apply_genre`] and then dropped here,
/// which made most of that function dead code — the minimal, ambient, acid and
/// dub branches adjusted nothing that reached the generator.
///
/// v2 wiring: a measured swing stat pins the nearest groove template
/// ([`crate::groove::nearest_swing`]) — the one audio-derived knob that
/// lands inside the session itself.
pub fn session_from_taste(profile: &TasteProfile, seed: u64) -> Session {
    let params = gen_params_for_taste(profile, seed);
    crate::arrangement::generate_session(&params)
}

/// The SessionDirector selection hook (issue #30): taste-weighted world
/// choice feeds generation, so the same taste profile that drives tempo/
/// energy also picks the sound world ([`crate::world::select_world`] —
/// deterministic, slice-order independent). No candidate worlds → the
/// session is exactly [`session_from_taste`]'s.
pub fn session_from_taste_with_worlds(
    profile: &TasteProfile,
    seed: u64,
    worlds: &[crate::world::SoundWorld],
) -> Session {
    let mut params = gen_params_for_taste(profile, seed);
    if let Some(world) = crate::world::select_world(worlds, profile) {
        params.world = Some(world.clone());
    }
    crate::arrangement::generate_session(&params)
}

/// Profile → GenParams, public: the importer (#21) maps DNA → knobs
/// without generating a full session. The one profile→knob mapping both
/// session entry points share.
pub fn gen_params_for_taste(profile: &TasteProfile, seed: u64) -> GenParams {
    let mut profile = profile.clone();
    // The genre's own nudges have to land on this path too: it is what the app
    // calls, and `apply_genre` was only ever reached from Spotify metadata
    // import, so a session generated from `{"genres": ["techno"]}` picked up
    // none of them.
    let stated_bpm = profile.bpm;
    if let Some(genre) = profile.genres.first().cloned() {
        apply_genre(&genre, &mut profile);
    }
    // A tempo the caller actually stated outranks the genre's.
    profile.bpm = stated_bpm.or(profile.bpm);
    // A measured swing stat picks the closest template; nothing measured,
    // the genre's own pool draws (the v1 behavior, unchanged).
    let groove = profile.swing.map(|s| crate::groove::nearest_swing(s.mean).name.to_string());
    GenParams {
        arc: None,
        seed,
        target_bars: 128,
        bpm: profile.bpm,
        intensity: profile.energy.clamp(0.0, 1.0),
        genre: profile.genres.first().cloned(),
        groove,
        density: profile.density.clamp(0.0, 1.0),
        variation: profile.variation.clamp(0.0, 1.0),
        darkness: profile.darkness.clamp(0.0, 1.0),
        bass_archetype: None,
        groove_bank: None,
        structure: None,
        world: None,
        souls: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn techno_genres_push_the_right_direction() {
        let mut p = TasteProfile::default();
        apply_genre("minimal techno", &mut p);
        // A named style defers to `genre::spec_for` for tempo rather than
        // carrying a second table here.
        assert_eq!(p.bpm, None, "a named style owns its own tempo");
        assert!(p.darkness > 0.7);
        // Ambient is a named style now (#87): its spec owns the tempo, and
        // the nudge only moves energy/density/darkness.
        let mut ambient = TasteProfile::default();
        apply_genre("ambient", &mut ambient);
        assert_eq!(ambient.bpm, None);
        assert!(ambient.energy < 0.5);
        // Styles the spec does not name keep their own nudges.
        let mut other = TasteProfile::default();
        apply_genre("jazz fusion", &mut other);
        assert_eq!(other.bpm, None, "nothing here pins a tempo either");
    }

    /// The taste boundary (#87): every profile dimension that reaches
    /// `GenParams` must change the generated session — most of `apply_genre`
    /// used to be dead code because `session_from_taste` dropped three of
    /// its outputs.
    #[test]
    fn density_variation_and_darkness_change_generation() {
        let session = |mut p: TasteProfile| {
            p.genres = vec!["techno".into()];
            session_from_taste(&p, 11)
        };
        let json = |s: &kontinuum_ir::schema::Session| serde_json::to_string(s).unwrap();
        let base = TasteProfile::default();
        let denser = TasteProfile { density: 1.0, ..TasteProfile::default() };
        let sparser = TasteProfile { density: 0.2, ..TasteProfile::default() };
        assert_ne!(json(&session(base.clone())), json(&session(denser)), "density is dropped");
        assert_ne!(json(&session(base.clone())), json(&session(sparser)), "density is dropped");
        let varied = TasteProfile { variation: 1.0, ..TasteProfile::default() };
        assert_ne!(json(&session(base.clone())), json(&session(varied)), "variation is dropped");
        // Darkness is a weighted bias, not a deterministic switch: one seed
        // can draw the same template at either setting, so sweep seeds and
        // require the bias to show up somewhere.
        let differs = (0..24u64).any(|seed| {
            let base = TasteProfile { darkness: 0.0, ..TasteProfile::default() };
            let dark = TasteProfile { darkness: 1.0, ..TasteProfile::default() };
            let of = |p: &TasteProfile| {
                let mut p = p.clone();
                p.genres = vec!["techno".into()];
                session_from_taste(&p, seed)
            };
            json(&of(&base)) != json(&of(&dark))
        });
        assert!(differs, "darkness is dropped");
    }

    #[test]
    fn spotify_items_map_to_profile() {
        let v: Value = serde_json::from_str(
            r#"{"genres": ["minimal techno", "microhouse"],
                "items": [
                  {"track": {"artists": [{"name": "some 909 act"}], "duration_ms": 420000, "popularity": 40}},
                  {"track": {"artists": [{"name": "deep artist"}], "duration_ms": 300000, "popularity": 50}}
                ]}"#,
        )
        .unwrap();
        let p = from_spotify_metadata(&v);
        assert!(p.genres.contains(&"minimal techno".to_string()));
        assert_eq!(p.bpm, None, "named styles defer tempo to the genre spec");
        assert!(p.density >= 0.55);
    }

    #[test]
    fn soundcloud_bpms_average() {
        let v: Value = serde_json::from_str(
            r#"{"collection": [
                 {"track": {"genre": "Techno", "bpm": 128}},
                 {"track": {"genre": "Deep House", "bpm": 122}}
               ]}"#,
        )
        .unwrap();
        let p = from_soundcloud_metadata(&v);
        assert!(p.bpm.is_some_and(|b| (b - 125.0).abs() < 1e-9), "stated BPMs still average");
        assert!(p.genres.contains(&"Techno".to_string()));
    }

    #[test]
    fn taste_generates_a_valid_session() {
        let p = TasteProfile { bpm: Some(126.0), energy: 0.8, ..Default::default() };
        let session = session_from_taste(&p, 7);
        assert!(kontinuum_ir::validate_session(&session).is_ok());
    }

    #[test]
    fn v1_documents_deserialize_as_v2() {
        // The bridge and stored profiles carry v1 JSON: no version field,
        // none of the v2 additions. Back-compat is the unification contract
        // with #22/#24 — nothing may break.
        let p: TasteProfile = serde_json::from_str(
            r#"{"bpm": 126.0, "energy": 0.8, "darkness": 0.7, "density": 0.6,
                "variation": 0.5, "genres": ["techno"]}"#,
        )
        .expect("v1 json");
        assert_eq!(p.dna_version, DNA_VERSION, "v1 documents upgrade to the reader's schema");
        assert_eq!(p.swing, None);
        assert_eq!(p.genre_mix, Vec::new());
        assert!(p.adventurousness.is_none());
        // And a fresh profile round-trips at the current version.
        let v2: TasteProfile = serde_json::from_str(&serde_json::to_string(&TasteProfile::default()).unwrap()).unwrap();
        assert_eq!(v2.dna_version, DNA_VERSION);
    }

    #[test]
    fn swing_stat_pins_the_nearest_groove() {
        let straight = TasteProfile { swing: Some(Stat::new(0.0, 0.01)), ..Default::default() };
        let shuffled = TasteProfile { swing: Some(Stat::new(0.17, 0.02)), ..Default::default() };
        let unmeasured = TasteProfile::default();
        let groove_of = |p: &TasteProfile| gen_params_for_taste(p, 5).groove;
        assert_eq!(groove_of(&straight).as_deref(), Some("straight-machine"));
        assert_eq!(groove_of(&shuffled).as_deref(), Some("drunk-shuffle"));
        assert_eq!(groove_of(&unmeasured), None, "no measurement, genre pool draws");
        // The knob lands in the session: different swings, different output.
        let s1 = session_from_taste(&straight, 9);
        let s2 = session_from_taste(&shuffled, 9);
        assert_ne!(
            serde_json::to_string(&s1).unwrap(),
            serde_json::to_string(&s2).unwrap()
        );
    }

    #[test]
    fn summary_is_deterministic_and_compact() {
        let mut p = TasteProfile { bpm: Some(126.5), tempo_dispersion: Some(3.2), ..Default::default() };
        p.genres = vec!["minimal techno".into(), "microhouse".into()];
        p.swing = Some(Stat::new(0.12, 0.03));
        p.adventurousness = Some(0.62);
        let a = p.summary();
        let b = p.summary();
        assert_eq!(a, b);
        assert!(a.contains("bpm 126±3"), "got: {a}");
        assert!(a.contains("swing 0.12±0.03"));
        assert!(a.contains("adv 0.62"));
        assert!(a.contains("minimal techno"));
        // #22 clamps to 320 chars; stay far under it.
        assert!(a.len() < 320);
    }

    #[test]
    fn a_genre_only_profile_runs_at_the_genre_tempo() {
        // Exactly what the iOS app sends: a style name and nothing else. With
        // a non-optional `bpm` the struct default (124) reached the generator
        // and every style came out at one tempo — the app's main path could
        // not hear the per-genre tempos at all.
        let tempo_of = |genre: &str| {
            let p: TasteProfile =
                serde_json::from_str(&format!(r#"{{"genres": ["{genre}"]}}"#)).expect("profile");
            assert_eq!(p.bpm, None, "{genre}: no tempo stated");
            session_from_taste(&p, 7).tempo_lane[0].1
        };
        let techno = tempo_of("techno");
        let house = tempo_of("house");
        let deep = tempo_of("deep house");
        assert_ne!(techno, house, "techno and house must not share a tempo");
        assert_ne!(house, deep, "house and deep house must not share a tempo");
        assert!(techno > house, "techno runs faster than house: {techno} vs {house}");
        // A stated tempo still wins.
        let stated = TasteProfile { bpm: Some(118.0), genres: vec!["techno".into()], ..Default::default() };
        assert_eq!(session_from_taste(&stated, 7).tempo_lane[0].1, 118.0);
    }
}

/// Live-production variation: mutate a session for one lap of playback.
/// Same (seed, lap) is always identical (deterministic), different laps
/// always drift — velocity sway, microtiming, pattern rotation, and
/// structural drop-outs so no lap is ever the same (the gym brief).
pub fn vary_session(base: &Session, seed: u64, lap: u32) -> Session {
    use kontinuum_clock::stream;
    use kontinuum_ir::schema::Pattern;
    let mut s = base.clone();
    let mut rng = stream(seed ^ (lap as u64).wrapping_mul(0x9E37_79B9), 0xF0, 0xA1);
    let sway = |rng: &mut kontinuum_clock::Rng| 0.88 + 0.24 * rng.next_f32();
    for sec in s.sections.iter_mut() {
        for pat in sec.pattern_bindings.values_mut() {
            match pat {
                Pattern::Steps(st) => {
                    for step in st.steps.iter_mut() {
                        step.velocity = (step.velocity * sway(&mut rng)).clamp(0.05, 1.0);
                        step.microtiming_ticks = rng.below(13) as i16 - 6;
                    }
                }
                Pattern::Euclidean(e) => {
                    e.velocity = (e.velocity * sway(&mut rng)).clamp(0.05, 1.0);
                    if rng.chance(0.4) {
                        e.rot += rng.below(3) as i32 - 1;
                    }
                }
                Pattern::ProbabilityMask(m) => {
                    m.velocity = (m.velocity * sway(&mut rng)).clamp(0.05, 1.0);
                }
            }
        }
    }
    // Lap-flavored structure: every 2nd lap lifts the final section; every
    // 3rd lap opens the intro by dropping the harmonic tracks.
    if lap % 3 == 2 {
        if let Some(first) = s.sections.first_mut() {
            first.pattern_bindings.remove("pad");
            first.pattern_bindings.remove("stab");
        }
    }
    if lap % 2 == 1 {
        if let Some(last) = s.sections.last_mut() {
            for pat in last.pattern_bindings.values_mut() {
                match pat {
                    Pattern::Steps(st) => {
                        for step in st.steps.iter_mut() {
                            step.velocity = (step.velocity + 0.1).min(1.0);
                        }
                    }
                    Pattern::Euclidean(e) => e.velocity = (e.velocity + 0.1).min(1.0),
                    Pattern::ProbabilityMask(m) => m.velocity = (m.velocity + 0.1).min(1.0),
                }
            }
        }
    }
    s
}
