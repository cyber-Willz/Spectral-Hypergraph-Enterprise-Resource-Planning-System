//! General ledger: chart of accounts, double-entry posting core, journal API and statements.

use std::collections::HashSet;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sea_orm::{
    prelude::*, ActiveValue::{NotSet, Set}, ConnectionTrait, FromQueryResult, QueryOrder,
    QuerySelect, TransactionTrait,
};
use serde::{Deserialize, Serialize};

use crate::{
    erp::entities::{account, journal_entry, journal_line},
    error::{ApiResult, AppError, Created},
    erp::util::{clamp_limit, non_empty, require_scale, stmt, today},
    state::AppState,
};

// System account codes seeded by the migration.
pub const CASH: &str = "1000";
pub const AR: &str = "1100";
pub const AP: &str = "2000";
pub const PAYROLL_LIABILITIES: &str = "2100";
pub const SALARY_EXPENSE: &str = "5000";
pub const BENEFITS_EXPENSE: &str = "5100";

const ACCOUNT_TYPES: [&str; 5] = ["asset", "liability", "equity", "revenue", "expense"];

pub async fn account_by_code<C: ConnectionTrait>(c: &C, code: &str) -> Result<account::Model, AppError> {
    account::Entity::find()
        .filter(account::Column::Code.eq(code))
        .one(c)
        .await?
        .ok_or_else(|| {
            tracing::error!(code, "required system account is missing");
            AppError::internal()
        })
}

#[derive(Debug, Clone)]
pub struct NewLine {
    pub account_id: i64,
    pub debit: Decimal,
    pub credit: Decimal,
    pub memo: Option<String>,
}

impl NewLine {
    pub fn dr(account_id: i64, amount: Decimal) -> Self {
        Self { account_id, debit: amount, credit: Decimal::ZERO, memo: None }
    }
    pub fn cr(account_id: i64, amount: Decimal) -> Self {
        Self { account_id, debit: Decimal::ZERO, credit: amount, memo: None }
    }
}

/// Validates and posts a balanced journal entry. Callers must run this inside a transaction.
pub async fn post_entry<C: ConnectionTrait>(
    c: &C,
    date: NaiveDate,
    memo: &str,
    source: &str,
    source_id: Option<i64>,
    reverses: Option<i64>,
    lines: Vec<NewLine>,
) -> Result<i64, AppError> {
    if lines.len() < 2 {
        return Err(AppError::BadRequest("a journal entry needs at least two lines".into()));
    }
    let mut dr = Decimal::ZERO;
    let mut cr = Decimal::ZERO;
    for l in &lines {
        require_scale(l.debit, "debit")?;
        require_scale(l.credit, "credit")?;
        let d_pos = l.debit > Decimal::ZERO;
        let c_pos = l.credit > Decimal::ZERO;
        if d_pos == c_pos || l.debit < Decimal::ZERO || l.credit < Decimal::ZERO {
            return Err(AppError::BadRequest(
                "each line must have exactly one positive amount, in debit or credit".into(),
            ));
        }
        dr += l.debit;
        cr += l.credit;
    }
    if dr != cr {
        return Err(AppError::BadRequest(format!(
            "entry is not balanced: debits {dr} != credits {cr}"
        )));
    }
    let ids: HashSet<i64> = lines.iter().map(|l| l.account_id).collect();
    let found = account::Entity::find()
        .filter(account::Column::Id.is_in(ids.iter().copied().collect::<Vec<_>>()))
        .filter(account::Column::IsActive.eq(true))
        .count(c)
        .await?;
    if found as usize != ids.len() {
        return Err(AppError::BadRequest("a line references an unknown or inactive account".into()));
    }

    let entry = journal_entry::ActiveModel {
        id: NotSet,
        entry_date: Set(date),
        memo: Set(memo.to_owned()),
        source: Set(source.to_owned()),
        source_id: Set(source_id),
        reverses_entry_id: Set(reverses),
        created_at: NotSet,
    }
    .insert(c)
    .await?;

    let models = lines.into_iter().map(|l| journal_line::ActiveModel {
        id: NotSet,
        entry_id: Set(entry.id),
        account_id: Set(l.account_id),
        debit: Set(l.debit),
        credit: Set(l.credit),
        memo: Set(l.memo),
    });
    journal_line::Entity::insert_many(models).exec(c).await?;
    Ok(entry.id)
}

