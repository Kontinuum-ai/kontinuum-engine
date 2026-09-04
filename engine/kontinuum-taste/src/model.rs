//! The metadata taste model (issue #21): a library of [`LibraryEvent`]s →
//! entity graph → the metadata fields of the canonical DNA.
//!
//! Affinity weights per the spec: **saved > playlisted > recently-played**
//! (top-* items slot between saved and playlists — a deliberate top-2
//! ranking, documented in docs/dna-mapping.md). Every contribution decays
//! with a ~90-day half-life at read time, so a library gone quiet fades.

use std::collections::BTreeMap;

use kontinuum_compose::taste::{Stat, TasteProfile};

use crate::store::{EventContext, LibraryEvent};

/// Affinity of one library event by how the item entered the library.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Affinity {
    Saved,
    Top,
    Playlist,
    Recent,
}

impl Affinity {
    /// Spec ordering: saved (1.0) > top (0.9) > playlisted (0.6) >
    /// recently-played (0.3).
    pub fn weight(self) -> f32 {
        match self {
            Affinity::Saved => 1.0,
            Affinity::Top => 0.9,
            Affinity::Playlist => 0.6,
            Affinity::Recent => 0.3,
        }
    }

    fn of(event: &LibraryEvent) -> Affinity {
        match event.context {
            EventContext::Saved => Affinity::Saved,
            EventContext::TopTracks | EventContext::TopArtists => Affinity::Top,
            EventContext::Playlist => Affinity::Playlist,
            EventContext::RecentlyPlayed => Affinity::Recent,
        }
    }
}

/// Recency decay half-life, days (issue #21: ~90).
pub const HALF_LIFE_DAYS: f64 = 90.0;
const MS_PER_DAY: f64 = 86_400_000.0;

/// One entity's accumulated, decaying weight.
#[derive(Clone, Debug, PartialEq)]
pub struct EntityNode {
    pub name: String,
    pub weight: f32,
    pub last_seen_ms: i64,
    /// Distinct tracks that touched this entity (diversity evidence).
    pub track_count: u32,
}

/// The entity graph: genre / artist / label nodes over one source's
/// library events. BTreeMap everywhere → deterministic iteration.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EntityGraph {
    pub genres: BTreeMap<String, EntityNode>,
    pub artists: BTreeMap<String, EntityNode>,
    pub labels: BTreeMap<String, EntityNode>,
}

impl EntityGraph {
    /// Builds the graph from events, decaying every prior contribution by
    /// the time since it was seen, then adding the new event's weight.
    /// `now_ms` anchors the decay (tests inject a fixed clock).
    pub fn build(events: &[LibraryEvent], now_ms: i64) -> Self {
        let mut graph = EntityGraph::default();
        let decay_factor = |from_ms: i64| -> f32 {
            let days = ((now_ms - from_ms).max(0) as f64 / MS_PER_DAY).min(365.0 * 80.0);
            0.5f64.powf(days / HALF_LIFE_DAYS) as f32
        };
        for e in events {
            let contribution = Affinity::of(e).weight() * decay_factor(e.occurred_ms);
            let add = |map: &mut BTreeMap<String, EntityNode>, name: &str| {
                let node = map.entry(name.to_string()).or_insert_with(|| EntityNode {
                    name: name.to_string(),
                    weight: 0.0,
                    last_seen_ms: e.occurred_ms,
                    track_count: 0,
                });
                node.weight *= decay_factor(node.last_seen_ms);
                node.weight += contribution;
                node.last_seen_ms = node.last_seen_ms.max(e.occurred_ms);
                node.track_count += 1;
            };
            for g in &e.genres {
                add(&mut graph.genres, g);
            }
            if !e.artist.is_empty() {
                add(&mut graph.artists, &e.artist);
            }
            if let Some(label) = &e.label {
                add(&mut graph.labels, label);
            }
        }
        graph
    }

    /// Normalized weight distribution over a node class, descending.
    fn mix(nodes: &BTreeMap<String, EntityNode>) -> Vec<(String, f32)> {
        let total: f32 = nodes.values().map(|n| n.weight).sum();
        if total <= 0.0 {
            return Vec::new();
        }
        let mut out: Vec<(String, f32)> = nodes
            .values()
            .map(|n| (n.name.clone(), n.weight / total))
            .collect();
        out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        out
    }

