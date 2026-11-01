//! Project portfolio: CSV import, portfolio analytics, finish-or-stop rankings, risk flags and
//! reference-class benchmarks. Portfolio scope = projects that carry a business case (`expected_benefit` set).
use crate::db::{row, rows};
use crate::error::{AppError, AppResult};
use crate::state::{AppState, AuthUser};
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::NaiveDate;
use sea_orm::{DatabaseConnection, TransactionTrait, Value};
use serde_json::{json, Value as J};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/pm/import/projects", post(import_projects).layer(DefaultBodyLimit::max(10 * 1024 * 1024)))
        .route("/api/pm/portfolio/summary", get(summary))
        .route("/api/pm/portfolio/by/{dimension}", get(by_dimension))
        .route("/api/pm/portfolio/rankings", get(rankings))
        .route("/api/pm/portfolio/risk-flags", get(risk_flags))
        .route("/api/pm/portfolio/timeline", get(timeline))
        .route("/api/pm/portfolio/benchmarks", get(benchmarks))
        .route("/api/pm/portfolio/estimate", post(estimate))
}

// ------------------------------------------------------------------ import

fn norm_key(s: &str) -> String { s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_lowercase() }

fn parse_money(s: &str) -> Option<f64> {
    let t: String = s.chars().filter(|c| !c.is_whitespace() && *c != ',' && *c != '$').collect();
    t.parse::<f64>().ok().filter(|v| *v >= 0.0)
}

fn parse_date(s: &str) -> Option<NaiveDate> {
    ["%m/%d/%Y", "%Y-%m-%d", "%d-%b-%Y"].iter().find_map(|f| NaiveDate::parse_from_str(s.trim(), f).ok())
}

fn map_status(s: &str) -> Option<&'static str> {
    match norm_key(s).as_str() {
        "inprogress" | "active" => Some("active"),
        "completed" | "complete" => Some("completed"),
        "onhold" => Some("on_hold"),
        "cancelled" | "canceled" => Some("cancelled"),
        "planning" | "notstarted" => Some("planning"),
        _ => None,
    }
}

fn parse_phase(s: &str) -> Option<(i32, String)> {
    let t = s.trim();
    let rest = t.strip_prefix("Phase")?.trim_start();
    let n: i32 = rest.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().ok()?;
    let name = rest.trim_start_matches(|c: char| c.is_ascii_digit()).trim_start_matches([' ', '-']).trim().to_string();
    (1..=5).contains(&n).then_some((n, name))
}

fn project_code(name: &str, start: NaiveDate) -> String {
    let slug: String = name.to_uppercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect::<String>()
        .split('-').filter(|x| !x.is_empty()).collect::<Vec<_>>().join("-");
    let h = hex::encode(Sha256::digest(format!("{}|{}", name.trim().to_lowercase(), start).as_bytes()));
    format!("PMD-{}-{}-{}", start.format("%Y%m"), &slug[..slug.len().min(20)], &h[..4].to_uppercase())
}

