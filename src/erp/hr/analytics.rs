//! Workforce analytics.

use axum::{extract::State, Json};
use rust_decimal::Decimal;
use sea_orm::FromQueryResult;
use serde::Serialize;

use crate::{
    error::{ApiResult, AppError},
    erp::util::stmt,
    state::AppState,
};

#[derive(Serialize, FromQueryResult)]
pub struct DeptHeadcount {
    department: String,
    headcount: i64,
    avg_salary: Decimal,
}

#[derive(Serialize, FromQueryResult)]
pub struct StageCount {
    stage: String,
    applications: i64,
}

#[derive(Serialize, FromQueryResult)]
pub struct RatingCount {
    rating: i16,
    reviews: i64,
}

#[derive(Serialize)]
pub struct Summary {
    headcount: i64,
    annualised_salary_cost: Decimal,
    average_salary: Decimal,
    average_tenure_years: Decimal,
    hires_last_12_months: i64,
    terminations_last_12_months: i64,
    /// Terminations over the last 12 months divided by average headcount (start and end of window).
    turnover_rate_12m: Option<Decimal>,
    by_department: Vec<DeptHeadcount>,
    average_rating: Option<Decimal>,
    rating_distribution: Vec<RatingCount>,
    training_enrolments: i64,
    training_completion_rate: Option<Decimal>,
    open_postings: i64,
    recruitment_pipeline: Vec<StageCount>,
}

#[derive(FromQueryResult)]
struct Core {
    headcount: i64,
    salary_cost: Decimal,
    avg_salary: Decimal,
    avg_tenure_years: Decimal,
    hires_12m: i64,
    terms_12m: i64,
    headcount_12m_ago: i64,
}

#[derive(FromQueryResult)]
struct Other {
    avg_rating: Option<Decimal>,
    enrolments: i64,
    completed: i64,
    open_postings: i64,
}

pub async fn summary(State(st): State<AppState>) -> ApiResult<Summary> {
    let core_sql = r#"
        SELECT
          COUNT(*) FILTER (WHERE status = 'active')                                        AS headcount,
          COALESCE(SUM(base_salary) FILTER (WHERE status = 'active'), 0)                   AS salary_cost,
          COALESCE(AVG(base_salary) FILTER (WHERE status = 'active'), 0)                   AS avg_salary,
          COALESCE(AVG((CURRENT_DATE - hire_date) / 365.25) FILTER (WHERE status = 'active'), 0)::numeric AS avg_tenure_years,
          COUNT(*) FILTER (WHERE hire_date > CURRENT_DATE - INTERVAL '12 months')          AS hires_12m,
          COUNT(*) FILTER (WHERE termination_date > CURRENT_DATE - INTERVAL '12 months')   AS terms_12m,
          COUNT(*) FILTER (WHERE hire_date <= CURRENT_DATE - INTERVAL '12 months'
                           AND (termination_date IS NULL OR termination_date > CURRENT_DATE - INTERVAL '12 months')) AS headcount_12m_ago
        FROM employees"#;
    let core = Core::find_by_statement(stmt(core_sql, vec![]))
        .one(&st.db)
        .await?
        .ok_or(AppError::internal())?;

    let other_sql = r#"
        SELECT
          (SELECT AVG(rating)::numeric FROM performance_reviews)                          AS avg_rating,
          (SELECT COUNT(*) FROM training_enrollments)                                     AS enrolments,
          (SELECT COUNT(*) FROM training_enrollments WHERE status = 'completed')          AS completed,
          (SELECT COUNT(*) FROM job_postings WHERE status = 'open')                       AS open_postings"#;
    let other = Other::find_by_statement(stmt(other_sql, vec![]))
        .one(&st.db)
        .await?
        .ok_or(AppError::internal())?;

    let by_department = DeptHeadcount::find_by_statement(stmt(
        r#"SELECT COALESCE(d.name, 'Unassigned') AS department, COUNT(*) AS headcount,
                  COALESCE(ROUND(AVG(e.base_salary), 2), 0) AS avg_salary
           FROM employees e LEFT JOIN departments d ON d.id = e.department_id
           WHERE e.status = 'active' GROUP BY 1 ORDER BY 1"#,
        vec![],
    ))
    .all(&st.db)
    .await?;
    let rating_distribution = RatingCount::find_by_statement(stmt(
        "SELECT rating, COUNT(*) AS reviews FROM performance_reviews GROUP BY rating ORDER BY rating",
        vec![],
    ))
    .all(&st.db)
    .await?;
    let recruitment_pipeline = StageCount::find_by_statement(stmt(
        "SELECT stage, COUNT(*) AS applications FROM applications GROUP BY stage ORDER BY stage",
        vec![],
    ))
    .all(&st.db)
    .await?;

    let avg_headcount = Decimal::from(core.headcount + core.headcount_12m_ago) / Decimal::from(2);
    Ok(Json(Summary {
        headcount: core.headcount,
        annualised_salary_cost: core.salary_cost,
        average_salary: core.avg_salary.round_dp(2),
        average_tenure_years: core.avg_tenure_years.round_dp(1),
        hires_last_12_months: core.hires_12m,
        terminations_last_12_months: core.terms_12m,
        turnover_rate_12m: (!avg_headcount.is_zero())
            .then(|| (Decimal::from(core.terms_12m) / avg_headcount * Decimal::from(100)).round_dp(1)),
        by_department,
        average_rating: other.avg_rating.map(|r| r.round_dp(2)),
        rating_distribution,
        training_enrolments: other.enrolments,
        training_completion_rate: (other.enrolments > 0)
            .then(|| (Decimal::from(other.completed) / Decimal::from(other.enrolments) * Decimal::from(100)).round_dp(1)),
        open_postings: other.open_postings,
        recruitment_pipeline,
    }))
}
