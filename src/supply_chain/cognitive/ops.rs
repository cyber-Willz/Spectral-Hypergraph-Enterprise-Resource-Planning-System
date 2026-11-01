//! Orchestration against the database: decision planning, guard-railed autonomous execution,
//! digital-twin scenarios and the continuous-learning loop.
use std::collections::HashMap;
use std::sync::Arc;

use chrono::{Duration, NaiveDate, Utc};
use sea_orm::{prelude::Decimal, ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait, FromQueryResult, Statement, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    analytics::{self, mean_sd},
    cirgnn::FitConfig,
    data::{load_context, monday, Context, CHANNELS, NCH},
    engine::{self, Fc, Op, Shock, Trained, LATE_DAYS},
    graph::Kind,
    insights::series,
    optimize::{self, Cand, InvIn, WhPos},
};
use crate::supply_chain::{
    entities::{location, product},
    error::{AppError, AppResult},
    routes::{gen_number, loop_ops::apply_order_rules},
    state::AppState,
    stats::service_level,
    stock::{add_stock, ensure_storage_compatible, record_movement, remove_stock},
};

// ---- policies ------------------------------------------------------------------------------------------

#[derive(FromQueryResult)]
struct PolRow {
    autonomy: String,
    params: Value,
}

pub async fn policy(db: &DatabaseConnection, kind: &str) -> AppResult<(String, Value)> {
    let r = PolRow::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres, "SELECT autonomy, params FROM cognitive_policies WHERE action_kind = $1", [kind.into()]))
        .one(db)
        .await?
        .ok_or_else(|| AppError::not_found("policy"))?;
    Ok((r.autonomy, r.params))
}

fn pf(v: &Value, k: &str, d: f64) -> f64 {
    v.get(k).and_then(|x| x.as_f64()).unwrap_or(d)
}

pub async fn train_async(ctx: Arc<Context>, cfg: FitConfig, warm: Option<Trained>) -> AppResult<(Trained, super::cirgnn::FitReport)> {
    tokio::task::spawn_blocking(move || engine::train(&ctx, &cfg, warm.as_ref()))
        .await
        .map_err(|e| AppError::conflict(format!("training task failed: {e}")))?
}

// ---- planning -------------------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct PlanReq {
    pub warehouse_id: Option<Uuid>,
    pub as_of: Option<NaiveDate>,
    pub weeks: Option<usize>,
    /// store proposals in the action log (default true)
    pub create_actions: Option<bool>,
    pub max_items: Option<usize>,
    /// cost of one late shipment, used to rank carriers (default 50)
    pub late_penalty: Option<f64>,
}

#[derive(FromQueryResult)]
struct InvRow {
    product_id: Uuid,
    sku: String,
    warehouse_id: Uuid,
    wh: String,
    criticality_class: String,
    storage_class: String,
    unit_cost: f64,
    holding_cost_rate: f64,
    stockout_cost: Option<f64>,
    review_period_days: i32,
    lead_time_days: i32,
    reorder_point: i64,
    reorder_qty: i64,
    available: f64,
    on_order: f64,
    issued91: f64,
    has_draft: bool,
}
#[derive(FromQueryResult)]
struct CandRow {
    product_id: Uuid,
    supplier_id: Uuid,
    code: String,
    unit_cost: f64,
    lead_days: i32,
    min_order_qty: i64,
    order_multiple: i64,
    contract_id: Option<Uuid>,
}
#[derive(FromQueryResult)]
struct SigRow {
    node_id: Uuid,
    v: f64,
}
#[derive(FromQueryResult)]
struct LotRow {
    location_id: Uuid,
    lot_number: String,
    avail: i64,
}
#[derive(FromQueryResult)]
struct IdRow {
    id: Uuid,
}
#[derive(FromQueryResult)]
struct CarrierCost {
    carrier_id: Uuid,
    avg_freight: f64,
}
#[derive(FromQueryResult)]
struct ProdRun {
    product_id: Uuid,
    uph: f64,
    scrap: f64,
}

struct Forecaster<'a> {
    ctx: &'a Context,
    model: Option<&'a Trained>,
    fc: HashMap<(usize, usize, usize), &'a Fc>,
    idx: HashMap<Uuid, usize>,
}

impl<'a> Forecaster<'a> {
    fn get(&self, node: usize, ch: usize, h: usize) -> Option<&Fc> {
        self.fc.get(&(node, ch, h)).copied()
    }
    fn trusted(&self, ch: usize) -> bool {
        self.model.map(|m| m.channel_ok(ch)).unwrap_or(false)
    }
    fn recent(&self, node: usize, ch: usize, n: usize) -> Vec<f64> {
        let s = series(self.ctx, node, ch);
        s[s.len().saturating_sub(n)..].to_vec()
    }
    /// (mu per week, sd per week, source)
    fn demand(&self, node: usize) -> (f64, f64, &'static str) {
        if self.trusted(0) {
            let hs: Vec<f64> = engine::HORIZONS.iter().filter_map(|h| self.get(node, 0, *h)).map(|f| f.value).collect();
            if !hs.is_empty() {
                let mu = hs.iter().sum::<f64>() / hs.len() as f64;
                let rs = self.model.unwrap().channel_eval[0].resid_sd.iter().sum::<f64>() / self.model.unwrap().channel_eval[0].resid_sd.len().max(1) as f64;
                let sd_ln = rs * self.model.unwrap().sd[node * NCH];
                return (mu, (mu * sd_ln).max(0.05 * mu), "cir_gnn");
            }
        }
        let r = self.recent(node, 0, 13);
        let (m, s) = mean_sd(&r);
        let last4 = &r[r.len().saturating_sub(4)..];
        let mu = if last4.is_empty() { m } else { last4.iter().sum::<f64>() / last4.len() as f64 };
        (mu, s, "moving_average_fallback")
    }
    /// (P(late), recent defect rate 0..1, expected delay days)
    fn supplier_signal(&self, node: usize) -> (f64, f64, f64) {
        let d = self.recent(node, 1, 13);
        let late_share = if d.is_empty() { 0.0 } else { d.iter().filter(|x| **x > LATE_DAYS).count() as f64 / d.len() as f64 };
        let p = match self.get(node, 1, 1) {
            Some(f) if self.trusted(1) => f.prob_late.unwrap_or(late_share),
            _ => late_share,
        };
        let q = self.recent(node, 2, 13);
        let defect = if q.is_empty() { 0.0 } else { q.iter().sum::<f64>() / q.len() as f64 / 100.0 };
        let exp_delay = match self.get(node, 1, 1) {
            Some(f) if self.trusted(1) => f.value.max(0.0),
            _ => if d.is_empty() { 0.0 } else { (d.iter().sum::<f64>() / d.len() as f64).max(0.0) },
        };
        (p, defect, exp_delay)
    }
}

