//! Planning analytics: statistical replenishment, ABC/XYZ classification, forecast accuracy.
use std::collections::HashMap;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use chrono::{NaiveDate, Utc};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement, TransactionTrait};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::supply_chain::{
    error::{AppError, AppResult},
    state::AppState,
    stats::{norm_inv, safety_stock, service_level},
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/reports/replenishment", get(replenishment))
        .route("/reports/abc-xyz", get(abc_xyz))
        .route("/reports/excess-cover", get(excess_cover))
        .route("/reports/forecast-accuracy", get(forecast_accuracy))
        .route("/forecasts", post(upsert_forecasts))
}

/// Fallback lead-time sd (as a fraction of the mean) when neither the product's nor its suppliers' receipts
/// give enough samples. Observed supplier-level CVs: aerospace 0.23, retail 0.47, Cacau Prime 0.59,
/// defence 0.63, ERP simulation 0.50-0.81 (median about 0.55); the previous default of 0.30 understated them.
const DEFAULT_LT_CV: f64 = 0.50;
const MIN_LT_SAMPLES: i64 = 5;

#[derive(Deserialize)]
pub struct ReplQuery {
    warehouse_id: Option<Uuid>,
    /// Demand history window in days (default 90, 14..=730)
    lookback_days: Option<i32>,
    /// Evaluate as of this date instead of today (backtests, imported history)
    as_of: Option<NaiveDate>,
    /// forecast (default): statistical engine in /forecast/demand; history: plain window mean
    demand_source: Option<String>,
    /// weeks of history given to the forecasting engine (default 104, 8..=260)
    history_weeks: Option<i32>,
}

#[derive(Debug, FromQueryResult)]
struct ReplRow {
    product_id: Uuid,
    sku: String,
    name: String,
    criticality_class: String,
    unit_cost: f64,
    holding_cost_rate: f64,
    stockout_cost: Option<f64>,
    review_period_days: i32,
    lead_time_days: i32,
    reorder_point: i64,
    reorder_qty: i64,
    mu: f64,
    sd: f64,
    mu30: f64,
    lt_n: Option<i64>,
    lt_mean: Option<f64>,
    lt_sd: Option<f64>,
    lts_n: Option<i64>,
    lts_mean: Option<f64>,
    lts_sd: Option<f64>,
    available: i64,
    on_order: i64,
}

#[derive(Serialize)]
pub struct ReplItem {
    pub product_id: Uuid,
    pub sku: String,
    pub name: String,
    pub criticality_class: String,
    pub daily_demand_mean: f64,
    pub daily_demand_sd: f64,
    /// last-4-weeks demand / window demand: >1.25 rising, <0.75 falling (seasonality check)
    pub recent_demand_ratio: Option<f64>,
    pub demand_trend: &'static str,
    pub lead_time_days: f64,
    pub lead_time_sd_days: f64,
    pub lead_time_source: &'static str,
    pub review_period_days: i32,
    pub service_level: f64,
    pub safety_stock: i64,
    pub recommended_reorder_point: i64,
    pub order_up_to_level: i64,
    pub current_reorder_point: i64,
    pub reorder_point_action: &'static str,
    pub available: i64,
    pub on_order: i64,
    pub inventory_position: i64,
    pub coverage_days: Option<f64>,
    pub reorder_now: bool,
    pub suggested_order_qty: i64,
    /// ma13 | ses | sba | tsb | ses_tsb (forecast engine) or history_mean
    pub demand_method: String,
    pub unit_cost: f64,
    pub holding_cost_rate: f64,
}

/// Demand is measured in complete weeks (robust to lumpy posting days) and converted to daily:
/// mean/7, sd/sqrt(7).
/// Safety stock = z * sqrt((L+R)*sd_d^2 + mu_d^2*sd_L^2); reorder point = mu_d*(L+R) + SS;
/// service level from the critical ratio (stock-out vs holding cost) or the criticality class.
async fn replenishment(
    State(s): State<AppState>,
    Query(q): Query<ReplQuery>,
) -> AppResult<Json<Vec<ReplItem>>> {
    let use_forecast = match q.demand_source.as_deref().unwrap_or("forecast") {
        "forecast" => true,
        "history" => false,
        _ => return Err(AppError::bad("demand_source must be forecast or history")),
    };
    Ok(Json(
        compute_replenishment(
            &s.db,
            q.warehouse_id,
            q.lookback_days.unwrap_or(90),
            q.as_of.unwrap_or_else(|| Utc::now().date_naive()),
            use_forecast,
            q.history_weeks.unwrap_or(104),
        )
        .await?,
    ))
}

