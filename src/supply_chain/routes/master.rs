//! Master data: warehouses, bin locations, products, suppliers, customers, carriers.
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use sea_orm::{
    prelude::Decimal, ActiveModelTrait, ColumnTrait, Condition, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect, Set,
};
use serde::Deserialize;
use uuid::Uuid;

use super::{page_limit, require_text};
use crate::supply_chain::{
    entities::{carrier, customer, location, product, supplier, warehouse},
    error::{AppError, AppResult},
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/warehouses", post(create_warehouse).get(list_warehouses))
        .route("/warehouses/{id}", get(get_warehouse))
        .route(
            "/warehouses/{id}/locations",
            post(create_location).get(list_locations),
        )
        .route("/products", post(create_product).get(list_products))
        .route("/products/{id}", get(get_product).patch(patch_product))
        .route("/suppliers", post(create_supplier).get(list_suppliers))
        .route("/suppliers/{id}", get(get_supplier).patch(patch_supplier))
        .route("/customers", post(create_customer).get(list_customers))
        .route("/carriers", post(create_carrier).get(list_carriers))
}

#[derive(Deserialize, Default)]
pub struct ListQuery {
    pub q: Option<String>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

// ---- warehouses ------------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct NewWarehouse {
    code: String,
    name: String,
    address: Option<String>,
    city: Option<String>,
    country: Option<String>,
}

async fn create_warehouse(
    State(s): State<AppState>,
    Json(i): Json<NewWarehouse>,
) -> AppResult<(StatusCode, Json<warehouse::Model>)> {
    let m = warehouse::ActiveModel {
        id: Set(Uuid::new_v4()),
        code: Set(require_text("code", &i.code)?.to_uppercase()),
        name: Set(require_text("name", &i.name)?),
        address: Set(i.address),
        city: Set(i.city),
        country: Set(i.country),
        active: Set(true),
        created_at: Set(Utc::now()),
    }
    .insert(&s.db)
    .await?;
    Ok((StatusCode::CREATED, Json(m)))
}

async fn list_warehouses(
    State(s): State<AppState>,
    Query(q): Query<ListQuery>,
) -> AppResult<Json<Vec<warehouse::Model>>> {
    let rows = warehouse::Entity::find()
        .order_by_asc(warehouse::Column::Code)
        .limit(page_limit(q.limit))
        .offset(q.offset.unwrap_or(0))
        .all(&s.db)
        .await?;
    Ok(Json(rows))
}

async fn get_warehouse(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<warehouse::Model>> {
    Ok(Json(get_or_404!(warehouse::Entity, &s.db, id, "warehouse")))
}

// ---- locations ---------------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct NewLocation {
    code: String,
    zone: Option<String>,
    capacity: Option<i64>,
    /// ambient | cold | frozen | controlled (default ambient)
    storage_class: Option<String>,
}

async fn create_location(
    State(s): State<AppState>,
    Path(warehouse_id): Path<Uuid>,
    Json(i): Json<NewLocation>,
) -> AppResult<(StatusCode, Json<location::Model>)> {
    get_or_404!(warehouse::Entity, &s.db, warehouse_id, "warehouse");
    if matches!(i.capacity, Some(c) if c < 0) {
        return Err(AppError::bad("capacity must not be negative"));
    }
    let m = location::ActiveModel {
        id: Set(Uuid::new_v4()),
        warehouse_id: Set(warehouse_id),
        code: Set(require_text("code", &i.code)?.to_uppercase()),
        zone: Set(i.zone),
        capacity: Set(i.capacity),
        storage_class: Set(check_storage_class(i.storage_class.as_deref())?),
        active: Set(true),
        created_at: Set(Utc::now()),
    }
    .insert(&s.db)
    .await?;
    Ok((StatusCode::CREATED, Json(m)))
}