/// Posts an entry that exactly mirrors `entry_id` with debits and credits swapped.
pub async fn reverse_entry<C: ConnectionTrait>(
    c: &C,
    entry_id: i64,
    date: NaiveDate,
    memo: &str,
) -> Result<i64, AppError> {
    let lines = journal_line::Entity::find()
        .filter(journal_line::Column::EntryId.eq(entry_id))
        .all(c)
        .await?;
    let swapped = lines
        .into_iter()
        .map(|l| NewLine { account_id: l.account_id, debit: l.credit, credit: l.debit, memo: l.memo })
        .collect();
    post_entry(c, date, memo, "reversal", Some(entry_id), Some(entry_id), swapped).await
}

// ───────────────────────── Accounts ─────────────────────────

#[derive(Deserialize)]
pub struct NewAccount {
    code: String,
    name: String,
    account_type: String,
    parent_id: Option<i64>,
}

pub async fn create_account(State(st): State<AppState>, Json(req): Json<NewAccount>) -> Created<account::Model> {
    if !ACCOUNT_TYPES.contains(&req.account_type.as_str()) {
        return Err(AppError::BadRequest(format!("account_type must be one of {ACCOUNT_TYPES:?}")));
    }
    let m = account::ActiveModel {
        id: NotSet,
        code: Set(non_empty(&req.code, "code")?),
        name: Set(non_empty(&req.name, "name")?),
        account_type: Set(req.account_type),
        parent_id: Set(req.parent_id),
        is_active: Set(true),
        created_at: NotSet,
    }
    .insert(&st.db)
    .await?;
    Ok((StatusCode::CREATED, Json(m)))
}

pub async fn list_accounts(State(st): State<AppState>) -> ApiResult<Vec<account::Model>> {
    Ok(Json(account::Entity::find().order_by_asc(account::Column::Code).all(&st.db).await?))
}

// ───────────────────────── Journal ─────────────────────────

#[derive(Deserialize)]
pub struct LineReq {
    account_id: i64,
    #[serde(default)]
    debit: Decimal,
    #[serde(default)]
    credit: Decimal,
    memo: Option<String>,
}

#[derive(Deserialize)]
pub struct NewEntry {
    entry_date: NaiveDate,
    memo: String,
    lines: Vec<LineReq>,
}

#[derive(Serialize)]
pub struct EntryView {
    #[serde(flatten)]
    entry: journal_entry::Model,
    lines: Vec<journal_line::Model>,
}

async fn entry_view<C: ConnectionTrait>(c: &C, id: i64) -> Result<EntryView, AppError> {
    let entry = journal_entry::Entity::find_by_id(id)
        .one(c)
        .await?
        .ok_or_else(|| AppError::not_found(format!("journal entry {id}")))?;
    let lines = journal_line::Entity::find()
        .filter(journal_line::Column::EntryId.eq(id))
        .order_by_asc(journal_line::Column::Id)
        .all(c)
        .await?;
    Ok(EntryView { entry, lines })
}

pub async fn create_entry(State(st): State<AppState>, Json(req): Json<NewEntry>) -> Created<EntryView> {
    let memo = non_empty(&req.memo, "memo")?;
    let ids: Vec<i64> = req.lines.iter().map(|l| l.account_id).collect();
    let lines = req
        .lines
        .into_iter()
        .map(|l| NewLine { account_id: l.account_id, debit: l.debit, credit: l.credit, memo: l.memo })
        .collect();
    let txn = st.db.begin().await?;
    super::cash::reject_bank_accounts(&txn, &ids).await?;
    super::cash::reject_control_accounts(&txn, &ids).await?;
    let id = post_entry(&txn, req.entry_date, &memo, "manual", None, None, lines).await?;
    let view = entry_view(&txn, id).await?;
    txn.commit().await?;
    Ok((StatusCode::CREATED, Json(view)))
}

#[derive(Deserialize)]
pub struct EntryQuery {
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    limit: Option<u64>,
}

pub async fn list_entries(
    State(st): State<AppState>,
    Query(q): Query<EntryQuery>,
) -> ApiResult<Vec<journal_entry::Model>> {
    use sea_orm::QueryTrait;
    let rows = journal_entry::Entity::find()
        .apply_if(q.from, |s, v| s.filter(journal_entry::Column::EntryDate.gte(v)))
        .apply_if(q.to, |s, v| s.filter(journal_entry::Column::EntryDate.lte(v)))
        .order_by_desc(journal_entry::Column::EntryDate)
        .order_by_desc(journal_entry::Column::Id)
        .limit(clamp_limit(q.limit))
        .all(&st.db)
        .await?;
    Ok(Json(rows))
}

pub async fn get_entry(State(st): State<AppState>, Path(id): Path<i64>) -> ApiResult<EntryView> {
    Ok(Json(entry_view(&st.db, id).await?))
}