pub async fn plan(state: &AppState, req: PlanReq) -> AppResult<Value> {
    let db = &state.db;
    let as_of = req.as_of.unwrap_or_else(|| Utc::now().date_naive());
    let ctx = load_context(db, as_of, req.weeks.unwrap_or(104), 365).await?;
    let loaded = engine::load_active(db).await?;
    let mut status = "no_model: statistical fallbacks used (train the CIR-GNN for learned forecasts)".to_string();
    let (mut model_id, mut model, mut fcs) = (None, None, Vec::new());
    if let Some((id, t)) = loaded {
        match engine::replay(&t, &ctx) {
            Ok((_, y)) => {
                fcs = engine::forecasts(&t, &ctx, &y);
                status = format!("cir_gnn model {id}; trusted channels: {}", (0..NCH).filter(|c| t.channel_ok(*c)).map(|c| CHANNELS[c]).collect::<Vec<_>>().join(","));
                model_id = Some(id);
                model = Some(t);
            }
            Err(_) => status = "model_stale: network changed since training; statistical fallbacks used (run /cognitive/learning/step)".into(),
        }
    }
    let fz = Forecaster { ctx: &ctx, model: model.as_ref(), fc: fcs.iter().map(|f| ((f.node, CHANNELS.iter().position(|c| *c == f.channel).unwrap(), f.horizon_weeks), f)).collect(), idx: ctx.graph.index() };

    let (_, learn) = policy(db, "learning").await?;
    let (z_mult, risk_pen) = (pf(&learn, "service_z_mult", 1.0), pf(&learn, "risk_penalty", 1.0));
    let (po_auto, po_p) = policy(db, "create_po").await?;
    let max_value = pf(&po_p, "max_order_value", 50000.0);
    let max_risk = pf(&po_p, "max_supplier_risk", 0.5);
    let max_share = pf(&po_p, "max_share_per_supplier", 0.7);
    let submit = po_p.get("submit").and_then(|v| v.as_bool()).unwrap_or(false);

    let sig: HashMap<Uuid, f64> = SigRow::find_by_statement(Statement::from_string(
        DbBackend::Postgres,
        "SELECT node_id, MAX(value)::float8 AS v FROM cognitive_signals WHERE signal_type = 'disruption' AND node_id IS NOT NULL AND observed_at > now() - interval '14 days' GROUP BY node_id".to_string(),
    ))
    .all(db)
    .await?
    .into_iter()
    .map(|r| (r.node_id, r.v.clamp(0.0, 1.0)))
    .collect();

    let rows = InvRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT p.id product_id, p.sku, w.id warehouse_id, w.code wh, p.criticality_class, p.storage_class, p.unit_cost::float8 unit_cost, p.holding_cost_rate::float8 holding_cost_rate, \
           p.stockout_cost::float8 stockout_cost, p.review_period_days, p.lead_time_days, p.reorder_point, p.reorder_qty, \
           COALESCE((SELECT SUM(sl.quantity - sl.reserved - sl.blocked) FROM stock_levels sl JOIN locations l ON l.id = sl.location_id WHERE sl.product_id = p.id AND l.warehouse_id = w.id), 0)::float8 available, \
           COALESCE((SELECT SUM(pol.quantity_ordered - pol.quantity_received) FROM purchase_order_lines pol JOIN purchase_orders po ON po.id = pol.purchase_order_id \
              WHERE pol.product_id = p.id AND po.warehouse_id = w.id AND po.status IN ('submitted','partially_received')), 0)::float8 on_order, \
           COALESCE((SELECT SUM(-sm.quantity_delta) FROM stock_movements sm JOIN locations l ON l.id = sm.location_id WHERE sm.product_id = p.id AND l.warehouse_id = w.id \
              AND sm.movement_type = 'issue' AND sm.created_at::date > $1::date - 91), 0)::float8 issued91, \
           EXISTS(SELECT 1 FROM purchase_order_lines pol JOIN purchase_orders po ON po.id = pol.purchase_order_id WHERE pol.product_id = p.id AND po.warehouse_id = w.id AND po.status = 'draft') has_draft \
         FROM products p CROSS JOIN warehouses w WHERE p.active AND w.active AND ($2::uuid IS NULL OR w.id = $2) ORDER BY p.sku, w.code",
        [as_of.into(), req.warehouse_id.into()],
    ))
    .all(db)
    .await?;
    let cands = CandRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT ps.product_id, ps.supplier_id, s.code, COALESCE(ps.unit_cost, p.unit_cost)::float8 unit_cost, COALESCE(ps.lead_time_days, p.lead_time_days) lead_days, \
           ps.min_order_qty, ps.order_multiple, \
           (SELECT c.id FROM contracts c WHERE c.supplier_id = ps.supplier_id AND c.start_date <= $1::date AND c.end_date >= $1::date ORDER BY c.end_date DESC LIMIT 1) contract_id \
         FROM product_suppliers ps JOIN suppliers s ON s.id = ps.supplier_id JOIN products p ON p.id = ps.product_id WHERE ps.active AND s.active".to_string(),
        [as_of.into()],
    ))
    .all(db)
    .await?;
    let mut cand_by: HashMap<Uuid, Vec<&CandRow>> = HashMap::new();
    for c in &cands {
        cand_by.entry(c.product_id).or_default().push(c);
    }

    let mut by_prod: Vec<(Uuid, Vec<&InvRow>)> = Vec::new();
    for r in &rows {
        match by_prod.last_mut() {
            Some((p, v)) if *p == r.product_id => v.push(r),
            _ => by_prod.push((r.product_id, vec![r])),
        }
    }

    let (mut items, mut order_lines, mut transfers, mut policy_changes, mut no_source) = (Vec::new(), Vec::<(Uuid, Uuid, Value)>::new(), Vec::new(), Vec::new(), Vec::new());
    let mut prod_summary: Vec<(Uuid, f64, f64, f64)> = Vec::new(); // product, mu_week_total, position_total, lead_days
    for (pid, group) in &by_prod {
        let total_issued: f64 = group.iter().map(|r| r.issued91).sum();
        let (mu_total, sd_total, src) = match fz.idx.get(pid) {
            Some(&n) => fz.demand(n),
            None => (total_issued / 13.0, 0.5 * total_issued / 13.0, "history_mean"),
        };
        if mu_total <= 0.0 && group.iter().all(|r| r.available <= 0.0) {
            continue;
        }
        // supplier candidates with risk
        let cs: Vec<&&CandRow> = cand_by.get(pid).map(|v| v.iter().collect()).unwrap_or_default();
        let mut cand_in: Vec<(Cand, &CandRow, (f64, f64, f64))> = Vec::new();
        for c in cs {
            let (p_late, defect, exp_delay) = fz.idx.get(&c.supplier_id).map(|&n| fz.supplier_signal(n)).unwrap_or((0.0, 0.0, 0.0));
            let mut risk = 1.0 - (1.0 - p_late) * (1.0 - (5.0 * defect).min(1.0));
            if let Some(s) = sig.get(&c.supplier_id) {
                risk = risk.max(*s);
            }
            cand_in.push((Cand { supplier: cand_in.len(), unit_cost: c.unit_cost, lead_days: c.lead_days as f64 + exp_delay, risk }, *c, (p_late, defect, exp_delay)));
        }
        let lead_extra = if cand_in.is_empty() { 0.0 } else { cand_in.iter().map(|c| c.2 .2).sum::<f64>() / cand_in.len() as f64 };
        let lead_sd = cand_in.iter().filter_map(|c| fz.idx.get(&c.1.supplier_id)).map(|&n| mean_sd(&fz.recent(n, 1, 26)).1).fold(0.0, f64::max);
        let mut pos_total = 0.0;
        let mut pos_list: Vec<WhPos> = Vec::new();
        let mut wh_ids: Vec<(Uuid, String)> = Vec::new();
        for r in group {
            let share = if total_issued > 0.0 { r.issued91 / total_issued } else { 1.0 / group.len() as f64 };
            let (mu, sd) = (mu_total * share, sd_total * share.sqrt());
            let lead = r.lead_time_days as f64 + lead_extra;
            let sl = service_level(&r.criticality_class, r.stockout_cost, r.unit_cost, r.holding_cost_rate, lead + r.review_period_days as f64);
            let o = optimize::inventory_policy(&InvIn { mu_week: mu, sd_week: sd, lead_days: lead, lead_sd_days: lead_sd, review_days: r.review_period_days as f64, service_level: sl, z_mult, available: r.available, on_order: r.on_order });
            pos_total += o.position;
            pos_list.push(WhPos { wh: pos_list.len(), available: r.available, mu_day: mu / 7.0 });
            wh_ids.push((r.warehouse_id, r.wh.clone()));
            let excess = o.cover_days.map(|c| c > 120.0).unwrap_or(false) && o.position > 2.0 * o.order_up_to;
            let flag = if o.stockout_prob_in_lead_time > 0.2 { "stockout_risk" } else if excess { "excess_inventory" } else { "ok" };
            let mut item = json!({"sku": r.sku, "warehouse": r.wh, "demand_week": mu, "demand_sd_week": sd, "demand_source": src, "lead_days_adjusted": lead,
                "service_level": sl, "policy": o, "flag": flag, "current_reorder_point": r.reorder_point});
            // order proposal
            if o.order_qty >= 1.0 && !r.has_draft {
                if cand_in.is_empty() {
                    no_source.push(json!({"sku": r.sku, "warehouse": r.wh, "needed_qty": o.order_qty.ceil()}));
                } else {
                    let ok: Vec<usize> = (0..cand_in.len()).filter(|i| cand_in[*i].0.risk <= max_risk).collect();
                    let pool: Vec<usize> = if ok.is_empty() { (0..cand_in.len()).collect() } else { ok };
                    let pc: Vec<Cand> = pool.iter().enumerate().map(|(k, i)| Cand { supplier: k, ..cand_in[*i].0.clone() }).collect();
                    let alloc = optimize::allocate(o.order_qty.ceil() as i64, &pc, risk_pen, max_share, r.unit_cost * r.holding_cost_rate / 365.0);
                    let mut parts = Vec::new();
                    for (k, q) in alloc {
                        let c = &cand_in[pool[k]];
                        let q = apply_order_rules(q, c.1.min_order_qty, c.1.order_multiple);
                        parts.push(q);
                        order_lines.push((c.1.supplier_id, r.warehouse_id, json!({"product_id": pid, "sku": r.sku, "quantity": q, "unit_cost": c.1.unit_cost,
                            "supplier": c.1.code, "contract_id": c.1.contract_id, "lead_days": c.1.lead_days, "supplier_risk": c.0.risk, "p_late": c.2 .0, "defect_rate": c.2 .1})));
                    }
                    item["allocation_hhi"] = json!(optimize::hhi(&parts));
                }
            }
            items.push(item);
        }
        prod_summary.push((*pid, mu_total, pos_total, group[0].lead_time_days as f64 + lead_extra));
        // warehouse balancing
        if group.len() >= 2 {
            for (from, to, q) in optimize::balance(&pos_list, 14.0, 90.0, 30.0) {
                transfers.push((*pid, group[0].sku.clone(), group[0].storage_class.clone(), wh_ids[from].clone(), wh_ids[to].clone(), q));
            }
        }
        // policy suggestion (sum over warehouses)
        let cur_rop: i64 = group.iter().map(|r| r.reorder_point).sum();
        let new_rop: f64 = items.iter().rev().take(group.len()).map(|i| i["policy"]["reorder_point"].as_f64().unwrap_or(0.0)).sum();
        if cur_rop > 0 && new_rop > 0.0 && ((new_rop - cur_rop as f64).abs() / cur_rop as f64) > 0.15 {
            policy_changes.push((*pid, group[0].sku.clone(), cur_rop, new_rop.ceil() as i64, group[0].reorder_qty));
        }
    }

    // ---- group order lines into purchase orders
    let mut po_map: HashMap<(Uuid, Uuid), Vec<Value>> = HashMap::new();
    for (s, w, l) in order_lines {
        po_map.entry((s, w)).or_default().push(l);
    }
    let mut pos: Vec<Value> = Vec::new();
    for ((s, w), lines) in po_map {
        let value: f64 = lines.iter().map(|l| l["quantity"].as_f64().unwrap_or(0.0) * l["unit_cost"].as_f64().unwrap_or(0.0)).sum();
        let lead = lines.iter().map(|l| l["lead_days"].as_i64().unwrap_or(0)).max().unwrap_or(0);
        pos.push(json!({"supplier_id": s, "warehouse_id": w, "lines": lines, "value": value, "lead_days": lead, "submit": submit,
                        "contract_id": lines.first().map(|l| l["contract_id"].clone())}));
    }
    pos.sort_by(|a, b| b["value"].as_f64().partial_cmp(&a["value"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));

    // ---- transfers: pick lot and receiving location
    let mut transfer_payloads = Vec::new();
    for (pid, sku, storage, (fw, fcode), (tw, tcode), q) in transfers {
        let lot = LotRow::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT sl.location_id, sl.lot_number, (sl.quantity - sl.reserved - sl.blocked)::bigint avail FROM stock_levels sl JOIN locations l ON l.id = sl.location_id \
             WHERE sl.product_id = $1 AND l.warehouse_id = $2 AND sl.quantity - sl.reserved - sl.blocked > 0 ORDER BY 3 DESC LIMIT 1",
            [pid.into(), fw.into()],
        ))
        .one(db)
        .await?;
        let dest = IdRow::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT id FROM locations WHERE warehouse_id = $1 AND active AND storage_class = $2 ORDER BY code LIMIT 1",
            [tw.into(), storage.into()],
        ))
        .one(db)
        .await?;
        if let (Some(l), Some(d)) = (lot, dest) {
            let qty = q.min(l.avail);
            if qty > 0 {
                transfer_payloads.push(json!({"product_id": pid, "sku": sku, "from_warehouse": fcode, "to_warehouse": tcode, "from_location_id": l.location_id, "to_location_id": d.id, "lot_number": l.lot_number, "quantity": qty}));
            }
        }
    }

    // ---- carriers
    let late_pen = req.late_penalty.unwrap_or(50.0);
    let costs: HashMap<Uuid, f64> = CarrierCost::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT carrier_id, AVG(freight_cost)::float8 avg_freight FROM shipments WHERE carrier_id IS NOT NULL AND created_at::date > $1::date - 365 GROUP BY 1",
        [as_of.into()],
    ))
    .all(db)
    .await?
    .into_iter()
    .map(|c| (c.carrier_id, c.avg_freight))
    .collect();
    let mut carriers: Vec<Value> = ctx
        .graph
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.kind == Kind::Carrier)
        .map(|(i, n)| {
            let (p_late, _, exp_delay) = fz.supplier_signal(i);
            let exc = mean_sd(&fz.recent(i, 2, 13)).0 / 100.0;
            let freight = costs.get(&n.id).copied().unwrap_or(0.0);
            let risk = sig.get(&n.id).copied().unwrap_or(0.0);
            let p = (1.0 - (1.0 - p_late) * (1.0 - exc) * (1.0 - risk)).clamp(0.0, 1.0);
            json!({"carrier": n.code, "avg_freight": freight, "p_late": p_late, "exception_rate": exc, "expected_delay_days": exp_delay, "expected_cost": freight + p * late_pen})
        })
        .collect();
    carriers.sort_by(|a, b| a["expected_cost"].as_f64().partial_cmp(&b["expected_cost"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));

    // ---- production priorities
    let runs: HashMap<Uuid, (f64, f64)> = ProdRun::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT product_id, (SUM(quantity_good) / NULLIF(SUM(hours), 0))::float8 uph, (SUM(quantity_scrap)::float8 / NULLIF(SUM(quantity_started), 0))::float8 scrap \
         FROM production_runs WHERE produced_on > $1::date - 365 AND hours > 0 GROUP BY 1",
        [as_of.into()],
    ))
    .all(db)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|r| (r.product_id, (r.uph, r.scrap)))
    .collect();
    let mut production: Vec<Value> = prod_summary
        .iter()
        .filter_map(|(pid, mu, pos, lead)| {
            let (uph, scrap) = runs.get(pid)?;
            let need = (mu * (lead / 7.0 + 1.0) - pos).max(0.0);
            (need > 0.0 && *uph > 0.0).then(|| json!({"product_id": pid, "start_quantity": (need / (1.0 - scrap.clamp(0.0, 0.9))).ceil(), "hours": need / (1.0 - scrap.clamp(0.0, 0.9)) / uph, "scrap_rate": scrap, "cover_days": pos / (mu / 7.0).max(1e-9)}))
        })
        .collect();
    production.sort_by(|a, b| a["cover_days"].as_f64().partial_cmp(&b["cover_days"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));

    // ---- action log
    let create = req.create_actions.unwrap_or(true);
    let mut actions: Vec<Value> = Vec::new();
    if create {
        // a new plan replaces proposals nobody has acted on yet
        db.execute(Statement::from_string(DbBackend::Postgres, "UPDATE cognitive_actions SET status = 'rejected', decided_at = now(), result = '{\"superseded\": true}'::jsonb WHERE status IN ('proposed','blocked')".to_string())).await?;
        for p in &pos {
            let mut blocked = None;
            if p["value"].as_f64().unwrap_or(0.0) > max_value {
                blocked = Some(format!("order value {:.2} exceeds max_order_value {:.2}", p["value"].as_f64().unwrap_or(0.0), max_value));
            }
            let (id, st) = record_action(state, "create_po", p.clone(), json!({"model": status, "reason": "inventory position at or below reorder point; risk-aware allocation"}), model_id, blocked).await?;
            actions.push(json!({"action_id": id, "kind": "create_po", "status": st, "value": p["value"]}));
        }
        let (_, tp) = policy(db, "transfer_stock").await?;
        let max_units = pf(&tp, "max_units", 5000.0) as i64;
        for t in &transfer_payloads {
            let blocked = (t["quantity"].as_i64().unwrap_or(0) > max_units).then(|| format!("transfer exceeds max_units {max_units}"));
            let (id, st) = record_action(state, "transfer_stock", t.clone(), json!({"reason": "warehouse cover imbalance (donor > 90 days, receiver < 14 days)"}), model_id, blocked).await?;
            actions.push(json!({"action_id": id, "kind": "transfer_stock", "status": st}));
        }
        let (_, ap) = policy(db, "adjust_reorder_policy").await?;
        let maxc = pf(&ap, "max_change_pct", 50.0) / 100.0;
        for (pid, sku, cur, new, qty) in &policy_changes {
            let capped = (*new as f64).clamp(*cur as f64 * (1.0 - maxc), *cur as f64 * (1.0 + maxc)).round() as i64;
            let (id, st) = record_action(state, "adjust_reorder_policy", json!({"product_id": pid, "sku": sku, "reorder_point": capped, "reorder_qty": (*qty).max(1), "previous_reorder_point": cur, "unconstrained_reorder_point": new}), json!({"reason": "reorder point drifted >15% from forecast-based policy"}), model_id, None).await?;
            actions.push(json!({"action_id": id, "kind": "adjust_reorder_policy", "status": st}));
        }
    }
    let at_risk = items.iter().filter(|i| i["flag"] == "stockout_risk").count();
    let excess = items.iter().filter(|i| i["flag"] == "excess_inventory").count();
    let max_items = req.max_items.unwrap_or(50);
    items.sort_by(|a, b| b["policy"]["stockout_prob_in_lead_time"].as_f64().partial_cmp(&a["policy"]["stockout_prob_in_lead_time"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));
    items.truncate(max_items);
    Ok(json!({
        "as_of": as_of, "model": status, "autonomy": {"create_po": po_auto},
        "learning_policy": {"service_z_mult": z_mult, "risk_penalty": risk_pen},
        "summary": {"stockout_risk_items": at_risk, "excess_items": excess, "purchase_orders_proposed": pos.len(), "transfers_proposed": transfer_payloads.len(),
                    "reorder_policy_changes": policy_changes.len(), "products_without_approved_source": no_source.len()},
        "inventory": items, "purchase_orders": pos, "transfers": transfer_payloads, "carrier_ranking": carriers,
        "production_priority": production.into_iter().take(20).collect::<Vec<_>>(), "no_source": no_source, "actions": actions,
    }))
}

// ---- action log + guard-railed execution -------------------------------------------------------------------

pub async fn record_action(state: &AppState, kind: &str, payload: Value, rationale: Value, model_id: Option<Uuid>, blocked: Option<String>) -> AppResult<(Uuid, String)> {
    let id = Uuid::new_v4();
    let st = if blocked.is_some() { "blocked" } else { "proposed" };
    let mut rat = rationale;
    if let Some(b) = &blocked {
        rat["blocked_reason"] = json!(b);
    }
    state.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO cognitive_actions (id, kind, status, payload, rationale, model_id) VALUES ($1,$2,$3,$4,$5,$6)",
        [id.into(), kind.into(), st.into(), payload.into(), rat.into(), model_id.into()],
    ))
    .await?;
    let (autonomy, _) = policy(&state.db, kind).await?;
    if autonomy == "autonomous" && blocked.is_none() {
        let r = execute_action(state, id, true).await;
        return Ok((id, match r { Ok(_) => "executed".into(), Err(_) => "failed".into() }));
    }
    Ok((id, st.into()))
}

