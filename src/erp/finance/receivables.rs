//! Accounts receivable: customers, invoices, receipts, voids, aging.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{Duration, NaiveDate};
use rust_decimal::Decimal;
use sea_orm::{
    prelude::*, ActiveValue::{NotSet, Set}, ConnectionTrait, QueryOrder, QuerySelect, QueryTrait, TransactionTrait,
};
use serde::Deserialize;

use super::{
    cash::{active_bank, record_cash},
    ledger::{account_by_code, build_aging, post_entry, reverse_entry, AgingReport, AsOf, NewLine, AR},
};
use crate::{
    erp::entities::{account, customer, invoice, invoice_payment},
    error::{ApiResult, AppError, Created},
    erp::util::{clamp_limit, non_empty, require_positive, today},
    state::AppState,
};

#[derive(Deserialize)]
pub struct NewCustomer {
    name: String,
    email: Option<String>,
    phone: Option<String>,
    payment_terms_days: Option<i32>,
}

pub async fn create_customer(State(st): State<AppState>, Json(req): Json<NewCustomer>) -> Created<customer::Model> {
    let terms = req.payment_terms_days.unwrap_or(30);
    if !(0..=365).contains(&terms) {
        return Err(AppError::BadRequest("payment_terms_days must be between 0 and 365".into()));
    }
    let m = customer::ActiveModel {
        id: NotSet,
        name: Set(non_empty(&req.name, "name")?),
        email: Set(req.email),
        phone: Set(req.phone),
        payment_terms_days: Set(terms),
        is_active: Set(true),
        created_at: NotSet,
    }
    .insert(&st.db)
    .await?;
    Ok((StatusCode::CREATED, Json(m)))
}

pub async fn list_customers(State(st): State<AppState>) -> ApiResult<Vec<customer::Model>> {
    Ok(Json(customer::Entity::find().order_by_asc(customer::Column::Name).all(&st.db).await?))
}

#[derive(Deserialize)]
pub struct NewInvoice {
    customer_id: i64,
    invoice_number: String,
    invoice_date: NaiveDate,
    /// Defaults to invoice_date plus the customer's payment terms.
    due_date: Option<NaiveDate>,
    amount: Decimal,
    revenue_account_id: i64,
    description: Option<String>,
}

pub async fn create_invoice(State(st): State<AppState>, Json(req): Json<NewInvoice>) -> Created<invoice::Model> {
    let txn = st.db.begin().await?;
    let inv = issue_invoice(&txn, IssueInvoice {
        customer_id: req.customer_id,
        invoice_number: req.invoice_number,
        invoice_date: req.invoice_date,
        due_date: req.due_date,
        net_amount: req.amount,
        tax: Decimal::ZERO,
        revenue_account_id: req.revenue_account_id,
        description: req.description,
    }).await?;
    txn.commit().await?;
    Ok((StatusCode::CREATED, Json(inv)))
}

pub struct IssueInvoice {
    pub customer_id: i64,
    pub invoice_number: String,
    pub invoice_date: NaiveDate,
    pub due_date: Option<NaiveDate>,
    /// Revenue, excluding tax.
    pub net_amount: Decimal,
    /// Sales tax collected on top (credited to 2200 Sales Tax Payable); the invoice total is net + tax.
    pub tax: Decimal,
    pub revenue_account_id: i64,
    pub description: Option<String>,
}

/// Validates, posts the balanced journal entry (Dr AR, Cr Revenue [, Cr Sales Tax Payable]) and records the
/// invoice, all inside the caller's transaction. Shared by the AR endpoint and the CRM/PM bridges.
pub async fn issue_invoice<C: ConnectionTrait>(txn: &C, r: IssueInvoice) -> Result<invoice::Model, AppError> {
    let net = require_positive(r.net_amount, "amount")?;
    let tax = crate::erp::util::require_non_negative(r.tax, "tax")?;
    let gross = net + tax;
    let number = non_empty(&r.invoice_number, "invoice_number")?;
    let c = customer::Entity::find_by_id(r.customer_id)
        .one(txn)
        .await?
        .filter(|c| c.is_active)
        .ok_or_else(|| AppError::BadRequest("customer does not exist or is inactive".into()))?;
    let bankrupt = crate::erp::entities::counterparty_risk_profile::Entity::find()
        .filter(crate::erp::entities::counterparty_risk_profile::Column::PartyKind.eq("customer"))
        .filter(crate::erp::entities::counterparty_risk_profile::Column::PartyId.eq(c.id))
        .filter(crate::erp::entities::counterparty_risk_profile::Column::IsBankrupt.eq(true))
        .count(txn)
        .await?;
    if bankrupt > 0 {
        return Err(AppError::Conflict("customer is flagged bankrupt; new invoices are blocked".into()));
    }
    let rev = account::Entity::find_by_id(r.revenue_account_id)
        .one(txn)
        .await?
        .ok_or_else(|| AppError::BadRequest("revenue_account_id does not exist".into()))?;
    if rev.account_type != "revenue" {
        return Err(AppError::BadRequest("an invoice must be credited to a revenue account".into()));
    }
    let due = r.due_date.unwrap_or(r.invoice_date + Duration::days(c.payment_terms_days as i64));
    if due < r.invoice_date {
        return Err(AppError::BadRequest("due_date must not be before invoice_date".into()));
    }
    let ar = account_by_code(txn, AR).await?;
    let mut lines = vec![NewLine::dr(ar.id, gross), NewLine::cr(rev.id, net)];
    if tax > Decimal::ZERO {
        lines.push(NewLine::cr(account_by_code(txn, "2200").await?.id, tax));
    }
    let je = post_entry(txn, r.invoice_date, &format!("Invoice {number} to {}", c.name), "invoice", None, None, lines).await?;
    Ok(invoice::ActiveModel {
        id: NotSet,
        customer_id: Set(c.id),
        invoice_number: Set(number),
        invoice_date: Set(r.invoice_date),
        due_date: Set(due),
        amount: Set(gross),
        amount_paid: Set(Decimal::ZERO),
        revenue_account_id: Set(rev.id),
        description: Set(r.description),
        status: Set("open".into()),
        journal_entry_id: Set(Some(je)),
        created_at: NotSet,
    }
    .insert(txn)
    .await?)
}

