//! Procurement controls: supplier contracts, invoice 3-way matching, spend-control KPIs.
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use chrono::{Duration, NaiveDate, Utc};
use sea_orm::{
    prelude::Decimal, ActiveModelTrait, ColumnTrait, ConnectionTrait, DbBackend, EntityTrait,
    FromQueryResult, QueryFilter, QueryOrder, QuerySelect, Set, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{page_limit, require_positive, require_text};
use crate::supply_chain::{
    entities::{contract, purchase_order, purchase_order_line, supplier, supplier_invoice, supplier_invoice_line},
    error::{AppError, AppResult},
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/contracts", post(create_contract).get(list_contracts))
        .route("/purchase-orders/{id}/invoices", post(create_invoice))
        .route("/invoices", get(list_invoices))
        .route("/invoices/{id}", get(get_invoice))
        .route("/invoices/{id}/approve", post(approve_invoice))
        .route("/invoices/{id}/pay", post(pay_invoice))
        .route("/reports/procurement-controls", get(controls))
}

/// Invoice unit price may exceed the PO price by at most this fraction before the match fails.
const PRICE_TOLERANCE: f64 = 0.02;

// ---- contracts -------------------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct NewContract {
    supplier_id: Uuid,
    contract_number: String,
    /// framework | long_term | master_supply | spot
    contract_type: Option<String>,
    start_date: NaiveDate,
    end_date: NaiveDate,
    currency: Option<String>,
}

async fn create_contract(
    State(s): State<AppState>,
    Json(i): Json<NewContract>,
) -> AppResult<(StatusCode, Json<contract::Model>)> {
    get_or_404!(supplier::Entity, &s.db, i.supplier_id, "supplier");
    if i.end_date < i.start_date {
        return Err(AppError::bad("end_date must not be before start_date"));
    }
    let ctype = i.contract_type.unwrap_or_else(|| "framework".into());
    if !matches!(ctype.as_str(), "framework" | "long_term" | "master_supply" | "spot") {
        return Err(AppError::bad("contract_type must be framework, long_term, master_supply or spot"));
    }
    let m = contract::ActiveModel {
        id: Set(Uuid::new_v4()),
        supplier_id: Set(i.supplier_id),
        contract_number: Set(require_text("contract_number", &i.contract_number)?.to_uppercase()),
        contract_type: Set(ctype),
        start_date: Set(i.start_date),
        end_date: Set(i.end_date),
        currency: Set(i.currency.unwrap_or_else(|| "USD".into()).to_uppercase()),
        created_at: Set(Utc::now()),
    }
    .insert(&s.db)
    .await?;
    Ok((StatusCode::CREATED, Json(m)))
}

#[derive(Deserialize)]
pub struct ContractQuery {
    supplier_id: Option<Uuid>,
    /// only contracts in force on this date
    active_on: Option<NaiveDate>,
    limit: Option<u64>,
    offset: Option<u64>,
}

async fn list_contracts(
    State(s): State<AppState>,
    Query(q): Query<ContractQuery>,
) -> AppResult<Json<Vec<contract::Model>>> {
    let mut sel = contract::Entity::find();
    if let Some(v) = q.supplier_id {
        sel = sel.filter(contract::Column::SupplierId.eq(v));
    }
    if let Some(d) = q.active_on {
        sel = sel
            .filter(contract::Column::StartDate.lte(d))
            .filter(contract::Column::EndDate.gte(d));
    }
    Ok(Json(
        sel.order_by_asc(contract::Column::ContractNumber)
            .limit(page_limit(q.limit))
            .offset(q.offset.unwrap_or(0))
            .all(&s.db)
            .await?,
    ))
}

// ---- invoices (3-way match: PO price x goods received x invoice) ------------------------------------

#[derive(Deserialize)]
pub struct NewInvoiceLine {
    po_line_id: Uuid,
    quantity: i64,
    unit_price: Decimal,
}

