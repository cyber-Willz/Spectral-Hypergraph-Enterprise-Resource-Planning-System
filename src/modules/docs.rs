//! Document management: immutable version control, diff/rollback, approval workflows, records retention.
use crate::db::{row, rows};
use crate::error::{AppError, AppResult};
use crate::state::{AppState, AuthUser};
use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/docs/documents/{id}/versions", get(list_versions).post(new_version))
        .route("/api/docs/documents/{id}/versions/{n}", get(get_version))
        .route("/api/docs/documents/{id}/rollback", post(rollback))
        .route("/api/docs/documents/{id}/diff", get(diff))
        .route("/api/docs/documents/{id}/workflow", post(start_workflow))
        .route("/api/docs/workflow-tasks/inbox", get(inbox))
        .route("/api/docs/workflow-tasks/{id}/decide", post(decide))
        .route("/api/docs/records/disposition-due", get(disposition_due))
        .route("/api/docs/records/{id}/dispose", post(dispose))
}

fn opt(b: &Value, k: &str) -> Option<String> { b[k].as_str().map(String::from) }

async fn insert_version(tx: &impl ConnectionTrait, doc: Uuid, user: Uuid, summary: Option<String>, file_name: Option<String>,
    mime: Option<String>, content: Option<String>, uri: Option<String>) -> AppResult<Value> {
    let d = row(tx, "SELECT status FROM doc_documents WHERE id=$1 FOR UPDATE", vec![doc.into()]).await?;
    if d.is_null() { return Err(AppError::NotFound(None)); }
    if content.is_none() && uri.is_none() { return Err(AppError::BadRequest("content_text or storage_uri required".into())); }
    let n = row(tx, "SELECT COALESCE(max(version_number),0)::int + 1 AS n FROM doc_versions WHERE document_id=$1", vec![doc.into()]).await?["n"].as_i64().unwrap_or(1) as i32;
    let basis = content.clone().unwrap_or_else(|| uri.clone().unwrap_or_default());
    let hash = hex::encode(Sha256::digest(basis.as_bytes()));
    let size = content.as_ref().map(|c| c.len() as i64);
    let v = row(tx, "INSERT INTO doc_versions (document_id, version_number, change_summary, file_name, mime_type, size_bytes, content_hash, content_text, storage_uri, created_by)
        VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) RETURNING id::text AS id, version_number, content_hash, size_bytes, change_summary, created_at",
        vec![doc.into(), n.into(), summary.into(), file_name.into(), mime.into(), size.into(), hash.into(), content.into(), uri.into(), user.into()]).await?;
    // a changed controlled document must be re-approved
    tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE doc_documents SET current_version=$2, status=CASE WHEN status IN ('approved','effective','in_review') THEN 'draft' ELSE status END WHERE id=$1", [doc.into(), n.into()])).await?;
    Ok(v)
}

async fn new_version(Extension(u): Extension<AuthUser>, State(st): State<AppState>, Path(id): Path<Uuid>, Json(b): Json<Value>) -> AppResult<(axum::http::StatusCode, Json<Value>)> {
    let tx = st.db.begin().await?;
    let v = insert_version(&tx, id, u.id, opt(&b, "change_summary"), opt(&b, "file_name"), opt(&b, "mime_type"), opt(&b, "content_text"), opt(&b, "storage_uri")).await?;
    tx.commit().await?;
    Ok((axum::http::StatusCode::CREATED, Json(v)))
}

