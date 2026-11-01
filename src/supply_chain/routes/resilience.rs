//! Supplier-network resilience: concentration risk, hypergraph spectral analysis, disruption simulation.
use std::collections::HashMap;

use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use chrono::{NaiveDate, Utc};
use sea_orm::{DatabaseConnection, DbBackend, EntityTrait, FromQueryResult, QueryOrder, Statement};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use super::supplier_perf;
use crate::supply_chain::{
    entities::supplier,
    error::{AppError, AppResult},
    resilience::{analyse, heat_diffusion, hhi, null_comparison, simulate, Hypergraph, ProductFacts, SimParams, Spectral},
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/resilience/concentration", get(concentration))
        .route("/resilience/spectral", get(spectral))
        .route("/resilience/disruption", post(disruption))
}

#[derive(Debug, FromQueryResult)]
struct EdgeRow {
    product_id: Uuid,
    supplier_id: Uuid,
    units: f64,
    spend: f64,
    currency: String,
}
#[derive(Debug, FromQueryResult)]
struct ApprovedRow {
    product_id: Uuid,
    supplier_id: Uuid,
    lead_time_days: Option<i32>,
}
#[derive(Debug, FromQueryResult)]
struct LeadRow {
    product_id: Uuid,
    supplier_id: Uuid,
    lead: f64,
}
#[derive(Debug, FromQueryResult)]
struct ProdRow {
    id: Uuid,
    sku: String,
    name: String,
    unit_value: f64,
    lead_time_days: i32,
    daily_demand: f64,
    available: f64,
}

#[allow(dead_code)]
pub struct NetProduct {
    pub id: Uuid,
    pub sku: String,
    pub name: String,
    /// spend by supplier index (basis currency only)
    pub spend: Vec<(usize, f64)>,
    pub facts: ProductFacts,
}

pub struct Network {
    pub suppliers: Vec<(Uuid, String, String)>,
    pub products: Vec<NetProduct>,
    pub currency: String,
}