#[derive(Deserialize)]
pub struct NewInvoice {
    invoice_number: String,
    invoice_date: Option<NaiveDate>,
    lines: Vec<NewInvoiceLine>,
}

#[derive(Serialize)]
pub struct InvoiceDetail {
    invoice: supplier_invoice::Model,
    lines: Vec<supplier_invoice_line::Model>,
}

async fn load_invoice<C: ConnectionTrait>(db: &C, id: Uuid) -> AppResult<InvoiceDetail> {
    let invoice = get_or_404!(supplier_invoice::Entity, db, id, "invoice");
    let lines = supplier_invoice_line::Entity::find()
        .filter(supplier_invoice_line::Column::InvoiceId.eq(id))
        .all(db)
        .await?;
    Ok(InvoiceDetail { invoice, lines })
}

/// Records a supplier invoice and matches it automatically: invoiced quantity must not exceed
/// received-but-not-yet-invoiced quantity, and unit price must be within tolerance of the PO price.
/// Matches become `matched` (payable); anything else is `disputed` with the reasons recorded.
async fn create_invoice(
    State(s): State<AppState>,
    Path(po_id): Path<Uuid>,
    Json(i): Json<NewInvoice>,
) -> AppResult<(StatusCode, Json<InvoiceDetail>)> {
    if i.lines.is_empty() {
        return Err(AppError::bad("an invoice needs at least one line"));
    }
    let number = require_text("invoice_number", &i.invoice_number)?;
    let txn = s.db.begin().await?;
    let po = purchase_order::Entity::find_by_id(po_id)
        .lock_exclusive() // serialises invoicing per PO
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::not_found("purchase order"))?;
    if matches!(po.status.as_str(), "draft" | "cancelled") {
        return Err(AppError::conflict(format!("cannot invoice a '{}' purchase order", po.status)));
    }

    let mut issues: Vec<String> = Vec::new();
    let mut total = Decimal::ZERO;
    let invoice_id = Uuid::new_v4();
    let mut line_models = Vec::new();
    for l in &i.lines {
        require_positive("invoice line quantity", l.quantity)?;
        if l.unit_price.is_sign_negative() {
            return Err(AppError::bad("unit_price must not be negative"));
        }
        let pol = purchase_order_line::Entity::find_by_id(l.po_line_id)
            .one(&txn)
            .await?
            .filter(|x| x.purchase_order_id == po_id)
            .ok_or_else(|| AppError::not_found("purchase order line"))?;
        let invoiced: i64 = txn
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT COALESCE(SUM(quantity), 0)::bigint AS q FROM supplier_invoice_lines \
                 WHERE purchase_order_line_id = $1",
                [pol.id.into()],
            ))
            .await?
            .map(|r| r.try_get::<i64>("", "q"))
            .transpose()?
            .unwrap_or(0);
        let billable = pol.quantity_received - invoiced;
        if l.quantity > billable {
            issues.push(format!(
                "line {}: invoiced {} but only {} received and not yet invoiced",
                pol.id, l.quantity, billable.max(0)
            ));
        }
        let po_price = pol.unit_cost;
        let limit = po_price * (Decimal::ONE + Decimal::try_from(PRICE_TOLERANCE).unwrap_or_default());
        if l.unit_price > limit {
            issues.push(format!(
                "line {}: unit price {} exceeds PO price {} by more than {:.0}%",
                pol.id, l.unit_price, po_price, PRICE_TOLERANCE * 100.0
            ));
        }
        total += l.unit_price * Decimal::from(l.quantity);
        line_models.push((pol.id, l.quantity, l.unit_price));
    }

    let invoice_date = i.invoice_date.unwrap_or_else(|| Utc::now().date_naive());
    let status = if issues.is_empty() { "matched" } else { "disputed" };
    supplier_invoice::ActiveModel {
        id: Set(invoice_id),
        purchase_order_id: Set(po_id),
        invoice_number: Set(number),
        invoice_date: Set(invoice_date),
        due_date: Set(invoice_date + Duration::days(po.payment_terms_days as i64)),
        currency: Set(po.currency.clone()),
        total: Set(total),
        status: Set(status.into()),
        match_notes: Set((!issues.is_empty()).then(|| issues.join("; "))),
        paid_on: Set(None),
        created_at: Set(Utc::now()),
    }
    .insert(&txn)
    .await?;
    for (line_id, qty, price) in line_models {
        supplier_invoice_line::ActiveModel {
            id: Set(Uuid::new_v4()),
            invoice_id: Set(invoice_id),
            purchase_order_line_id: Set(line_id),
            quantity: Set(qty),
            unit_price: Set(price),
        }
        .insert(&txn)
        .await?;
    }
    let detail = load_invoice(&txn, invoice_id).await?;
    txn.commit().await?;
    Ok((StatusCode::CREATED, Json(detail)))
}

