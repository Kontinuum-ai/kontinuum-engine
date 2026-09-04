//! Cross-cutting compose tests: determinism, structure variety, validation
//! cleanliness, energy influence, and block hot-swapping at bar boundaries.

use std::sync::Arc;

use kontinuum_compose::palette::hat_density;
use kontinuum_compose::{generate_session, ArrangementEngine, GenParams};
use kontinuum_ir::{compile_session, validate_session, IrDiff, Pattern};
use kontinuum_schedule::{BlockSource, CompiledBlock, Event};

const SR: u32 = 48_000;
const KICK: u8 = 0;
const PERC: u8 = 1;

fn params(seed: u64) -> GenParams {
    GenParams { seed, ..GenParams::default() }
}

fn blocks_for(seed: u64) -> Vec<Arc<CompiledBlock>> {
    compile_session(&generate_session(&params(seed)), SR).expect("compile")
}

fn noteon_velocities(block: &CompiledBlock, track: u8) -> Vec<f32> {
    block
        .tracks
        .iter()
        .find(|t| t.track == track)
        .map(|t| {
            t.events
                .iter()
                .filter_map(|(_, e)| match e {
                    Event::NoteOn { velocity, .. } => Some(*velocity),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Onsets per bar for `track` across absolute bars `[lo, hi)`.
fn density(blocks: &[Arc<CompiledBlock>], track: u8, lo: u32, hi: u32) -> f32 {
    let n: usize = blocks
        .iter()
        .filter(|b| b.start_bar >= lo && b.start_bar + b.bars <= hi)
        .map(|b| noteon_velocities(b, track).len())
        .sum();
    n as f32 / (hi - lo) as f32
}

fn mean_velocity(blocks: &[Arc<CompiledBlock>], track: u8, lo: u32, hi: u32) -> f32 {
    let mut n = 0usize;
    let mut sum = 0.0f32;
    for b in blocks.iter().filter(|b| b.start_bar >= lo && b.start_bar + b.bars <= hi) {
        for v in noteon_velocities(b, track) {
            n += 1;
            sum += v;
        }
    }
    if n == 0 { 0.0 } else { sum / n as f32 }
}

#[test]
fn sessions_validate_clean_across_seeds_targets_and_intensity() {
    for seed in 0..20u64 {
        for target in [128u32, 256] {
            let p = GenParams { seed, target_bars: target, ..GenParams::default() };
            let s = generate_session(&p);
            validate_session(&s)
                .unwrap_or_else(|e| panic!("seed {seed} target {target}: {e:?}"));
        }
    }
    for intensity in [0.0f32, 0.25, 0.75, 1.0] {
        let p = GenParams { seed: 9, intensity, ..GenParams::default() };
        validate_session(&generate_session(&p))
            .unwrap_or_else(|e| panic!("intensity {intensity}: {e:?}"));
    }
}

#[test]
fn same_seed_reproduces_session_and_compiled_events() {
    for seed in [11u64, 22, 33, 44, 55] {
        let a = generate_session(&params(seed));
        let b = generate_session(&params(seed));
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap(),
            "serialized session must be identical for seed {seed}"
        );
        let ea = compile_session(&a, SR).unwrap();
        let eb = compile_session(&b, SR).unwrap();
        assert_eq!(format!("{ea:?}"), format!("{eb:?}"), "compiled events differ for seed {seed}");
    }
    let x = serde_json::to_string(&generate_session(&params(11))).unwrap();
    let y = serde_json::to_string(&generate_session(&params(22))).unwrap();
    assert_ne!(x, y, "different seeds must produce different sessions");
}

#[test]
fn structure_varies_across_seeds() {
    let mut distinct = std::collections::HashSet::new();
    for seed in 0..20u64 {
        let s = generate_session(&params(seed));
        let layout: Vec<String> =
            s.sections.iter().map(|sec| format!("{}:{}", sec.id, sec.bars)).collect();
        eprintln!("seed {seed:2}: {}", layout.join(" | "));
        distinct.insert(layout.join(" "));
    }
    assert!(distinct.len() >= 5, "only {} distinct layouts across 20 seeds", distinct.len());
}

#[test]
fn breakdown_sections_reduce_kick_density() {
    for seed in 0..20u64 {
        let s = generate_session(&params(seed));
        let blocks = blocks_for(seed);
        let starts = s.section_start_bars();
        for (si, sec) in s.sections.iter().enumerate() {
            let (lo, hi) = (starts[si], starts[si] + sec.bars);
            let d = density(&blocks, KICK, lo, hi);
            if sec.id.starts_with("break_") {
                assert!(d <= 2.0, "seed {seed} {}: kick density {d}", sec.id);
            } else if sec.id.starts_with("dev_") {
                assert!(d >= 4.0, "seed {seed} {}: kick density {d}", sec.id);
            }
        }
    }
}

#[test]
fn energy_drives_density_and_velocity() {
    // The toolkit's density mapping is monotone in energy.
    assert_eq!(hat_density(0.0), 3);
    assert_eq!(hat_density(1.0), 10);
    assert!(hat_density(1.0) <= 11);
    assert!(hat_density(0.8) > hat_density(0.2));

    for seed in 0..8u64 {
        let s = generate_session(&params(seed));
        let blocks = blocks_for(seed);
        let starts = s.section_start_bars();
        let intro = s.sections.iter().position(|sec| sec.id == "intro").unwrap();
        let reintro = s.sections.iter().position(|sec| sec.id == "reintro").unwrap();
        let (lo_i, hi_i) = (starts[intro], starts[intro] + s.sections[intro].bars);
        let (lo_r, hi_r) = (starts[reintro], starts[reintro] + s.sections[reintro].bars);

        let d_intro = density(&blocks, PERC, lo_i, hi_i);
        let d_reintro = density(&blocks, PERC, lo_r, hi_r);
        assert!(
            d_reintro > d_intro,
            "seed {seed}: reintro perc density {d_reintro} must exceed intro {d_intro}"
        );

        let v_intro = mean_velocity(&blocks, KICK, lo_i, hi_i);
        let v_reintro = mean_velocity(&blocks, KICK, lo_r, hi_r);
        assert!(
            v_reintro > v_intro,
            "seed {seed}: kick velocity {v_reintro} must exceed intro {v_intro}"
        );
    }
}

#[test]
fn ten_minute_session_compiles_to_256_bars() {
    for seed in [2026u64, 1, 42] {
        let p = GenParams { seed, target_bars: 256, ..GenParams::default() };
        let s = generate_session(&p);
        assert_eq!(s.total_bars(), 256, "seed {seed} must hit the length target");
    }

    let p = GenParams { seed: 2026, target_bars: 256, ..GenParams::default() };
    let s = generate_session(&p);
    validate_session(&s).expect("validates");
    let blocks = compile_session(&s, SR).unwrap();
    assert_eq!(s.total_bars(), 256);
    assert_eq!(blocks.len(), 64, "64 chained 4-bar blocks");
    let last = blocks.last().unwrap();
    assert_eq!(last.start_bar + last.bars, 256);
    assert!(blocks.iter().all(|b| b.total_events() > 0));
}

#[test]
fn engine_hot_swaps_pattern_at_bar_boundary() {
    let session = generate_session(&params(7));
    // The grammar (#16) orders the middles, so target the first section
    // that actually binds the kick rather than assuming position 1.
    let (dev0_idx, dev0) = session
        .sections
        .iter()
        .enumerate()
        .find(|(_, s)| s.pattern_bindings.contains_key("kick"))
        .expect("a kick-bound section exists");
    let dev0_id = dev0.id.clone();
    let dev0_bars = dev0.bars;
    let dev0_start = session.section_start_bars()[dev0_idx];
    let mut engine = ArrangementEngine::new(session, SR);

    let before: Vec<_> = (0..4)
        .map(|i| engine.block_for_bars(i * 4, 4).unwrap())
        .collect();
    assert!(before.iter().all(|b| b.total_events() > 0));

    let fresh = kick_euclid(8);
    let swap_at = dev0_start + dev0_bars / 2;
    let diff = IrDiff::ReplacePattern {
        section: dev0_id.clone(),
        track: "kick".into(),
        pattern: Pattern::Euclidean(fresh.clone()),
    };
    let report = engine.apply_diff(&diff, swap_at).expect("mid-section swap lands");
    assert_eq!(report.applied.len(), 1);

    // Blocks before the swap bar are served from cache — bit-identical past audio.
    let past_blocks = swap_at as usize / 4;
    for i in 0..past_blocks {
        let after = engine.block_for_bars((i as u32) * 4, 4).unwrap();
        assert_eq!(format!("{:?}", before[i].tracks), format!("{:?}", after.tracks));
    }
    // The block containing the swap bar recompiles with k=8.
    let swap_block = (swap_at / 4) as usize;
    let swapped = engine.block_for_bars(swap_at - swap_at % 4, 4).unwrap();
    assert_ne!(
        format!("{:?}", before[swap_block].tracks),
        format!("{:?}", swapped.tracks)
    );
    assert_eq!(
        density(&[Arc::clone(&swapped)], KICK, swap_at - swap_at % 4, swap_at - swap_at % 4 + 4),
        8.0
    );

    // Blocks past the replaced section recompile to identical content.
    for (i, past) in before.iter().enumerate().skip(swap_block + 1) {
        let start = (i as u32) * 4;
        if start >= dev0_start + dev0_bars {
            let after = engine.block_for_bars(start, 4).unwrap();
            assert_eq!(format!("{:?}", past.tracks), format!("{:?}", after.tracks));
        }
    }
    // The engine's session reflects the edit.
    assert_eq!(
        engine.current_session().sections[dev0_idx].pattern_bindings["kick"],
        Pattern::Euclidean(fresh)
    );
}

fn kick_euclid(k: u32) -> kontinuum_ir::schema::EuclideanPattern {
    kontinuum_ir::schema::EuclideanPattern {
        generator: kontinuum_ir::schema::EuclideanTag::Euclidean,
        k,
        n: 16,
        rot: 0,
        velocity: 0.9,
        probability: 1.0,
        repeats: 1,
        gate: None,
        pitch: None,
    }
}

#[test]
fn engine_cache_hits_are_stable_and_purged_at_boundary() {
    let mut engine = ArrangementEngine::new(generate_session(&params(13)), SR);
    let a = engine.block_for_bars(0, 4).unwrap();
    let b = engine.block_for_bars(0, 4).unwrap();
    assert!(Arc::ptr_eq(&a, &b), "cache hit returns the same allocation");

    // Intro starts at bar 0, so a diff at_bar 0 anchors the boundary there:
    // every cached block is purged and recompiled fresh.
    let diff = IrDiff::SetSectionEnergy { id: "intro".into(), energy: vec![0.5] };
    engine.apply_diff(&diff, 0).expect("apply");
    let c = engine.block_for_bars(0, 4).unwrap();
    assert!(!Arc::ptr_eq(&a, &c), "purged blocks are recompiled");
}

#[test]
fn engine_merges_blocks_for_oversized_requests() {
    let mut engine = ArrangementEngine::new(generate_session(&params(21)), SR);
    let a = engine.block_for_bars(0, 4).unwrap();
    let b = engine.block_for_bars(4, 4).unwrap();
    let merged = engine.block_for_bars(0, 8).unwrap();
    assert_eq!(merged.bars, 8);
    assert_eq!(merged.start_frame, a.start_frame);
    assert_eq!(merged.total_events(), a.total_events() + b.total_events());
    assert!(engine.block_for_bars(u32::MAX - 4, 8).is_none(), "past session end");
}

#[test]
fn genre_concurrency_caps_hold() {
    for seed in 0..20u64 {
        // Caps mirror `genre::GenreSpec::max_concurrent` against a six-track
        // rig; the sparse styles still stay below the full rig.
        for (genre, cap) in [("microhouse", 5usize), ("minimal techno", 5), ("techno", 6)] {
            let s = generate_session(&GenParams {
                seed,
                genre: Some(genre.into()),
                ..GenParams::default()
            });
            for sec in &s.sections {
                if sec.id.starts_with("dev_") {
                    assert!(
                        sec.pattern_bindings.len() <= cap,
                        "seed {seed} {genre} {}: {} bindings exceed cap {cap}",
                        sec.id,
                        sec.pattern_bindings.len()
                    );
                }
            }
        }
    }
}

#[test]
fn breakdowns_collapse_to_one_or_two_elements() {
    for seed in 0..30u64 {
        let s = generate_session(&params(seed));
        let breakdowns: Vec<_> =
            s.sections.iter().filter(|sec| sec.id.starts_with("break_")).collect();
        if breakdowns.is_empty() {
            continue;
        }
        for sec in breakdowns {
            assert!(
                sec.pattern_bindings.len() <= 2,
                "seed {seed} {}: {} elements in breakdown",
                sec.id,
                sec.pattern_bindings.len()
            );
            assert!(!sec.pattern_bindings.is_empty());
        }
    }
}

#[test]
fn near_solo_passages_exist_per_five_minutes() {
    for (target, expect_min) in [(128u32, 1usize), (256, 2)] {
        for seed in 0..10u64 {
            let s = generate_session(&GenParams { seed, target_bars: target, ..params(seed) });
            let bound: Vec<_> = s
                .sections
                .iter()
                .filter(|sec| {
                    sec.id.starts_with("dev_") && sec.pattern_bindings.len() <= 2
                })
                .collect();
            assert!(
                bound.len() >= expect_min,
                "seed {seed} target {target}: only {} near-solo dev sections",
                bound.len()
            );
        }
    }
}

#[test]
fn sparse_genres_stage_their_rig_spectrally() {
    use kontinuum_ir::schema::InstrumentDef;
    for genre in ["microhouse", "minimal techno"] {
        let s = generate_session(&GenParams {
            seed: 7,
            genre: Some(genre.into()),
            ..GenParams::default()
        });
        let bass = s.tracks.iter().find(|t| t.id == "bass").unwrap();
        match &bass.instrument {
            InstrumentDef::Bass(b) => {
                // Staged below the default 900 Hz, but still allowed a voice.
                // Closing this to 250 Hz — as an earlier revision did, chasing
                // a mid-share ceiling — is how the midrange went hollow.
                assert!(b.cutoff_hz < 900.0, "{genre}: bass not staged: {}", b.cutoff_hz);
                assert!(b.cutoff_hz >= 400.0, "{genre}: bass carved out: {}", b.cutoff_hz);
            }
            other => panic!("{genre}: bass is not a bass: {other:?}"),
        }
        // The sparse racks (#88) carry a pluck where the old rig carried a
        // pad; either way the harmony voice stays felt-not-heard: quiet in
        // the mix, short gates, low velocities.
        let harmony = s
            .tracks
            .iter()
            .find(|t| {
                matches!(&t.instrument, InstrumentDef::Pad(_) | InstrumentDef::Pluck(_) | InstrumentDef::Stab(_))
            })
            .unwrap();
        assert!(harmony.gain <= 0.55, "{genre}: harmony gain {}", harmony.gain);
        for sec in &s.sections {
            if let Some(kontinuum_ir::Pattern::Steps(p)) = sec.pattern_bindings.get(&harmony.id) {
                for st in &p.steps {
                    assert!(st.gate.is_none_or(|g| g <= 2.5), "{genre}: harmony gate {:?}", st.gate);
                    assert!(st.velocity <= 0.5, "{genre}: harmony velocity {}", st.velocity);
                }
            }
        }
        let perc = s.tracks.iter().find(|t| t.id == "perc").unwrap();
        assert!(perc.gain <= 0.75, "{genre}: perc gain {}", perc.gain);
    }
}

#[test]
fn default_rig_stays_unstaged() {
    let s = generate_session(&params(7));
    let pad = s.tracks.iter().find(|t| t.id == "pad").unwrap();
    assert!((pad.gain - 0.7).abs() < 1e-4, "default pad gain drifted: {}", pad.gain);
}

#[test]
fn presence_lanes_breathe_and_respect_gain_bounds() {
    use kontinuum_ir::schema::bounds;
    for seed in 0..20u64 {
        let s = generate_session(&params(seed));
        let lanes: Vec<_> =
            s.sections.iter().flat_map(|sec| sec.automation.values()).collect();
        assert!(!lanes.is_empty(), "seed {seed}: no presence automation at all");
        for lane in &lanes {
            for (_, v, _) in &lane.points {
                assert!(
                    v >= &bounds::GAIN.0 && v <= &bounds::GAIN.1,
                    "seed {seed}: lane value {v} out of bounds"
                );
            }
        }
        // The breathing dip: some presence lane must fall below 0.35 (−9 dB).
        let dipped = s.sections.iter().any(|sec| {
            sec.automation.values().any(|l| {
                l.target_param == "gain" && l.points.iter().any(|(_, v, _)| *v < 0.35)
            })
        });
        assert!(dipped, "seed {seed}: no breathing dip below 0.35 anywhere");
    }
}

#[test]
fn pack_hotload_request_survives_until_taken_once() {
    use kontinuum_samples::PackLoader;
    let mut engine = ArrangementEngine::new(generate_session(&params(5)), SR);
    assert!(engine.take_pending_pack().is_none(), "no request initially");

    let mut loader = PackLoader::new(SR);
    let hash = loader
        .load(std::include_str!("../../../fixtures/recipes/dusty-micro-kit.json"))
        .expect("pack renders");
    engine.request_pack(1, hash);
    assert_eq!(engine.take_pending_pack(), Some((1, hash)));
    assert!(engine.take_pending_pack().is_none(), "consumed exactly once");
}

#[test]
fn motion_lanes_parse_stay_in_bounds_and_never_double_book() {
    use kontinuum_ir::schema::bounds;
    let mut saw_swell = false;
    for seed in 0..20u64 {
        let s = generate_session(&params(seed));
        validate_session(&s).unwrap_or_else(|e| panic!("seed {seed}: {e:?}"));
        let mut has_throw = false;
        for sec in &s.sections {
            assert!(
                sec.automation.len() <= s.tracks.len(),
                "seed {seed} {}: more lanes than tracks",
                sec.id
            );
            for (tid, lane) in &sec.automation {
                let lim = if lane.target_param == "gain" { bounds::GAIN } else { bounds::UNIT };
                let mut prev_bar = None;
                for (bar, v, _) in &lane.points {
                    assert!(
                        *v >= lim.0 && *v <= lim.1,
                        "seed {seed} {tid}: value {v} outside {lim:?}"
                    );
                    assert!(*bar < sec.bars, "seed {seed} {tid}: bar {bar} outside {}", sec.bars);
                    assert!(
                        prev_bar.is_none_or(|b| *bar > b),
                        "seed {seed} {tid}: bars must strictly ascend"
                    );
                    prev_bar = Some(*bar);
                }
                if tid == "perc" && lane.target_param == "send_delay" {
                    has_throw = true;
                    assert!(lane.points.iter().any(|(_, v, _)| *v >= 0.5), "throw must peak");
                }
                if tid == "pad" && lane.target_param == "send_reverb" {
                    saw_swell = saw_swell
                        || lane.points.iter().any(|(_, v, _)| *v >= 0.5);
                }
            }
        }
        assert!(has_throw, "seed {seed}: no delay throw anywhere");
    }
    assert!(saw_swell, "no reverb swell into any breakdown across all seeds");
}

#[test]
fn perc_ghosts_are_audible_in_data_and_never_accented() {
    let mut saw_ghost = false;
    for seed in 0..20u64 {
        let s = generate_session(&params(seed));
        for sec in &s.sections {
            let Some(Pattern::Steps(pat)) = sec.pattern_bindings.get("perc") else { continue };
            let ghosts: Vec<_> = pat.steps.iter().filter(|st| st.probability < 1.0).collect();
            // Fill bars can consume their ghost (the roll replaces the last
            // step), so the post-fill bound is 0..=3; ghost_pass itself pins
            // 1..=3 before the fill runs.
            assert!(
                (0..=3).contains(&ghosts.len()),
                "seed {seed} {}: {} ghosts in one bar",
                sec.id,
                ghosts.len()
            );
            saw_ghost = saw_ghost || !ghosts.is_empty();
            for st in &pat.steps {
                if st.probability < 1.0 {
                    assert!(st.velocity <= 0.33, "seed {seed}: ghost velocity {}", st.velocity);
                    assert!(st.pitch.is_none() && st.gate.is_none(), "ghosts are unpitched");
                    assert!(!st.accent, "seed {seed}: ghost accented");
                } else if st.position == 0 {
                    assert!(st.accent, "seed {seed} {}: perc downbeat unaccented", sec.id);
                }
            }
        }
    }
    assert!(saw_ghost, "no ghosts anywhere across all seeds");
}

#[test]
fn accented_steps_compile_at_boosted_velocity() {
    let mut seeds_with_accents = 0;
    for seed in 0..20u64 {
        let session = generate_session(&params(seed));
        let blocks = compile_session(&session, SR).expect("compile");
        let starts = session.section_start_bars();
        let mut saw_accent = false;
        for (si, sec) in session.sections.iter().enumerate() {
            let Some(Pattern::Steps(pat)) = sec.pattern_bindings.get("kick") else { continue };
            for st in &pat.steps {
                if !st.accent {
                    continue;
                }
                saw_accent = true;
                let want = (st.velocity * 1.2).clamp(0.0, 1.0);
                let (lo, hi) = (starts[si], starts[si] + sec.bars);
                let boosted = blocks
                    .iter()
                    .filter(|b| b.start_bar >= lo && b.start_bar + b.bars <= hi)
                    .flat_map(|b| {
                        b.tracks
                            .iter()
                            .find(|t| t.track == KICK)
                            .map(|t| t.events.as_slice())
                            .unwrap_or(&[])
                    })
                    .filter_map(|(_, e)| match e {
                        Event::NoteOn { velocity, .. } => Some(*velocity),
                        _ => None,
                    })
                    .any(|v| (v - want).abs() < 1e-4);
                assert!(
                    boosted,
                    "seed {seed} {}: accent at vel {} compiles to {want}",
                    sec.id, st.velocity
                );
            }
        }
        seeds_with_accents += usize::from(saw_accent);
    }
    assert!(seeds_with_accents > 0, "no accented kick onsets across all seeds");
}

#[test]
fn bass_archetype_pin_reaches_the_pattern_builder() {
    for seed in 0..20u64 {
        let pinned = generate_session(&GenParams {
            seed,
            bass_archetype: Some("call-response".into()),
            ..GenParams::default()
        });
        let mut pinned_bars = 0;
        for sec in &pinned.sections {
            let Some(Pattern::Steps(b)) = sec.pattern_bindings.get("bass") else { continue };
            pinned_bars += 1;
            // Call-response always musters >= 3 hits (2–4 call + 1–2 answer);
            // a fall-through draw would sometimes land the 1–2-hit dub-sub.
            // The reintro (#16 motif memory) plays a TRANSFORMED motif —
            // thinning or half-time legitimately drops below the archetype's
            // floor, so the archetype contract applies to fresh draws only.
            if sec.id == "reintro" {
                assert!(b.steps.first().is_some_and(|st| st.accent), "bass bar start unaccented");
                continue;
            }
            assert!(b.steps.len() >= 3, "seed {seed} {}: {} bass steps", sec.id, b.steps.len());
            assert!(b.steps.first().is_some_and(|st| st.accent), "bass bar start unaccented");
        }
        assert!(pinned_bars > 0, "seed {seed}: pin produced no bass sections");
        assert!(validate_session(&pinned).is_ok(), "seed {seed}: pinned session invalid");

        let plain = generate_session(&params(seed));
        let unknown = generate_session(&GenParams {
            seed,
            bass_archetype: Some("not-an-archetype".into()),
            ..GenParams::default()
        });
        assert_eq!(
            serde_json::to_string(&plain).unwrap(),
            serde_json::to_string(&unknown).unwrap(),
            "seed {seed}: unknown name must fall through to the seeded draw"
        );
    }
}

#[test]
fn every_bass_archetype_resolves_from_its_name() {
    for a in kontinuum_compose::bass::ALL {
        let drawn = kontinuum_compose::bass::pick(Some(a.name()), 0.5, &mut kontinuum_clock::Rng::from_seed(1));
        assert_eq!(drawn, a, "{}", a.name());
    }
}

#[test]
fn call_response_states_the_root_then_answers_on_the_fifth() {
    use kontinuum_compose::bass::{self, BassArchetype};
    let mid = 3840i64 / 2;
    for seed in 0..20u64 {
        let mut a = kontinuum_clock::Rng::from_seed(seed);
        let mut b = kontinuum_clock::Rng::from_seed(seed);
        let steps = bass::pattern(BassArchetype::CallResponse, 0.6, 0.1, &mut a);
        let twin = bass::pattern(BassArchetype::CallResponse, 0.6, 0.1, &mut b);
        assert_eq!(steps, twin, "seed {seed}: non-deterministic");
        assert!(!steps.is_empty());
        for st in &steps {
            assert!(st.position < 3840);
            assert!((0.0..=1.0).contains(&st.velocity));
            assert!(st.pitch.is_some());
        }
        // Humanization can nudge a boundary hit ±8 ticks across mid, so
        // classify by pitch and allow that jitter in the placement check.
        let call: Vec<_> =
            steps.iter().filter(|s| [36.0, 41.0].contains(&s.pitch.unwrap_or(0.0))).collect();
        let response: Vec<_> =
            steps.iter().filter(|s| [43.0, 48.0].contains(&s.pitch.unwrap_or(0.0))).collect();
        assert!((2..=4).contains(&call.len()), "seed {seed}: {} call hits", call.len());
        assert!((1..=2).contains(&response.len()), "seed {seed}: {} response hits", response.len());
        assert!(call.iter().all(|s| (s.position as i64) < mid + 16), "call in the first half");
        assert!(response.iter().all(|s| (s.position as i64) >= mid - 16), "response in the second half");
        assert!(steps.first().is_some_and(|s| s.accent), "seed {seed}: bar start unaccented");
    }
}

#[test]
fn every_bass_archetype_generates_valid_deterministic_sessions() {
    for a in kontinuum_compose::bass::ALL {
        for seed in [0u64, 7, 13] {
            let mk = || {
                generate_session(&GenParams {
                    seed,
                    bass_archetype: Some(a.name().into()),
                    ..GenParams::default()
                })
            };
            let (x, y) = (mk(), mk());
            assert_eq!(
                serde_json::to_string(&x).unwrap(),
                serde_json::to_string(&y).unwrap(),
                "{} non-deterministic at seed {seed}",
                a.name()
            );
            validate_session(&x).unwrap_or_else(|e| panic!("{} seed {seed}: {e:?}", a.name()));
        }
    }
}
