//! Recurrent-memory and temporal-learning findings plus the encoder / resilience reports (no model needed).
use hcrnn::topology;
use serde::Serialize;
use serde_json::{json, Value};

use super::{
    analytics::{self, profile, slope_t},
    data::{Context, NCH},
    graph::{FlowMix, Kind},
};

#[derive(Serialize, Clone, Debug)]
pub struct Finding {
    pub node: String,
    pub kind: Kind,
    pub metric: &'static str,
    pub severity: &'static str,
    pub value: f64,
    pub detail: String,
}

/// Raw-scale weekly series of node i / channel c; unobserved weeks carry the last observed value forward.
pub fn series(ctx: &Context, i: usize, c: usize) -> Vec<f64> {
    let r = ctx.graph.r();
    let mut last: Option<f64> = None;
    let mut out = Vec::with_capacity(ctx.weeks);
    let mut first_obs = None;
    for t in 0..ctx.weeks {
        let k = (t * r + i) * NCH + c;
        if ctx.obs[k] > 0.0 {
            let v = if c == 0 { ctx.values[k].exp_m1() } else { ctx.values[k] };
            last = Some(v);
            first_obs.get_or_insert(t);
        }
        out.push(last.unwrap_or(f64::NAN));
    }
    // back-fill the head with the first observation
    if let Some(f) = first_obs {
        let v = out[f];
        for x in out.iter_mut().take(f) {
            *x = v;
        }
    }
    out.retain(|x| !x.is_nan());
    out
}

fn observed_share(ctx: &Context, i: usize, c: usize) -> f64 {
    let r = ctx.graph.r();
    (0..ctx.weeks).filter(|t| ctx.obs[(t * r + i) * NCH + c] > 0.0).count() as f64 / ctx.weeks as f64
}

pub fn findings(ctx: &Context) -> Vec<Finding> {
    let mut out = Vec::new();
    for (i, n) in ctx.graph.nodes.iter().enumerate() {
        let mut push = |metric: &'static str, severity: &'static str, value: f64, detail: String| {
            out.push(Finding { node: n.code.clone(), kind: n.kind, metric, severity, value, detail })
        };
        let delay = series(ctx, i, 1);
        let enough_delay = observed_share(ctx, i, 1) >= 0.25 && delay.len() >= 12;
        if enough_delay && matches!(n.kind, Kind::Supplier | Kind::Carrier | Kind::Product) {
            let (b, t) = slope_t(&delay);
            if t > 2.5 && b > 0.02 {
                let d13 = b * 13.0;
                push("lead_time_drift", if d13 > 3.0 { "alert" } else { "watch" }, b, format!("delay grows {:.2} days/week (t={:.1}), about {:.1} days per quarter", b, t, d13));
            }
            let late: Vec<f64> = delay.iter().map(|d| if *d > 1.0 { 1.0 } else { 0.0 }).collect();
            let share = late.iter().sum::<f64>() / late.len() as f64;
            let p = profile(&delay);
            if share >= 0.3 && p.best_acf.unwrap_or(0.0) >= 0.3 {
                push("recurring_delays", "watch", share, format!("late in {:.0}% of weeks; delay autocorrelation {:.2} at lag {} weeks", share * 100.0, p.best_acf.unwrap_or(0.0), p.best_acf_lag.unwrap_or(0)));
            }
        }
        let q = series(ctx, i, 2);
        if observed_share(ctx, i, 2) >= 0.25 && q.len() >= 12 && matches!(n.kind, Kind::Supplier | Kind::Product) {
            let (b, t) = slope_t(&q);
            if t > 2.5 && b > 0.01 {
                push("quality_degradation", "alert", b, format!("defect rate rising {:.3} points/week (t={:.1})", b, t));
            }
        }
        let vol = series(ctx, i, 0);
        if vol.len() >= 26 && matches!(n.kind, Kind::Product | Kind::Customer | Kind::Warehouse) {
            let p = profile(&vol);
            if p.seasonal_amplitude.unwrap_or(0.0) > 0.25 {
                push("seasonal_demand", "info", p.seasonal_amplitude.unwrap_or(0.0), format!("seasonal swing ±{:.0}% around the mean; dominant period {:.1} weeks", p.seasonal_amplitude.unwrap_or(0.0) * 100.0, p.dominant_period_weeks.unwrap_or(0.0)));
            }
            if p.trend_t.abs() > 2.5 && matches!(n.kind, Kind::Product | Kind::Customer) {
                push("demand_trend", "info", p.trend_per_week, format!("demand {} {:.1} units/week per week (t={:.1})", p.trend_label, p.trend_per_week.abs(), p.trend_t));
            }
            if n.kind == Kind::Warehouse {
                let diffs: Vec<f64> = vol.windows(2).map(|w| w[1] - w[0]).collect();
                let m = vol.iter().sum::<f64>() / vol.len() as f64;
                let sd = analytics::mean_sd(&diffs).1;
                if m > 0.0 && sd / m > 1.0 {
                    push("throughput_variability", "watch", sd / m, format!("week-to-week change sd is {:.0}% of mean throughput", 100.0 * sd / m));
                }
            }
        }
        let pr = series(ctx, i, 3);
        if pr.len() >= 12 && matches!(n.kind, Kind::Carrier | Kind::Warehouse) {
            let (b, t) = slope_t(&pr);
            if t > 2.5 && b > 0.0 {
                push("logistics_congestion", "watch", b, format!("load rising {:.3}/week (t={:.1})", b, t));
            }
        }
        if pr.len() >= 26 && n.kind == Kind::Product {
            if let Some((per, share)) = analytics::dominant_period(&pr) {
                if share > 0.25 && (3.0..=26.0).contains(&per) {
                    push("inventory_oscillation", "info", per, format!("stock position oscillates with a ~{:.1}-week cycle ({:.0}% of variance)", per, share * 100.0));
                }
            }
        }
    }
    let rank = |s: &str| match s { "alert" => 0, "watch" => 1, _ => 2 };
    out.sort_by(|a, b| rank(a.severity).cmp(&rank(b.severity)).then(b.value.abs().partial_cmp(&a.value.abs()).unwrap_or(std::cmp::Ordering::Equal)));
    out
}

