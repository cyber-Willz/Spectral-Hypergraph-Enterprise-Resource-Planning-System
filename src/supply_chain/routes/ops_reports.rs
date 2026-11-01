//! Operations reports driven by the dataset analysis: shipment risk, carrier scorecards, lead-time
//! master-data accuracy, warehouse utilisation, stock/ledger reconciliation and KPI benchmarking.
use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use chrono::{NaiveDate, Utc};
use sea_orm::{DatabaseConnection, DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::supply_chain::{
    error::{AppError, AppResult},
    risk::{delay_probability, disruption_probability, normalise_weather, disruption_weather, DelayInputs, DisruptionInputs},
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/risk/shipment", post(assess_endpoint))
        .route("/reports/carrier-scorecards", get(carrier_scorecards))
        .route("/reports/lead-time-accuracy", get(lead_time_accuracy))
        .route("/reports/warehouse-utilization", get(utilization))
        .route("/reports/stock-reconciliation", get(reconciliation))
        .route("/reports/kpis", get(kpis))
}

// ---- warehouse utilisation ---------------------------------------------------------------------------------

#[derive(Debug, FromQueryResult, Serialize)]
pub struct UtilRow {
    warehouse_id: Uuid,
    code: String,
    locations: i64,
    capacity_units: i64,
    on_hand_units: i64,
    over_capacity_locations: i64,
}

async fn util_rows(db: &DatabaseConnection, warehouse: Option<Uuid>) -> AppResult<Vec<UtilRow>> {
    Ok(UtilRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT w.id AS warehouse_id, w.code, \
                COUNT(l.id) FILTER (WHERE l.active AND l.capacity IS NOT NULL)::bigint AS locations, \
                COALESCE(SUM(l.capacity) FILTER (WHERE l.active AND l.capacity IS NOT NULL), 0)::bigint AS capacity_units, \
                COALESCE(SUM(oh.q) FILTER (WHERE l.active AND l.capacity IS NOT NULL), 0)::bigint AS on_hand_units, \
                COUNT(*) FILTER (WHERE l.capacity IS NOT NULL AND COALESCE(oh.q, 0) > l.capacity)::bigint AS over_capacity_locations \
         FROM warehouses w \
         LEFT JOIN locations l ON l.warehouse_id = w.id \
         LEFT JOIN (SELECT location_id, SUM(quantity) AS q FROM stock_levels GROUP BY location_id) oh ON oh.location_id = l.id \
         WHERE ($1::uuid IS NULL OR w.id = $1) GROUP BY w.id, w.code ORDER BY w.code",
        [warehouse.into()],
    ))
    .all(db)
    .await?)
}

/// Percent of bin capacity in use (only bins with a declared capacity count); None without capacity data.
pub async fn warehouse_utilization(db: &DatabaseConnection, warehouse: Uuid) -> AppResult<Option<f64>> {
    Ok(util_rows(db, Some(warehouse)).await?.into_iter().next().and_then(|r| {
        (r.capacity_units > 0).then(|| r.on_hand_units as f64 / r.capacity_units as f64 * 100.0)
    }))
}

/// Congestion indicator: the logistics data show delay hours rising with utilisation (r = 0.89) and
/// damage claims too (r = 0.80); the delay model also uses utilisation as an input.
async fn utilization(State(s): State<AppState>) -> AppResult<Json<Value>> {
    let rows = util_rows(&s.db, None).await?;
    let out: Vec<Value> = rows
        .iter()
        .map(|r| {
            let u = (r.capacity_units > 0).then(|| r.on_hand_units as f64 / r.capacity_units as f64 * 100.0);
            json!({
                "warehouse_id": r.warehouse_id, "code": r.code, "bins_with_capacity": r.locations,
                "capacity_units": r.capacity_units, "on_hand_units": r.on_hand_units,
                "utilization_pct": u, "over_capacity_bins": r.over_capacity_locations,
                "status": match u { None => "no_capacity_data", Some(x) if x >= 90.0 => "congested", Some(x) if x >= 80.0 => "tight", Some(_) => "ok" },
            })
        })
        .collect();
    Ok(Json(json!({ "warehouses": out })))
}

