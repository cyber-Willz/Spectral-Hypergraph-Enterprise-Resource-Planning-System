//! Asset management: maintenance scheduling, work-order completion, predictive maintenance, lifecycle/depreciation.
use crate::db::{row, rows};
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/am/maintenance/generate-work-orders", post(generate_work_orders))
        .route("/api/am/work-orders/{id}/complete", post(complete_wo))
        .route("/api/am/meter-readings/bulk", post(bulk_readings))
        .route("/api/am/predictive/analyze", post(analyze))
        .route("/api/am/assets/{id}/lifecycle", get(lifecycle))
        .route("/api/am/analytics/overview", get(overview))
}

/// Creates preventive work orders for plans that are due within the horizon and have none open.
async fn generate_work_orders(State(st): State<AppState>, Json(b): Json<Value>) -> AppResult<Json<Value>> {
    let horizon = b["horizon_days"].as_i64().unwrap_or(7) as i32;
    let created = rows(&st.db, "INSERT INTO am_work_orders (asset_id, plan_id, title, description, wo_type, priority, scheduled_date, assigned_to, status)
        SELECT p.asset_id, p.id, p.name || ' - ' || a.name, p.instructions,
               CASE WHEN p.plan_type IN ('preventive','predictive','inspection','calibration') THEN p.plan_type ELSE 'preventive' END,
               CASE a.criticality WHEN 'critical' THEN 'high' ELSE 'medium' END, p.next_due, p.assigned_to, 'scheduled'
        FROM am_maintenance_plans p JOIN am_assets a ON a.id=p.asset_id
        WHERE p.is_active AND a.status NOT IN ('retired','disposed') AND p.next_due <= current_date + $1
          AND NOT EXISTS (SELECT 1 FROM am_work_orders w WHERE w.plan_id=p.id AND w.status NOT IN ('completed','cancelled'))
        RETURNING id::text AS id, wo_number, asset_id::text AS asset_id, title, scheduled_date::text AS scheduled_date", vec![horizon.into()]).await?;
    Ok(Json(json!({ "horizon_days": horizon, "created": created.len(), "work_orders": created })))
}

/// Completes a work order: records effort/cost, resets the plan clock, restores the asset and logs the lifecycle event.
async fn complete_wo(State(st): State<AppState>, Path(id): Path<Uuid>, Json(b): Json<Value>) -> AppResult<Json<Value>> {
    let tx = st.db.begin().await?;
    let wo = row(&tx, "SELECT status, plan_id::text AS plan_id, asset_id::text AS asset_id, wo_type, title FROM am_work_orders WHERE id=$1 FOR UPDATE", vec![id.into()]).await?;
    if wo.is_null() { return Err(AppError::NotFound(None)); }
    if matches!(wo["status"].as_str(), Some("completed" | "cancelled")) { return Err(AppError::Conflict(format!("work order already {}", wo["status"].as_str().unwrap()))); }
    let hours = b["labor_hours"].as_f64().unwrap_or(0.0);
    let rate = b["labor_rate"].as_f64().unwrap_or(0.0);
    let parts = b["parts_cost"].as_f64().unwrap_or(0.0);
    let down = b["downtime_hours"].as_f64().unwrap_or(0.0);
    let done = row(&tx, "UPDATE am_work_orders SET status='completed', labor_hours=$2::float8::numeric, labor_cost=($2*$3)::numeric, parts_cost=$4::float8::numeric,
        downtime_hours=$5::float8::numeric, failure_code=COALESCE($6, failure_code), notes=COALESCE($7, notes) WHERE id=$1
        RETURNING id::text AS id, wo_number, status, completed_at, (labor_cost+parts_cost)::float8 AS total_cost",
        vec![id.into(), hours.into(), rate.into(), parts.into(), down.into(), b["failure_code"].as_str().map(String::from).into(), b["notes"].as_str().map(String::from).into()]).await?;
    if let Some(pid) = wo["plan_id"].as_str().and_then(|s| s.parse::<Uuid>().ok()) {
        tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "UPDATE am_maintenance_plans SET last_performed=current_date, next_due=current_date+frequency_days WHERE id=$1", [pid.into()])).await?;
    }
    let aid: Uuid = wo["asset_id"].as_str().unwrap().parse().unwrap();
    tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE am_assets SET status='active' WHERE id=$1 AND status='under_maintenance'", [aid.into()])).await?;
    tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO am_lifecycle_events (asset_id, event_type, description, cost) VALUES ($1,'repaired',$2,($3+$4)::float8::numeric)",
        [aid.into(), format!("Work order completed: {}", wo["title"].as_str().unwrap_or("")).into(), (hours * rate).into(), parts.into()])).await?;
    // resolve alerts tied to this work order
    tx.execute(Statement::from_sql_and_values(DbBackend::Postgres, "UPDATE am_predictive_alerts SET status='resolved' WHERE work_order_id=$1", [id.into()])).await?;
    tx.commit().await?;
    Ok(Json(done))
}

