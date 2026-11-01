//! Outbound flow: sales orders, stock allocation, shipping and shipment tracking.
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use chrono::{NaiveDate, Utc};
use sea_orm::{
    prelude::Decimal, ActiveModelTrait, ColumnTrait, ConnectionTrait, DbBackend, EntityTrait,
    FromQueryResult, QueryFilter, QueryOrder, QuerySelect, Set, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    gen_number,
    ops_reports::{assess, RiskIn},
    page_limit, require_positive,
};
use crate::supply_chain::{
    entities::{
        allocation, carrier, customer, product, sales_order, sales_order_line, shipment,
        shipment_event, warehouse,
    },
    error::{AppError, AppResult},
    state::AppState,
    stock::record_movement,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/sales-orders", post(create_so).get(list_sos))
        .route("/sales-orders/{id}", get(get_so))
        .route("/sales-orders/{id}/confirm", post(confirm_so))
        .route("/sales-orders/{id}/allocate", post(allocate_so))
        .route("/sales-orders/{id}/fulfil", post(fulfil_so))
        .route("/sales-orders/{id}/ship", post(ship_so))
        .route("/sales-orders/{id}/cancel", post(cancel_so))
        .route("/shipments", get(list_shipments))
        .route("/shipments/{id}", get(get_shipment))
        .route("/shipments/{id}/events", post(add_event))
        .route("/tracking/{tracking_number}", get(track))
}

// ---- status logic ------------------------------------------------------------------------------

/// Derive the order status from line totals. Pure so it can be unit-tested.
pub fn compute_so_status(total: i64, allocated: i64, shipped: i64) -> &'static str {
    if total > 0 && shipped >= total {
        "shipped"
    } else if shipped > 0 {
        "partially_shipped"
    } else if allocated >= total {
        "confirmed"
    } else if allocated > 0 {
        "partially_allocated"
    } else {
        "backordered"
    }
}

#[derive(Debug, FromQueryResult)]
struct Totals {
    total: i64,
    allocated: i64,
    shipped: i64,
}

async fn refresh_so_status<C: ConnectionTrait>(db: &C, order_id: Uuid) -> Result<String, sea_orm::DbErr> {
    let t = Totals::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT COALESCE(SUM(quantity),0)::BIGINT AS total, \
                COALESCE(SUM(quantity_allocated),0)::BIGINT AS allocated, \
                COALESCE(SUM(quantity_shipped),0)::BIGINT AS shipped \
         FROM sales_order_lines WHERE sales_order_id = $1",
        [order_id.into()],
    ))
    .one(db)
    .await?
    .unwrap_or(Totals { total: 0, allocated: 0, shipped: 0 });
    let status = compute_so_status(t.total, t.allocated, t.shipped);
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE sales_orders SET status = $1, updated_at = now() WHERE id = $2",
        [status.into(), order_id.into()],
    ))
    .await?;
    Ok(status.to_string())
}

// ---- allocation --------------------------------------------------------------------------------

#[derive(Debug, FromQueryResult)]
struct Avail {
    id: Uuid,
    available: i64,
}

