//! Project management: CPM scheduling, resource utilization, cost tracking, project accounting, earned value.
use crate::db::{row, rows};
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Duration, NaiveDate};
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use serde_json::{json, Value};
use std::collections::HashMap;
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/pm/projects/{id}/schedule", post(schedule))
        .route("/api/pm/projects/{id}/costs", get(costs))
        .route("/api/pm/projects/{id}/accounting", get(accounting))
        .route("/api/pm/projects/{id}/analytics", get(evm))
        .route("/api/pm/resources/utilization", get(utilization))
        .route("/api/pm/analytics/portfolio", get(portfolio))
}

struct T { id: Uuid, dur: i64 }

/// Critical-path scheduling. Every dependency (FS/SS/FF/SF + lag) is expressed as a lower bound
/// `start_j >= start_i + w`, so the forward pass is a longest-path over the DAG and the backward pass
/// yields late dates, slack and the critical path. Cycles are rejected.
async fn schedule(State(st): State<AppState>, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let proj = row(&st.db, "SELECT start_date::text AS sd FROM pm_projects WHERE id=$1", vec![id.into()]).await?;
    if proj.is_null() { return Err(AppError::NotFound(None)); }
    let start: NaiveDate = proj["sd"].as_str().and_then(|s| s.parse().ok()).unwrap_or_else(|| chrono::Utc::now().date_naive());
    let tasks_j = rows(&st.db, "SELECT id::text AS id, name, duration_days::bigint AS dur FROM pm_tasks WHERE project_id=$1 AND status <> 'cancelled'", vec![id.into()]).await?;
    let tasks: Vec<T> = tasks_j.iter().map(|t| T { id: t["id"].as_str().unwrap().parse().unwrap(), dur: t["dur"].as_i64().unwrap_or(1).max(1) }).collect();
    let n = tasks.len();
    if n == 0 { return Ok(Json(json!({ "project_id": id, "tasks": [], "finish": null }))); }
    let idx: HashMap<Uuid, usize> = tasks.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
    let deps = rows(&st.db, "SELECT d.task_id::text AS succ, d.depends_on_task_id::text AS pred, d.dependency_type AS ty, d.lag_days::bigint AS lag
        FROM pm_task_dependencies d JOIN pm_tasks t ON t.id=d.task_id WHERE t.project_id=$1", vec![id.into()]).await?;
    // edges: (pred, succ, weight)
    let mut edges: Vec<(usize, usize, i64)> = vec![];
    for d in &deps {
        let (Some(&p), Some(&s)) = (idx.get(&d["pred"].as_str().unwrap().parse().unwrap()), idx.get(&d["succ"].as_str().unwrap().parse().unwrap())) else { continue };
        let lag = d["lag"].as_i64().unwrap_or(0);
        let w = match d["ty"].as_str().unwrap_or("FS") {
            "SS" => lag,
            "FF" => tasks[p].dur + lag - tasks[s].dur,
            "SF" => lag - tasks[s].dur + 1,
            _ => tasks[p].dur + lag, // FS
        };
        edges.push((p, s, w));
    }
    // topological order (Kahn)
    let mut indeg = vec![0usize; n];
    let mut adj: Vec<Vec<(usize, i64)>> = vec![vec![]; n];
    let mut radj: Vec<Vec<(usize, i64)>> = vec![vec![]; n];
    for &(p, s, w) in &edges { indeg[s] += 1; adj[p].push((s, w)); radj[s].push((p, w)); }
    let mut queue: Vec<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();
    let mut order = vec![];
    while let Some(u) = queue.pop() {
        order.push(u);
        for &(v, _) in &adj[u] { indeg[v] -= 1; if indeg[v] == 0 { queue.push(v); } }
    }
    if order.len() != n { return Err(AppError::Conflict("dependency cycle detected; schedule not possible".into())); }
    let mut es = vec![0i64; n];
    for &u in &order { for &(v, w) in &adj[u] { es[v] = es[v].max(es[u] + w); } }
    let finish = (0..n).map(|i| es[i] + tasks[i].dur - 1).max().unwrap();
    let mut ls: Vec<i64> = (0..n).map(|i| finish - tasks[i].dur + 1).collect();
    for &u in order.iter().rev() { for &(v, w) in &adj[u] { ls[u] = ls[u].min(ls[v] - w); } }
    let _ = radj;

    let tx = st.db.begin().await?;
    let mut out = vec![];
    for i in 0..n {
        let s = start + Duration::days(es[i]);
        let e = start + Duration::days(es[i] + tasks[i].dur - 1);
        tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "UPDATE pm_tasks SET planned_start=$2, planned_end=$3 WHERE id=$1", [tasks[i].id.into(), s.into(), e.into()])).await?;
        let slack = ls[i] - es[i];
        let name = tasks_j[i]["name"].clone();
        out.push(json!({ "task_id": tasks[i].id, "name": name, "planned_start": s, "planned_end": e,
            "late_start": start + Duration::days(ls[i]), "slack_days": slack, "critical": slack == 0 }));
    }
    tx.commit().await?;
    out.sort_by(|a, b| a["planned_start"].as_str().cmp(&b["planned_start"].as_str()));
    let crit: Vec<&Value> = out.iter().filter(|t| t["critical"] == true).collect();
    Ok(Json(json!({ "project_id": id, "start": start, "finish": start + Duration::days(finish),
        "duration_days": finish + 1, "critical_path": crit, "tasks": out })))
}