async fn list_versions(State(st): State<AppState>, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let v = rows(&st.db, "SELECT v.id::text AS id, version_number, change_summary, file_name, size_bytes, content_hash, u.email AS created_by, v.created_at
        FROM doc_versions v LEFT JOIN users u ON u.id=v.created_by WHERE document_id=$1 ORDER BY version_number DESC", vec![id.into()]).await?;
    Ok(Json(json!({ "data": v })))
}

async fn get_version(State(st): State<AppState>, Path((id, n)): Path<(Uuid, i32)>) -> AppResult<Json<Value>> {
    let v = row(&st.db, "SELECT to_jsonb(v) AS v FROM doc_versions v WHERE document_id=$1 AND version_number=$2", vec![id.into(), n.into()]).await?;
    if v.is_null() { return Err(AppError::NotFound(None)); }
    Ok(Json(v["v"].clone()))
}

/// Rollback never rewrites history: it publishes a new version whose content equals the chosen one.
async fn rollback(Extension(u): Extension<AuthUser>, State(st): State<AppState>, Path(id): Path<Uuid>, Json(b): Json<Value>) -> AppResult<Json<Value>> {
    let n = b["version_number"].as_i64().ok_or_else(|| AppError::BadRequest("version_number required".into()))? as i32;
    let tx = st.db.begin().await?;
    let src = row(&tx, "SELECT file_name, mime_type, content_text, storage_uri FROM doc_versions WHERE document_id=$1 AND version_number=$2", vec![id.into(), n.into()]).await?;
    if src.is_null() { return Err(AppError::NotFound(None)); }
    let reason = opt(&b, "reason").map(|r| format!(": {r}")).unwrap_or_default();
    let v = insert_version(&tx, id, u.id, Some(format!("Rollback to version {n}{reason}")), opt(&src, "file_name"), opt(&src, "mime_type"), opt(&src, "content_text"), opt(&src, "storage_uri")).await?;
    tx.commit().await?;
    Ok(Json(v))
}

fn line_diff(a: &[&str], b: &[&str]) -> Vec<Value> {
    let (n, m) = (a.len(), b.len());
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() { for j in (0..m).rev() {
        dp[i][j] = if a[i] == b[j] { dp[i + 1][j + 1] + 1 } else { dp[i + 1][j].max(dp[i][j + 1]) };
    } }
    let (mut i, mut j, mut out) = (0, 0, vec![]);
    while i < n && j < m {
        if a[i] == b[j] { out.push(json!({ "op": "equal", "text": a[i] })); i += 1; j += 1; }
        else if dp[i + 1][j] >= dp[i][j + 1] { out.push(json!({ "op": "remove", "text": a[i] })); i += 1; }
        else { out.push(json!({ "op": "add", "text": b[j] })); j += 1; }
    }
    while i < n { out.push(json!({ "op": "remove", "text": a[i] })); i += 1; }
    while j < m { out.push(json!({ "op": "add", "text": b[j] })); j += 1; }
    out
}

async fn diff(State(st): State<AppState>, Path(id): Path<Uuid>, Query(p): Query<HashMap<String, String>>) -> AppResult<Json<Value>> {
    let get = |k: &str| p.get(k).and_then(|v| v.parse::<i32>().ok()).ok_or_else(|| AppError::BadRequest(format!("query param '{k}' (version number) required")));
    let (from, to) = (get("from")?, get("to")?);
    let fetch = |n: i32| { let db = st.db.clone(); async move {
        let r = row(&db, "SELECT content_text FROM doc_versions WHERE document_id=$1 AND version_number=$2", vec![id.into(), n.into()]).await?;
        if r.is_null() { return Err(AppError::NotFound(None)); }
        Ok::<String, AppError>(r["content_text"].as_str().unwrap_or_default().to_string())
    } };
    let (a, b) = (fetch(from).await?, fetch(to).await?);
    let (al, bl): (Vec<&str>, Vec<&str>) = (a.lines().collect(), b.lines().collect());
    if al.len() * bl.len() > 4_000_000 { return Err(AppError::BadRequest("documents too large to diff".into())); }
    let d = line_diff(&al, &bl);
    let c = |op: &str| d.iter().filter(|x| x["op"] == op).count();
    Ok(Json(json!({ "from": from, "to": to, "added": c("add"), "removed": c("remove"), "unchanged": c("equal"), "diff": d })))
}

async fn start_workflow(Extension(u): Extension<AuthUser>, State(st): State<AppState>, Path(id): Path<Uuid>, Json(b): Json<Value>) -> AppResult<Json<Value>> {
    let tx = st.db.begin().await?;
    let d = row(&tx, "SELECT current_version, status, doc_type FROM doc_documents WHERE id=$1 FOR UPDATE", vec![id.into()]).await?;
    if d.is_null() { return Err(AppError::NotFound(None)); }
    if d["current_version"].as_i64().unwrap_or(0) == 0 { return Err(AppError::Unprocessable("document has no version to review".into())); }
    if !row(&tx, "SELECT 1 AS x FROM doc_workflow_instances WHERE document_id=$1 AND status='in_progress'", vec![id.into()]).await?.is_null() {
        return Err(AppError::Conflict("a workflow is already in progress for this document".into()));
    }
    let wf = match b["workflow_id"].as_str().and_then(|s| s.parse::<Uuid>().ok()) {
        Some(w) => row(&tx, "SELECT id::text AS id, steps FROM doc_workflows WHERE id=$1 AND is_active", vec![w.into()]).await?,
        None => row(&tx, "SELECT id::text AS id, steps FROM doc_workflows WHERE is_active AND (doc_type IS NULL OR doc_type=$1) ORDER BY (doc_type IS NULL), created_at LIMIT 1", vec![d["doc_type"].as_str().unwrap_or("").to_string().into()]).await?,
    };
    if wf.is_null() { return Err(AppError::NotFound(None)); }
    let steps = wf["steps"].as_array().cloned().unwrap_or_default();
    let first = steps.first().ok_or_else(|| AppError::Unprocessable("workflow has no steps".into()))?;
    let wid: Uuid = wf["id"].as_str().unwrap().parse().unwrap();
    let inst = row(&tx, "INSERT INTO doc_workflow_instances (workflow_id, document_id, version_number, started_by) VALUES ($1,$2,$3,$4) RETURNING id::text AS id",
        vec![wid.into(), id.into(), (d["current_version"].as_i64().unwrap() as i32).into(), u.id.into()]).await?;
    let iid: Uuid = inst["id"].as_str().unwrap().parse().unwrap();
    tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO doc_workflow_tasks (instance_id, step_order, step_name, assigned_role) VALUES ($1,1,$2,$3)",
        [iid.into(), first["name"].as_str().unwrap_or("Step 1").to_string().into(), opt(first, "role").into()])).await?;
    tx.execute(Statement::from_sql_and_values(DbBackend::Postgres, "UPDATE doc_documents SET status='in_review' WHERE id=$1", [id.into()])).await?;
    tx.commit().await?;
    Ok(Json(json!({ "instance_id": iid, "document_id": id, "steps": steps.len(), "current_step": first["name"], "assigned_role": first["role"] })))
}