/// Reserve stock for every unfilled line from the order's warehouse (first-expiry-first-out, then oldest; expired and quarantined stock is skipped).
/// Stock-level rows are locked with FOR UPDATE so concurrent orders cannot double-reserve.
async fn allocate_lines<C: ConnectionTrait>(
    db: &C,
    order: &sales_order::Model,
) -> Result<(), sea_orm::DbErr> {
    let lines = sales_order_line::Entity::find()
        .filter(sales_order_line::Column::SalesOrderId.eq(order.id))
        .order_by_asc(sales_order_line::Column::Id)
        .all(db)
        .await?;
    for line in lines {
        let mut need = line.quantity - line.quantity_allocated - line.quantity_shipped;
        if need <= 0 {
            continue;
        }
        let candidates = Avail::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT sl.id, (sl.quantity - sl.reserved - sl.blocked) AS available \
             FROM stock_levels sl JOIN locations l ON l.id = sl.location_id \
             WHERE sl.product_id = $1 AND l.warehouse_id = $2 AND l.active \
               AND sl.quantity - sl.reserved - sl.blocked > 0 \
               AND (sl.expiry_date IS NULL OR sl.expiry_date >= CURRENT_DATE) \
             ORDER BY sl.expiry_date ASC NULLS LAST, sl.updated_at ASC, sl.id \
             FOR UPDATE OF sl",
            [line.product_id.into(), order.warehouse_id.into()],
        ))
        .all(db)
        .await?;

        let mut got = 0i64;
        for c in candidates {
            if need == 0 {
                break;
            }
            let take = need.min(c.available);
            db.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE stock_levels SET reserved = reserved + $1, updated_at = now() WHERE id = $2",
                [take.into(), c.id.into()],
            ))
            .await?;
            allocation::ActiveModel {
                id: Set(Uuid::new_v4()),
                sales_order_line_id: Set(line.id),
                stock_level_id: Set(c.id),
                quantity: Set(take),
                status: Set("reserved".into()),
                shipment_id: Set(None),
                created_at: Set(Utc::now()),
            }
            .insert(db)
            .await?;
            need -= take;
            got += take;
        }
        if got > 0 {
            db.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE sales_order_lines SET quantity_allocated = quantity_allocated + $1 WHERE id = $2",
                [got.into(), line.id.into()],
            ))
            .await?;
        }
    }
    Ok(())
}

// ---- sales orders ------------------------------------------------------------------------------

#[derive(Serialize)]
pub struct SoDetail {
    order: sales_order::Model,
    lines: Vec<sales_order_line::Model>,
}

async fn load_so<C: ConnectionTrait>(db: &C, id: Uuid) -> AppResult<SoDetail> {
    let order = get_or_404!(sales_order::Entity, db, id, "sales order");
    let lines = sales_order_line::Entity::find()
        .filter(sales_order_line::Column::SalesOrderId.eq(id))
        .order_by_asc(sales_order_line::Column::Id)
        .all(db)
        .await?;
    Ok(SoDetail { order, lines })
}

async fn lock_so<C: ConnectionTrait>(db: &C, id: Uuid) -> AppResult<sales_order::Model> {
    sales_order::Entity::find_by_id(id)
        .lock_exclusive()
        .one(db)
        .await?
        .ok_or_else(|| AppError::not_found("sales order"))
}

#[derive(Deserialize)]
pub struct NewSoLine {
    product_id: Uuid,
    quantity: i64,
    /// Defaults to the product's list price.
    unit_price: Option<Decimal>,
}

#[derive(Deserialize)]
pub struct NewSo {
    customer_id: Uuid,
    warehouse_id: Uuid,
    ship_to: Option<String>,
    lines: Vec<NewSoLine>,
}

async fn create_so(
    State(s): State<AppState>,
    Json(i): Json<NewSo>,
) -> AppResult<(StatusCode, Json<SoDetail>)> {
    if i.lines.is_empty() {
        return Err(AppError::bad("a sales order needs at least one line"));
    }
    get_or_404!(customer::Entity, &s.db, i.customer_id, "customer");
    let wh = get_or_404!(warehouse::Entity, &s.db, i.warehouse_id, "warehouse");
    if !wh.active {
        return Err(AppError::conflict("warehouse is inactive"));
    }

    let txn = s.db.begin().await?;
    let now = Utc::now();
    let order_id = Uuid::new_v4();
    sales_order::ActiveModel {
        id: Set(order_id),
        so_number: Set(gen_number("SO")),
        customer_id: Set(i.customer_id),
        warehouse_id: Set(i.warehouse_id),
        status: Set("draft".into()),
        ship_to: Set(i.ship_to),
        fulfillment_stage: Set("none".into()),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&txn)
    .await?;
    for l in &i.lines {
        require_positive("line quantity", l.quantity)?;
        let p = get_or_404!(product::Entity, &txn, l.product_id, "product");
        if !p.active {
            return Err(AppError::conflict(format!("product {} is inactive", p.sku)));
        }
        let price = l.unit_price.unwrap_or(p.list_price);
        if price.is_sign_negative() {
            return Err(AppError::bad("unit_price must not be negative"));
        }
        sales_order_line::ActiveModel {
            id: Set(Uuid::new_v4()),
            sales_order_id: Set(order_id),
            product_id: Set(l.product_id),
            quantity: Set(l.quantity),
            quantity_allocated: Set(0),
            quantity_shipped: Set(0),
            unit_price: Set(price),
        }
        .insert(&txn)
        .await?;
    }
    let detail = load_so(&txn, order_id).await?;
    txn.commit().await?;
    Ok((StatusCode::CREATED, Json(detail)))
}