async fn list_locations(
    State(s): State<AppState>,
    Path(warehouse_id): Path<Uuid>,
    Query(q): Query<ListQuery>,
) -> AppResult<Json<Vec<location::Model>>> {
    let rows = location::Entity::find()
        .filter(location::Column::WarehouseId.eq(warehouse_id))
        .order_by_asc(location::Column::Code)
        .limit(page_limit(q.limit))
        .offset(q.offset.unwrap_or(0))
        .all(&s.db)
        .await?;
    Ok(Json(rows))
}

// ---- products ----------------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct NewProduct {
    sku: String,
    name: String,
    description: Option<String>,
    unit_of_measure: Option<String>,
    unit_cost: Option<Decimal>,
    list_price: Option<Decimal>,
    reorder_point: Option<i64>,
    reorder_qty: Option<i64>,
    /// A (flight/mission critical) | B | C. Drives the default service level.
    criticality_class: Option<String>,
    storage_class: Option<String>,
    shelf_life_days: Option<i32>,
    is_repairable: Option<bool>,
    /// Restocking cadence: 7 weekly, 30 monthly, 90 quarterly.
    review_period_days: Option<i32>,
    lead_time_days: Option<i32>,
    /// Annual holding cost as a fraction of unit cost (default 0.25).
    holding_cost_rate: Option<Decimal>,
    /// Cost of one unit short; when set, the service level comes from the critical ratio.
    stockout_cost: Option<Decimal>,
    /// acceptable defect rate (percent) at incoming inspection; lots above it are rejected
    aql_defect_pct: Option<f64>,
}

pub fn check_storage_class(v: Option<&str>) -> AppResult<String> {
    let v = v.unwrap_or("ambient");
    if !matches!(v, "ambient" | "cold" | "frozen" | "controlled") {
        return Err(AppError::bad("storage_class must be ambient, cold, frozen or controlled"));
    }
    Ok(v.to_string())
}

fn check_criticality(v: Option<&str>) -> AppResult<String> {
    let v = v.unwrap_or("C");
    if !matches!(v, "A" | "B" | "C") {
        return Err(AppError::bad("criticality_class must be A, B or C"));
    }
    Ok(v.to_string())
}

fn check_money(field: &str, v: &Decimal) -> AppResult<()> {
    if v.is_sign_negative() {
        return Err(AppError::bad(format!("{field} must not be negative")));
    }
    Ok(())
}

fn check_pos_opt(field: &str, v: Option<i32>) -> AppResult<Option<i32>> {
    if matches!(v, Some(x) if x <= 0) {
        return Err(AppError::bad(format!("{field} must be greater than zero")));
    }
    Ok(v)
}

fn check_nonneg(field: &str, v: i64) -> AppResult<()> {
    if v < 0 {
        return Err(AppError::bad(format!("{field} must not be negative")));
    }
    Ok(())
}

async fn create_product(
    State(s): State<AppState>,
    Json(i): Json<NewProduct>,
) -> AppResult<(StatusCode, Json<product::Model>)> {
    let unit_cost = i.unit_cost.unwrap_or_default();
    let list_price = i.list_price.unwrap_or_default();
    let reorder_point = i.reorder_point.unwrap_or(0);
    let reorder_qty = i.reorder_qty.unwrap_or(0);
    check_money("unit_cost", &unit_cost)?;
    check_money("list_price", &list_price)?;
    check_nonneg("reorder_point", reorder_point)?;
    check_nonneg("reorder_qty", reorder_qty)?;
    let now = Utc::now();
    let m = product::ActiveModel {
        id: Set(Uuid::new_v4()),
        sku: Set(require_text("sku", &i.sku)?.to_uppercase()),
        name: Set(require_text("name", &i.name)?),
        description: Set(i.description),
        unit_of_measure: Set(i.unit_of_measure.unwrap_or_else(|| "each".into())),
        unit_cost: Set(unit_cost),
        list_price: Set(list_price),
        reorder_point: Set(reorder_point),
        reorder_qty: Set(reorder_qty),
        criticality_class: Set(check_criticality(i.criticality_class.as_deref())?),
        storage_class: Set(check_storage_class(i.storage_class.as_deref())?),
        shelf_life_days: Set(check_pos_opt("shelf_life_days", i.shelf_life_days)?),
        is_repairable: Set(i.is_repairable.unwrap_or(false)),
        review_period_days: Set(check_pos_opt("review_period_days", i.review_period_days)?.unwrap_or(7)),
        lead_time_days: Set(match i.lead_time_days { Some(v) if v < 0 => return Err(AppError::bad("lead_time_days must not be negative")), Some(v) => v, None => 14 }),
        holding_cost_rate: Set({ let v = i.holding_cost_rate.unwrap_or_else(|| Decimal::new(25, 2)); check_money("holding_cost_rate", &v)?; v }),
        stockout_cost: Set(match i.stockout_cost { Some(v) => { check_money("stockout_cost", &v)?; Some(v) } None => None }),
        aql_defect_pct: Set(super::check_pct("aql_defect_pct", i.aql_defect_pct)?),
        active: Set(true),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&s.db)
    .await?;
    Ok((StatusCode::CREATED, Json(m)))
}

