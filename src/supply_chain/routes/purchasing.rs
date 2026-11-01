//! Procurement: purchase orders and goods receipt into bin locations.
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use chrono::{NaiveDate, Utc};
use sea_orm::{
    prelude::Decimal, ActiveModelTrait, ColumnTrait, ConnectionTrait, DbBackend, EntityTrait,
    QueryFilter, QueryOrder, QuerySelect, Set, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{clean_lot, gen_number, page_limit, require_positive};
use crate::supply_chain::{
    entities::{
        contract, goods_receipt, location, product, purchase_order, purchase_order_line,
        quality_incident, supplier, warehouse,
    },
    error::{AppError, AppResult},
    state::AppState,
    stock::{add_stock, default_expiry, ensure_storage_compatible, record_movement},
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/purchase-orders", post(create_po).get(list_pos))
        .route("/purchase-orders/{id}", get(get_po))
        .route("/purchase-orders/{id}/submit", post(submit_po))
        .route("/purchase-orders/{id}/cancel", post(cancel_po))
        .route("/purchase-orders/{id}/close", post(close_po))
        .route("/purchase-orders/{id}/freight", post(set_freight))
        .route("/purchase-orders/{id}/landed-cost", get(landed_cost))
        .route("/purchase-orders/{id}/receive", post(receive_po))
}

#[derive(Serialize)]
pub struct PoDetail {
    order: purchase_order::Model,
    lines: Vec<purchase_order_line::Model>,
}

async fn load_detail<C: ConnectionTrait>(db: &C, id: Uuid) -> AppResult<PoDetail> {
    let order = get_or_404!(purchase_order::Entity, db, id, "purchase order");
    let lines = purchase_order_line::Entity::find()
        .filter(purchase_order_line::Column::PurchaseOrderId.eq(id))
        .order_by_asc(purchase_order_line::Column::Id)
        .all(db)
        .await?;
    Ok(PoDetail { order, lines })
}

#[derive(Deserialize)]
pub struct NewPoLine {
    product_id: Uuid,
    quantity: i64,
    /// Defaults to the product's current unit cost.
    unit_cost: Option<Decimal>,
    /// Defaults to the order's expected_date.
    promised_date: Option<NaiveDate>,
    /// Budgeted price, used for price-variance reporting.
    budget_unit_cost: Option<Decimal>,
}

#[derive(Deserialize)]
pub struct NewPo {
    supplier_id: Uuid,
    warehouse_id: Uuid,
    expected_date: Option<NaiveDate>,
    notes: Option<String>,
    /// Defaults to today; set it when loading historical orders.
    order_date: Option<NaiveDate>,
    /// standard | blanket | contract | emergency
    po_type: Option<String>,
    currency: Option<String>,
    payment_terms_days: Option<i32>,
    /// When given, the order date must fall inside the contract window and the supplier must match.
    contract_id: Option<Uuid>,
    /// EXW, FCA, FOB, CIF, DAP, DDP ...
    incoterm: Option<String>,
    /// air | air_charter | sea | rail | road | courier | multimodal
    transport_mode: Option<String>,
    freight_cost: Option<Decimal>,
    insurance_cost: Option<Decimal>,
    /// freight already included in the unit price (no landed-cost allocation needed)
    freight_included: Option<bool>,
    lines: Vec<NewPoLine>,
}

fn nonneg_money(field: &str, v: Option<Decimal>) -> AppResult<Decimal> {
    let v = v.unwrap_or_default();
    if v.is_sign_negative() {
        return Err(AppError::bad(format!("{field} must not be negative")));
    }
    Ok(v)
}

async fn create_po(
    State(s): State<AppState>,
    Json(i): Json<NewPo>,
) -> AppResult<(StatusCode, Json<PoDetail>)> {
    if i.lines.is_empty() {
        return Err(AppError::bad("a purchase order needs at least one line"));
    }
    let sup = get_or_404!(supplier::Entity, &s.db, i.supplier_id, "supplier");
    let wh = get_or_404!(warehouse::Entity, &s.db, i.warehouse_id, "warehouse");
    if !sup.active || !wh.active {
        return Err(AppError::conflict("supplier and warehouse must be active"));
    }

    let order_date = i.order_date.unwrap_or_else(|| Utc::now().date_naive());
    let po_type = i.po_type.clone().unwrap_or_else(|| "standard".into());
    if !matches!(po_type.as_str(), "standard" | "blanket" | "contract" | "emergency") {
        return Err(AppError::bad("po_type must be standard, blanket, contract or emergency"));
    }
    let terms = i.payment_terms_days.unwrap_or(sup.payment_terms_days);
    if terms < 0 {
        return Err(AppError::bad("payment_terms_days must not be negative"));
    }
    if let Some(cid) = i.contract_id {
        let c = get_or_404!(contract::Entity, &s.db, cid, "contract");
        if c.supplier_id != i.supplier_id {
            return Err(AppError::conflict("contract belongs to a different supplier"));
        }
        if order_date < c.start_date || order_date > c.end_date {
            return Err(AppError::conflict(format!(
                "order date {order_date} is outside the contract window {} to {}",
                c.start_date, c.end_date
            )));
        }
    }
    let txn = s.db.begin().await?;
    let now = Utc::now();
    let order_id = Uuid::new_v4();
    purchase_order::ActiveModel {
        id: Set(order_id),
        po_number: Set(gen_number("PO")),
        supplier_id: Set(i.supplier_id),
        warehouse_id: Set(i.warehouse_id),
        status: Set("draft".into()),
        expected_date: Set(i.expected_date),
        notes: Set(i.notes),
        created_at: Set(now),
        updated_at: Set(now),
        order_date: Set(order_date),
        po_type: Set(po_type),
        currency: Set(i.currency.clone().unwrap_or_else(|| "USD".into()).to_uppercase()),
        payment_terms_days: Set(terms),
        contract_id: Set(i.contract_id),
        auto_generated: Set(false),
        incoterm: Set(super::check_incoterm(i.incoterm.as_deref())?),
        transport_mode: Set(super::check_mode(i.transport_mode.as_deref())?),
        freight_cost: Set(nonneg_money("freight_cost", i.freight_cost)?),
        insurance_cost: Set(nonneg_money("insurance_cost", i.insurance_cost)?),
        freight_included: Set(i.freight_included.unwrap_or(false)),
    }
    .insert(&txn)
    .await?;

    for l in &i.lines {
        require_positive("line quantity", l.quantity)?;
        let p = get_or_404!(product::Entity, &txn, l.product_id, "product");
        if !p.active {
            return Err(AppError::conflict(format!("product {} is inactive", p.sku)));
        }
        let cost = l.unit_cost.unwrap_or(p.unit_cost);
        if cost.is_sign_negative() {
            return Err(AppError::bad("unit_cost must not be negative"));
        }
        purchase_order_line::ActiveModel {
            id: Set(Uuid::new_v4()),
            purchase_order_id: Set(order_id),
            product_id: Set(l.product_id),
            quantity_ordered: Set(l.quantity),
            quantity_received: Set(0),
            unit_cost: Set(cost),
            promised_date: Set(l.promised_date.or(i.expected_date)),
            budget_unit_cost: Set(l.budget_unit_cost),
        }
        .insert(&txn)
        .await?;
    }
    let detail = load_detail(&txn, order_id).await?;
    txn.commit().await?;
    Ok((StatusCode::CREATED, Json(detail)))
}

#[derive(Deserialize)]
pub struct PoQuery {
    status: Option<String>,
    supplier_id: Option<Uuid>,
    warehouse_id: Option<Uuid>,
    limit: Option<u64>,
    offset: Option<u64>,
}

async fn list_pos(
    State(s): State<AppState>,
    Query(q): Query<PoQuery>,
) -> AppResult<Json<Vec<purchase_order::Model>>> {
    let mut sel = purchase_order::Entity::find();
    if let Some(v) = q.status {
        sel = sel.filter(purchase_order::Column::Status.eq(v));
    }
    if let Some(v) = q.supplier_id {
        sel = sel.filter(purchase_order::Column::SupplierId.eq(v));
    }
    if let Some(v) = q.warehouse_id {
        sel = sel.filter(purchase_order::Column::WarehouseId.eq(v));
    }
    let rows = sel
        .order_by_desc(purchase_order::Column::CreatedAt)
        .limit(page_limit(q.limit))
        .offset(q.offset.unwrap_or(0))
        .all(&s.db)
        .await?;
    Ok(Json(rows))
}

async fn get_po(State(s): State<AppState>, Path(id): Path<Uuid>) -> AppResult<Json<PoDetail>> {
    Ok(Json(load_detail(&s.db, id).await?))
}

/// Lock the order row, check its status is one of `allowed`, then set `new_status`.
async fn transition(
    s: &AppState,
    id: Uuid,
    allowed: &[&str],
    new_status: &str,
) -> AppResult<PoDetail> {
    let txn = s.db.begin().await?;
    let order = purchase_order::Entity::find_by_id(id)
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::not_found("purchase order"))?;
    if !allowed.contains(&order.status.as_str()) {
        return Err(AppError::conflict(format!(
            "cannot move a '{}' purchase order to '{}'",
            order.status, new_status
        )));
    }
    let mut am: purchase_order::ActiveModel = order.into();
    am.status = Set(new_status.to_string());
    am.updated_at = Set(Utc::now());
    am.update(&txn).await?;
    let detail = load_detail(&txn, id).await?;
    txn.commit().await?;
    Ok(detail)
}

