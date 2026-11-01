//! Stock visibility, adjustments, inter-bin/inter-warehouse transfers and the movement ledger.
use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use sea_orm::{
    ColumnTrait, DbBackend, EntityTrait, FromQueryResult, QueryFilter, QueryOrder, QuerySelect,
    Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{clean_lot, page_limit, require_positive};
use crate::supply_chain::{
    entities::{location, product, stock_level, stock_movement},
    error::{AppError, AppResult},
    state::AppState,
    stock::{add_stock, default_expiry, ensure_storage_compatible, record_movement, remove_stock},
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/inventory/stock", get(list_stock))
        .route("/inventory/movements", get(list_movements))
        .route("/inventory/adjust", post(adjust))
        .route("/inventory/transfer", post(transfer))
}

#[derive(Deserialize)]
pub struct StockQuery {
    product_id: Option<Uuid>,
    warehouse_id: Option<Uuid>,
    location_id: Option<Uuid>,
    limit: Option<u64>,
    offset: Option<u64>,
}

#[derive(Debug, FromQueryResult, Serialize)]
pub struct StockRow {
    id: Uuid,
    product_id: Uuid,
    sku: String,
    product_name: String,
    location_id: Uuid,
    location_code: String,
    warehouse_id: Uuid,
    lot_number: String,
    quantity: i64,
    reserved: i64,
    blocked: i64,
    expiry_date: Option<chrono::NaiveDate>,
    available: i64,
}

async fn list_stock(
    State(s): State<AppState>,
    Query(q): Query<StockQuery>,
) -> AppResult<Json<Vec<StockRow>>> {
    let rows = StockRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT sl.id, sl.product_id, p.sku, p.name AS product_name, sl.location_id, \
                l.code AS location_code, l.warehouse_id, sl.lot_number, sl.quantity, sl.reserved, \
                sl.blocked, sl.expiry_date, (sl.quantity - sl.reserved - sl.blocked) AS available \
         FROM stock_levels sl \
         JOIN products p ON p.id = sl.product_id \
         JOIN locations l ON l.id = sl.location_id \
         WHERE ($1::uuid IS NULL OR sl.product_id = $1) \
           AND ($2::uuid IS NULL OR l.warehouse_id = $2) \
           AND ($3::uuid IS NULL OR sl.location_id = $3) \
         ORDER BY p.sku, l.code, sl.lot_number \
         LIMIT $4 OFFSET $5",
        [
            q.product_id.into(),
            q.warehouse_id.into(),
            q.location_id.into(),
            (page_limit(q.limit) as i64).into(),
            (q.offset.unwrap_or(0) as i64).into(),
        ],
    ))
    .all(&s.db)
    .await?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct MovementQuery {
    product_id: Option<Uuid>,
    location_id: Option<Uuid>,
    movement_type: Option<String>,
    reference_id: Option<Uuid>,
    limit: Option<u64>,
    offset: Option<u64>,
}

