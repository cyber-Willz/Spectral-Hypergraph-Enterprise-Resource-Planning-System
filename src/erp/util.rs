use chrono::NaiveDate;
use rust_decimal::Decimal;
use sea_orm::{DbBackend, Statement, Value};

use crate::error::AppError;

pub fn require_scale(v: Decimal, field: &str) -> Result<Decimal, AppError> {
    if v.round_dp(2) != v {
        return Err(AppError::BadRequest(format!("{field} must have at most 2 decimal places")));
    }
    Ok(v)
}

pub fn require_positive(v: Decimal, field: &str) -> Result<Decimal, AppError> {
    if v <= Decimal::ZERO {
        return Err(AppError::BadRequest(format!("{field} must be greater than zero")));
    }
    require_scale(v, field)
}

pub fn require_non_negative(v: Decimal, field: &str) -> Result<Decimal, AppError> {
    if v < Decimal::ZERO {
        return Err(AppError::BadRequest(format!("{field} must not be negative")));
    }
    require_scale(v, field)
}

pub fn non_empty(s: &str, field: &str) -> Result<String, AppError> {
    let t = s.trim();
    if t.is_empty() {
        return Err(AppError::BadRequest(format!("{field} must not be empty")));
    }
    Ok(t.to_owned())
}

pub fn clamp_limit(limit: Option<u64>) -> u64 {
    limit.unwrap_or(100).clamp(1, 500)
}

pub fn today() -> NaiveDate {
    chrono::Utc::now().date_naive()
}

pub fn stmt(sql: &str, values: Vec<Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    #[test]
    fn money_validation() {
        assert!(require_positive(d("10.25"), "x").is_ok());
        assert!(require_positive(d("0"), "x").is_err());
        assert!(require_positive(d("1.005"), "x").is_err());
        assert!(require_non_negative(d("-1"), "x").is_err());
        assert!(require_non_negative(d("0"), "x").is_ok());
    }
}
