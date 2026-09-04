//! Hand-written bounded Nelder–Mead simplex (issue #75: no optimizer
//! crates, CMA-ES explicitly out of scope). Standard coefficient set —
//! reflection ρ = 1, expansion χ = 2, contraction γ = 0.5, shrink σ = 0.5
//! (Gao–Han style would converge faster; the textbook set is enough for
//! 2–4 parameter problems and keeps the reference behavior documented).
//!
//! Bounds: candidate points are clamped into `[lo, hi]` before every
//! evaluation, so the simplex can lean on a bound but never leave the
//! box. Termination: simplex coordinate spread < `tol_x`, objective
//! spread < `tol_f`, or the iteration/evaluation budget.

/// Tunables. Defaults suit the 2–4 param voice fits; the toy test below
/// pins convergence behavior.
pub struct NelderMead {
    pub max_iters: usize,
    pub max_evals: usize,
    /// Initial simplex offset per axis, as a fraction of (hi − lo).
    pub init_step: f64,
    /// Convergence: largest per-axis vertex spread in the simplex.
    pub tol_x: f64,
    /// Convergence: best-vs-worst objective spread.
    pub tol_f: f64,
}

impl Default for NelderMead {
    fn default() -> Self {
        NelderMead {
            max_iters: 800,
            max_evals: 1600,
            init_step: 0.25,
            // 1e-5 of the parameter range: loose enough to not burn
            // render evaluations on the final asymptotic crawl, tight
            // enough that the simplex must have genuinely settled.
            tol_x: 1e-5,
            tol_f: 1e-12,
        }
    }
}

impl NelderMead {
    /// Second-phase settings for the fitting driver: a fresh small
    /// simplex re-seeded at the coarse winner descends the flat valleys
    /// the coarse phase stops in (verified on the kick fixtures — the
    /// click/drive valley alone keeps ~0.09 of loss unreachable without
    /// this pass).
    pub fn polish() -> Self {
        NelderMead {
            init_step: 0.03,
            tol_x: 1e-9,
            max_iters: 2000,
            max_evals: 3000,
            tol_f: 1e-12,
        }
    }
}

pub struct Optimum {
    pub x: Vec<f64>,
    pub f: f64,
    pub evals: usize,
}