pub fn node_profile(ctx: &Context, i: usize) -> Value {
    let chans = super::data::CHANNELS;
    let mut m = serde_json::Map::new();
    for c in 0..NCH {
        let s = series(ctx, i, c);
        if s.len() >= 8 {
            m.insert(chans[c].to_string(), serde_json::to_value(profile(&s)).unwrap_or(Value::Null));
        }
    }
    Value::Object(m)
}

/// Supplier evolution: last 13 weeks vs the 13 before, for delay and quality.
pub fn supplier_evolution(ctx: &Context) -> Vec<Value> {
    let mut out = Vec::new();
    for (i, n) in ctx.graph.nodes.iter().enumerate().filter(|(_, n)| matches!(n.kind, Kind::Supplier | Kind::Carrier)) {
        let (d, q) = (series(ctx, i, 1), series(ctx, i, 2));
        let split = |s: &[f64]| -> Option<(f64, f64)> {
            if s.len() < 26 {
                return None;
            }
            let (a, b) = (&s[s.len() - 26..s.len() - 13], &s[s.len() - 13..]);
            Some((a.iter().sum::<f64>() / 13.0, b.iter().sum::<f64>() / 13.0))
        };
        if let (Some(dd), Some(qq)) = (split(&d), split(&q)) {
            out.push(json!({"node": n.code, "kind": n.kind, "delay_days": {"prior_13w": dd.0, "last_13w": dd.1}, "defect_pct": {"prior_13w": qq.0, "last_13w": qq.1}}));
        }
    }
    out
}

pub fn encoder_report(ctx: &Context, top: usize) -> Result<Value, String> {
    let conn = ctx.graph.connectome(&FlowMix::default())?;
    let crit = analytics::criticality(&conn);
    let n = ctx.graph.r();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|a, b| crit[*b].criticality.partial_cmp(&crit[*a].criticality).unwrap_or(std::cmp::Ordering::Equal));
    let ranked: Vec<Value> = order.iter().take(top).map(|&i| json!({"node": ctx.graph.nodes[i].code, "name": ctx.graph.nodes[i].name, "kind": ctx.graph.nodes[i].kind, "scores": crit[i]})).collect();
    // bottlenecks: structural betweenness amplified by recent load
    let recent = |i: usize| {
        let s = series(ctx, i, 3);
        let k = s.len().min(4);
        if k == 0 { 0.0 } else { s[s.len() - k..].iter().sum::<f64>() / k as f64 }
    };
    let mut bn: Vec<(usize, f64)> = (0..n).map(|i| (i, crit[i].betweenness * (0.5 + recent(i).min(1.5)))).collect();
    bn.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let bottlenecks: Vec<Value> = bn.iter().take(top).filter(|(_, s)| *s > 0.0).map(|(i, s)| json!({"node": ctx.graph.nodes[*i].code, "kind": ctx.graph.nodes[*i].kind, "bottleneck_score": s, "betweenness": crit[*i].betweenness, "recent_load": recent(*i)})).collect();
    let topo = topology::topology_report(&conn);
    let layers = ctx.graph.layers();
    let totals: Vec<f64> = layers.iter().map(|l| l.iter().sum::<f64>() / 2.0).collect();
    let paths: Vec<Value> = order
        .iter()
        .filter(|&&i| ctx.graph.nodes[i].kind == Kind::Supplier)
        .take(3)
        .map(|&i| {
            let ps = analytics::risk_paths(conn.weights(), n, i, 5);
            json!({"source": ctx.graph.nodes[i].code, "paths": ps.iter().map(|p| json!({"path": p.nodes.iter().map(|x| ctx.graph.nodes[*x].code.clone()).collect::<Vec<_>>(), "probability": p.probability})).collect::<Vec<_>>()})
        })
        .collect();
    Ok(json!({
        "nodes": n, "nodes_in_database": ctx.total_nodes, "truncated": ctx.total_nodes > n,
        "edges": topo.edges, "hyperedges": ctx.graph.hypers.len(),
        "flow_totals": {"material_units": totals[0], "financial_value": totals[1], "traffic_events": totals[2], "dependency_sum": totals[3]},
        "topology": {"mean_degree": topo.mean_degree, "nonbacktracking_radius": topo.rho_b, "adjacency_norm": topo.adj_norm, "expansion_ratio": topo.expansion_ratio, "fiedler": topo.fiedler},
        "critical_nodes": ranked, "bottlenecks": bottlenecks, "risk_propagation": paths,
    }))
}
