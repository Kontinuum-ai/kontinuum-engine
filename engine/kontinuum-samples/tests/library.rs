//! End-to-end test of the checked-in starter library (issue #19): manifest
//! integrity against the checked-in WAVs, then the v1 playback features —
//! choke groups, slot pitch, granular mode — through a real AudioGraph with
//! real library content.

use std::path::PathBuf;
use std::sync::Arc;

use hound::WavReader;
use kontinuum_core::graph::SampleTuning;
use kontinuum_core::AudioGraph;
use kontinuum_schedule::Event;
use kontinuum_samples::pack::PackManifest;

fn assets_dir() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/samples"))
}

fn load_library() -> (PackManifest, Vec<(String, Vec<f32>)>) {
    let dir = assets_dir();
    let manifest: PackManifest = serde_json::from_str(
        &std::fs::read_to_string(dir.join("manifest.json")).expect("manifest.json"),
    )
    .expect("manifest parse");
    let wavs = manifest
        .samples
        .iter()
        .map(|e| {
            let pcm = read_wav_f32(&dir.join(format!("{}.wav", e.id))).expect("wav");
            (e.id.clone(), pcm)
        })
        .collect();
    (manifest, wavs)
}

fn read_wav_f32(path: &PathBuf) -> Result<Vec<f32>, hound::Error> {
    let mut reader = WavReader::open(path)?;
    let mut out = Vec::new();
    for sample in reader.samples::<i16>() {
        out.push(sample? as f32 / 32_768.0);
    }
    Ok(out)
}

fn f32_le_bytes(pcm: &[f32]) -> Vec<u8> {
    pcm.iter().flat_map(|s| s.to_le_bytes()).collect()
}

/// Manifest hashes and frame counts must match the checked-in WAVs
/// bit-exactly: the library is reproducible from recipes, so any drift is
/// a build accident, not a content change.
#[test]
fn manifest_hashes_verify_the_checked_in_wavs() {
    let (manifest, wavs) = load_library();
    assert!(manifest.samples.len() >= 5, "starter library shrank");
    for entry in &manifest.samples {
        let pcm = &wavs
            .iter()
            .find(|(id, _)| *id == entry.id)
            .expect("wav for entry")
            .1;
        let hash = kontinuum_core::fnv1a64(&f32_le_bytes(pcm));
        assert_eq!(format!("{hash:016x}"), entry.pcm_hash, "hash: {}", entry.id);
        assert_eq!(pcm.len() as u32, entry.frames, "frames: {}", entry.id);
        assert_eq!(48_000, entry.sample_rate);
    }
    // The hat pair shares choke group 1 (909 convention, per the manifest).
    let open = manifest.samples.iter().find(|e| e.id == "hat-open").unwrap();
    let closed = manifest.samples.iter().find(|e| e.id == "hat-closed").unwrap();
    assert_eq!(open.choke_group, Some(1));
    assert_eq!(closed.choke_group, Some(1));
}

/// Attach the manifest choke pair through the graph (choke groups read
/// from the manifest, exactly as session setup will) and verify the open
/// hat is choked by the closed hat within 10 ms. The post-fade tail is
/// compared bit-exactly against a closed-hat-only reference render: any
/// un-choked open-hat contribution would break the equality.
#[test]
fn choke_pair_from_the_library_cuts_each_other() {
    let (manifest, wavs) = load_library();
    let open: Arc<[f32]> = wavs
        .iter()
        .find(|(id, _)| id == "hat-open")
        .unwrap()
        .1
        .clone()
        .into();
    let closed: Arc<[f32]> = wavs
        .iter()
        .find(|(id, _)| id == "hat-closed")
        .unwrap()
        .1
        .clone()
        .into();
    let group = manifest
        .samples
        .iter()
        .find(|e| e.id == "hat-open")
        .unwrap()
        .choke_group
        .expect("open hat choke group");

    let mut g = AudioGraph::new(48_000);
    g.set_mastering_bypass(true);
    g.attach_sampler_with_slices(
        0,
        Arc::clone(&open),
        48_000,
        Arc::from(Vec::new()),
        SampleTuning { choke_group: Some(group), ..SampleTuning::default() },
    );
    g.attach_sampler_with_slices(
        1,
        Arc::clone(&closed),
        48_000,
        Arc::from(Vec::new()),
        SampleTuning { choke_group: Some(group), ..SampleTuning::default() },
    );
    g.snap_track_gain(0, 1.0);
    g.snap_track_gain(1, 1.0);
    let events = vec![
        (0u32, 0u8, Event::SampleTrigger { sample_id: 0, slice: 0, rate: 1.0 }),
        (640u32, 0u8, Event::NoteOff { voice: 99 }),
        (640u32, 1u8, Event::SampleTrigger { sample_id: 0, slice: 0, rate: 1.0 }),
        (1280u32, 1u8, Event::NoteOff { voice: 99 }),
    ];
    let (mut l, mut r) = (vec![0.0; 24_000], vec![0.0; 24_000]);
    g.render_block(&mut l, &mut r, &events, 0);
    let mono: Vec<f32> = l.iter().zip(r.iter()).map(|(l, r)| 0.5 * (l + r)).collect();
    // Open hat sounds before the closed hat lands (its trigger applies at 640).
    assert!(mono[700..1_120].iter().any(|s| s.abs() > 0.001), "open hat silent");

    // Reference: the closed hat alone, same trigger frame.
    let mut ref_g = AudioGraph::new(48_000);
    ref_g.set_mastering_bypass(true);
    ref_g.attach_sampler_with_slices(
        1,
        closed,
        48_000,
        Arc::from(Vec::new()),
        SampleTuning { choke_group: Some(group), ..SampleTuning::default() },
    );
    ref_g.snap_track_gain(1, 1.0);
    let ref_events = vec![
        (640u32, 1u8, Event::SampleTrigger { sample_id: 0, slice: 0, rate: 1.0 }),
        (1280u32, 1u8, Event::NoteOff { voice: 99 }),
    ];
    let (mut rl, mut rr) = (vec![0.0; 24_000], vec![0.0; 24_000]);
    ref_g.render_block(&mut rl, &mut rr, &ref_events, 0);
    let ref_mono: Vec<f32> = rl.iter().zip(rr.iter()).map(|(l, r)| 0.5 * (l + r)).collect();
    assert!(ref_mono[1_400..1_800].iter().any(|s| s.abs() > 0.001), "reference silent");

    // After the 10 ms choke fade (open's trigger applied at 640; closed's
    // stamped the group at 1280), the mix tail equals closed-hat-alone.
    let settled = 1280 + 480 + 64;
    assert!(
        mono[settled..settled + 960]
            .iter()
            .zip(ref_mono[settled..settled + 960].iter())
            .all(|(a, b)| a.to_bits() == b.to_bits()),
        "open hat not choked: {:?} vs {:?}",
        &mono[settled..settled + 4],
        &ref_mono[settled..settled + 4]
    );
}