/// `q` matches SKU or name (substring).
async fn list_products(
    State(s): State<AppState>,
    Query(q): Query<ListQuery>,
) -> AppResult<Json<Vec<product::Model>>> {
    let mut cond = Condition::all();
    if let Some(text) = q.q.as_deref().filter(|t| !t.trim().is_empty()) {
        cond = cond.add(
            Condition::any()
                .add(product::Column::Sku.contains(text.trim()))
                .add(product::Column::Name.contains(text.trim())),
        );
    }
    let rows = product::Entity::find()
        .filter(cond)
        .order_by_asc(product::Column::Sku)
        .limit(page_limit(q.limit))
        .offset(q.offset.unwrap_or(0))
        .all(&s.db)
        .await?;
    Ok(Json(rows))
}

async fn get_product(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<product::Model>> {
    Ok(Json(get_or_404!(product::Entity, &s.db, id, "product")))
}

#[derive(Deserialize)]
pub struct PatchProduct {
    name: Option<String>,
    description: Option<String>,
    unit_of_measure: Option<String>,
    unit_cost: Option<Decimal>,
    list_price: Option<Decimal>,
    reorder_point: Option<i64>,
    reorder_qty: Option<i64>,
    criticality_class: Option<String>,
    storage_class: Option<String>,
    shelf_life_days: Option<i32>,
    is_repairable: Option<bool>,
    review_period_days: Option<i32>,
    lead_time_days: Option<i32>,
    holding_cost_rate: Option<Decimal>,
    stockout_cost: Option<Decimal>,
    aql_defect_pct: Option<f64>,
    active: Option<bool>,
}

async fn patch_product(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(p): Json<PatchProduct>,
) -> AppResult<Json<product::Model>> {
    let existing = get_or_404!(product::Entity, &s.db, id, "product");
    let mut am: product::ActiveModel = existing.into();
    if let Some(v) = p.name {
        am.name = Set(require_text("name", &v)?);
    }
    if let Some(v) = p.description {
        am.description = Set(Some(v));
    }
    if let Some(v) = p.unit_of_measure {
        am.unit_of_measure = Set(require_text("unit_of_measure", &v)?);
    }
    if let Some(v) = p.unit_cost {
        check_money("unit_cost", &v)?;
        am.unit_cost = Set(v);
    }
    if let Some(v) = p.list_price {
        check_money("list_price", &v)?;
        am.list_price = Set(v);
    }
    if let Some(v) = p.reorder_point {
        check_nonneg("reorder_point", v)?;
        am.reorder_point = Set(v);
    }
    if let Some(v) = p.reorder_qty {
        check_nonneg("reorder_qty", v)?;
        am.reorder_qty = Set(v);
    }
    if let Some(v) = p.criticality_class {
        am.criticality_class = Set(check_criticality(Some(&v))?);
    }
    if let Some(v) = p.storage_class {
        am.storage_class = Set(check_storage_class(Some(&v))?);
    }
    if let Some(v) = p.shelf_life_days {
        am.shelf_life_days = Set(check_pos_opt("shelf_life_days", Some(v))?);
    }
    if let Some(v) = p.is_repairable {
        am.is_repairable = Set(v);
    }
    if let Some(v) = p.review_period_days {
        am.review_period_days = Set(check_pos_opt("review_period_days", Some(v))?.unwrap_or(7));
    }
    if let Some(v) = p.lead_time_days {
        check_nonneg("lead_time_days", v as i64)?;
        am.lead_time_days = Set(v);
    }
    if let Some(v) = p.holding_cost_rate {
        check_money("holding_cost_rate", &v)?;
        am.holding_cost_rate = Set(v);
    }
    if let Some(v) = p.stockout_cost {
        check_money("stockout_cost", &v)?;
        am.stockout_cost = Set(Some(v));
    }
    if p.aql_defect_pct.is_some() {
        am.aql_defect_pct = Set(super::check_pct("aql_defect_pct", p.aql_defect_pct)?);
    }
    if let Some(v) = p.active {
        am.active = Set(v);
    }
    am.updated_at = Set(Utc::now());
    Ok(Json(am.update(&s.db).await?))
}

// ---- suppliers ---------------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct NewSupplier {
    code: String,
    name: String,
    contact_email: Option<String>,
    phone: Option<String>,
    lead_time_days: Option<i32>,
    #[serde(flatten)]
    profile: SupplierProfile,
}

