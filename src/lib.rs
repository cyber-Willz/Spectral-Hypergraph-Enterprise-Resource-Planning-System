#[macro_use]
pub mod crud;
pub mod auth;
pub mod db;
pub mod entities;
pub mod erp;
pub mod error;
pub mod modules;
pub mod routes_generated;
pub mod state;
pub mod supply_chain;

use axum::{middleware, routing::{get, post}, Json, Router};
use serde_json::{json, Value};
use state::AppState;
use tower_http::{cors::CorsLayer, services::{ServeDir, ServeFile}, trace::TraceLayer};

async fn health(axum::extract::State(st): axum::extract::State<AppState>) -> Json<Value> {
    let ok = db::rows(&st.db, "SELECT 1 AS ok", vec![]).await.is_ok();
    Json(json!({ "status": if ok { "ok" } else { "degraded" }, "database": ok, "version": env!("CARGO_PKG_VERSION") }))
}

/// Directory holding the built Yew frontend: `FRONTEND_DIST`, else `frontend/dist` if present.
fn frontend_dir() -> Option<String> {
    let dir = std::env::var("FRONTEND_DIST").ok().filter(|d| !d.is_empty())
        .or_else(|| std::path::Path::new("frontend/dist/index.html").exists().then(|| "frontend/dist".to_string()))?;
    std::path::Path::new(&dir).join("index.html").exists().then_some(dir)
}

/// The full application: every module behind JWT auth + role policy + audit logging.
pub fn build_app(st: AppState) -> Router {
    let protected = routes_generated::router()
        .merge(modules::router())
        .merge(erp::router())
        .nest("/api/scm", supply_chain::router())
        .merge(auth::router())
        .layer(middleware::from_fn_with_state(st.clone(), auth::middleware));

    let app = Router::new()
        .route("/health", get(health))
        .route("/api/auth/login", post(auth::login))
        .merge(protected);
    // Serve the built web UI (frontend/dist) from the same origin when it exists or FRONTEND_DIST points at it.
    let app = match frontend_dir() {
        Some(dir) => {
            let index = format!("{dir}/index.html");
            app.fallback_service(ServeDir::new(dir).not_found_service(ServeFile::new(index)))
        }
        None => app,
    };
    app
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(st)
}
