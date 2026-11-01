//! Payroll: runs, payslips, approval and posting to the general ledger.

use std::collections::HashMap;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sea_orm::{
    prelude::*, ActiveValue::{NotSet, Set}, Condition, ConnectionTrait, QueryOrder, QuerySelect, TransactionTrait,
};
use serde::{Deserialize, Serialize};

use crate::{
    erp::entities::{benefit_plan, employee, employee_benefit, payroll_run, payslip},
    error::{ApiResult, AppError, Created},
    erp::finance::{
        cash::{active_bank, record_cash},
        ledger::{account_by_code, post_entry, NewLine, BENEFITS_EXPENSE, PAYROLL_LIABILITIES, SALARY_EXPENSE},
    },
    state::AppState,
};

#[derive(Deserialize)]
pub struct NewRun {
    period_start: NaiveDate,
    period_end: NaiveDate,
    pay_date: NaiveDate,
    /// Pay periods per year used to derive gross from annual salary (default 12).
    periods_per_year: Option<u32>,
}

/// Gross pay for one employee: annual salary / periods, prorated by days employed in the period.
pub fn gross_for(
    salary: Decimal,
    periods_per_year: u32,
    hire: NaiveDate,
    term: Option<NaiveDate>,
    start: NaiveDate,
    end: NaiveDate,
) -> Decimal {
    let from = hire.max(start);
    let to = term.map_or(end, |t| t.min(end));
    let active_days = (to - from).num_days() + 1;
    if active_days <= 0 {
        return Decimal::ZERO;
    }
    let period_days = (end - start).num_days() + 1;
    (salary / Decimal::from(periods_per_year) * Decimal::from(active_days) / Decimal::from(period_days)).round_dp(2)
}

#[derive(Serialize)]
pub struct RunView {
    #[serde(flatten)]
    run: payroll_run::Model,
    payslips: Vec<payslip::Model>,
    total_gross: Decimal,
    total_tax: Decimal,
    total_net: Decimal,
    total_employer_cost: Decimal,
}

fn view(run: payroll_run::Model, payslips: Vec<payslip::Model>) -> RunView {
    let sum = |f: fn(&payslip::Model) -> Decimal| payslips.iter().map(f).sum::<Decimal>();
    RunView {
        total_gross: sum(|p| p.gross_pay),
        total_tax: sum(|p| p.tax_withheld),
        total_net: sum(|p| p.net_pay),
        total_employer_cost: sum(|p| p.gross_pay + p.employer_benefit_cost),
        run,
        payslips,
    }
}

async fn load_view<C: ConnectionTrait>(c: &C, id: i64) -> Result<RunView, AppError> {
    let run = payroll_run::Entity::find_by_id(id)
        .one(c)
        .await?
        .ok_or_else(|| AppError::not_found(format!("payroll run {id}")))?;
    let slips = payslip::Entity::find()
        .filter(payslip::Column::RunId.eq(id))
        .order_by_asc(payslip::Column::EmployeeId)
        .all(c)
        .await?;
    Ok(view(run, slips))
}

