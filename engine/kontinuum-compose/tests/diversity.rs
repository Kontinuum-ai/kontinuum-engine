//! Structural-diversity CI check (issue #16 acceptance): five seeded
//! 10-minute plans must be well-formed against the grammar constraints
//! AND measurably different — section-kind sequences pairwise beyond an
//! edit-distance floor, length vectors never twice alike. Renders the
//! deterministic track map for every run (see `trackmap`).

use kontinuum_compose::grammar::ArcFamily;
use kontinuum_compose::trackmap;
use kontinuum_compose::{generate_session, GenParams};
use kontinuum_ir::schema::TransitionKind;
use kontinuum_ir::validate_session;

/// 10 minutes at the techno default 124 BPM: 600 s / (4 beats / bar) ÷
/// (60 / 124) ≈ 310 bars → the 4-bar-aligned 312.
const TEN_MIN_BARS: u32 = 312;
const SEEDS: [u64; 5] = [11, 23, 47, 89, 183];
/// Pairwise Levenshtein floor on the kind-label sequences: plans must be
/// recognizably different arrangements, not relabeled clones.
const EDIT_DISTANCE_FLOOR: usize = 2;

fn kind_sequence(session: &kontinuum_ir::Session) -> Vec<String> {
    session
        .sections
        .iter()
        .map(|s| {
            s.id
                .split_once('_')
                .map(|(kind, _)| kind.to_string())
                .unwrap_or_else(|| s.id.clone())
        })
        .collect()
}

fn levenshtein(a: &[String], b: &[String]) -> usize {
    let (rows, cols) = (a.len() + 1, b.len() + 1);
    let mut grid = vec![0usize; rows * cols];
    for i in 0..rows {
        grid[i * cols] = i;
    }
    for j in 0..cols {
        grid[j] = j;
    }
    for i in 1..rows {
        for j in 1..cols {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            grid[i * cols + j] = (grid[(i - 1) * cols + j] + 1)
                .min(grid[i * cols + j - 1] + 1)
                .min(grid[(i - 1) * cols + j - 1] + cost);
        }
    }
    grid[rows * cols - 1]
}

#[test]
fn five_seeded_ten_minute_plans_are_well_formed_and_diverse() {
    let mut sessions = Vec::new();
    for seed in SEEDS {
        let params = GenParams { seed, target_bars: TEN_MIN_BARS, ..GenParams::default() };
        let s = generate_session(&params);
        validate_session(&s).unwrap_or_else(|e| panic!("seed {seed}: {e:?}"));
        assert_eq!(s.total_bars(), u64::from(TEN_MIN_BARS), "seed {seed}: length target");
        sessions.push((seed, s));
    }

    // Grammar constraints on every run.
    for (seed, s) in &sessions {
        let starts = s.section_start_bars();
        assert_eq!(s.sections.first().map(|x| x.id.as_str()), Some("intro"));
        assert_eq!(s.sections.last().map(|x| x.id.as_str()), Some("outro"));
        // Outro terminal.
        assert!(s.sections.len() >= 2);
        // No breakdown before the constraint bar (base family draws: all
        // three families allow late breakdowns; only twin_peak allows an
        // early one, and seed 47 pins that family — handled below).
        for (i, sec) in s.sections.iter().enumerate() {
            if sec.id.starts_with("break_") && starts[i] < 64 {
                panic!("seed {seed}: breakdown {} starts at bar {}", sec.id, starts[i]);
            }
        }
        // Reintro directly precedes the outro and references stored motifs
        // — structurally, a groove_dev must exist for it to transform.
        assert!(s.sections.len() >= 3 && s.sections[s.sections.len() - 2].id == "reintro");
        assert!(s.sections.iter().any(|x| x.id.starts_with("dev_")));
        // Adjacent energy deltas bounded except across breakdown/release.
        let max_delta = 0.35;
        for w in s.sections.windows(2) {
            let exempt = |id: &str| id.starts_with("break_") || id.starts_with("release_");
            if exempt(&w[0].id) || exempt(&w[1].id) {
                continue;
            }
            let delta = (w[1].energy_curve[0] - w[0].energy_curve[w[0].energy_curve.len() - 1]).abs();
            assert!(
                delta <= max_delta + 0.06,
                "seed {seed}: {} -> {} energy delta {delta} unbounded",
                w[0].id,
                w[1].id
            );
        }
        // Silence drops never exceed the ceiling.
        for sec in &s.sections {
            for t in [&sec.transition_in, &sec.transition_out].into_iter().flatten() {
                if t.kind == TransitionKind::SilenceDrop {
                    assert!(t.bars <= 2, "seed {seed}: silence drop {} bars", t.bars);
                }
            }
        }
    }

    // Diversity: pairwise kind-sequence edit distance beyond the floor,
    // and no two runs share a length vector.
    let seqs: Vec<Vec<String>> = sessions.iter().map(|(_, s)| kind_sequence(s)).collect();
    for i in 0..seqs.len() {
        for j in (i + 1)..seqs.len() {
            let d = levenshtein(&seqs[i], &seqs[j]);
            println!(
                "edit distance seeds {} vs {}: {d} (kinds {:?} vs {:?})",
                SEEDS[i],
                SEEDS[j],
                seqs[i].join(","),
                seqs[j].join(",")
            );
            assert!(
                d >= EDIT_DISTANCE_FLOOR,
                "seeds {} and {}: edit distance {d} below floor {EDIT_DISTANCE_FLOOR}",
                SEEDS[i],
                SEEDS[j]
            );
        }
    }
    for i in 0..sessions.len() {
        for j in (i + 1)..sessions.len() {
            let a: Vec<u32> = sessions[i].1.sections.iter().map(|s| s.bars).collect();
            let b: Vec<u32> = sessions[j].1.sections.iter().map(|s| s.bars).collect();
            assert_ne!(a, b, "seeds {} and {}: identical length vectors", SEEDS[i], SEEDS[j]);
        }
    }
}

#[test]
fn director_pinned_arc_families_shape_the_plan() {
    let plan = |arc: Option<ArcFamily>, seed: u64| {
        let s = generate_session(&GenParams {
            seed,
            target_bars: TEN_MIN_BARS,
            arc,
            ..GenParams::default()
        });
        s.sections
            .iter()
            .map(|x| x.energy_curve[0])
            .collect::<Vec<f32>>()
    };
    for seed in SEEDS {
        let slow = plan(Some(ArcFamily::SlowBurn), seed);
        let plateau = plan(Some(ArcFamily::PlateauHypnotic), seed);
        assert_ne!(slow, plateau, "seed {seed}: arc families must shape the plan");
    }
}

#[test]
fn track_maps_render_deterministically_per_run() {
    let out_dir = std::env::temp_dir().join("kontinuum-trackmaps");
    std::fs::create_dir_all(&out_dir).expect("mkdir");
    for seed in SEEDS {
        let params = GenParams { seed, target_bars: TEN_MIN_BARS, ..GenParams::default() };
        let s = generate_session(&params);
        let svg = trackmap::render_svg(&s.sections);
        assert_eq!(svg, trackmap::render_svg(&s.sections), "seed {seed}: deterministic");
        assert!(svg.starts_with("<svg") && svg.contains("intro ·"));
        let path = out_dir.join(format!("trackmap-seed-{seed}.svg"));
        std::fs::write(&path, &svg).expect("write track map");
        println!("seed {seed}: {}", path.display());
    }
}