#[derive(Deserialize)]
pub struct SoQuery {
    status: Option<String>,
    customer_id: Option<Uuid>,
    warehouse_id: Option<Uuid>,
    limit: Option<u64>,
    offset: Option<u64>,
}

async fn list_sos(
    State(s): State<AppState>,
    Query(q): Query<SoQuery>,
) -> AppResult<Json<Vec<sales_order::Model>>> {
    let mut sel = sales_order::Entity::find();
    if let Some(v) = q.status {
        sel = sel.filter(sales_order::Column::Status.eq(v));
    }
    if let Some(v) = q.customer_id {
        sel = sel.filter(sales_order::Column::CustomerId.eq(v));
    }
    if let Some(v) = q.warehouse_id {
        sel = sel.filter(sales_order::Column::WarehouseId.eq(v));
    }
    let rows = sel
        .order_by_desc(sales_order::Column::CreatedAt)
        .limit(page_limit(q.limit))
        .offset(q.offset.unwrap_or(0))
        .all(&s.db)
        .await?;
    Ok(Json(rows))
}

async fn get_so(State(s): State<AppState>, Path(id): Path<Uuid>) -> AppResult<Json<SoDetail>> {
    Ok(Json(load_so(&s.db, id).await?))
}

/// draft -> confirmed | partially_allocated | backordered (reserves what stock exists).
async fn confirm_so(State(s): State<AppState>, Path(id): Path<Uuid>) -> AppResult<Json<SoDetail>> {
    let txn = s.db.begin().await?;
    let order = lock_so(&txn, id).await?;
    if order.status != "draft" {
        return Err(AppError::conflict(format!(
            "only draft orders can be confirmed (status is '{}')",
            order.status
        )));
    }
    allocate_lines(&txn, &order).await?;
    refresh_so_status(&txn, id).await?;
    let detail = load_so(&txn, id).await?;
    txn.commit().await?;
    Ok(Json(detail))
}

/// Retry allocation for backordered / partially allocated orders after new stock arrives.
async fn allocate_so(State(s): State<AppState>, Path(id): Path<Uuid>) -> AppResult<Json<SoDetail>> {
    let txn = s.db.begin().await?;
    let order = lock_so(&txn, id).await?;
    if matches!(order.status.as_str(), "draft" | "shipped" | "cancelled") {
        return Err(AppError::conflict(format!(
            "cannot allocate an order in '{}' status",
            order.status
        )));
    }
    allocate_lines(&txn, &order).await?;
    refresh_so_status(&txn, id).await?;
    let detail = load_so(&txn, id).await?;
    txn.commit().await?;
    Ok(Json(detail))
}

/// Release reservations and cancel. Not allowed once anything has shipped.
async fn cancel_so(State(s): State<AppState>, Path(id): Path<Uuid>) -> AppResult<Json<SoDetail>> {
    let txn = s.db.begin().await?;
    let order = lock_so(&txn, id).await?;
    if matches!(order.status.as_str(), "shipped" | "partially_shipped" | "cancelled") {
        return Err(AppError::conflict(format!(
            "cannot cancel an order in '{}' status",
            order.status
        )));
    }
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE stock_levels sl SET reserved = sl.reserved - r.q::BIGINT, updated_at = now() \
         FROM (SELECT a.stock_level_id, SUM(a.quantity) AS q \
               FROM allocations a JOIN sales_order_lines l ON l.id = a.sales_order_line_id \
               WHERE l.sales_order_id = $1 AND a.status = 'reserved' \
               GROUP BY a.stock_level_id) r \
         WHERE sl.id = r.stock_level_id",
        [id.into()],
    ))
    .await?;
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE allocations a SET status = 'released' FROM sales_order_lines l \
         WHERE l.id = a.sales_order_line_id AND l.sales_order_id = $1 AND a.status = 'reserved'",
        [id.into()],
    ))
    .await?;
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE sales_order_lines SET quantity_allocated = 0 WHERE sales_order_id = $1",
        [id.into()],
    ))
    .await?;
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE sales_orders SET status = 'cancelled', updated_at = now() WHERE id = $1",
        [id.into()],
    ))
    .await?;
    let detail = load_so(&txn, id).await?;
    txn.commit().await?;
    Ok(Json(detail))
}