// ---- shipment risk -------------------------------------------------------------------------------------------

#[derive(Deserialize, Default, Clone)]
pub struct RiskIn {
    /// clear | cloudy | extreme heat | fog | rain | heavy rain | snow | storm | hurricane
    pub weather: Option<String>,
    /// 0-100
    pub traffic_index: Option<f64>,
    /// percent
    pub supplier_reliability: Option<f64>,
    /// percent; defaults to the carrier's observed on-time rate (>= 10 delivered shipments)
    pub carrier_reliability: Option<f64>,
    /// percent; defaults to the warehouse's measured bin utilisation
    pub warehouse_utilization: Option<f64>,
    /// 0-10, international legs
    pub geopolitical_risk: Option<f64>,
    pub carrier_id: Option<Uuid>,
    pub warehouse_id: Option<Uuid>,
}

pub struct Assessment {
    pub delay: Option<(f64, usize)>,
    pub disruption: Option<(f64, usize)>,
    pub context: Value,
}

fn in_range(field: &str, v: Option<f64>, lo: f64, hi: f64) -> AppResult<()> {
    if matches!(v, Some(x) if x.is_nan() || x < lo || x > hi) {
        return Err(AppError::bad(format!("{field} must be between {lo} and {hi}")));
    }
    Ok(())
}

pub async fn carrier_on_time_pct(db: &DatabaseConnection, carrier: Uuid) -> AppResult<Option<f64>> {
    #[derive(FromQueryResult)]
    struct R {
        n: i64,
        rate: Option<f64>,
    }
    let r = R::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT COUNT(*)::bigint AS n, (AVG((delivered_at::date <= expected_delivery_date)::int) * 100)::float8 AS rate \
         FROM shipments WHERE carrier_id = $1 AND delivered_at IS NOT NULL AND expected_delivery_date IS NOT NULL",
        [carrier.into()],
    ))
    .one(db)
    .await?;
    Ok(r.filter(|x| x.n >= 10).and_then(|x| x.rate))
}

/// Fills missing inputs from our own data where possible, then runs both models.
pub async fn assess(db: &DatabaseConnection, r: &RiskIn) -> AppResult<Assessment> {
    in_range("traffic_index", r.traffic_index, 0.0, 100.0)?;
    in_range("supplier_reliability", r.supplier_reliability, 0.0, 100.0)?;
    in_range("carrier_reliability", r.carrier_reliability, 0.0, 100.0)?;
    in_range("warehouse_utilization", r.warehouse_utilization, 0.0, 100.0)?;
    in_range("geopolitical_risk", r.geopolitical_risk, 0.0, 10.0)?;
    if let Some(w) = &r.weather {
        if normalise_weather(w).is_none() || disruption_weather(w).is_none() {
            return Err(AppError::bad("weather must be clear, cloudy, extreme heat, fog, rain, heavy rain, snow, storm or hurricane"));
        }
    }
    let mut auto = Vec::new();
    let carrier_rel = match (r.carrier_reliability, r.carrier_id) {
        (Some(v), _) => Some(v),
        (None, Some(c)) => {
            let v = carrier_on_time_pct(db, c).await?;
            if v.is_some() {
                auto.push("carrier_reliability from observed on-time rate");
            }
            v
        }
        _ => None,
    };
    let util = match (r.warehouse_utilization, r.warehouse_id) {
        (Some(v), _) => Some(v),
        (None, Some(w)) => {
            let v = warehouse_utilization(db, w).await?;
            if v.is_some() {
                auto.push("warehouse_utilization from bin capacity");
            }
            v
        }
        _ => None,
    };
    let delay_in = DelayInputs {
        traffic_index: r.traffic_index,
        supplier_reliability: r.supplier_reliability,
        carrier_reliability: carrier_rel,
        warehouse_utilization: util,
        weather: r.weather.clone(),
    };
    let (dp, di) = delay_probability(&delay_in);
    let delay = (di <= 3).then_some((dp, di));
    let disruption = (r.geopolitical_risk.is_some() || r.weather.is_some()).then(|| {
        disruption_probability(&DisruptionInputs { geopolitical_risk: r.geopolitical_risk, carrier_reliability: carrier_rel, weather: r.weather.clone() })
    });
    let context = json!({
        "weather": r.weather, "traffic_index": r.traffic_index, "supplier_reliability": r.supplier_reliability,
        "carrier_reliability": carrier_rel, "warehouse_utilization": util, "geopolitical_risk": r.geopolitical_risk,
        "filled_automatically": auto, "delay_inputs_imputed": di,
    });
    Ok(Assessment { delay, disruption: disruption.filter(|(_, i)| *i <= 2), context })
}

