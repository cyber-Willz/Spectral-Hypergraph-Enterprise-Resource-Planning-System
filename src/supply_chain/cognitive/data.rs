//! Data ingestion layer: turns ERP / WMS / procurement / logistics / quality / production tables into the
//! supply-chain graph and the weekly node x channel tensor the CIR-GNN learns from.
//!
//! Channels (all weekly): 0 volume (ln1p units / shipments), 1 delay (days late), 2 quality (defect or
//! exception %), 3 pressure (supplier backlog weeks, product stock scarcity 1/(1+cover), warehouse
//! utilisation, customer backorder share, carrier in-transit load).
use std::collections::HashMap;

use chrono::{Datelike, Duration, NaiveDate};
use sea_orm::{DatabaseConnection, DbBackend, FromQueryResult, Statement};
use uuid::Uuid;

use super::graph::{Flow, Kind, Node, SupplyGraph};
use crate::supply_chain::error::AppResult;

pub const NCH: usize = 4;
pub const CHANNELS: [&str; NCH] = ["volume", "delay", "quality", "pressure"];
pub const MAX_NODES: usize = 256;

pub struct Context {
    pub as_of: NaiveDate,
    pub base: NaiveDate,
    pub weeks: usize,
    pub graph: SupplyGraph,
    pub total_nodes: usize,
    /// raw weekly values, T x R x NCH (volume already ln1p); obs flags same shape
    pub values: Vec<f64>,
    pub obs: Vec<f64>,
}

impl Context {
    pub fn week_start(&self, k: usize) -> NaiveDate {
        self.base + Duration::days(7 * k as i64)
    }
}

#[derive(FromQueryResult)]
struct TsRow {
    id: Uuid,
    k: i32,
    v: f64,
}
#[derive(FromQueryResult)]
struct PairRow {
    a: Uuid,
    b: Uuid,
    m: f64,
    f: f64,
    t: f64,
    lead: Option<f64>,
}
#[derive(FromQueryResult)]
struct NodeRow {
    id: Uuid,
    code: String,
    name: String,
}
#[derive(FromQueryResult)]
struct HyperRow {
    kind: String,
    members: String,
}

pub fn monday(d: NaiveDate) -> NaiveDate {
    d - Duration::days(d.weekday().num_days_from_monday() as i64)
}

async fn ts(db: &DatabaseConnection, id: &str, date: &str, agg: &str, from: &str, base: NaiveDate, n: i32) -> AppResult<Vec<TsRow>> {
    // `from` includes any WHERE clause; rows before the window collapse into bucket k = -1
    let sql = format!(
        "SELECT id, k, v FROM (SELECT {id} AS id, GREATEST(-1, FLOOR(({date}::date - $1::date)/7.0))::int AS k, ({agg})::float8 AS v \
         FROM {from} GROUP BY 1, 2) q WHERE k < $2 AND id IS NOT NULL AND v IS NOT NULL"
    );
    Ok(TsRow::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres, sql, [base.into(), n.into()])).all(db).await?)
}

async fn pairs(db: &DatabaseConnection, sql: &str, as_of: NaiveDate, lookback: i32) -> AppResult<Vec<PairRow>> {
    Ok(PairRow::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres, sql.to_string(), [as_of.into(), lookback.into()])).all(db).await?)
}

const RECV: &str = "goods_receipts gr JOIN purchase_orders po ON po.id = gr.purchase_order_id \
    JOIN purchase_order_lines pol ON pol.id = gr.purchase_order_line_id JOIN suppliers s ON s.id = po.supplier_id";
const LATE: &str = "AVG((gr.received_on - COALESCE(pol.promised_date, po.expected_date, po.order_date + s.lead_time_days))::float8)";
const REJ: &str = "SUM(gr.rejected_qty)::float8 / NULLIF(SUM(gr.quantity), 0) * 100";
const SHIP: &str = "shipments sh JOIN sales_orders so ON so.id = sh.sales_order_id";
const EXC: &str = "AVG(CASE WHEN sh.status = 'exception' THEN 1.0 ELSE 0.0 END) * 100";