/// Shared by the report, the automatic purchase-order run and the planning cycle.
pub async fn compute_replenishment(
    db: &sea_orm::DatabaseConnection,
    warehouse_id: Option<Uuid>,
    lookback: i32,
    as_of: NaiveDate,
    use_forecast: bool,
    history_weeks: i32,
) -> AppResult<Vec<ReplItem>> {
    if !(14..=730).contains(&lookback) {
        return Err(AppError::bad("lookback_days must be between 14 and 730"));
    }
    let rows = ReplRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "WITH weeks AS (
           SELECT generate_series(date_trunc('week', $3::date)::date - 7 * (($2::int + 6) / 7),
                                  date_trunc('week', $3::date)::date - 7, interval '1 week')::date AS w),
         iss AS (
           SELECT sm.product_id, date_trunc('week', sm.created_at)::date AS w, SUM(-sm.quantity_delta)::float8 AS q
           FROM stock_movements sm JOIN locations l ON l.id = sm.location_id
           WHERE sm.movement_type = 'issue'
             AND sm.created_at::date >= date_trunc('week', $3::date)::date - 7 * (($2::int + 6) / 7)
             AND sm.created_at::date < date_trunc('week', $3::date)::date
             AND ($1::uuid IS NULL OR l.warehouse_id = $1)
           GROUP BY 1, 2),
         dem AS (
           SELECT p.id AS product_id, (AVG(COALESCE(i.q, 0)) / 7.0)::float8 AS mu,
                  (COALESCE(STDDEV_SAMP(COALESCE(i.q, 0)), 0) / SQRT(7.0))::float8 AS sd,
                  (COALESCE(AVG(COALESCE(i.q, 0)) FILTER (WHERE weeks.w >= date_trunc('week', $3::date)::date - 28), 0) / 7.0)::float8 AS mu30
           FROM products p CROSS JOIN weeks LEFT JOIN iss i ON i.product_id = p.id AND i.w = weeks.w
           WHERE p.active GROUP BY p.id),
         lt AS (
           SELECT gr.product_id, COUNT(*)::bigint AS n,
                  AVG((gr.received_on - po.order_date)::float8) AS mean,
                  COALESCE(STDDEV_SAMP((gr.received_on - po.order_date)::float8), 0) AS sd
           FROM goods_receipts gr JOIN purchase_orders po ON po.id = gr.purchase_order_id
           GROUP BY gr.product_id),
         lts AS (
           SELECT sp.product_id, COUNT(*)::bigint AS n, AVG(x.d) AS mean, COALESCE(STDDEV_SAMP(x.d), 0) AS sd
           FROM (SELECT pol.product_id, po.supplier_id FROM purchase_order_lines pol
                   JOIN purchase_orders po ON po.id = pol.purchase_order_id
                 UNION SELECT product_id, supplier_id FROM product_suppliers WHERE active) sp
           JOIN (SELECT po.supplier_id, (gr.received_on - po.order_date)::float8 AS d FROM goods_receipts gr
                   JOIN purchase_orders po ON po.id = gr.purchase_order_id) x ON x.supplier_id = sp.supplier_id
           GROUP BY sp.product_id),
         stk AS (
           SELECT sl.product_id, SUM(sl.quantity - sl.reserved - sl.blocked)::bigint AS available
           FROM stock_levels sl JOIN locations l ON l.id = sl.location_id
           WHERE ($1::uuid IS NULL OR l.warehouse_id = $1) GROUP BY sl.product_id),
         oo AS (
           SELECT pol.product_id, SUM(pol.quantity_ordered - pol.quantity_received)::bigint AS on_order
           FROM purchase_order_lines pol JOIN purchase_orders po ON po.id = pol.purchase_order_id
           WHERE po.status IN ('submitted', 'partially_received')
             AND ($1::uuid IS NULL OR po.warehouse_id = $1)
           GROUP BY pol.product_id)
         SELECT p.id AS product_id, p.sku, p.name, p.criticality_class, p.unit_cost::float8 AS unit_cost,
                p.holding_cost_rate::float8 AS holding_cost_rate, p.stockout_cost::float8 AS stockout_cost,
                p.review_period_days, p.lead_time_days, p.reorder_point, p.reorder_qty,
                dem.mu, dem.sd, dem.mu30, lt.n AS lt_n, lt.mean AS lt_mean, lt.sd AS lt_sd, lts.n AS lts_n, lts.mean AS lts_mean, lts.sd AS lts_sd,
                COALESCE(stk.available, 0) AS available, COALESCE(oo.on_order, 0) AS on_order
         FROM products p
         JOIN dem ON dem.product_id = p.id
         LEFT JOIN lt ON lt.product_id = p.id
         LEFT JOIN lts ON lts.product_id = p.id
         LEFT JOIN stk ON stk.product_id = p.id
         LEFT JOIN oo ON oo.product_id = p.id
         WHERE dem.mu > 0 OR p.reorder_point > 0
         ORDER BY p.sku",
        [warehouse_id.into(), lookback.into(), as_of.into()],
    ))
    .all(db)
    .await?;

    let forecasts = if use_forecast {
        super::forecasting::fetch_selected(db, warehouse_id, as_of, history_weeks).await?
    } else {
        std::collections::HashMap::new()
    };

    let items = rows
        .into_iter()
        .map(|r| {
            let (mu, sd, demand_method) = match forecasts.get(&r.product_id) {
                Some(sel) => (
                    sel.forecast_per_week / 7.0,
                    sel.error_rmse.map(|e| e / 7f64.sqrt()).unwrap_or(r.sd),
                    sel.method.name().to_string(),
                ),
                None => (r.mu, r.sd, "history_mean".to_string()),
            };
            let (lead, lead_sd, source) = match (r.lt_n, r.lt_mean, r.lt_sd) {
                (Some(n), Some(m), Some(sd)) if n >= MIN_LT_SAMPLES => (m, sd, "observed_receipts"),
                _ => match (r.lts_n, r.lts_mean, r.lts_sd) {
                    // too little history for this product: pool the receipts of its suppliers
                    (Some(n), Some(m), Some(sd)) if n >= MIN_LT_SAMPLES => (m, sd, "supplier_receipts"),
                    _ => {
                        let l = r.lead_time_days as f64;
                        (l, l * DEFAULT_LT_CV, "product_default")
                    }
                },
            };
            let review = r.review_period_days as f64;
            let sl = service_level(&r.criticality_class, r.stockout_cost, r.unit_cost, r.holding_cost_rate, lead + review);
            let z = norm_inv(sl);
            let ss = safety_stock(z, mu, sd, lead, lead_sd, review);
            let rop = (mu * (lead + review) + ss).ceil();
            let up_to = (rop + mu * review).ceil();
            let position = r.available + r.on_order;
            let reorder_now = mu > 0.0 && (position as f64) <= rop;
            let mut suggested = if reorder_now { (up_to - position as f64).max(0.0).ceil() as i64 } else { 0 };
            if reorder_now && r.reorder_qty > 0 {
                suggested = suggested.max(r.reorder_qty);
            }
            let ratio = (r.mu > 0.0).then(|| r.mu30 / r.mu);
            let trend = match ratio {
                Some(x) if x > 1.25 => "rising",
                Some(x) if x < 0.75 => "falling",
                _ => "stable",
            };
            let action = if r.reorder_point == 0 {
                "set"
            } else {
                let rel = (rop - r.reorder_point as f64) / r.reorder_point as f64;
                if rel > 0.2 { "raise" } else if rel < -0.2 { "lower" } else { "ok" }
            };
            ReplItem {
                product_id: r.product_id,
                sku: r.sku,
                name: r.name,
                criticality_class: r.criticality_class,
                daily_demand_mean: mu,
                daily_demand_sd: sd,
                recent_demand_ratio: ratio,
                demand_trend: trend,
                lead_time_days: lead,
                lead_time_sd_days: lead_sd,
                lead_time_source: source,
                review_period_days: r.review_period_days,
                service_level: sl,
                safety_stock: ss.ceil() as i64,
                recommended_reorder_point: rop as i64,
                order_up_to_level: up_to as i64,
                current_reorder_point: r.reorder_point,
                reorder_point_action: action,
                available: r.available,
                on_order: r.on_order,
                inventory_position: position,
                coverage_days: (mu > 0.0).then(|| r.available as f64 / mu),
                reorder_now,
                suggested_order_qty: suggested,
                demand_method,
                unit_cost: r.unit_cost,
                holding_cost_rate: r.holding_cost_rate,
            }
        })
        .collect();
    Ok(items)
}

