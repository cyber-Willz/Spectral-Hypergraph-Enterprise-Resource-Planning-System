//! HR core: departments and employee records.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sea_orm::{
    prelude::*, ActiveValue::{NotSet, Set}, ConnectionTrait, QueryOrder, QuerySelect, QueryTrait,
    TransactionTrait,
};
use serde::Deserialize;

use crate::{
    erp::entities::{department, employee},
    error::{ApiResult, AppError, Created},
    erp::util::{clamp_limit, non_empty, require_non_negative, stmt},
    state::AppState,
};

#[derive(Deserialize)]
pub struct NewDepartment {
    name: String,
}

pub async fn create_department(State(st): State<AppState>, Json(req): Json<NewDepartment>) -> Created<department::Model> {
    let m = department::ActiveModel { id: NotSet, name: Set(non_empty(&req.name, "name")?), created_at: NotSet }
        .insert(&st.db)
        .await?;
    Ok((StatusCode::CREATED, Json(m)))
}

pub async fn list_departments(State(st): State<AppState>) -> ApiResult<Vec<department::Model>> {
    Ok(Json(department::Entity::find().order_by_asc(department::Column::Name).all(&st.db).await?))
}

pub fn valid_email(e: &str) -> Result<String, AppError> {
    let e = e.trim();
    match e.split_once('@') {
        Some((l, d)) if !l.is_empty() && d.contains('.') && !d.starts_with('.') && !d.ends_with('.') => Ok(e.to_lowercase()),
        _ => Err(AppError::BadRequest("email is not valid".into())),
    }
}

#[derive(Deserialize)]
pub struct NewEmployee {
    pub employee_number: String,
    pub first_name: String,
    pub last_name: String,
    pub email: String,
    pub department_id: Option<i64>,
    pub job_title: String,
    pub hire_date: NaiveDate,
    /// Annual salary.
    pub base_salary: Decimal,
    #[serde(default)]
    pub education_level: Option<String>,
    /// Total professional experience in whole years as of `hire_date`.
    #[serde(default)]
    pub years_experience: Option<i32>,
    /// Work-location cost tier, 1 (highest cost) to 3.
    #[serde(default)]
    pub city_tier: Option<i16>,
    #[serde(default)]
    pub remote_work: Option<bool>,
}

pub const EDUCATION_LEVELS: [&str; 4] = ["high_school", "bachelor", "master", "phd"];

/// Validates the optional benchmark attributes.
pub fn check_attributes(edu: &Option<String>, exp: Option<i32>, tier: Option<i16>) -> Result<(), AppError> {
    if let Some(e) = edu {
        if !EDUCATION_LEVELS.contains(&e.as_str()) {
            return Err(AppError::BadRequest(format!("education_level must be one of {EDUCATION_LEVELS:?}")));
        }
    }
    if let Some(x) = exp {
        if !(0..=60).contains(&x) {
            return Err(AppError::BadRequest("years_experience must be between 0 and 60".into()));
        }
    }
    if let Some(t) = tier {
        if !(1..=3).contains(&t) {
            return Err(AppError::BadRequest("city_tier must be 1, 2 or 3".into()));
        }
    }
    Ok(())
}

pub async fn insert_employee<C: ConnectionTrait>(c: &C, req: NewEmployee) -> Result<employee::Model, AppError> {
    check_attributes(&req.education_level, req.years_experience, req.city_tier)?;
    Ok(employee::ActiveModel {
        id: NotSet,
        employee_number: Set(non_empty(&req.employee_number, "employee_number")?),
        first_name: Set(non_empty(&req.first_name, "first_name")?),
        last_name: Set(non_empty(&req.last_name, "last_name")?),
        email: Set(valid_email(&req.email)?),
        department_id: Set(req.department_id),
        job_title: Set(non_empty(&req.job_title, "job_title")?),
        hire_date: Set(req.hire_date),
        termination_date: Set(None),
        status: Set("active".into()),
        base_salary: Set(require_non_negative(req.base_salary, "base_salary")?),
        created_at: NotSet,
        education_level: Set(req.education_level),
        years_experience: Set(req.years_experience),
        city_tier: Set(req.city_tier),
        remote_work: Set(req.remote_work),
    }
    .insert(c)
    .await?)
}

