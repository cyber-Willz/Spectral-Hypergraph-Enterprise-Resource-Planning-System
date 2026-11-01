//! Accounts payable: vendors, bills, payments, voids, aging.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{Duration, NaiveDate};
use rust_decimal::Decimal;
use sea_orm::{
    prelude::*, ActiveValue::{NotSet, Set}, QueryOrder, QuerySelect, QueryTrait, TransactionTrait,
};
use serde::Deserialize;

use super::{
    cash::{active_bank, record_cash},
    ledger::{account_by_code, build_aging, post_entry, reverse_entry, AgingReport, AsOf, NewLine, AP},
};
use crate::{
    erp::entities::{account, bill, bill_payment, vendor},
    error::{ApiResult, AppError, Created},
    erp::util::{clamp_limit, non_empty, require_positive, today},
    state::AppState,
};

#[derive(Deserialize)]
pub struct NewVendor {
    name: String,
    email: Option<String>,
    phone: Option<String>,
    payment_terms_days: Option<i32>,
}

pub async fn create_vendor(State(st): State<AppState>, Json(req): Json<NewVendor>) -> Created<vendor::Model> {
    let terms = req.payment_terms_days.unwrap_or(30);
    if !(0..=365).contains(&terms) {
        return Err(AppError::BadRequest("payment_terms_days must be between 0 and 365".into()));
    }
    let m = vendor::ActiveModel {
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

pub async fn list_vendors(State(st): State<AppState>) -> ApiResult<Vec<vendor::Model>> {
    Ok(Json(vendor::Entity::find().order_by_asc(vendor::Column::Name).all(&st.db).await?))
}

#[derive(Deserialize)]
pub struct NewBill {
    vendor_id: i64,
    bill_number: String,
    bill_date: NaiveDate,
    /// Defaults to bill_date plus the vendor's payment terms.
    due_date: Option<NaiveDate>,
    amount: Decimal,
    expense_account_id: i64,
    description: Option<String>,
}

pub async fn create_bill(State(st): State<AppState>, Json(req): Json<NewBill>) -> Created<bill::Model> {
    let amount = require_positive(req.amount, "amount")?;
    let number = non_empty(&req.bill_number, "bill_number")?;
    let txn = st.db.begin().await?;
    let v = vendor::Entity::find_by_id(req.vendor_id)
        .one(&txn)
        .await?
        .filter(|v| v.is_active)
        .ok_or_else(|| AppError::BadRequest("vendor does not exist or is inactive".into()))?;
    let exp = account::Entity::find_by_id(req.expense_account_id)
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::BadRequest("expense_account_id does not exist".into()))?;
    if exp.account_type != "expense" && exp.account_type != "asset" {
        return Err(AppError::BadRequest("a bill must be charged to an expense or asset account".into()));
    }
    let due = req.due_date.unwrap_or(req.bill_date + Duration::days(v.payment_terms_days as i64));
    if due < req.bill_date {
        return Err(AppError::BadRequest("due_date must not be before bill_date".into()));
    }
    let ap = account_by_code(&txn, AP).await?;
    let je = post_entry(
        &txn,
        req.bill_date,
        &format!("Bill {number} from {}", v.name),
        "bill",
        None,
        None,
        vec![NewLine::dr(exp.id, amount), NewLine::cr(ap.id, amount)],
    )
    .await?;
    let b = bill::ActiveModel {
        id: NotSet,
        vendor_id: Set(v.id),
        bill_number: Set(number),
        bill_date: Set(req.bill_date),
        due_date: Set(due),
        amount: Set(amount),
        amount_paid: Set(Decimal::ZERO),
        expense_account_id: Set(exp.id),
        description: Set(req.description),
        status: Set("open".into()),
        journal_entry_id: Set(Some(je)),
        created_at: NotSet,
    }
    .insert(&txn)
    .await?;
    txn.commit().await?;
    Ok((StatusCode::CREATED, Json(b)))
}

#[derive(Deserialize)]
pub struct BillQuery {
    status: Option<String>,
    vendor_id: Option<i64>,
    limit: Option<u64>,
}

pub async fn list_bills(State(st): State<AppState>, Query(q): Query<BillQuery>) -> ApiResult<Vec<bill::Model>> {
    let rows = bill::Entity::find()
        .apply_if(q.status, |s, v| s.filter(bill::Column::Status.eq(v)))
        .apply_if(q.vendor_id, |s, v| s.filter(bill::Column::VendorId.eq(v)))
        .order_by_desc(bill::Column::DueDate)
        .limit(clamp_limit(q.limit))
        .all(&st.db)
        .await?;
    Ok(Json(rows))
}

pub async fn get_bill(State(st): State<AppState>, Path(id): Path<i64>) -> ApiResult<bill::Model> {
    bill::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .map(Json)
        .ok_or_else(|| AppError::not_found(format!("bill {id}")))
}

#[derive(Deserialize)]
pub struct PayReq {
    bank_account_id: i64,
    payment_date: Option<NaiveDate>,
    /// Defaults to the full outstanding balance.
    amount: Option<Decimal>,
}

pub async fn pay_bill(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<PayReq>,
) -> Created<bill::Model> {
    let date = req.payment_date.unwrap_or_else(today);
    let txn = st.db.begin().await?;
    let b = bill::Entity::find_by_id(id)
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::not_found(format!("bill {id}")))?;
    if b.status != "open" {
        return Err(AppError::Conflict(format!("bill is {}", b.status)));
    }
    let remaining = b.amount - b.amount_paid;
    let amount = match req.amount {
        Some(a) => require_positive(a, "amount")?,
        None => remaining,
    };
    if amount > remaining {
        return Err(AppError::BadRequest(format!("payment exceeds outstanding balance of {remaining}")));
    }
    let bank = active_bank(&txn, req.bank_account_id).await?;
    let ap = account_by_code(&txn, AP).await?;
    let desc = format!("Payment of bill {}", b.bill_number);
    let je = post_entry(
        &txn,
        date,
        &desc,
        "bill_payment",
        Some(b.id),
        None,
        vec![NewLine::dr(ap.id, amount), NewLine::cr(bank.gl_account_id, amount)],
    )
    .await?;
    let pay = bill_payment::ActiveModel {
        id: NotSet,
        bill_id: Set(b.id),
        payment_date: Set(date),
        amount: Set(amount),
        bank_account_id: Set(bank.id),
        journal_entry_id: Set(Some(je)),
        created_at: NotSet,
    }
    .insert(&txn)
    .await?;
    record_cash(&txn, bank.id, date, -amount, &desc, "bill_payment", Some(pay.id), je).await?;

    let paid = b.amount_paid + amount;
    let status = if paid == b.amount { "paid" } else { "open" };
    let mut am: bill::ActiveModel = b.into();
    am.amount_paid = Set(paid);
    am.status = Set(status.into());
    let updated = am.update(&txn).await?;
    txn.commit().await?;
    Ok((StatusCode::CREATED, Json(updated)))
}

