//! Generic CRUD router macro and query-parameter parsers used by generated entities.
use crate::error::{AppError, AppResult};
use sea_orm::prelude::{Date, DateTimeWithTimeZone, Decimal};
use std::str::FromStr;
use uuid::Uuid;

fn err<T>(name: &str, v: &str, what: &str) -> AppResult<T> {
    Err(AppError::BadRequest(format!("invalid {what} for '{name}': {v}")))
}
pub fn parse_uuid(n: &str, v: &str) -> AppResult<Uuid> { Uuid::parse_str(v).or_else(|_| err(n, v, "uuid")) }
pub fn parse_i32(n: &str, v: &str) -> AppResult<i32> { v.parse().or_else(|_| err(n, v, "integer")) }
pub fn parse_i64(n: &str, v: &str) -> AppResult<i64> { v.parse().or_else(|_| err(n, v, "integer")) }
pub fn parse_f64(n: &str, v: &str) -> AppResult<f64> { v.parse().or_else(|_| err(n, v, "number")) }
pub fn parse_bool(n: &str, v: &str) -> AppResult<bool> { v.parse().or_else(|_| err(n, v, "boolean")) }
pub fn parse_dec(n: &str, v: &str) -> AppResult<Decimal> { Decimal::from_str(v).or_else(|_| err(n, v, "decimal")) }
pub fn parse_date(n: &str, v: &str) -> AppResult<Date> {
    Date::parse_from_str(v, "%Y-%m-%d").or_else(|_| err(n, v, "date (YYYY-MM-DD)"))
}
pub fn parse_ts(n: &str, v: &str) -> AppResult<DateTimeWithTimeZone> {
    if let Ok(t) = DateTimeWithTimeZone::parse_from_rfc3339(v) {
        return Ok(t);
    }
    if let Ok(d) = Date::parse_from_str(v, "%Y-%m-%d") {
        let dt = d.and_hms_opt(0, 0, 0).unwrap().and_utc();
        return Ok(dt.fixed_offset());
    }
    err(n, v, "timestamp (RFC3339 or YYYY-MM-DD)")
}

/// Builds a `Router<AppState>` with list/get/create/update/delete for an entity module.
/// `$ops` is a string containing any of: l g c u d.
macro_rules! crud_router {
    ($m:ident, $ops:expr) => {{
        use crate::entities::$m as ent;
        use crate::error::{AppError, AppResult};
        use crate::state::AppState;
        use axum::extract::{Path, Query, State};
        use axum::http::StatusCode;
        use axum::{Json, Router};
        use sea_orm::{ActiveModelTrait, EntityTrait, PaginatorTrait, QueryOrder};
        use serde_json::{json, Value};
        use std::collections::HashMap;
        use std::str::FromStr;
        use uuid::Uuid;

        async fn list(State(st): State<AppState>, Query(p): Query<HashMap<String, String>>) -> AppResult<Json<Value>> {
            let page: u64 = p.get("page").and_then(|v| v.parse().ok()).unwrap_or(1u64).max(1);
            let per_page: u64 = p.get("per_page").and_then(|v| v.parse().ok()).unwrap_or(50u64).clamp(1, 500);
            let mut q = ent::apply_filters(ent::Entity::find(), &p)?;
            let desc = p.get("order").map(|o| o.eq_ignore_ascii_case("desc")).unwrap_or(true);
            q = match p.get("sort") {
                Some(s) => {
                    let col = ent::Column::from_str(s)
                        .map_err(|_| AppError::BadRequest(format!("unknown sort column '{s}'")))?;
                    if desc { q.order_by_desc(col) } else { q.order_by_asc(col) }
                }
                None => q.order_by_desc(ent::Column::CreatedAt),
            };
            let paginator = q.paginate(&st.db, per_page);
            let total = paginator.num_items().await?;
            let items = paginator.fetch_page(page - 1).await?;
            Ok(Json(json!({ "data": items, "page": page, "per_page": per_page, "total": total })))
        }
        async fn get_one(State(st): State<AppState>, Path(id): Path<Uuid>) -> AppResult<Json<ent::Model>> {
            Ok(Json(ent::Entity::find_by_id(id).one(&st.db).await?.ok_or(AppError::NotFound(None))?))
        }
        async fn create(State(st): State<AppState>, Json(mut body): Json<Value>) -> AppResult<(StatusCode, Json<ent::Model>)> {
            let obj = body.as_object_mut().ok_or_else(|| AppError::BadRequest("JSON object expected".into()))?;
            for k in ent::READ_ONLY { obj.remove(*k); }
            let am = ent::ActiveModel::from_json(body).map_err(|e| AppError::BadRequest(e.to_string()))?;
            Ok((StatusCode::CREATED, Json(am.insert(&st.db).await?)))
        }
        async fn update(State(st): State<AppState>, Path(id): Path<Uuid>, Json(mut body): Json<Value>) -> AppResult<Json<ent::Model>> {
            let obj = body.as_object_mut().ok_or_else(|| AppError::BadRequest("JSON object expected".into()))?;
            for k in ent::READ_ONLY { obj.remove(*k); }
            let model = ent::Entity::find_by_id(id).one(&st.db).await?.ok_or(AppError::NotFound(None))?;
            let mut am: ent::ActiveModel = model.into();
            am.set_from_json(body).map_err(|e| AppError::BadRequest(e.to_string()))?;
            Ok(Json(am.update(&st.db).await?))
        }
        async fn delete(State(st): State<AppState>, Path(id): Path<Uuid>) -> AppResult<StatusCode> {
            let r = ent::Entity::delete_by_id(id).exec(&st.db).await?;
            if r.rows_affected == 0 { Err(AppError::NotFound(None)) } else { Ok(StatusCode::NO_CONTENT) }
        }

        let ops: &str = $ops;
        let mut collection = axum::routing::MethodRouter::new();
        if ops.contains('l') { collection = collection.get(list); }
        if ops.contains('c') { collection = collection.post(create); }
        let mut item = axum::routing::MethodRouter::new();
        if ops.contains('g') { item = item.get(get_one); }
        if ops.contains('u') { item = item.put(update).patch(update); }
        if ops.contains('d') { item = item.delete(delete); }
        Router::new().route("/", collection).route("/{id}", item)
    }};
}