// ---- shipping ----------------------------------------------------------------------------------

#[derive(Debug, FromQueryResult)]
struct AllocRow {
    allocation_id: Uuid,
    quantity: i64,
    line_id: Uuid,
    stock_level_id: Uuid,
    product_id: Uuid,
    location_id: Uuid,
    lot_number: String,
}

#[derive(Deserialize)]
pub struct ShipReq {
    carrier_id: Option<Uuid>,
    tracking_number: Option<String>,
    /// When true the order must have been picked, packed and staged first (default false)
    require_staging: Option<bool>,
    /// Promised delivery date; enables on-time / early / late carrier scorecards
    expected_delivery_date: Option<NaiveDate>,
    freight_cost: Option<Decimal>,
    /// Context for the delay / disruption models (weather, traffic, reliabilities, ...). Carrier and
    /// warehouse utilisation are filled in from our own data when omitted.
    risk: Option<RiskIn>,
}

#[derive(Deserialize)]
pub struct FulfilReq {
    /// picked -> packed -> staged, in that order
    stage: String,
}

/// Warehouse fulfilment steps between allocation and shipping.
async fn fulfil_so(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(r): Json<FulfilReq>,
) -> AppResult<Json<SoDetail>> {
    let txn = s.db.begin().await?;
    let order = lock_so(&txn, id).await?;
    if !matches!(order.status.as_str(), "confirmed" | "partially_allocated" | "partially_shipped") {
        return Err(AppError::conflict(format!("cannot fulfil an order in '{}' status", order.status)));
    }
    let expected = match order.fulfillment_stage.as_str() {
        "none" => "picked",
        "picked" => "packed",
        "packed" => "staged",
        _ => return Err(AppError::conflict("order is already staged; ship it")),
    };
    if r.stage != expected {
        return Err(AppError::conflict(format!("next stage must be '{expected}', got '{}'", r.stage)));
    }
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE sales_orders SET fulfillment_stage = $1, updated_at = now() WHERE id = $2",
        [expected.into(), id.into()],
    ))
    .await?;
    let detail = load_so(&txn, id).await?;
    txn.commit().await?;
    Ok(Json(detail))
}

