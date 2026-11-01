use axum::{http::StatusCode, response::{IntoResponse, Response}, Json};
use sea_orm::{DbErr, RuntimeErr};
use serde_json::json;

#[derive(Debug)]
pub enum AppError {
    BadRequest(String),
    Unauthorized(String),
    Forbidden(String),
    NotFound(Option<String>),
    Conflict(String),
    Unprocessable(String),
    Internal(String),
}

pub type AppResult<T> = Result<T, AppError>;
pub type ApiResult<T> = Result<Json<T>, AppError>;
pub type Created<T> = Result<(StatusCode, Json<T>), AppError>;

impl AppError {
    pub fn not_found(msg: impl Into<String>) -> Self { AppError::NotFound(Some(msg.into())) }
    pub fn internal() -> Self { AppError::Internal("unexpected internal state".into()) }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            AppError::Unauthorized(m) => (StatusCode::UNAUTHORIZED, m),
            AppError::Forbidden(m) => (StatusCode::FORBIDDEN, m),
            AppError::NotFound(m) => (StatusCode::NOT_FOUND, m.unwrap_or_else(|| "resource not found".to_string())),
            AppError::Conflict(m) => (StatusCode::CONFLICT, m),
            AppError::Unprocessable(m) => (StatusCode::UNPROCESSABLE_ENTITY, m),
            AppError::Internal(m) => {
                tracing::error!("internal error: {m}");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal server error".to_string())
            }
        };
        (status, Json(json!({ "error": msg }))).into_response()
    }
}

impl From<DbErr> for AppError {
    fn from(e: DbErr) -> Self {
        let sqlx_err = match &e {
            DbErr::Exec(RuntimeErr::SqlxError(x)) | DbErr::Query(RuntimeErr::SqlxError(x)) => Some(x),
            _ => None,
        };
        if let Some(sea_orm::sqlx::Error::Database(d)) = sqlx_err {
            let msg = d.message().to_string();
            return match d.code().as_deref() {
                Some("23505") => AppError::Conflict(format!("duplicate value: {msg}")),
                Some("23503") => AppError::Conflict(format!("referential integrity: {msg}")),
                Some("23502") => AppError::Unprocessable(format!("missing required field: {msg}")),
                Some("23514") => AppError::Unprocessable(msg),
                Some(c) if c.starts_with("22") => AppError::BadRequest(msg),
                _ => AppError::Internal(msg),
            };
        }
        match e {
            DbErr::RecordNotFound(_) => AppError::NotFound(None),
            DbErr::Json(m) | DbErr::Type(m) => AppError::BadRequest(m),
            other => AppError::Internal(other.to_string()),
        }
    }
}
