//! Connectome-informed graph encoder diagnostics plus recurrent-memory / temporal statistics (pure).
use hcrnn::{topology, Connectome};
use serde::Serialize;

// ---- graph encoder ----------------------------------------------------------------------------------

#[derive(Serialize, Clone, Debug)]
pub struct NodeCrit {
    pub pagerank: f64,
    pub betweenness: f64,
    pub harmonic_energy: f64,
    /// 0..1 composite (0.4 pagerank + 0.3 betweenness + 0.3 low-frequency harmonic energy, each max-normalised)
    pub criticality: f64,
}

fn norm_max(v: &mut [f64]) {
    let m = v.iter().cloned().fold(0.0, f64::max);
    if m > 0.0 {
        v.iter_mut().for_each(|x| *x /= m);
    }
}

pub fn pagerank(w: &[f64], n: usize) -> Vec<f64> {
    let s: Vec<f64> = (0..n).map(|i| (0..n).map(|j| w[i * n + j]).sum()).collect();
    let mut p = vec![1.0 / n.max(1) as f64; n];
    for _ in 0..100 {
        let mut q = vec![0.15 / n.max(1) as f64; n];
        let mut dangling = 0.0;
        for i in 0..n {
            if s[i] <= 0.0 {
                dangling += p[i];
                continue;
            }
            for j in 0..n {
                if w[i * n + j] > 0.0 {
                    q[j] += 0.85 * p[i] * w[i * n + j] / s[i];
                }
            }
        }
        for x in q.iter_mut() {
            *x += 0.85 * dangling / n as f64;
        }
        p = q;
    }
    p
}

/// Brandes betweenness on the weighted graph with edge cost 1 / w (strong flow = short distance).
pub fn betweenness(w: &[f64], n: usize) -> Vec<f64> {
    let mut cb = vec![0.0; n];
    for s in 0..n {
        let mut dist = vec![f64::INFINITY; n];
        let mut sigma = vec![0.0; n];
        let mut pred: Vec<Vec<usize>> = vec![vec![]; n];
        let mut done = vec![false; n];
        let mut order = Vec::new();
        dist[s] = 0.0;
        sigma[s] = 1.0;
        for _ in 0..n {
            let mut u = usize::MAX;
            for v in 0..n {
                if !done[v] && dist[v].is_finite() && (u == usize::MAX || dist[v] < dist[u]) {
                    u = v;
                }
            }
            if u == usize::MAX {
                break;
            }
            done[u] = true;
            order.push(u);
            for v in 0..n {
                let wt = w[u * n + v];
                if wt <= 0.0 || done[v] {
                    continue;
                }
                let nd = dist[u] + 1.0 / wt;
                if nd < dist[v] - 1e-12 {
                    dist[v] = nd;
                    sigma[v] = sigma[u];
                    pred[v] = vec![u];
                } else if (nd - dist[v]).abs() <= 1e-12 {
                    sigma[v] += sigma[u];
                    pred[v].push(u);
                }
            }
        }
        let mut delta = vec![0.0; n];
        for &v in order.iter().rev() {
            for &u in &pred[v] {
                delta[u] += sigma[u] / sigma[v] * (1.0 + delta[v]);
            }
            if v != s {
                cb[v] += delta[v];
            }
        }
    }
    cb.iter().map(|x| x / 2.0).collect()
}

pub fn criticality(conn: &Connectome) -> Vec<NodeCrit> {
    let n = conn.n();
    let w = conn.weights();
    let mut pr = pagerank(w, n);
    let mut bt = betweenness(w, n);
    let h = topology::harmonics(conn);
    let k = (n - 1).min(5);
    let mut he = vec![0.0; n];
    for i in 0..n {
        for j in 1..=k {
            he[i] += h.vectors[(i, j)].powi(2) / h.values[j].max(1e-2);
        }
    }
    norm_max(&mut pr);
    norm_max(&mut bt);
    norm_max(&mut he);
    (0..n)
        .map(|i| NodeCrit { pagerank: pr[i], betweenness: bt[i], harmonic_energy: he[i], criticality: 0.4 * pr[i] + 0.3 * bt[i] + 0.3 * he[i] })
        .collect()
}

#[derive(Serialize, Clone, Debug)]
pub struct RiskPath {
    pub nodes: Vec<usize>,
    /// product of per-hop propagation probabilities (w_ij / strength_i)
    pub probability: f64,
}