/// Idempotent project import (upsert by deterministic project code). Valid rows are imported and invalid rows
/// are reported with their line number; with `strict` any invalid row aborts the whole import.
pub async fn import_csv(db: &DatabaseConnection, text: &str, filename: &str, user: Option<Uuid>, strict: bool) -> AppResult<J> {
    let text = text.trim_start_matches('\u{feff}');
    let mut rdr = csv::ReaderBuilder::new().trim(csv::Trim::All).flexible(true).from_reader(text.as_bytes());
    let headers: Vec<String> = rdr.headers().map_err(|e| AppError::BadRequest(format!("unreadable CSV header: {e}")))?.iter().map(norm_key).collect();
    for req in ["projectname", "projectcost", "projectbenefit", "status", "startdate", "enddate"] {
        if !headers.iter().any(|h| h == req) { return Err(AppError::BadRequest(format!("missing required column '{req}'"))); }
    }
    let col = |rec: &csv::StringRecord, k: &str| -> Option<String> {
        headers.iter().position(|h| h == k).and_then(|i| rec.get(i)).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    };
    struct R { code: String, name: String, desc: Option<String>, status: &'static str, start: NaiveDate, end: NaiveDate, cost: f64, benefit: f64,
        ptype: Option<String>, region: Option<String>, dept: Option<String>, cx: Option<String>, phase: Option<(i32, String)>, pct: Option<i32>, mgr: Option<String> }
    let (mut good, mut errors, mut warnings): (Vec<R>, Vec<J>, Vec<J>) = (vec![], vec![], vec![]);
    let mut seen = HashSet::new();
    let mut total = 0;
    for rec in rdr.records() {
        total += 1;
        let rec = match rec { Ok(r) => r, Err(e) => { errors.push(json!({ "line": null, "error": e.to_string() })); continue } };
        let line = rec.position().map(|p| p.line()).unwrap_or(0);
        let mut bad = |m: &str| errors.push(json!({ "line": line, "error": m }));
        let Some(name) = col(&rec, "projectname") else { bad("project name is empty"); continue };
        let Some(start) = col(&rec, "startdate").and_then(|s| parse_date(&s)) else { bad("invalid or missing start date"); continue };
        let Some(end) = col(&rec, "enddate").and_then(|s| parse_date(&s)) else { bad("invalid or missing end date"); continue };
        if end < start { bad("end date is before start date"); continue; }
        let Some(cost) = col(&rec, "projectcost").and_then(|s| parse_money(&s)) else { bad("invalid project cost"); continue };
        let Some(benefit) = col(&rec, "projectbenefit").and_then(|s| parse_money(&s)) else { bad("invalid project benefit"); continue };
        let Some(status) = col(&rec, "status").as_deref().and_then(map_status) else { bad("unknown status"); continue };
        let cx = match col(&rec, "complexity") {
            None => None,
            Some(c) if ["low", "medium", "high"].contains(&c.to_lowercase().as_str()) => Some(c.to_lowercase()),
            Some(_) => { bad("complexity must be Low, Medium or High"); continue }
        };
        let pct = match col(&rec, "completion") {
            None => None,
            Some(p) => match p.trim_end_matches('%').trim().parse::<i32>() { Ok(v) if (0..=100).contains(&v) => Some(v), _ => { bad("completion % must be 0-100"); continue } },
        };
        let phase = match col(&rec, "phase") { None => None, Some(p) => match parse_phase(&p) { Some(x) => Some(x), None => { bad("phase must look like 'Phase 3 - Plan'"); continue } } };
        let code = project_code(&name, start);
        if !seen.insert(code.clone()) { bad("duplicate project (same name and start date) in file"); continue; }
        if let (Some(y), Some(m)) = (col(&rec, "year").and_then(|s| s.parse::<i32>().ok()), col(&rec, "month").and_then(|s| s.parse::<u32>().ok())) {
            if (y, m) != (chrono::Datelike::year(&start), chrono::Datelike::month(&start)) { warnings.push(json!({ "line": line, "warning": "Year/Month columns disagree with start date; start date used" })); }
        }
        good.push(R { code, name, desc: col(&rec, "projectdescription"), status, start, end, cost, benefit, ptype: col(&rec, "projecttype").map(|s| s.to_uppercase()),
            region: col(&rec, "region"), dept: col(&rec, "department"), cx, phase, pct, mgr: col(&rec, "projectmanager") });
    }
    if strict && !errors.is_empty() {
        return Err(AppError::Unprocessable(format!("strict import aborted: {} invalid row(s); first: {}", errors.len(), errors[0])));
    }
    let tx = db.begin().await?;
    let mut mgr_ids: HashMap<String, Uuid> = HashMap::new();
    let (mut inserted, mut updated) = (0, 0);
    for r in &good {
        let mgr_id: Option<Uuid> = match &r.mgr {
            None => None,
            Some(m) => {
                if let Some(id) = mgr_ids.get(&m.to_lowercase()) { Some(*id) } else {
                    let found = row(&tx, "SELECT id::text AS id FROM pm_resources WHERE lower(name)=lower($1) AND role='Project Manager' LIMIT 1", vec![m.clone().into()]).await?;
                    let id: Uuid = match found["id"].as_str() {
                        Some(s) => s.parse().unwrap(),
                        None => row(&tx, "INSERT INTO pm_resources (name, kind, role) VALUES ($1,'person','Project Manager') RETURNING id::text AS id", vec![m.clone().into()]).await?["id"].as_str().unwrap().parse().unwrap(),
                    };
                    mgr_ids.insert(m.to_lowercase(), id);
                    Some(id)
                }
            }
        };
        let res = row(&tx, "INSERT INTO pm_projects (code,name,description,status,start_date,end_date,budget,billing_type,project_type,region,department,expected_benefit,complexity,phase_no,phase_name,percent_complete,project_manager_id,import_source)
            VALUES ($1,$2,$3,$4,$5::date,$6::date,$7::text::numeric,'non_billable',$8,$9,$10,$11::text::numeric,$12,$13,$14,$15,$16,$17)
            ON CONFLICT (code) DO UPDATE SET name=EXCLUDED.name, description=EXCLUDED.description, status=EXCLUDED.status, start_date=EXCLUDED.start_date, end_date=EXCLUDED.end_date,
              budget=EXCLUDED.budget, project_type=EXCLUDED.project_type, region=EXCLUDED.region, department=EXCLUDED.department, expected_benefit=EXCLUDED.expected_benefit,
              complexity=EXCLUDED.complexity, phase_no=EXCLUDED.phase_no, phase_name=EXCLUDED.phase_name, percent_complete=EXCLUDED.percent_complete,
              project_manager_id=EXCLUDED.project_manager_id, import_source=EXCLUDED.import_source
            RETURNING (xmax = 0) AS inserted", vec![
            r.code.clone().into(), r.name.clone().into(), r.desc.clone().into(), r.status.into(), r.start.to_string().into(), r.end.to_string().into(),
            format!("{:.2}", r.cost).into(), r.ptype.clone().into(), r.region.clone().into(), r.dept.clone().into(), format!("{:.2}", r.benefit).into(),
            r.cx.clone().into(), r.phase.as_ref().map(|p| p.0).into(), r.phase.as_ref().map(|p| p.1.clone()).into(), r.pct.into(), mgr_id.into(), filename.to_string().into()]).await?;
        if res["inserted"] == true { inserted += 1 } else { updated += 1 }
    }
    let batch = row(&tx, "INSERT INTO pm_import_batches (source_name, filename, rows_total, rows_inserted, rows_updated, rows_rejected, errors, created_by)
        VALUES ('projects_csv',$1,$2,$3,$4,$5,$6::text::jsonb,$7) RETURNING id::text AS id",
        vec![filename.to_string().into(), (total as i32).into(), (inserted as i32).into(), (updated as i32).into(), (errors.len() as i32).into(), json!(errors).to_string().into(), user.into()]).await?;
    tx.commit().await?;
    Ok(json!({ "batch_id": batch["id"], "rows": total, "inserted": inserted, "updated": updated, "rejected": errors.len(),
        "project_managers": mgr_ids.len(), "errors": errors, "warnings": warnings }))
}