#[derive(Deserialize, Default)]
pub struct ReverseReq {
    entry_date: Option<NaiveDate>,
}

pub async fn reverse_manual(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    body: Option<Json<ReverseReq>>,
) -> Created<EntryView> {
    let req = body.map(|b| b.0).unwrap_or_default();
    let txn = st.db.begin().await?;
    let original = journal_entry::Entity::find_by_id(id)
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::not_found(format!("journal entry {id}")))?;
    if original.source != "manual" {
        return Err(AppError::BadRequest(
            "only manual entries can be reversed directly; void the originating document instead".into(),
        ));
    }
    let rid = reverse_entry(
        &txn,
        id,
        req.entry_date.unwrap_or_else(today),
        &format!("Reversal of entry {id}: {}", original.memo),
    )
    .await?;
    let view = entry_view(&txn, rid).await?;
    txn.commit().await?;
    Ok((StatusCode::CREATED, Json(view)))
}

// ───────────────────────── Statements ─────────────────────────

#[derive(Debug, FromQueryResult)]
pub(crate) struct RawBalance {
    pub(crate) code: String,
    pub(crate) name: String,
    pub(crate) account_type: String,
    pub(crate) debit: Decimal,
    pub(crate) credit: Decimal,
}

pub(crate) async fn raw_balances<C: ConnectionTrait>(
    c: &C,
    from: Option<NaiveDate>,
    to: NaiveDate,
) -> Result<Vec<RawBalance>, AppError> {
    let sql = r#"
        SELECT a.id, a.code, a.name, a.account_type,
               COALESCE(SUM(x.debit), 0)  AS debit,
               COALESCE(SUM(x.credit), 0) AS credit
        FROM accounts a
        LEFT JOIN (
            SELECT l.account_id, l.debit, l.credit
            FROM journal_lines l
            JOIN journal_entries e ON e.id = l.entry_id
            WHERE e.entry_date <= $1 AND ($2::date IS NULL OR e.entry_date >= $2)
        ) x ON x.account_id = a.id
        GROUP BY a.id, a.code, a.name, a.account_type
        ORDER BY a.code"#;
    Ok(RawBalance::find_by_statement(stmt(sql, vec![to.into(), from.into()])).all(c).await?)
}

#[derive(Serialize)]
pub struct TbRow {
    code: String,
    name: String,
    account_type: String,
    debit: Decimal,
    credit: Decimal,
}

#[derive(Serialize)]
pub struct TrialBalance {
    as_of: NaiveDate,
    rows: Vec<TbRow>,
    total_debit: Decimal,
    total_credit: Decimal,
    balanced: bool,
}

#[derive(Deserialize)]
pub struct AsOf {
    pub as_of: Option<NaiveDate>,
}

/// Shows each account's net balance on its natural side.
pub async fn trial_balance(State(st): State<AppState>, Query(q): Query<AsOf>) -> ApiResult<TrialBalance> {
    let as_of = q.as_of.unwrap_or_else(today);
    let mut rows = vec![];
    let (mut td, mut tc) = (Decimal::ZERO, Decimal::ZERO);
    for r in raw_balances(&st.db, None, as_of).await? {
        let net = r.debit - r.credit;
        if net.is_zero() && r.debit.is_zero() {
            continue;
        }
        let (debit, credit) = if net >= Decimal::ZERO { (net, Decimal::ZERO) } else { (Decimal::ZERO, -net) };
        td += debit;
        tc += credit;
        rows.push(TbRow { code: r.code, name: r.name, account_type: r.account_type, debit, credit });
    }
    Ok(Json(TrialBalance { as_of, rows, total_debit: td, total_credit: tc, balanced: td == tc }))
}

#[derive(Serialize)]
pub struct StatementLine {
    code: String,
    name: String,
    amount: Decimal,
}

fn section(rows: &[RawBalance], kind: &str, credit_normal: bool) -> (Vec<StatementLine>, Decimal) {
    let mut total = Decimal::ZERO;
    let lines = rows
        .iter()
        .filter(|r| r.account_type == kind)
        .filter_map(|r| {
            let amount = if credit_normal { r.credit - r.debit } else { r.debit - r.credit };
            if amount.is_zero() {
                return None;
            }
            total += amount;
            Some(StatementLine { code: r.code.clone(), name: r.name.clone(), amount })
        })
        .collect();
    (lines, total)
}

#[derive(Deserialize)]
pub struct Range {
    from: NaiveDate,
    to: NaiveDate,
}

#[derive(Serialize)]
pub struct IncomeStatement {
    from: NaiveDate,
    to: NaiveDate,
    revenue: Vec<StatementLine>,
    total_revenue: Decimal,
    expenses: Vec<StatementLine>,
    total_expenses: Decimal,
    net_income: Decimal,
}

