//! Supplier-network resilience maths (pure functions, unit-tested).
//!
//! The network is a hypergraph: vertices are suppliers, and every product is a hyperedge joining the
//! suppliers qualified to supply it. Its spectrum comes from the normalised hypergraph Laplacian of
//! Zhou, Huang & Scholkopf (2006), the same operator used by `spectral_hypergraph` / SHPINN:
//!   Delta = I - Dv^{-1/2} H W De^{-1} H^T Dv^{-1/2}
//! * lambda_2 (algebraic connectivity; eigenvalues lie in [0, 1], thresholds 0.10 / 0.30 below) measures how strongly suppliers are interlinked through shared
//!   products (Cheeger: lambda_2/2 <= conductance <= sqrt(2 lambda_2)).
//! * connected components = groups of suppliers that share no product with anyone else.
//! * the Fiedler vector bisects the largest component into its two weakest-linked halves.
//! * the heat kernel exp(-t Delta) diffuses a shock from failed suppliers (the SHPINN heat equation
//!   du/dt = -alpha Delta u) to show which suppliers are structurally closest to the failure.
use nalgebra::{DMatrix, DVector, SymmetricEigen};
use serde::Serialize;

pub const MAX_DENSE_VERTICES: usize = 1500;

#[derive(Clone, Debug)]
pub struct Hypergraph {
    pub n: usize,
    /// (member vertices, weight)
    pub edges: Vec<(Vec<usize>, f64)>,
}

/// Connected components over hyperedges (union-find). Returns (component id per vertex, count).
pub fn components(h: &Hypergraph) -> (Vec<usize>, usize) {
    let mut parent: Vec<usize> = (0..h.n).collect();
    fn find(p: &mut [usize], x: usize) -> usize {
        let mut r = x;
        while p[r] != r {
            r = p[r];
        }
        let mut c = x;
        while p[c] != r {
            let nx = p[c];
            p[c] = r;
            c = nx;
        }
        r
    }
    for (e, _) in &h.edges {
        for w in e.windows(2) {
            let (a, b) = (find(&mut parent, w[0]), find(&mut parent, w[1]));
            if a != b {
                parent[a] = b;
            }
        }
    }
    let mut ids = std::collections::HashMap::new();
    let comp: Vec<usize> = (0..h.n)
        .map(|v| {
            let r = find(&mut parent, v);
            let next = ids.len();
            *ids.entry(r).or_insert(next)
        })
        .collect();
    let count = ids.len();
    (comp, count)
}

/// Dense normalised hypergraph Laplacian. Vertices in no hyperedge get Delta_vv = 1 (isolated).
pub fn laplacian(h: &Hypergraph) -> DMatrix<f64> {
    let n = h.n;
    let mut dv = vec![0.0; n];
    for (e, w) in &h.edges {
        for &v in e {
            dv[v] += w;
        }
    }
    let mut theta = DMatrix::<f64>::zeros(n, n);
    for (e, w) in &h.edges {
        let k = e.len() as f64;
        if k == 0.0 {
            continue;
        }
        for &a in e {
            for &b in e {
                if dv[a] > 0.0 && dv[b] > 0.0 {
                    theta[(a, b)] += w / k / (dv[a] * dv[b]).sqrt();
                }
            }
        }
    }
    DMatrix::<f64>::identity(n, n) - theta
}

#[derive(Debug)]
pub struct Eigen {
    pub values: Vec<f64>,
    pub vectors: DMatrix<f64>,
}

pub fn eigen(l: DMatrix<f64>) -> Eigen {
    let e = SymmetricEigen::new(l);
    let mut idx: Vec<usize> = (0..e.eigenvalues.len()).collect();
    idx.sort_by(|a, b| e.eigenvalues[*a].partial_cmp(&e.eigenvalues[*b]).unwrap_or(std::cmp::Ordering::Equal));
    let values = idx.iter().map(|&i| e.eigenvalues[i].max(0.0)).collect();
    let mut vectors = DMatrix::zeros(e.eigenvectors.nrows(), idx.len());
    for (j, &i) in idx.iter().enumerate() {
        vectors.set_column(j, &e.eigenvectors.column(i));
    }
    Eigen { values, vectors }
}

