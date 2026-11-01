use axum::{extract::State, routing::get, Json, Router};
use chrono::Utc;
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::supply_chain::{
    error::{AppError, AppResult},
    state::AppState,
};

pub mod cognitive;
pub mod forecasting;
pub mod inventory;
pub mod loop_ops;
pub mod master;
pub mod ops_reports;
pub mod planning;
pub mod procurement;
pub mod production;
pub mod purchasing;
pub mod quality;
pub mod reports;
pub mod resilience;
pub mod sales;
pub mod supplier_perf;

pub fn router() -> Router<AppState> {
    Router::new()
        .merge(master::routes())
        .merge(inventory::routes())
        .merge(purchasing::routes())
        .merge(sales::routes())
        .merge(reports::routes())
        .merge(quality::routes())
        .merge(supplier_perf::routes())
        .merge(planning::routes())
        .merge(forecasting::routes())
        .merge(resilience::routes())
        .merge(loop_ops::routes())
        .merge(ops_reports::routes())
        .merge(production::routes())
        .merge(procurement::routes())
        .merge(cognitive::routes())
        .merge(crate::supply_chain::bridges::routes())
        .route("/health", get(health))
}

async fn health(State(s): State<AppState>) -> Json<Value> {
    let db_ok = s.db.execute_unprepared("SELECT 1").await.is_ok();
    Json(json!({ "status": if db_ok { "ok" } else { "degraded" }, "database": db_ok, "module": "supply_chain" }))
}

// ---- shared helpers ------------------------------------------------------------------------

pub fn page_limit(limit: Option<u64>) -> u64 {
    limit.unwrap_or(50).clamp(1, 500)
}

/// Human-friendly document number, e.g. PO-20261025-A1B2C3.
pub fn gen_number(prefix: &str) -> String {
    let rand = Uuid::new_v4().simple().to_string()[..6].to_uppercase();
    format!("{}-{}-{}", prefix, Utc::now().format("%Y%m%d"), rand)
}

pub fn require_text(field: &str, value: &str) -> AppResult<String> {
    let v = value.trim();
    if v.is_empty() {
        return Err(AppError::bad(format!("{field} must not be empty")));
    }
    Ok(v.to_string())
}

pub fn require_positive(field: &str, value: i64) -> AppResult<i64> {
    if value <= 0 {
        return Err(AppError::bad(format!("{field} must be greater than zero")));
    }
    Ok(value)
}

pub fn clean_lot(lot: &Option<String>) -> String {
    lot.as_deref().unwrap_or("").trim().to_string()
}

pub const MODES: [&str; 7] = ["air", "air_charter", "sea", "rail", "road", "courier", "multimodal"];
pub const INCOTERMS: [&str; 12] = ["EXW", "FCA", "FAS", "FOB", "CFR", "CIF", "CPT", "CIP", "DAP", "DPU", "DDP", "DDU"];

pub fn check_mode(v: Option<&str>) -> AppResult<Option<String>> {
    match v.map(|m| m.trim().to_ascii_lowercase()) {
        None => Ok(None),
        Some(m) if MODES.contains(&m.as_str()) => Ok(Some(m)),
        Some(_) => Err(AppError::bad(format!("transport_mode must be one of {}", MODES.join(", ")))),
    }
}

pub fn check_incoterm(v: Option<&str>) -> AppResult<Option<String>> {
    match v.map(|m| m.trim().to_ascii_uppercase()) {
        None => Ok(None),
        Some(m) if INCOTERMS.contains(&m.as_str()) => Ok(Some(m)),
        Some(_) => Err(AppError::bad(format!("incoterm must be one of {}", INCOTERMS.join(", ")))),
    }
}

pub fn check_pct(field: &str, v: Option<f64>) -> AppResult<Option<f64>> {
    if matches!(v, Some(x) if !(0.0..=100.0).contains(&x) || x.is_nan()) {
        return Err(AppError::bad(format!("{field} must be between 0 and 100")));
    }
    Ok(v)
}