async fn bulk_readings(State(st): State<AppState>, Json(b): Json<Value>) -> AppResult<Json<Value>> {
    let arr = b.as_array().ok_or_else(|| AppError::BadRequest("expected a JSON array of readings".into()))?;
    if arr.len() > 5000 { return Err(AppError::BadRequest("max 5000 readings per call".into())); }
    let tx = st.db.begin().await?;
    for r in arr {
        let aid: Uuid = r["asset_id"].as_str().and_then(|s| s.parse().ok()).ok_or_else(|| AppError::BadRequest("asset_id (uuid) required".into()))?;
        let metric = r["metric"].as_str().ok_or_else(|| AppError::BadRequest("metric required".into()))?;
        let value = r["value"].as_f64().ok_or_else(|| AppError::BadRequest("numeric value required".into()))?;
        tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "INSERT INTO am_meter_readings (asset_id, metric, value, unit, recorded_at) VALUES ($1,$2,$3,$4,COALESCE($5::timestamptz, now()))",
            [aid.into(), metric.into(), value.into(), r["unit"].as_str().map(String::from).into(), r["recorded_at"].as_str().map(String::from).into()])).await?;
    }
    tx.commit().await?;
    Ok(Json(json!({ "inserted": arr.len() })))
}

fn mean_std(v: &[f64]) -> (f64, f64) {
    let n = v.len() as f64;
    let m = v.iter().sum::<f64>() / n;
    let var = v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (n - 1.0).max(1.0);
    (m, var.sqrt())
}