#[derive(Debug, Clone, Serialize)]
pub struct Spectral {
    pub suppliers: usize,
    pub products_multi_sourced: usize,
    pub components: usize,
    pub component_sizes: Vec<usize>,
    /// second-smallest Laplacian eigenvalue of the whole network (0 when disconnected)
    pub lambda2: f64,
    /// algebraic connectivity inside the largest component
    pub lambda2_main_component: f64,
    pub cheeger_lower_bound: f64,
    pub cheeger_upper_bound: f64,
    pub fragmentation_risk: &'static str,
    /// side (0/1) of the Fiedler bisection per vertex, only meaningful for the largest component
    #[serde(skip)]
    pub fiedler_side: Vec<Option<u8>>,
    #[serde(skip)]
    pub component_of: Vec<usize>,
}

pub fn risk_label(components: usize, lambda2_main: f64) -> &'static str {
    if components > 1 || lambda2_main < 0.10 {
        "high"
    } else if lambda2_main < 0.30 {
        "medium"
    } else {
        "low"
    }
}

/// Full spectral report. `None` when the network is too large for dense analysis.
pub fn analyse(h: &Hypergraph) -> Option<Spectral> {
    if h.n > MAX_DENSE_VERTICES {
        return None;
    }
    let (comp, count) = components(h);
    let mut sizes = vec![0usize; count];
    for &c in &comp {
        sizes[c] += 1;
    }
    let multi = h.edges.iter().filter(|(e, _)| e.len() > 1).count();
    if h.n == 0 {
        return Some(Spectral { suppliers: 0, products_multi_sourced: 0, components: 0, component_sizes: vec![], lambda2: 0.0, lambda2_main_component: 0.0, cheeger_lower_bound: 0.0, cheeger_upper_bound: 0.0, fragmentation_risk: "high", fiedler_side: vec![], component_of: vec![] });
    }
    let full = eigen(laplacian(h));
    let lambda2 = if count > 1 || h.n < 2 { 0.0 } else { full.values.get(1).copied().unwrap_or(0.0) };

    // largest component: its own Laplacian and Fiedler vector
    let main = (0..count).max_by_key(|c| sizes[*c]).unwrap_or(0);
    let members: Vec<usize> = (0..h.n).filter(|v| comp[*v] == main).collect();
    let mut local = vec![usize::MAX; h.n];
    for (i, &v) in members.iter().enumerate() {
        local[v] = i;
    }
    let sub = Hypergraph {
        n: members.len(),
        edges: h
            .edges
            .iter()
            .filter(|(e, _)| e.iter().all(|v| comp[*v] == main))
            .map(|(e, w)| (e.iter().map(|v| local[*v]).collect(), *w))
            .collect(),
    };
    let mut side = vec![None; h.n];
    let mut l2_main = 0.0;
    if sub.n >= 2 {
        let es = eigen(laplacian(&sub));
        l2_main = es.values[1];
        let f: DVector<f64> = es.vectors.column(1).into_owned();
        for (i, &v) in members.iter().enumerate() {
            side[v] = Some(if f[i] >= 0.0 { 1 } else { 0 });
        }
    }
    Some(Spectral {
        suppliers: h.n,
        products_multi_sourced: multi,
        components: count,
        component_sizes: sizes,
        lambda2,
        lambda2_main_component: l2_main,
        cheeger_lower_bound: lambda2 / 2.0,
        cheeger_upper_bound: (2.0 * lambda2).sqrt(),
        fragmentation_risk: risk_label(count, l2_main),
        fiedler_side: side,
        component_of: comp,
    })
}

