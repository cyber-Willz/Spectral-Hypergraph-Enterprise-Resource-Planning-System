//! Quality incidents with quarantine (blocked stock), release and scrap.
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use chrono::{NaiveDate, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, Statement, TransactionTrait,
};
use serde::Deserialize;
use uuid::Uuid;

use super::{clean_lot, page_limit};
use crate::supply_chain::{
    entities::{product, purchase_order, quality_incident, stock_level, supplier},
    error::{AppError, AppResult},
    state::AppState,
    stock::record_movement,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/quality-incidents", post(create).get(list))
        .route("/quality-incidents/{id}", get(get_one))
        .route("/quality-incidents/{id}/resolve", post(resolve))
}

#[derive(Deserialize)]
pub struct NewIncident {
    product_id: Uuid,
    supplier_id: Option<Uuid>,
    purchase_order_id: Option<Uuid>,
    /// minor | major | critical
    severity: String,
    defect_type: Option<String>,
    note: Option<String>,
    occurred_on: Option<NaiveDate>,
    /// Quarantine part of the stock held in this bin/lot (moves it from available to blocked).
    location_id: Option<Uuid>,
    lot_number: Option<String>,
    quarantine_qty: Option<i64>,
}

async fn create(
    State(s): State<AppState>,
    Json(i): Json<NewIncident>,
) -> AppResult<(StatusCode, Json<quality_incident::Model>)> {
    if !matches!(i.severity.as_str(), "minor" | "major" | "critical") {
        return Err(AppError::bad("severity must be minor, major or critical"));
    }
    get_or_404!(product::Entity, &s.db, i.product_id, "product");
    let mut supplier_id = i.supplier_id;
    if let Some(sid) = supplier_id {
        get_or_404!(supplier::Entity, &s.db, sid, "supplier");
    }
    if let Some(po_id) = i.purchase_order_id {
        let po = get_or_404!(purchase_order::Entity, &s.db, po_id, "purchase order");
        supplier_id.get_or_insert(po.supplier_id);
    }
    let qty = i.quarantine_qty.unwrap_or(0);
    if qty < 0 {
        return Err(AppError::bad("quarantine_qty must not be negative"));
    }

    let txn = s.db.begin().await?;
    let mut stock_level_id = None;
    if qty > 0 {
        let loc = i
            .location_id
            .ok_or_else(|| AppError::bad("location_id is required to quarantine stock"))?;
        let lot = clean_lot(&i.lot_number);
        let row = txn
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE stock_levels SET blocked = blocked + $1, updated_at = now() \
                 WHERE product_id = $2 AND location_id = $3 AND lot_number = $4 \
                   AND quantity - reserved - blocked >= $1 RETURNING id",
                [qty.into(), i.product_id.into(), loc.into(), lot.into()],
            ))
            .await?
            .ok_or_else(|| AppError::conflict("insufficient available stock to quarantine"))?;
        stock_level_id = Some(row.try_get::<Uuid>("", "id")?);
    }
    let m = quality_incident::ActiveModel {
        id: Set(Uuid::new_v4()),
        supplier_id: Set(supplier_id),
        product_id: Set(i.product_id),
        purchase_order_id: Set(i.purchase_order_id),
        stock_level_id: Set(stock_level_id),
        severity: Set(i.severity),
        defect_type: Set(i.defect_type),
        quarantined_qty: Set(qty),
        released_qty: Set(0),
        scrapped_qty: Set(0),
        reworked_qty: Set(0),
        status: Set("open".into()),
        note: Set(i.note),
        occurred_on: Set(i.occurred_on.unwrap_or_else(|| Utc::now().date_naive())),
        created_at: Set(Utc::now()),
    }
    .insert(&txn)
    .await?;
    txn.commit().await?;
    Ok((StatusCode::CREATED, Json(m)))
}