async fn submit_po(State(s): State<AppState>, Path(id): Path<Uuid>) -> AppResult<Json<PoDetail>> {
    Ok(Json(transition(&s, id, &["draft"], "submitted").await?))
}

/// Only orders with nothing received yet can be cancelled.
async fn cancel_po(State(s): State<AppState>, Path(id): Path<Uuid>) -> AppResult<Json<PoDetail>> {
    Ok(Json(transition(&s, id, &["draft", "submitted"], "cancelled").await?))
}

/// Close a partially received order: the outstanding balance is written off (supplier short-shipped).
async fn close_po(State(s): State<AppState>, Path(id): Path<Uuid>) -> AppResult<Json<PoDetail>> {
    Ok(Json(transition(&s, id, &["partially_received"], "closed_short").await?))
}

#[derive(Deserialize)]
pub struct ReceiveLine {
    line_id: Uuid,
    quantity: i64,
    /// Bin location (must belong to the PO's warehouse) where the goods are put away.
    location_id: Uuid,
    lot_number: Option<String>,
    /// Lot expiry; defaults to received_on + the product's shelf life.
    expiry_date: Option<NaiveDate>,
    /// Units that FAILED incoming inspection: they are put away but quarantined at once and a quality
    /// incident is opened against the supplier. The rest of `quantity` passes into available stock.
    rejected_qty: Option<i64>,
    defect_type: Option<String>,
    /// minor | major (default) | critical
    severity: Option<String>,
    /// Measured defect rate of the inspected sample (percent). When the product has an AQL and the
    /// rate exceeds it, the whole lot is rejected unless `rejected_qty` says otherwise.
    defect_rate_pct: Option<f64>,
}