/// Predictive maintenance. For each (asset, metric) series: the latest reading is scored against the
/// preceding baseline window (z-score anomaly), and a least-squares trend over the window projects the
/// days until `critical_threshold` (per metric, optional) is crossed. Alerts are de-duplicated.
async fn analyze(State(st): State<AppState>, Json(b): Json<Value>) -> AppResult<Json<Value>> {
    let window = b["window"].as_i64().unwrap_or(30).clamp(8, 1000);
    let z_thr = b["z_threshold"].as_f64().unwrap_or(3.0);
    let horizon = b["trend_horizon_days"].as_f64().unwrap_or(30.0);
    let auto_wo = b["auto_work_order"].as_bool().unwrap_or(false);
    let asset: Option<Uuid> = b["asset_id"].as_str().and_then(|s| s.parse().ok());
    let thresholds = b["critical_thresholds"].clone();
    let series = rows(&st.db, "SELECT DISTINCT asset_id::text AS asset_id, metric FROM am_meter_readings WHERE ($1::uuid IS NULL OR asset_id=$1)", vec![asset.into()]).await?;
    let mut alerts = vec![];
    for s in &series {
        let aid: Uuid = s["asset_id"].as_str().unwrap().parse().unwrap();
        let metric = s["metric"].as_str().unwrap().to_string();
        let pts = rows(&st.db, "SELECT value, extract(epoch FROM recorded_at)::float8 AS ts FROM am_meter_readings WHERE asset_id=$1 AND metric=$2 ORDER BY recorded_at DESC LIMIT $3",
            vec![aid.into(), metric.clone().into(), (window + 1).into()]).await?;
        if pts.len() < 8 { continue; }
        let latest = pts[0]["value"].as_f64().unwrap_or(0.0);
        let base: Vec<f64> = pts[1..].iter().filter_map(|p| p["value"].as_f64()).collect();
        let (mean, sd) = mean_std(&base);
        let z = if sd > 1e-9 { (latest - mean) / sd } else { 0.0 };
        let mut found: Vec<(&str, String, Option<f64>)> = vec![];
        if z.abs() >= z_thr {
            found.push(("anomaly", format!("{metric} reading {latest:.2} deviates {z:.1} sigma from baseline {mean:.2}"), None));
        }
        // linear trend (value vs. days) over chronological window
        let chrono: Vec<(f64, f64)> = pts.iter().rev().filter_map(|p| Some((p["ts"].as_f64()? / 86400.0, p["value"].as_f64()?))).collect();
        let n = chrono.len() as f64;
        let (mx, my) = (chrono.iter().map(|p| p.0).sum::<f64>() / n, chrono.iter().map(|p| p.1).sum::<f64>() / n);
        let sxx: f64 = chrono.iter().map(|p| (p.0 - mx).powi(2)).sum();
        let slope = if sxx > 1e-12 { chrono.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum::<f64>() / sxx } else { 0.0 };
        let mut days_to: Option<f64> = None;
        if let Some(th) = thresholds[&metric].as_f64() {
            let cur = my + slope * (chrono.last().unwrap().0 - mx);
            if slope.abs() > 1e-9 && ((th > cur && slope > 0.0) || (th < cur && slope < 0.0)) {
                days_to = Some((th - cur) / slope);
            }
            if let Some(d) = days_to.filter(|d| *d <= horizon) {
                found.push(("trend", format!("{metric} trending to {th} in ~{d:.1} days (slope {slope:.3}/day)"), Some(d)));
            }
        }
        for (kind, msg, dt) in found {
            let severity = if kind == "trend" && dt.map(|d| d <= horizon / 3.0).unwrap_or(false) || z.abs() >= z_thr * 1.5 { "critical" } else { "warning" };
            let exists = row(&st.db, "SELECT 1 AS x FROM am_predictive_alerts WHERE asset_id=$1 AND metric=$2 AND kind=$3 AND status IN ('open','acknowledged')",
                vec![aid.into(), metric.clone().into(), kind.into()]).await?;
            if !exists.is_null() { continue; }
            let mut wo_id: Option<Uuid> = None;
            if auto_wo && severity == "critical" {
                let w = row(&st.db, "INSERT INTO am_work_orders (asset_id, title, description, wo_type, priority, status) VALUES ($1,$2,$3,'predictive','high','open') RETURNING id::text AS id",
                    vec![aid.into(), format!("Predictive: {metric} {kind}").into(), msg.clone().into()]).await?;
                wo_id = w["id"].as_str().and_then(|s| s.parse().ok());
            }
            let a = row(&st.db, "INSERT INTO am_predictive_alerts (asset_id, metric, severity, kind, message, observed_value, expected_value, z_score, days_to_threshold, work_order_id)
                VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) RETURNING id::text AS id, asset_id::text AS asset_id, metric, severity, kind, message, z_score, days_to_threshold, work_order_id::text AS work_order_id",
                vec![aid.into(), metric.clone().into(), severity.into(), kind.into(), msg.into(), latest.into(), mean.into(), z.into(), dt.into(), wo_id.into()]).await?;
            alerts.push(a);
        }
    }
    Ok(Json(json!({ "series_analyzed": series.len(), "alerts_created": alerts.len(), "alerts": alerts })))
}