#[derive(Deserialize)]
pub struct IncidentQuery {
    status: Option<String>,
    supplier_id: Option<Uuid>,
    product_id: Option<Uuid>,
    limit: Option<u64>,
    offset: Option<u64>,
}

async fn list(
    State(s): State<AppState>,
    Query(q): Query<IncidentQuery>,
) -> AppResult<Json<Vec<quality_incident::Model>>> {
    let mut sel = quality_incident::Entity::find();
    if let Some(v) = q.status {
        sel = sel.filter(quality_incident::Column::Status.eq(v));
    }
    if let Some(v) = q.supplier_id {
        sel = sel.filter(quality_incident::Column::SupplierId.eq(v));
    }
    if let Some(v) = q.product_id {
        sel = sel.filter(quality_incident::Column::ProductId.eq(v));
    }
    Ok(Json(
        sel.order_by_desc(quality_incident::Column::CreatedAt)
            .limit(page_limit(q.limit))
            .offset(q.offset.unwrap_or(0))
            .all(&s.db)
            .await?,
    ))
}

async fn get_one(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<quality_incident::Model>> {
    Ok(Json(get_or_404!(quality_incident::Entity, &s.db, id, "quality incident")))
}

#[derive(Deserialize)]
pub struct ResolveReq {
    /// Quarantined units found good: back to available.
    release_qty: Option<i64>,
    /// Quarantined units destroyed: removed from on-hand (ledger entry `scrap`).
    scrap_qty: Option<i64>,
    /// Quarantined units repaired and returned to available stock (tracked separately from release).
    rework_qty: Option<i64>,
}

async fn resolve(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(r): Json<ResolveReq>,
) -> AppResult<Json<quality_incident::Model>> {
    let release = r.release_qty.unwrap_or(0);
    let scrap = r.scrap_qty.unwrap_or(0);
    let rework = r.rework_qty.unwrap_or(0);
    if release < 0 || scrap < 0 || rework < 0 {
        return Err(AppError::bad("quantities must not be negative"));
    }
    let txn = s.db.begin().await?;
    let inc = quality_incident::Entity::find_by_id(id)
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::not_found("quality incident"))?;
    if inc.status != "open" {
        return Err(AppError::conflict("incident is already closed"));
    }
    let remaining = inc.quarantined_qty - inc.released_qty - inc.scrapped_qty - inc.reworked_qty;
    if release + scrap + rework > remaining {
        return Err(AppError::conflict(format!("only {remaining} quarantined units remain")));
    }
    if release + scrap + rework > 0 {
        let level = stock_level::Entity::find_by_id(
            inc.stock_level_id.ok_or_else(|| AppError::conflict("incident has no quarantined stock"))?,
        )
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::not_found("stock level"))?;
        if release + rework > 0 {
            txn.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE stock_levels SET blocked = blocked - $1, updated_at = now() WHERE id = $2",
                [(release + rework).into(), level.id.into()],
            ))
            .await?;
        }
        if scrap > 0 {
            txn.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE stock_levels SET quantity = quantity - $1, blocked = blocked - $1, \
                 updated_at = now() WHERE id = $2",
                [scrap.into(), level.id.into()],
            ))
            .await?;
            record_movement(
                &txn, level.product_id, level.location_id, &level.lot_number, "scrap", -scrap,
                Some("quality_incident"), Some(id), inc.defect_type.clone(),
            )
            .await?;
        }
    }
    let left = remaining - release - scrap - rework;
    let (rel, scr, rwk) = (inc.released_qty + release, inc.scrapped_qty + scrap, inc.reworked_qty + rework);
    let mut am: quality_incident::ActiveModel = inc.into();
    am.released_qty = Set(rel);
    am.scrapped_qty = Set(scr);
    am.reworked_qty = Set(rwk);
    if left == 0 {
        am.status = Set("closed".into());
    }
    let m = am.update(&txn).await?;
    txn.commit().await?;
    Ok(Json(m))
}
