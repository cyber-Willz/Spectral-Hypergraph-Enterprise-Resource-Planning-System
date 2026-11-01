//! Governance, risk & compliance: heat map, control testing, compliance dashboards, audit reports.
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
        .route("/api/grc/risks/heatmap", get(heatmap))
        .route("/api/grc/risks/summary", get(risk_summary))
        .route("/api/grc/controls/coverage", get(coverage))
        .route("/api/grc/controls/test-schedule", get(test_schedule))
        .route("/api/grc/controls/{id}/test", post(record_test))
        .route("/api/grc/compliance/dashboard", get(compliance_dashboard))
        .route("/api/grc/audits/{id}/report", get(audit_report))
}

async fn heatmap(State(st): State<AppState>, Query(p): Query<HashMap<String, String>>) -> AppResult<Json<Value>> {
    let residual = p.get("view").map(|v| v == "residual").unwrap_or(false);
    let (l, i) = if residual { ("residual_likelihood", "residual_impact") } else { ("likelihood", "impact") };
    let cells = rows(&st.db, &format!("SELECT {l} AS likelihood, {i} AS impact, count(*) AS risks,
        COALESCE(jsonb_agg(jsonb_build_object('id',id,'code',risk_code,'title',title)),'[]'::jsonb) AS items
        FROM grc_risks WHERE status NOT IN ('closed') AND {l} IS NOT NULL AND {i} IS NOT NULL GROUP BY 1,2 ORDER BY 1 DESC,2"), vec![]).await?;
    Ok(Json(json!({ "view": if residual { "residual" } else { "inherent" }, "scale": "1-5 x 1-5", "cells": cells })))
}

async fn risk_summary(State(st): State<AppState>) -> AppResult<Json<Value>> {
    let by_rating = rows(&st.db, "SELECT rating, count(*) AS risks FROM grc_risks WHERE status<>'closed' GROUP BY rating ORDER BY array_position(ARRAY['critical','high','medium','low'], rating)", vec![]).await?;
    let by_cat = rows(&st.db, "SELECT category, count(*) AS risks, round(avg(score),1)::float8 AS avg_inherent, round(avg(residual_score),1)::float8 AS avg_residual FROM grc_risks WHERE status<>'closed' GROUP BY category ORDER BY 2 DESC", vec![]).await?;
    let by_status = rows(&st.db, "SELECT status, count(*) AS risks FROM grc_risks GROUP BY status", vec![]).await?;
    let overdue = rows(&st.db, "SELECT id::text AS id, risk_code, title, rating, review_date::text AS review_date FROM grc_risks WHERE status<>'closed' AND review_date < current_date ORDER BY review_date", vec![]).await?;
    Ok(Json(json!({ "by_rating": by_rating, "by_category": by_cat, "by_status": by_status, "overdue_reviews": overdue })))
}

/// Risks with no mapped control (internal-control gaps) and controls that mitigate nothing.
async fn coverage(State(st): State<AppState>) -> AppResult<Json<Value>> {
    let gaps = rows(&st.db, "SELECT r.id::text AS id, r.risk_code, r.title, r.rating FROM grc_risks r
        WHERE r.status NOT IN ('closed','accepted') AND NOT EXISTS (SELECT 1 FROM grc_risk_controls rc WHERE rc.risk_id=r.id) ORDER BY r.score DESC", vec![]).await?;
    let orphan = rows(&st.db, "SELECT c.id::text AS id, c.control_code, c.name FROM grc_controls c WHERE c.status='active' AND NOT EXISTS (SELECT 1 FROM grc_risk_controls rc WHERE rc.control_id=c.id)", vec![]).await?;
    let weak = rows(&st.db, "SELECT DISTINCT r.id::text AS id, r.risk_code, r.title, c.control_code, c.effectiveness FROM grc_risk_controls rc
        JOIN grc_risks r ON r.id=rc.risk_id JOIN grc_controls c ON c.id=rc.control_id WHERE c.effectiveness='ineffective' AND r.status<>'closed'", vec![]).await?;
    Ok(Json(json!({ "risks_without_controls": gaps, "controls_without_risks": orphan, "risks_relying_on_ineffective_controls": weak })))
}

async fn test_schedule(State(st): State<AppState>) -> AppResult<Json<Value>> {
    let d = rows(&st.db, "SELECT id::text AS id, control_code, name, frequency, effectiveness, next_test_due::text AS next_test_due,
        CASE WHEN next_test_due IS NULL THEN 'never_scheduled' WHEN next_test_due < current_date THEN 'overdue' WHEN next_test_due <= current_date + 30 THEN 'due_soon' ELSE 'scheduled' END AS state
        FROM grc_controls WHERE status='active' ORDER BY next_test_due NULLS FIRST", vec![]).await?;
    Ok(Json(json!({ "data": d })))
}

fn interval_days(freq: &str) -> Option<i32> {
    Some(match freq { "daily" => 1, "weekly" => 7, "monthly" => 30, "quarterly" => 91, "annually" => 365, _ => return None })
}

/// Records a control test and rolls the next due date forward according to the control's frequency.
async fn record_test(Extension(u): Extension<AuthUser>, State(st): State<AppState>, Path(id): Path<Uuid>, Json(b): Json<Value>) -> AppResult<Json<Value>> {
    let result = b["result"].as_str().ok_or_else(|| AppError::BadRequest("result required (effective|partially_effective|ineffective)".into()))?;
    let ctl = row(&st.db, "SELECT frequency FROM grc_controls WHERE id=$1", vec![id.into()]).await?;
    if ctl.is_null() { return Err(AppError::NotFound(None)); }
    let days = interval_days(ctl["frequency"].as_str().unwrap_or("")).unwrap_or(0);
    let t = row(&st.db, "INSERT INTO grc_control_tests (control_id, tested_by, result, notes, evidence_url) VALUES ($1,$2,$3,$4,$5) RETURNING id::text AS id, tested_on::text AS tested_on",
        vec![id.into(), u.id.into(), result.into(), b["notes"].as_str().map(String::from).into(), b["evidence_url"].as_str().map(String::from).into()]).await?;
    let c = row(&st.db, "UPDATE grc_controls SET effectiveness=$2, last_tested_on=current_date, next_test_due = CASE WHEN $3 > 0 THEN current_date + $3 ELSE NULL END WHERE id=$1
        RETURNING id::text AS id, effectiveness, last_tested_on::text AS last_tested_on, next_test_due::text AS next_test_due", vec![id.into(), result.into(), days.into()]).await?;
    Ok(Json(json!({ "test": t, "control": c })))
}

async fn compliance_dashboard(State(st): State<AppState>, Query(p): Query<HashMap<String, String>>) -> AppResult<Json<Value>> {
    let area = p.get("area").cloned();
    let by_fw = rows(&st.db, "SELECT framework, count(*) AS requirements,
        count(*) FILTER (WHERE status='compliant') AS compliant, count(*) FILTER (WHERE status='partially_compliant') AS partial,
        count(*) FILTER (WHERE status='non_compliant') AS non_compliant, count(*) FILTER (WHERE status='not_assessed') AS not_assessed,
        round(100.0*count(*) FILTER (WHERE status='compliant')/NULLIF(count(*) FILTER (WHERE status<>'not_applicable'),0),1)::float8 AS compliance_pct
        FROM grc_compliance_requirements WHERE ($1::text IS NULL OR area=$1) GROUP BY framework ORDER BY framework", vec![area.clone().into()]).await?;
    let gaps = rows(&st.db, "SELECT id::text AS id, framework, reference, title, status, next_review_on::text AS next_review_on FROM grc_compliance_requirements
        WHERE ($1::text IS NULL OR area=$1) AND (status IN ('non_compliant','partially_compliant') OR next_review_on < current_date) ORDER BY status, next_review_on", vec![area.into()]).await?;
    Ok(Json(json!({ "by_framework": by_fw, "gaps_and_overdue_reviews": gaps })))
}

async fn audit_report(State(st): State<AppState>, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let a = row(&st.db, "SELECT to_jsonb(a) AS a FROM grc_audits a WHERE id=$1", vec![id.into()]).await?;
    if a.is_null() { return Err(AppError::NotFound(None)); }
    let sev = rows(&st.db, "SELECT severity, count(*) AS findings, count(*) FILTER (WHERE status IN ('verified','closed')) AS resolved FROM grc_audit_findings WHERE audit_id=$1 GROUP BY severity ORDER BY array_position(ARRAY['critical','major','minor','observation'], severity)", vec![id.into()]).await?;
    let f = rows(&st.db, "SELECT id::text AS id, title, severity, status, due_date::text AS due_date, (status NOT IN ('verified','closed') AND due_date < current_date) AS overdue FROM grc_audit_findings WHERE audit_id=$1 ORDER BY created_at", vec![id.into()]).await?;
    let open = f.iter().filter(|x| !matches!(x["status"].as_str(), Some("verified" | "closed"))).count();
    Ok(Json(json!({ "audit": a["a"], "findings_by_severity": sev, "findings": f, "open_findings": open, "ready_to_close": open == 0 })))
}