// ---- ABC / XYZ -----------------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct AbcQuery {
    warehouse_id: Option<Uuid>,
    /// default 365, 28..=1095
    lookback_days: Option<i32>,
    as_of: Option<NaiveDate>,
}

#[derive(Debug, FromQueryResult)]
struct AbcRow {
    product_id: Uuid,
    sku: String,
    name: String,
    units: f64,
    value: f64,
    mu: f64,
    sd: f64,
}

#[derive(Serialize)]
pub struct AbcItem {
    product_id: Uuid,
    sku: String,
    name: String,
    units_issued: f64,
    issue_value: f64,
    cumulative_share: f64,
    abc: &'static str,
    /// coefficient of variation of weekly demand
    demand_cv: f64,
    xyz: &'static str,
    policy: &'static str,
}

fn policy(abc: &str, xyz: &str) -> &'static str {
    match (abc, xyz) {
        ("A", "X") | ("A", "Y") => "tight control: frequent review, high service level, supplier collaboration",
        ("A", _) => "high value, erratic: hold safety stock selectively, consider make-to-order or consignment",
        ("B", "X") | ("B", "Y") => "standard statistical replenishment",
        ("B", _) | ("C", "Z") => "review less often; order on demand where lead time allows",
        _ => "low-touch: larger, infrequent orders",
    }
}