pub async fn create_run(State(st): State<AppState>, Json(req): Json<NewRun>) -> Created<RunView> {
    let ppy = req.periods_per_year.unwrap_or(12);
    if !(1..=366).contains(&ppy) {
        return Err(AppError::BadRequest("periods_per_year must be between 1 and 366".into()));
    }
    if req.period_end < req.period_start {
        return Err(AppError::BadRequest("period_end must not be before period_start".into()));
    }
    if req.pay_date < req.period_end {
        return Err(AppError::BadRequest("pay_date must not be before period_end".into()));
    }
    let txn = st.db.begin().await?;
    // Serialise run creation so overlapping periods cannot slip through concurrently.
    txn.execute_unprepared("SELECT pg_advisory_xact_lock(7001)").await?;
    let overlap = payroll_run::Entity::find()
        .filter(payroll_run::Column::PeriodStart.lte(req.period_end))
        .filter(payroll_run::Column::PeriodEnd.gte(req.period_start))
        .count(&txn)
        .await?;
    if overlap > 0 {
        return Err(AppError::Conflict("a payroll run already covers part of this period".into()));
    }

    let employees = employee::Entity::find()
        .filter(employee::Column::HireDate.lte(req.period_end))
        .filter(
            Condition::any()
                .add(employee::Column::TerminationDate.is_null())
                .add(employee::Column::TerminationDate.gte(req.period_start)),
        )
        .order_by_asc(employee::Column::Id)
        .all(&txn)
        .await?;

    let plans: HashMap<i64, benefit_plan::Model> =
        benefit_plan::Entity::find().all(&txn).await?.into_iter().map(|p| (p.id, p)).collect();
    let mut costs: HashMap<i64, (Decimal, Decimal)> = HashMap::new();
    let enrolments = employee_benefit::Entity::find()
        .filter(employee_benefit::Column::EffectiveDate.lte(req.period_end))
        .filter(
            Condition::any()
                .add(employee_benefit::Column::EndDate.is_null())
                .add(employee_benefit::Column::EndDate.gte(req.period_start)),
        )
        .all(&txn)
        .await?;
    for en in enrolments {
        if let Some(p) = plans.get(&en.plan_id) {
            let e = costs.entry(en.employee_id).or_default();
            e.0 += p.employee_cost;
            e.1 += p.employer_cost;
        }
    }

    let run = payroll_run::ActiveModel {
        id: NotSet,
        period_start: Set(req.period_start),
        period_end: Set(req.period_end),
        pay_date: Set(req.pay_date),
        status: Set("draft".into()),
        journal_entry_id: Set(None),
        created_at: NotSet,
    }
    .insert(&txn)
    .await?;

    let mut slips = vec![];
    for e in &employees {
        let gross = gross_for(e.base_salary, ppy, e.hire_date, e.termination_date, req.period_start, req.period_end);
        if gross.is_zero() {
            continue;
        }
        let tax = (gross * st.payroll_tax_rate).round_dp(2);
        let (ded, employer) = costs.get(&e.id).copied().unwrap_or_default();
        let net = gross - tax - ded;
        if net < Decimal::ZERO {
            return Err(AppError::BadRequest(format!(
                "deductions for employee {} exceed gross pay",
                e.employee_number
            )));
        }
        slips.push(payslip::ActiveModel {
            id: NotSet,
            run_id: Set(run.id),
            employee_id: Set(e.id),
            gross_pay: Set(gross),
            tax_withheld: Set(tax),
            employee_benefit_deductions: Set(ded),
            employer_benefit_cost: Set(employer),
            net_pay: Set(net),
        });
    }
    if slips.is_empty() {
        return Err(AppError::BadRequest("no employees to pay in this period".into()));
    }
    payslip::Entity::insert_many(slips).exec(&txn).await?;
    let v = load_view(&txn, run.id).await?;
    txn.commit().await?;
    Ok((StatusCode::CREATED, Json(v)))
}

pub async fn list_runs(State(st): State<AppState>) -> ApiResult<Vec<payroll_run::Model>> {
    Ok(Json(payroll_run::Entity::find().order_by_desc(payroll_run::Column::PeriodEnd).all(&st.db).await?))
}

pub async fn get_run(State(st): State<AppState>, Path(id): Path<i64>) -> ApiResult<RunView> {
    Ok(Json(load_view(&st.db, id).await?))
}

async fn locked_run<C: ConnectionTrait>(c: &C, id: i64) -> Result<payroll_run::Model, AppError> {
    payroll_run::Entity::find_by_id(id)
        .lock_exclusive()
        .one(c)
        .await?
        .ok_or_else(|| AppError::not_found(format!("payroll run {id}")))
}

pub async fn approve(State(st): State<AppState>, Path(id): Path<i64>) -> ApiResult<RunView> {
    let txn = st.db.begin().await?;
    let run = locked_run(&txn, id).await?;
    if run.status != "draft" {
        return Err(AppError::Conflict(format!("run is {}", run.status)));
    }
    let mut am: payroll_run::ActiveModel = run.into();
    am.status = Set("approved".into());
    am.update(&txn).await?;
    let v = load_view(&txn, id).await?;
    txn.commit().await?;
    Ok(Json(v))
}