/// Two dataset-fitted models: delay probability (domestic/operational drivers: traffic, supplier and
/// carrier reliability, warehouse utilisation, weather) and disruption probability (weather, geopolitical
/// risk, carrier reliability). Inputs left out are imputed; results with too many imputed inputs are withheld.
async fn assess_endpoint(State(s): State<AppState>, Json(r): Json<RiskIn>) -> AppResult<Json<Value>> {
    let a = assess(&s.db, &r).await?;
    let band = |p: f64| if p < 0.2 { "low" } else if p < 0.5 { "medium" } else { "high" };
    Ok(Json(json!({
        "delay": a.delay.map(|(p, i)| json!({"probability": p, "band": band(p), "inputs_imputed": i, "base_rate_in_training_data": 0.253})),
        "disruption": a.disruption.map(|(p, i)| json!({"probability": p, "band": band(p), "inputs_imputed": i, "base_rate_in_training_data": 0.613})),
        "inputs": a.context,
    })))
}

// ---- carrier scorecards ----------------------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CarrierQuery {
    /// deliveries earlier than this many days before the promise count as early (default 3)
    early_tolerance_days: Option<i32>,
    since: Option<NaiveDate>,
}

#[derive(Debug, FromQueryResult, Serialize)]
pub struct CarrierRow {
    carrier_id: Uuid,
    code: String,
    name: String,
    shipments: i64,
    delivered: i64,
    judged: i64,
    on_time_rate: Option<f64>,
    early_rate: Option<f64>,
    observed_late_rate: Option<f64>,
    avg_days_late_when_late: Option<f64>,
    avg_transit_days: Option<f64>,
    avg_freight_cost: Option<f64>,
    predicted_late_rate: Option<f64>,
}

/// Observed carrier performance from shipments with a promised date, next to what the delay model
/// predicted when each shipment left (a live calibration check).
async fn carrier_scorecards(State(s): State<AppState>, Query(q): Query<CarrierQuery>) -> AppResult<Json<Vec<CarrierRow>>> {
    let rows = CarrierRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT c.id AS carrier_id, c.code, c.name, COUNT(s.id)::bigint AS shipments, \
                COUNT(s.delivered_at)::bigint AS delivered, \
                COUNT(*) FILTER (WHERE s.delivered_at IS NOT NULL AND s.expected_delivery_date IS NOT NULL)::bigint AS judged, \
                (AVG((s.delivered_at::date <= s.expected_delivery_date)::int) FILTER (WHERE s.delivered_at IS NOT NULL AND s.expected_delivery_date IS NOT NULL))::float8 AS on_time_rate, \
                (AVG((s.delivered_at::date < s.expected_delivery_date - $1::int)::int) FILTER (WHERE s.delivered_at IS NOT NULL AND s.expected_delivery_date IS NOT NULL))::float8 AS early_rate, \
                (AVG((s.delivered_at::date > s.expected_delivery_date)::int) FILTER (WHERE s.delivered_at IS NOT NULL AND s.expected_delivery_date IS NOT NULL))::float8 AS observed_late_rate, \
                (AVG((s.delivered_at::date - s.expected_delivery_date)::float8) FILTER (WHERE s.delivered_at::date > s.expected_delivery_date))::float8 AS avg_days_late_when_late, \
                (AVG((s.delivered_at::date - s.shipped_at::date)::float8) FILTER (WHERE s.delivered_at IS NOT NULL AND s.shipped_at IS NOT NULL))::float8 AS avg_transit_days, \
                AVG(s.freight_cost)::float8 AS avg_freight_cost, \
                (AVG(s.delay_risk) FILTER (WHERE s.delivered_at IS NOT NULL AND s.expected_delivery_date IS NOT NULL AND s.delay_risk IS NOT NULL))::float8 AS predicted_late_rate \
         FROM carriers c LEFT JOIN shipments s ON s.carrier_id = c.id AND ($2::date IS NULL OR s.shipped_at::date >= $2) \
         GROUP BY c.id, c.code, c.name ORDER BY c.code",
        [q.early_tolerance_days.unwrap_or(3).into(), q.since.into()],
    ))
    .all(&s.db)
    .await?;
    Ok(Json(rows))
}