/// Straight-line depreciation, total cost of ownership and reliability (MTTR / MTBF) for one asset.
async fn lifecycle(State(st): State<AppState>, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let a = row(&st.db, "SELECT to_jsonb(a) AS a,
        COALESCE(a.installed_on, a.purchase_date) AS start_d,
        a.purchase_cost::float8 AS cost, a.salvage_value::float8 AS salvage, a.useful_life_months AS life,
        GREATEST(0, (extract(year from age(current_date, COALESCE(a.installed_on,a.purchase_date)))*12 + extract(month from age(current_date, COALESCE(a.installed_on,a.purchase_date))))::int) AS months_in_service
        FROM am_assets a WHERE id=$1", vec![id.into()]).await?;
    if a.is_null() { return Err(AppError::NotFound(None)); }
    let cost = a["cost"].as_f64().unwrap_or(0.0);
    let salvage = a["salvage"].as_f64().unwrap_or(0.0);
    let life = a["life"].as_i64().unwrap_or(60) as f64;
    let months = a["months_in_service"].as_i64().unwrap_or(0) as f64;
    let monthly = if life > 0.0 { (cost - salvage) / life } else { 0.0 };
    let accumulated = (monthly * months.min(life)).min(cost - salvage).max(0.0);
    let maint = row(&st.db, "SELECT COALESCE(sum(labor_cost+parts_cost),0)::float8 AS maintenance_cost,
        count(*) FILTER (WHERE wo_type='corrective' AND status='completed') AS failures,
        round(avg(downtime_hours) FILTER (WHERE wo_type='corrective' AND status='completed'),2)::float8 AS mttr_hours,
        COALESCE(sum(downtime_hours),0)::float8 AS total_downtime_hours,
        min(created_at) AS first_wo FROM am_work_orders WHERE asset_id=$1 AND status='completed'", vec![id.into()]).await?;
    let failures = maint["failures"].as_i64().unwrap_or(0);
    let mtbf_days = if failures > 0 { Some((months * 30.4 / failures as f64 * 10.0).round() / 10.0) } else { None };
    let events = rows(&st.db, "SELECT event_type, event_date::text AS event_date, description, cost::float8 AS cost FROM am_lifecycle_events WHERE asset_id=$1 ORDER BY event_date, created_at", vec![id.into()]).await?;
    let mc = maint["maintenance_cost"].as_f64().unwrap_or(0.0);
    // when the asset has been capitalised in the ledger, show the books' figures next to the computed ones
    let ledger = row(&st.db, "SELECT f.id AS fixed_asset_id, f.cost::float8 AS cost, f.accumulated_depreciation::float8 AS accumulated_depreciation,
        (f.cost - f.accumulated_depreciation)::float8 AS book_value, f.status FROM fixed_assets f JOIN am_assets a ON a.fixed_asset_id=f.id WHERE a.id=$1", vec![id.into()]).await?;
    Ok(Json(json!({ "asset": a["a"],
        "depreciation": { "method": "straight_line", "monthly": (monthly * 100.0).round() / 100.0, "months_in_service": months,
            "accumulated": (accumulated * 100.0).round() / 100.0, "book_value": ((cost - accumulated) * 100.0).round() / 100.0,
            "remaining_life_months": (life - months).max(0.0), "fully_depreciated": months >= life },
        "total_cost_of_ownership": { "purchase": cost, "maintenance": mc, "total": cost + mc },
        "reliability": { "failures": failures, "mttr_hours": maint["mttr_hours"], "mtbf_days": mtbf_days, "total_downtime_hours": maint["total_downtime_hours"] },
        "ledger": ledger,
        "events": events })))
}

async fn overview(State(st): State<AppState>) -> AppResult<Json<Value>> {
    let by_status = rows(&st.db, "SELECT status, count(*) AS assets FROM am_assets GROUP BY status", vec![]).await?;
    let by_crit = rows(&st.db, "SELECT criticality, count(*) AS assets FROM am_assets WHERE status NOT IN ('retired','disposed') GROUP BY criticality", vec![]).await?;
    let m = row(&st.db, "SELECT (SELECT count(*) FROM am_maintenance_plans WHERE is_active AND next_due < current_date) AS overdue_plans,
        (SELECT count(*) FROM am_work_orders WHERE status NOT IN ('completed','cancelled')) AS open_work_orders,
        (SELECT count(*) FROM am_work_orders WHERE status NOT IN ('completed','cancelled') AND scheduled_date < current_date) AS overdue_work_orders,
        (SELECT round(100.0*count(*) FILTER (WHERE completed_at::date <= COALESCE(scheduled_date, completed_at::date))/NULLIF(count(*),0),1)::float8 FROM am_work_orders WHERE wo_type='preventive' AND status='completed') AS pm_on_time_pct,
        (SELECT count(*) FROM am_predictive_alerts WHERE status='open') AS open_predictive_alerts,
        (SELECT count(*) FROM am_assets WHERE warranty_expires BETWEEN current_date AND current_date+60) AS warranties_expiring_60d,
        (SELECT COALESCE(sum(downtime_hours),0)::float8 FROM am_work_orders WHERE completed_at > now() - interval '90 days') AS downtime_hours_90d", vec![]).await?;
    Ok(Json(json!({ "assets_by_status": by_status, "active_assets_by_criticality": by_crit, "maintenance": m })))
}