#[derive(Deserialize)]
pub struct ReceiveReq {
    lines: Vec<ReceiveLine>,
    /// Date goods arrived (defaults to today); used for on-time and lead-time statistics.
    received_on: Option<NaiveDate>,
}

async fn receive_po(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<ReceiveReq>,
) -> AppResult<Json<PoDetail>> {
    if req.lines.is_empty() {
        return Err(AppError::bad("no lines to receive"));
    }
    let received_on = req.received_on.unwrap_or_else(|| Utc::now().date_naive());
    let txn = s.db.begin().await?;
    let order = purchase_order::Entity::find_by_id(id)
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::not_found("purchase order"))?;
    if !matches!(order.status.as_str(), "submitted" | "partially_received") {
        return Err(AppError::conflict(format!(
            "cannot receive against a '{}' purchase order",
            order.status
        )));
    }

    for r in &req.lines {
        require_positive("receive quantity", r.quantity)?;
        let line = purchase_order_line::Entity::find_by_id(r.line_id)
            .lock_exclusive()
            .one(&txn)
            .await?
            .filter(|l| l.purchase_order_id == id)
            .ok_or_else(|| AppError::not_found("purchase order line"))?;
        let remaining = line.quantity_ordered - line.quantity_received;
        if r.quantity > remaining {
            return Err(AppError::conflict(format!(
                "line {} has only {} units outstanding",
                line.id, remaining
            )));
        }
        let loc = get_or_404!(location::Entity, &txn, r.location_id, "location");
        if loc.warehouse_id != order.warehouse_id || !loc.active {
            return Err(AppError::bad(
                "location must be an active bin in the purchase order's warehouse",
            ));
        }
        let lot = clean_lot(&r.lot_number);
        let prod = get_or_404!(product::Entity, &txn, line.product_id, "product");
        ensure_storage_compatible(&prod, &loc)?;
        let expiry = default_expiry(&prod, received_on, r.expiry_date);
        add_stock(&txn, line.product_id, r.location_id, &lot, r.quantity, expiry).await?;
        super::check_pct("defect_rate_pct", r.defect_rate_pct)?;
        let aql_exceeded = matches!((r.defect_rate_pct, prod.aql_defect_pct), (Some(rate), Some(aql)) if rate > aql);
        let rejected = r.rejected_qty.unwrap_or(if aql_exceeded { r.quantity } else { 0 });
        if rejected < 0 || rejected > r.quantity {
            return Err(AppError::bad("rejected_qty must be between 0 and the received quantity"));
        }
        let severity = r.severity.clone().unwrap_or_else(|| "major".into());
        if !matches!(severity.as_str(), "minor" | "major" | "critical") {
            return Err(AppError::bad("severity must be minor, major or critical"));
        }
        let mut incident_id = None;
        if rejected > 0 {
            let row = txn
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE stock_levels SET blocked = blocked + $1, updated_at = now() \
                     WHERE product_id = $2 AND location_id = $3 AND lot_number = $4 RETURNING id",
                    [rejected.into(), line.product_id.into(), r.location_id.into(), lot.clone().into()],
                ))
                .await?
                .ok_or_else(|| AppError::not_found("stock level"))?;
            let inc = quality_incident::ActiveModel {
                id: Set(Uuid::new_v4()),
                supplier_id: Set(Some(order.supplier_id)),
                product_id: Set(line.product_id),
                purchase_order_id: Set(Some(id)),
                stock_level_id: Set(Some(row.try_get::<Uuid>("", "id")?)),
                severity: Set(severity),
                defect_type: Set(r.defect_type.clone().or_else(|| aql_exceeded.then(|| "AQL exceeded".to_string()))),
                quarantined_qty: Set(rejected),
                released_qty: Set(0),
                scrapped_qty: Set(0),
                reworked_qty: Set(0),
                status: Set("open".into()),
                note: Set(Some(format!("failed incoming inspection, PO {}", order.po_number))),
                occurred_on: Set(received_on),
                created_at: Set(Utc::now()),
            }
            .insert(&txn)
            .await?;
            incident_id = Some(inc.id);
        }
        goods_receipt::ActiveModel {
            id: Set(Uuid::new_v4()),
            purchase_order_id: Set(id),
            purchase_order_line_id: Set(line.id),
            product_id: Set(line.product_id),
            location_id: Set(r.location_id),
            lot_number: Set(lot.clone()),
            quantity: Set(r.quantity),
            received_on: Set(received_on),
            rejected_qty: Set(rejected),
            quality_incident_id: Set(incident_id),
            defect_rate_pct: Set(r.defect_rate_pct),
            created_at: Set(Utc::now()),
        }
        .insert(&txn)
        .await?;
        record_movement(
            &txn, line.product_id, r.location_id, &lot, "receipt", r.quantity,
            Some("purchase_order"), Some(id), Some(order.po_number.clone()),
        )
        .await?;
        let new_received = line.quantity_received + r.quantity;
        let mut am: purchase_order_line::ActiveModel = line.into();
        am.quantity_received = Set(new_received);
        am.update(&txn).await?;
    }

    let lines = purchase_order_line::Entity::find()
        .filter(purchase_order_line::Column::PurchaseOrderId.eq(id))
        .all(&txn)
        .await?;
    let complete = lines.iter().all(|l| l.quantity_received >= l.quantity_ordered);
    let mut am: purchase_order::ActiveModel = order.into();
    am.status = Set(if complete { "received" } else { "partially_received" }.to_string());
    am.updated_at = Set(Utc::now());
    am.update(&txn).await?;

    let detail = load_detail(&txn, id).await?;
    txn.commit().await?;
    Ok(Json(detail))
}