/// Commercial attributes and the inputs of the structural risk model (all optional, 0-100 scales).
#[derive(Deserialize, Default)]
pub struct SupplierProfile {
    country: Option<String>,
    region: Option<String>,
    tier: Option<i32>,
    esg_score: Option<f64>,
    financial_stability_score: Option<f64>,
    geopolitical_risk_index: Option<f64>,
    alternatives_available: Option<i32>,
    environmental_compliance: Option<f64>,
    previous_disruptions: Option<i32>,
    payment_terms_days: Option<i32>,
}

fn check_score(field: &str, v: Option<f64>) -> AppResult<Option<f64>> {
    if matches!(v, Some(x) if !(0.0..=100.0).contains(&x)) {
        return Err(AppError::bad(format!("{field} must be between 0 and 100")));
    }
    Ok(v)
}

fn apply_profile(am: &mut supplier::ActiveModel, p: SupplierProfile) -> AppResult<()> {
    if let Some(v) = p.country { am.country = Set(Some(v)); }
    if let Some(v) = p.region { am.region = Set(Some(v)); }
    if let Some(v) = p.tier {
        if !(1..=3).contains(&v) { return Err(AppError::bad("tier must be 1, 2 or 3")); }
        am.tier = Set(Some(v));
    }
    if p.esg_score.is_some() { am.esg_score = Set(check_score("esg_score", p.esg_score)?); }
    if p.financial_stability_score.is_some() { am.financial_stability_score = Set(check_score("financial_stability_score", p.financial_stability_score)?); }
    if p.geopolitical_risk_index.is_some() { am.geopolitical_risk_index = Set(check_score("geopolitical_risk_index", p.geopolitical_risk_index)?); }
    if p.environmental_compliance.is_some() { am.environmental_compliance = Set(check_score("environmental_compliance", p.environmental_compliance)?); }
    if let Some(v) = p.alternatives_available {
        if v < 0 { return Err(AppError::bad("alternatives_available must not be negative")); }
        am.alternatives_available = Set(Some(v));
    }
    if let Some(v) = p.previous_disruptions {
        if v < 0 { return Err(AppError::bad("previous_disruptions must not be negative")); }
        am.previous_disruptions = Set(Some(v));
    }
    if let Some(v) = p.payment_terms_days {
        if v < 0 { return Err(AppError::bad("payment_terms_days must not be negative")); }
        am.payment_terms_days = Set(v);
    }
    Ok(())
}