/// Minimizes `f` over the box `lo..hi`, starting from `x0` (clamped in).
pub fn minimize<F>(f: F, x0: &[f64], lo: &[f64], hi: &[f64], cfg: &NelderMead) -> Optimum
where
    F: FnMut(&[f64]) -> f64,
{
    let n = x0.len();
    let mut f = f;
    let clamp = |x: &mut [f64]| {
        for i in 0..n {
            x[i] = x[i].clamp(lo[i], hi[i]);
        }
    };
    // Simplex: x0 plus one offset vertex per axis.
    let mut verts: Vec<Vec<f64>> = Vec::with_capacity(n + 1);
    let mut v0 = x0.to_vec();
    clamp(&mut v0);
    verts.push(v0.clone());
    for i in 0..n {
        let mut v = v0.clone();
        v[i] += cfg.init_step * (hi[i] - lo[i]);
        clamp(&mut v);
        verts.push(v);
    }
    let mut vals: Vec<f64> = verts.iter().map(|v| f(v)).collect();
    let mut evals = n + 1;

    for _ in 0..cfg.max_iters {
        // Sort vertices by objective (best first); verts stays paired with vals.
        let mut order: Vec<usize> = (0..verts.len()).collect();
        order.sort_by(|&a, &b| vals[a].total_cmp(&vals[b]));
        verts = order.iter().map(|&i| verts[i].clone()).collect();
        vals = order.iter().map(|&i| vals[i]).collect();

        let spread_x = (0..n)
            .map(|i| {
                verts.iter().fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(v[i]), b.max(v[i])))
            })
            .map(|(a, b)| b - a)
            .fold(0.0f64, f64::max);
        if spread_x < cfg.tol_x || vals[vals.len() - 1] - vals[0] < cfg.tol_f {
            break;
        }
        if evals >= cfg.max_evals {
            break;
        }

        let worst = verts.len() - 1;
        let mut c = vec![0.0; n];
        for v in &verts[..worst] {
            for (ci, &x) in c.iter_mut().zip(v.iter()) {
                *ci += x / n as f64;
            }
        }

        // Reflect: xr = c + (c − worst).
        let mut xr: Vec<f64> =
            c.iter().zip(verts[worst].iter()).map(|(ci, wi)| ci + (ci - wi)).collect();
        clamp(&mut xr);
        let fr = f(&xr);
        evals += 1;
        if fr < vals[0] {
            // Expand: xe = c + 2·(xr − c).
            let mut xe: Vec<f64> =
                c.iter().zip(xr.iter()).map(|(ci, ri)| ci + 2.0 * (ri - ci)).collect();
            clamp(&mut xe);
            let fe = f(&xe);
            evals += 1;
            let accept = if fe < fr { xe } else { xr };
            let faccept = if fe < fr { fe } else { fr };
            verts[worst] = accept;
            vals[worst] = faccept;
        } else if fr < vals[worst - 1] {
            verts[worst] = xr;
            vals[worst] = fr;
        } else {
            // Contract toward the centroid (outside: past-centroid side,
            // c + ½(c − worst); inside: halfway back, c − ½(c − worst)).
            let outside = fr < vals[worst];
            let dir = if outside { 1.0 } else { -1.0 };
            let mut xc: Vec<f64> = c
                .iter()
                .zip(verts[worst].iter())
                .map(|(ci, wi)| ci + dir * 0.5 * (ci - wi))
                .collect();
            clamp(&mut xc);
            let fc = f(&xc);
            evals += 1;
            if fc < vals[worst] {
                verts[worst] = xc;
                vals[worst] = fc;
            } else {
                // Shrink everything toward the best vertex.
                for j in 1..verts.len() {
                    for i in 0..n {
                        verts[j][i] = verts[0][i] + 0.5 * (verts[j][i] - verts[0][i]);
                    }
                    clamp(&mut verts[j]);
                    vals[j] = f(&verts[j]);
                }
                evals += verts.len() - 1;
            }
        }
    }

    let mut best = 0usize;
    for i in 1..vals.len() {
        if vals[i] < vals[best] {
            best = i;
        }
    }
    Optimum { x: verts[best].clone(), f: vals[best], evals }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Axis-scaled quadratic with a known minimum at `c`.
    fn quad(x: &[f64], c: &[f64], k: f64) -> f64 {
        x.iter().zip(c.iter()).map(|(&xi, &ci)| k * (xi - ci) * (xi - ci)).sum()
    }

    #[test]
    fn nm_converges_on_quadratic_toy() {
        let c = [0.31, 0.72, 0.05, 0.58];
        let f = |x: &[f64]| quad(x, &c, 7.0);
        let cfg = NelderMead::default();
        let r = minimize(f, &[0.8, 0.2, 0.5, 0.9], &[0.0; 4], &[1.0; 4], &cfg);
        for i in 0..4 {
            assert!(
                (r.x[i] - c[i]).abs() < 1e-3,
                "axis {i}: {} vs {}",
                r.x[i],
                c[i]
            );
        }
        assert!(r.f < 1e-5, "final objective {}", r.f);
    }

    #[test]
    fn nm_respects_bounds() {
        let f = |x: &[f64]| (x[0] - 5.0) * (x[0] - 5.0);
        let cfg = NelderMead::default();
        let r = minimize(f, &[0.5], &[0.0], &[1.0], &cfg);
        assert!((0.0..=1.0).contains(&r.x[0]));
        assert!((r.x[0] - 1.0).abs() < 1e-3, "must pin to the upper bound: {}", r.x[0]);
    }
}
