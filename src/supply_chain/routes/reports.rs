//! Replenishment and valuation reports.
use axum::{extract::State, routing::get, Json, Router};
use sea_orm::{prelude::Decimal, DbBackend, FromQueryResult, Statement};
use serde::Serialize;
use uuid::Uuid;

use crate::supply_chain::{error::AppResult, state::AppState};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/reports/low-stock", get(low_stock))
        .route("/reports/valuation", get(valuation))
}

#[derive(Debug, FromQueryResult)]
struct LowRow {
    product_id: Uuid,
    sku: String,
    name: String,
    reorder_point: i64,
    reorder_qty: i64,
    on_hand: i64,
    reserved: i64,
    on_order: i64,
}

#[derive(Serialize)]
pub struct LowStockItem {
    product_id: Uuid,
    sku: String,
    name: String,
    on_hand: i64,
    reserved: i64,
    available: i64,
    on_order: i64,
    reorder_point: i64,
    /// available + on_order
    inventory_position: i64,
    suggested_order_qty: i64,
}

/// Products whose (available + open PO quantity) has fallen to or below the reorder point.
async fn low_stock(State(s): State<AppState>) -> AppResult<Json<Vec<LowStockItem>>> {
    let rows = LowRow::find_by_statement(Statement::from_string(
        DbBackend::Postgres,
        "SELECT p.id AS product_id, p.sku, p.name, p.reorder_point, p.reorder_qty, \
                COALESCE(s.on_hand, 0)::BIGINT AS on_hand, \
                COALESCE(s.reserved, 0)::BIGINT AS reserved, \
                COALESCE(o.on_order, 0)::BIGINT AS on_order \
         FROM products p \
         LEFT JOIN (SELECT product_id, SUM(quantity - blocked) AS on_hand, SUM(reserved) AS reserved \
                    FROM stock_levels GROUP BY product_id) s ON s.product_id = p.id \
         LEFT JOIN (SELECT pol.product_id, SUM(pol.quantity_ordered - pol.quantity_received) AS on_order \
                    FROM purchase_order_lines pol \
                    JOIN purchase_orders po ON po.id = pol.purchase_order_id \
                    WHERE po.status IN ('submitted', 'partially_received') \
                    GROUP BY pol.product_id) o ON o.product_id = p.id \
         WHERE p.active AND p.reorder_point > 0 \
           AND COALESCE(s.on_hand, 0) - COALESCE(s.reserved, 0) + COALESCE(o.on_order, 0) <= p.reorder_point \
         ORDER BY p.sku"
            .to_string(),
    ))
    .all(&s.db)
    .await?;

    let items = rows
        .into_iter()
        .map(|r| {
            let available = r.on_hand - r.reserved;
            let position = available + r.on_order;
            let deficit = (r.reorder_point - position).max(0);
            LowStockItem {
                product_id: r.product_id,
                sku: r.sku,
                name: r.name,
                on_hand: r.on_hand,
                reserved: r.reserved,
                available,
                on_order: r.on_order,
                reorder_point: r.reorder_point,
                inventory_position: position,
                suggested_order_qty: deficit.max(r.reorder_qty),
            }
        })
        .collect();
    Ok(Json(items))
}

#[derive(Debug, FromQueryResult, Serialize)]
pub struct ValuationRow {
    warehouse_id: Uuid,
    warehouse_code: String,
    units: i64,
    value: Decimal,
}

/// On-hand units and value (at current unit cost) per warehouse.
async fn valuation(State(s): State<AppState>) -> AppResult<Json<Vec<ValuationRow>>> {
    let rows = ValuationRow::find_by_statement(Statement::from_string(
        DbBackend::Postgres,
        "SELECT w.id AS warehouse_id, w.code AS warehouse_code, \
                COALESCE(SUM(sl.quantity), 0)::BIGINT AS units, \
                COALESCE(SUM(sl.quantity * p.unit_cost), 0) AS value \
         FROM warehouses w \
         LEFT JOIN locations l ON l.warehouse_id = w.id \
         LEFT JOIN stock_levels sl ON sl.location_id = l.id \
         LEFT JOIN products p ON p.id = sl.product_id \
         GROUP BY w.id, w.code ORDER BY w.code"
            .to_string(),
    ))
    .all(&s.db)
    .await?;
    Ok(Json(rows))
}