/// Value ranking (A = first 80% of issue value, B = next 15%, C = rest) crossed with demand
/// variability (X: CV<0.5, Y: <1.0, Z: >=1.0) on weekly demand.
async fn abc_xyz(State(s): State<AppState>, Query(q): Query<AbcQuery>) -> AppResult<Json<Vec<AbcItem>>> {
    let lookback = q.lookback_days.unwrap_or(365);
    if !(28..=1095).contains(&lookback) {
        return Err(AppError::bad("lookback_days must be between 28 and 1095"));
    }
    let rows = AbcRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "WITH weeks AS (
           SELECT generate_series(date_trunc('week', $3::date - $2::int)::date,
                                  date_trunc('week', $3::date)::date, interval '1 week')::date AS w),
         iss AS (
           SELECT sm.product_id, date_trunc('week', sm.created_at)::date AS w, SUM(-sm.quantity_delta)::float8 AS q
           FROM stock_movements sm JOIN locations l ON l.id = sm.location_id
           WHERE sm.movement_type = 'issue' AND sm.created_at::date >= $3::date - $2::int
             AND ($1::uuid IS NULL OR l.warehouse_id = $1)
           GROUP BY 1, 2)
         SELECT p.id AS product_id, p.sku, p.name,
                SUM(COALESCE(i.q, 0))::float8 AS units,
                (SUM(COALESCE(i.q, 0)) * p.unit_cost::float8)::float8 AS value,
                AVG(COALESCE(i.q, 0))::float8 AS mu,
                COALESCE(STDDEV_SAMP(COALESCE(i.q, 0)), 0)::float8 AS sd
         FROM products p CROSS JOIN weeks LEFT JOIN iss i ON i.product_id = p.id AND i.w = weeks.w
         GROUP BY p.id, p.sku, p.name, p.unit_cost
         HAVING SUM(COALESCE(i.q, 0)) > 0
         ORDER BY value DESC, p.sku",
        [q.warehouse_id.into(), lookback.into(), q.as_of.unwrap_or_else(|| Utc::now().date_naive()).into()],
    ))
    .all(&s.db)
    .await?;

    let total: f64 = rows.iter().map(|r| r.value).sum();
    let mut cum = 0.0;
    let items = rows
        .into_iter()
        .map(|r| {
            let before = if total > 0.0 { cum / total } else { 0.0 };
            cum += r.value;
            let abc = if before < 0.80 { "A" } else if before < 0.95 { "B" } else { "C" };
            let cv = if r.mu > 0.0 { r.sd / r.mu } else { 0.0 };
            let xyz = if cv < 0.5 { "X" } else if cv < 1.0 { "Y" } else { "Z" };
            AbcItem {
                product_id: r.product_id,
                sku: r.sku,
                name: r.name,
                units_issued: r.units,
                issue_value: r.value,
                cumulative_share: if total > 0.0 { cum / total } else { 0.0 },
                abc,
                demand_cv: cv,
                xyz,
                policy: policy(abc, xyz),
            }
        })
        .collect();
    Ok(Json(items))
}

