//! Supplier scorecards: observed delivery/quality performance next to structural risk.
use axum::{
    extract::{Path, Query, State},
    routing::get,
    Json, Router,
};
use chrono::NaiveDate;
use sea_orm::{DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::supply_chain::{
    error::{AppError, AppResult},
    risk::{performance_score, quality_score, recommendation, risk_band, risk_probability, RiskInputs},
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/reports/supplier-scorecards", get(scorecards))
        .route("/suppliers/{id}/scorecard", get(one))
}

#[derive(Debug, FromQueryResult)]
struct Row {
    supplier_id: Uuid,
    code: String,
    name: String,
    country: Option<String>,
    financial: Option<f64>,
    geo: Option<f64>,
    alternatives: Option<f64>,
    environmental: Option<f64>,
    disruptions: Option<f64>,
    lines_received: i64,
    on_time_rate: Option<f64>,
    early_rate: Option<f64>,
    avg_days_early: Option<f64>,
    in_full_rate: Option<f64>,
    fill_rate: Option<f64>,
    avg_days_late: Option<f64>,
    lead_mean: Option<f64>,
    lead_sd: Option<f64>,
    minor: i64,
    major: i64,
    critical: i64,
}

#[derive(Serialize)]
pub struct Scorecard {
    pub supplier_id: Uuid,
    pub code: String,
    pub name: String,
    pub country: Option<String>,
    pub lines_received: i64,
    pub on_time_rate: Option<f64>,
    /// share received more than `early_tolerance_days` (default 3) before the promised date
    pub early_rate: Option<f64>,
    pub avg_days_early: Option<f64>,
    /// on time but not early: received inside the acceptance window
    pub on_window_rate: Option<f64>,
    pub in_full_rate: Option<f64>,
    pub fill_rate: Option<f64>,
    pub avg_days_late: Option<f64>,
    pub lead_time_mean_days: Option<f64>,
    pub lead_time_sd_days: Option<f64>,
    pub incidents_minor: i64,
    pub incidents_major: i64,
    pub incidents_critical: i64,
    pub quality_score: Option<f64>,
    /// 0-100: 40% on-time, 30% in-full, 30% quality. None until deliveries exist.
    pub performance_score: Option<f64>,
    /// Probability from the dataset-fitted structural model (financial, geopolitical, alternatives,
    /// environmental, disruption history).
    pub structural_risk: f64,
    pub risk_band: &'static str,
    pub risk_inputs_imputed: usize,
    pub recommendation: &'static str,
}

const SQL: &str = "\
WITH l AS (
  SELECT po.supplier_id, pol.id AS line_id, pol.quantity_ordered AS qo, pol.quantity_received AS qr,
         po.status AS pst, COALESCE(pol.promised_date, po.expected_date) AS promised, po.order_date,
         MAX(gr.received_on) AS last_rcpt
  FROM purchase_order_lines pol
  JOIN purchase_orders po ON po.id = pol.purchase_order_id
  JOIN goods_receipts gr ON gr.purchase_order_line_id = pol.id
  WHERE ($2::date IS NULL OR po.order_date >= $2)
  GROUP BY po.supplier_id, pol.id, pol.quantity_ordered, pol.quantity_received, po.status,
           pol.promised_date, po.expected_date, po.order_date),
perf AS (
  SELECT supplier_id, COUNT(*)::bigint AS lines_received,
    COUNT(*) FILTER (WHERE pst IN ('received','closed_short'))::bigint AS closed_lines,
    (AVG((last_rcpt <= promised)::int) FILTER (WHERE promised IS NOT NULL))::float8 AS on_time_rate,
    (AVG((last_rcpt < promised - $3::int)::int) FILTER (WHERE promised IS NOT NULL))::float8 AS early_rate,
    (AVG(GREATEST(promised - last_rcpt, 0)::float8) FILTER (WHERE promised IS NOT NULL AND last_rcpt < promised))::float8 AS avg_days_early,
    (AVG((qr >= qo)::int) FILTER (WHERE pst IN ('received','closed_short')))::float8 AS in_full_rate,
    (AVG(qr::float8 / qo) FILTER (WHERE pst IN ('received','closed_short')))::float8 AS fill_rate,
    (AVG((last_rcpt - promised)::float8) FILTER (WHERE promised IS NOT NULL))::float8 AS avg_days_late,
    AVG((last_rcpt - order_date)::float8) AS lead_mean,
    STDDEV_SAMP((last_rcpt - order_date)::float8) AS lead_sd
  FROM l GROUP BY supplier_id),