#[derive(Deserialize)]
pub struct FreightReq {
    freight_cost: Option<Decimal>,
    insurance_cost: Option<Decimal>,
    freight_included: Option<bool>,
    incoterm: Option<String>,
    transport_mode: Option<String>,
}

/// Book freight/insurance when the carrier or forwarder invoice arrives.
async fn set_freight(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(r): Json<FreightReq>,
) -> AppResult<Json<purchase_order::Model>> {
    let o = get_or_404!(purchase_order::Entity, &s.db, id, "purchase order");
    if o.status == "cancelled" {
        return Err(AppError::conflict("purchase order is cancelled"));
    }
    let mut am: purchase_order::ActiveModel = o.into();
    if r.freight_cost.is_some() {
        am.freight_cost = Set(nonneg_money("freight_cost", r.freight_cost)?);
    }
    if r.insurance_cost.is_some() {
        am.insurance_cost = Set(nonneg_money("insurance_cost", r.insurance_cost)?);
    }
    if let Some(v) = r.freight_included {
        am.freight_included = Set(v);
    }
    if r.incoterm.is_some() {
        am.incoterm = Set(super::check_incoterm(r.incoterm.as_deref())?);
    }
    if r.transport_mode.is_some() {
        am.transport_mode = Set(super::check_mode(r.transport_mode.as_deref())?);
    }
    am.updated_at = Set(Utc::now());
    Ok(Json(am.update(&s.db).await?))
}

