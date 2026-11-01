//! Cash management: bank accounts, cash transactions, reconciliation, forecast.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{Duration, NaiveDate};
use rust_decimal::Decimal;
use sea_orm::{
    prelude::*, ActiveValue::{NotSet, Set}, ConnectionTrait, FromQueryResult, QueryOrder, QuerySelect,
    QueryTrait, TransactionTrait,
};
use serde::{Deserialize, Serialize};

use super::ledger::{post_entry, NewLine};
use crate::{
    erp::entities::{account, bank_account, bill, cash_transaction, invoice},
    error::{ApiResult, AppError, Created},
    erp::util::{clamp_limit, non_empty, stmt, today},
    state::AppState,
};

/// Records a cash movement against a bank account (signed amount: + in, - out).
pub async fn record_cash<C: ConnectionTrait>(
    c: &C,
    bank_account_id: i64,
    date: NaiveDate,
    amount: Decimal,
    description: &str,
    source: &str,
    source_id: Option<i64>,
    journal_entry_id: i64,
) -> Result<cash_transaction::Model, AppError> {
    Ok(cash_transaction::ActiveModel {
        id: NotSet,
        bank_account_id: Set(bank_account_id),
        txn_date: Set(date),
        amount: Set(amount),
        description: Set(description.to_owned()),
        source: Set(source.to_owned()),
        source_id: Set(source_id),
        journal_entry_id: Set(Some(journal_entry_id)),
        reconciled: Set(false),
        reconciled_on: Set(None),
        created_at: NotSet,
    }
    .insert(c)
    .await?)
}

/// Bank GL accounts may only move through cash transactions, bill/invoice payments and
/// payroll, so the cash subledger and the GL cannot drift apart.
pub async fn reject_bank_accounts<C: ConnectionTrait>(c: &C, account_ids: &[i64]) -> Result<(), AppError> {
    let n = bank_account::Entity::find()
        .filter(bank_account::Column::GlAccountId.is_in(account_ids.to_vec()))
        .count(c)
        .await?;
    if n > 0 {
        return Err(AppError::BadRequest(
            "this entry touches a bank account's GL account; use cash transactions, bill/invoice payments or payroll instead".into(),
        ));
    }
    Ok(())
}

/// Receivables (1100) and payables (2000) are control accounts: only invoices, bills and their
/// payments may move them, otherwise the subledgers stop reconciling to the GL.
pub async fn reject_control_accounts<C: ConnectionTrait>(c: &C, account_ids: &[i64]) -> Result<(), AppError> {
    let n = account::Entity::find()
        .filter(account::Column::Id.is_in(account_ids.to_vec()))
        .filter(account::Column::Code.is_in([super::ledger::AR, super::ledger::AP]))
        .count(c)
        .await?;
    if n > 0 {
        return Err(AppError::BadRequest(
            "this entry touches a receivables/payables control account; use invoices, bills and their payments instead".into(),
        ));
    }
    Ok(())
}

pub async fn active_bank<C: ConnectionTrait>(c: &C, id: i64) -> Result<bank_account::Model, AppError> {
    let b = bank_account::Entity::find_by_id(id)
        .one(c)
        .await?
        .ok_or_else(|| AppError::BadRequest(format!("bank account {id} does not exist")))?;
    if !b.is_active {
        return Err(AppError::BadRequest(format!("bank account {id} is inactive")));
    }
    Ok(b)
}

#[derive(Deserialize)]
pub struct NewBank {
    name: String,
    /// Last digits only (max 8). Full account numbers are deliberately not stored.
    account_mask: Option<String>,
    gl_account_id: i64,
    currency: Option<String>,
}

