//! Budgeting: annual budgets by account and month, with budget-vs-actual variance.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use rust_decimal::Decimal;
use sea_orm::{
    prelude::*, ActiveValue::{NotSet, Set}, FromQueryResult, QueryOrder, TransactionTrait,
};
use serde::{Deserialize, Serialize};

use crate::{
    erp::entities::{budget, budget_line},
    error::{ApiResult, AppError, Created},
    erp::util::{non_empty, require_non_negative, stmt},
    state::AppState,
};

#[derive(Deserialize)]
pub struct BudgetLineReq {
    account_id: i64,
    month: i16,
    amount: Decimal,
}

#[derive(Deserialize)]
pub struct NewBudget {
    name: String,
    fiscal_year: i32,
    lines: Vec<BudgetLineReq>,
}

#[derive(Serialize)]
pub struct BudgetView {
    #[serde(flatten)]
    budget: budget::Model,
    lines: Vec<budget_line::Model>,
}

pub async fn create_budget(State(st): State<AppState>, Json(req): Json<NewBudget>) -> Created<BudgetView> {
    if req.lines.is_empty() {
        return Err(AppError::BadRequest("a budget needs at least one line".into()));
    }
    for l in &req.lines {
        if !(1..=12).contains(&l.month) {
            return Err(AppError::BadRequest("month must be between 1 and 12".into()));
        }
        require_non_negative(l.amount, "amount")?;
    }
    let txn = st.db.begin().await?;
    let b = budget::ActiveModel {
        id: NotSet,
        name: Set(non_empty(&req.name, "name")?),
        fiscal_year: Set(req.fiscal_year),
        created_at: NotSet,
    }
    .insert(&txn)
    .await?;
    let models = req.lines.into_iter().map(|l| budget_line::ActiveModel {
        id: NotSet,
        budget_id: Set(b.id),
        account_id: Set(l.account_id),
        month: Set(l.month),
        amount: Set(l.amount),
    });
    budget_line::Entity::insert_many(models).exec(&txn).await?;
    let lines = budget_line::Entity::find()
        .filter(budget_line::Column::BudgetId.eq(b.id))
        .order_by_asc(budget_line::Column::AccountId)
        .order_by_asc(budget_line::Column::Month)
        .all(&txn)
        .await?;
    txn.commit().await?;
    Ok((StatusCode::CREATED, Json(BudgetView { budget: b, lines })))
}

pub async fn list_budgets(State(st): State<AppState>) -> ApiResult<Vec<budget::Model>> {
    Ok(Json(budget::Entity::find().order_by_desc(budget::Column::FiscalYear).all(&st.db).await?))
}

pub async fn get_budget(State(st): State<AppState>, Path(id): Path<i64>) -> ApiResult<BudgetView> {
    let b = budget::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found(format!("budget {id}")))?;
    let lines = budget_line::Entity::find()
        .filter(budget_line::Column::BudgetId.eq(id))
        .order_by_asc(budget_line::Column::AccountId)
        .order_by_asc(budget_line::Column::Month)
        .all(&st.db)
        .await?;
    Ok(Json(BudgetView { budget: b, lines }))
}

#[derive(Deserialize)]
pub struct VarianceQuery {
    /// Include months 1..=through_month (default 12).
    through_month: Option<i32>,
}

#[derive(FromQueryResult)]
struct RawVariance {
    code: String,
    name: String,
    account_type: String,
    budget: Decimal,
    actual: Decimal,
}

#[derive(Serialize)]
pub struct VarianceRow {
    code: String,
    name: String,
    account_type: String,
    budget: Decimal,
    actual: Decimal,
    /// actual - budget. For expenses positive means overspent; for revenue positive means ahead of plan.
    variance: Decimal,
    percent_of_budget: Option<Decimal>,
}

#[derive(Serialize)]
pub struct Variance {
    budget_id: i64,
    fiscal_year: i32,
    through_month: i32,
    rows: Vec<VarianceRow>,
}

pub async fn variance(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<VarianceQuery>,
) -> ApiResult<Variance> {
    let through = q.through_month.unwrap_or(12);
    if !(1..=12).contains(&through) {
        return Err(AppError::BadRequest("through_month must be between 1 and 12".into()));
    }
    let b = budget::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found(format!("budget {id}")))?;
    let sql = r#"
        WITH bud AS (
            SELECT account_id, SUM(amount) AS amount
            FROM budget_lines WHERE budget_id = $1 AND month <= $2::int GROUP BY account_id
        ), act AS (
            SELECT l.account_id, SUM(l.debit) AS d, SUM(l.credit) AS c
            FROM journal_lines l JOIN journal_entries e ON e.id = l.entry_id
            WHERE e.entry_date >= make_date($3::int, 1, 1)
              AND e.entry_date <  make_date($3::int, 1, 1) + make_interval(months => $2::int)
            GROUP BY l.account_id
        )
        SELECT a.code, a.name, a.account_type,
               bud.amount AS budget,
               CASE WHEN a.account_type IN ('revenue','liability','equity')
                    THEN COALESCE(act.c,0) - COALESCE(act.d,0)
                    ELSE COALESCE(act.d,0) - COALESCE(act.c,0) END AS actual
        FROM bud
        JOIN accounts a ON a.id = bud.account_id
        LEFT JOIN act ON act.account_id = bud.account_id
        ORDER BY a.code"#;
    let raw = RawVariance::find_by_statement(stmt(sql, vec![id.into(), through.into(), b.fiscal_year.into()]))
        .all(&st.db)
        .await?;
    let rows = raw
        .into_iter()
        .map(|r| VarianceRow {
            percent_of_budget: (!r.budget.is_zero()).then(|| (r.actual / r.budget * Decimal::from(100)).round_dp(1)),
            variance: r.actual - r.budget,
            code: r.code,
            name: r.name,
            account_type: r.account_type,
            budget: r.budget,
            actual: r.actual,
        })
        .collect();
    Ok(Json(Variance { budget_id: id, fiscal_year: b.fiscal_year, through_month: through, rows }))
}
