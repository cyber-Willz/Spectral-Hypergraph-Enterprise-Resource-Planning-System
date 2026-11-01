//! Environment, health & safety: incident rates, safety audit scoring, environmental reporting.
use crate::db::{row, rows};
use crate::error::{AppError, AppResult};
use crate::state::{AppState, AuthUser};
use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde_json::{json, Value};
use std::collections::HashMap;
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/ehs/analytics/safety", get(safety))
        .route("/api/ehs/safety-audits/{id}/complete", post(complete_audit))
        .route("/api/ehs/environmental/report", get(env_report))
        .route("/api/ehs/incidents/{id}/corrective-action", post(incident_capa))
        .route("/api/ehs/compliance", get(compliance))
}

fn q(p: &HashMap<String, String>, k: &str) -> Option<String> { p.get(k).cloned() }

/// TRIR = recordables x 200,000 / hours; LTIFR = lost-time incidents x 1,000,000 / hours; severity rate = lost days x 200,000 / hours.
async fn safety(State(st): State<AppState>, Query(p): Query<HashMap<String, String>>) -> AppResult<Json<Value>> {
    let args = || vec![q(&p, "from").into(), q(&p, "to").into()];
    let range = "occurred_at >= COALESCE($1::date, date_trunc('year', current_date)::date) AND occurred_at < COALESCE($2::date, (date_trunc('year', current_date) + interval '1 year')::date)";
    let hours = row(&st.db, "SELECT COALESCE(sum(hours_worked),0)::float8 AS hours FROM ehs_work_hours
        WHERE period_start >= COALESCE($1::date, date_trunc('year', current_date)::date) AND period_start < COALESCE($2::date, (date_trunc('year', current_date) + interval '1 year')::date)", args()).await?;
    let h = hours["hours"].as_f64().unwrap_or(0.0);
    let t = row(&st.db, &format!("SELECT count(*) AS incidents, count(*) FILTER (WHERE is_recordable) AS recordable,
        count(*) FILTER (WHERE lost_days > 0) AS lost_time_incidents, COALESCE(sum(lost_days),0)::bigint AS lost_days,
        count(*) FILTER (WHERE incident_type='near_miss') AS near_misses, count(*) FILTER (WHERE is_reportable) AS reportable,
        count(*) FILTER (WHERE severity='fatal') AS fatalities FROM ehs_incidents WHERE {range}"), args()).await?;
    let by_type = rows(&st.db, &format!("SELECT incident_type, count(*) AS incidents FROM ehs_incidents WHERE {range} GROUP BY 1 ORDER BY 2 DESC"), args()).await?;
    let by_sev = rows(&st.db, &format!("SELECT severity, count(*) AS incidents FROM ehs_incidents WHERE {range} GROUP BY 1"), args()).await?;
    let open = row(&st.db, "SELECT count(*) AS open_incidents, count(*) FILTER (WHERE status='reported' AND created_at < now() - interval '7 days') AS uninvestigated_over_7d FROM ehs_incidents WHERE status<>'closed'", vec![]).await?;
    let rate = |n: &str, f: f64| if h > 0.0 { Some((t[n].as_f64().unwrap_or(0.0) * f / h * 100.0).round() / 100.0) } else { None };
    Ok(Json(json!({ "hours_worked": h, "totals": t,
        "rates": { "trir": rate("recordable", 200_000.0), "ltifr": rate("lost_time_incidents", 1_000_000.0), "severity_rate": rate("lost_days", 200_000.0) },
        "by_type": by_type, "by_severity": by_sev, "open": open })))
}

/// Scores an inspection: conforming / (conforming + non-conforming). Non-conformances are returned for follow-up.
async fn complete_audit(State(st): State<AppState>, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let c = row(&st.db, "SELECT count(*) FILTER (WHERE result='pending') AS pending, count(*) FILTER (WHERE result='conforming') AS ok,
        count(*) FILTER (WHERE result='non_conforming') AS bad FROM ehs_safety_audit_items WHERE audit_id=$1", vec![id.into()]).await?;
    let exists = row(&st.db, "SELECT 1 AS x FROM ehs_safety_audits WHERE id=$1", vec![id.into()]).await?;
    if exists.is_null() { return Err(AppError::NotFound(None)); }
    let (pending, ok, bad) = (c["pending"].as_i64().unwrap_or(0), c["ok"].as_i64().unwrap_or(0), c["bad"].as_i64().unwrap_or(0));
    if pending > 0 { return Err(AppError::Unprocessable(format!("{pending} checklist item(s) are still pending"))); }
    if ok + bad == 0 { return Err(AppError::Unprocessable("audit has no scored checklist items".into())); }
    let score = (ok as f64 / (ok + bad) as f64 * 1000.0).round() / 10.0;
    let a = row(&st.db, "UPDATE ehs_safety_audits SET status='completed', score=$2 WHERE id=$1 RETURNING id::text AS id, audit_number, status, score", vec![id.into(), score.into()]).await?;
    let nc = rows(&st.db, "SELECT id::text AS id, category, item, comments FROM ehs_safety_audit_items WHERE audit_id=$1 AND result='non_conforming'", vec![id.into()]).await?;
    Ok(Json(json!({ "audit": a, "conforming": ok, "non_conforming": bad, "non_conforming_items": nc })))
}

async fn env_report(State(st): State<AppState>, Query(p): Query<HashMap<String, String>>) -> AppResult<Json<Value>> {
    let args = || vec![q(&p, "from").into(), q(&p, "to").into(), q(&p, "site").into()];
    let w = "reading_date >= COALESCE($1::date, date_trunc('year', current_date)::date) AND reading_date <= COALESCE($2::date, current_date) AND ($3::text IS NULL OR site=$3)";
    let summary = rows(&st.db, &format!("SELECT site, metric, unit, count(*) AS readings, round(sum(value)::numeric,2)::float8 AS total, round(avg(value)::numeric,2)::float8 AS average,
        min(value) AS min, max(value) AS max, count(*) FILTER (WHERE limit_value IS NOT NULL AND value > limit_value) AS exceedances
        FROM ehs_environmental_readings WHERE {w} GROUP BY site, metric, unit ORDER BY site, metric"), args()).await?;
    let monthly = rows(&st.db, &format!("SELECT to_char(date_trunc('month', reading_date),'YYYY-MM') AS month, site, metric, round(avg(value)::numeric,2)::float8 AS average, round(sum(value)::numeric,2)::float8 AS total
        FROM ehs_environmental_readings WHERE {w} GROUP BY 1,2,3 ORDER BY 1,2,3"), args()).await?;
    let ex = rows(&st.db, &format!("SELECT id::text AS id, site, metric, value, limit_value, unit, reading_date::text AS reading_date FROM ehs_environmental_readings
        WHERE {w} AND limit_value IS NOT NULL AND value > limit_value ORDER BY reading_date DESC LIMIT 100"), args()).await?;
    Ok(Json(json!({ "summary": summary, "monthly": monthly, "exceedances": ex })))
}

/// Raises a corrective action from an incident and moves the incident into the corrective-action stage.
async fn incident_capa(Extension(u): Extension<AuthUser>, State(st): State<AppState>, Path(id): Path<Uuid>, Json(b): Json<Value>) -> AppResult<Json<Value>> {
    let desc = b["description"].as_str().ok_or_else(|| AppError::BadRequest("description required".into()))?;
    let owner: Option<Uuid> = b["owner_id"].as_str().and_then(|s| s.parse().ok()).or(Some(u.id));
    let due: Option<String> = b["due_date"].as_str().map(String::from);
    if row(&st.db, "SELECT 1 AS x FROM ehs_incidents WHERE id=$1", vec![id.into()]).await?.is_null() { return Err(AppError::NotFound(None)); }
    let r = row(&st.db, "INSERT INTO qms_corrective_actions (incident_id, action_type, description, owner_id, due_date) VALUES ($1, COALESCE($2,'corrective'), $3, $4, $5::date)
        RETURNING id::text AS id, capa_number, status, due_date::text AS due_date",
        vec![id.into(), b["action_type"].as_str().map(String::from).into(), desc.into(), owner.into(), due.into()]).await?;
    row(&st.db, "UPDATE ehs_incidents SET status='corrective_action' WHERE id=$1 AND status IN ('reported','investigating') RETURNING 1 AS x", vec![id.into()]).await?;
    Ok(Json(r))
}

async fn compliance(State(st): State<AppState>) -> AppResult<Json<Value>> {
    let s = rows(&st.db, "SELECT area, framework, count(*) AS requirements, count(*) FILTER (WHERE status='compliant') AS compliant,
        count(*) FILTER (WHERE status IN ('non_compliant','partially_compliant')) AS gaps FROM grc_compliance_requirements
        WHERE area IN ('environmental','health_safety') GROUP BY area, framework ORDER BY area, framework", vec![]).await?;
    Ok(Json(json!({ "summary": s })))
}