// ---- lead-time master-data accuracy ----------------------------------------------------------------------------

#[derive(Debug, FromQueryResult)]
struct LtRow {
    supplier_id: Uuid,
    code: String,
    name: String,
    master_lead: Option<f64>,
    source_lead: Option<f64>,
    n: i64,
    mean: Option<f64>,
    sd: Option<f64>,
    p90: Option<f64>,
}

#[derive(Debug, FromQueryResult)]
struct ModeRow {
    transport_mode: String,
    n: i64,
    mean: Option<f64>,
    sd: Option<f64>,
}

#[derive(Deserialize)]
pub struct LtQuery {
    since: Option<NaiveDate>,
}

/// Planned lead time (approved-source value, else supplier master) against what receipts show.
/// ERP data: actuals ran +2.5 days over route master data with 3x the stated spread; Cacau Prime:
/// master values of 4-20 days against ~9.8 days observed for every supplier.
async fn lead_time_accuracy(State(s): State<AppState>, Query(q): Query<LtQuery>) -> AppResult<Json<Value>> {
    let rows = LtRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "WITH obs AS ( \
           SELECT po.supplier_id, (gr.received_on - po.order_date)::float8 AS d \
           FROM goods_receipts gr JOIN purchase_orders po ON po.id = gr.purchase_order_id \
           WHERE $1::date IS NULL OR po.order_date >= $1), \
         plan AS (SELECT supplier_id, AVG(lead_time_days)::float8 AS planned FROM product_suppliers \
                  WHERE active AND lead_time_days IS NOT NULL GROUP BY supplier_id) \
         SELECT s.id AS supplier_id, s.code, s.name, s.lead_time_days::float8 AS master_lead, plan.planned AS source_lead, \
                COUNT(obs.d)::bigint AS n, AVG(obs.d) AS mean, STDDEV_SAMP(obs.d) AS sd, \
                PERCENTILE_CONT(0.9) WITHIN GROUP (ORDER BY obs.d) AS p90 \
         FROM suppliers s JOIN obs ON obs.supplier_id = s.id LEFT JOIN plan ON plan.supplier_id = s.id \
         GROUP BY s.id, s.code, s.name, s.lead_time_days, plan.planned ORDER BY s.code",
        [q.since.into()],
    ))
    .all(&s.db)
    .await?;
    let modes = ModeRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT po.transport_mode, COUNT(*)::bigint AS n, AVG((gr.received_on - po.order_date)::float8) AS mean, \
                STDDEV_SAMP((gr.received_on - po.order_date)::float8) AS sd \
         FROM goods_receipts gr JOIN purchase_orders po ON po.id = gr.purchase_order_id \
         WHERE po.transport_mode IS NOT NULL AND ($1::date IS NULL OR po.order_date >= $1) \
         GROUP BY po.transport_mode ORDER BY po.transport_mode",
        [q.since.into()],
    ))
    .all(&s.db)
    .await?;
    let (mut under, mut judged, mut cvs) = (0, 0, Vec::new());
    let (mut w_bias, mut w_n) = (0.0, 0.0);
    let suppliers: Vec<Value> = rows
        .iter()
        .map(|r| {
            let planned = r.source_lead.or(r.master_lead);
            let mean = r.mean.unwrap_or(0.0);
            let cv = match (r.sd, r.mean) { (Some(sd), Some(m)) if m > 0.0 => Some(sd / m), _ => None };
            let flag = match planned {
                Some(p) if r.n >= 10 && p > 0.0 && mean > p * 1.2 => "understated",
                Some(p) if r.n >= 10 && p > 0.0 && mean < p * 0.8 => "overstated",
                Some(_) if r.n >= 10 => "ok",
                _ => "too_few_receipts",
            };
            if r.n >= 10 {
                judged += 1;
                if flag == "understated" { under += 1; }
                if let Some(c) = cv { cvs.push(c); }
                if let Some(p) = planned { w_bias += (mean - p) * r.n as f64; w_n += r.n as f64; }
            }
            json!({
                "supplier_id": r.supplier_id, "code": r.code, "name": r.name, "receipts": r.n,
                "planned_lead_days": planned, "planned_source": if r.source_lead.is_some() { "approved_sources" } else { "supplier_master" },
                "observed_mean_days": r.mean, "observed_sd_days": r.sd, "observed_p90_days": r.p90,
                "bias_days": planned.map(|p| mean - p), "observed_to_planned": planned.filter(|p| *p > 0.0).map(|p| mean / p),
                "observed_cv": cv, "master_data": flag,
            })
        })
        .collect();
    cvs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_cv = (!cvs.is_empty()).then(|| cvs[cvs.len() / 2]);
    Ok(Json(json!({
        "summary": {
            "suppliers_judged": judged, "understated": under,
            "receipt_weighted_bias_days": (w_n > 0.0).then(|| w_bias / w_n),
            "median_observed_cv": median_cv,
            "note": "when a product has fewer than 5 receipts the planner now pools its suppliers' receipts, then falls back to master data with a CV of 0.5",
        },
        "suppliers": suppliers,
        "by_transport_mode": modes.iter().map(|m| json!({"mode": m.transport_mode, "receipts": m.n, "mean_days": m.mean, "sd_days": m.sd, "cv": match (m.sd, m.mean) { (Some(a), Some(b)) if b > 0.0 => Some(a / b), _ => None }})).collect::<Vec<_>>(),
    })))
}