/// Most probable propagation paths from `src` (best path to each other node, top `k`).
pub fn risk_paths(w: &[f64], n: usize, src: usize, k: usize) -> Vec<RiskPath> {
    let s: Vec<f64> = (0..n).map(|i| (0..n).map(|j| w[i * n + j]).sum()).collect();
    let mut dist = vec![f64::INFINITY; n];
    let mut prev = vec![usize::MAX; n];
    let mut done = vec![false; n];
    dist[src] = 0.0;
    for _ in 0..n {
        let mut u = usize::MAX;
        for v in 0..n {
            if !done[v] && dist[v].is_finite() && (u == usize::MAX || dist[v] < dist[u]) {
                u = v;
            }
        }
        if u == usize::MAX {
            break;
        }
        done[u] = true;
        for v in 0..n {
            if w[u * n + v] > 0.0 && !done[v] {
                let p = (w[u * n + v] / s[u]).clamp(1e-9, 0.999);
                let nd = dist[u] - p.ln();
                if nd < dist[v] {
                    dist[v] = nd;
                    prev[v] = u;
                }
            }
        }
    }
    let mut out: Vec<RiskPath> = (0..n)
        .filter(|&v| v != src && dist[v].is_finite())
        .map(|v| {
            let mut path = vec![v];
            let mut c = v;
            while prev[c] != usize::MAX {
                c = prev[c];
                path.push(c);
            }
            path.reverse();
            RiskPath { nodes: path, probability: (-dist[v]).exp() }
        })
        .collect();
    out.sort_by(|a, b| b.probability.partial_cmp(&a.probability).unwrap_or(std::cmp::Ordering::Equal));
    out.truncate(k);
    out
}

// ---- memory / temporal statistics ------------------------------------------------------------------

pub fn mean_sd(y: &[f64]) -> (f64, f64) {
    if y.is_empty() {
        return (0.0, 0.0);
    }
    let m = y.iter().sum::<f64>() / y.len() as f64;
    let v = y.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (y.len().max(2) - 1) as f64;
    (m, v.sqrt())
}

/// OLS slope per step and its t statistic.
pub fn slope_t(y: &[f64]) -> (f64, f64) {
    let n = y.len();
    if n < 4 {
        return (0.0, 0.0);
    }
    let xm = (n as f64 - 1.0) / 2.0;
    let ym = y.iter().sum::<f64>() / n as f64;
    let sxx: f64 = (0..n).map(|i| (i as f64 - xm).powi(2)).sum();
    let sxy: f64 = (0..n).map(|i| (i as f64 - xm) * (y[i] - ym)).sum();
    let b = sxy / sxx;
    let sse: f64 = (0..n).map(|i| (y[i] - ym - b * (i as f64 - xm)).powi(2)).sum();
    let se = (sse / (n as f64 - 2.0) / sxx).sqrt();
    (b, if se > 0.0 { b / se } else { 0.0 })
}

pub fn acf(y: &[f64], lag: usize) -> f64 {
    let n = y.len();
    if lag >= n || n < 4 {
        return 0.0;
    }
    let (m, _) = mean_sd(y);
    let den: f64 = y.iter().map(|x| (x - m).powi(2)).sum();
    if den <= 0.0 {
        return 0.0;
    }
    (lag..n).map(|i| (y[i] - m) * (y[i - lag] - m)).sum::<f64>() / den
}

/// Dominant period (steps) by DFT power, 2..=n/2, with its share of total non-DC power.
pub fn dominant_period(y: &[f64]) -> Option<(f64, f64)> {
    let n = y.len();
    if n < 8 {
        return None;
    }
    let (b, _) = slope_t(y);
    let xm = (n as f64 - 1.0) / 2.0;
    let (m0, _) = mean_sd(y);
    let detr: Vec<f64> = y.iter().enumerate().map(|(i, v)| v - m0 - b * (i as f64 - xm)).collect();
    let y = &detr[..];
    let m = 0.0;
    let mut best = (0usize, 0.0f64);
    let mut tot = 0.0;
    for k in 1..=n / 2 {
        let (mut re, mut im) = (0.0, 0.0);
        for (t, v) in y.iter().enumerate() {
            let a = 2.0 * std::f64::consts::PI * k as f64 * t as f64 / n as f64;
            re += (v - m) * a.cos();
            im -= (v - m) * a.sin();
        }
        let p = re * re + im * im;
        tot += p;
        if p > best.1 {
            best = (k, p);
        }
    }
    if tot <= 0.0 || best.0 == 0 {
        return None;
    }
    Some((n as f64 / best.0 as f64, best.1 / tot))
}

pub fn cv(y: &[f64]) -> f64 {
    let (m, s) = mean_sd(y);
    if m.abs() < 1e-9 { 0.0 } else { s / m.abs() }
}

/// Seasonal index per phase for `period` (mean of phase / overall mean); None if < 2 full cycles.
pub fn seasonal_index(y: &[f64], period: usize) -> Option<Vec<f64>> {
    if period < 2 || y.len() < 2 * period {
        return None;
    }
    let (m, _) = mean_sd(y);
    if m.abs() < 1e-9 {
        return None;
    }
    let mut sums = vec![0.0; period];
    let mut cnt = vec![0usize; period];
    for (i, v) in y.iter().enumerate() {
        sums[i % period] += v;
        cnt[i % period] += 1;
    }
    Some((0..period).map(|p| sums[p] / cnt[p] as f64 / m).collect())
}

