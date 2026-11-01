//! Demand forecasting endpoints: statistical methods, automatic selection by backtest,
//! and persistence of forecasts so they flow into accuracy tracking and replenishment.
use std::collections::HashMap;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use chrono::{Datelike, Duration, NaiveDate, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, FromQueryResult, Statement, TransactionTrait};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::supply_chain::{
    error::{AppError, AppResult},
    forecast::{classify, select, Backtest, DemandClass, Method, Selected},
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/forecast/demand", get(forecast_demand))
        .route("/forecasts/generate", post(generate))
}

#[derive(Debug, FromQueryResult)]
struct SeriesRow {
    product_id: Uuid,
    sku: String,
    name: String,
    w: NaiveDate,
    q: f64,
}

pub struct Series {
    pub product_id: Uuid,
    pub sku: String,
    pub name: String,
    /// first week of the (leading-zero-trimmed) series
    pub start: NaiveDate,
    pub y: Vec<f64>,
}

pub fn monday(d: NaiveDate) -> NaiveDate {
    d - Duration::days(d.weekday().num_days_from_monday() as i64)
}

/// Weekly issue series per product over the last `weeks` complete weeks before `as_of`'s week.
/// Leading zeros (before the first issue) are trimmed: the item did not exist yet.
pub async fn fetch_series(
    db: &DatabaseConnection,
    warehouse: Option<Uuid>,
    product: Option<Uuid>,
    as_of: NaiveDate,
    weeks: i32,
) -> AppResult<Vec<Series>> {
    if !(8..=260).contains(&weeks) {
        return Err(AppError::bad("history_weeks must be between 8 and 260"));
    }
    let rows = SeriesRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "WITH weeks AS (
           SELECT generate_series(date_trunc('week', $2::date)::date - 7 * $3::int,
                                  date_trunc('week', $2::date)::date - 7, interval '1 week')::date AS w),
         iss AS (
           SELECT sm.product_id, date_trunc('week', sm.created_at)::date AS w, SUM(-sm.quantity_delta)::float8 AS q
           FROM stock_movements sm JOIN locations l ON l.id = sm.location_id
           WHERE sm.movement_type = 'issue'
             AND sm.created_at::date >= date_trunc('week', $2::date)::date - 7 * $3::int
             AND sm.created_at::date < date_trunc('week', $2::date)::date
             AND ($1::uuid IS NULL OR l.warehouse_id = $1) AND ($4::uuid IS NULL OR sm.product_id = $4)
           GROUP BY 1, 2)
         SELECT p.id AS product_id, p.sku, p.name, weeks.w, COALESCE(i.q, 0)::float8 AS q
         FROM products p CROSS JOIN weeks LEFT JOIN iss i ON i.product_id = p.id AND i.w = weeks.w
         WHERE p.active AND p.id IN (SELECT product_id FROM iss)
         ORDER BY p.sku, weeks.w",
        [warehouse.into(), as_of.into(), weeks.into(), product.into()],
    ))
    .all(db)
    .await?;
    let mut out: Vec<Series> = Vec::new();
    for r in rows {
        match out.last_mut() {
            Some(s) if s.product_id == r.product_id => {
                if !(s.y.is_empty() && r.q == 0.0) {
                    if s.y.is_empty() {
                        s.start = r.w;
                    }
                    s.y.push(r.q);
                }
            }
            _ => out.push(Series {
                product_id: r.product_id,
                sku: r.sku,
                name: r.name,
                start: r.w,
                y: if r.q == 0.0 { vec![] } else { vec![r.q] },
            }),
        }
    }
    Ok(out)
}

/// Forecast engine run for every product with demand history (used by replenishment).
pub async fn fetch_selected(
    db: &DatabaseConnection,
    warehouse: Option<Uuid>,
    as_of: NaiveDate,
    weeks: i32,
) -> AppResult<HashMap<Uuid, Selected>> {
    Ok(fetch_series(db, warehouse, None, as_of, weeks)
        .await?
        .into_iter()
        .filter(|s| !s.y.is_empty())
        .map(|s| (s.product_id, select(&s.y, None)))
        .collect())
}

#[derive(Deserialize)]
pub struct ForecastQuery {
    warehouse_id: Option<Uuid>,
    product_id: Option<Uuid>,
    /// auto (default) | ma13 | ses | sba | tsb | ses_tsb
    method: Option<String>,
    horizon_weeks: Option<i32>,
    history_weeks: Option<i32>,
    as_of: Option<NaiveDate>,
}

#[derive(Serialize)]
pub struct HorizonPoint {
    week_start: NaiveDate,
    quantity: f64,
}

#[derive(Serialize)]
pub struct ProductForecast {
    product_id: Uuid,
    sku: String,
    name: String,
    demand_class: DemandClass,
    /// average inter-demand interval in weeks
    adi: Option<f64>,
    cv2: f64,
    history_weeks: usize,
    method: &'static str,
    forecast_per_week: f64,
    horizon: Vec<HorizonPoint>,
    /// weekly one-step forecast error sd from the backtest
    error_rmse: Option<f64>,
    backtest_start: Option<NaiveDate>,
    backtests: Vec<Backtest>,
}