async fn inbox(Extension(u): Extension<AuthUser>, State(st): State<AppState>) -> AppResult<Json<Value>> {
    let r = rows(&st.db, "SELECT t.id::text AS task_id, t.step_name, t.assigned_role, d.id::text AS document_id, d.doc_number, d.title, i.version_number, t.created_at
        FROM doc_workflow_tasks t JOIN doc_workflow_instances i ON i.id=t.instance_id JOIN doc_documents d ON d.id=i.document_id
        WHERE t.status='pending' AND ($1='admin' OR t.assigned_role=$1 OR t.assignee_id=$2) ORDER BY t.created_at",
        vec![u.role.clone().into(), u.id.into()]).await?;
    Ok(Json(json!({ "data": r })))
}

async fn decide(Extension(u): Extension<AuthUser>, State(st): State<AppState>, Path(id): Path<Uuid>, Json(b): Json<Value>) -> AppResult<Json<Value>> {
    let approve = b["approve"].as_bool().ok_or_else(|| AppError::BadRequest("approve (bool) required".into()))?;
    let tx = st.db.begin().await?;
    let t = row(&tx, "SELECT t.status, t.step_order, t.assigned_role, t.assignee_id::text AS assignee, t.instance_id::text AS iid, i.document_id::text AS doc,
        i.started_by::text AS started_by, i.workflow_id::text AS wf FROM doc_workflow_tasks t JOIN doc_workflow_instances i ON i.id=t.instance_id WHERE t.id=$1 FOR UPDATE OF t", vec![id.into()]).await?;
    if t.is_null() { return Err(AppError::NotFound(None)); }
    if t["status"] != "pending" { return Err(AppError::Conflict("task already decided".into())); }
    let authorised = u.role == "admin" || t["assigned_role"].as_str() == Some(u.role.as_str()) || t["assignee"].as_str() == Some(&u.id.to_string());
    if !authorised { return Err(AppError::Forbidden("you are not an assigned approver for this step".into())); }
    if t["started_by"].as_str() == Some(&u.id.to_string()) && u.role != "admin" {
        return Err(AppError::Forbidden("segregation of duties: you cannot approve a workflow you started".into()));
    }
    let (iid, doc): (Uuid, Uuid) = (t["iid"].as_str().unwrap().parse().unwrap(), t["doc"].as_str().unwrap().parse().unwrap());
    tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE doc_workflow_tasks SET status=$2, decided_by=$3, decided_at=now(), comments=$4 WHERE id=$1",
        [id.into(), (if approve { "approved" } else { "rejected" }).into(), u.id.into(), opt(&b, "comments").into()])).await?;
    let order = t["step_order"].as_i64().unwrap_or(1);
    let outcome;
    if !approve {
        tx.execute(Statement::from_sql_and_values(DbBackend::Postgres, "UPDATE doc_workflow_instances SET status='rejected', completed_at=now() WHERE id=$1", [iid.into()])).await?;
        tx.execute(Statement::from_sql_and_values(DbBackend::Postgres, "UPDATE doc_documents SET status='draft' WHERE id=$1", [doc.into()])).await?;
        outcome = json!({ "workflow": "rejected", "document_status": "draft" });
    } else {
        let wf: Uuid = t["wf"].as_str().unwrap().parse().unwrap();
        let steps = row(&tx, "SELECT steps FROM doc_workflows WHERE id=$1", vec![wf.into()]).await?["steps"].as_array().cloned().unwrap_or_default();
        if let Some(next) = steps.get(order as usize) {
            tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
                "INSERT INTO doc_workflow_tasks (instance_id, step_order, step_name, assigned_role) VALUES ($1,$2,$3,$4)",
                [iid.into(), ((order + 1) as i32).into(), next["name"].as_str().unwrap_or("Step").to_string().into(), opt(next, "role").into()])).await?;
            tx.execute(Statement::from_sql_and_values(DbBackend::Postgres, "UPDATE doc_workflow_instances SET current_step=$2 WHERE id=$1", [iid.into(), ((order + 1) as i32).into()])).await?;
            outcome = json!({ "workflow": "in_progress", "next_step": next["name"], "assigned_role": next["role"] });
        } else {
            tx.execute(Statement::from_sql_and_values(DbBackend::Postgres, "UPDATE doc_workflow_instances SET status='approved', completed_at=now() WHERE id=$1", [iid.into()])).await?;
            tx.execute(Statement::from_sql_and_values(DbBackend::Postgres, "UPDATE doc_documents SET status='approved', effective_date=COALESCE(effective_date, current_date) WHERE id=$1", [doc.into()])).await?;
            outcome = json!({ "workflow": "approved", "document_status": "approved" });
        }
    }
    tx.commit().await?;
    Ok(Json(outcome))
}