#[derive(Serialize, Debug, Clone, Default)]
pub struct SeriesProfile {
    pub weeks: usize,
    pub level: f64,
    pub trend_per_week: f64,
    pub trend_t: f64,
    pub trend_label: &'static str,
    pub cv: f64,
    pub recent_vs_prior: Option<f64>,
    pub dominant_period_weeks: Option<f64>,
    pub periodic_share: Option<f64>,
    pub best_acf_lag: Option<usize>,
    pub best_acf: Option<f64>,
    pub seasonal_amplitude: Option<f64>,
}

pub fn profile(y: &[f64]) -> SeriesProfile {
    let n = y.len();
    let (m, _) = mean_sd(y);
    let (b, t) = slope_t(y);
    let label = if t > 2.5 { "rising" } else if t < -2.5 { "falling" } else { "flat" };
    let w = 13.min(n / 2);
    let rvp = if w >= 4 {
        let rec = y[n - w..].iter().sum::<f64>() / w as f64;
        let pri = y[n - 2 * w..n - w].iter().sum::<f64>() / w as f64;
        if pri.abs() > 1e-9 { Some(rec / pri) } else { None }
    } else {
        None
    };
    let dp = dominant_period(y);
    let (mut bl, mut ba) = (None, None);
    for lag in 1..=(n / 3).min(26) {
        let a = acf(y, lag);
        if lag > 1 && a > ba.unwrap_or(0.0) {
            ba = Some(a);
            bl = Some(lag);
        }
    }
    let seas = [52usize, 26, 13, 4].iter().find_map(|p| seasonal_index(y, *p)).map(|idx| {
        let mx = idx.iter().cloned().fold(f64::MIN, f64::max);
        let mn = idx.iter().cloned().fold(f64::MAX, f64::min);
        (mx - mn) / 2.0
    });
    SeriesProfile {
        weeks: n,
        level: m,
        trend_per_week: b,
        trend_t: t,
        trend_label: label,
        cv: cv(y),
        recent_vs_prior: rvp,
        dominant_period_weeks: dp.map(|d| d.0),
        periodic_share: dp.map(|d| d.1),
        best_acf_lag: bl,
        best_acf: ba,
        seasonal_amplitude: seas,
    }
}

/// Standard normal CDF.
pub fn phi(x: f64) -> f64 {
    0.5 * (1.0 + erf(x / std::f64::consts::SQRT_2))
}
fn erf(x: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.3275911 * x.abs());
    let y = 1.0 - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t + 0.254829592) * t * (-x * x).exp();
    if x >= 0.0 { y } else { -y }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supply_chain::cognitive::graph::{test_graph, FlowMix};

    #[test]
    fn warehouse_hub_is_most_critical_and_betweenness_is_exact_on_a_path() {
        let g = test_graph();
        let c = g.connectome(&FlowMix::default()).unwrap();
        let cr = criticality(&c);
        let top = (0..7).max_by(|a, b| cr[*a].criticality.partial_cmp(&cr[*b].criticality).unwrap()).unwrap();
        assert_eq!(g.nodes[top].code, "W1");
        // path 0-1-2: middle node carries the single shortest path between the ends
        let w = vec![0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0];
        let b = betweenness(&w, 3);
        assert!((b[1] - 1.0).abs() < 1e-9 && b[0] == 0.0 && b[2] == 0.0);
    }

    #[test]
    fn risk_paths_follow_strongest_links() {
        let g = test_graph();
        let c = g.connectome(&FlowMix::default()).unwrap();
        let p = risk_paths(c.weights(), 7, 0, 3);
        assert!(!p.is_empty());
        assert!(p[0].probability > 0.0 && p[0].probability <= 1.0);
        assert_eq!(p[0].nodes[0], 0);
    }

    #[test]
    fn profile_detects_trend_and_period() {
        let y: Vec<f64> = (0..104).map(|t| 50.0 + 0.4 * t as f64 + 10.0 * (2.0 * std::f64::consts::PI * t as f64 / 13.0).sin()).collect();
        let p = profile(&y);
        assert_eq!(p.trend_label, "rising");
        let per = p.dominant_period_weeks.unwrap();
        assert!((per - 13.0).abs() < 1.5, "{per}");
        assert!(p.seasonal_amplitude.unwrap() > 0.05);
        assert!((phi(0.0) - 0.5).abs() < 1e-6 && (phi(1.96) - 0.975).abs() < 1e-3);
    }
}
