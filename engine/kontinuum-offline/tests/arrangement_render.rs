//! End-to-end acceptance (issue #16): the five seeded 10-minute
//! arrangement plans each render through the real render path — compile,
//! voice pool, mastering chain — to finite, full-length audio. The five
//! renders run in parallel threads to keep the wall clock near one
//! render; each is independent and deterministic.

use kontinuum_compose::{generate_session, GenParams};
use kontinuum_offline::{render_session, DEFAULT_SAMPLE_RATE};

const TEN_MIN_BARS: u32 = 312;
const SEEDS: [u64; 5] = [11, 23, 47, 89, 183];

#[test]
fn five_seeded_ten_minute_plans_render_end_to_end() {
    std::thread::scope(|scope| {
        let handles: Vec<_> = SEEDS
            .map(|seed| {
                scope.spawn(move || {
                    let params = GenParams { seed, target_bars: TEN_MIN_BARS, ..GenParams::default() };
                    let session = generate_session(&params);
                    assert_eq!(session.total_bars(), u64::from(TEN_MIN_BARS));
                    let out = render_session(&session, DEFAULT_SAMPLE_RATE)
                        .unwrap_or_else(|e| panic!("seed {seed}: render failed: {e:?}"));
                    let seconds = out.left.len() as f64 / f64::from(out.sample_rate);
                    assert!(
                        (580.0..=630.0).contains(&seconds),
                        "seed {seed}: rendered {seconds:.1}s, expected ~600s"
                    );
                    assert!(
                        out.left.iter().chain(&out.right).all(|v| v.is_finite()),
                        "seed {seed}: non-finite sample in render"
                    );
                    let peak = out.left.iter().fold(0.0f32, |m, v| m.max(v.abs()));
                    assert!(peak > 0.1, "seed {seed}: render is silent (peak {peak})");
                    println!("seed {seed}: {:.1}s rendered, peak {peak:.3}", seconds);
                })
            })
            .into_iter()
            .collect();
        for h in handles {
            h.join().expect("render thread");
        }
    });
}
