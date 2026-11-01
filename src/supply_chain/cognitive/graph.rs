//! Supply-chain graph and hypergraph construction (pure, no database access).
//!
//! Nodes: suppliers, products, warehouses, customers, carriers. Edges carry four flow layers
//! (material, financial, traffic, dependency) and a lead time. Hyperedges capture many-to-many
//! interactions (purchase order, shipment, contract, production run). The pairwise layers become the
//! `hcrnn::Connectome` the CIR-GNN is wired from; the hyperedges feed the spectral resilience maths.
use std::collections::HashMap;

use hcrnn::{Connectome, Region};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::supply_chain::resilience::Hypergraph;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Supplier,
    Product,
    Warehouse,
    Customer,
    Carrier,
}

impl Kind {
    pub const ALL: [Kind; 5] = [Kind::Supplier, Kind::Product, Kind::Warehouse, Kind::Customer, Kind::Carrier];
    pub fn idx(self) -> usize {
        Kind::ALL.iter().position(|k| *k == self).unwrap()
    }
    pub fn label(self) -> &'static str {
        match self {
            Kind::Supplier => "Supplier",
            Kind::Product => "Product",
            Kind::Warehouse => "Warehouse",
            Kind::Customer => "Customer",
            Kind::Carrier => "Carrier",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub id: Uuid,
    pub kind: Kind,
    pub code: String,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Flow {
    pub material: f64,
    pub financial: f64,
    pub traffic: f64,
    pub dependency: f64,
    /// (sum of lead_days * weight, weight) for a weighted average
    pub lead_acc: (f64, f64),
}

#[derive(Clone, Debug)]
pub struct Hyper {
    pub kind: &'static str,
    pub members: Vec<usize>,
    pub weight: f64,
}

#[derive(Clone, Debug, Default)]
pub struct SupplyGraph {
    pub nodes: Vec<Node>,
    pub flows: HashMap<(usize, usize), Flow>,
    pub hypers: Vec<Hyper>,
}

pub struct FlowMix {
    pub material: f64,
    pub financial: f64,
    pub traffic: f64,
    pub dependency: f64,
}
impl Default for FlowMix {
    fn default() -> Self {
        FlowMix { material: 0.35, financial: 0.25, traffic: 0.2, dependency: 0.2 }
    }
}

impl SupplyGraph {
    pub fn new(nodes: Vec<Node>) -> Self {
        SupplyGraph { nodes, ..Default::default() }
    }
    pub fn r(&self) -> usize {
        self.nodes.len()
    }
    pub fn index(&self) -> HashMap<Uuid, usize> {
        self.nodes.iter().enumerate().map(|(i, n)| (n.id, i)).collect()
    }
    pub fn add_flow(&mut self, from: usize, to: usize, f: Flow) {
        if from == to || from >= self.r() || to >= self.r() {
            return;
        }
        let e = self.flows.entry((from, to)).or_default();
        e.material += f.material;
        e.financial += f.financial;
        e.traffic += f.traffic;
        e.dependency = e.dependency.max(f.dependency);
        e.lead_acc.0 += f.lead_acc.0;
        e.lead_acc.1 += f.lead_acc.1;
    }
    pub fn add_hyper(&mut self, kind: &'static str, mut members: Vec<usize>, weight: f64) {
        members.retain(|m| *m < self.r());
        members.sort_unstable();
        members.dedup();
        if members.len() >= 2 {
            self.hypers.push(Hyper { kind, members, weight });
        }
    }

    /// Four symmetric flow layers (R x R, row-major).
    pub fn layers(&self) -> [Vec<f64>; 4] {
        let n = self.r();
        let mut l = [vec![0.0; n * n], vec![0.0; n * n], vec![0.0; n * n], vec![0.0; n * n]];
        for (&(a, b), f) in &self.flows {
            for (k, v) in [f.material, f.financial, f.traffic, f.dependency].into_iter().enumerate() {
                l[k][a * n + b] += v;
                l[k][b * n + a] += v;
            }
        }
        l
    }

    /// Mixed edge weights: each layer is scaled by its mean positive entry and log-compressed
    /// (flows are heavy-tailed), then combined; hyperedges add a clique-expansion bonus.
    pub fn mixed_weights(&self, mix: &FlowMix) -> Vec<f64> {
        let n = self.r();
        let layers = self.layers();
        let mut w = vec![0.0; n * n];
        for (k, m) in [mix.material, mix.financial, mix.traffic, mix.dependency].into_iter().enumerate() {
            let pos: Vec<f64> = layers[k].iter().copied().filter(|x| *x > 0.0).collect();
            if pos.is_empty() || m == 0.0 {
                continue;
            }
            let mean = pos.iter().sum::<f64>() / pos.len() as f64;
            for (i, x) in layers[k].iter().enumerate() {
                if *x > 0.0 {
                    w[i] += m * (1.0 + x / mean).ln();
                }
            }
        }
        let base_mean = {
            let pos: Vec<f64> = w.iter().copied().filter(|x| *x > 0.0).collect();
            if pos.is_empty() { 1.0 } else { pos.iter().sum::<f64>() / pos.len() as f64 }
        };
        for h in &self.hypers {
            let k = h.members.len().min(12);
            let bonus = 0.15 * base_mean * h.weight / (k as f64 - 1.0).max(1.0);
            for a in 0..k {
                for b in (a + 1)..k {
                    let (i, j) = (h.members[a], h.members[b]);
                    w[i * n + j] += bonus;
                    w[j * n + i] += bonus;
                }
            }
        }
        w
    }

    /// Lead time (weeks) between node pairs; 1 week where no lead time was observed.
    pub fn lead_weeks(&self) -> Vec<f64> {
        let n = self.r();
        let mut len = vec![0.0; n * n];
        for (&(a, b), f) in &self.flows {
            if f.lead_acc.1 > 0.0 {
                let wk = (f.lead_acc.0 / f.lead_acc.1 / 7.0).clamp(1.0, 8.0);
                for (i, j) in [(a, b), (b, a)] {
                    len[i * n + j] = if len[i * n + j] > 0.0 { 0.5 * (len[i * n + j] + wk) } else { wk };
                }
            }
        }
        for i in 0..n * n {
            if len[i] <= 0.0 {
                len[i] = 1.0;
            }
        }
        len
    }

    pub fn connectome(&self, mix: &FlowMix) -> Result<Connectome, String> {
        let regions: Vec<Region> = self
            .nodes
            .iter()
            .map(|n| Region { name: n.code.clone(), hemi: '?', system: n.kind.label().to_string(), pos: [0.0; 3] })
            .collect();
        Connectome::new(regions, self.mixed_weights(mix), self.lead_weeks(), "supply-chain flow graph")
    }

    /// Hypergraph over all nodes for the spectral resilience maths (hyperedges + pairwise flows).
    pub fn hypergraph(&self) -> Hypergraph {
        let mut edges: Vec<(Vec<usize>, f64)> = self.hypers.iter().map(|h| (h.members.clone(), h.weight)).collect();
        for &(a, b) in self.flows.keys() {
            if a < b || !self.flows.contains_key(&(b, a)) {
                edges.push((vec![a.min(b), a.max(b)], 0.5));
            }
        }
        Hypergraph { n: self.r(), edges }
    }

    /// Keep the `max` strongest nodes (strength = total flow incl. hyperedge participation).
    /// Returns the reduced graph and the indices kept (in new order = old order).
    pub fn restrict(&self, max: usize) -> (SupplyGraph, Vec<usize>) {
        let n = self.r();
        if n <= max {
            return (self.clone(), (0..n).collect());
        }
        let mut s = vec![0.0; n];
        for (&(a, b), f) in &self.flows {
            let v = f.traffic + 1.0;
            s[a] += v;
            s[b] += v;
        }
        for h in &self.hypers {
            for &m in &h.members {
                s[m] += 1.0;
            }
        }
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|a, b| s[*b].partial_cmp(&s[*a]).unwrap_or(std::cmp::Ordering::Equal));
        let mut keep: Vec<usize> = order.into_iter().take(max).collect();
        keep.sort_unstable();
        let map: HashMap<usize, usize> = keep.iter().enumerate().map(|(i, k)| (*k, i)).collect();
        let mut g = SupplyGraph::new(keep.iter().map(|k| self.nodes[*k].clone()).collect());
        for (&(a, b), f) in &self.flows {
            if let (Some(&na), Some(&nb)) = (map.get(&a), map.get(&b)) {
                g.flows.insert((na, nb), *f);
            }
        }
        for h in &self.hypers {
            let m: Vec<usize> = h.members.iter().filter_map(|x| map.get(x).copied()).collect();
            g.add_hyper(h.kind, m, h.weight);
        }
        (g, keep)
    }

    /// Stable fingerprint of the node set (model is stale when it changes).
    pub fn fingerprint(&self) -> String {
        let mut ids: Vec<String> = self.nodes.iter().map(|n| n.id.to_string()).collect();
        ids.sort();
        let mut h: u64 = 0xcbf29ce484222325;
        for s in ids {
            for b in s.bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(0x100000001b3);
            }
        }
        format!("{h:016x}")
    }
}