#[derive(Deserialize)]
pub struct InvoiceQuery {
    status: Option<String>,
    customer_id: Option<i64>,
    limit: Option<u64>,
}

pub async fn list_invoices(
    State(st): State<AppState>,
    Query(q): Query<InvoiceQuery>,
) -> ApiResult<Vec<invoice::Model>> {
    let rows = invoice::Entity::find()
        .apply_if(q.status, |s, v| s.filter(invoice::Column::Status.eq(v)))
        .apply_if(q.customer_id, |s, v| s.filter(invoice::Column::CustomerId.eq(v)))
        .order_by_desc(invoice::Column::DueDate)
        .limit(clamp_limit(q.limit))
        .all(&st.db)
        .await?;
    Ok(Json(rows))
}

pub async fn get_invoice(State(st): State<AppState>, Path(id): Path<i64>) -> ApiResult<invoice::Model> {
    invoice::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .map(Json)
        .ok_or_else(|| AppError::not_found(format!("invoice {id}")))
}

#[derive(Deserialize)]
pub struct ReceiptReq {
    bank_account_id: i64,
    payment_date: Option<NaiveDate>,
    /// Defaults to the full outstanding balance.
    amount: Option<Decimal>,
}

pub async fn receive_payment(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<ReceiptReq>,
) -> Created<invoice::Model> {
    let date = req.payment_date.unwrap_or_else(today);
    let txn = st.db.begin().await?;
    let inv = invoice::Entity::find_by_id(id)
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::not_found(format!("invoice {id}")))?;
    if inv.status != "open" {
        return Err(AppError::Conflict(format!("invoice is {}", inv.status)));
    }
    let remaining = inv.amount - inv.amount_paid;
    let amount = match req.amount {
        Some(a) => require_positive(a, "amount")?,
        None => remaining,
    };
    if amount > remaining {
        return Err(AppError::BadRequest(format!("payment exceeds outstanding balance of {remaining}")));
    }
    let bank = active_bank(&txn, req.bank_account_id).await?;
    let ar = account_by_code(&txn, AR).await?;
    let desc = format!("Receipt for invoice {}", inv.invoice_number);
    let je = post_entry(
        &txn,
        date,
        &desc,
        "invoice_payment",
        Some(inv.id),
        None,
        vec![NewLine::dr(bank.gl_account_id, amount), NewLine::cr(ar.id, amount)],
    )
    .await?;
    let pay = invoice_payment::ActiveModel {
        id: NotSet,
        invoice_id: Set(inv.id),
        payment_date: Set(date),
        amount: Set(amount),
        bank_account_id: Set(bank.id),
        journal_entry_id: Set(Some(je)),
        created_at: NotSet,
    }
    .insert(&txn)
    .await?;
    record_cash(&txn, bank.id, date, amount, &desc, "invoice_payment", Some(pay.id), je).await?;

    let paid = inv.amount_paid + amount;
    let status = if paid == inv.amount { "paid" } else { "open" };
    let mut am: invoice::ActiveModel = inv.into();
    am.amount_paid = Set(paid);
    am.status = Set(status.into());
    let updated = am.update(&txn).await?;
    txn.commit().await?;
    Ok((StatusCode::CREATED, Json(updated)))
}

pub async fn void_invoice(State(st): State<AppState>, Path(id): Path<i64>) -> ApiResult<invoice::Model> {
    let txn = st.db.begin().await?;
    let inv = invoice::Entity::find_by_id(id)
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::not_found(format!("invoice {id}")))?;
    if inv.status != "open" || !inv.amount_paid.is_zero() {
        return Err(AppError::Conflict("only open invoices with no payments can be voided".into()));
    }
    if let Some(je) = inv.journal_entry_id {
        reverse_entry(&txn, je, today(), &format!("Void of invoice {}", inv.invoice_number)).await?;
    }
    let mut am: invoice::ActiveModel = inv.into();
    am.status = Set("void".into());
    let updated = am.update(&txn).await?;
    txn.commit().await?;
    Ok(Json(updated))
}

pub async fn aging(State(st): State<AppState>, Query(q): Query<AsOf>) -> ApiResult<AgingReport> {
    let as_of = q.as_of.unwrap_or_else(today);
    let open = invoice::Entity::find().filter(invoice::Column::Status.eq("open")).all(&st.db).await?;
    let names: std::collections::HashMap<i64, String> =
        customer::Entity::find().all(&st.db).await?.into_iter().map(|c| (c.id, c.name)).collect();
    let docs = open
        .into_iter()
        .map(|i| {
            let party = names.get(&i.customer_id).cloned().unwrap_or_default();
            (i.id, party, i.invoice_number, i.due_date, i.amount - i.amount_paid)
        })
        .collect();
    Ok(Json(build_aging(docs, as_of)))
}