/// Slot transpose (issue #19 pitch) measurably changes the rendered content
/// of the same library kick under identical triggers.
#[test]
fn library_kick_transposes_measurably() {
    let (_, wavs) = load_library();
    let kick: Arc<[f32]> = wavs.iter().find(|(id, _)| id == "kick").unwrap().1.clone().into();
    let render = |tuning: SampleTuning| -> Vec<f32> {
        let mut g = AudioGraph::new(48_000);
        g.set_mastering_bypass(true);
        g.attach_sampler_with_slices(0, Arc::clone(&kick), 48_000, Arc::from(Vec::new()), tuning);
        g.snap_track_gain(0, 1.0);
        let events = vec![
            (0u32, 0u8, Event::SampleTrigger { sample_id: 0, slice: 0, rate: 1.0 }),
            (640u32, 0u8, Event::NoteOff { voice: 99 }),
        ];
        let (mut l, mut r) = (vec![0.0; 12_000], vec![0.0; 12_000]);
        g.render_block(&mut l, &mut r, &events, 0);
        l.iter().zip(r.iter()).map(|(l, r)| 0.5 * (l + r)).collect()
    };
    let neutral = render(SampleTuning::default());
    let up = render(SampleTuning { transpose_semitones: 12.0, ..SampleTuning::default() });
    assert!(neutral.iter().any(|s| *s != 0.0));
    let differing = neutral
        .iter()
        .zip(up.iter())
        .filter(|(a, b)| a.to_bits() != b.to_bits())
        .count();
    assert!(differing > 6_000, "transpose barely changed content: {differing}/12000");
}

/// The texture bed plays through the granular attach and is deterministic:
/// identical triggers render bit-identical clouds (issue #19 granular, RT).
#[test]
fn library_texture_renders_deterministic_granular_cloud() {
    let (_, wavs) = load_library();
    let texture: Arc<[f32]> =
        wavs.iter().find(|(id, _)| id == "texture").unwrap().1.clone().into();
    let run = || {
        let mut g = AudioGraph::new(48_000);
        g.set_mastering_bypass(true);
        g.attach_granular(
            0,
            Arc::clone(&texture),
            48_000,
            kontinuum_core::voice::GrainConfig {
                grain_ms: 70.0,
                density: 40.0,
                spray_ms: 90.0,
                pitch_jitter_cents: 30.0,
                level: 0.7,
            },
        );
        g.snap_track_gain(0, 1.0);
        let events = vec![
            (0u32, 0u8, Event::NoteOn { voice: 0, pitch: 60.0, velocity: 1.0, microtiming_ticks: 0 }),
            (640u32, 0u8, Event::NoteOff { voice: 0 }),
            (1280u32, 0u8, Event::NoteOff { voice: 99 }),
        ];
        let (mut l, mut r) = (vec![0.0; 12_000], vec![0.0; 12_000]);
        g.render_block(&mut l, &mut r, &events, 0);
        l.iter().zip(r.iter()).map(|(l, r)| 0.5 * (l + r)).collect::<Vec<_>>()
    };
    let a = run();
    let b = run();
    assert!(a.iter().any(|s| s.abs() > 0.001), "granular texture silent");
    assert!(a.iter().all(|s| s.is_finite()));
    assert!(
        a.iter().zip(b.iter()).all(|(x, y)| x.to_bits() == y.to_bits()),
        "granular texture not deterministic"
    );
}