#[derive(FromQueryResult)]
struct ActRow {
    kind: String,
    status: String,
    payload: Value,
}

pub async fn execute_action(state: &AppState, id: Uuid, automatic: bool) -> AppResult<Value> {
    let db = &state.db;
    let a = ActRow::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres, "SELECT kind, status, payload FROM cognitive_actions WHERE id = $1", [id.into()]))
        .one(db)
        .await?
        .ok_or_else(|| AppError::not_found("action"))?;
    let (autonomy, params) = policy(db, &a.kind).await?;
    if autonomy == "advisory" {
        return Err(AppError::conflict(format!("policy for {} is 'advisory': raise it to 'supervised' to execute approved actions", a.kind)));
    }
    let ok_state = if automatic { a.status == "proposed" } else { matches!(a.status.as_str(), "approved") || (a.status == "proposed" && autonomy == "autonomous") };
    if !ok_state {
        return Err(AppError::conflict(format!("action is '{}': only approved actions can be executed (approve it first)", a.status)));
    }
    let res = run_action(db, &a.kind, &a.payload, &params).await;
    match res {
        Ok(v) => {
            db.execute(Statement::from_sql_and_values(DbBackend::Postgres, "UPDATE cognitive_actions SET status = 'executed', result = $2, executed_at = now() WHERE id = $1", [id.into(), v.clone().into()])).await?;
            Ok(v)
        }
        Err(e) => {
            db.execute(Statement::from_sql_and_values(DbBackend::Postgres, "UPDATE cognitive_actions SET status = 'failed', result = $2 WHERE id = $1", [id.into(), json!({"error": e.to_string()}).into()])).await?;
            Err(e)
        }
    }
}