pub async fn create_bank(State(st): State<AppState>, Json(req): Json<NewBank>) -> Created<bank_account::Model> {
    let gl = account::Entity::find_by_id(req.gl_account_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::BadRequest("gl_account_id does not exist".into()))?;
    if gl.account_type != "asset" {
        return Err(AppError::BadRequest("the GL account of a bank account must be an asset account".into()));
    }
    if let Some(m) = &req.account_mask {
        if m.len() > 8 || !m.chars().all(|c| c.is_ascii_digit()) {
            return Err(AppError::BadRequest("account_mask must be at most 8 digits".into()));
        }
    }
    let currency = req.currency.unwrap_or_else(|| "USD".into()).to_uppercase();
    if currency.len() != 3 || !currency.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(AppError::BadRequest("currency must be a 3-letter code".into()));
    }
    let m = bank_account::ActiveModel {
        id: NotSet,
        name: Set(non_empty(&req.name, "name")?),
        account_mask: Set(req.account_mask),
        gl_account_id: Set(req.gl_account_id),
        currency: Set(currency),
        is_active: Set(true),
        created_at: NotSet,
    }
    .insert(&st.db)
    .await?;
    Ok((StatusCode::CREATED, Json(m)))
}

#[derive(Serialize, FromQueryResult)]
pub struct BankView {
    id: i64,
    name: String,
    account_mask: Option<String>,
    gl_account_id: i64,
    currency: String,
    is_active: bool,
    balance: Decimal,
    unreconciled_count: i64,
}

pub async fn list_banks(State(st): State<AppState>) -> ApiResult<Vec<BankView>> {
    let sql = r#"
        SELECT b.id, b.name, b.account_mask, b.gl_account_id, b.currency, b.is_active,
               COALESCE(SUM(t.amount), 0) AS balance,
               COUNT(t.id) FILTER (WHERE NOT t.reconciled) AS unreconciled_count
        FROM bank_accounts b LEFT JOIN cash_transactions t ON t.bank_account_id = b.id
        GROUP BY b.id ORDER BY b.name"#;
    Ok(Json(BankView::find_by_statement(stmt(sql, vec![])).all(&st.db).await?))
}

#[derive(Deserialize)]
pub struct NewCashTxn {
    bank_account_id: i64,
    txn_date: NaiveDate,
    /// Signed: positive is a deposit, negative a withdrawal.
    amount: Decimal,
    description: String,
    /// The other side of the entry (e.g. revenue, expense, equity).
    offset_account_id: i64,
}

pub async fn create_txn(State(st): State<AppState>, Json(req): Json<NewCashTxn>) -> Created<cash_transaction::Model> {
    if req.amount.is_zero() {
        return Err(AppError::BadRequest("amount must not be zero".into()));
    }
    let desc = non_empty(&req.description, "description")?;
    let txn = st.db.begin().await?;
    let bank = active_bank(&txn, req.bank_account_id).await?;
    reject_bank_accounts(&txn, &[req.offset_account_id]).await?;
    reject_control_accounts(&txn, &[req.offset_account_id]).await?;
    let abs = req.amount.abs();
    let lines = if req.amount > Decimal::ZERO {
        vec![NewLine::dr(bank.gl_account_id, abs), NewLine::cr(req.offset_account_id, abs)]
    } else {
        vec![NewLine::dr(req.offset_account_id, abs), NewLine::cr(bank.gl_account_id, abs)]
    };
    let je = post_entry(&txn, req.txn_date, &desc, "cash", None, None, lines).await?;
    let row = record_cash(&txn, bank.id, req.txn_date, req.amount, &desc, "manual", None, je).await?;
    txn.commit().await?;
    Ok((StatusCode::CREATED, Json(row)))
}

#[derive(Deserialize)]
pub struct TxnQuery {
    bank_account_id: Option<i64>,
    reconciled: Option<bool>,
    limit: Option<u64>,
}