#[derive(Deserialize)]
pub struct InvoiceQuery {
    status: Option<String>,
    purchase_order_id: Option<Uuid>,
    limit: Option<u64>,
    offset: Option<u64>,
}

async fn list_invoices(
    State(s): State<AppState>,
    Query(q): Query<InvoiceQuery>,
) -> AppResult<Json<Vec<supplier_invoice::Model>>> {
    let mut sel = supplier_invoice::Entity::find();
    if let Some(v) = q.status {
        sel = sel.filter(supplier_invoice::Column::Status.eq(v));
    }
    if let Some(v) = q.purchase_order_id {
        sel = sel.filter(supplier_invoice::Column::PurchaseOrderId.eq(v));
    }
    Ok(Json(
        sel.order_by_desc(supplier_invoice::Column::CreatedAt)
            .limit(page_limit(q.limit))
            .offset(q.offset.unwrap_or(0))
            .all(&s.db)
            .await?,
    ))
}

async fn get_invoice(State(s): State<AppState>, Path(id): Path<Uuid>) -> AppResult<Json<InvoiceDetail>> {
    Ok(Json(load_invoice(&s.db, id).await?))
}

#[derive(Deserialize)]
pub struct ApproveReq {
    reason: String,
}

/// A buyer overrides a disputed match (e.g. price change agreed by phone). The reason is kept.
async fn approve_invoice(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(r): Json<ApproveReq>,
) -> AppResult<Json<InvoiceDetail>> {
    let reason = require_text("reason", &r.reason)?;
    let txn = s.db.begin().await?;
    let inv = supplier_invoice::Entity::find_by_id(id)
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::not_found("invoice"))?;
    if inv.status != "disputed" {
        return Err(AppError::conflict("only disputed invoices can be approved"));
    }
    let notes = format!("{} | approved: {}", inv.match_notes.clone().unwrap_or_default(), reason);
    let mut am: supplier_invoice::ActiveModel = inv.into();
    am.status = Set("matched".into());
    am.match_notes = Set(Some(notes));
    am.update(&txn).await?;
    let d = load_invoice(&txn, id).await?;
    txn.commit().await?;
    Ok(Json(d))
}

async fn pay_invoice(State(s): State<AppState>, Path(id): Path<Uuid>) -> AppResult<Json<InvoiceDetail>> {
    let txn = s.db.begin().await?;
    let inv = supplier_invoice::Entity::find_by_id(id)
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::not_found("invoice"))?;
    if inv.status != "matched" {
        return Err(AppError::conflict(format!("cannot pay a '{}' invoice; it must be matched", inv.status)));
    }
    let mut am: supplier_invoice::ActiveModel = inv.into();
    am.status = Set("paid".into());
    am.paid_on = Set(Some(Utc::now().date_naive()));
    am.update(&txn).await?;
    let d = load_invoice(&txn, id).await?;
    txn.commit().await?;
    Ok(Json(d))
}

// ---- controls report ---------------------------------------------------------------------------------