#[cfg(test)]
pub fn test_graph() -> SupplyGraph {
    let mk = |k: Kind, c: &str| Node { id: Uuid::new_v4(), kind: k, code: c.into(), name: c.into() };
    let mut g = SupplyGraph::new(vec![
        mk(Kind::Supplier, "S1"),
        mk(Kind::Supplier, "S2"),
        mk(Kind::Product, "P1"),
        mk(Kind::Product, "P2"),
        mk(Kind::Warehouse, "W1"),
        mk(Kind::Customer, "C1"),
        mk(Kind::Carrier, "K1"),
    ]);
    let f = |m: f64, lead: f64| Flow { material: m, financial: m * 3.0, traffic: 5.0, dependency: 0.5, lead_acc: (lead * m, m) };
    g.add_flow(0, 2, f(100.0, 14.0));
    g.add_flow(1, 2, f(40.0, 21.0));
    g.add_flow(1, 3, f(80.0, 21.0));
    g.add_flow(2, 4, f(90.0, 7.0));
    g.add_flow(3, 4, f(70.0, 7.0));
    g.add_flow(4, 5, f(120.0, 3.0));
    g.add_flow(4, 6, f(120.0, 3.0));
    g.add_flow(6, 5, f(120.0, 3.0));
    g.add_hyper("po", vec![0, 2, 4], 1.0);
    g.add_hyper("shipment", vec![6, 4, 5, 2], 1.0);
    g
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connectome_has_flow_weights_lead_delays_and_kinds() {
        let g = test_graph();
        let c = g.connectome(&FlowMix::default()).unwrap();
        assert_eq!(c.n(), 7);
        assert!(c.weight(0, 2) > 0.0 && c.weight(2, 0) == c.weight(0, 2));
        assert_eq!(c.regions[4].system, "Warehouse");
        // 21 days = 3 weeks of conduction delay between S2 and P1
        assert!((c.length(1, 2) - 3.0).abs() < 1e-9);
        // hyperedge clique expansion links S1 and W1 even without a direct flow
        assert!(c.weight(0, 4) > 0.0);
    }

    #[test]
    fn restrict_keeps_strongest_nodes_and_remaps() {
        let g = test_graph();
        let (h, keep) = g.restrict(5);
        assert_eq!(h.r(), 5);
        assert_eq!(keep.len(), 5);
        assert!(h.flows.keys().all(|(a, b)| *a < 5 && *b < 5));
    }

    #[test]
    fn hypergraph_covers_all_nodes() {
        let g = test_graph();
        let h = g.hypergraph();
        assert_eq!(h.n, 7);
        assert!(crate::supply_chain::resilience::analyse(&h).unwrap().components == 1);
    }
}