#[derive(serde::Deserialize)]
struct Range { from: Option<String>, to: Option<String> }

/// Weekly planned load per resource versus capacity.
async fn utilization(State(st): State<AppState>, Query(r): Query<Range>) -> AppResult<Json<Value>> {
    let from = r.from.unwrap_or_else(|| chrono::Utc::now().date_naive().to_string());
    let to = r.to.unwrap_or_else(|| (chrono::Utc::now().date_naive() + Duration::days(83)).to_string());
    let data = rows(&st.db, "WITH weeks AS (SELECT generate_series(date_trunc('week', $1::date), date_trunc('week', $2::date), interval '7 days')::date AS wk)
        SELECT r.id::text AS resource_id, r.name, r.capacity_hours_per_week::float8 AS capacity, w.wk::text AS week_start,
               COALESCE(sum(a.hours_per_week),0)::float8 AS planned_hours,
               round(100*COALESCE(sum(a.hours_per_week),0)/NULLIF(r.capacity_hours_per_week,0),1)::float8 AS utilization_pct,
               COALESCE(sum(a.hours_per_week),0) > r.capacity_hours_per_week AS overallocated
        FROM pm_resources r CROSS JOIN weeks w
        LEFT JOIN pm_allocations a ON a.resource_id=r.id AND a.start_date < w.wk + 7 AND a.end_date >= w.wk
        WHERE r.is_active GROUP BY r.id, r.name, r.capacity_hours_per_week, w.wk ORDER BY r.name, w.wk",
        vec![from.clone().into(), to.clone().into()]).await.map_err(|e| match e { AppError::Internal(m) => AppError::BadRequest(m), o => o })?;
    let over = data.iter().filter(|d| d["overallocated"] == true).count();
    Ok(Json(json!({ "from": from, "to": to, "overallocated_weeks": over, "data": data })))
}