async fn run_action(db: &DatabaseConnection, kind: &str, p: &Value, params: &Value) -> AppResult<Value> {
    let uid = |k: &str| p.get(k).and_then(|v| v.as_str()).and_then(|s| Uuid::parse_str(s).ok()).ok_or_else(|| AppError::bad(format!("payload missing {k}")));
    match kind {
        "create_po" => {
            let (sid, wid) = (uid("supplier_id")?, uid("warehouse_id")?);
            let lines = p["lines"].as_array().ok_or_else(|| AppError::bad("payload missing lines"))?;
            let value = p["value"].as_f64().unwrap_or(0.0);
            if value > pf(params, "max_order_value", 50000.0) {
                return Err(AppError::conflict("order value exceeds the guardrail"));
            }
            let active = IdRow::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres, "SELECT id FROM suppliers WHERE id = $1 AND active", [sid.into()])).one(db).await?;
            if active.is_none() {
                return Err(AppError::conflict("supplier is inactive"));
            }
            let id = Uuid::new_v4();
            let number = gen_number("PO");
            let today = Utc::now().date_naive();
            let lead = p["lead_days"].as_i64().unwrap_or(7);
            let status = if params.get("submit").and_then(|v| v.as_bool()).unwrap_or(false) { "submitted" } else { "draft" };
            let contract = p["contract_id"].as_str().and_then(|s| Uuid::parse_str(s).ok());
            let txn = db.begin().await?;
            txn.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO purchase_orders (id, po_number, supplier_id, warehouse_id, status, expected_date, notes, order_date, auto_generated, contract_id) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,TRUE,$9)",
                [id.into(), number.clone().into(), sid.into(), wid.into(), status.into(), (today + Duration::days(lead)).into(), "raised by the cognitive layer".into(), today.into(), contract.into()],
            ))
            .await?;
            for l in lines {
                let pid = l["product_id"].as_str().and_then(|s| Uuid::parse_str(s).ok()).ok_or_else(|| AppError::bad("line missing product_id"))?;
                let cost = Decimal::from_f64_retain(l["unit_cost"].as_f64().unwrap_or(0.0)).unwrap_or_default().round_dp(4);
                txn.execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "INSERT INTO purchase_order_lines (id, purchase_order_id, product_id, quantity_ordered, unit_cost, promised_date) VALUES ($1,$2,$3,$4,$5,$6)",
                    [Uuid::new_v4().into(), id.into(), pid.into(), l["quantity"].as_i64().unwrap_or(1).max(1).into(), cost.into(), (today + Duration::days(l["lead_days"].as_i64().unwrap_or(lead))).into()],
                ))
                .await?;
            }
            txn.commit().await?;
            Ok(json!({"purchase_order_id": id, "po_number": number, "status": status, "lines": lines.len()}))
        }
        "transfer_stock" => {
            let (pid, from, to) = (uid("product_id")?, uid("from_location_id")?, uid("to_location_id")?);
            let qty = p["quantity"].as_i64().unwrap_or(0);
            if qty <= 0 || qty > pf(params, "max_units", 5000.0) as i64 {
                return Err(AppError::conflict("transfer quantity outside the guardrail"));
            }
            let lot = p["lot_number"].as_str().unwrap_or("").to_string();
            let prod = product::Entity::find_by_id(pid).one(db).await?.ok_or_else(|| AppError::not_found("product"))?;
            let dest = location::Entity::find_by_id(to).one(db).await?.ok_or_else(|| AppError::not_found("destination location"))?;
            ensure_storage_compatible(&prod, &dest)?;
            let tid = Uuid::new_v4();
            let txn = db.begin().await?;
            if !remove_stock(&txn, pid, from, &lot, qty).await? {
                return Err(AppError::conflict("insufficient available stock at source (it may have been reserved since the plan)"));
            }
            let expiry = crate::supply_chain::entities::stock_level::Entity::find().filter_lot(pid, from, &lot, &txn).await?;
            add_stock(&txn, pid, to, &lot, qty, expiry).await?;
            record_movement(&txn, pid, from, &lot, "transfer_out", -qty, Some("transfer"), Some(tid), Some("cognitive balancing".into())).await?;
            record_movement(&txn, pid, to, &lot, "transfer_in", qty, Some("transfer"), Some(tid), Some("cognitive balancing".into())).await?;
            txn.commit().await?;
            Ok(json!({"transfer_id": tid, "quantity": qty}))
        }
        "adjust_reorder_policy" => {
            let pid = uid("product_id")?;
            let (rop, rq) = (p["reorder_point"].as_i64().unwrap_or(0), p["reorder_qty"].as_i64().unwrap_or(0));
            let prev = p["previous_reorder_point"].as_i64().unwrap_or(0);
            let maxc = pf(params, "max_change_pct", 50.0) / 100.0;
            if rop < 0 || rq < 0 || (prev > 0 && ((rop - prev).abs() as f64 / prev as f64) > maxc + 1e-9) {
                return Err(AppError::conflict("reorder point change outside the guardrail"));
            }
            db.execute(Statement::from_sql_and_values(DbBackend::Postgres, "UPDATE products SET reorder_point = $2, reorder_qty = $3, updated_at = now() WHERE id = $1", [pid.into(), rop.into(), rq.into()])).await?;
            Ok(json!({"product_id": pid, "reorder_point": rop, "reorder_qty": rq}))
        }
        _ => Err(AppError::bad("unknown action kind")),
    }
}