#[derive(Serialize)]
pub struct LandedLine {
    line_id: Uuid,
    product_id: Uuid,
    quantity: i64,
    unit_cost: Decimal,
    line_value: Decimal,
    allocated_freight: Decimal,
    allocated_insurance: Decimal,
    landed_unit_cost: Decimal,
}

/// Freight and insurance are booked once per shipment; here they are spread over the order lines in
/// proportion to line value (SCMS data: 24% of lines only reference another line's freight). Uses received
/// quantities once goods have arrived, ordered quantities before that.
async fn landed_cost(State(s): State<AppState>, Path(id): Path<Uuid>) -> AppResult<Json<serde_json::Value>> {
    let d = load_detail(&s.db, id).await?;
    let any_received = d.lines.iter().any(|l| l.quantity_received > 0);
    let qty = |l: &purchase_order_line::Model| if any_received { l.quantity_received } else { l.quantity_ordered };
    let total: Decimal = d.lines.iter().map(|l| l.unit_cost * Decimal::from(qty(l))).sum();
    let (fr, ins) = if d.order.freight_included { (Decimal::ZERO, d.order.insurance_cost) } else { (d.order.freight_cost, d.order.insurance_cost) };
    let lines: Vec<LandedLine> = d
        .lines
        .iter()
        .map(|l| {
            let q = qty(l);
            let value = l.unit_cost * Decimal::from(q);
            let share = if total.is_zero() { Decimal::ZERO } else { value / total };
            let (af, ai) = (fr * share, ins * share);
            LandedLine {
                line_id: l.id,
                product_id: l.product_id,
                quantity: q,
                unit_cost: l.unit_cost,
                line_value: value,
                allocated_freight: af,
                allocated_insurance: ai,
                landed_unit_cost: if q > 0 { l.unit_cost + (af + ai) / Decimal::from(q) } else { l.unit_cost },
            }
        })
        .collect();
    let pct = if total.is_zero() { None } else { Some((fr + ins) / total) };
    Ok(Json(serde_json::json!({
        "po_number": d.order.po_number, "incoterm": d.order.incoterm, "transport_mode": d.order.transport_mode,
        "basis": if any_received { "received" } else { "ordered" }, "goods_value": total,
        "freight": fr, "insurance": ins, "freight_included_in_price": d.order.freight_included,
        "freight_and_insurance_pct_of_goods": pct, "lines": lines,
    })))
}
