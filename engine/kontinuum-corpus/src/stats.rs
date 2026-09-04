//! Deterministic statistics for the corpus fitters (#23): no randomness,
//! stable ordering everywhere. Every convention a reviewer might question
//! is documented at its definition.

/// Fixed Lloyd iteration count for k-means: bounded work, deterministic by
/// construction (no convergence tolerance to drift across platforms).
pub const KMEANS_ITERS: usize = 24;

pub(crate) fn mean(xs: &[f32]) -> f32 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.iter().sum::<f32>() / xs.len() as f32
}

/// Population standard deviation.
pub(crate) fn std(xs: &[f32]) -> f32 {
    if xs.len() < 2 {
        return 0.0;
    }
    let m = mean(xs);
    (xs.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / xs.len() as f32).sqrt()
}

/// Quantile with linear interpolation on a sorted sample (the "linear"
/// method, as numpy's default); `q` in 0..=1.
pub(crate) fn quantile(sorted: &[f32], q: f32) -> f32 {
    let Some(&first) = sorted.first() else { return 0.0 };
    if sorted.len() == 1 || q <= 0.0 {
        return first;
    }
    if q >= 1.0 {
        return sorted[sorted.len() - 1];
    }
    let h = (sorted.len() - 1) as f32 * q;
    let lo = h.floor() as usize;
    let hi = (lo + 1).min(sorted.len() - 1);
    let frac = h - lo as f32;
    sorted[lo] * (1.0 - frac) + sorted[hi] * frac
}

/// Squared Euclidean distance; the fitters' sole distance metric.
pub(crate) fn sq_dist(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum()
}

/// Total-order lexicographic comparison for f32 slices (NaN never occurs in
/// fitted data, but `total_cmp` keeps the sort total regardless).
pub(crate) fn lex_cmp(a: &[f32], b: &[f32]) -> std::cmp::Ordering {
    a.iter()
        .zip(b)
        .map(|(x, y)| x.total_cmp(y))
        .find(|o| o.is_ne())
        .unwrap_or(std::cmp::Ordering::Equal)
}

pub struct KMeansCluster {
    pub centroid: Vec<f32>,
    /// Indices into the input rows, ascending.
    pub members: Vec<usize>,
}

/// k-means with documented deterministic conventions:
/// - initialization by farthest-first traversal starting at row 0 (ties
///   break to the lowest index); duplicated rows stop the traversal early,
///   so k collapses down to the number of DISTINCT rows when inputs repeat;
/// - exactly [`KMEANS_ITERS`] Lloyd iterations, no early exit;
/// - Euclidean assignment; empty clusters keep their previous centroid;
/// - returned clusters are sorted by centroid lexicographically, so cluster
///   order is a pure function of the input.
pub fn kmeans(rows: &[Vec<f32>], k: usize) -> Vec<KMeansCluster> {
    if rows.is_empty() {
        return Vec::new();
    }
    let k = k.clamp(1, rows.len());
    let seeds = farthest_first(rows, k);
    let mut centroids: Vec<Vec<f32>> = seeds.iter().map(|&i| rows[i].clone()).collect();
    let mut assign = vec![0usize; rows.len()];
    for _ in 0..KMEANS_ITERS {
        for (ri, row) in rows.iter().enumerate() {
            let mut best = 0;
            let mut best_d = f32::INFINITY;
            for (ci, c) in centroids.iter().enumerate() {
                let d = sq_dist(row, c);
                if d < best_d {
                    best_d = d;
                    best = ci;
                }
            }
            assign[ri] = best;
        }
        let dim = rows[0].len();
        let mut sums = vec![vec![0.0f32; dim]; centroids.len()];
        let mut counts = vec![0usize; centroids.len()];
        for (row, &c) in rows.iter().zip(&assign) {
            counts[c] += 1;
            for (s, v) in sums[c].iter_mut().zip(row) {
                *s += v;
            }
        }
        for (ci, c) in centroids.iter_mut().enumerate() {
            if counts[ci] > 0 {
                *c = sums[ci].iter().map(|s| s / counts[ci] as f32).collect();
            }
        }
    }
    let mut clusters: Vec<KMeansCluster> = centroids
        .into_iter()
        .enumerate()
        .map(|(ci, centroid)| KMeansCluster {
            centroid,
            members: (0..rows.len()).filter(|&r| assign[r] == ci).collect(),
        })
        .collect();
    clusters.sort_by(|a, b| lex_cmp(&a.centroid, &b.centroid));
    clusters
}

