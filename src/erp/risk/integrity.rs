//! Ledger and subledger integrity checks.

use axum::{extract::State, Json};
use chrono::{Duration, NaiveDate};
use rust_decimal::Decimal;
use sea_orm::{ConnectionTrait, DatabaseConnection};
use serde::Serialize;

use crate::{
    error::{ApiResult, AppError},
    erp::finance::ledger::raw_balances,
    erp::util::{stmt, today},
    state::AppState,
};

#[derive(Serialize)]
pub struct Check {
    pub name: &'static str,
    pub status: &'static str,
    pub detail: String,
}

#[derive(Serialize)]
pub struct IntegrityReport {
    pub overall: &'static str,
    pub checks: Vec<Check>,
}

async fn scalar(db: &DatabaseConnection, sql: &str, params: Vec<sea_orm::Value>) -> Result<Decimal, AppError> {
    let row = db.query_one(stmt(sql, params)).await?.ok_or(AppError::internal())?;
    Ok(row.try_get_by_index::<Decimal>(0)?)
}

fn eq_check(name: &'static str, left_label: &str, left: Decimal, right_label: &str, right: Decimal, hint: &str) -> Check {
    if left == right {
        Check { name, status: "pass", detail: format!("{left_label} = {right_label} = {left}") }
    } else {
        Check {
            name,
            status: "fail",
            detail: format!("{left_label} {left} != {right_label} {right} (difference {}); {hint}", left - right),
        }
    }
}

