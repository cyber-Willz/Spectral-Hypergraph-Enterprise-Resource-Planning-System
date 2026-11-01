//! Financial health: ratios computed from the ledger and a benchmarked risk score.

use axum::{
    extract::{Query, State},
    Json,
};
use chrono::{Months, NaiveDate};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use serde::{Deserialize, Serialize};

use super::model::{health_score, HealthInputs};
use crate::{
    error::ApiResult,
    erp::finance::ledger::{raw_balances, RawBalance},
    erp::util::today,
    state::AppState,
};

#[derive(Deserialize)]
pub struct HealthQuery {
    as_of: Option<NaiveDate>,
    /// Trailing window for income and burn (default 3, max 24 months).
    months: Option<u32>,
}

#[derive(Serialize)]
pub struct Metrics {
    total_assets: Decimal,
    total_liabilities: Decimal,
    cash: Decimal,
    revenue: Decimal,
    expenses: Decimal,
    net_income: Decimal,
    debt_ratio_pct: Option<f64>,
    /// (cash + receivables) / (payables + payroll liabilities).
    current_ratio: Option<f64>,
    profit_margin_pct: Option<f64>,
    /// Average monthly net loss over the window (0 when profitable).
    monthly_burn: f64,
    /// Cash / monthly burn, capped at 48 months (0 if cash is negative).
    runway_months: f64,
}

#[derive(Serialize)]
pub struct Score {
    score: f64,
    band: &'static str,
    inputs_clamped: Vec<&'static str>,
}

#[derive(Serialize)]
pub struct Flag {
    code: &'static str,
    message: String,
}

#[derive(Serialize)]
pub struct HealthReport {
    as_of: NaiveDate,
    window_start: NaiveDate,
    window_months: u32,
    status: &'static str,
    missing: Vec<&'static str>,
    metrics: Metrics,
    score: Option<Score>,
    /// Rule-of-thumb warnings (conventional thresholds, not fitted to data).
    flags: Vec<Flag>,
    notes: Vec<&'static str>,
}

fn net(rows: &[RawBalance], kind: &str, credit_normal: bool) -> Decimal {
    rows.iter()
        .filter(|r| r.account_type == kind)
        .map(|r| if credit_normal { r.credit - r.debit } else { r.debit - r.credit })
        .sum()
}

fn code_net(rows: &[RawBalance], code: &str, credit_normal: bool) -> Decimal {
    rows.iter()
        .filter(|r| r.code == code)
        .map(|r| if credit_normal { r.credit - r.debit } else { r.debit - r.credit })
        .sum()
}

pub async fn financial_health(State(st): State<AppState>, Query(q): Query<HealthQuery>) -> ApiResult<HealthReport> {
    let as_of = q.as_of.unwrap_or_else(today);
    let months = q.months.unwrap_or(3).clamp(1, 24);
    let start = as_of
        .checked_sub_months(Months::new(months))
        .and_then(|d| d.succ_opt())
        .unwrap_or(as_of);

    let bal = raw_balances(&st.db, None, as_of).await?;
    let flow = raw_balances(&st.db, Some(start), as_of).await?;

    let assets = net(&bal, "asset", false);
    let liabilities = net(&bal, "liability", true);
    let cash = code_net(&bal, "1000", false);
    let receivables = code_net(&bal, "1100", false);
    let short_term = code_net(&bal, "2000", true) + code_net(&bal, "2100", true);
    let revenue = net(&flow, "revenue", true);
    let expenses = net(&flow, "expense", false);
    let net_income = revenue - expenses;

    let f = |d: Decimal| d.to_f64().unwrap_or(0.0);
    let mut missing = vec![];
    let mut notes = vec![];

    let debt_ratio_pct = (assets > Decimal::ZERO).then(|| f(liabilities) / f(assets) * 100.0);
    if debt_ratio_pct.is_none() {
        missing.push("total assets are not positive");
    }
    let profit_margin_pct = (revenue > Decimal::ZERO).then(|| f(net_income) / f(revenue) * 100.0);
    if profit_margin_pct.is_none() {
        missing.push("no revenue in the window");
    }
    let current_ratio = (short_term > Decimal::ZERO).then(|| f(cash + receivables) / f(short_term));
    if current_ratio.is_none() {
        notes.push("no short-term obligations: current ratio treated as 4.0 for scoring");
    }
    let months_f = months as f64;
    let monthly_burn = ((f(expenses) - f(revenue)) / months_f).max(0.0);
    let runway_months = if cash <= Decimal::ZERO {
        0.0
    } else if monthly_burn == 0.0 {
        48.0
    } else {
        (f(cash) / monthly_burn).min(48.0)
    };
    notes.push("the ledger has no current/non-current split; cash, receivables, payables and payroll liabilities are used");
    notes.push("score is relative to a benchmark population (see docs/DATASET_EVALUATION.md); it is not a default probability");

    let score = match (debt_ratio_pct, profit_margin_pct) {
        (Some(d), Some(m)) => {
            let s = health_score(HealthInputs {
                debt_ratio_pct: d,
                current_ratio: current_ratio.unwrap_or(4.0),
                profit_margin_pct: m,
                runway_months,
            });
            Some(Score { score: (s.score * 10.0).round() / 10.0, band: s.band, inputs_clamped: s.clamped })
        }
        _ => None,
    };

    let mut flags = vec![];
    if let Some(cr) = current_ratio {
        if cr < 1.0 {
            flags.push(Flag { code: "current_ratio_below_1", message: format!("current ratio {cr:.2} < 1.0") });
        }
    }
    if runway_months < 6.0 && monthly_burn > 0.0 {
        flags.push(Flag { code: "runway_under_6_months", message: format!("runway {runway_months:.1} months at current burn") });
    }
    if let Some(m) = profit_margin_pct {
        if m < 0.0 {
            flags.push(Flag { code: "net_loss", message: format!("net margin {m:.1}% over the window") });
        }
    }
    if let Some(d) = debt_ratio_pct {
        if d > 60.0 {
            flags.push(Flag { code: "high_leverage", message: format!("liabilities are {d:.0}% of assets") });
        }
    }

    Ok(Json(HealthReport {
        as_of,
        window_start: start,
        window_months: months,
        status: if score.is_some() { "ok" } else { "insufficient_data" },
        missing,
        metrics: Metrics {
            total_assets: assets,
            total_liabilities: liabilities,
            cash,
            revenue,
            expenses,
            net_income,
            debt_ratio_pct: debt_ratio_pct.map(|v| (v * 10.0).round() / 10.0),
            current_ratio: current_ratio.map(|v| (v * 100.0).round() / 100.0),
            profit_margin_pct: profit_margin_pct.map(|v| (v * 10.0).round() / 10.0),
            monthly_burn: (monthly_burn * 100.0).round() / 100.0,
            runway_months: (runway_months * 10.0).round() / 10.0,
        },
        score,
        flags,
        notes,
    }))
}