/// Farthest-first seeding: start at row 0, repeatedly add the row maximizing
/// the minimum distance to the chosen set. Stops early when every remaining
/// row is a duplicate of a chosen seed.
fn farthest_first(rows: &[Vec<f32>], k: usize) -> Vec<usize> {
    let mut chosen = vec![0usize];
    while chosen.len() < k {
        let mut best_i = 0usize;
        let mut best_d = -1.0f32;
        for (i, row) in rows.iter().enumerate() {
            if chosen.contains(&i) {
                continue;
            }
            let d = chosen.iter().map(|&c| sq_dist(row, &rows[c])).fold(f32::INFINITY, f32::min);
            if d > best_d {
                best_d = d;
                best_i = i;
            }
        }
        if best_d <= 0.0 {
            break;
        }
        chosen.push(best_i);
    }
    chosen
}

/// Minimal deterministic PRNG (SplitMix64) for grammar sampling. Same seed
/// → same stream on every platform; no external `rand` dependency.
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        SplitMix64 { state: seed }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in [0, 1).
    pub fn next_f32(&mut self) -> f32 {
        ((self.next_u64() >> 40) as f32) / (1u64 << 24) as f32
    }
}

/// Box–Muller normal draw from two uniforms. `sigma ≈ 0` collapses to `mu`
/// (degenerate distributions must not amplify float noise).
pub(crate) fn normal(rng: &mut SplitMix64, mu: f32, sigma: f32) -> f32 {
    if sigma <= 1e-6 {
        return mu;
    }
    let u1 = 1.0 - rng.next_f32(); // in (0, 1]: keeps ln finite
    let u2 = rng.next_f32();
    mu + sigma * (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mean_std_quantile_known_values() {
        let xs = [1.0f32, 2.0, 3.0, 4.0];
        assert_eq!(mean(&xs), 2.5);
        assert!((std(&xs) - 1.118_034).abs() < 1e-5);
        let sorted = [1.0f32, 2.0, 3.0, 4.0];
        assert_eq!(quantile(&sorted, 0.5), 2.5);
        assert!((quantile(&sorted, 0.1) - 1.3).abs() < 1e-6);
        assert_eq!(quantile(&sorted, 0.0), 1.0);
        assert_eq!(quantile(&sorted, 1.0), 4.0);
        assert_eq!(quantile(&[], 0.5), 0.0);
    }

    #[test]
    fn kmeans_is_deterministic_and_separates_blobs() {
        let rows: Vec<Vec<f32>> = [[0.0, 0.0], [0.1, 0.0], [10.0, 10.0], [10.1, 10.0]]
            .iter()
            .map(|r| r.to_vec())
            .collect();
        let a = kmeans(&rows, 2);
        let b = kmeans(&rows, 2);
        let members: Vec<Vec<usize>> = a.iter().map(|c| c.members.clone()).collect();
        assert_eq!(members, b.iter().map(|c| c.members.clone()).collect::<Vec<_>>());
        assert_eq!(members.len(), 2);
        let all: Vec<usize> = members.concat();
        assert_eq!(all, vec![0, 1, 2, 3], "clusters partition the rows");
        let (first, second) = (&members[0], &members[1]);
        let blob = |c: &[usize]| c == &[0, 1] || c == &[2, 3];
        assert!(blob(first) && blob(second), "two blobs must separate: {members:?}");
    }

    #[test]
    fn kmeans_handles_duplicate_and_degenerate_inputs() {
        let dup: Vec<Vec<f32>> = vec![vec![1.0], vec![1.0], vec![1.0]];
        assert_eq!(kmeans(&dup, 3).len(), 1, "identical rows collapse to one cluster");
        let one = kmeans(&dup, 3);
        assert_eq!(one[0].members, vec![0, 1, 2], "clusters stay a total partition");
        assert!(kmeans(&[], 5).is_empty());
    }

    #[test]
    fn splitmix_stream_is_stable_and_normal_collapses_on_zero_sigma() {
        let mut a = SplitMix64::new(42);
        let mut b = SplitMix64::new(42);
        for _ in 0..16 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        let mut r = SplitMix64::new(1);
        for _ in 0..64 {
            let u = r.next_f32();
            assert!((0.0..1.0).contains(&u));
        }
        assert_eq!(normal(&mut SplitMix64::new(9), 8.0, 0.0), 8.0);
    }
}