pub async fn create_employee(State(st): State<AppState>, Json(req): Json<NewEmployee>) -> Created<employee::Model> {
    Ok((StatusCode::CREATED, Json(insert_employee(&st.db, req).await?)))
}

#[derive(Deserialize)]
pub struct EmployeeQuery {
    status: Option<String>,
    department_id: Option<i64>,
    limit: Option<u64>,
}

pub async fn list_employees(
    State(st): State<AppState>,
    Query(q): Query<EmployeeQuery>,
) -> ApiResult<Vec<employee::Model>> {
    let rows = employee::Entity::find()
        .apply_if(q.status, |s, v| s.filter(employee::Column::Status.eq(v)))
        .apply_if(q.department_id, |s, v| s.filter(employee::Column::DepartmentId.eq(v)))
        .order_by_asc(employee::Column::EmployeeNumber)
        .limit(clamp_limit(q.limit))
        .all(&st.db)
        .await?;
    Ok(Json(rows))
}

pub async fn get_employee(State(st): State<AppState>, Path(id): Path<i64>) -> ApiResult<employee::Model> {
    employee::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .map(Json)
        .ok_or_else(|| AppError::not_found(format!("employee {id}")))
}

#[derive(Deserialize)]
pub struct UpdateEmployee {
    job_title: Option<String>,
    department_id: Option<i64>,
    base_salary: Option<Decimal>,
    email: Option<String>,
    education_level: Option<String>,
    years_experience: Option<i32>,
    city_tier: Option<i16>,
    remote_work: Option<bool>,
}

pub async fn update_employee(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<UpdateEmployee>,
) -> ApiResult<employee::Model> {
    check_attributes(&req.education_level, req.years_experience, req.city_tier)?;
    let txn = st.db.begin().await?;
    let e = employee::Entity::find_by_id(id)
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::not_found(format!("employee {id}")))?;
    if e.status != "active" {
        return Err(AppError::Conflict("terminated employees cannot be modified".into()));
    }
    let mut am: employee::ActiveModel = e.into();
    if let Some(t) = req.job_title {
        am.job_title = Set(non_empty(&t, "job_title")?);
    }
    if let Some(d) = req.department_id {
        am.department_id = Set(Some(d));
    }
    if let Some(s) = req.base_salary {
        am.base_salary = Set(require_non_negative(s, "base_salary")?);
    }
    if let Some(m) = req.email {
        am.email = Set(valid_email(&m)?);
    }
    if let Some(v) = req.education_level {
        am.education_level = Set(Some(v));
    }
    if let Some(v) = req.years_experience {
        am.years_experience = Set(Some(v));
    }
    if let Some(v) = req.city_tier {
        am.city_tier = Set(Some(v));
    }
    if let Some(v) = req.remote_work {
        am.remote_work = Set(Some(v));
    }
    let updated = am.update(&txn).await?;
    txn.commit().await?;
    Ok(Json(updated))
}

#[derive(Deserialize)]
pub struct TerminateReq {
    termination_date: NaiveDate,
}

/// Terminates an employee and closes their active benefit enrolments.
pub async fn terminate_employee(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<TerminateReq>,
) -> ApiResult<employee::Model> {
    let txn = st.db.begin().await?;
    let e = employee::Entity::find_by_id(id)
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::not_found(format!("employee {id}")))?;
    if e.status != "active" {
        return Err(AppError::Conflict("employee is already terminated".into()));
    }
    if req.termination_date < e.hire_date {
        return Err(AppError::BadRequest("termination_date must not be before hire_date".into()));
    }
    txn.execute(stmt(
        "UPDATE employee_benefits SET end_date = GREATEST($1::date, effective_date) \
         WHERE employee_id = $2 AND end_date IS NULL",
        vec![req.termination_date.into(), id.into()],
    ))
    .await?;
    let mut am: employee::ActiveModel = e.into();
    am.status = Set("terminated".into());
    am.termination_date = Set(Some(req.termination_date));
    let updated = am.update(&txn).await?;
    txn.commit().await?;
    Ok(Json(updated))
}

#[cfg(test)]
mod tests {
    use super::valid_email;

    #[test]
    fn email_validation() {
        assert_eq!(valid_email(" A@Example.com ").unwrap(), "a@example.com");
        assert!(valid_email("nope").is_err());
        assert!(valid_email("@x.com").is_err());
        assert!(valid_email("a@b").is_err());
    }
}