pub async fn load_network(db: &DatabaseConnection, as_of: NaiveDate, lookback: i32) -> AppResult<Network> {
    if !(30..=1825).contains(&lookback) {
        return Err(AppError::bad("lookback_days must be between 30 and 1825"));
    }
    let sup_models = supplier::Entity::find().order_by_asc(supplier::Column::Code).all(db).await?;
    let suppliers: Vec<(Uuid, String, String)> = sup_models.iter().map(|s| (s.id, s.code.clone(), s.name.clone())).collect();
    let sidx: HashMap<Uuid, usize> = suppliers.iter().enumerate().map(|(i, s)| (s.0, i)).collect();

    let edges = EdgeRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT pol.product_id, po.supplier_id, SUM(pol.quantity_ordered)::float8 AS units, \
                SUM(pol.quantity_ordered::numeric * pol.unit_cost)::float8 AS spend, po.currency \
         FROM purchase_order_lines pol JOIN purchase_orders po ON po.id = pol.purchase_order_id \
         WHERE po.status NOT IN ('draft', 'cancelled') AND po.order_date > $1::date - $2::int AND po.order_date <= $1::date \
         GROUP BY pol.product_id, po.supplier_id, po.currency",
        [as_of.into(), lookback.into()],
    ))
    .all(db)
    .await?;
    let approved = ApprovedRow::find_by_statement(Statement::from_string(
        DbBackend::Postgres,
        "SELECT product_id, supplier_id, lead_time_days FROM product_suppliers WHERE active".to_string(),
    ))
    .all(db)
    .await?;
    let leads = LeadRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT gr.product_id, po.supplier_id, AVG((gr.received_on - po.order_date)::float8) AS lead \
         FROM goods_receipts gr JOIN purchase_orders po ON po.id = gr.purchase_order_id \
         WHERE po.order_date <= $1::date GROUP BY gr.product_id, po.supplier_id",
        [as_of.into()],
    ))
    .all(db)
    .await?;
    let prods = ProdRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT p.id, p.sku, p.name, (CASE WHEN p.list_price > 0 THEN p.list_price ELSE p.unit_cost END)::float8 AS unit_value, \
                p.lead_time_days, \
                COALESCE((SELECT SUM(-sm.quantity_delta) FROM stock_movements sm WHERE sm.product_id = p.id \
                   AND sm.movement_type = 'issue' AND sm.created_at::date > $1::date - 90 AND sm.created_at::date <= $1::date), 0)::float8 / 90.0 AS daily_demand, \
                COALESCE((SELECT SUM(quantity - reserved - blocked) FROM stock_levels sl WHERE sl.product_id = p.id), 0)::float8 AS available \
         FROM products p WHERE p.active ORDER BY p.sku",
        [as_of.into()],
    ))
    .all(db)
    .await?;

    // basis currency = the one with the most spend (amounts are never converted)
    let mut by_ccy: HashMap<&str, f64> = HashMap::new();
    for e in &edges {
        *by_ccy.entry(e.currency.as_str()).or_default() += e.spend;
    }
    let currency = by_ccy.iter().max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal)).map(|(c, _)| c.to_string()).unwrap_or_else(|| "USD".into());

    let mut observed: HashMap<Uuid, HashMap<usize, (f64, f64)>> = HashMap::new(); // product -> supplier -> (units, spend in basis ccy)
    for e in &edges {
        if let Some(&si) = sidx.get(&e.supplier_id) {
            let slot = observed.entry(e.product_id).or_default().entry(si).or_insert((0.0, 0.0));
            slot.0 += e.units;
            if e.currency == currency {
                slot.1 += e.spend;
            }
        }
    }
    let mut approved_by: HashMap<Uuid, HashMap<usize, Option<f64>>> = HashMap::new();
    for a in &approved {
        if let Some(&si) = sidx.get(&a.supplier_id) {
            approved_by.entry(a.product_id).or_default().insert(si, a.lead_time_days.map(|d| d as f64));
        }
    }
    let mut lead_obs: HashMap<(Uuid, usize), f64> = HashMap::new();
    for l in &leads {
        if let Some(&si) = sidx.get(&l.supplier_id) {
            lead_obs.insert((l.product_id, si), l.lead);
        }
    }

    let mut products = Vec::new();
    for p in prods {
        let obs = observed.get(&p.id);
        let appr = approved_by.get(&p.id);
        if obs.is_none() && appr.is_none() {
            continue;
        }
        let mut qualified: Vec<(usize, f64)> = Vec::new();
        let mut ids: Vec<usize> = obs.map(|m| m.keys().copied().collect()).unwrap_or_default();
        if let Some(a) = appr {
            for k in a.keys() {
                if !ids.contains(k) {
                    ids.push(*k);
                }
            }
        }
        ids.sort_unstable();
        for si in ids {
            let lt = appr.and_then(|a| a.get(&si).copied().flatten()).or_else(|| lead_obs.get(&(p.id, si)).copied()).unwrap_or(p.lead_time_days as f64);
            qualified.push((si, lt));
        }
        let shares: Vec<(usize, f64)> = obs.map(|m| m.iter().map(|(k, v)| (*k, v.0)).collect()).unwrap_or_default();
        let spend: Vec<(usize, f64)> = obs.map(|m| m.iter().filter(|(_, v)| v.1 > 0.0).map(|(k, v)| (*k, v.1)).collect()).unwrap_or_default();
        products.push(NetProduct {
            id: p.id,
            sku: p.sku,
            name: p.name,
            spend,
            facts: ProductFacts { daily_demand: p.daily_demand, available_stock: p.available, unit_value: p.unit_value, shares, qualified },
        });
    }
    Ok(Network { suppliers, products, currency })
}

/// Hypergraph over suppliers that qualify for at least one product; `skip` removes failed suppliers.
/// Returns the graph and the mapping vertex -> global supplier index.
fn build_hypergraph(net: &Network, skip: &[usize]) -> (Hypergraph, Vec<usize>) {
    let mut vmap: HashMap<usize, usize> = HashMap::new();
    let mut back: Vec<usize> = Vec::new();
    let mut edges = Vec::new();
    for p in &net.products {
        let members: Vec<usize> = p.facts.qualified.iter().map(|(s, _)| *s).filter(|s| !skip.contains(s)).collect();
        if members.is_empty() {
            continue;
        }
        let e: Vec<usize> = members
            .iter()
            .map(|s| {
                *vmap.entry(*s).or_insert_with(|| {
                    back.push(*s);
                    back.len() - 1
                })
            })
            .collect();
        edges.push((e, 1.0));
    }
    (Hypergraph { n: back.len(), edges }, back)
}

#[derive(Deserialize)]
pub struct NetQuery {
    as_of: Option<NaiveDate>,
    /// purchase history window for observed sourcing (default 365)
    lookback_days: Option<i32>,
}

fn today(q: &NetQuery) -> (NaiveDate, i32) {
    (q.as_of.unwrap_or_else(|| Utc::now().date_naive()), q.lookback_days.unwrap_or(365))
}

#[derive(Serialize)]
struct ProductConc {
    sku: String,
    name: String,
    qualified_suppliers: usize,
    active_suppliers: usize,
    hhi: Option<f64>,
    effective_sources: Option<f64>,
    sole_source: bool,
    top_supplier: Option<String>,
    top_share: Option<f64>,
}