trait LotExpiry {
    async fn filter_lot<C: ConnectionTrait>(self, pid: Uuid, loc: Uuid, lot: &str, db: &C) -> AppResult<Option<NaiveDate>>;
}
impl LotExpiry for sea_orm::Select<crate::supply_chain::entities::stock_level::Entity> {
    async fn filter_lot<C: ConnectionTrait>(self, pid: Uuid, loc: Uuid, lot: &str, db: &C) -> AppResult<Option<NaiveDate>> {
        use crate::supply_chain::entities::stock_level::Column as C_;
        use sea_orm::{ColumnTrait, QueryFilter};
        Ok(self.filter(C_::ProductId.eq(pid)).filter(C_::LocationId.eq(loc)).filter(C_::LotNumber.eq(lot)).one(db).await?.and_then(|l| l.expiry_date))
    }
}

pub async fn decide_action(db: &DatabaseConnection, id: Uuid, approve: bool) -> AppResult<Value> {
    let r = db
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE cognitive_actions SET status = $2, decided_at = now() WHERE id = $1 AND status = 'proposed'",
            [id.into(), (if approve { "approved" } else { "rejected" }).into()],
        ))
        .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::conflict("only proposed actions can be approved or rejected (blocked actions need a policy change)"));
    }
    Ok(json!({"action_id": id, "status": if approve { "approved" } else { "rejected" }}))
}