const DUE_SQL: &str = "SELECT d.id::text AS id, d.doc_number, d.title, p.name AS policy, p.disposition_action AS action,
    (COALESCE(d.effective_date, d.created_at::date) + make_interval(years => p.retention_years))::date::text AS eligible_on
    FROM doc_documents d JOIN doc_retention_policies p ON p.id=d.retention_policy_id
    WHERE d.is_record AND NOT d.legal_hold AND d.disposed_at IS NULL
      AND (COALESCE(d.effective_date, d.created_at::date) + make_interval(years => p.retention_years)) <= current_date";

async fn disposition_due(State(st): State<AppState>) -> AppResult<Json<Value>> {
    let r = rows(&st.db, &format!("{DUE_SQL} ORDER BY eligible_on"), vec![]).await?;
    let held = row(&st.db, "SELECT count(*) AS on_legal_hold FROM doc_documents WHERE legal_hold", vec![]).await?;
    Ok(Json(json!({ "due": r, "legal_holds": held["on_legal_hold"] })))
}

/// Executes the retention policy's disposition for a record that has reached end of retention.
async fn dispose(State(st): State<AppState>, Path(id): Path<Uuid>, Json(b): Json<Value>) -> AppResult<Json<Value>> {
    let tx = st.db.begin().await?;
    let r = row(&tx, &format!("{DUE_SQL} AND d.id=$1 FOR UPDATE OF d"), vec![id.into()]).await?;
    if r.is_null() { return Err(AppError::Conflict("document is not a record eligible for disposition (retention not elapsed, legal hold, or already disposed)".into())); }
    let action = r["action"].as_str().unwrap_or("archive");
    if action == "review" && b["confirm"].as_bool() != Some(true) {
        return Err(AppError::Conflict("policy requires review: resubmit with {\"confirm\": true}".into()));
    }
    if action == "destroy" {
        tx.execute(Statement::from_sql_and_values(DbBackend::Postgres, "UPDATE doc_documents SET is_record=false WHERE id=$1", [id.into()])).await?;
        tx.execute(Statement::from_sql_and_values(DbBackend::Postgres, "DELETE FROM doc_documents WHERE id=$1", [id.into()])).await?;
    } else {
        tx.execute(Statement::from_sql_and_values(DbBackend::Postgres, "UPDATE doc_documents SET status='archived', disposed_at=now() WHERE id=$1", [id.into()])).await?;
    }
    tx.commit().await?;
    Ok(Json(json!({ "document": r["doc_number"], "disposition": if action == "destroy" { "destroyed" } else { "archived" } })))
}