#[derive(Serialize)]
struct SupplierConc {
    code: String,
    name: String,
    spend_share: f64,
    products_supplied: usize,
    sole_source_products: usize,
    esg_score: Option<f64>,
    on_time_rate: Option<f64>,
    in_full_rate: Option<f64>,
    quality_score: Option<f64>,
    performance_score: Option<f64>,
    risk_band: &'static str,
    recommendation: &'static str,
}

/// Concentration risk: HHI, effective sources, sole-source exposure and spend concentration, joined
/// with each supplier's scorecard. Product shares are by purchased units; supplier spend shares use
/// the dominant currency (never converted).
async fn concentration(State(s): State<AppState>, Query(q): Query<NetQuery>) -> AppResult<Json<Value>> {
    let (as_of, lookback) = today(&q);
    let net = load_network(&s.db, as_of, lookback).await?;
    let scorecards: HashMap<Uuid, supplier_perf::Scorecard> =
        supplier_perf::fetch(&s.db, None, None, 3).await?.into_iter().map(|c| (c.supplier_id, c)).collect();
    let esg: HashMap<Uuid, Option<f64>> = supplier::Entity::find().all(&s.db).await?.into_iter().map(|m| (m.id, m.esg_score)).collect();

    let mut products = Vec::new();
    let mut supplier_spend: HashMap<usize, f64> = HashMap::new();
    let mut supplied: HashMap<usize, usize> = HashMap::new();
    let mut sole: HashMap<usize, usize> = HashMap::new();
    let (mut total_spend, mut sole_spend, mut hhi_weighted, mut hhi_weight) = (0.0, 0.0, 0.0, 0.0);
    let mut sole_count = 0usize;
    for p in &net.products {
        let shares: Vec<f64> = p.facts.shares.iter().map(|(_, u)| *u).collect();
        let h = (!shares.is_empty()).then(|| hhi(&shares));
        let sole_source = p.facts.qualified.len() == 1;
        let top = p.facts.shares.iter().max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let tot_units: f64 = shares.iter().sum();
        let pspend: f64 = p.spend.iter().map(|(_, v)| v).sum();
        total_spend += pspend;
        if sole_source {
            sole_count += 1;
            sole_spend += pspend;
            sole.entry(p.facts.qualified[0].0).and_modify(|c| *c += 1).or_insert(1);
        }
        if let Some(hv) = h {
            hhi_weighted += hv * pspend.max(1e-9);
            hhi_weight += pspend.max(1e-9);
        }
        for (si, v) in &p.spend {
            *supplier_spend.entry(*si).or_default() += v;
        }
        for (si, _) in &p.facts.qualified {
            *supplied.entry(*si).or_default() += 1;
        }
        products.push(ProductConc {
            sku: p.sku.clone(),
            name: p.name.clone(),
            qualified_suppliers: p.facts.qualified.len(),
            active_suppliers: p.facts.shares.len(),
            hhi: h,
            effective_sources: h.map(|v| 1.0 / v),
            sole_source,
            top_supplier: top.map(|(si, _)| net.suppliers[*si].1.clone()),
            top_share: top.map(|(_, u)| if tot_units > 0.0 { u / tot_units } else { 0.0 }),
        });
    }
    let spend_vec: Vec<f64> = supplier_spend.values().copied().collect();
    let sup_hhi = hhi(&spend_vec);
    let top_supplier = supplier_spend.iter().max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut suppliers: Vec<SupplierConc> = supplied
        .iter()
        .map(|(si, n)| {
            let (id, code, name) = &net.suppliers[*si];
            let sc = scorecards.get(id);
            SupplierConc {
                code: code.clone(),
                name: name.clone(),
                spend_share: if total_spend > 0.0 { supplier_spend.get(si).copied().unwrap_or(0.0) / total_spend } else { 0.0 },
                products_supplied: *n,
                sole_source_products: sole.get(si).copied().unwrap_or(0),
                esg_score: esg.get(id).copied().flatten(),
                on_time_rate: sc.and_then(|c| c.on_time_rate),
                in_full_rate: sc.and_then(|c| c.in_full_rate),
                quality_score: sc.and_then(|c| c.quality_score),
                performance_score: sc.and_then(|c| c.performance_score),
                risk_band: sc.map(|c| c.risk_band).unwrap_or("unknown"),
                recommendation: sc.map(|c| c.recommendation).unwrap_or("ok"),
            }
        })
        .collect();
    suppliers.sort_by(|a, b| b.spend_share.partial_cmp(&a.spend_share).unwrap_or(std::cmp::Ordering::Equal).then(a.code.cmp(&b.code)));
    products.sort_by(|a, b| b.sole_source.cmp(&a.sole_source).then(a.sku.cmp(&b.sku)));
    let n = net.products.len();
    Ok(Json(json!({
        "as_of": as_of,
        "lookback_days": lookback,
        "network": {
            "products": n,
            "suppliers": suppliers.len(),
            "sole_source_products": sole_count,
            "sole_source_share": if n > 0 { sole_count as f64 / n as f64 } else { 0.0 },
            "sole_source_spend_share": if total_spend > 0.0 { sole_spend / total_spend } else { 0.0 },
            "mean_product_hhi": if hhi_weight > 0.0 { Some(hhi_weighted / hhi_weight) } else { None },
            "supplier_spend_hhi": sup_hhi,
            "effective_suppliers": if sup_hhi > 0.0 { Some(1.0 / sup_hhi) } else { None },
            "top_supplier": top_supplier.map(|(si, v)| json!({"code": net.suppliers[*si].1, "spend_share": if total_spend > 0.0 { v / total_spend } else { 0.0 }})),
            "currency": net.currency,
        },
        "suppliers": suppliers,
        "products": products,
    })))
}