pub async fn delete_draft(State(st): State<AppState>, Path(id): Path<i64>) -> Result<StatusCode, AppError> {
    let txn = st.db.begin().await?;
    let run = locked_run(&txn, id).await?;
    if run.status != "draft" {
        return Err(AppError::Conflict("only draft runs can be deleted".into()));
    }
    payslip::Entity::delete_many().filter(payslip::Column::RunId.eq(id)).exec(&txn).await?;
    payroll_run::Entity::delete_by_id(id).exec(&txn).await?;
    txn.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct PostReq {
    /// Bank account the net pay is disbursed from.
    bank_account_id: i64,
}

/// Posts an approved run to the ledger:
/// Dr Salaries, Dr Benefits (employer share); Cr Bank (net pay), Cr Payroll Liabilities
/// (tax withheld + employee deductions + employer benefit cost, remitted later).
pub async fn post_run(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<PostReq>,
) -> ApiResult<RunView> {
    let txn = st.db.begin().await?;
    let run = locked_run(&txn, id).await?;
    if run.status != "approved" {
        return Err(AppError::Conflict(format!("run must be approved before posting (it is {})", run.status)));
    }
    let slips = payslip::Entity::find().filter(payslip::Column::RunId.eq(id)).all(&txn).await?;
    let gross: Decimal = slips.iter().map(|s| s.gross_pay).sum();
    let employer: Decimal = slips.iter().map(|s| s.employer_benefit_cost).sum();
    let net: Decimal = slips.iter().map(|s| s.net_pay).sum();
    let liabilities: Decimal =
        slips.iter().map(|s| s.tax_withheld + s.employee_benefit_deductions + s.employer_benefit_cost).sum();

    let bank = active_bank(&txn, req.bank_account_id).await?;
    let salary = account_by_code(&txn, SALARY_EXPENSE).await?;
    let benefits = account_by_code(&txn, BENEFITS_EXPENSE).await?;
    let liab = account_by_code(&txn, PAYROLL_LIABILITIES).await?;

    let mut lines = vec![NewLine::dr(salary.id, gross)];
    if employer > Decimal::ZERO {
        lines.push(NewLine::dr(benefits.id, employer));
    }
    if net > Decimal::ZERO {
        lines.push(NewLine::cr(bank.gl_account_id, net));
    }
    if liabilities > Decimal::ZERO {
        lines.push(NewLine::cr(liab.id, liabilities));
    }
    let desc = format!("Payroll {} to {}", run.period_start, run.period_end);
    let je = post_entry(&txn, run.pay_date, &desc, "payroll", Some(run.id), None, lines).await?;
    if net > Decimal::ZERO {
        record_cash(&txn, bank.id, run.pay_date, -net, &desc, "payroll", Some(run.id), je).await?;
    }
    let mut am: payroll_run::ActiveModel = run.into();
    am.status = Set("posted".into());
    am.journal_entry_id = Set(Some(je));
    am.update(&txn).await?;
    let v = load_view(&txn, id).await?;
    txn.commit().await?;
    Ok(Json(v))
}

pub async fn payslips_for_employee(
    State(st): State<AppState>,
    Path(employee_id): Path<i64>,
) -> ApiResult<Vec<payslip::Model>> {
    let rows = payslip::Entity::find()
        .filter(payslip::Column::EmployeeId.eq(employee_id))
        .order_by_desc(payslip::Column::RunId)
        .all(&st.db)
        .await?;
    Ok(Json(rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    #[test]
    fn full_month_is_one_twelfth() {
        let g = gross_for(Decimal::from(120_000), 12, d("2020-01-01"), None, d("2026-10-01"), d("2026-10-31"));
        assert_eq!(g, Decimal::from(10_000));
    }

    #[test]
    fn prorates_mid_month_hire_and_termination() {
        // hired on 16th of a 30-day month: 15 of 30 days
        let g = gross_for(Decimal::from(120_000), 12, d("2026-09-16"), None, d("2026-09-01"), d("2026-09-30"));
        assert_eq!(g, Decimal::from(5_000));
        // terminated on the 10th of a 30-day month: 10 of 30 days
        let g = gross_for(Decimal::from(36_000), 12, d("2020-01-01"), Some(d("2026-09-10")), d("2026-09-01"), d("2026-09-30"));
        assert_eq!(g, Decimal::from(1_000));
    }

    #[test]
    fn not_employed_in_period_is_zero() {
        let g = gross_for(Decimal::from(120_000), 12, d("2026-11-01"), None, d("2026-10-01"), d("2026-10-31"));
        assert_eq!(g, Decimal::ZERO);
    }
}