    /// Normalized Shannon entropy of the genre mix, 0..1 — the
    /// adventurousness (catalog diversity) score. One dominant genre → 0;
    /// spread evenly over k genres → 1.
    pub fn adventurousness(&self) -> Option<f32> {
        let weights: Vec<f64> = self.genres.values().map(|n| f64::from(n.weight)).collect();
        let total: f64 = weights.iter().sum();
        if weights.len() < 2 || total <= 0.0 {
            return if weights.is_empty() { None } else { Some(0.0) };
        }
        let entropy: f64 = weights
            .iter()
            .filter(|w| **w > 0.0)
            .map(|w| {
                let p = w / total;
                -p * p.ln()
            })
            .sum();
        let max_entropy = (weights.len() as f64).ln();
        Some((entropy / max_entropy) as f32)
    }

    /// Decade buckets ("1990s") from release years, weighted by each
    /// event's affinity.
    pub fn era_weights(&self, events: &[LibraryEvent]) -> Vec<(String, f32)> {
        let mut eras: BTreeMap<String, f32> = BTreeMap::new();
        for e in events {
            if let Some(year) = e.release_year {
                let decade = (year / 10) * 10;
                *eras.entry(format!("{decade}s")).or_default() += Affinity::of(e).weight();
            }
        }
        normalize(eras)
    }

    /// The metadata fields of the DNA, ready to merge into a profile.
    pub fn into_profile(&self, events: &[LibraryEvent]) -> TasteProfile {
        let mut p = TasteProfile::default();
        p.genre_mix = Self::mix(&self.genres);
        p.genres = p.genre_mix.iter().take(8).map(|(g, _)| g.clone()).collect();
        if let Some(adv) = self.adventurousness() {
            p.adventurousness = Some(adv);
        }
        p.era_weights = self.era_weights(events);
        p.scene_weights = Self::mix(&self.labels);
        // The same seasoning the raw-JSON path gets (compose::apply_genre).
        let genres = p.genres.clone();
        for genre in &genres {
            p.apply_genre_nudge(genre);
        }
        // Tempo priors from metadata-stated BPMs (e.g. SoundCloud-style
        // fields; Spotify exposes none). Weighted by affinity.
        let tempos: Vec<(f64, f64)> = events
            .iter()
            .filter_map(|e| e.bpm.map(|b| (b, f64::from(Affinity::of(e).weight()))))
            .filter(|(b, _)| (30.0..=300.0).contains(b))
            .collect();
        if !tempos.is_empty() {
            let wsum: f64 = tempos.iter().map(|(_, w)| w).sum();
            let mean = tempos.iter().map(|(b, w)| b * w).sum::<f64>() / wsum;
            p.bpm = Some(mean);
            let var = tempos.iter().map(|(b, w)| w * (b - mean) * (b - mean)).sum::<f64>() / wsum;
            p.tempo_dispersion = Some(var.sqrt());
        }
        p
    }
}