q AS (
  SELECT supplier_id,
    COUNT(*) FILTER (WHERE severity = 'minor')::bigint AS minor,
    COUNT(*) FILTER (WHERE severity = 'major')::bigint AS major,
    COUNT(*) FILTER (WHERE severity = 'critical')::bigint AS critical
  FROM quality_incidents
  WHERE supplier_id IS NOT NULL AND ($2::date IS NULL OR occurred_on >= $2)
  GROUP BY supplier_id)
SELECT s.id AS supplier_id, s.code, s.name, s.country,
       s.financial_stability_score AS financial, s.geopolitical_risk_index AS geo,
       s.alternatives_available::float8 AS alternatives, s.environmental_compliance AS environmental,
       s.previous_disruptions::float8 AS disruptions,
       COALESCE(perf.lines_received, 0) AS lines_received,
       perf.on_time_rate, perf.early_rate, perf.avg_days_early, perf.in_full_rate, perf.fill_rate, perf.avg_days_late, perf.lead_mean, perf.lead_sd,
       COALESCE(q.minor, 0) AS minor, COALESCE(q.major, 0) AS major, COALESCE(q.critical, 0) AS critical
FROM suppliers s
LEFT JOIN perf ON perf.supplier_id = s.id
LEFT JOIN q ON q.supplier_id = s.id
WHERE ($1::uuid IS NULL OR s.id = $1)
ORDER BY s.code";

fn build(r: Row) -> Scorecard {
    let (p, imputed) = risk_probability(&RiskInputs {
        financial_stability: r.financial,
        geopolitical_risk: r.geo,
        alternatives: r.alternatives,
        environmental_compliance: r.environmental,
        previous_disruptions: r.disruptions,
    });
    // with most inputs missing the imputed score only reflects the dataset's base rate
    let band = if imputed >= 3 { "unknown" } else { risk_band(p) };
    let quality = (r.lines_received > 0)
        .then(|| quality_score(r.minor, r.major, r.critical, r.lines_received));
    let perf = match (r.on_time_rate, quality) {
        (Some(ot), Some(q)) => {
            // in-full is only judged on closed orders; before any close, assume it is not the problem
            let inf = r.in_full_rate.unwrap_or(1.0);
            Some(performance_score(ot * 100.0, inf * 100.0, q))
        }
        _ => None,
    };
    Scorecard {
        supplier_id: r.supplier_id,
        code: r.code,
        name: r.name,
        country: r.country,
        lines_received: r.lines_received,
        on_time_rate: r.on_time_rate,
        early_rate: r.early_rate,
        avg_days_early: r.avg_days_early,
        on_window_rate: r.on_time_rate.zip(r.early_rate).map(|(o, e)| (o - e).max(0.0)),
        in_full_rate: r.in_full_rate,
        fill_rate: r.fill_rate,
        avg_days_late: r.avg_days_late,
        lead_time_mean_days: r.lead_mean,
        lead_time_sd_days: r.lead_sd,
        incidents_minor: r.minor,
        incidents_major: r.major,
        incidents_critical: r.critical,
        quality_score: quality,
        performance_score: perf,
        structural_risk: p,
        risk_band: band,
        risk_inputs_imputed: imputed,
        recommendation: recommendation(perf, band),
    }
}

pub async fn fetch(db: &sea_orm::DatabaseConnection, id: Option<Uuid>, since: Option<NaiveDate>, early_tol: i32) -> AppResult<Vec<Scorecard>> {
    let rows = Row::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        SQL,
        [id.into(), since.into(), early_tol.into()],
    ))
    .all(db)
    .await?;
    Ok(rows.into_iter().map(build).collect())
}

#[derive(Deserialize)]
pub struct ScoreQuery {
    /// Only orders placed on/after this date
    since: Option<NaiveDate>,
    /// days before the promise beyond which a delivery counts as early (default 3)
    early_tolerance_days: Option<i32>,
}

async fn scorecards(
    State(s): State<AppState>,
    Query(q): Query<ScoreQuery>,
) -> AppResult<Json<Vec<Scorecard>>> {
    Ok(Json(fetch(&s.db, None, q.since, q.early_tolerance_days.unwrap_or(3)).await?))
}

async fn one(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Query(q): Query<ScoreQuery>,
) -> AppResult<Json<Scorecard>> {
    fetch(&s.db, Some(id), q.since, q.early_tolerance_days.unwrap_or(3))
        .await?
        .into_iter()
        .next()
        .map(Json)
        .ok_or_else(|| AppError::not_found("supplier"))
}