// ---- digital twin ------------------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct TwinReq {
    /// supplier_failure | port_congestion | demand_surge | warehouse_saturation | lead_time
    pub scenario: String,
    pub supplier: Option<String>,
    pub carrier: Option<String>,
    pub product: Option<String>,
    pub warehouse: Option<String>,
    pub pct: Option<f64>,
    pub factor: Option<f64>,
    pub days: Option<f64>,
    pub level: Option<f64>,
    /// how many weeks the shock lasts (default 4)
    pub duration_weeks: Option<usize>,
    pub horizon_weeks: Option<usize>,
    pub as_of: Option<NaiveDate>,
}

pub async fn twin(state: &AppState, r: TwinReq) -> AppResult<Value> {
    let as_of = r.as_of.unwrap_or_else(|| Utc::now().date_naive());
    let ctx = load_context(&state.db, as_of, 104, 365).await?;
    let (_, model) = engine::require_model(&state.db).await?;
    let (sess, y0) = engine::replay(&model, &ctx)?;
    let horizon = r.horizon_weeks.unwrap_or(8).clamp(2, 26);
    let dur = r.duration_weeks.unwrap_or(4).clamp(1, horizon);
    let find = |code: &str, kind: Kind| ctx.graph.nodes.iter().position(|n| n.kind == kind && n.code.eq_ignore_ascii_case(code.trim())).ok_or_else(|| AppError::not_found(&format!("{} {code}", kind.label().to_lowercase())));
    let of_kind = |k: Kind| -> Vec<usize> { ctx.graph.nodes.iter().enumerate().filter(|(_, n)| n.kind == k).map(|(i, _)| i).collect() };
    let mut shocks: Vec<Shock> = Vec::new();
    let sh = |node, channel, op, value| Shock { node, channel, op, value, from: 0, dur };
    let mut mech: Value = Value::Null;
    let description;
    match r.scenario.as_str() {
        "supplier_failure" => {
            let code = r.supplier.clone().ok_or_else(|| AppError::bad("supplier (code) is required"))?;
            let s = find(&code, Kind::Supplier)?;
            shocks.push(sh(s, 0, Op::Set, 0.0));
            shocks.push(sh(s, 1, Op::Add, 21.0));
            description = format!("supplier {code} delivers nothing and is 21 days late for {dur} weeks");
            let dr = crate::supply_chain::routes::resilience::DisruptionReq { supplier_codes: vec![code], as_of: Some(as_of), lookback_days: None, horizon_days: Some(dur as f64 * 7.0), max_uplift: None, standby_share: None, qualification_days: None, top: Some(10) };
            mech = crate::supply_chain::routes::resilience::disruption(axum::extract::State(state.clone()), axum::Json(dr)).await.map(|j| j.0).unwrap_or(Value::Null);
        }
        "port_congestion" => {
            let pct = r.pct.unwrap_or(30.0);
            let cs = match &r.carrier { Some(c) => vec![find(c, Kind::Carrier)?], None => of_kind(Kind::Carrier) };
            for c in cs {
                shocks.push(sh(c, 1, Op::Add, pct / 100.0 * 10.0));
                shocks.push(sh(c, 3, Op::Mul, 1.0 + pct / 100.0));
            }
            description = format!("congestion of {pct}% adds {:.1} days to carrier delay and scales in-transit load by {:.2} for {dur} weeks", pct / 10.0, 1.0 + pct / 100.0);
        }
        "demand_surge" => {
            let f = r.factor.unwrap_or(2.0);
            let ns = match &r.product { Some(c) => vec![ctx.graph.nodes.iter().position(|n| n.kind == Kind::Product && n.code.eq_ignore_ascii_case(c.trim())).ok_or_else(|| AppError::not_found("product"))?], None => of_kind(Kind::Product) };
            for n in ns {
                shocks.push(sh(n, 0, Op::Mul, f));
            }
            description = format!("demand multiplied by {f} for {dur} weeks");
        }
        "warehouse_saturation" => {
            let lvl = r.level.unwrap_or(0.95);
            let ws = match &r.warehouse { Some(c) => vec![find(c, Kind::Warehouse)?], None => of_kind(Kind::Warehouse) };
            for w in ws {
                shocks.push(sh(w, 3, Op::Set, lvl));
            }
            description = format!("warehouse utilisation held at {:.0}% for {dur} weeks", lvl * 100.0);
        }
        "lead_time" => {
            let days = r.days.unwrap_or(40.0);
            let ss = match &r.supplier { Some(c) => vec![find(c, Kind::Supplier)?], None => of_kind(Kind::Supplier) };
            for s in ss {
                shocks.push(sh(s, 1, Op::Set, days));
            }
            description = format!("supplier delay forced to {days} days for {dur} weeks");
            mech = json!({"note": "mechanistic effect: safety stock scales with sqrt(lead time + review); see /cognitive/optimize for per-SKU numbers", "lead_time_days_assumed": days});
        }
        _ => return Err(AppError::bad("scenario must be one of supplier_failure, port_congestion, demand_surge, warehouse_saturation, lead_time")),
    }
    let base = engine::free_run(&model, &sess, &y0, horizon, &[]);
    let scn = engine::free_run(&model, &sess, &y0, horizon, &shocks);
    let rr = ctx.graph.r();
    let mut node_scores: Vec<(usize, f64)> = Vec::new();
    let mut by_kind: HashMap<(Kind, usize), (f64, usize)> = HashMap::new();
    let mut deltas: Vec<[f64; NCH]> = vec![[0.0; NCH]; rr];
    for i in 0..rr {
        let mut score = 0.0;
        for c in 0..NCH {
            let (m, s) = (model.mean[i * NCH + c], model.sd[i * NCH + c]);
            let mut d = 0.0;
            for t in 0..horizon {
                let (a, b) = (base[t][i * NCH + c], scn[t][i * NCH + c]);
                d += b - a;
                score += (engine::to_z(c, b, m, s) - engine::to_z(c, a, m, s)).abs();
            }
            deltas[i][c] = d / horizon as f64;
            let e = by_kind.entry((ctx.graph.nodes[i].kind, c)).or_insert((0.0, 0));
            e.0 += deltas[i][c];
            e.1 += 1;
        }
        node_scores.push((i, score));
    }
    node_scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let top: Vec<Value> = node_scores.iter().take(10).filter(|(_, s)| *s > 1e-9).map(|(i, s)| {
        json!({"node": ctx.graph.nodes[*i].code, "kind": ctx.graph.nodes[*i].kind, "impact_score": s,
               "mean_change": {"volume": deltas[*i][0], "delay_days": deltas[*i][1], "defect_pct": deltas[*i][2], "pressure": deltas[*i][3]}})
    }).collect();
    let mut kinds_out = Vec::new();
    for k in Kind::ALL {
        let ch: Vec<Value> = (0..NCH).filter_map(|c| by_kind.get(&(k, c)).map(|(s, n)| json!({"channel": CHANNELS[c], "mean_change": s / *n as f64, "trusted_head": model.channel_ok(c)}))).collect();
        if !ch.is_empty() {
            kinds_out.push(json!({"kind": k, "channels": ch}));
        }
    }
    Ok(json!({
        "scenario": r.scenario, "description": description, "horizon_weeks": horizon, "shock_weeks": dur,
        "learned_propagation": {"note": "free-run of the trained CIR-GNN with the shock injected; deltas on channels whose head is not trusted are indicative only",
                                 "by_node_kind": kinds_out, "most_affected_nodes": top},
        "mechanistic": mech,
    }))
}

