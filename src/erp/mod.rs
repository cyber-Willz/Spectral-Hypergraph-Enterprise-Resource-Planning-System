//! ERP core (formerly the standalone `erp_system`): Financial Management, Human Resources, Risk.
//! Tables are unprefixed (accounts, journal_entries, employees, ...) and live in the same database as
//! the suite; auth, RBAC, audit and error handling are shared with the rest of the application.

pub mod entities;
pub mod finance;
pub mod hr;
pub mod risk;
pub mod util;

use crate::state::AppState;
use axum::Router;

pub fn router() -> Router<AppState> {
    Router::new()
        .nest("/api/finance", finance::router())
        .nest("/api/hr", hr::router())
        .nest("/api/risk", risk::router())
}
