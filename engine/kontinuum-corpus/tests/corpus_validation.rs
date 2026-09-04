//! End-to-end validation on the synthetic fixture (issue #23, scaled down).
//!
//! The fixture PLANTS structure — minimal-techno's intro→build→drop→groove
//! dominance, microhouse's longer intros and groove-dominant flow, two
//! energy-arc families and two groove families per subgenre — and these
//! tests assert the fitters recover exactly that. Everything here is a
//! pipeline-shape gate: it says nothing about musical truth until the real
//! corpus lands.

use std::collections::BTreeMap;
use std::path::Path;

use kontinuum_corpus::{
    fit_subgenre, groove_feature, load_jsonl_file, track_arc, ArcCluster, ArrangementParamsArtifact,
    GrooveTemplatesArtifact, SubgenreFit, TrackObservation,
};
use kontinuum_corpus::{stats, ARTIFACT_VERSION};

fn fixture_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/corpus-sample.jsonl")
}

fn load_observations() -> Vec<TrackObservation> {
    load_jsonl_file(&fixture_path()).expect("fixture must parse")
}

fn fits() -> (SubgenreFit, SubgenreFit) {
    let obs = load_observations();
    let minimal: Vec<TrackObservation> =
        obs.iter().filter(|o| o.subgenre == "minimal-techno").cloned().collect();
    let microhouse: Vec<TrackObservation> =
        obs.iter().filter(|o| o.subgenre == "microhouse").cloned().collect();
    (
        fit_subgenre(&minimal).expect("minimal fit"),
        fit_subgenre(&microhouse).expect("microhouse fit"),
    )
}

/// Fixture track ids end in a zero-padded index whose parity is the planted
/// family (even = family A, odd = family B).
fn family_of(track_id: &str) -> usize {
    track_id[track_id.len() - 2..].parse::<usize>().expect("two-digit suffix") % 2
}

fn argmax(row: &BTreeMap<String, f32>) -> (&str, f32) {
    row.iter().fold(("", 0.0f32), |(k, p), (k2, p2)| if *p2 > p { (k2.as_str(), *p2) } else { (k, p) })
}

#[test]
fn fixture_loads_with_both_subgenres() {
    let obs = load_observations();
    assert_eq!(obs.len(), 30);
    assert_eq!(obs.iter().filter(|o| o.subgenre == "minimal-techno").count(), 15);
    assert_eq!(obs.iter().filter(|o| o.subgenre == "microhouse").count(), 15);
    assert!(obs.iter().all(|o| !o.sections.is_empty()));
    assert!(obs.iter().all(|o| o.groove.is_some()));
}

#[test]
fn planted_transition_structure_is_recovered() {
    let (minimal, microhouse) = fits();
    // The planted dominance chains (Laplace α=1 keeps every floor cell
    // positive, so argmax is meaningful only against planted counts).
    for (kind, expected) in
        [("intro", "build"), ("build", "drop"), ("drop", "groove"), ("groove", "outro")]
    {
        let (to, p) = argmax(&minimal.transition_matrix[kind]);
        assert_eq!(to, expected, "minimal {kind}→? argmax (p={p})");
    }
    assert_eq!(argmax(&minimal.transition_matrix["break"]).0, "drop");
    for (kind, expected) in
        [("intro", "groove"), ("groove", "outro"), ("drop", "groove"), ("break", "drop")]
    {
        let (to, p) = argmax(&microhouse.transition_matrix[kind]);
        assert_eq!(to, expected, "microhouse {kind}→? argmax (p={p})");
    }
    // The intro rows differ across subgenres — the longer-intros plant:
    // minimal goes intro→build, microhouse intro→groove.
    assert_eq!(argmax(&minimal.transition_matrix["intro"]).0, "build");
    assert_eq!(argmax(&microhouse.transition_matrix["intro"]).0, "groove");
}

#[test]
fn planted_transition_types_are_recovered() {
    let (minimal, _) = fits();
    assert_eq!(argmax(&minimal.transition_types["build->drop"]).0, "filter_sweep");
    assert_eq!(argmax(&minimal.transition_types["groove->break"]).0, "silence");
    assert_eq!(argmax(&minimal.transition_types["groove->outro"]).0, "hard_cut");
}

#[test]
fn planted_arc_families_separate() {
    let obs = load_observations();
    for subgenre in ["minimal-techno", "microhouse"] {
        let tracks: Vec<&TrackObservation> =
            obs.iter().filter(|o| o.subgenre == subgenre).collect();
        let arcs: Vec<Vec<f32>> = tracks.iter().map(|t| track_arc(t)).collect();
        let clusters = stats::kmeans(&arcs, 2);
        assert_eq!(clusters.len(), 2);
        // Family parity must map 1:1 onto cluster labels — the planted
        // shapes (rise-peak-fall vs steady climb; flat vs double peak)
        // are far apart relative to the ±0.01 planting wobble.
        let label = |cluster: &kontinuum_corpus::stats::KMeansCluster| {
            family_of(&tracks[cluster.members[0]].track_id)
        };
        for c in &clusters {
            let fam = label(c);
            assert!(
                c.members.iter().all(|&m| family_of(&tracks[m].track_id) == fam),
                "{subgenre}: cluster mixes families: {:?}",
                c.members.iter().map(|&m| tracks[m].track_id.clone()).collect::<Vec<_>>(),
            );
        }
        assert_ne!(label(&clusters[0]), label(&clusters[1]));
    }
}