// ---- continuous learning -----------------------------------------------------------------------------------

#[derive(FromQueryResult, Clone)]
struct PredRow {
    id: Uuid,
    node_id: Uuid,
    channel: String,
    horizon: i32,
    target_week: NaiveDate,
    predicted: f64,
    prob: Option<f64>,
}

fn raw_at(ctx: &Context, i: usize, c: usize, k: usize) -> Option<f64> {
    let r = ctx.graph.r();
    let j = (k * r + i) * NCH + c;
    (ctx.obs[j] > 0.0).then(|| if c == 0 { ctx.values[j].exp_m1() } else { ctx.values[j] })
}

pub async fn reconcile(db: &DatabaseConnection, ctx: &Context) -> AppResult<Value> {
    let idx = ctx.graph.index();
    let today_week = monday(Utc::now().date_naive());
    let open = PredRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT id, node_id, channel, horizon, target_week, predicted, prob FROM cognitive_predictions WHERE actual IS NULL AND target_week < $1 AND target_week >= $2",
        [today_week.into(), ctx.base.into()],
    ))
    .all(db)
    .await?;
    let mut closed = 0;
    for p in &open {
        let (Some(&i), Some(c)) = (idx.get(&p.node_id), CHANNELS.iter().position(|c| *c == p.channel)) else { continue };
        let k = ((p.target_week - ctx.base).num_days() / 7) as usize;
        if k >= ctx.weeks {
            continue;
        }
        if let Some(a) = raw_at(ctx, i, c, k) {
            db.execute(Statement::from_sql_and_values(DbBackend::Postgres, "UPDATE cognitive_predictions SET actual = $2, error = $3, reconciled_at = now() WHERE id = $1", [p.id.into(), a.into(), (p.predicted - a).into()])).await?;
            closed += 1;
        }
    }
    // scoring over all reconciled predictions inside the loaded window
    let done = PredRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT id, node_id, channel, horizon, target_week, predicted, prob FROM cognitive_predictions WHERE actual IS NOT NULL AND target_week >= $1",
        [ctx.base.into()],
    ))
    .all(db)
    .await?;
    let (mut abs_err, mut abs_naive, mut sum_act, mut sum_err, mut nvol) = (0.0, 0.0, 0.0, 0.0, 0usize);
    let (mut lt_abs, mut nlt) = (0.0, 0usize);
    let (mut q_abs, mut nq) = (0.0, 0usize);
    let (mut brier, mut base_rate, mut nb, mut late_n) = (0.0, 0.0, 0usize, 0.0);
    for p in &done {
        let (Some(&i), Some(c)) = (idx.get(&p.node_id), CHANNELS.iter().position(|c| *c == p.channel)) else { continue };
        let k = ((p.target_week - ctx.base).num_days() / 7) as usize;
        let Some(a) = raw_at(ctx, i, c, k) else { continue };
        match c {
            0 => {
                let l = k.checked_sub(p.horizon as usize).and_then(|l| raw_at(ctx, i, 0, l)).unwrap_or(a);
                abs_err += (p.predicted - a).abs();
                abs_naive += (l - a).abs();
                sum_act += a;
                sum_err += p.predicted - a;
                nvol += 1;
            }
            1 => {
                lt_abs += (p.predicted - a).abs();
                nlt += 1;
                if let Some(pr) = p.prob {
                    let o = if a > LATE_DAYS { 1.0 } else { 0.0 };
                    brier += (pr - o).powi(2);
                    late_n += o;
                    nb += 1;
                }
            }
            2 => {
                q_abs += (p.predicted - a).abs();
                nq += 1;
            }
            _ => {}
        }
    }
    if nb > 0 {
        base_rate = late_n / nb as f64;
    }
    let base_brier = if nb > 0 { base_rate * (1.0 - base_rate) } else { 0.0 };
    #[derive(FromQueryResult)]
    struct Cost {
        mape: Option<f64>,
        n: i64,
    }
    let cost = Cost::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT AVG(ABS(pol.unit_cost - p.unit_cost) / NULLIF(p.unit_cost, 0))::float8 mape, COUNT(*)::bigint n FROM purchase_order_lines pol \
         JOIN purchase_orders po ON po.id = pol.purchase_order_id JOIN products p ON p.id = pol.product_id WHERE po.status NOT IN ('draft','cancelled') AND po.order_date > $1::date - 90",
        [ctx.as_of.into()],
    ))
    .one(db)
    .await?;
    Ok(json!({
        "newly_reconciled": closed, "scored_predictions": done.len(),
        "forecast_error": {"n": nvol, "wape": (sum_act > 0.0).then(|| abs_err / sum_act), "naive_wape": (sum_act > 0.0).then(|| abs_naive / sum_act), "bias": (sum_act > 0.0).then(|| sum_err / sum_act)},
        "lead_time_error": {"n": nlt, "mae_days": (nlt > 0).then(|| lt_abs / nlt as f64)},
        "risk_model_error": {"n": nb, "brier": (nb > 0).then_some(brier / nb.max(1) as f64), "base_rate_brier": (nb > 0).then_some(base_brier)},
        "supplier_model_error": {"n": nq, "defect_mae_pct": (nq > 0).then(|| q_abs / nq as f64)},
        "cost_model_error": {"n": cost.as_ref().map(|c| c.n), "price_vs_standard_mape": cost.and_then(|c| c.mape)},
    }))
}

