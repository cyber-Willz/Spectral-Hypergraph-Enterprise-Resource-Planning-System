//! Risk: counterparty credit scoring, ledger-derived financial health, integrity checks.
//! Models were fitted and validated on the supplied datasets; see `docs/DATASET_EVALUATION.md`.

pub mod counterparty;
pub mod health;
pub mod integrity;
pub mod model;

use axum::{
    routing::{get, put},
    Router,
};

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/counterparties", get(counterparty::list_profiles))
        .route(
            "/counterparties/{kind}/{id}",
            put(counterparty::put_profile).get(counterparty::get_profile),
        )
        .route("/receivables-exposure", get(counterparty::receivables_exposure))
        .route("/financial-health", get(health::financial_health))
        .route("/integrity-checks", get(integrity::integrity_checks))
}