async fn list_movements(
    State(s): State<AppState>,
    Query(q): Query<MovementQuery>,
) -> AppResult<Json<Vec<stock_movement::Model>>> {
    let mut sel = stock_movement::Entity::find();
    if let Some(v) = q.product_id {
        sel = sel.filter(stock_movement::Column::ProductId.eq(v));
    }
    if let Some(v) = q.location_id {
        sel = sel.filter(stock_movement::Column::LocationId.eq(v));
    }
    if let Some(v) = q.movement_type {
        sel = sel.filter(stock_movement::Column::MovementType.eq(v));
    }
    if let Some(v) = q.reference_id {
        sel = sel.filter(stock_movement::Column::ReferenceId.eq(v));
    }
    let rows = sel
        .order_by_desc(stock_movement::Column::CreatedAt)
        .limit(page_limit(q.limit))
        .offset(q.offset.unwrap_or(0))
        .all(&s.db)
        .await?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct AdjustReq {
    product_id: Uuid,
    location_id: Uuid,
    lot_number: Option<String>,
    /// Positive to add stock (found/cycle-count gain), negative to remove (damage, shrinkage).
    delta: i64,
    reason: Option<String>,
    /// Lot expiry for stock being added (defaults from the product's shelf life).
    expiry_date: Option<chrono::NaiveDate>,
}

async fn adjust(
    State(s): State<AppState>,
    Json(r): Json<AdjustReq>,
) -> AppResult<Json<stock_level::Model>> {
    if r.delta == 0 {
        return Err(AppError::bad("delta must not be zero"));
    }
    let lot = clean_lot(&r.lot_number);
    let prod = get_or_404!(product::Entity, &s.db, r.product_id, "product");
    let loc = get_or_404!(location::Entity, &s.db, r.location_id, "location");
    if !loc.active {
        return Err(AppError::conflict("location is inactive"));
    }
    if r.delta > 0 {
        ensure_storage_compatible(&prod, &loc)?;
    }
    let expiry = default_expiry(&prod, chrono::Utc::now().date_naive(), r.expiry_date);

    let txn = s.db.begin().await?;
    if r.delta > 0 {
        add_stock(&txn, r.product_id, r.location_id, &lot, r.delta, expiry).await?;
    } else if !remove_stock(&txn, r.product_id, r.location_id, &lot, -r.delta).await? {
        return Err(AppError::conflict(
            "insufficient available (unreserved) stock for this adjustment",
        ));
    }
    record_movement(
        &txn, r.product_id, r.location_id, &lot, "adjustment", r.delta, None, None, r.reason,
    )
    .await?;
    txn.commit().await?;

    let level = stock_level::Entity::find()
        .filter(stock_level::Column::ProductId.eq(r.product_id))
        .filter(stock_level::Column::LocationId.eq(r.location_id))
        .filter(stock_level::Column::LotNumber.eq(lot))
        .one(&s.db)
        .await?
        .ok_or_else(|| AppError::not_found("stock level"))?;
    Ok(Json(level))
}

#[derive(Deserialize)]
pub struct TransferReq {
    product_id: Uuid,
    from_location_id: Uuid,
    to_location_id: Uuid,
    lot_number: Option<String>,
    quantity: i64,
    note: Option<String>,
}

async fn transfer(
    State(s): State<AppState>,
    Json(r): Json<TransferReq>,
) -> AppResult<Json<Value>> {
    require_positive("quantity", r.quantity)?;
    if r.from_location_id == r.to_location_id {
        return Err(AppError::bad("source and destination locations must differ"));
    }
    let lot = clean_lot(&r.lot_number);
    let prod = get_or_404!(product::Entity, &s.db, r.product_id, "product");
    get_or_404!(location::Entity, &s.db, r.from_location_id, "source location");
    let dest = get_or_404!(location::Entity, &s.db, r.to_location_id, "destination location");
    if !dest.active {
        return Err(AppError::conflict("destination location is inactive"));
    }
    ensure_storage_compatible(&prod, &dest)?;

    let transfer_id = Uuid::new_v4();
    let txn = s.db.begin().await?;
    if !remove_stock(&txn, r.product_id, r.from_location_id, &lot, r.quantity).await? {
        return Err(AppError::conflict("insufficient available stock at source location"));
    }
    // the lot keeps its expiry date when it moves
    let src_expiry = stock_level::Entity::find()
        .filter(stock_level::Column::ProductId.eq(r.product_id))
        .filter(stock_level::Column::LocationId.eq(r.from_location_id))
        .filter(stock_level::Column::LotNumber.eq(lot.clone()))
        .one(&txn)
        .await?
        .and_then(|l| l.expiry_date);
    add_stock(&txn, r.product_id, r.to_location_id, &lot, r.quantity, src_expiry).await?;
    record_movement(
        &txn, r.product_id, r.from_location_id, &lot, "transfer_out", -r.quantity,
        Some("transfer"), Some(transfer_id), r.note.clone(),
    )
    .await?;
    record_movement(
        &txn, r.product_id, r.to_location_id, &lot, "transfer_in", r.quantity,
        Some("transfer"), Some(transfer_id), r.note,
    )
    .await?;
    txn.commit().await?;
    Ok(Json(json!({ "transfer_id": transfer_id, "quantity": r.quantity })))
}
