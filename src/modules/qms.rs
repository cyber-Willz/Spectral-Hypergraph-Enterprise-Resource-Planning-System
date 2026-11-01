//! Quality management: testing analytics, NCR workflow, CAPA verification, regulatory compliance.
use crate::db::{row, rows};
use crate::error::{AppError, AppResult};
use crate::state::{AppState, AuthUser};
use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde_json::{json, Value};
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/qms/nonconformances/{id}/transition", post(transition))
        .route("/api/qms/corrective-actions/{id}/verify", post(verify_capa))
        .route("/api/qms/analytics/quality", get(quality))
        .route("/api/qms/compliance", get(compliance))
}

fn allowed(from: &str) -> &'static [&'static str] {
    match from {
        "open" => &["investigating"],
        "investigating" => &["containment", "corrective_action", "open"],
        "containment" => &["corrective_action", "investigating"],
        "corrective_action" => &["verification", "containment"],
        "verification" => &["closed", "corrective_action"],
        "closed" => &["open"],
        _ => &[],
    }
}

/// Enforces the NCR workflow graph. Closing is additionally guarded by a database trigger
/// (all corrective actions must be verified/closed/cancelled).
async fn transition(State(st): State<AppState>, Path(id): Path<Uuid>, Json(b): Json<Value>) -> AppResult<Json<Value>> {
    let to = b["status"].as_str().ok_or_else(|| AppError::BadRequest("status required".into()))?;
    let cur = row(&st.db, "SELECT status FROM qms_nonconformances WHERE id=$1", vec![id.into()]).await?;
    let from = cur["status"].as_str().ok_or(AppError::NotFound(None))?.to_string();
    if !allowed(&from).contains(&to) {
        return Err(AppError::Conflict(format!("cannot move from '{from}' to '{to}'; allowed: {:?}", allowed(&from))));
    }
    if to == "closed" && b["disposition"].as_str().is_none() {
        let d = row(&st.db, "SELECT disposition FROM qms_nonconformances WHERE id=$1", vec![id.into()]).await?;
        if d["disposition"].is_null() { return Err(AppError::Unprocessable("disposition is required to close a nonconformance".into())); }
    }
    let r = row(&st.db, "UPDATE qms_nonconformances SET status=$2, root_cause=COALESCE($3, root_cause), disposition=COALESCE($4, disposition) WHERE id=$1
        RETURNING id::text AS id, ncr_number, status, disposition, root_cause, closed_at",
        vec![id.into(), to.into(), b["root_cause"].as_str().map(String::from).into(), b["disposition"].as_str().map(String::from).into()]).await?;
    Ok(Json(r))
}

/// Effectiveness verification: effective -> verified; otherwise the CAPA is reopened.
async fn verify_capa(Extension(u): Extension<AuthUser>, State(st): State<AppState>, Path(id): Path<Uuid>, Json(b): Json<Value>) -> AppResult<Json<Value>> {
    u.require(&["admin", "manager"])?;
    let effective = b["effective"].as_bool().ok_or_else(|| AppError::BadRequest("effective (bool) required".into()))?;
    let cur = row(&st.db, "SELECT status FROM qms_corrective_actions WHERE id=$1", vec![id.into()]).await?;
    match cur["status"].as_str() {
        None => return Err(AppError::NotFound(None)),
        Some("implemented") => {}
        Some(s) => return Err(AppError::Conflict(format!("only implemented actions can be verified (current: {s})"))),
    }
    let r = row(&st.db, "UPDATE qms_corrective_actions SET status=CASE WHEN $2 THEN 'verified' ELSE 'in_progress' END,
        verified_by=CASE WHEN $2 THEN $3::uuid END, verified_on=CASE WHEN $2 THEN current_date END, effectiveness_check=$4 WHERE id=$1
        RETURNING id::text AS id, capa_number, status, verified_on::text AS verified_on, effectiveness_check",
        vec![id.into(), effective.into(), u.id.into(), b["notes"].as_str().map(String::from).into()]).await?;
    Ok(Json(r))
}

async fn quality(State(st): State<AppState>) -> AppResult<Json<Value>> {
    let tests = rows(&st.db, "SELECT p.id::text AS plan_id, p.plan_code, p.name, count(r.*) AS results, count(r.*) FILTER (WHERE r.result='pass') AS passed,
        count(r.*) FILTER (WHERE r.result='fail') AS failed, round(100.0*count(r.*) FILTER (WHERE r.result='pass')/NULLIF(count(r.*) FILTER (WHERE r.result<>'pending'),0),1)::float8 AS pass_rate_pct,
        round(avg(r.measured_value)::numeric,3)::float8 AS mean, round(stddev_samp(r.measured_value)::numeric,3)::float8 AS std_dev
        FROM qms_test_plans p LEFT JOIN qms_test_results r ON r.test_plan_id=p.id GROUP BY p.id ORDER BY failed DESC, p.name", vec![]).await?;
    let ncr = row(&st.db, "SELECT count(*) AS total, count(*) FILTER (WHERE status<>'closed') AS open,
        count(*) FILTER (WHERE status<>'closed' AND severity='critical') AS open_critical,
        round(avg(EXTRACT(epoch FROM (closed_at-created_at))/86400) FILTER (WHERE closed_at IS NOT NULL)::numeric,1)::float8 AS avg_days_to_close FROM qms_nonconformances", vec![]).await?;
    let by_status = rows(&st.db, "SELECT status, count(*) AS ncrs FROM qms_nonconformances GROUP BY status", vec![]).await?;
    let pareto = rows(&st.db, "SELECT COALESCE(root_cause,'(not determined)') AS root_cause, count(*) AS ncrs,
        round(100.0*sum(count(*)) OVER (ORDER BY count(*) DESC, COALESCE(root_cause,'')) / sum(count(*)) OVER (),1)::float8 AS cumulative_pct
        FROM qms_nonconformances GROUP BY root_cause ORDER BY 2 DESC, 1", vec![]).await?;
    let capa = row(&st.db, "SELECT count(*) AS total, count(*) FILTER (WHERE status NOT IN ('verified','closed','cancelled')) AS open,
        count(*) FILTER (WHERE status NOT IN ('verified','closed','cancelled') AND due_date < current_date) AS overdue FROM qms_corrective_actions", vec![]).await?;
    Ok(Json(json!({ "testing": tests, "nonconformances": ncr, "ncr_by_status": by_status, "root_cause_pareto": pareto, "corrective_actions": capa })))
}

async fn compliance(State(st): State<AppState>) -> AppResult<Json<Value>> {
    let s = rows(&st.db, "SELECT framework, count(*) AS requirements, count(*) FILTER (WHERE status='compliant') AS compliant,
        count(*) FILTER (WHERE status IN ('non_compliant','partially_compliant')) AS gaps,
        round(100.0*count(*) FILTER (WHERE status='compliant')/NULLIF(count(*) FILTER (WHERE status<>'not_applicable'),0),1)::float8 AS compliance_pct
        FROM grc_compliance_requirements WHERE area='quality' GROUP BY framework", vec![]).await?;
    let items = rows(&st.db, "SELECT id::text AS id, framework, reference, title, status, next_review_on::text AS next_review_on FROM grc_compliance_requirements WHERE area='quality' ORDER BY framework, reference", vec![]).await?;
    Ok(Json(json!({ "summary": s, "requirements": items })))
}
