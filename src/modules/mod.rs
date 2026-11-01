use crate::state::AppState;
use axum::Router;

pub mod am;
pub mod bridges;
pub mod crm;
pub mod crm_import;
pub mod customers;
pub mod docs;
pub mod ehs;
pub mod grc;
pub mod pm;
pub mod portfolio;
pub mod qms;
pub mod sales;

pub fn router() -> Router<AppState> {
    Router::new()
        .merge(crm::router())
        .merge(sales::router())
        .merge(crm_import::router())
        .merge(customers::router())
        .merge(pm::router())
        .merge(portfolio::router())
        .merge(grc::router())
        .merge(am::router())
        .merge(bridges::router())
        .merge(qms::router())
        .merge(ehs::router())
        .merge(docs::router())
}