/// Heat-kernel diffusion u(t) = exp(-t Delta) u0 of a unit shock placed on `sources`.
pub fn heat_diffusion(h: &Hypergraph, sources: &[usize], t: f64) -> Vec<f64> {
    let e = eigen(laplacian(h));
    let mut u0 = DVector::<f64>::zeros(h.n);
    for &s in sources {
        u0[s] = 1.0;
    }
    let coeff = e.vectors.transpose() * u0;
    let scaled = DVector::from_iterator(h.n, (0..h.n).map(|k| (-t * e.values[k]).exp() * coeff[k]));
    (e.vectors * scaled).iter().copied().collect()
}

/// Herfindahl-Hirschman index of shares that sum to ~1 (1 = sole source).
pub fn hhi(shares: &[f64]) -> f64 {
    let total: f64 = shares.iter().sum();
    if total <= 0.0 {
        return 0.0;
    }
    shares.iter().map(|s| (s / total).powi(2)).sum()
}

// ---- null model ------------------------------------------------------------------------------------------

struct XorShift(u64);
impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Degree-preserving rewiring of the product-supplier incidence (bipartite double-edge swaps):
/// every product keeps its number of suppliers and every supplier keeps its number of products.
pub fn rewire(h: &Hypergraph, seed: u64) -> Hypergraph {
    let mut inc: Vec<(usize, usize)> = h.edges.iter().enumerate().flat_map(|(e, (vs, _))| vs.iter().map(move |v| (e, *v))).collect();
    let mut present: std::collections::HashSet<(usize, usize)> = inc.iter().copied().collect();
    let mut rng = XorShift(seed.max(1));
    if inc.len() >= 2 {
        for _ in 0..inc.len() * 20 {
            let (i, j) = (rng.below(inc.len()), rng.below(inc.len()));
            let ((e1, v1), (e2, v2)) = (inc[i], inc[j]);
            if e1 == e2 || v1 == v2 || present.contains(&(e1, v2)) || present.contains(&(e2, v1)) {
                continue;
            }
            present.remove(&(e1, v1));
            present.remove(&(e2, v2));
            present.insert((e1, v2));
            present.insert((e2, v1));
            inc[i] = (e1, v2);
            inc[j] = (e2, v1);
        }
    }
    let mut edges: Vec<(Vec<usize>, f64)> = h.edges.iter().map(|(_, w)| (Vec::new(), *w)).collect();
    for (e, v) in inc {
        edges[e].0.push(v);
    }
    Hypergraph { n: h.n, edges }
}

#[derive(Debug, Clone, Serialize)]
pub struct NullComparison {
    pub samples: usize,
    pub observed_lambda2_main: f64,
    pub null_lambda2_mean: f64,
    pub null_lambda2_sd: f64,
    /// (observed - null mean) / null sd; negative = less connected than random sourcing would be
    pub z: Option<f64>,
    pub observed_components: usize,
    pub null_components_mean: f64,
    pub reading: &'static str,
}

/// Compare the observed network with degree-preserving random sourcing (the same null-model logic
/// the connectome RNN uses for wiring). Only meaningful when some products are multi-sourced and the
/// incidence is not already complete.
pub fn null_comparison(h: &Hypergraph, samples: usize, seed: u64) -> Option<NullComparison> {
    if h.n > 300 || h.n < 3 || !h.edges.iter().any(|(e, _)| e.len() > 1) {
        return None;
    }
    let obs = analyse(h)?;
    let (mut l2, mut comps) = (Vec::new(), Vec::new());
    for k in 0..samples {
        let r = analyse(&rewire(h, seed.wrapping_add(k as u64 * 7919 + 1)))?;
        l2.push(r.lambda2_main_component);
        comps.push(r.components as f64);
    }
    let mean = l2.iter().sum::<f64>() / samples as f64;
    let sd = (l2.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / samples.max(1) as f64).sqrt();
    let z = (sd > 1e-9).then(|| (obs.lambda2_main_component - mean) / sd);
    let reading = match z {
        Some(z) if z < -2.0 => "much less connected than random sourcing: clustered, bottlenecked supplier base",
        Some(z) if z > 2.0 => "much better connected than random sourcing: deliberately overlapping supplier base",
        Some(_) => "connectivity consistent with random sourcing of the same shape",
        None => "null networks are identical (incidence leaves no room to rewire)",
    };
    Some(NullComparison {
        samples,
        observed_lambda2_main: obs.lambda2_main_component,
        null_lambda2_mean: mean,
        null_lambda2_sd: sd,
        z,
        observed_components: obs.components,
        null_components_mean: comps.iter().sum::<f64>() / samples as f64,
        reading,
    })
}

