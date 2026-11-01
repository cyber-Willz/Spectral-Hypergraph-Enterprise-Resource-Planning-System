//! Supply chain, logistics, warehouse and inventory management (formerly the standalone `supply_chain`
//! service). Mounted under `/api/scm` behind the suite's JWT auth, role policy and audit logging.
//! Tables are `scm_`-prefixed only where they would collide with the ERP (`scm_customers`); the rest
//! (products, suppliers, warehouses, purchase_orders, ...) keep their names. Cross-system flows with the
//! ERP core, CRM and GRC live in `bridges`.

/// Fetch a row by primary key or return a 404 `AppError`.
macro_rules! get_or_404 {
    ($entity:path, $db:expr, $id:expr, $what:expr) => {
        <$entity as sea_orm::EntityTrait>::find_by_id($id)
            .one($db)
            .await?
            .ok_or_else(|| crate::supply_chain::error::AppError::not_found($what))?
    };
}

pub mod bridges;
pub mod cognitive;
pub mod entities;
pub mod error;
pub mod forecast;
pub mod resilience;
pub mod risk;
pub mod routes;
pub mod state;
pub mod stats;
pub mod stock;

pub use routes::router;