// ---- forecasts -------------------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ForecastIn {
    warehouse_id: Uuid,
    product_id: Uuid,
    period_start: NaiveDate,
    period_days: Option<i32>,
    /// baseline | adjusted (default baseline)
    forecast_type: Option<String>,
    forecast_qty: f64,
    uplift_pct: Option<f64>,
}

#[derive(Deserialize)]
pub struct ForecastBatch {
    items: Vec<ForecastIn>,
}

/// Upsert up to 5,000 forecasts per call.
async fn upsert_forecasts(
    State(s): State<AppState>,
    Json(b): Json<ForecastBatch>,
) -> AppResult<(StatusCode, Json<Value>)> {
    if b.items.is_empty() || b.items.len() > 5000 {
        return Err(AppError::bad("items must contain 1 to 5000 forecasts"));
    }
    let txn = s.db.begin().await?;
    for f in &b.items {
        let ftype = f.forecast_type.as_deref().unwrap_or("baseline");
        if !matches!(ftype, "baseline" | "adjusted") {
            return Err(AppError::bad("forecast_type must be baseline or adjusted"));
        }
        if f.forecast_qty < 0.0 || f.period_days.unwrap_or(7) <= 0 {
            return Err(AppError::bad("forecast_qty must be >= 0 and period_days > 0"));
        }
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO demand_forecasts (id, warehouse_id, product_id, period_start, period_days, \
                 forecast_type, forecast_qty, uplift_pct) VALUES ($1,$2,$3,$4,$5,$6,$7,$8) \
             ON CONFLICT (warehouse_id, product_id, period_start, forecast_type) \
             DO UPDATE SET forecast_qty = EXCLUDED.forecast_qty, period_days = EXCLUDED.period_days, \
                           uplift_pct = EXCLUDED.uplift_pct",
            [
                Uuid::new_v4().into(),
                f.warehouse_id.into(),
                f.product_id.into(),
                f.period_start.into(),
                f.period_days.unwrap_or(7).into(),
                ftype.into(),
                f.forecast_qty.into(),
                f.uplift_pct.into(),
            ],
        ))
        .await?;
    }
    txn.commit().await?;
    Ok((StatusCode::CREATED, Json(json!({ "upserted": b.items.len(), "at": Utc::now() }))))
}

#[derive(Deserialize)]
pub struct AccQuery {
    warehouse_id: Option<Uuid>,
    product_id: Option<Uuid>,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
}

#[derive(Debug, FromQueryResult)]
struct AccRow {
    forecast_type: String,
    n: i64,
    forecast: f64,
    actual: f64,
    abs_err: f64,
    err: f64,
}

#[derive(Serialize)]
pub struct AccItem {
    forecast_type: String,
    periods: i64,
    forecast_units: f64,
    actual_units: f64,
    /// sum|forecast-actual| / sum actual
    wape: Option<f64>,
    /// sum(forecast-actual) / sum actual; positive = over-forecasting
    bias: Option<f64>,
    mean_abs_error: f64,
}

/// Compares each forecast with the units actually issued in its period (completed periods only),
/// split by forecast type so manual "adjusted" forecasts can be checked against baselines.
async fn forecast_accuracy(
    State(s): State<AppState>,
    Query(q): Query<AccQuery>,
) -> AppResult<Json<Vec<AccItem>>> {
    let rows = AccRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "WITH f AS (
           SELECT * FROM demand_forecasts
           WHERE period_start + period_days <= CURRENT_DATE
             AND ($1::uuid IS NULL OR warehouse_id = $1) AND ($2::uuid IS NULL OR product_id = $2)
             AND ($3::date IS NULL OR period_start >= $3) AND ($4::date IS NULL OR period_start <= $4)),
         act AS (
           SELECT f.id, COALESCE(SUM(-sm.quantity_delta), 0)::float8 AS actual
           FROM f
           LEFT JOIN locations l ON l.warehouse_id = f.warehouse_id
           LEFT JOIN stock_movements sm ON sm.location_id = l.id AND sm.product_id = f.product_id
                AND sm.movement_type = 'issue'
                AND sm.created_at::date >= f.period_start AND sm.created_at::date < f.period_start + f.period_days
           GROUP BY f.id)
         SELECT f.forecast_type, COUNT(*)::bigint AS n, SUM(f.forecast_qty)::float8 AS forecast,
                SUM(a.actual)::float8 AS actual, SUM(ABS(f.forecast_qty - a.actual))::float8 AS abs_err,
                SUM(f.forecast_qty - a.actual)::float8 AS err
         FROM f JOIN act a ON a.id = f.id GROUP BY f.forecast_type ORDER BY f.forecast_type",
        [q.warehouse_id.into(), q.product_id.into(), q.from.into(), q.to.into()],
    ))
    .all(&s.db)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| {
                AccItem {
                    forecast_type: r.forecast_type,
                    periods: r.n,
                    forecast_units: r.forecast,
                    actual_units: r.actual,
                    wape: (r.actual > 0.0).then(|| r.abs_err / r.actual),
                    bias: (r.actual > 0.0).then(|| r.err / r.actual),
                    mean_abs_error: r.abs_err / r.n as f64,
                }
            })
            .collect(),
    ))
}