pub async fn income_statement(State(st): State<AppState>, Query(q): Query<Range>) -> ApiResult<IncomeStatement> {
    if q.to < q.from {
        return Err(AppError::BadRequest("`to` must not be before `from`".into()));
    }
    let rows = raw_balances(&st.db, Some(q.from), q.to).await?;
    let (revenue, total_revenue) = section(&rows, "revenue", true);
    let (expenses, total_expenses) = section(&rows, "expense", false);
    Ok(Json(IncomeStatement {
        from: q.from,
        to: q.to,
        revenue,
        total_revenue,
        expenses,
        total_expenses,
        net_income: total_revenue - total_expenses,
    }))
}

#[derive(Serialize)]
pub struct BalanceSheet {
    as_of: NaiveDate,
    assets: Vec<StatementLine>,
    total_assets: Decimal,
    liabilities: Vec<StatementLine>,
    total_liabilities: Decimal,
    equity: Vec<StatementLine>,
    /// Cumulative revenue minus expenses, which has not been closed to equity.
    current_earnings: Decimal,
    total_equity: Decimal,
    balanced: bool,
}

pub async fn balance_sheet(State(st): State<AppState>, Query(q): Query<AsOf>) -> ApiResult<BalanceSheet> {
    let as_of = q.as_of.unwrap_or_else(today);
    let rows = raw_balances(&st.db, None, as_of).await?;
    let (assets, total_assets) = section(&rows, "asset", false);
    let (liabilities, total_liabilities) = section(&rows, "liability", true);
    let (equity, equity_accounts) = section(&rows, "equity", true);
    let (_, rev) = section(&rows, "revenue", true);
    let (_, exp) = section(&rows, "expense", false);
    let current_earnings = rev - exp;
    let total_equity = equity_accounts + current_earnings;
    Ok(Json(BalanceSheet {
        as_of,
        assets,
        total_assets,
        liabilities,
        total_liabilities,
        equity,
        current_earnings,
        total_equity,
        balanced: total_assets == total_liabilities + total_equity,
    }))
}

// ───────────────────────── Aging (shared by AP and AR) ─────────────────────────

#[derive(Serialize)]
pub struct AgingItem {
    id: i64,
    party: String,
    document_number: String,
    due_date: NaiveDate,
    outstanding: Decimal,
    days_past_due: i64,
    bucket: &'static str,
}

#[derive(Serialize, Default)]
pub struct AgingReport {
    pub as_of: Option<NaiveDate>,
    current: Decimal,
    days_1_30: Decimal,
    days_31_60: Decimal,
    days_61_90: Decimal,
    over_90: Decimal,
    total: Decimal,
    items: Vec<AgingItem>,
}

/// `docs` is (id, party name, document number, due date, outstanding amount).
pub fn build_aging(docs: Vec<(i64, String, String, NaiveDate, Decimal)>, as_of: NaiveDate) -> AgingReport {
    let mut rep = AgingReport { as_of: Some(as_of), ..Default::default() };
    for (id, party, document_number, due_date, outstanding) in docs {
        let days = (as_of - due_date).num_days();
        let (bucket, slot): (&'static str, &mut Decimal) = match days {
            d if d <= 0 => ("current", &mut rep.current),
            1..=30 => ("1-30", &mut rep.days_1_30),
            31..=60 => ("31-60", &mut rep.days_31_60),
            61..=90 => ("61-90", &mut rep.days_61_90),
            _ => ("90+", &mut rep.over_90),
        };
        *slot += outstanding;
        rep.total += outstanding;
        rep.items.push(AgingItem { id, party, document_number, due_date, outstanding, days_past_due: days.max(0), bucket });
    }
    rep
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    #[test]
    fn aging_buckets() {
        let docs = vec![
            (1, "A".into(), "1".into(), d("2026-12-01"), Decimal::from(10)),
            (2, "A".into(), "2".into(), d("2026-10-20"), Decimal::from(20)),
            (3, "B".into(), "3".into(), d("2026-09-01"), Decimal::from(30)),
            (4, "B".into(), "4".into(), d("2026-06-01"), Decimal::from(40)),
        ];
        let r = build_aging(docs, d("2026-10-28"));
        assert_eq!(r.current, Decimal::from(10));
        assert_eq!(r.days_1_30, Decimal::from(20));
        assert_eq!(r.days_31_60, Decimal::from(30));
        assert_eq!(r.over_90, Decimal::from(40));
        assert_eq!(r.total, Decimal::from(100));
    }
}