// ---- stock / ledger reconciliation ------------------------------------------------------------------------------

#[derive(Debug, FromQueryResult, Serialize)]
struct DriftRow {
    sku: String,
    location: String,
    lot_number: String,
    stock_level_quantity: i64,
    ledger_quantity: i64,
    drift: i64,
}

#[derive(Debug, FromQueryResult, Serialize)]
struct ResvRow {
    sku: String,
    location: String,
    lot_number: String,
    reserved_on_stock_level: i64,
    reserved_by_allocations: i64,
    drift: i64,
}

#[derive(Deserialize)]
pub struct ReconQuery {
    limit: Option<i64>,
}

/// Compares stock levels with the sum of the movement ledger, and reserved quantities with open
/// allocations. The Cacau Prime snapshots fail this kind of check (next-day opening != prior closing in
/// 99.7% of rows and identical rows copied across branches); a ledger-backed system should reconcile exactly.
async fn reconciliation(State(s): State<AppState>, Query(q): Query<ReconQuery>) -> AppResult<Json<Value>> {
    let limit = q.limit.unwrap_or(50).clamp(1, 1000);
    let drift = DriftRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "WITH m AS (SELECT product_id, location_id, lot_number, SUM(quantity_delta)::bigint AS q FROM stock_movements GROUP BY 1, 2, 3), \
              x AS (SELECT COALESCE(sl.product_id, m.product_id) AS product_id, COALESCE(sl.location_id, m.location_id) AS location_id, \
                           COALESCE(sl.lot_number, m.lot_number) AS lot_number, COALESCE(sl.quantity, 0) AS sq, COALESCE(m.q, 0) AS lq \
                    FROM stock_levels sl FULL OUTER JOIN m ON m.product_id = sl.product_id AND m.location_id = sl.location_id AND m.lot_number = sl.lot_number) \
         SELECT p.sku, l.code AS location, x.lot_number, x.sq AS stock_level_quantity, x.lq AS ledger_quantity, (x.sq - x.lq) AS drift \
         FROM x JOIN products p ON p.id = x.product_id JOIN locations l ON l.id = x.location_id \
         WHERE x.sq <> x.lq ORDER BY ABS(x.sq - x.lq) DESC, p.sku LIMIT $1",
        [limit.into()],
    ))
    .all(&s.db)
    .await?;
    let resv = ResvRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT p.sku, l.code AS location, sl.lot_number, sl.reserved AS reserved_on_stock_level, \
                COALESCE(a.q, 0)::bigint AS reserved_by_allocations, (sl.reserved - COALESCE(a.q, 0))::bigint AS drift \
         FROM stock_levels sl JOIN products p ON p.id = sl.product_id JOIN locations l ON l.id = sl.location_id \
         LEFT JOIN (SELECT stock_level_id, SUM(quantity) AS q FROM allocations WHERE status = 'reserved' GROUP BY stock_level_id) a ON a.stock_level_id = sl.id \
         WHERE sl.reserved <> COALESCE(a.q, 0) ORDER BY ABS(sl.reserved - COALESCE(a.q, 0)) DESC LIMIT $1",
        [limit.into()],
    ))
    .all(&s.db)
    .await?;
    #[derive(FromQueryResult)]
    struct Tot {
        levels: i64,
        movements: i64,
    }
    let t = Tot::find_by_statement(Statement::from_string(
        DbBackend::Postgres,
        "SELECT (SELECT COUNT(*) FROM stock_levels)::bigint AS levels, (SELECT COUNT(*) FROM stock_movements)::bigint AS movements".to_string(),
    ))
    .one(&s.db)
    .await?
    .ok_or_else(|| AppError::not_found("totals"))?;
    Ok(Json(json!({
        "stock_levels_checked": t.levels, "ledger_entries": t.movements,
        "quantity_drift_rows": drift.len(), "reservation_drift_rows": resv.len(),
        "balanced": drift.is_empty() && resv.is_empty(),
        "quantity_drift": drift, "reservation_drift": resv,
    })))
}