async fn cost_totals(db: &sea_orm::DatabaseConnection, id: Uuid) -> AppResult<Value> {
    row(db, "SELECT
        COALESCE((SELECT sum(hours*COALESCE(cost_rate,0)) FROM pm_time_entries WHERE project_id=$1),0)::float8 AS labor_actual,
        COALESCE((SELECT sum(amount) FROM pm_cost_entries WHERE project_id=$1 AND cost_type='actual'),0)::float8 AS other_actual,
        COALESCE((SELECT sum(amount) FROM pm_cost_entries WHERE project_id=$1 AND cost_type='committed'),0)::float8 AS committed,
        COALESCE((SELECT sum(amount) FROM pm_cost_entries WHERE project_id=$1 AND cost_type='forecast'),0)::float8 AS forecast,
        COALESCE((SELECT budget FROM pm_projects WHERE id=$1),0)::float8 AS budget", vec![id.into()]).await
}

async fn costs(State(st): State<AppState>, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let t = cost_totals(&st.db, id).await?;
    let f = |k: &str| t[k].as_f64().unwrap_or(0.0);
    let actual = f("labor_actual") + f("other_actual");
    let by_cat = rows(&st.db, "SELECT category, sum(amount)::float8 AS amount FROM pm_cost_entries WHERE project_id=$1 AND cost_type='actual' GROUP BY category
        UNION ALL SELECT 'labor (time entries)', COALESCE(sum(hours*COALESCE(cost_rate,0)),0)::float8 FROM pm_time_entries WHERE project_id=$1 ORDER BY 2 DESC", vec![id.into()]).await?;
    let budget = f("budget");
    Ok(Json(json!({ "project_id": id, "budget": budget, "actual_cost": actual, "committed": f("committed"),
        "forecast_additional": f("forecast"), "estimate_at_completion": actual + f("committed") + f("forecast"),
        "budget_remaining": budget - actual - f("committed"),
        "budget_used_pct": if budget > 0.0 { Some((actual / budget * 1000.0).round() / 10.0) } else { None },
        "labor_actual": f("labor_actual"), "other_actual": f("other_actual"), "by_category": by_cat })))
}

async fn accounting(State(st): State<AppState>, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let p = row(&st.db, "SELECT billing_type, contract_value::float8 AS contract_value FROM pm_projects WHERE id=$1", vec![id.into()]).await?;
    if p.is_null() { return Err(AppError::NotFound(None)); }
    let c = cost_totals(&st.db, id).await?;
    let inv = row(&st.db, "SELECT COALESCE(sum(amount+tax) FILTER (WHERE status<>'void'),0)::float8 AS invoiced,
        COALESCE(sum(amount+tax) FILTER (WHERE status='paid'),0)::float8 AS collected,
        COALESCE(sum(amount+tax) FILTER (WHERE status='sent'),0)::float8 AS receivable,
        COALESCE(sum(amount+tax) FILTER (WHERE status='sent' AND due_date < current_date),0)::float8 AS overdue
        FROM pm_invoices WHERE project_id=$1", vec![id.into()]).await?;
    let tm = row(&st.db, "SELECT COALESCE(sum(hours*COALESCE(bill_rate,0)) FILTER (WHERE billable),0)::float8 AS billable_value FROM pm_time_entries WHERE project_id=$1", vec![id.into()]).await?;
    let cost = c["labor_actual"].as_f64().unwrap_or(0.0) + c["other_actual"].as_f64().unwrap_or(0.0);
    let invoiced = inv["invoiced"].as_f64().unwrap_or(0.0);
    let (earned, basis) = match p["billing_type"].as_str().unwrap_or("") {
        "fixed_price" => {
            let pc = row(&st.db, "SELECT COALESCE(sum(planned_cost*percent_complete)/NULLIF(sum(planned_cost),0),0)::float8 AS pc FROM pm_tasks WHERE project_id=$1", vec![id.into()]).await?;
            (p["contract_value"].as_f64().unwrap_or(0.0) * pc["pc"].as_f64().unwrap_or(0.0) / 100.0, "percent-complete of contract value")
        }
        "non_billable" => (0.0, "non-billable"),
        _ => (tm["billable_value"].as_f64().unwrap_or(0.0), "billable time at bill rates"),
    };
    let margin = earned - cost;
    Ok(Json(json!({ "project_id": id, "billing_type": p["billing_type"], "revenue_basis": basis, "earned_revenue": earned,
        "cost": cost, "gross_margin": margin, "gross_margin_pct": if earned > 0.0 { Some((margin / earned * 1000.0).round() / 10.0) } else { None },
        "invoicing": inv, "unbilled_wip": (earned - invoiced).max(0.0), "overbilled": (invoiced - earned).max(0.0) })))
}

/// Earned value management: PV, EV, AC, CPI, SPI, EAC...
async fn evm(State(st): State<AppState>, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let m = row(&st.db, "SELECT COALESCE(sum(planned_cost),0)::float8 AS bac,
        COALESCE(sum(planned_cost * CASE WHEN planned_start IS NULL OR planned_end IS NULL THEN 0
            ELSE LEAST(1, GREATEST(0, (current_date - planned_start + 1)::float8 / GREATEST(1, planned_end - planned_start + 1))) END),0)::float8 AS pv,
        COALESCE(sum(planned_cost * percent_complete / 100.0),0)::float8 AS ev,
        count(*) AS tasks, count(*) FILTER (WHERE status='completed') AS completed,
        count(*) FILTER (WHERE status='blocked') AS blocked,
        count(*) FILTER (WHERE status NOT IN ('completed','cancelled') AND planned_end < current_date) AS overdue,
        round(COALESCE(avg(percent_complete),0),1)::float8 AS avg_percent_complete
        FROM pm_tasks WHERE project_id=$1 AND status<>'cancelled'", vec![id.into()]).await?;
    let c = cost_totals(&st.db, id).await?;
    let (mut bac, mut pv, mut ev) = (m["bac"].as_f64().unwrap_or(0.0), m["pv"].as_f64().unwrap_or(0.0), m["ev"].as_f64().unwrap_or(0.0));
    // Projects without tasks (e.g. imported portfolio projects) are measured at project level:
    // BAC = budget, EV = budget x completion %, PV = budget x elapsed share of the planned window.
    let mut basis = "tasks";
    if m["tasks"].as_i64().unwrap_or(0) == 0 {
        let pj = row(&st.db, "SELECT COALESCE(budget,0)::float8 AS bac, COALESCE(percent_complete,0) AS pct,
            CASE WHEN start_date IS NULL OR end_date IS NULL THEN 0
                 ELSE LEAST(1, GREATEST(0, (current_date - start_date + 1)::float8 / GREATEST(1, end_date - start_date + 1))) END AS elapsed
            FROM pm_projects WHERE id=$1", vec![id.into()]).await?;
        if pj.is_null() { return Err(AppError::NotFound(None)); }
        bac = pj["bac"].as_f64().unwrap_or(0.0);
        ev = bac * pj["pct"].as_f64().unwrap_or(0.0) / 100.0;
        pv = bac * pj["elapsed"].as_f64().unwrap_or(0.0);
        basis = "project";
    }
    let ac = c["labor_actual"].as_f64().unwrap_or(0.0) + c["other_actual"].as_f64().unwrap_or(0.0);
    let r2 = |x: f64| (x * 100.0).round() / 100.0;
    let cpi = if ac > 0.0 { Some(r2(ev / ac)) } else { None };
    let spi = if pv > 0.0 { Some(r2(ev / pv)) } else { None };
    let eac = cpi.filter(|c| *c > 0.0).map(|c| r2(bac / c));
    Ok(Json(json!({ "project_id": id, "basis": basis, "bac": bac, "planned_value": r2(pv), "earned_value": r2(ev), "actual_cost": r2(ac),
        "cost_variance": r2(ev - ac), "schedule_variance": r2(ev - pv), "cpi": cpi, "spi": spi,
        "estimate_at_completion": eac, "estimate_to_complete": eac.map(|e| r2(e - ac)), "variance_at_completion": eac.map(|e| r2(bac - e)),
        "health": match (cpi, spi) { (Some(c), Some(s)) if c >= 0.95 && s >= 0.95 => "green", (Some(c), Some(s)) if c >= 0.8 && s >= 0.8 => "amber", (None, None) => "unknown", _ => "red" },
        "tasks": { "total": m["tasks"], "completed": m["completed"], "blocked": m["blocked"], "overdue": m["overdue"], "avg_percent_complete": m["avg_percent_complete"] } })))
}

async fn portfolio(State(st): State<AppState>) -> AppResult<Json<Value>> {
    let d = rows(&st.db, "SELECT p.id::text AS id, p.code, p.name, p.status, p.budget::float8 AS budget,
        COALESCE((SELECT sum(hours*COALESCE(cost_rate,0)) FROM pm_time_entries WHERE project_id=p.id),0)::float8 +
        COALESCE((SELECT sum(amount) FROM pm_cost_entries WHERE project_id=p.id AND cost_type='actual'),0)::float8 AS actual_cost,
        COALESCE((SELECT sum(planned_cost*percent_complete/100.0) FROM pm_tasks WHERE project_id=p.id),0)::float8 AS earned_value,
        (SELECT count(*) FROM pm_tasks WHERE project_id=p.id AND status NOT IN ('completed','cancelled') AND planned_end < current_date) AS overdue_tasks
        FROM pm_projects p ORDER BY p.created_at DESC", vec![]).await?;
    Ok(Json(json!({ "data": d })))
}