#[derive(Serialize)]
struct Cluster {
    cluster: usize,
    suppliers: Vec<String>,
    fiedler_side: Option<u8>,
}

fn clusters(net: &Network, back: &[usize], sp: &Spectral) -> Vec<Cluster> {
    let mut out: HashMap<(usize, Option<u8>), Vec<String>> = HashMap::new();
    for (v, &si) in back.iter().enumerate() {
        out.entry((sp.component_of[v], sp.fiedler_side[v])).or_default().push(net.suppliers[si].1.clone());
    }
    let mut c: Vec<Cluster> = out.into_iter().map(|((cl, side), mut sup)| { sup.sort(); Cluster { cluster: cl, suppliers: sup, fiedler_side: side } }).collect();
    c.sort_by(|a, b| b.suppliers.len().cmp(&a.suppliers.len()).then(a.cluster.cmp(&b.cluster)));
    c
}

/// Supplier hypergraph (suppliers = vertices, products = hyperedges): lambda_2, components,
/// Fiedler clusters and fragmentation risk.
async fn spectral(State(s): State<AppState>, Query(q): Query<NetQuery>) -> AppResult<Json<Value>> {
    let (as_of, lookback) = today(&q);
    let net = load_network(&s.db, as_of, lookback).await?;
    let (h, back) = build_hypergraph(&net, &[]);
    let sp = analyse(&h).ok_or_else(|| AppError::bad("network too large for dense spectral analysis (max 1500 suppliers)"))?;
    let cl = clusters(&net, &back, &sp);
    let nm = null_comparison(&h, 40, 20260501);
    Ok(Json(json!({ "as_of": as_of, "lookback_days": lookback, "spectrum": sp, "clusters": cl, "null_model": nm })))
}

#[derive(Deserialize)]
pub struct DisruptionReq {
    /// supplier codes to remove
    pub supplier_codes: Vec<String>,
    pub as_of: Option<NaiveDate>,
    pub lookback_days: Option<i32>,
    /// planning horizon for the shortfall (default 90 days)
    pub horizon_days: Option<f64>,
    /// extra volume active remaining suppliers can absorb, fraction of baseline (default 0.5)
    pub max_uplift: Option<f64>,
    /// volume a qualified-but-unused supplier can take, fraction of demand (default 0.25)
    pub standby_share: Option<f64>,
    /// days to qualify a brand-new source when none remains (default 120)
    pub qualification_days: Option<f64>,
    /// return at most this many products (default 25)
    pub top: Option<usize>,
}

