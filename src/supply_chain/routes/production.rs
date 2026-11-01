//! Manufacturing runs: good output enters stock, scrap is recorded, cost is compared with standard.
use axum::{
    extract::{Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use chrono::{NaiveDate, Utc};
use sea_orm::{
    prelude::Decimal, ActiveModelTrait, ColumnTrait, DbBackend, EntityTrait, FromQueryResult, QueryFilter,
    QueryOrder, QuerySelect, Set, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{clean_lot, page_limit};
use crate::supply_chain::{
    entities::{location, product, production_run},
    error::{AppError, AppResult},
    state::AppState,
    stock::{add_stock, default_expiry, ensure_storage_compatible, record_movement},
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/production-runs", post(create_run).get(list_runs))
        .route("/reports/production-yield", get(yield_report))
}

#[derive(Deserialize)]
pub struct NewRun {
    product_id: Uuid,
    location_id: Uuid,
    /// units put into the run
    quantity_started: i64,
    /// either a percentage or an absolute count of scrapped units
    scrap_pct: Option<f64>,
    scrap_qty: Option<i64>,
    hours: Option<f64>,
    cost: Option<Decimal>,
    produced_on: Option<NaiveDate>,
    lot_number: Option<String>,
    expiry_date: Option<NaiveDate>,
}

async fn create_run(State(s): State<AppState>, Json(r): Json<NewRun>) -> AppResult<(StatusCode, Json<production_run::Model>)> {
    if r.quantity_started <= 0 {
        return Err(AppError::bad("quantity_started must be greater than zero"));
    }
    if r.scrap_pct.is_some() && r.scrap_qty.is_some() {
        return Err(AppError::bad("give scrap_pct or scrap_qty, not both"));
    }
    let scrap = match (r.scrap_qty, r.scrap_pct) {
        (Some(q), _) => q,
        (None, Some(p)) => {
            super::check_pct("scrap_pct", Some(p))?;
            (r.quantity_started as f64 * p / 100.0).round() as i64
        }
        _ => 0,
    };
    if scrap < 0 || scrap > r.quantity_started {
        return Err(AppError::bad("scrap must be between 0 and quantity_started"));
    }
    if matches!(r.hours, Some(h) if h < 0.0 || h.is_nan()) || matches!(r.cost, Some(c) if c.is_sign_negative()) {
        return Err(AppError::bad("hours and cost must not be negative"));
    }
    let prod = get_or_404!(product::Entity, &s.db, r.product_id, "product");
    let loc = get_or_404!(location::Entity, &s.db, r.location_id, "location");
    if !loc.active {
        return Err(AppError::conflict("location is inactive"));
    }
    ensure_storage_compatible(&prod, &loc)?;
    let good = r.quantity_started - scrap;
    let produced_on = r.produced_on.unwrap_or_else(|| Utc::now().date_naive());
    let lot = clean_lot(&r.lot_number);
    let id = Uuid::new_v4();
    let txn = s.db.begin().await?;
    if good > 0 {
        add_stock(&txn, prod.id, loc.id, &lot, good, default_expiry(&prod, produced_on, r.expiry_date)).await?;
        record_movement(&txn, prod.id, loc.id, &lot, "production", good, Some("production_run"), Some(id), None).await?;
    }
    let m = production_run::ActiveModel {
        id: Set(id),
        product_id: Set(prod.id),
        location_id: Set(loc.id),
        lot_number: Set(lot),
        quantity_started: Set(r.quantity_started),
        quantity_good: Set(good),
        quantity_scrap: Set(scrap),
        hours: Set(r.hours.unwrap_or(0.0)),
        cost: Set(r.cost.unwrap_or_default()),
        produced_on: Set(produced_on),
        created_at: Set(Utc::now()),
    }
    .insert(&txn)
    .await?;
    txn.commit().await?;
    Ok((StatusCode::CREATED, Json(m)))
}

#[derive(Deserialize)]
pub struct RunQuery {
    product_id: Option<Uuid>,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    limit: Option<u64>,
    offset: Option<u64>,
}

async fn list_runs(State(s): State<AppState>, Query(q): Query<RunQuery>) -> AppResult<Json<Vec<production_run::Model>>> {
    let mut sel = production_run::Entity::find();
    if let Some(v) = q.product_id {
        sel = sel.filter(production_run::Column::ProductId.eq(v));
    }
    if let Some(v) = q.from {
        sel = sel.filter(production_run::Column::ProducedOn.gte(v));
    }
    if let Some(v) = q.to {
        sel = sel.filter(production_run::Column::ProducedOn.lte(v));
    }
    Ok(Json(
        sel.order_by_desc(production_run::Column::ProducedOn)
            .limit(page_limit(q.limit))
            .offset(q.offset.unwrap_or(0))
            .all(&s.db)
            .await?,
    ))
}

#[derive(Deserialize)]
pub struct YieldQuery {
    warehouse_id: Option<Uuid>,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
}

#[derive(Debug, FromQueryResult)]
struct YRow {
    product_id: Uuid,
    sku: String,
    name: String,
    runs: i64,
    started: i64,
    good: i64,
    scrap: i64,
    hours: f64,
    cost: f64,
    standard_unit_cost: f64,
}

#[derive(Serialize)]
struct YItem {
    product_id: Uuid,
    sku: String,
    name: String,
    runs: i64,
    units_started: i64,
    units_good: i64,
    units_scrapped: i64,
    scrap_rate: f64,
    units_per_hour: Option<f64>,
    total_cost: f64,
    /// cost / good units
    actual_unit_cost: Option<f64>,
    standard_unit_cost: f64,
    /// (actual - standard) / standard
    cost_variance: Option<f64>,
}

fn item(r: &YRow) -> YItem {
    let actual = (r.good > 0).then(|| r.cost / r.good as f64);
    YItem {
        product_id: r.product_id,
        sku: r.sku.clone(),
        name: r.name.clone(),
        runs: r.runs,
        units_started: r.started,
        units_good: r.good,
        units_scrapped: r.scrap,
        scrap_rate: if r.started > 0 { r.scrap as f64 / r.started as f64 } else { 0.0 },
        units_per_hour: (r.hours > 0.0).then(|| r.good as f64 / r.hours),
        total_cost: r.cost,
        actual_unit_cost: actual,
        standard_unit_cost: r.standard_unit_cost,
        cost_variance: actual.filter(|_| r.standard_unit_cost > 0.0).map(|a| a / r.standard_unit_cost - 1.0),
    }
}

/// Yield, throughput and actual-vs-standard unit cost per product. In the Cacau Prime data every product
/// ran about 13% above its standard cost with a flat ~1.25% scrap rate, independent of batch size.
async fn yield_report(State(s): State<AppState>, Query(q): Query<YieldQuery>) -> AppResult<Json<Value>> {
    let rows = YRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT p.id AS product_id, p.sku, p.name, COUNT(r.id)::bigint AS runs, SUM(r.quantity_started)::bigint AS started, \
                SUM(r.quantity_good)::bigint AS good, SUM(r.quantity_scrap)::bigint AS scrap, SUM(r.hours)::float8 AS hours, \
                SUM(r.cost)::float8 AS cost, p.unit_cost::float8 AS standard_unit_cost \
         FROM production_runs r JOIN products p ON p.id = r.product_id JOIN locations l ON l.id = r.location_id \
         WHERE ($1::date IS NULL OR r.produced_on >= $1) AND ($2::date IS NULL OR r.produced_on <= $2) \
           AND ($3::uuid IS NULL OR l.warehouse_id = $3) \
         GROUP BY p.id, p.sku, p.name, p.unit_cost ORDER BY p.sku",
        [q.from.into(), q.to.into(), q.warehouse_id.into()],
    ))
    .all(&s.db)
    .await?;
    let items: Vec<YItem> = rows.iter().map(item).collect();
    let (st, sc): (i64, i64) = rows.iter().fold((0, 0), |a, r| (a.0 + r.started, a.1 + r.scrap));
    let std_cost: f64 = rows.iter().map(|r| r.good as f64 * r.standard_unit_cost).sum();
    let act_cost: f64 = rows.iter().map(|r| r.cost).sum();
    Ok(Json(json!({
        "totals": {
            "runs": rows.iter().map(|r| r.runs).sum::<i64>(), "units_started": st, "units_scrapped": sc,
            "scrap_rate": if st > 0 { sc as f64 / st as f64 } else { 0.0 },
            "actual_cost": act_cost, "cost_at_standard_for_good_units": std_cost,
            "cost_variance": if std_cost > 0.0 { Some(act_cost / std_cost - 1.0) } else { None },
        },
        "products": items,
    })))
}