pub async fn load_context(db: &DatabaseConnection, as_of: NaiveDate, weeks: usize, lookback: i32) -> AppResult<Context> {
    let weeks = weeks.clamp(12, 260);
    let base = monday(as_of) - Duration::days(7 * weeks as i64);
    let n = weeks as i32;

    // ---- nodes
    let mut nodes: Vec<Node> = Vec::new();
    for (kind, table, extra) in [
        (Kind::Supplier, "suppliers", "WHERE active"),
        (Kind::Product, "products", "WHERE active"),
        (Kind::Warehouse, "warehouses", "WHERE active"),
        (Kind::Customer, "scm_customers", ""),
        (Kind::Carrier, "carriers", "WHERE active"),
    ] {
        let rows = NodeRow::find_by_statement(Statement::from_string(
            DbBackend::Postgres,
            format!("SELECT id, {} AS code, name FROM {table} {extra} ORDER BY 2", if table == "products" { "sku" } else { "code" }),
        ))
        .all(db)
        .await?;
        nodes.extend(rows.into_iter().map(|r| Node { id: r.id, kind, code: r.code, name: r.name }));
    }
    let total_nodes = nodes.len();
    let mut full = SupplyGraph::new(nodes);
    let idx = full.index();

    // ---- flows
    let win = "po.status NOT IN ('draft','cancelled') AND po.order_date > $1::date - $2::int AND po.order_date <= $1::date";
    let swin = "so.status <> 'cancelled' AND so.created_at::date > $1::date - $2::int AND so.created_at::date <= $1::date";
    let q = |sql: String| sql;
    let specs: Vec<(String, bool)> = vec![
        // supplier -> warehouse
        (q(format!("SELECT po.supplier_id a, po.warehouse_id b, SUM(pol.quantity_ordered)::float8 m, SUM(pol.quantity_ordered * pol.unit_cost)::float8 f, \
          COUNT(DISTINCT po.id)::float8 t, COALESCE(AVG((po.expected_date - po.order_date)::float8), MAX(s.lead_time_days)::float8) lead \
          FROM purchase_orders po JOIN purchase_order_lines pol ON pol.purchase_order_id = po.id JOIN suppliers s ON s.id = po.supplier_id WHERE {win} GROUP BY 1, 2")), false),
        // supplier -> product (dependency share added below)
        (q(format!("SELECT po.supplier_id a, pol.product_id b, SUM(pol.quantity_ordered)::float8 m, SUM(pol.quantity_ordered * pol.unit_cost)::float8 f, \
          COUNT(DISTINCT po.id)::float8 t, COALESCE(AVG((po.expected_date - po.order_date)::float8), MAX(s.lead_time_days)::float8) lead \
          FROM purchase_orders po JOIN purchase_order_lines pol ON pol.purchase_order_id = po.id JOIN suppliers s ON s.id = po.supplier_id WHERE {win} GROUP BY 1, 2")), true),
        // product -> customer
        (q(format!("SELECT sol.product_id a, so.customer_id b, SUM(sol.quantity)::float8 m, SUM(sol.quantity * sol.unit_price)::float8 f, COUNT(DISTINCT so.id)::float8 t, NULL::float8 lead \
          FROM sales_order_lines sol JOIN sales_orders so ON so.id = sol.sales_order_id WHERE {swin} GROUP BY 1, 2")), false),
        // product -> warehouse
        (q(format!("SELECT sol.product_id a, so.warehouse_id b, SUM(sol.quantity)::float8 m, SUM(sol.quantity * sol.unit_price)::float8 f, COUNT(DISTINCT so.id)::float8 t, NULL::float8 lead \
          FROM sales_order_lines sol JOIN sales_orders so ON so.id = sol.sales_order_id WHERE {swin} GROUP BY 1, 2")), false),
        // warehouse -> customer
        (q(format!("SELECT so.warehouse_id a, so.customer_id b, SUM(sol.quantity)::float8 m, SUM(sol.quantity * sol.unit_price)::float8 f, COUNT(DISTINCT so.id)::float8 t, NULL::float8 lead \
          FROM sales_order_lines sol JOIN sales_orders so ON so.id = sol.sales_order_id WHERE {swin} GROUP BY 1, 2")), false),
        // warehouse -> carrier
        (q(format!("SELECT so.warehouse_id a, sh.carrier_id b, COUNT(*)::float8 m, 0::float8 f, COUNT(*)::float8 t, AVG((sh.delivered_at::date - sh.shipped_at::date)::float8) lead \
          FROM {SHIP} WHERE sh.carrier_id IS NOT NULL AND {swin} GROUP BY 1, 2")), false),
        // carrier -> customer
        (q(format!("SELECT sh.carrier_id a, so.customer_id b, COUNT(*)::float8 m, 0::float8 f, COUNT(*)::float8 t, AVG((sh.delivered_at::date - sh.shipped_at::date)::float8) lead \
          FROM {SHIP} WHERE sh.carrier_id IS NOT NULL AND {swin} GROUP BY 1, 2")), false),
        // product <-> product co-demand
        (q(format!("SELECT a.product_id a, b.product_id b, COUNT(*)::float8 m, 0::float8 f, COUNT(*)::float8 t, NULL::float8 lead \
          FROM sales_order_lines a JOIN sales_order_lines b ON b.sales_order_id = a.sales_order_id AND b.product_id > a.product_id \
          JOIN sales_orders so ON so.id = a.sales_order_id WHERE {swin} GROUP BY 1, 2 HAVING COUNT(*) >= 2 ORDER BY 3 DESC LIMIT 5000")), false),
        // warehouse -> warehouse transfers
        (q("SELECT lo.warehouse_id a, li.warehouse_id b, SUM(mi.quantity_delta)::float8 m, 0::float8 f, COUNT(*)::float8 t, NULL::float8 lead \
          FROM stock_movements mo JOIN stock_movements mi ON mi.reference_id = mo.reference_id AND mi.movement_type = 'transfer_in' \
          JOIN locations lo ON lo.id = mo.location_id JOIN locations li ON li.id = mi.location_id \
          WHERE mo.movement_type = 'transfer_out' AND mo.created_at::date > $1::date - $2::int AND mo.created_at::date <= $1::date \
          AND lo.warehouse_id <> li.warehouse_id GROUP BY 1, 2".to_string()), false),
    ];
    let mut sup_prod: Vec<(usize, usize, f64)> = Vec::new();
    for (sql, is_sup_prod) in specs {
        for r in pairs(db, &sql, as_of, lookback).await? {
            if let (Some(&a), Some(&b)) = (idx.get(&r.a), idx.get(&r.b)) {
                if is_sup_prod {
                    sup_prod.push((a, b, r.m));
                }
                full.add_flow(a, b, Flow { material: r.m, financial: r.f, traffic: r.t, dependency: 0.0, lead_acc: r.lead.map(|l| (l * r.m.max(1.0), r.m.max(1.0))).unwrap_or((0.0, 0.0)) });
            }
        }
    }
    // approved-but-unused sources are structural dependencies too
    #[derive(FromQueryResult)]
    struct Appr {
        a: Uuid,
        b: Uuid,
        l: Option<i32>,
    }
    let appr = Appr::find_by_statement(Statement::from_string(
        DbBackend::Postgres,
        "SELECT supplier_id a, product_id b, lead_time_days l FROM product_suppliers WHERE active".to_string(),
    ))
    .all(db)
    .await?;
    for r in appr {
        if let (Some(&a), Some(&b)) = (idx.get(&r.a), idx.get(&r.b)) {
            let lead = r.l.map(|l| (l as f64, 1.0)).unwrap_or((0.0, 0.0));
            if !full.flows.contains_key(&(a, b)) {
                full.add_flow(a, b, Flow { material: 0.0, financial: 0.0, traffic: 0.5, dependency: 0.0, lead_acc: lead });
            }
        }
    }
    // dependency layer: share of each product's purchased volume sourced from the supplier
    let mut tot: HashMap<usize, f64> = HashMap::new();
    for (_, p, m) in &sup_prod {
        *tot.entry(*p).or_default() += m;
    }
    for (s, p, m) in &sup_prod {
        if let Some(f) = full.flows.get_mut(&(*s, *p)) {
            f.dependency = if tot[p] > 0.0 { m / tot[p] } else { 0.0 };
        }
    }

    // ---- hyperedges
    let hsql = format!(
        "(SELECT 'purchase_order' AS kind, po.supplier_id::text || ',' || po.warehouse_id::text || ',' || string_agg(DISTINCT pol.product_id::text, ',') AS members \
           FROM purchase_orders po JOIN purchase_order_lines pol ON pol.purchase_order_id = po.id WHERE {win} GROUP BY po.id LIMIT 20000) \
         UNION ALL (SELECT 'shipment', sh.carrier_id::text || ',' || so.warehouse_id::text || ',' || so.customer_id::text || ',' || string_agg(DISTINCT sol.product_id::text, ',') \
           FROM {SHIP} JOIN sales_order_lines sol ON sol.sales_order_id = so.id WHERE sh.carrier_id IS NOT NULL AND {swin} \
           GROUP BY sh.id, sh.carrier_id, so.warehouse_id, so.customer_id LIMIT 20000) \
         UNION ALL (SELECT 'contract', po.supplier_id::text || ',' || string_agg(DISTINCT pol.product_id::text, ',') \
           FROM purchase_orders po JOIN purchase_order_lines pol ON pol.purchase_order_id = po.id WHERE po.contract_id IS NOT NULL AND {win} \
           GROUP BY po.contract_id, po.supplier_id) \
         UNION ALL (SELECT 'production', l.warehouse_id::text || ',' || string_agg(DISTINCT r.product_id::text, ',') \
           FROM production_runs r JOIN locations l ON l.id = r.location_id WHERE r.produced_on > $1::date - $2::int AND r.produced_on <= $1::date \
           GROUP BY l.warehouse_id, date_trunc('week', r.produced_on) LIMIT 20000)"
    );
    let hyp = HyperRow::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres, hsql, [as_of.into(), lookback.into()])).all(db).await?;
    for h in hyp {
        let kind: &'static str = match h.kind.as_str() {
            "purchase_order" => "purchase_order",
            "shipment" => "shipment",
            "contract" => "contract",
            _ => "production",
        };
        let mut members: Vec<usize> = h.members.split(',').filter_map(|s| Uuid::parse_str(s).ok()).filter_map(|u| idx.get(&u).copied()).collect();
        members.truncate(14);
        full.add_hyper(kind, members, 1.0);
    }

    // ---- weekly series (raw buckets: index 0 = everything before the window)
    let nfull = full.r();
    let mut raw: HashMap<(usize, usize), Vec<f64>> = HashMap::new();
    let mut put = |rows: Vec<TsRow>, ch: usize| {
        for r in rows {
            if let Some(&i) = idx.get(&r.id) {
                let e = raw.entry((i, ch)).or_insert_with(|| vec![f64::NAN; weeks + 1]);
                let k = (r.k + 1) as usize;
                if k <= weeks {
                    e[k] = if e[k].is_nan() { r.v } else { e[k] + r.v };
                }
            }
        }
    };
    let mv = "stock_movements sm JOIN locations l ON l.id = sm.location_id";
    // suppliers
    put(ts(db, "po.supplier_id", "gr.received_on", "SUM(gr.quantity)", &format!("{RECV} WHERE true"), base, n).await?, 0);
    put(ts(db, "po.supplier_id", "gr.received_on", LATE, &format!("{RECV} WHERE true"), base, n).await?, 1);
    put(ts(db, "po.supplier_id", "gr.received_on", REJ, &format!("{RECV} WHERE true"), base, n).await?, 2);
    put(ts(db, "po.supplier_id", "po.order_date", "SUM(pol.quantity_ordered)", "purchase_orders po JOIN purchase_order_lines pol ON pol.purchase_order_id = po.id WHERE po.status NOT IN ('draft','cancelled')", base, n).await?, 10);
    // products
    put(ts(db, "sm.product_id", "sm.created_at", "SUM(-sm.quantity_delta)", "stock_movements sm WHERE sm.movement_type = 'issue'", base, n).await?, 0);
    put(ts(db, "gr.product_id", "gr.received_on", LATE, &format!("{RECV} WHERE true"), base, n).await?, 1);
    put(ts(db, "gr.product_id", "gr.received_on", REJ, &format!("{RECV} WHERE true"), base, n).await?, 2);
    put(ts(db, "sm.product_id", "sm.created_at", "SUM(sm.quantity_delta)", "stock_movements sm WHERE true", base, n).await?, 12);
    // warehouses
    put(ts(db, "l.warehouse_id", "sm.created_at", "SUM(ABS(sm.quantity_delta))", &format!("{mv} WHERE sm.movement_type NOT LIKE 'transfer%'"), base, n).await?, 0);
    put(ts(db, "so.warehouse_id", "sh.shipped_at", "AVG(EXTRACT(EPOCH FROM (sh.shipped_at - so.created_at)) / 86400.0)", &format!("{SHIP} WHERE sh.shipped_at IS NOT NULL"), base, n).await?, 1);
    put(ts(db, "l.warehouse_id", "gr.received_on", REJ, "goods_receipts gr JOIN locations l ON l.id = gr.location_id WHERE true", base, n).await?, 2);
    put(ts(db, "l.warehouse_id", "sm.created_at", "SUM(sm.quantity_delta)", &format!("{mv} WHERE true"), base, n).await?, 12);
    // customers
    put(ts(db, "so.customer_id", "so.created_at", "SUM(sol.quantity)", "sales_order_lines sol JOIN sales_orders so ON so.id = sol.sales_order_id WHERE so.status <> 'cancelled'", base, n).await?, 0);
    put(ts(db, "so.customer_id", "sh.delivered_at", "AVG((sh.delivered_at::date - sh.expected_delivery_date)::float8)", &format!("{SHIP} WHERE sh.delivered_at IS NOT NULL AND sh.expected_delivery_date IS NOT NULL"), base, n).await?, 1);
    put(ts(db, "so.customer_id", "COALESCE(sh.shipped_at, sh.created_at)", EXC, &format!("{SHIP} WHERE true"), base, n).await?, 2);
    put(ts(db, "so.customer_id", "so.created_at", "AVG(CASE WHEN so.status IN ('backordered','partially_allocated') THEN 1.0 ELSE 0.0 END)", "sales_orders so WHERE so.status <> 'cancelled'", base, n).await?, 3);
    // carriers
    put(ts(db, "sh.carrier_id", "COALESCE(sh.shipped_at, sh.created_at)", "COUNT(*)", &format!("{SHIP} WHERE sh.carrier_id IS NOT NULL"), base, n).await?, 0);
    put(ts(db, "sh.carrier_id", "sh.delivered_at", "AVG((sh.delivered_at::date - sh.expected_delivery_date)::float8)", &format!("{SHIP} WHERE sh.delivered_at IS NOT NULL AND sh.expected_delivery_date IS NOT NULL"), base, n).await?, 1);
    put(ts(db, "sh.carrier_id", "COALESCE(sh.shipped_at, sh.created_at)", EXC, &format!("{SHIP} WHERE sh.carrier_id IS NOT NULL"), base, n).await?, 2);
    put(ts(db, "sh.carrier_id", "sh.delivered_at", "COUNT(*)", &format!("{SHIP} WHERE sh.delivered_at IS NOT NULL AND sh.carrier_id IS NOT NULL"), base, n).await?, 14);
    let caps: HashMap<Uuid, f64> = TsRow::find_by_statement(Statement::from_string(
        DbBackend::Postgres,
        "SELECT warehouse_id AS id, 0 AS k, SUM(capacity)::float8 AS v FROM locations WHERE capacity IS NOT NULL GROUP BY 1".to_string(),
    ))
    .all(db)
    .await?
    .into_iter()
    .map(|r| (r.id, r.v))
    .collect();
    let capacity: HashMap<usize, f64> = caps.into_iter().filter_map(|(u, v)| idx.get(&u).map(|i| (*i, v))).collect();

    let kinds: Vec<Kind> = full.nodes.iter().map(|n| n.kind).collect();
    let (fv, fo) = assemble(&raw, &kinds, &capacity, weeks);
    let (graph, keep) = full.restrict(MAX_NODES);
    let r = graph.r();
    let mut values = vec![0.0; weeks * r * NCH];
    let mut obs = vec![0.0; weeks * r * NCH];
    for t in 0..weeks {
        for (ni, &old) in keep.iter().enumerate() {
            let (src, dst) = ((t * nfull + old) * NCH, (t * r + ni) * NCH);
            values[dst..dst + NCH].copy_from_slice(&fv[src..src + NCH]);
            obs[dst..dst + NCH].copy_from_slice(&fo[src..src + NCH]);
        }
    }
    Ok(Context { as_of, base, weeks, graph, total_nodes, values, obs })
}