pub async fn integrity_checks(State(st): State<AppState>) -> ApiResult<IntegrityReport> {
    let db = &st.db;
    let far = NaiveDate::from_ymd_opt(9999, 12, 31).ok_or(AppError::internal())?;
    let bal = raw_balances(db, None, far).await?;
    let mut checks = vec![];

    let (td, tc) = bal.iter().fold((Decimal::ZERO, Decimal::ZERO), |a, r| (a.0 + r.debit, a.1 + r.credit));
    checks.push(eq_check("trial_balance", "total debits", td, "total credits", tc, "the ledger is out of balance"));

    let gl = |code: &str, credit_normal: bool| -> Decimal {
        bal.iter()
            .filter(|r| r.code == code)
            .map(|r| if credit_normal { r.credit - r.debit } else { r.debit - r.credit })
            .sum()
    };
    let open_ar = scalar(db, "SELECT COALESCE(SUM(amount - amount_paid), 0) FROM invoices WHERE status = 'open'", vec![]).await?;
    checks.push(eq_check(
        "receivables_control",
        "GL 1100",
        gl("1100", false),
        "open invoices",
        open_ar,
        "something other than invoices and receipts has posted to the receivables account",
    ));
    let open_ap = scalar(db, "SELECT COALESCE(SUM(amount - amount_paid), 0) FROM bills WHERE status = 'open'", vec![]).await?;
    checks.push(eq_check(
        "payables_control",
        "GL 2000",
        gl("2000", true),
        "open bills",
        open_ap,
        "something other than bills and payments has posted to the payables account",
    ));
    let gl_cash = scalar(
        db,
        "SELECT COALESCE(SUM(l.debit - l.credit), 0) FROM journal_lines l \
         WHERE l.account_id IN (SELECT gl_account_id FROM bank_accounts)",
        vec![],
    )
    .await?;
    let bank_cash = scalar(db, "SELECT COALESCE(SUM(amount), 0) FROM cash_transactions", vec![]).await?;
    checks.push(eq_check("cash_vs_ledger", "bank GL accounts", gl_cash, "cash transactions", bank_cash, "a bank GL account moved without a cash transaction"));

    // Contra-assets (accumulated depreciation) legitimately carry credit balances.
    let contra: Vec<String> = db
        .query_all(stmt(
            "SELECT code FROM accounts WHERE id IN (SELECT accumulated_depreciation_account_id FROM fixed_assets)",
            vec![],
        ))
        .await?
        .iter()
        .map(|r| r.try_get_by_index::<String>(0))
        .collect::<Result<_, _>>()?;
    let abnormal: Vec<String> = bal
        .iter()
        .filter(|r| !contra.contains(&r.code))
        .filter_map(|r| {
            let n = r.debit - r.credit;
            let bad = match r.account_type.as_str() {
                "asset" | "expense" => n < Decimal::ZERO,
                "liability" | "revenue" => n > Decimal::ZERO,
                _ => false,
            };
            bad.then(|| format!("{} {} ({})", r.code, r.name, n))
        })
        .collect();
    checks.push(if abnormal.is_empty() {
        Check { name: "abnormal_balances", status: "pass", detail: "no account has a balance on its unnatural side".into() }
    } else {
        Check { name: "abnormal_balances", status: "warn", detail: format!("unnatural-side balances: {}", abnormal.join("; ")) }
    });

    let stale_cutoff = today() - Duration::days(90);
    let stale_ar = scalar(db, "SELECT COUNT(*)::numeric FROM invoices WHERE status = 'open' AND due_date < $1", vec![stale_cutoff.into()]).await?;
    checks.push(if stale_ar.is_zero() {
        Check { name: "stale_receivables", status: "pass", detail: "no open invoice is more than 90 days past due".into() }
    } else {
        Check { name: "stale_receivables", status: "warn", detail: format!("{stale_ar} open invoice(s) are more than 90 days past due") }
    });
    let unrec_cutoff = today() - Duration::days(30);
    let unrec = scalar(db, "SELECT COUNT(*)::numeric FROM cash_transactions WHERE NOT reconciled AND txn_date < $1", vec![unrec_cutoff.into()]).await?;
    checks.push(if unrec.is_zero() {
        Check { name: "unreconciled_cash", status: "pass", detail: "no cash transaction older than 30 days is unreconciled".into() }
    } else {
        Check { name: "unreconciled_cash", status: "warn", detail: format!("{unrec} cash transaction(s) older than 30 days are unreconciled") }
    });

    // HR controls
    let bad_runs = scalar(
        db,
        "SELECT COUNT(*)::numeric FROM payroll_runs r WHERE r.status = 'posted' AND \
         (SELECT COALESCE(SUM(gross_pay), 0) FROM payslips WHERE run_id = r.id) <> \
         (SELECT COALESCE(SUM(l.debit), 0) FROM journal_lines l JOIN accounts a ON a.id = l.account_id \
          WHERE l.entry_id = r.journal_entry_id AND a.code = '5000')",
        vec![],
    )
    .await?;
    checks.push(if bad_runs.is_zero() {
        Check { name: "payroll_to_ledger", status: "pass", detail: "every posted payroll run's gross pay equals its salary expense in the ledger".into() }
    } else {
        Check { name: "payroll_to_ledger", status: "fail", detail: format!("{bad_runs} posted payroll run(s) have payslip gross pay that differs from the ledger") }
    });
    let open_benefits = scalar(
        db,
        "SELECT COUNT(*)::numeric FROM employee_benefits b JOIN employees e ON e.id = b.employee_id \
         WHERE e.status = 'terminated' AND b.end_date IS NULL",
        vec![],
    )
    .await?;
    checks.push(if open_benefits.is_zero() {
        Check { name: "terminated_benefits", status: "pass", detail: "no terminated employee has an open benefit enrolment".into() }
    } else {
        Check { name: "terminated_benefits", status: "fail", detail: format!("{open_benefits} benefit enrolment(s) are still open for terminated employees") }
    });
    let (complete, total) = crate::erp::hr::compensation::attribute_completeness(db).await?;
    checks.push(if complete == 0 {
        Check { name: "pay_benchmark_data", status: "pass", detail: format!("pay benchmark not in use (0 of {total} active employees carry the attributes)") }
    } else if complete * 100 >= total * 80 {
        Check { name: "pay_benchmark_data", status: "pass", detail: format!("{complete} of {total} active employees have every pay-benchmark attribute") }
    } else {
        Check { name: "pay_benchmark_data", status: "warn", detail: format!("only {complete} of {total} active employees have every pay-benchmark attribute (education_level, years_experience, city_tier, remote_work)") }
    });

    let overall = if checks.iter().any(|c| c.status == "fail") {
        "fail"
    } else if checks.iter().any(|c| c.status == "warn") {
        "warn"
    } else {
        "pass"
    };
    Ok(Json(IntegrityReport { overall, checks }))
}