// ---- excess cover / MOQ burden ---------------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ExcessQuery {
    warehouse_id: Option<Uuid>,
    as_of: Option<NaiveDate>,
    lookback_days: Option<i32>,
    /// list products holding at least this many days of cover (default 90)
    min_cover_days: Option<f64>,
}

#[derive(Debug, FromQueryResult)]
struct MoqRow {
    product_id: Uuid,
    moq: i64,
}

/// Stock above the order-up-to level, valued at cost with its annual holding charge, and whether the
/// supplier MOQ alone explains it. ERP data: median 24 days of cover but a mean of 52 and a maximum of 414,
/// with 86% of orders placed at exactly the MOQ.
async fn excess_cover(State(s): State<AppState>, Query(q): Query<ExcessQuery>) -> AppResult<Json<Value>> {
    let min_cover = q.min_cover_days.unwrap_or(90.0);
    let items = compute_replenishment(
        &s.db,
        q.warehouse_id,
        q.lookback_days.unwrap_or(90),
        q.as_of.unwrap_or_else(|| Utc::now().date_naive()),
        true,
        104,
    )
    .await?;
    let moqs: HashMap<Uuid, i64> = MoqRow::find_by_statement(Statement::from_string(
        DbBackend::Postgres,
        "SELECT product_id, MAX(min_order_qty)::bigint AS moq FROM product_suppliers WHERE active GROUP BY product_id".to_string(),
    ))
    .all(&s.db)
    .await?
    .into_iter()
    .map(|m| (m.product_id, m.moq))
    .collect();
    let mut out = Vec::new();
    let (mut total_value, mut total_holding, mut moq_driven) = (0.0, 0.0, 0usize);
    for it in items.iter().filter(|i| i.available > 0) {
        let on_hand = (it.available + it.on_order) as f64;
        let mu = it.daily_demand_mean;
        let cover = (mu > 0.0).then(|| it.available as f64 / mu);
        let no_demand = mu <= 0.0;
        if !no_demand && cover.map(|c| c < min_cover).unwrap_or(true) {
            continue;
        }
        let excess_units = if no_demand { on_hand } else { (on_hand - it.order_up_to_level as f64).max(0.0) };
        if excess_units <= 0.0 {
            continue;
        }
        let value = excess_units * it.unit_cost;
        let holding = value * it.holding_cost_rate;
        let moq = moqs.get(&it.product_id).copied();
        let moq_cover = moq.filter(|_| mu > 0.0).map(|m| m as f64 / mu);
        let driven = matches!((moq_cover, cover), (Some(m), Some(c)) if m >= 30.0 && m >= 0.5 * c);
        if driven {
            moq_driven += 1;
        }
        total_value += value;
        total_holding += holding;
        out.push(json!({
            "sku": it.sku, "name": it.name, "available": it.available, "on_order": it.on_order,
            "daily_demand": mu, "cover_days": cover, "order_up_to_level": it.order_up_to_level,
            "excess_units": excess_units.round(), "excess_value": value, "annual_holding_cost_of_excess": holding,
            "max_min_order_qty": moq, "moq_cover_days": moq_cover, "moq_driven": driven,
            "reason": if no_demand { "no_demand_in_window" } else if driven { "moq" } else { "over_ordering_or_demand_drop" },
        }));
    }
    out.sort_by(|a, b| b["excess_value"].as_f64().partial_cmp(&a["excess_value"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));
    Ok(Json(json!({
        "min_cover_days": min_cover,
        "summary": {"products": out.len(), "moq_driven_products": moq_driven, "excess_value": total_value, "annual_holding_cost": total_holding},
        "products": out,
    })))
}
