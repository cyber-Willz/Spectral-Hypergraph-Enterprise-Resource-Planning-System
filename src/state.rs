use rust_decimal::Decimal;
use sea_orm::DatabaseConnection;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone)]
pub struct AppState {
    pub db: DatabaseConnection,
    pub jwt_secret: Arc<String>,
    /// Flat payroll withholding rate (placeholder for a real tax table).
    pub payroll_tax_rate: Decimal,
}

#[derive(Clone, Debug)]
pub struct AuthUser {
    pub id: Uuid,
    pub email: String,
    pub role: String,
}

impl AuthUser {
    pub fn require(&self, roles: &[&str]) -> Result<(), crate::error::AppError> {
        if roles.contains(&self.role.as_str()) {
            Ok(())
        } else {
            Err(crate::error::AppError::Forbidden(format!("requires one of roles: {}", roles.join(", "))))
        }
    }
}
