//! Every shipped genre profile must parse and carry sane targets (#52/#28).

use kontinuum_analysis::QualityProfile;

const PROFILES: &[&str] = &[
    "microhouse",
    "minimal-techno",
    "techno",
    "house",
    "deep-house",
];

#[test]
fn all_genre_profiles_parse_with_sane_targets() {
    for name in PROFILES {
        let path = format!("{}/../../fixtures/profiles/{name}.json", env!("CARGO_MANIFEST_DIR"));
        let p = QualityProfile::load(std::path::Path::new(&path))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(!p.targets.is_empty(), "{name}: no targets");
        for (metric, bound) in &p.targets {
            if let Some(min) = bound.min {
                assert!(min.is_finite() && min >= 0.0, "{name}/{metric}: min {min}");
            }
            if let Some(max) = bound.max {
                assert!(max.is_finite() && max >= 0.0, "{name}/{metric}: max {max}");
            }
            assert!(bound.min.is_some() || bound.max.is_some(), "{name}/{metric}: empty bound");
        }
    }
}

#[test]
fn sparser_genres_demand_more_dynamics_than_dense_ones() {
    let dir = |n: &str| format!("{}/../../fixtures/profiles/{n}.json", env!("CARGO_MANIFEST_DIR"));
    let load = |n: &str| QualityProfile::load(std::path::Path::new(&dir(n))).expect(n);
    let micro = load("microhouse");
    let deep = load("deep-house");
    let dyn_of = |p: &QualityProfile| p.targets["short_term_dyn_db"].min.expect("min");
    assert!(dyn_of(&micro) > dyn_of(&deep), "sparser styles breathe more");
    let mid_of = |p: &QualityProfile| p.targets["band_mid"].max.expect("max");
    assert!(mid_of(&micro) < mid_of(&deep), "sparser styles are scooped harder");
}