pub async fn list_txns(
    State(st): State<AppState>,
    Query(q): Query<TxnQuery>,
) -> ApiResult<Vec<cash_transaction::Model>> {
    let rows = cash_transaction::Entity::find()
        .apply_if(q.bank_account_id, |s, v| s.filter(cash_transaction::Column::BankAccountId.eq(v)))
        .apply_if(q.reconciled, |s, v| s.filter(cash_transaction::Column::Reconciled.eq(v)))
        .order_by_desc(cash_transaction::Column::TxnDate)
        .order_by_desc(cash_transaction::Column::Id)
        .limit(clamp_limit(q.limit))
        .all(&st.db)
        .await?;
    Ok(Json(rows))
}

#[derive(Deserialize, Default)]
pub struct ReconcileReq {
    reconciled_on: Option<NaiveDate>,
}

pub async fn reconcile(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    body: Option<Json<ReconcileReq>>,
) -> ApiResult<cash_transaction::Model> {
    let on = body.and_then(|b| b.0.reconciled_on).unwrap_or_else(today);
    let txn = st.db.begin().await?;
    let row = cash_transaction::Entity::find_by_id(id)
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::not_found(format!("cash transaction {id}")))?;
    if row.reconciled {
        return Err(AppError::Conflict("transaction is already reconciled".into()));
    }
    let mut am: cash_transaction::ActiveModel = row.into();
    am.reconciled = Set(true);
    am.reconciled_on = Set(Some(on));
    let updated = am.update(&txn).await?;
    txn.commit().await?;
    Ok(Json(updated))
}

#[derive(Deserialize)]
pub struct ForecastQuery {
    weeks: Option<i64>,
}

#[derive(Serialize)]
pub struct ForecastWeek {
    week_starting: NaiveDate,
    expected_inflow: Decimal,
    expected_outflow: Decimal,
    projected_balance: Decimal,
}

#[derive(Serialize)]
pub struct Forecast {
    as_of: NaiveDate,
    opening_cash: Decimal,
    /// Overdue receivables/payables are assumed settled in the first week.
    assumption: &'static str,
    weeks: Vec<ForecastWeek>,
}

/// Cash forecast from open receivables (inflows) and payables (outflows) by due date.
pub async fn forecast(State(st): State<AppState>, Query(q): Query<ForecastQuery>) -> ApiResult<Forecast> {
    let weeks_n = q.weeks.unwrap_or(13).clamp(1, 52);
    let start = today();
    let opening: Decimal = cash_transaction::Entity::find()
        .select_only()
        .column_as(cash_transaction::Column::Amount.sum(), "s")
        .into_tuple::<Option<Decimal>>()
        .one(&st.db)
        .await?
        .flatten()
        .unwrap_or_default();

    let mut inflow = vec![Decimal::ZERO; weeks_n as usize];
    let mut outflow = vec![Decimal::ZERO; weeks_n as usize];
    let slot = |due: NaiveDate| -> Option<usize> {
        let idx = ((due - start).num_days().max(0) / 7) as usize;
        (idx < weeks_n as usize).then_some(idx)
    };
    for i in invoice::Entity::find().filter(invoice::Column::Status.eq("open")).all(&st.db).await? {
        if let Some(ix) = slot(i.due_date) {
            inflow[ix] += i.amount - i.amount_paid;
        }
    }
    for b in bill::Entity::find().filter(bill::Column::Status.eq("open")).all(&st.db).await? {
        if let Some(ix) = slot(b.due_date) {
            outflow[ix] += b.amount - b.amount_paid;
        }
    }
    let mut bal = opening;
    let weeks = (0..weeks_n as usize)
        .map(|i| {
            bal += inflow[i] - outflow[i];
            ForecastWeek {
                week_starting: start + Duration::days(7 * i as i64),
                expected_inflow: inflow[i],
                expected_outflow: outflow[i],
                projected_balance: bal,
            }
        })
        .collect();
    Ok(Json(Forecast {
        as_of: start,
        opening_cash: opening,
        assumption: "open invoices are collected and open bills paid on their due dates; overdue items land in week 1",
        weeks,
    }))
}