async fn import_projects(Extension(u): Extension<AuthUser>, State(st): State<AppState>, Query(p): Query<HashMap<String, String>>, body: String) -> AppResult<Json<J>> {
    u.require(&["admin", "manager", "user"])?;
    let strict = p.get("strict").map(|v| v == "true").unwrap_or(false);
    Ok(Json(import_csv(&st.db, &body, p.get("filename").map(String::as_str).unwrap_or("upload.csv"), Some(u.id), strict).await?))
}

// --------------------------------------------------------------- analytics

const SCOPE: &str = "p.expected_benefit IS NOT NULL";
const FROM: &str = "FROM pm_projects p LEFT JOIN pm_resources m ON m.id = p.project_manager_id";

const DIMS: &[(&str, &str)] = &[("project_type", "p.project_type"), ("region", "p.region"), ("department", "p.department"),
    ("complexity", "p.complexity"), ("status", "p.status"), ("manager", "m.name")];

/// Builds " AND ..." filters from whitelisted query parameters (values are always bound, never interpolated).
fn filters(p: &HashMap<String, String>) -> (String, Vec<Value>) {
    let (mut sql, mut vals) = (String::new(), vec![]);
    for (k, col) in DIMS {
        if let Some(v) = p.get(*k) {
            vals.push(v.clone().into());
            sql += &format!(" AND lower({col}) = lower(${})", vals.len());
        }
    }
    if let Some(v) = p.get("phase_no") { vals.push(v.clone().into()); sql += &format!(" AND p.phase_no = ${}::int", vals.len()); }
    if let Some(v) = p.get("year") { vals.push(v.clone().into()); sql += &format!(" AND extract(year from p.start_date) = ${}::int", vals.len()); }
    (sql, vals)
}