/// Ships everything currently reserved for the order: consumes the reserved stock, writes ledger
/// entries and creates a shipment in `in_transit` status.
async fn ship_so(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<ShipReq>,
) -> AppResult<(StatusCode, Json<ShipmentDetail>)> {
    if let Some(cid) = req.carrier_id {
        let c = get_or_404!(carrier::Entity, &s.db, cid, "carrier");
        if !c.active {
            return Err(AppError::conflict("carrier is inactive"));
        }
    }
    let txn = s.db.begin().await?;
    let order = lock_so(&txn, id).await?;
    if !matches!(
        order.status.as_str(),
        "confirmed" | "partially_allocated" | "partially_shipped" | "backordered"
    ) {
        return Err(AppError::conflict(format!(
            "cannot ship an order in '{}' status",
            order.status
        )));
    }

    if req.require_staging.unwrap_or(false) && order.fulfillment_stage != "staged" {
        return Err(AppError::conflict("order must be picked, packed and staged before shipping"));
    }
    let rows = AllocRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT a.id AS allocation_id, a.quantity, l.id AS line_id, sl.id AS stock_level_id, \
                sl.product_id, sl.location_id, sl.lot_number \
         FROM allocations a \
         JOIN sales_order_lines l ON l.id = a.sales_order_line_id \
         JOIN stock_levels sl ON sl.id = a.stock_level_id \
         WHERE l.sales_order_id = $1 AND a.status = 'reserved' \
         ORDER BY a.created_at, a.id \
         FOR UPDATE OF a, sl",
        [id.into()],
    ))
    .all(&txn)
    .await?;
    if rows.is_empty() {
        return Err(AppError::conflict("no allocated stock to ship; allocate first"));
    }

    if matches!(req.freight_cost, Some(f) if f.is_sign_negative()) {
        return Err(AppError::bad("freight_cost must not be negative"));
    }
    let assessment = match &req.risk {
        Some(r) => {
            let mut ri = r.clone();
            ri.carrier_id = ri.carrier_id.or(req.carrier_id);
            ri.warehouse_id = ri.warehouse_id.or(Some(order.warehouse_id));
            Some(assess(&s.db, &ri).await?)
        }
        None => None,
    };
    let now = Utc::now();
    let shipment_id = Uuid::new_v4();
    shipment::ActiveModel {
        id: Set(shipment_id),
        shipment_number: Set(gen_number("SHP")),
        sales_order_id: Set(id),
        carrier_id: Set(req.carrier_id),
        tracking_number: Set(req.tracking_number.filter(|t| !t.trim().is_empty()).map(|t| t.trim().to_string())),
        status: Set("in_transit".into()),
        shipped_at: Set(Some(now)),
        delivered_at: Set(None),
        created_at: Set(now),
        expected_delivery_date: Set(req.expected_delivery_date),
        freight_cost: Set(req.freight_cost.unwrap_or_default()),
        delay_risk: Set(assessment.as_ref().and_then(|a| a.delay.map(|d| d.0))),
        disruption_risk: Set(assessment.as_ref().and_then(|a| a.disruption.map(|d| d.0))),
        risk_context: Set(assessment.map(|a| a.context)),
    }
    .insert(&txn)
    .await?;
    shipment_event::ActiveModel {
        id: Set(Uuid::new_v4()),
        shipment_id: Set(shipment_id),
        status: Set("in_transit".into()),
        location: Set(None),
        description: Set(Some("Shipment dispatched from warehouse".into())),
        occurred_at: Set(now),
    }
    .insert(&txn)
    .await?;

    for r in &rows {
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE stock_levels SET quantity = quantity - $1, reserved = reserved - $1, \
             updated_at = now() WHERE id = $2",
            [r.quantity.into(), r.stock_level_id.into()],
        ))
        .await?;
        record_movement(
            &txn, r.product_id, r.location_id, &r.lot_number, "issue", -r.quantity,
            Some("sales_order"), Some(id), Some(format!("shipment {shipment_id}")),
        )
        .await?;
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE allocations SET status = 'shipped', shipment_id = $1 WHERE id = $2",
            [shipment_id.into(), r.allocation_id.into()],
        ))
        .await?;
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE sales_order_lines SET quantity_allocated = quantity_allocated - $1, \
             quantity_shipped = quantity_shipped + $1 WHERE id = $2",
            [r.quantity.into(), r.line_id.into()],
        ))
        .await?;
    }
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE sales_orders SET fulfillment_stage = 'none' WHERE id = $1",
        [id.into()],
    ))
    .await?;
    refresh_so_status(&txn, id).await?;
    let detail = load_shipment(&txn, shipment_id).await?;
    txn.commit().await?;
    Ok((StatusCode::CREATED, Json(detail)))
}

// ---- shipments & tracking ----------------------------------------------------------------------

#[derive(Debug, FromQueryResult, Serialize)]
pub struct ShipItem {
    product_id: Uuid,
    sku: String,
    quantity: i64,
}

#[derive(Serialize)]
pub struct ShipmentDetail {
    shipment: shipment::Model,
    items: Vec<ShipItem>,
    events: Vec<shipment_event::Model>,
}

async fn load_shipment<C: ConnectionTrait>(db: &C, id: Uuid) -> AppResult<ShipmentDetail> {
    let shipment = get_or_404!(shipment::Entity, db, id, "shipment");
    detail_for(db, shipment).await
}