fn normalize(map: BTreeMap<String, f32>) -> Vec<(String, f32)> {
    let total: f32 = map.values().sum();
    if total <= 0.0 {
        return Vec::new();
    }
    let mut out: Vec<(String, f32)> = map
        .into_iter()
        .map(|(k, w)| (k, w / total))
        .collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

/// Builds the metadata half of a source's DNA from its stored events.
pub fn profile_from_events(source_events: &[LibraryEvent], now_ms: i64) -> TasteProfile {
    EntityGraph::build(source_events, now_ms).into_profile(source_events)
}

/// Dispersion (population std dev) of a weighted sample set.
pub(crate) fn weighted_stat(samples: &[(f32, f32)]) -> Option<Stat> {
    let wsum: f32 = samples.iter().map(|(_, w)| w).sum();
    if samples.is_empty() || wsum <= 0.0 {
        return None;
    }
    let mean = samples.iter().map(|(v, w)| v * w).sum::<f32>() / wsum;
    let var = samples.iter().map(|(v, w)| w * (v - mean) * (v - mean)).sum::<f32>() / wsum;
    Some(Stat::new(mean, var.sqrt()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(ctx: EventContext, artist: &str, genres: &[&str], year: Option<i32>, occurred: i64) -> LibraryEvent {
        LibraryEvent {
            context: ctx,
            artist: artist.into(),
            track: format!("{artist} — track"),
            album: None,
            label: None,
            release_year: year,
            genres: genres.iter().map(|s| s.to_string()).collect(),
            bpm: None,
            occurred_ms: occurred,
        }
    }

    #[test]
    fn saved_outranks_top_outranks_playlist_outranks_recent() {
        let now = 1_000_000_000i64;
        let events = vec![
            ev(EventContext::Saved, "saved-act", &["minimal techno"], None, now),
            ev(EventContext::TopArtists, "top-act", &["dub techno"], None, now),
            ev(EventContext::Playlist, "pl-act", &["microhouse"], None, now),
            ev(EventContext::RecentlyPlayed, "recent-act", &["trance"], None, now),
        ];
        let g = EntityGraph::build(&events, now);
        let genres = EntityGraph::mix(&g.genres);
        let w = |name: &str| genres.iter().find(|(n, _)| n == name).unwrap().1;
        let total: f32 = 1.0 + 0.9 + 0.6 + 0.3;
        assert!((w("minimal techno") - 1.0 / total).abs() < 1e-6);
        assert!(w("dub techno") > w("microhouse"));
        assert!(w("microhouse") > w("trance"));
    }

    #[test]
    fn ninety_day_half_life_decays_weight() {
        let now = 1_000_000_000i64;
        // Two identical saved events; one today, one 90 days ago.
        let events = vec![
            ev(EventContext::Saved, "fresh", &["techno"], None, now),
            ev(EventContext::Saved, "stale", &["techno"], None, now - (90 * 86_400_000)),
        ];
        let g = EntityGraph::build(&events, now);
        // The stale one decayed to half before its contribution was added.
        let fresh = &g.artists["fresh"];
        let stale = &g.artists["stale"];
        assert!((stale.weight * 2.0 - fresh.weight).abs() < 1e-4, "stale {} fresh {}", stale.weight, fresh.weight);
    }

    #[test]
    fn adventurousness_scores_diversity() {
        let now = 1_000_000i64;
        let narrow = vec![ev(EventContext::Saved, "a", &["minimal techno"], None, now); 10];
        let wide: Vec<LibraryEvent> = ["minimal techno", "dub techno", "microhouse", "ambient"]
            .iter()
            .flat_map(|g| {
                (0..10).map(move |i| {
                    let mut e = ev(EventContext::Saved, &format!("artist-{g}-{i}"), &[g], None, now);
                    e.track = format!("t{i}");
                    e
                })
            })
            .collect();
        let gn = EntityGraph::build(&narrow, now);
        let gw = EntityGraph::build(&wide, now);
        assert_eq!(gn.adventurousness(), Some(0.0), "one genre is zero adventurousness");
        let wide_score = gw.adventurousness().unwrap();
        assert!(wide_score > 0.9, "even spread over 4 genres should approach 1: {wide_score}");
        assert!(EntityGraph::build(&[], now).adventurousness().is_none());
    }

    #[test]
    fn profile_carries_mix_eras_scenes_and_tempo_prior() {
        let now = 1_000_000i64;
        let events = vec![
            {
                let mut e = ev(EventContext::Saved, "perlon-ish", &["minimal techno"], Some(1999), now);
                e.label = Some("Perlon".into());
                e.bpm = Some(128.0);
                e
            },
            {
                let mut e = ev(EventContext::RecentlyPlayed, "kompakt-ish", &["ambient"], Some(2003), now);
                e.label = Some("Kompakt".into());
                e.bpm = Some(120.0);
                e
            },
        ];
        let p = profile_from_events(&events, now);
        assert_eq!(p.genre_mix[0].0, "minimal techno", "saved outranks recent");
        assert!(p.genres.contains(&"ambient".to_string()));
        assert_eq!(p.era_weights.first().map(|(e, _)| e.as_str()), Some("1990s"));
        assert_eq!(p.scene_weights[0].0, "Perlon", "labels become scenes");
        assert!(p.bpm.is_some());
        assert!(p.tempo_dispersion.unwrap() > 0.0, "two distinct tempos disperse");
    }
}