pub async fn void_bill(State(st): State<AppState>, Path(id): Path<i64>) -> ApiResult<bill::Model> {
    let txn = st.db.begin().await?;
    let b = bill::Entity::find_by_id(id)
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::not_found(format!("bill {id}")))?;
    if b.status != "open" || !b.amount_paid.is_zero() {
        return Err(AppError::Conflict("only open bills with no payments can be voided".into()));
    }
    if let Some(je) = b.journal_entry_id {
        reverse_entry(&txn, je, today(), &format!("Void of bill {}", b.bill_number)).await?;
    }
    let mut am: bill::ActiveModel = b.into();
    am.status = Set("void".into());
    let updated = am.update(&txn).await?;
    txn.commit().await?;
    Ok(Json(updated))
}

pub async fn aging(State(st): State<AppState>, Query(q): Query<AsOf>) -> ApiResult<AgingReport> {
    let as_of = q.as_of.unwrap_or_else(today);
    let bills = bill::Entity::find().filter(bill::Column::Status.eq("open")).all(&st.db).await?;
    let names: std::collections::HashMap<i64, String> =
        vendor::Entity::find().all(&st.db).await?.into_iter().map(|v| (v.id, v.name)).collect();
    let docs = bills
        .into_iter()
        .map(|b| {
            let party = names.get(&b.vendor_id).cloned().unwrap_or_default();
            (b.id, party, b.bill_number, b.due_date, b.amount - b.amount_paid)
        })
        .collect();
    Ok(Json(build_aging(docs, as_of)))
}