/// Remove suppliers and measure: lost volume share, absorbable vs uncovered volume, required uplift
/// from the remaining sources, stock-out timing, units and value short over the horizon, recovery time,
/// plus the effect on network connectivity and which suppliers sit structurally closest to the failure.
pub async fn disruption(State(s): State<AppState>, Json(r): Json<DisruptionReq>) -> AppResult<Json<Value>> {
    if r.supplier_codes.is_empty() {
        return Err(AppError::bad("supplier_codes must not be empty"));
    }
    let as_of = r.as_of.unwrap_or_else(|| Utc::now().date_naive());
    let net = load_network(&s.db, as_of, r.lookback_days.unwrap_or(365)).await?;
    let removed: Vec<usize> = r
        .supplier_codes
        .iter()
        .map(|c| {
            net.suppliers.iter().position(|s| s.1.eq_ignore_ascii_case(c.trim())).ok_or_else(|| AppError::not_found(&format!("supplier {c}")))
        })
        .collect::<AppResult<_>>()?;
    let params = SimParams {
        horizon_days: r.horizon_days.unwrap_or(90.0).clamp(1.0, 730.0),
        max_uplift: r.max_uplift.unwrap_or(0.5).clamp(0.0, 5.0),
        standby_share: r.standby_share.unwrap_or(0.25).clamp(0.0, 1.0),
        qualification_days: r.qualification_days.unwrap_or(120.0).max(0.0),
    };
    let mut rows = Vec::new();
    let (mut demand_value, mut short_value, mut short_units) = (0.0, 0.0, 0.0);
    let (mut affected, mut sole_hit) = (0usize, 0usize);
    for p in &net.products {
        demand_value += p.facts.daily_demand * params.horizon_days * p.facts.unit_value;
        if let Some(i) = simulate(&p.facts, &removed, params) {
            affected += 1;
            if i.sole_source {
                sole_hit += 1;
            }
            short_value += i.shortfall_value;
            short_units += i.shortfall_units;
            rows.push((p, i));
        }
    }
    rows.sort_by(|a, b| b.1.shortfall_value.partial_cmp(&a.1.shortfall_value).unwrap_or(std::cmp::Ordering::Equal).then(a.0.sku.cmp(&b.0.sku)));
    let max_rec = rows.iter().map(|(_, i)| i.recovery_days).fold(0.0, f64::max);

    let (h0, back0) = build_hypergraph(&net, &[]);
    let before = analyse(&h0);
    let (h1, _) = build_hypergraph(&net, &removed);
    let after = analyse(&h1);
    // heat-kernel proximity of the surviving suppliers to the failed ones (shock on the original graph)
    let mut proximity = Vec::new();
    if h0.n <= 800 {
        let src: Vec<usize> = back0.iter().enumerate().filter(|(_, si)| removed.contains(si)).map(|(v, _)| v).collect();
        if !src.is_empty() {
            let u = heat_diffusion(&h0, &src, 1.0);
            let mut sc: Vec<(String, f64)> = back0.iter().enumerate().filter(|(_, si)| !removed.contains(si)).map(|(v, si)| (net.suppliers[*si].1.clone(), u[v])).collect();
            sc.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(&b.0)));
            proximity = sc.into_iter().take(5).filter(|(_, v)| *v > 1e-6).map(|(c, v)| json!({"supplier": c, "structural_proximity": v})).collect();
        }
    }
    let top = r.top.unwrap_or(25);
    let list: Vec<Value> = rows
        .iter()
        .take(top)
        .map(|(p, i)| json!({"sku": p.sku, "name": p.name, "impact": i}))
        .collect();
    let summary = |sp: &Option<Spectral>| sp.as_ref().map(|x| json!({"suppliers": x.suppliers, "components": x.components, "lambda2": x.lambda2, "lambda2_main_component": x.lambda2_main_component, "fragmentation_risk": x.fragmentation_risk}));
    Ok(Json(json!({
        "removed": r.supplier_codes,
        "parameters": {"horizon_days": params.horizon_days, "max_uplift": params.max_uplift, "standby_share": params.standby_share, "qualification_days": params.qualification_days},
        "summary": {
            "products_affected": affected,
            "products_now_without_any_source": sole_hit,
            "shortfall_units": short_units,
            "shortfall_value": short_value,
            "demand_value_in_horizon": demand_value,
            "uncovered_demand_share": if demand_value > 0.0 { short_value / demand_value } else { 0.0 },
            "longest_recovery_days": max_rec,
        },
        "network_before": summary(&before),
        "network_after": summary(&after),
        "nearest_suppliers": proximity,
        "products": list,
    })))
}

/// Compact resilience snapshot used by the planning cycle.
pub async fn network_snapshot(db: &DatabaseConnection, as_of: NaiveDate) -> AppResult<Value> {
    let net = load_network(db, as_of, 365).await?;
    let sole = net.products.iter().filter(|p| p.facts.qualified.len() == 1).count();
    let n = net.products.len();
    let (h, _) = build_hypergraph(&net, &[]);
    let sp = analyse(&h);
    Ok(json!({
        "products_with_sourcing": n,
        "sole_source_products": sole,
        "sole_source_share": if n > 0 { sole as f64 / n as f64 } else { 0.0 },
        "components": sp.as_ref().map(|x| x.components),
        "lambda2_main_component": sp.as_ref().map(|x| x.lambda2_main_component),
        "fragmentation_risk": sp.as_ref().map(|x| x.fragmentation_risk),
    }))
}
