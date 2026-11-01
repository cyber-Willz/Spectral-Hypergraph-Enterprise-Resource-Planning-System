//! Compensation benchmark: expected pay from the company's own employees, and outliers.
//!
//! Expected pay is the fitted value of log(base_salary) on experience, education, location tier,
//! remote status and job title. Age and gender are deliberately not inputs. Performance is also
//! excluded so that pay-versus-performance can be examined rather than assumed.

use std::collections::HashMap;

use axum::{
    extract::{Query, State},
    Json,
};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use sea_orm::{prelude::*, ConnectionTrait, FromQueryResult};
use serde::{Deserialize, Serialize};

use super::pay_model::{build_design, effect_pct, fit_ols, median, pearson, PayRow};
use crate::{
    erp::entities::employee,
    error::{ApiResult, AppError},
    erp::util::{clamp_limit, stmt, today},
    state::AppState,
};

const MIN_EMPLOYEES: usize = 50;
const MIN_TITLE_COUNT: usize = 5;
/// An employee is flagged when pay deviates by at least 2 residual SDs and at least 10%.
const FLAG_Z: f64 = 2.0;
const FLAG_MIN_PCT: f64 = 0.10;
/// Points further than this from the first-pass fit are left out of the refit.
const TRIM_Z: f64 = 3.0;

#[derive(Deserialize)]
pub struct BenchQuery {
    limit: Option<u64>,
}

#[derive(Serialize)]
pub struct Term {
    term: String,
    /// Percent change in pay per unit (experience year, tier step) or versus the baseline group.
    effect_pct: f64,
}

#[derive(Serialize)]
pub struct ModelInfo {
    r2: f64,
    adjusted_r2: f64,
    /// Typical size of the unexplained pay difference (one residual SD), in percent.
    typical_error_pct: f64,
    terms: Vec<Term>,
}

#[derive(Serialize)]
pub struct Row {
    employee_id: i64,
    employee_number: String,
    job_title: String,
    base_salary: Decimal,
    expected_salary: f64,
    /// base_salary / expected_salary.
    compa_ratio: f64,
    z_score: f64,
    flag: Option<&'static str>,
}

#[derive(Serialize)]
pub struct Summary {
    median_compa_ratio: Option<f64>,
    flagged_above_expected: usize,
    flagged_below_expected: usize,
}

#[derive(Serialize)]
pub struct PayPerformance {
    employees_with_review: usize,
    /// Correlation between unexplained pay (log residual) and the latest review rating.
    correlation: Option<f64>,
}

#[derive(Serialize)]
pub struct Benchmark {
    status: &'static str,
    message: Option<String>,
    active_employees: usize,
    modelled: usize,
    excluded_missing_attributes: usize,
    /// Employees left out of the fit (not out of the results) as extreme outliers.
    trimmed_from_fit: usize,
    model: Option<ModelInfo>,
    summary: Option<Summary>,
    pay_performance: Option<PayPerformance>,
    /// Most unusual first (flagged employees, then by |z|).
    employees: Vec<Row>,
    notes: Vec<&'static str>,
}

#[derive(FromQueryResult)]
struct LatestRating {
    employee_id: i64,
    rating: i16,
}