#[derive(Debug, FromQueryResult, Serialize)]
pub struct SpendControls {
    purchase_orders: i64,
    without_contract: i64,
    emergency_orders: i64,
    total_spend: f64,
    spend_without_contract: f64,
    budgeted_lines: i64,
    over_budget_lines: i64,
    /// (spend - budget) / budget over budgeted lines; positive = overspend
    price_variance_vs_budget: Option<f64>,
}

#[derive(Debug, FromQueryResult, Serialize)]
pub struct InvoiceControls {
    invoices: i64,
    matched: i64,
    disputed: i64,
    paid: i64,
    overdue_unpaid: i64,
    dispute_rate: Option<f64>,
}

#[derive(Serialize)]
pub struct Controls {
    spend: SpendControls,
    invoices: InvoiceControls,
}

/// Maverick-spend and invoice-quality KPIs (orders outside contract, emergency buying, price
/// variance vs budget, dispute rate, overdue payables).
async fn controls(State(s): State<AppState>) -> AppResult<Json<Controls>> {
    let spend = SpendControls::find_by_statement(Statement::from_string(
        DbBackend::Postgres,
        "WITH v AS (
           SELECT po.id, po.contract_id, po.po_type,
                  SUM(pol.quantity_ordered::numeric * pol.unit_cost)::float8 AS spend,
                  COUNT(*) FILTER (WHERE pol.budget_unit_cost IS NOT NULL)::bigint AS bl,
                  COUNT(*) FILTER (WHERE pol.budget_unit_cost IS NOT NULL AND pol.unit_cost > pol.budget_unit_cost)::bigint AS ob,
                  COALESCE(SUM(pol.quantity_ordered::numeric * pol.unit_cost) FILTER (WHERE pol.budget_unit_cost IS NOT NULL), 0)::float8 AS bspend,
                  COALESCE(SUM(pol.quantity_ordered::numeric * pol.budget_unit_cost), 0)::float8 AS bbudget
           FROM purchase_orders po JOIN purchase_order_lines pol ON pol.purchase_order_id = po.id
           WHERE po.status NOT IN ('draft', 'cancelled') GROUP BY po.id)
         SELECT COUNT(*)::bigint AS purchase_orders,
                COUNT(*) FILTER (WHERE contract_id IS NULL)::bigint AS without_contract,
                COUNT(*) FILTER (WHERE po_type = 'emergency')::bigint AS emergency_orders,
                COALESCE(SUM(spend), 0)::float8 AS total_spend,
                COALESCE(SUM(spend) FILTER (WHERE contract_id IS NULL), 0)::float8 AS spend_without_contract,
                COALESCE(SUM(bl), 0)::bigint AS budgeted_lines,
                COALESCE(SUM(ob), 0)::bigint AS over_budget_lines,
                (CASE WHEN SUM(bbudget) > 0 THEN (SUM(bspend) - SUM(bbudget)) / SUM(bbudget) END)::float8
                    AS price_variance_vs_budget
         FROM v"
            .to_string(),
    ))
    .one(&s.db)
    .await?
    .ok_or_else(|| AppError::not_found("spend data"))?;
    let invoices = InvoiceControls::find_by_statement(Statement::from_string(
        DbBackend::Postgres,
        "SELECT COUNT(*)::bigint AS invoices,
                COUNT(*) FILTER (WHERE status = 'matched')::bigint AS matched,
                COUNT(*) FILTER (WHERE status = 'disputed')::bigint AS disputed,
                COUNT(*) FILTER (WHERE status = 'paid')::bigint AS paid,
                COUNT(*) FILTER (WHERE status <> 'paid' AND due_date < CURRENT_DATE)::bigint AS overdue_unpaid,
                (CASE WHEN COUNT(*) > 0 THEN COUNT(*) FILTER (WHERE match_notes IS NOT NULL
                      AND match_notes NOT LIKE '%approved:%')::float8 / COUNT(*) END)::float8 AS dispute_rate
         FROM supplier_invoices"
            .to_string(),
    ))
    .one(&s.db)
    .await?
    .ok_or_else(|| AppError::not_found("invoice data"))?;
    Ok(Json(Controls { spend, invoices }))
}