pub async fn learning_step(state: &AppState, force: bool, as_of: Option<NaiveDate>) -> AppResult<Value> {
    let db = &state.db;
    let as_of = as_of.unwrap_or_else(|| Utc::now().date_naive());
    let ctx = Arc::new(load_context(db, as_of, 104, 365).await?);
    let metrics = reconcile(db, &ctx).await?;
    let current = engine::load_active(db).await?;
    let mut reasons: Vec<String> = Vec::new();
    match &current {
        None => reasons.push("no model yet".into()),
        Some((_, t)) => {
            if t.fingerprint != ctx.graph.fingerprint() {
                reasons.push("supply network changed".into());
            }
            if (as_of - t.as_of).num_days() > 84 {
                reasons.push("model older than 12 weeks".into());
            }
            let fe = &metrics["forecast_error"];
            if fe["n"].as_u64().unwrap_or(0) >= 30 {
                if let (Some(w), Some(nv)) = (fe["wape"].as_f64(), fe["naive_wape"].as_f64()) {
                    if w > nv * 1.05 {
                        reasons.push(format!("realised WAPE {w:.3} is worse than last-week-naive {nv:.3}"));
                    }
                }
            }
        }
    }
    if force {
        reasons.push("forced".into());
    }
    let mut out = json!({"metrics": metrics, "retrain_reasons": reasons});
    let mut model_id = current.as_ref().map(|c| c.0);
    if !reasons.is_empty() {
        let warm = current.map(|c| c.1);
        let (t, rep) = train_async(ctx.clone(), FitConfig::default(), warm).await?;
        let id = engine::save_model(db, &t, &rep).await?;
        let (sess, y) = engine::replay(&t, &ctx)?;
        engine::store_node_state(db, id, &t, &ctx, &sess).await?;
        engine::store_predictions(db, id, &engine::forecasts(&t, &ctx, &y), &t.nodes).await?;
        out["retrained"] = json!({"model_id": id, "trusted_channels": (0..NCH).filter(|c| t.channel_ok(*c)).map(|c| CHANNELS[c]).collect::<Vec<_>>(), "holdout_weeks": rep.holdout_weeks, "seconds": rep.seconds});
        model_id = Some(id);
    }
    // policy feedback: under-forecasting raises safety stock; an unreliable risk model is discounted
    let (autonomy, mut p) = policy(db, "learning").await?;
    let (mut zm, mut rp) = (pf(&p, "service_z_mult", 1.0), pf(&p, "risk_penalty", 1.0));
    let fe = &out["metrics"]["forecast_error"];
    if fe["n"].as_u64().unwrap_or(0) >= 30 {
        if let Some(b) = fe["bias"].as_f64() {
            if b < -0.05 { zm = (zm * 1.05).min(1.5) } else if b > 0.05 { zm = (zm * 0.97).max(0.8) }
        }
    }
    let rm = &out["metrics"]["risk_model_error"];
    if rm["n"].as_u64().unwrap_or(0) >= 30 {
        if let (Some(b), Some(bb)) = (rm["brier"].as_f64(), rm["base_rate_brier"].as_f64()) {
            if b > bb { rp = (rp * 0.9).max(0.2) } else { rp = (rp * 1.05).min(3.0) }
        }
    }
    let proposed = json!({"service_z_mult": zm, "risk_penalty": rp});
    let applied = autonomy != "advisory";
    if applied {
        p["service_z_mult"] = json!(zm);
        p["risk_penalty"] = json!(rp);
        db.execute(Statement::from_sql_and_values(DbBackend::Postgres, "UPDATE cognitive_policies SET params = $1, updated_at = now() WHERE action_kind = 'learning'", [p.into()])).await?;
    }
    out["policy_update"] = json!({"proposed": proposed, "applied": applied, "note": "set the 'learning' policy to supervised or autonomous to apply updates automatically"});
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO cognitive_learning_events (id, model_id, metrics, decision) VALUES ($1,$2,$3,$4)",
        [Uuid::new_v4().into(), model_id.into(), out.clone().into(), (if out.get("retrained").is_some() { "retrained" } else { "kept" }).into()],
    ))
    .await?;
    let _ = analytics::phi(0.0);
    Ok(out)
}
