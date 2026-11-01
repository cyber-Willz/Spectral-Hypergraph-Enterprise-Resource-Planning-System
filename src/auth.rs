use crate::entities::users;
use crate::error::{AppError, AppResult};
use crate::state::{AppState, AuthUser};
use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::{Path, Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::{get, patch};
use axum::{Extension, Json, Router};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use sea_orm::{ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, Set};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

pub const ROLES: &[&str] = &["admin", "manager", "user", "auditor", "finance", "hr"];

#[derive(Serialize, Deserialize)]
struct Claims { sub: Uuid, email: String, role: String, exp: usize }

pub fn hash_password(pw: &str) -> AppResult<String> {
    if pw.len() < 8 { return Err(AppError::BadRequest("password must be at least 8 characters".into())); }
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default().hash_password(pw.as_bytes(), &salt).map(|h| h.to_string())
        .map_err(|e| AppError::Internal(e.to_string()))
}

fn verify(pw: &str, hash: &str) -> bool {
    PasswordHash::new(hash).map(|h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok()).unwrap_or(false)
}

fn issue(st: &AppState, u: &users::Model) -> AppResult<String> {
    let exp = (chrono::Utc::now() + chrono::Duration::hours(12)).timestamp() as usize;
    let claims = Claims { sub: u.id, email: u.email.clone(), role: u.role.clone(), exp };
    encode(&Header::default(), &claims, &EncodingKey::from_secret(st.jwt_secret.as_bytes()))
        .map_err(|e| AppError::Internal(e.to_string()))
}

/// Creates the bootstrap administrator when the users table is empty.
pub async fn bootstrap_admin(st: &AppState, email: &str, password: &str) -> AppResult<()> {
    if users::Entity::find().count(&st.db).await? == 0 {
        users::ActiveModel {
            id: Set(Uuid::new_v4()), email: Set(email.to_lowercase()), full_name: Set("Administrator".into()),
            password_hash: Set(hash_password(password)?), role: Set("admin".into()), is_active: Set(true),
            ..Default::default()
        }.insert(&st.db).await?;
        tracing::info!("bootstrap admin created: {email}");
    }
    Ok(())
}

pub async fn login(State(st): State<AppState>, Json(b): Json<Value>) -> AppResult<Json<Value>> {
    let email = b["email"].as_str().unwrap_or_default().to_lowercase();
    let pw = b["password"].as_str().unwrap_or_default();
    let u = users::Entity::find().filter(users::Column::Email.eq(email)).one(&st.db).await?
        .filter(|u| u.is_active && verify(pw, &u.password_hash))
        .ok_or_else(|| AppError::Unauthorized("invalid credentials".into()))?;
    st.db.execute_unprepared(&format!("UPDATE users SET last_login_at = now() WHERE id = '{}'", u.id)).await?;
    Ok(Json(json!({ "token": issue(&st, &u)?, "token_type": "Bearer", "expires_in": 43200, "user": u })))
}

async fn me(Extension(u): Extension<AuthUser>) -> Json<Value> {
    Json(json!({ "id": u.id, "email": u.email, "role": u.role }))
}

async fn list_users(Extension(u): Extension<AuthUser>, State(st): State<AppState>) -> AppResult<Json<Value>> {
    u.require(&["admin"])?;
    let items = users::Entity::find().order_by_asc(users::Column::Email).all(&st.db).await?;
    Ok(Json(json!({ "data": items })))
}

async fn create_user(Extension(u): Extension<AuthUser>, State(st): State<AppState>, Json(b): Json<Value>) -> AppResult<(axum::http::StatusCode, Json<users::Model>)> {
    u.require(&["admin"])?;
    let role = b["role"].as_str().unwrap_or("user");
    if !ROLES.contains(&role) { return Err(AppError::BadRequest(format!("role must be one of {ROLES:?}"))); }
    let email = b["email"].as_str().ok_or_else(|| AppError::BadRequest("email required".into()))?;
    let m = users::ActiveModel {
        id: Set(Uuid::new_v4()), email: Set(email.to_lowercase()),
        full_name: Set(b["full_name"].as_str().unwrap_or(email).to_string()),
        password_hash: Set(hash_password(b["password"].as_str().unwrap_or_default())?),
        role: Set(role.into()), is_active: Set(true), ..Default::default()
    }.insert(&st.db).await?;
    Ok((axum::http::StatusCode::CREATED, Json(m)))
}

async fn update_user(Extension(u): Extension<AuthUser>, State(st): State<AppState>, Path(id): Path<Uuid>, Json(b): Json<Value>) -> AppResult<Json<users::Model>> {
    u.require(&["admin"])?;
    let m = users::Entity::find_by_id(id).one(&st.db).await?.ok_or(AppError::NotFound(None))?;
    let mut am: users::ActiveModel = m.into();
    if let Some(n) = b["full_name"].as_str() { am.full_name = Set(n.into()); }
    if let Some(r) = b["role"].as_str() {
        if !ROLES.contains(&r) { return Err(AppError::BadRequest(format!("role must be one of {ROLES:?}"))); }
        am.role = Set(r.into());
    }
    if let Some(a) = b["is_active"].as_bool() { am.is_active = Set(a); }
    if let Some(p) = b["password"].as_str() { am.password_hash = Set(hash_password(p)?); }
    Ok(Json(am.update(&st.db).await?))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/auth/me", get(me))
        .route("/api/users", get(list_users).post(create_user))
        .route("/api/users/{id}", patch(update_user).put(update_user))
}

/// Authentication + role-based access + audit logging.
/// auditor: read-only. DELETE requires admin/manager. Mutations are recorded in sys_audit_log.
pub async fn middleware(State(st): State<AppState>, mut req: Request, next: Next) -> Result<Response, AppError> {
    let token = req.headers().get("authorization").and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer ")).ok_or_else(|| AppError::Unauthorized("missing bearer token".into()))?;
    let data = decode::<Claims>(token, &DecodingKey::from_secret(st.jwt_secret.as_bytes()), &Validation::default())
        .map_err(|_| AppError::Unauthorized("invalid or expired token".into()))?;
    let user = AuthUser { id: data.claims.sub, email: data.claims.email, role: data.claims.role };
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let mutating = method != Method::GET && method != Method::HEAD && method != Method::OPTIONS;
    authorize(&user.role, &method, &path)?;
    req.extensions_mut().insert(user.clone());
    let resp = next.run(req).await;
    if mutating {
        let status = resp.status().as_u16() as i32;
        let stmt = sea_orm::Statement::from_sql_and_values(sea_orm::DbBackend::Postgres,
            "INSERT INTO sys_audit_log (user_id, email, method, path, status_code) VALUES ($1,$2,$3,$4,$5)",
            [user.id.into(), user.email.into(), method.as_str().into(), path.into(), status.into()]);
        if let Err(e) = st.db.execute(stmt).await { tracing::warn!("audit log failed: {e}"); }
    }
    Ok(resp)
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum Area { Finance, Hr, Audit, Ops }

fn area(path: &str) -> Area {
    let under = |p: &str| path == p || path.starts_with(&format!("{p}/"));
    if under("/api/finance") || under("/api/risk") { Area::Finance }
    else if under("/api/hr") { Area::Hr }
    else if under("/api/sys") { Area::Audit }
    else { Area::Ops }
}

/// Cross-module bridge endpoints that a finance/hr user may trigger although they live under an operational module.
fn is_bridge(role: &str, path: &str) -> bool {
    match role {
        "finance" => path.ends_with("/sync-customer") || path.ends_with("/sync-vendor") || path.ends_with("/post-to-ledger") || path.ends_with("/register-fixed-asset")
            || (path.starts_with("/api/scm/sales-orders/") && path.ends_with("/invoice"))
            || (path.starts_with("/api/crm/opportunities/") && path.ends_with("/invoice")),
        "hr" => path == "/api/pm/resources/sync-employees",
        _ => false,
    }
}

/// Role policy (applied to every request before the handler runs).
///
/// | role    | operational modules (CRM, PM, GRC, assets, QMS, EHS, docs) | finance + risk | HR (salary data) | audit log |
/// |---------|------|------|------|------|
/// | admin   | read/write | read/write | read/write | read |
/// | manager | read/write | read/write | read/write | read |
/// | auditor | read | read | read | read |
/// | user    | read/write | none | none | none |
/// | finance | read | read/write | none | none |
/// | hr      | read | none | read/write | none |
///
/// DELETE needs admin/manager, or the owning role of that area (finance/hr).
pub fn authorize(role: &str, method: &Method, path: &str) -> Result<(), AppError> {
    let write = method != Method::GET && method != Method::HEAD && method != Method::OPTIONS;
    let a = area(path);
    let deny = |m: &str| Err(AppError::Forbidden(m.to_string()));
    if write && role == "auditor" { return deny("auditor role is read-only"); }
    let owner = matches!((role, a), ("finance", Area::Finance) | ("hr", Area::Hr));
    if *method == Method::DELETE && !(matches!(role, "admin" | "manager") || owner) {
        return deny("DELETE requires role admin or manager (or the owning finance/hr role)");
    }
    let (read_ok, write_ok) = match (role, a) {
        ("admin" | "manager", _) => (true, true),
        ("auditor", _) => (true, false),
        (_, Area::Audit) => (false, false),
        ("user", Area::Ops) => (true, true),
        ("user", _) => (false, false),
        ("finance", Area::Finance) => (true, true),
        ("finance", Area::Ops) => (true, false),
        ("hr", Area::Hr) => (true, true),
        ("hr", Area::Ops) => (true, false),
        _ => (false, false),
    };
    let write_ok = write_ok || (write && is_bridge(role, path));
    if write && !write_ok || !write && !read_ok {
        return deny(&format!("role '{role}' may not {} this resource", if write { "modify" } else { "read" }));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(r: &str, m: Method, p: &str) -> bool { authorize(r, &m, p).is_ok() }

    #[test]
    fn policy_matrix() {
        assert!(ok("admin", Method::POST, "/api/hr/payroll/runs"));
        assert!(ok("manager", Method::DELETE, "/api/hr/payroll/runs/1"));
        assert!(!ok("user", Method::GET, "/api/hr/employees"));
        assert!(!ok("user", Method::GET, "/api/finance/accounts"));
        assert!(ok("user", Method::POST, "/api/crm/accounts"));
        assert!(!ok("user", Method::DELETE, "/api/crm/accounts/x"));
        assert!(ok("auditor", Method::GET, "/api/hr/employees"));
        assert!(!ok("auditor", Method::POST, "/api/finance/journal-entries"));
        assert!(ok("finance", Method::POST, "/api/finance/bills"));
        assert!(ok("finance", Method::GET, "/api/risk/integrity-checks"));
        assert!(!ok("finance", Method::GET, "/api/hr/employees"));
        assert!(!ok("finance", Method::POST, "/api/crm/accounts"));
        assert!(ok("finance", Method::GET, "/api/crm/accounts"));
        assert!(ok("finance", Method::POST, "/api/crm/opportunities/abc/invoice"));
        assert!(!ok("finance", Method::POST, "/api/crm/opportunities/abc/stage"));
        assert!(ok("hr", Method::POST, "/api/pm/resources/sync-employees"));
        assert!(!ok("hr", Method::POST, "/api/pm/projects"));
        assert!(ok("hr", Method::POST, "/api/hr/employees"));
        assert!(ok("hr", Method::DELETE, "/api/hr/payroll/runs/1"));
        assert!(!ok("hr", Method::DELETE, "/api/finance/bills/1"));
        assert!(!ok("hr", Method::GET, "/api/finance/reports/trial-balance"));
        assert!(!ok("hr", Method::GET, "/api/sys/audit-log"));
        assert!(ok("auditor", Method::GET, "/api/sys/audit-log"));
        assert!(!ok("user", Method::GET, "/api/sys/audit-log"));
    }

    #[test]
    fn segment_aware_prefix() {
        assert_eq!(area("/api/hr"), Area::Hr);
        assert_eq!(area("/api/hr/employees"), Area::Hr);
        assert_eq!(area("/api/hrx/y"), Area::Ops);
        assert_eq!(area("/api/finance/x"), Area::Finance);
    }
}