const MEASURES: &str = "count(*) AS projects, COALESCE(sum(p.budget),0)::float8 AS cost, COALESCE(sum(p.expected_benefit),0)::float8 AS benefit,
    round(100*(sum(p.expected_benefit)/NULLIF(sum(p.budget),0)-1),1)::float8 AS roi_pct,
    round(sum(p.expected_benefit)/NULLIF(sum(p.budget),0),2)::float8 AS benefit_cost_ratio,
    round(avg(p.percent_complete),1)::float8 AS avg_completion_pct,
    count(*) FILTER (WHERE p.status='completed') AS completed, count(*) FILTER (WHERE p.status='cancelled') AS cancelled,
    count(*) FILTER (WHERE p.status='active') AS active, count(*) FILTER (WHERE p.status='on_hold') AS on_hold,
    round(100.0*count(*) FILTER (WHERE p.status='completed')/NULLIF(count(*) FILTER (WHERE p.status IN ('completed','cancelled')),0),1)::float8 AS success_rate_pct,
    COALESCE(sum(p.expected_benefit) FILTER (WHERE p.status='completed'),0)::float8 AS realized_benefit,
    COALESCE(sum(p.budget*COALESCE(p.percent_complete,0)/100.0) FILTER (WHERE p.status='cancelled'),0)::float8 AS cancelled_sunk_cost";