async fn create_supplier(
    State(s): State<AppState>,
    Json(i): Json<NewSupplier>,
) -> AppResult<(StatusCode, Json<supplier::Model>)> {
    let lead = i.lead_time_days.unwrap_or(7);
    if lead < 0 {
        return Err(AppError::bad("lead_time_days must not be negative"));
    }
    let mut am = supplier::ActiveModel {
        id: Set(Uuid::new_v4()),
        code: Set(require_text("code", &i.code)?.to_uppercase()),
        name: Set(require_text("name", &i.name)?),
        contact_email: Set(i.contact_email),
        phone: Set(i.phone),
        lead_time_days: Set(lead),
        active: Set(true),
        created_at: Set(Utc::now()),
        ..Default::default()
    };
    apply_profile(&mut am, i.profile)?;
    let m = am.insert(&s.db).await?;
    Ok((StatusCode::CREATED, Json(m)))
}

async fn get_supplier(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<supplier::Model>> {
    Ok(Json(get_or_404!(supplier::Entity, &s.db, id, "supplier")))
}

/// Update risk inputs / commercial attributes.
async fn patch_supplier(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(p): Json<SupplierProfile>,
) -> AppResult<Json<supplier::Model>> {
    let existing = get_or_404!(supplier::Entity, &s.db, id, "supplier");
    let mut am: supplier::ActiveModel = existing.into();
    apply_profile(&mut am, p)?;
    Ok(Json(am.update(&s.db).await?))
}

async fn list_suppliers(
    State(s): State<AppState>,
    Query(q): Query<ListQuery>,
) -> AppResult<Json<Vec<supplier::Model>>> {
    let rows = supplier::Entity::find()
        .order_by_asc(supplier::Column::Code)
        .limit(page_limit(q.limit))
        .offset(q.offset.unwrap_or(0))
        .all(&s.db)
        .await?;
    Ok(Json(rows))
}

// ---- customers ---------------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct NewCustomer {
    code: String,
    name: String,
    email: Option<String>,
    address: Option<String>,
}

async fn create_customer(
    State(s): State<AppState>,
    Json(i): Json<NewCustomer>,
) -> AppResult<(StatusCode, Json<customer::Model>)> {
    let m = customer::ActiveModel {
        id: Set(Uuid::new_v4()),
        code: Set(require_text("code", &i.code)?.to_uppercase()),
        name: Set(require_text("name", &i.name)?),
        email: Set(i.email),
        address: Set(i.address),
        created_at: Set(Utc::now()),
    }
    .insert(&s.db)
    .await?;
    Ok((StatusCode::CREATED, Json(m)))
}

async fn list_customers(
    State(s): State<AppState>,
    Query(q): Query<ListQuery>,
) -> AppResult<Json<Vec<customer::Model>>> {
    let rows = customer::Entity::find()
        .order_by_asc(customer::Column::Code)
        .limit(page_limit(q.limit))
        .offset(q.offset.unwrap_or(0))
        .all(&s.db)
        .await?;
    Ok(Json(rows))
}

// ---- carriers ----------------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct NewCarrier {
    code: String,
    name: String,
    /// e.g. "https://carrier.example/track/{tracking_number}"
    tracking_url_template: Option<String>,
}

async fn create_carrier(
    State(s): State<AppState>,
    Json(i): Json<NewCarrier>,
) -> AppResult<(StatusCode, Json<carrier::Model>)> {
    let m = carrier::ActiveModel {
        id: Set(Uuid::new_v4()),
        code: Set(require_text("code", &i.code)?.to_uppercase()),
        name: Set(require_text("name", &i.name)?),
        tracking_url_template: Set(i.tracking_url_template),
        active: Set(true),
    }
    .insert(&s.db)
    .await?;
    Ok((StatusCode::CREATED, Json(m)))
}

async fn list_carriers(
    State(s): State<AppState>,
    Query(q): Query<ListQuery>,
) -> AppResult<Json<Vec<carrier::Model>>> {
    let rows = carrier::Entity::find()
        .order_by_asc(carrier::Column::Code)
        .limit(page_limit(q.limit))
        .offset(q.offset.unwrap_or(0))
        .all(&s.db)
        .await?;
    Ok(Json(rows))
}