pub async fn benchmark(State(st): State<AppState>, Query(q): Query<BenchQuery>) -> ApiResult<Benchmark> {
    let active = employee::Entity::find()
        .filter(employee::Column::Status.eq("active"))
        .all(&st.db)
        .await?;
    let ratings: HashMap<i64, i16> = LatestRating::find_by_statement(stmt(
        "SELECT DISTINCT ON (employee_id) employee_id, rating FROM performance_reviews \
         ORDER BY employee_id, period_end DESC, id DESC",
        vec![],
    ))
    .all(&st.db)
    .await?
    .into_iter()
    .map(|r| (r.employee_id, r.rating))
    .collect();

    let now = today();
    let mut used = vec![];
    let mut pay = vec![];
    for e in &active {
        let (Some(edu), Some(exp), Some(tier), Some(remote)) =
            (e.education_level.clone(), e.years_experience, e.city_tier, e.remote_work)
        else {
            continue;
        };
        let Some(salary) = e.base_salary.to_f64().filter(|s| *s > 0.0) else { continue };
        // Experience was recorded as of the hire date; add whole years served since.
        let served = ((now - e.hire_date).num_days() as f64 / 365.25).floor().max(0.0);
        pay.push(PayRow {
            salary,
            experience_years: exp as f64 + served,
            education: edu,
            city_tier: tier as f64,
            remote,
            job_title: e.job_title.clone(),
        });
        used.push(e);
    }

    let notes = vec![
        "expected pay is fitted on your own employees, so it reflects your pay practice, not the market",
        "age and gender are not inputs; performance is excluded so pay-vs-performance can be examined",
        "flags are statistical outliers (|z| >= 2 and >= 10% off), a prompt to review, not evidence of unfair pay",
        "the model is refitted without extreme outliers (|z| > 3) so they cannot mask each other",
    ];
    let mut out = Benchmark {
        status: "ok",
        message: None,
        active_employees: active.len(),
        modelled: used.len(),
        excluded_missing_attributes: active.len() - used.len(),
        trimmed_from_fit: 0,
        model: None,
        summary: None,
        pay_performance: None,
        employees: vec![],
        notes,
    };

    let design = build_design(&pay, MIN_TITLE_COUNT);
    let k = design.columns.len();
    let needed = MIN_EMPLOYEES.max(5 * (k + 1));
    if pay.len() < needed {
        out.status = "insufficient_data";
        out.message = Some(format!(
            "{} employees have complete attributes (education_level, years_experience, city_tier, remote_work); \
             at least {needed} are needed{}",
            pay.len(),
            if k > 0 { format!(" for the {k}-term model") } else { String::new() }
        ));
        return Ok(Json(out));
    }
    let y: Vec<f64> = pay.iter().map(|p| p.salary.ln()).collect();
    let first = match fit_ols(&design.rows, &y) {
        Ok(f) => f,
        Err(e) => {
            out.status = "insufficient_data";
            out.message = Some(format!("model could not be fitted: {e}"));
            return Ok(Json(out));
        }
    };
    // Robust second pass: extreme pay inflates the residual SD and hides other outliers, so refit
    // without points beyond TRIM_Z SDs (when enough data remains) and judge everyone against that.
    let keep: Vec<usize> = (0..y.len())
        .filter(|&i| ((y[i] - first.predict(&design.rows[i])) / first.sigma.max(f64::MIN_POSITIVE)).abs() <= TRIM_Z)
        .collect();
    let (fit, trimmed) = if keep.len() < y.len() && keep.len() >= needed {
        let (kx, ky): (Vec<_>, Vec<_>) = keep.iter().map(|&i| (design.rows[i].clone(), y[i])).unzip();
        match fit_ols(&kx, &ky) {
            Ok(f) => (f, y.len() - keep.len()),
            Err(_) => (first, 0),
        }
    } else {
        (first, 0)
    };
    out.trimmed_from_fit = trimmed;

    let mut rows = vec![];
    let (mut compas, mut resid, mut ratings_used) = (vec![], vec![], vec![]);
    let (mut above, mut below) = (0, 0);
    for (i, e) in used.iter().enumerate() {
        let pred = fit.predict(&design.rows[i]);
        let r = y[i] - pred;
        let z = if fit.sigma > 0.0 { r / fit.sigma } else { 0.0 };
        let dev = r.exp() - 1.0;
        let flag = if z.abs() >= FLAG_Z && dev.abs() >= FLAG_MIN_PCT {
            if r > 0.0 {
                above += 1;
                Some("above_expected")
            } else {
                below += 1;
                Some("below_expected")
            }
        } else {
            None
        };
        compas.push(r.exp());
        if let Some(rt) = ratings.get(&e.id) {
            resid.push(r);
            ratings_used.push(*rt as f64);
        }
        rows.push(Row {
            employee_id: e.id,
            employee_number: e.employee_number.clone(),
            job_title: e.job_title.clone(),
            base_salary: e.base_salary,
            expected_salary: (pred.exp() * 100.0).round() / 100.0,
            compa_ratio: (r.exp() * 1000.0).round() / 1000.0,
            z_score: (z * 100.0).round() / 100.0,
            flag,
        });
    }
    rows.sort_by(|a, b| {
        b.flag.is_some().cmp(&a.flag.is_some()).then(
            b.z_score.abs().partial_cmp(&a.z_score.abs()).unwrap_or(std::cmp::Ordering::Equal),
        )
    });
    rows.truncate(clamp_limit(q.limit) as usize);

    out.model = Some(ModelInfo {
        r2: (fit.r2 * 1000.0).round() / 1000.0,
        adjusted_r2: (fit.adj_r2 * 1000.0).round() / 1000.0,
        typical_error_pct: ((fit.sigma.exp() - 1.0) * 1000.0).round() / 10.0,
        terms: design
            .columns
            .iter()
            .zip(&fit.coef[1..])
            .map(|(t, c)| Term { term: t.clone(), effect_pct: (effect_pct(*c) * 10.0).round() / 10.0 })
            .collect(),
    });
    out.summary = Some(Summary {
        median_compa_ratio: median(&mut compas).map(|m| (m * 1000.0).round() / 1000.0),
        flagged_above_expected: above,
        flagged_below_expected: below,
    });
    out.pay_performance = Some(PayPerformance {
        employees_with_review: resid.len(),
        correlation: if resid.len() >= 10 { pearson(&resid, &ratings_used).map(|c| (c * 1000.0).round() / 1000.0) } else { None },
    });
    out.employees = rows;
    Ok(Json(out))
}

/// Share of active employees that carry every benchmark attribute (used by integrity checks).
pub async fn attribute_completeness<C: ConnectionTrait>(c: &C) -> Result<(i64, i64), AppError> {
    let row = c
        .query_one(stmt(
            "SELECT COUNT(*) FILTER (WHERE education_level IS NOT NULL AND years_experience IS NOT NULL \
                    AND city_tier IS NOT NULL AND remote_work IS NOT NULL), COUNT(*) \
             FROM employees WHERE status = 'active'",
            vec![],
        ))
        .await?
        .ok_or(AppError::internal())?;
    Ok((row.try_get_by_index::<i64>(0)?, row.try_get_by_index::<i64>(1)?))
}