fn parse_method(m: Option<&str>) -> AppResult<Option<Method>> {
    match m.unwrap_or("auto") {
        "auto" => Ok(None),
        other => Method::parse(other)
            .map(Some)
            .ok_or_else(|| AppError::bad("method must be auto, ma13, ses, sba, tsb or ses_tsb")),
    }
}

async fn run(
    db: &DatabaseConnection,
    q: &ForecastQuery,
) -> AppResult<(NaiveDate, i32, Vec<(Series, Selected)>)> {
    let as_of = q.as_of.unwrap_or_else(|| Utc::now().date_naive());
    let weeks = q.history_weeks.unwrap_or(104);
    let forced = parse_method(q.method.as_deref())?;
    let out = fetch_series(db, q.warehouse_id, q.product_id, as_of, weeks)
        .await?
        .into_iter()
        .filter(|s| !s.y.is_empty())
        .map(|s| {
            let sel = select(&s.y, forced);
            (s, sel)
        })
        .collect();
    Ok((as_of, q.horizon_weeks.unwrap_or(4), out))
}

/// Statistical demand forecast per product. History is weekly issues (complete weeks only).
async fn forecast_demand(
    State(s): State<AppState>,
    Query(q): Query<ForecastQuery>,
) -> AppResult<Json<Vec<ProductForecast>>> {
    let (as_of, horizon, set) = run(&s.db, &q).await?;
    if !(1..=52).contains(&horizon) {
        return Err(AppError::bad("horizon_weeks must be between 1 and 52"));
    }
    let next = monday(as_of);
    Ok(Json(
        set.into_iter()
            .map(|(ser, sel)| {
                let (class, adi, cv2) = classify(&ser.y);
                let n = ser.y.len();
                let test_len = (n / 4).clamp(8, 26);
                ProductForecast {
                    product_id: ser.product_id,
                    sku: ser.sku,
                    name: ser.name,
                    demand_class: class,
                    adi: adi.is_finite().then_some(adi),
                    cv2,
                    history_weeks: n,
                    method: sel.method.name(),
                    forecast_per_week: sel.forecast_per_week,
                    horizon: (0..horizon)
                        .map(|i| HorizonPoint { week_start: next + Duration::days(7 * i as i64), quantity: sel.forecast_per_week })
                        .collect(),
                    error_rmse: sel.error_rmse,
                    backtest_start: (!sel.backtests.is_empty())
                        .then(|| ser.start + Duration::days(7 * (n - test_len) as i64)),
                    backtests: sel.backtests,
                }
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct GenerateReq {
    pub warehouse_id: Uuid,
    pub method: Option<String>,
    pub horizon_weeks: Option<i32>,
    pub history_weeks: Option<i32>,
    pub as_of: Option<NaiveDate>,
}

/// Compute forecasts for a warehouse and store them as `statistical` forecasts so that
/// `/reports/forecast-accuracy` can compare them with baseline/adjusted ones once the weeks close.
async fn generate(
    State(s): State<AppState>,
    Json(r): Json<GenerateReq>,
) -> AppResult<(StatusCode, Json<Value>)> {
    Ok((StatusCode::CREATED, Json(generate_for(&s.db, r).await?)))
}

pub async fn generate_for(db: &DatabaseConnection, r: GenerateReq) -> AppResult<Value> {
    let horizon = r.horizon_weeks.unwrap_or(4);
    if !(1..=52).contains(&horizon) {
        return Err(AppError::bad("horizon_weeks must be between 1 and 52"));
    }
    let q = ForecastQuery {
        warehouse_id: Some(r.warehouse_id),
        product_id: None,
        method: r.method,
        horizon_weeks: Some(horizon),
        history_weeks: r.history_weeks,
        as_of: r.as_of,
    };
    let (as_of, _, set) = run(db, &q).await?;
    let next = monday(as_of);
    let txn = db.begin().await?;
    let mut rows = 0usize;
    for (ser, sel) in &set {
        for i in 0..horizon {
            txn.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO demand_forecasts (id, warehouse_id, product_id, period_start, period_days, forecast_type, forecast_qty) \
                 VALUES ($1,$2,$3,$4,7,'statistical',$5) \
                 ON CONFLICT (warehouse_id, product_id, period_start, forecast_type) \
                 DO UPDATE SET forecast_qty = EXCLUDED.forecast_qty",
                [
                    Uuid::new_v4().into(),
                    r.warehouse_id.into(),
                    ser.product_id.into(),
                    (next + Duration::days(7 * i as i64)).into(),
                    sel.forecast_per_week.into(),
                ],
            ))
            .await?;
            rows += 1;
        }
    }
    txn.commit().await?;
    Ok(json!({ "products": set.len(), "forecast_rows": rows, "first_week": next, "horizon_weeks": horizon }))
}