// ---- KPI benchmarking -----------------------------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct KpiQuery {
    warehouse_id: Option<Uuid>,
    as_of: Option<NaiveDate>,
    window_days: Option<i32>,
}

fn position(v: Option<f64>, p25: f64, p75: f64, higher_is_better: bool) -> &'static str {
    match v {
        None => "no_data",
        Some(x) => {
            let (strong, weak) = if higher_is_better { (x > p75, x < p25) } else { (x < p25, x > p75) };
            if strong { "strong" } else if weak { "weak" } else { "typical" }
        }
    }
}

/// KPI scorecard against reference quartiles taken from the 1,000-row company SCM dataset (inventory
/// turnover 4.5/5.5/6.2, order fulfilment 90/91/93%, lead time 10/11/14 days, lead-time sd 2/3/4 days).
/// Those quartiles are descriptive of that sample, not targets. Turnover divides issue cost by the
/// CURRENT inventory value (no snapshots are kept), so treat it as an approximation.
async fn kpis(State(s): State<AppState>, Query(q): Query<KpiQuery>) -> AppResult<Json<Value>> {
    let window = q.window_days.unwrap_or(365);
    if !(30..=1095).contains(&window) {
        return Err(AppError::bad("window_days must be between 30 and 1095"));
    }
    let as_of = q.as_of.unwrap_or_else(|| Utc::now().date_naive());
    #[derive(FromQueryResult)]
    struct One {
        v: Option<f64>,
    }
    let one = |sql: &'static str, vals: Vec<sea_orm::Value>| {
        let db = s.db.clone();
        async move { One::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres, sql, vals)).one(&db).await.map(|r| r.and_then(|x| x.v)) }
    };
    let issue_cost = one(
        "SELECT COALESCE(SUM(-sm.quantity_delta * p.unit_cost), 0)::float8 AS v FROM stock_movements sm \
         JOIN products p ON p.id = sm.product_id JOIN locations l ON l.id = sm.location_id \
         WHERE sm.movement_type = 'issue' AND sm.created_at::date > $2::date - $3::int AND sm.created_at::date <= $2::date \
           AND ($1::uuid IS NULL OR l.warehouse_id = $1)",
        vec![q.warehouse_id.into(), as_of.into(), window.into()],
    )
    .await?;
    let inv_value = one(
        "SELECT COALESCE(SUM(sl.quantity * p.unit_cost), 0)::float8 AS v FROM stock_levels sl \
         JOIN products p ON p.id = sl.product_id JOIN locations l ON l.id = sl.location_id \
         WHERE ($1::uuid IS NULL OR l.warehouse_id = $1)",
        vec![q.warehouse_id.into()],
    )
    .await?;
    let fill = one(
        "SELECT (CASE WHEN SUM(sol.quantity) > 0 THEN SUM(sol.quantity_shipped)::float8 / SUM(sol.quantity) * 100 END)::float8 AS v \
         FROM sales_order_lines sol JOIN sales_orders so ON so.id = sol.sales_order_id \
         WHERE so.status NOT IN ('draft', 'cancelled') AND so.created_at::date > $2::date - $3::int AND so.created_at::date <= $2::date \
           AND ($1::uuid IS NULL OR so.warehouse_id = $1)",
        vec![q.warehouse_id.into(), as_of.into(), window.into()],
    )
    .await?;
    #[derive(FromQueryResult)]
    struct Sup {
        lines: i64,
        on_time: Option<f64>,
        in_full: Option<f64>,
        lt_mean: Option<f64>,
        lt_sd: Option<f64>,
    }
    let sup = Sup::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "WITH l AS (SELECT pol.id, pol.quantity_ordered AS qo, pol.quantity_received AS qr, po.status AS pst, \
                           COALESCE(pol.promised_date, po.expected_date) AS promised, po.order_date, MAX(gr.received_on) AS last_rcpt \
                    FROM purchase_order_lines pol JOIN purchase_orders po ON po.id = pol.purchase_order_id \
                    JOIN goods_receipts gr ON gr.purchase_order_line_id = pol.id \
                    WHERE po.order_date > $1::date - $2::int AND po.order_date <= $1::date AND ($3::uuid IS NULL OR po.warehouse_id = $3) \
                    GROUP BY pol.id, pol.quantity_ordered, pol.quantity_received, po.status, pol.promised_date, po.expected_date, po.order_date) \
         SELECT COUNT(*)::bigint AS lines, \
                (AVG((last_rcpt <= promised)::int) FILTER (WHERE promised IS NOT NULL) * 100)::float8 AS on_time, \
                (AVG((qr >= qo)::int) FILTER (WHERE pst IN ('received', 'closed_short')) * 100)::float8 AS in_full, \
                AVG((last_rcpt - order_date)::float8) AS lt_mean, STDDEV_SAMP((last_rcpt - order_date)::float8) AS lt_sd FROM l",
        [as_of.into(), window.into(), q.warehouse_id.into()],
    ))
    .one(&s.db)
    .await?
    .ok_or_else(|| AppError::not_found("supplier data"))?;
    let turnover = match (issue_cost, inv_value) {
        (Some(c), Some(v)) if v > 0.0 => Some(c * 365.0 / window as f64 / v),
        _ => None,
    };
    let metric = |name: &str, value: Option<f64>, unit: &str, bench: Option<(f64, f64, f64)>, higher: bool| {
        json!({
            "kpi": name, "value": value, "unit": unit,
            "reference_quartiles": bench.map(|(a, b, c)| json!({"p25": a, "p50": b, "p75": c})),
            "position": match bench { Some((a, _, c)) => position(value, a, c, higher), None => if value.is_some() { "no_reference" } else { "no_data" } },
        })
    };
    Ok(Json(json!({
        "as_of": as_of, "window_days": window,
        "kpis": [
            metric("inventory_turnover", turnover, "x per year", Some((4.5, 5.5, 6.2)), true),
            metric("order_fill_rate", fill, "% of ordered units shipped", Some((90.0, 91.0, 93.0)), true),
            metric("supplier_lead_time_mean", sup.lt_mean, "days", Some((10.0, 11.0, 14.0)), false),
            metric("supplier_lead_time_sd", sup.lt_sd, "days", Some((2.0, 3.0, 4.0)), false),
            metric("supplier_on_time_rate", sup.on_time, "% of PO lines", None, true),
            metric("supplier_in_full_rate", sup.in_full, "% of closed PO lines", None, true),
        ],
        "supplier_lines_in_window": sup.lines,
        "note": "reference quartiles describe a 1,000-company sample (extreme outliers excluded); in that sample longer and more variable lead times went with lower fulfilment (r = -0.78 / -0.70), but the data are cross-sectional",
    })))
}
