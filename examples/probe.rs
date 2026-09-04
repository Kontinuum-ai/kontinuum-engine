fn main() {
    let sr = 48_000u32;
    let n = sr as usize;
    let freq = 750.0f32;
    let l: Vec<f32> = (0..n).map(|i| 0.6 * (std::f32::consts::TAU * freq * i as f32 / sr as f32).sin()).collect();
    let r = l.clone();
    let m = kontinuum_analysis::Metrics::analyze(&l, &r, sr);
    println!("cv {} density {} dyn {} centroid {}", m.hit_cv, m.transients_per_sec, m.short_term_dyn_db, m.centroid_hz);
}
