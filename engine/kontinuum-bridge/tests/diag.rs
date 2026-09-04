use kontinuum_bridge::KontinuumEngine;
const SR: u32 = 48_000;
#[test]
fn dump_live_render() {
    let fixture = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/loop-4track.ir.json")).unwrap();
    let mut e = KontinuumEngine::new(SR, &fixture).unwrap();
    e.play();
    let seconds = 8.0;
    let chunks = (SR as usize / 512) * seconds as usize;
    let mut samples = Vec::with_capacity(chunks * 512);
    for _ in 0..chunks {
        let mut l = [0.0f32; 512];
        let mut r = [0.0f32; 512];
        e.render(&mut l, &mut r);
        samples.extend_from_slice(&l);
    }
    // Per-second RMS + peak
    for sec in 0..8 {
        let start = sec * SR as usize;
        let end = ((sec + 1) * SR as usize).min(samples.len());
        if start >= end { break; }
        let win = &samples[start..end];
        let rms = (win.iter().map(|s| s * s).sum::<f32>() / win.len() as f32).sqrt();
        let peak = win.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        println!("sec {sec}: rms {rms:.4} peak {peak:.4}");
    }
    let path = "/tmp/live-render.raw";
    let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    std::fs::write(path, bytes).unwrap();
    println!("wrote {path}");
}