async fn summary(State(st): State<AppState>, Query(p): Query<HashMap<String, String>>) -> AppResult<Json<J>> {
    let (f, v) = filters(&p);
    let t = row(&st.db, &format!("SELECT {MEASURES},
        (COALESCE(sum(p.expected_benefit),0)-COALESCE(sum(p.budget),0))::float8 AS net_benefit,
        corr(p.budget::float8, p.expected_benefit::float8) AS cost_benefit_correlation,
        count(*) FILTER (WHERE p.status='active' AND p.end_date < current_date) AS overdue_active,
        COALESCE(sum(p.expected_benefit) FILTER (WHERE p.status='active'),0)::float8 AS in_flight_benefit,
        COALESCE(sum(p.expected_benefit) FILTER (WHERE p.status='on_hold'),0)::float8 AS on_hold_benefit_at_risk,
        COALESCE(sum(p.expected_benefit) FILTER (WHERE p.status='cancelled'),0)::float8 AS cancelled_benefit_forgone
        {FROM} WHERE {SCOPE}{f}"), v.clone()).await?;
    let by_status = rows(&st.db, &format!("SELECT p.status, count(*) AS projects, sum(p.budget)::float8 AS cost, sum(p.expected_benefit)::float8 AS benefit,
        round(avg(p.percent_complete),1)::float8 AS avg_completion_pct {FROM} WHERE {SCOPE}{f} GROUP BY p.status ORDER BY 2 DESC"), v).await?;
    Ok(Json(json!({ "filters": p, "totals": t, "by_status": by_status,
        "notes": ["benefit is realised only for completed projects; cancelled_sunk_cost = cost x completion% at cancellation (estimate)",
                  "cost_benefit_correlation near 0 means expected benefit does not scale with cost"] })))
}

async fn by_dimension(State(st): State<AppState>, Path(dim): Path<String>, Query(p): Query<HashMap<String, String>>) -> AppResult<Json<J>> {
    let key = match dim.as_str() {
        "type" | "project_type" => "p.project_type", "region" => "p.region", "department" => "p.department", "complexity" => "p.complexity",
        "status" => "p.status", "manager" => "m.name", "year" => "extract(year from p.start_date)::int::text",
        "phase" => "('Phase ' || p.phase_no || COALESCE(' - ' || p.phase_name, ''))",
        _ => return Err(AppError::BadRequest("dimension must be one of: type, region, department, complexity, status, manager, year, phase".into())),
    };
    let (f, v) = filters(&p);
    let order = match p.get("order_by").map(String::as_str) { Some("cost") => "cost DESC", Some("projects") => "projects DESC", Some("success_rate") => "success_rate_pct DESC NULLS LAST", _ => "roi_pct DESC NULLS LAST" };
    let d = rows(&st.db, &format!("SELECT COALESCE({key}, '(none)') AS key, {MEASURES} {FROM} WHERE {SCOPE}{f} GROUP BY 1 ORDER BY {order}, 1"), v).await?;
    Ok(Json(json!({ "dimension": dim, "data": d })))
}

/// Finish-or-stop ranking for unfinished work. Sunk cost is ignored on purpose: the ratio compares the
/// benefit still to be won with the spend still to be made (cost x (1 - completion%)).
async fn rankings(State(st): State<AppState>, Query(p): Query<HashMap<String, String>>) -> AppResult<Json<J>> {
    let (f, v) = filters(&p);
    let limit: i64 = p.get("limit").and_then(|x| x.parse().ok()).unwrap_or(20).clamp(1, 200);
    let d = rows(&st.db, &format!("SELECT * FROM (SELECT p.id::text AS id, p.code, p.name, p.status, p.department, p.region, m.name AS manager, p.complexity,
        p.budget::float8 AS cost, p.expected_benefit::float8 AS benefit, p.percent_complete,
        (p.budget*(1-COALESCE(p.percent_complete,0)/100.0))::float8 AS remaining_cost,
        round((p.expected_benefit/NULLIF(p.budget*(1-COALESCE(p.percent_complete,0)/100.0),0))::numeric,2)::float8 AS marginal_return_ratio,
        p.end_date::text AS end_date, GREATEST(0, current_date - p.end_date) AS days_past_end,
        CASE WHEN p.budget*(1-COALESCE(p.percent_complete,0)/100.0) = 0 THEN 'close_out'
             WHEN p.expected_benefit/(p.budget*(1-COALESCE(p.percent_complete,0)/100.0)) >= 3 THEN 'prioritise'
             WHEN p.expected_benefit/(p.budget*(1-COALESCE(p.percent_complete,0)/100.0)) >= 1 THEN 'continue'
             ELSE 'review_for_stop' END AS recommendation
        {FROM} WHERE {SCOPE} AND p.status IN ('active','on_hold','planning'){f}) q
        ORDER BY marginal_return_ratio DESC NULLS LAST, days_past_end DESC LIMIT {limit}"), v).await?;
    Ok(Json(json!({ "method": "marginal_return_ratio = expected benefit / remaining cost (sunk cost ignored)",
        "thresholds": { "prioritise": ">= 3", "continue": ">= 1", "review_for_stop": "< 1" }, "data": d })))
}

const FLAGS_CTE: &str = "WITH flags AS (
    SELECT 'overdue_active' AS flag, 'high' AS severity, p.id::text AS id, p.code, p.name,
           ('ended ' || (current_date - p.end_date) || ' days ago, ' || COALESCE(p.percent_complete,0) || '% complete') AS detail
      FROM pm_projects p LEFT JOIN pm_resources m ON m.id=p.project_manager_id WHERE p.expected_benefit IS NOT NULL AND p.status='active' AND p.end_date < current_date
    UNION ALL
    SELECT 'cancelled_late', 'high', p.id::text, p.code, p.name,
           ('cancelled at ' || p.percent_complete || '% complete; estimated sunk cost ' || round(p.budget*p.percent_complete/100.0)::bigint)
      FROM pm_projects p WHERE p.expected_benefit IS NOT NULL AND p.status='cancelled' AND p.percent_complete >= 70
    UNION ALL
    SELECT 'completed_before_final_phase', 'medium', p.id::text, p.code, p.name, ('completed but phase is ' || p.phase_no || ' of 5')
      FROM pm_projects p WHERE p.expected_benefit IS NOT NULL AND p.status='completed' AND p.phase_no < 5
    UNION ALL
    SELECT 'on_hold_nearly_done', 'medium', p.id::text, p.code, p.name, ('on hold at ' || p.percent_complete || '% complete; remaining cost ' || round(p.budget*(1-p.percent_complete/100.0))::bigint)
      FROM pm_projects p WHERE p.expected_benefit IS NOT NULL AND p.status='on_hold' AND p.percent_complete >= 85
) ";

async fn risk_flags(State(st): State<AppState>, Query(p): Query<HashMap<String, String>>) -> AppResult<Json<J>> {
    let limit: i64 = p.get("limit").and_then(|x| x.parse().ok()).unwrap_or(100).clamp(1, 1000);
    let counts = rows(&st.db, &format!("{FLAGS_CTE} SELECT flag, severity, count(*) AS projects FROM flags GROUP BY 1,2 ORDER BY 2, 3 DESC"), vec![]).await?;
    let items = rows(&st.db, &format!("{FLAGS_CTE} SELECT * FROM flags WHERE ($1::text IS NULL OR flag=$1) ORDER BY severity, flag, name LIMIT {limit}"), vec![p.get("flag").cloned().into()]).await?;
    Ok(Json(json!({ "counts": counts, "items": items })))
}

async fn timeline(State(st): State<AppState>, Query(p): Query<HashMap<String, String>>) -> AppResult<Json<J>> {
    let (g, fmt) = match p.get("granularity").map(String::as_str).unwrap_or("quarter") {
        "year" => ("year", "YYYY"), "month" => ("month", "YYYY-MM"), "quarter" => ("quarter", "YYYY-\"Q\"Q"),
        _ => return Err(AppError::BadRequest("granularity must be year, quarter or month".into())),
    };
    let (f, v) = filters(&p);
    let d = rows(&st.db, &format!("WITH t AS (SELECT date_trunc('{g}', p.start_date)::date AS bucket, p.* {FROM} WHERE {SCOPE}{f})
        SELECT to_char(bucket, '{fmt}') AS period, count(*) AS projects_started, sum(budget)::float8 AS cost, sum(expected_benefit)::float8 AS benefit,
               count(*) FILTER (WHERE status='completed') AS completed, count(*) FILTER (WHERE status='cancelled') AS cancelled,
               sum(sum(budget)) OVER (ORDER BY bucket)::float8 AS cumulative_cost
        FROM t GROUP BY bucket ORDER BY bucket"), v).await?;
    Ok(Json(json!({ "granularity": g, "data": d })))
}

// -------------------------------------------------------- reference class

const CLASS_STATS: &str = "SELECT count(*) AS n,
    percentile_cont(0.25) WITHIN GROUP (ORDER BY p.budget)::float8 AS cost_p25, percentile_cont(0.5) WITHIN GROUP (ORDER BY p.budget)::float8 AS cost_p50,
    percentile_cont(0.75) WITHIN GROUP (ORDER BY p.budget)::float8 AS cost_p75,
    percentile_cont(0.25) WITHIN GROUP (ORDER BY p.expected_benefit)::float8 AS benefit_p25, percentile_cont(0.5) WITHIN GROUP (ORDER BY p.expected_benefit)::float8 AS benefit_p50,
    percentile_cont(0.75) WITHIN GROUP (ORDER BY p.expected_benefit)::float8 AS benefit_p75,
    percentile_cont(0.5) WITHIN GROUP (ORDER BY (p.expected_benefit/NULLIF(p.budget,0)-1)*100)::float8 AS roi_pct_p50,
    percentile_cont(0.5) WITHIN GROUP (ORDER BY (p.end_date-p.start_date))::float8 AS duration_days_p50,
    percentile_cont(0.9) WITHIN GROUP (ORDER BY (p.end_date-p.start_date))::float8 AS duration_days_p90,
    count(*) FILTER (WHERE p.status='completed') AS completed, count(*) FILTER (WHERE p.status='cancelled') AS cancelled";

async fn benchmarks(State(st): State<AppState>, Query(p): Query<HashMap<String, String>>) -> AppResult<Json<J>> {
    let (f, v) = filters(&p);
    let s = row(&st.db, &format!("{CLASS_STATS} {FROM} WHERE {SCOPE}{f}"), v).await?;
    Ok(Json(json!({ "filters": p, "stats": s })))
}

/// Reference-class estimate for a prospective project: widens the class (type+complexity+department ->
/// type+complexity -> type -> all) until at least `min_n` comparable projects exist.
async fn estimate(State(st): State<AppState>, Json(b): Json<J>) -> AppResult<Json<J>> {
    let min_n = b["min_n"].as_i64().unwrap_or(8).max(1);
    let get = |k: &str| b[k].as_str().map(String::from);
    let (t, c, d) = (get("project_type"), get("complexity"), get("department"));
    let mut levels: Vec<Vec<(&str, String)>> = vec![];
    let mut cur: Vec<(&str, String)> = vec![];
    for (k, v) in [("project_type", &t), ("complexity", &c), ("department", &d)] { if let Some(x) = v { cur.push((k, x.clone())); } }
    while !cur.is_empty() { levels.push(cur.clone()); cur.pop(); }
    levels.push(vec![]);
    let mut chosen = None;
    for lv in levels {
        let m: HashMap<String, String> = lv.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        let (f, v) = filters(&m);
        let s = row(&st.db, &format!("{CLASS_STATS} {FROM} WHERE {SCOPE}{f}"), v).await?;
        let n = s["n"].as_i64().unwrap_or(0);
        if n >= min_n || lv.is_empty() { chosen = Some((lv, s)); break; }
    }
    let (lv, s) = chosen.unwrap();
    let n = s["n"].as_i64().unwrap_or(0);
    if n == 0 { return Err(AppError::Unprocessable("no portfolio data available; import projects first".into())); }
    let (done, canc) = (s["completed"].as_f64().unwrap_or(0.0), s["cancelled"].as_f64().unwrap_or(0.0));
    let p_complete = (done + 1.0) / (done + canc + 2.0);
    let cost = b["planned_cost"].as_f64().or(s["cost_p50"].as_f64()).unwrap_or(0.0);
    let ben = s["benefit_p50"].as_f64().unwrap_or(0.0);
    Ok(Json(json!({ "reference_class": lv.iter().map(|(k, v)| json!({ (*k): v })).collect::<Vec<_>>(), "comparable_projects": n, "stats": s,
        "completion_probability": (p_complete * 1000.0).round() / 1000.0,
        "planned_cost_used": cost, "expected_benefit_if_completed": ben,
        "expected_net_value": (p_complete * ben - cost).round(),
        "method": "completion_probability = (completed+1)/(completed+cancelled+2) over the reference class; expected_net_value = probability x median benefit - cost. A reference-class estimate, not a forecasting model." })))
}