// ---- disruption simulation ------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ProductFacts {
    pub daily_demand: f64,
    pub available_stock: f64,
    pub unit_value: f64,
    /// (supplier index, baseline share of volume) for suppliers that actually supplied the product
    pub shares: Vec<(usize, f64)>,
    /// every supplier qualified for the product (observed or approved), with lead time in days
    pub qualified: Vec<(usize, f64)>,
}

#[derive(Debug, Clone, Copy)]
pub struct SimParams {
    pub horizon_days: f64,
    /// extra volume the remaining *active* suppliers can absorb, as a fraction of their baseline
    pub max_uplift: f64,
    /// volume (fraction of total demand) each qualified-but-unused supplier can take
    pub standby_share: f64,
    /// days to qualify a brand-new source when none remains
    pub qualification_days: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Impact {
    pub lost_share: f64,
    pub sole_source: bool,
    pub covered_share: f64,
    pub uncovered_share: f64,
    /// uplift the remaining active suppliers must deliver (fraction of their baseline)
    pub required_uplift: Option<f64>,
    pub days_until_stockout_on_lost_volume: Option<f64>,
    pub alternative_lead_time_days: Option<f64>,
    pub shortfall_units: f64,
    pub shortfall_value: f64,
    pub recovery_days: f64,
}

/// Remove `removed` suppliers and measure the consequences for one product (None if unaffected).
pub fn simulate(f: &ProductFacts, removed: &[usize], p: SimParams) -> Option<Impact> {
    let is_removed = |s: usize| removed.contains(&s);
    let total_share: f64 = f.shares.iter().map(|(_, s)| s).sum();
    let norm = |s: f64| if total_share > 0.0 { s / total_share } else { 0.0 };
    let lost: f64 = f.shares.iter().filter(|(s, _)| is_removed(*s)).map(|(_, x)| norm(*x)).sum();
    let lost_qualified_only = f.shares.is_empty() && f.qualified.iter().any(|(s, _)| is_removed(*s));
    let lost = if lost_qualified_only { 1.0 / f.qualified.len().max(1) as f64 } else { lost };
    if lost <= 0.0 {
        return None;
    }
    let remaining_q: Vec<&(usize, f64)> = f.qualified.iter().filter(|(s, _)| !is_removed(*s)).collect();
    let active_remaining: f64 = f.shares.iter().filter(|(s, _)| !is_removed(*s)).map(|(_, x)| norm(*x)).sum();
    let standby = remaining_q.iter().filter(|(s, _)| !f.shares.iter().any(|(a, _)| a == s)).count() as f64;
    let capacity = p.max_uplift * active_remaining + p.standby_share * standby;
    let covered = lost.min(capacity);
    let uncovered = lost - covered;
    let alt_lt = remaining_q.iter().map(|(_, l)| *l).fold(None, |a: Option<f64>, l| Some(a.map_or(l, |x| x.min(l))));
    let lost_rate = f.daily_demand * lost;
    let t_stock = if lost_rate > 0.0 { Some(f.available_stock / lost_rate) } else { None };
    let ts = t_stock.unwrap_or(f64::INFINITY);
    let h = p.horizon_days;
    let short_days_uncovered = (h - ts).max(0.0);
    let alt = alt_lt.unwrap_or(p.qualification_days);
    let short_days_covered = (h.min(alt) - ts).max(0.0);
    let shortfall = f.daily_demand * (uncovered * short_days_uncovered + covered * short_days_covered);
    Some(Impact {
        lost_share: lost,
        sole_source: remaining_q.is_empty(),
        covered_share: covered,
        uncovered_share: uncovered,
        required_uplift: (active_remaining > 0.0).then(|| lost / active_remaining),
        days_until_stockout_on_lost_volume: t_stock,
        alternative_lead_time_days: alt_lt,
        shortfall_units: shortfall,
        shortfall_value: shortfall * f.unit_value,
        recovery_days: if uncovered > 0.0 { p.qualification_days.max(alt) } else { alt },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hg(n: usize, edges: &[&[usize]]) -> Hypergraph {
        Hypergraph { n, edges: edges.iter().map(|e| (e.to_vec(), 1.0)).collect() }
    }

    #[test]
    fn complete_graph_has_known_spectrum() {
        // 2-edges forming K4. For 2-uniform hyperedges Zhou's Laplacian is half the graph normalised
        // Laplacian, so the eigenvalues are 0 and n/(2(n-1)) = 2/3 (x3)
        let h = hg(4, &[&[0, 1], &[0, 2], &[0, 3], &[1, 2], &[1, 3], &[2, 3]]);
        let s = analyse(&h).unwrap();
        assert_eq!(s.components, 1);
        assert!((s.lambda2 - 2.0 / 3.0).abs() < 1e-9, "{}", s.lambda2);
        assert_eq!(s.fragmentation_risk, "low");
    }

    #[test]
    fn disconnected_network_is_flagged() {
        // sole-sourced products only: singleton hyperedges -> every supplier an island
        let h = hg(3, &[&[0], &[1], &[2]]);
        let s = analyse(&h).unwrap();
        assert_eq!(s.components, 3);
        assert_eq!(s.lambda2, 0.0);
        assert_eq!(s.fragmentation_risk, "high");
    }

    #[test]
    fn path_graph_matches_closed_form_and_fiedler_bisects() {
        // path 0-1-2-3-4-5: weakly connected, Fiedler vector splits it in the middle
        let h = hg(6, &[&[0, 1], &[1, 2], &[2, 3], &[3, 4], &[4, 5]]);
        let s = analyse(&h).unwrap();
        assert_eq!(s.components, 1);
        assert!(s.lambda2 > 0.0 && s.lambda2 < 0.3, "{}", s.lambda2);
        let sides: Vec<u8> = (0..6).map(|v| s.fiedler_side[v].unwrap()).collect();
        assert_eq!(sides[0], sides[1]);
        assert_eq!(sides[1], sides[2]);
        assert_eq!(sides[3], sides[5]);
        assert_ne!(sides[0], sides[5]);
    }

    #[test]
    fn heat_kernel_decays_with_distance() {
        let h = hg(5, &[&[0, 1], &[1, 2], &[2, 3], &[3, 4]]);
        let u = heat_diffusion(&h, &[0], 1.0);
        assert!(u[0] > u[1] && u[1] > u[2] && u[2] > u[3].abs() - 1e-12, "{u:?}");
        // t = 0 returns the initial shock
        let u0 = heat_diffusion(&h, &[0], 0.0);
        assert!((u0[0] - 1.0).abs() < 1e-9 && u0[1].abs() < 1e-9);
    }

    #[test]
    fn rewiring_preserves_degrees_and_null_comparison_flags_bottlenecks() {
        // two tight 4-supplier clusters bridged by one product: much less connected than a random rewiring
        let mut edges: Vec<(Vec<usize>, f64)> = Vec::new();
        for c in [0usize, 4] {
            for a in 0..4 {
                for b in (a + 1)..4 {
                    edges.push((vec![c + a, c + b], 1.0));
                }
            }
        }
        edges.push((vec![3, 4], 1.0));
        let h = Hypergraph { n: 8, edges };
        let r = rewire(&h, 42);
        let deg = |g: &Hypergraph| {
            let mut d = vec![0usize; g.n];
            for (e, _) in &g.edges { for v in e { d[*v] += 1; } }
            d
        };
        assert_eq!(deg(&h), deg(&r));
        assert_eq!(h.edges.iter().map(|e| e.0.len()).collect::<Vec<_>>(), r.edges.iter().map(|e| e.0.len()).collect::<Vec<_>>());
        let nc = null_comparison(&h, 40, 7).unwrap();
        assert!(nc.z.unwrap() < -1.0, "{nc:?}");
        // complete overlap or all-singleton networks cannot be rewired meaningfully
        assert!(null_comparison(&Hypergraph { n: 3, edges: vec![(vec![0], 1.0), (vec![1], 1.0), (vec![2], 1.0)] }, 10, 1).is_none());
    }

    #[test]
    fn hhi_values() {
        assert!((hhi(&[1.0]) - 1.0).abs() < 1e-12);
        assert!((hhi(&[0.5, 0.5]) - 0.5).abs() < 1e-12);
        assert!((hhi(&[10.0, 10.0, 10.0, 10.0]) - 0.25).abs() < 1e-12);
    }

    fn params() -> SimParams {
        SimParams { horizon_days: 90.0, max_uplift: 0.5, standby_share: 0.25, qualification_days: 120.0 }
    }

    #[test]
    fn sole_source_loss_is_total_after_stock_runs_out() {
        let f = ProductFacts { daily_demand: 10.0, available_stock: 200.0, unit_value: 5.0, shares: vec![(0, 1.0)], qualified: vec![(0, 30.0)] };
        let i = simulate(&f, &[0], params()).unwrap();
        assert!(i.sole_source && (i.uncovered_share - 1.0).abs() < 1e-12);
        // stock lasts 20 days, shortfall for the remaining 70 days of the horizon
        assert!((i.shortfall_units - 700.0).abs() < 1e-9, "{}", i.shortfall_units);
        assert!((i.shortfall_value - 3500.0).abs() < 1e-9);
        assert_eq!(i.recovery_days, 120.0);
    }

    #[test]
    fn multi_source_loss_is_absorbed_within_uplift_capacity() {
        // 40% lost; remaining 60% supplier can add 50% of its volume = 30% of demand -> 10% uncovered
        let f = ProductFacts { daily_demand: 10.0, available_stock: 0.0, unit_value: 1.0, shares: vec![(0, 0.4), (1, 0.6)], qualified: vec![(0, 20.0), (1, 14.0)] };
        let i = simulate(&f, &[0], params()).unwrap();
        assert!(!i.sole_source);
        assert!((i.covered_share - 0.3).abs() < 1e-12 && (i.uncovered_share - 0.1).abs() < 1e-12);
        assert!((i.required_uplift.unwrap() - 0.4 / 0.6).abs() < 1e-12);
        // no stock: covered volume is short for the 14-day alternative lead time, uncovered for all 90 days
        let expected = 10.0 * (0.1 * 90.0 + 0.3 * 14.0);
        assert!((i.shortfall_units - expected).abs() < 1e-9, "{}", i.shortfall_units);
    }

    #[test]
    fn standby_sources_add_capacity_and_unaffected_products_are_skipped() {
        let f = ProductFacts { daily_demand: 4.0, available_stock: 0.0, unit_value: 1.0, shares: vec![(0, 1.0)], qualified: vec![(0, 10.0), (1, 21.0), (2, 28.0)] };
        let i = simulate(&f, &[0], params()).unwrap();
        assert!((i.covered_share - 0.5).abs() < 1e-12); // two standby sources x 0.25
        assert_eq!(i.alternative_lead_time_days, Some(21.0));
        assert!(simulate(&f, &[7], params()).is_none());
    }
}