async fn detail_for<C: ConnectionTrait>(db: &C, shipment: shipment::Model) -> AppResult<ShipmentDetail> {
    let items = ShipItem::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT sol.product_id, p.sku, SUM(a.quantity)::BIGINT AS quantity \
         FROM allocations a \
         JOIN sales_order_lines sol ON sol.id = a.sales_order_line_id \
         JOIN products p ON p.id = sol.product_id \
         WHERE a.shipment_id = $1 GROUP BY sol.product_id, p.sku ORDER BY p.sku",
        [shipment.id.into()],
    ))
    .all(db)
    .await?;
    let events = shipment_event::Entity::find()
        .filter(shipment_event::Column::ShipmentId.eq(shipment.id))
        .order_by_asc(shipment_event::Column::OccurredAt)
        .all(db)
        .await?;
    Ok(ShipmentDetail { shipment, items, events })
}

#[derive(Deserialize)]
pub struct ShipmentQuery {
    status: Option<String>,
    sales_order_id: Option<Uuid>,
    limit: Option<u64>,
    offset: Option<u64>,
}

async fn list_shipments(
    State(s): State<AppState>,
    Query(q): Query<ShipmentQuery>,
) -> AppResult<Json<Vec<shipment::Model>>> {
    let mut sel = shipment::Entity::find();
    if let Some(v) = q.status {
        sel = sel.filter(shipment::Column::Status.eq(v));
    }
    if let Some(v) = q.sales_order_id {
        sel = sel.filter(shipment::Column::SalesOrderId.eq(v));
    }
    let rows = sel
        .order_by_desc(shipment::Column::CreatedAt)
        .limit(page_limit(q.limit))
        .offset(q.offset.unwrap_or(0))
        .all(&s.db)
        .await?;
    Ok(Json(rows))
}

async fn get_shipment(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<ShipmentDetail>> {
    Ok(Json(load_shipment(&s.db, id).await?))
}

async fn track(
    State(s): State<AppState>,
    Path(tracking_number): Path<String>,
) -> AppResult<Json<ShipmentDetail>> {
    let sh = shipment::Entity::find()
        .filter(shipment::Column::TrackingNumber.eq(tracking_number))
        .one(&s.db)
        .await?
        .ok_or_else(|| AppError::not_found("shipment"))?;
    Ok(Json(detail_for(&s.db, sh).await?))
}

#[derive(Deserialize)]
pub struct NewEvent {
    /// in_transit | out_for_delivery | delivered | exception
    status: String,
    location: Option<String>,
    description: Option<String>,
}

async fn add_event(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(e): Json<NewEvent>,
) -> AppResult<Json<ShipmentDetail>> {
    if !matches!(
        e.status.as_str(),
        "in_transit" | "out_for_delivery" | "delivered" | "exception"
    ) {
        return Err(AppError::bad(
            "status must be one of: in_transit, out_for_delivery, delivered, exception",
        ));
    }
    let txn = s.db.begin().await?;
    let sh = shipment::Entity::find_by_id(id)
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::not_found("shipment"))?;
    if sh.status == "delivered" {
        return Err(AppError::conflict("shipment is already delivered"));
    }
    let now = Utc::now();
    shipment_event::ActiveModel {
        id: Set(Uuid::new_v4()),
        shipment_id: Set(id),
        status: Set(e.status.clone()),
        location: Set(e.location),
        description: Set(e.description),
        occurred_at: Set(now),
    }
    .insert(&txn)
    .await?;
    let mut am: shipment::ActiveModel = sh.into();
    am.status = Set(e.status.clone());
    if e.status == "delivered" {
        am.delivered_at = Set(Some(now));
    }
    am.update(&txn).await?;
    let detail = load_shipment(&txn, id).await?;
    txn.commit().await?;
    Ok(Json(detail))
}

#[cfg(test)]
mod tests {
    use super::compute_so_status;

    #[test]
    fn status_transitions() {
        assert_eq!(compute_so_status(10, 0, 0), "backordered");
        assert_eq!(compute_so_status(10, 4, 0), "partially_allocated");
        assert_eq!(compute_so_status(10, 10, 0), "confirmed");
        assert_eq!(compute_so_status(10, 6, 4), "partially_shipped");
        assert_eq!(compute_so_status(10, 0, 4), "partially_shipped");
        assert_eq!(compute_so_status(10, 0, 10), "shipped");
    }
}