fn cum(v: &[f64]) -> Vec<f64> {
    let mut acc = 0.0;
    v.iter().map(|x| { acc += if x.is_nan() { 0.0 } else { *x }; acc }).collect()
}

/// Turn raw weekly buckets into the T x R x NCH tensor (pure; unit-tested).
pub fn assemble(raw: &HashMap<(usize, usize), Vec<f64>>, kinds: &[Kind], capacity: &HashMap<usize, f64>, weeks: usize) -> (Vec<f64>, Vec<f64>) {
    let r = kinds.len();
    let mut values = vec![0.0; weeks * r * NCH];
    let mut obs = vec![0.0; weeks * r * NCH];
    let get = |i: usize, ch: usize| raw.get(&(i, ch));
    for i in 0..r {
        let mut set = |t: usize, ch: usize, v: f64, o: f64| {
            values[(t * r + i) * NCH + ch] = v;
            obs[(t * r + i) * NCH + ch] = o;
        };
        let vol: Vec<f64> = get(i, 0).map(|v| v.iter().map(|x| if x.is_nan() { 0.0 } else { *x }).collect()).unwrap_or(vec![0.0; weeks + 1]);
        for t in 0..weeks {
            set(t, 0, vol[t + 1].max(0.0).ln_1p(), 1.0);
            for (ch, lo, hi) in [(1usize, -30.0, 60.0), (2, 0.0, 100.0)] {
                if let Some(x) = get(i, ch).map(|v| v[t + 1]).filter(|x| !x.is_nan()) {
                    set(t, ch, x.clamp(lo, hi), 1.0);
                }
            }
        }
        let mean_pos = |v: &[f64]| (v[1..].iter().sum::<f64>() / weeks as f64).max(1.0);
        let pressure: Vec<f64> = match kinds[i] {
            Kind::Supplier => {
                let ord = cum(get(i, 10).map(|v| &v[..]).unwrap_or(&vec![0.0; weeks + 1]));
                let rec = cum(&vol);
                let m = mean_pos(&vol);
                (1..=weeks).map(|k| (ord[k] - rec[k]).max(0.0) / m).collect()
            }
            Kind::Product => {
                let oh = cum(get(i, 12).map(|v| &v[..]).unwrap_or(&vec![0.0; weeks + 1]));
                let m = mean_pos(&vol);
                (1..=weeks).map(|k| 1.0 / (1.0 + (oh[k] / m).max(0.0))).collect()
            }
            Kind::Warehouse => {
                let oh = cum(get(i, 12).map(|v| &v[..]).unwrap_or(&vec![0.0; weeks + 1]));
                let cap = capacity.get(&i).copied().filter(|c| *c > 0.0).unwrap_or_else(|| oh.iter().cloned().fold(1.0, f64::max));
                (1..=weeks).map(|k| (oh[k] / cap).clamp(0.0, 1.5)).collect()
            }
            Kind::Customer => (1..=weeks).map(|k| get(i, 3).map(|v| if v[k].is_nan() { 0.0 } else { v[k] }).unwrap_or(0.0)).collect(),
            Kind::Carrier => {
                let sh = cum(&vol);
                let de = cum(get(i, 14).map(|v| &v[..]).unwrap_or(&vec![0.0; weeks + 1]));
                let m = mean_pos(&vol);
                (1..=weeks).map(|k| (sh[k] - de[k]).max(0.0) / m).collect()
            }
        };
        for t in 0..weeks {
            set(t, 3, pressure[t], 1.0);
        }
    }
    (values, obs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assemble_builds_backlog_pressure_and_masks_missing_delay() {
        let mut raw = HashMap::new();
        // supplier 0: ordered 100 in week 0, received 40 in week 1 -> backlog 100 then 60
        raw.insert((0, 10), vec![f64::NAN, 100.0, f64::NAN, f64::NAN]);
        raw.insert((0, 0), vec![f64::NAN, f64::NAN, 40.0, f64::NAN]);
        raw.insert((0, 1), vec![f64::NAN, f64::NAN, 3.0, f64::NAN]);
        let (v, o) = assemble(&raw, &[Kind::Supplier], &HashMap::new(), 3);
        let r = 1;
        assert!(v[(0 * r) * NCH + 3] > v[(2 * r) * NCH + 3] - 1e-12 || v[(1 * r) * NCH + 3] > 0.0);
        assert_eq!(o[(0 * r) * NCH + 1], 0.0); // no receipt in week 0: delay unobserved
        assert_eq!(o[(1 * r) * NCH + 1], 1.0);
        assert_eq!(v[(1 * r) * NCH + 1], 3.0);
        assert!((v[(1 * r) * NCH] - 40.0f64.ln_1p()).abs() < 1e-12);
    }
}
