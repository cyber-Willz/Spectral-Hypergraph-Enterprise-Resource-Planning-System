use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use sea_orm::{DbErr, SqlErr};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0} not found")]
    NotFound(String),
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Conflict(String),
    #[error("missing or invalid API key")]
    Unauthorized,
    #[error(transparent)]
    Db(#[from] DbErr),
}

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    pub fn not_found(what: &str) -> Self {
        AppError::NotFound(what.to_string())
    }
    pub fn bad(msg: impl Into<String>) -> Self {
        AppError::BadRequest(msg.into())
    }
    pub fn conflict(msg: impl Into<String>) -> Self {
        AppError::Conflict(msg.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::NotFound(_) => (StatusCode::NOT_FOUND, self.to_string()),
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, m.clone()),
            AppError::Conflict(m) => (StatusCode::CONFLICT, m.clone()),
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, self.to_string()),
            AppError::Db(e) => match e.sql_err() {
                Some(SqlErr::UniqueConstraintViolation(_)) => (
                    StatusCode::CONFLICT,
                    "a record with the same unique key already exists".to_string(),
                ),
                Some(SqlErr::ForeignKeyConstraintViolation(_)) => (
                    StatusCode::CONFLICT,
                    "referenced record does not exist or is still in use".to_string(),
                ),
                _ if e.to_string().contains("check constraint") => (
                    StatusCode::CONFLICT,
                    "operation violates a stock or quantity constraint".to_string(),
                ),
                _ => {
                    tracing::error!(error = %e, "database error");
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "internal server error".to_string(),
                    )
                }
            },
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}

/// Lets supply chain code run inside suite-level (bridge) handlers.
impl From<AppError> for crate::error::AppError {
    fn from(e: AppError) -> Self {
        match e {
            AppError::NotFound(m) => crate::error::AppError::NotFound(Some(format!("{m} not found"))),
            AppError::BadRequest(m) => crate::error::AppError::BadRequest(m),
            AppError::Conflict(m) => crate::error::AppError::Conflict(m),
            AppError::Unauthorized => crate::error::AppError::Unauthorized("missing or invalid credentials".into()),
            AppError::Db(e) => e.into(),
        }
    }
}