#[test]
fn planted_groove_families_recover_cluster_count() {
    let obs = load_observations();
    for subgenre in ["minimal-techno", "microhouse"] {
        let tracks: Vec<&TrackObservation> =
            obs.iter().filter(|o| o.subgenre == subgenre).collect();
        let feats: Vec<Vec<f32>> =
            tracks.iter().map(|t| groove_feature(t.groove.as_ref().unwrap())).collect();
        let clusters = stats::kmeans(&feats, 2);
        assert_eq!(clusters.len(), 2, "{subgenre}: two planted groove families");
        let label = |cluster: &kontinuum_corpus::stats::KMeansCluster| {
            family_of(&tracks[cluster.members[0]].track_id)
        };
        for c in &clusters {
            let fam = label(c);
            assert!(
                c.members.iter().all(|&m| family_of(&tracks[m].track_id) == fam),
                "{subgenre}: groove cluster mixes families"
            );
        }
        assert_ne!(label(&clusters[0]), label(&clusters[1]));
    }
}

#[test]
fn emitter_roundtrips_through_json() {
    let (minimal, _) = fits();
    let a: ArrangementParamsArtifact = kontinuum_corpus::emit(&minimal);
    assert_eq!(a.artifact_version, ARTIFACT_VERSION);
    assert_eq!(a.corpus_size, 15);
    assert_eq!(a.subgenre, "minimal-techno");
    assert_eq!(a.energy_arcs.len(), 5, "k=5 arc families (k≈5 per the issue)");
    assert_eq!(a.section_lengths["intro"].mean_bars, 8.0);
    let text = serde_json::to_string(&a).unwrap();
    assert_eq!(kontinuum_corpus::load_arrangement(&text).unwrap(), a);

    let g: GrooveTemplatesArtifact = kontinuum_corpus::emit_groove(&minimal);
    // The fixture plants exactly two groove families per subgenre with
    // identical profiles inside a family — farthest-first seeding
    // collapses duplicates, so the fit recovers the planted count (2),
    // not the k≈5 ceiling. Real corpora with diverse grooves get k=5.
    assert_eq!(g.templates.len(), 2);
    assert_eq!(g.templates.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["t0", "t1"]);
    assert!(g.templates[0].swing < g.templates[1].swing, "names ordered by swing");
    let gtext = serde_json::to_string(&g).unwrap();
    assert_eq!(kontinuum_corpus::load_groove(&gtext).unwrap(), g);
}

#[test]
fn artifacts_write_and_reload_from_disk() {
    let (minimal, _) = fits();
    let dir = std::env::temp_dir().join("kontinuum-corpus-roundtrip");
    std::fs::create_dir_all(&dir).unwrap();
    let (a_path, g_path) = kontinuum_corpus::write_artifacts(&minimal, &dir).unwrap();
    assert_eq!(a_path.file_name().unwrap(), "arrangement-params-minimal-techno.json");
    assert_eq!(g_path.file_name().unwrap(), "groove-templates-minimal-techno.json");
    let a = kontinuum_corpus::load_arrangement(&std::fs::read_to_string(&a_path).unwrap()).unwrap();
    assert_eq!(a, kontinuum_corpus::emit(&minimal));
    let g = kontinuum_corpus::load_groove(&std::fs::read_to_string(&g_path).unwrap()).unwrap();
    assert_eq!(g, kontinuum_corpus::emit_groove(&minimal));
}

#[test]
fn fitting_is_bit_deterministic() {
    let obs = load_observations();
    let first = fit_subgenre(&obs.iter().filter(|o| o.subgenre == "microhouse")
        .cloned().collect::<Vec<_>>())
    .unwrap();
    let mut shuffled: Vec<TrackObservation> =
        obs.iter().filter(|o| o.subgenre == "microhouse").cloned().collect();
    shuffled.reverse();
    let second = fit_subgenre(&shuffled).unwrap();
    assert_eq!(
        serde_json::to_string(&kontinuum_corpus::emit(&first)).unwrap(),
        serde_json::to_string(&kontinuum_corpus::emit(&second)).unwrap(),
        "fit must be independent of input order, byte for byte"
    );
    assert_eq!(
        serde_json::to_string(&kontinuum_corpus::emit_groove(&first)).unwrap(),
        serde_json::to_string(&kontinuum_corpus::emit_groove(&second)).unwrap()
    );
}

#[test]
fn arc_clusters_carry_centroid_shape_and_weight() {
    let (minimal, _) = fits();
    let total_weight: f32 = minimal.energy_arcs.iter().map(|a: &ArcCluster| a.weight).sum();
    assert!((total_weight - 1.0).abs() < 1e-4, "weights partition the corpus");
    for arc in &minimal.energy_arcs {
        assert_eq!(arc.centroid.len(), 8);
        assert!(arc.weight > 0.0, "planted fixture has no empty families: {arc:?}");
        assert!(arc.spread < 0.05, "planted families are tight: {}", arc.spread);
    }
}
