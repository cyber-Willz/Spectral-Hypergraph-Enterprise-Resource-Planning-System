//! Benefits management: plans and employee enrolments.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sea_orm::{
    prelude::*, ActiveValue::{NotSet, Set}, QueryOrder, QuerySelect, TransactionTrait,
};
use serde::Deserialize;

use crate::{
    erp::entities::{benefit_plan, employee, employee_benefit},
    error::{ApiResult, AppError, Created},
    erp::util::{non_empty, require_non_negative},
    state::AppState,
};

#[derive(Deserialize)]
pub struct NewPlan {
    name: String,
    plan_type: String,
    /// Deducted from the employee each payroll run.
    employee_cost: Decimal,
    /// Paid by the employer each payroll run.
    employer_cost: Decimal,
}

pub async fn create_plan(State(st): State<AppState>, Json(req): Json<NewPlan>) -> Created<benefit_plan::Model> {
    let m = benefit_plan::ActiveModel {
        id: NotSet,
        name: Set(non_empty(&req.name, "name")?),
        plan_type: Set(non_empty(&req.plan_type, "plan_type")?),
        employee_cost: Set(require_non_negative(req.employee_cost, "employee_cost")?),
        employer_cost: Set(require_non_negative(req.employer_cost, "employer_cost")?),
        is_active: Set(true),
        created_at: NotSet,
    }
    .insert(&st.db)
    .await?;
    Ok((StatusCode::CREATED, Json(m)))
}

pub async fn list_plans(State(st): State<AppState>) -> ApiResult<Vec<benefit_plan::Model>> {
    Ok(Json(benefit_plan::Entity::find().order_by_asc(benefit_plan::Column::Name).all(&st.db).await?))
}

#[derive(Deserialize)]
pub struct EnrollReq {
    plan_id: i64,
    effective_date: NaiveDate,
}

pub async fn enroll(
    State(st): State<AppState>,
    Path(employee_id): Path<i64>,
    Json(req): Json<EnrollReq>,
) -> Created<employee_benefit::Model> {
    let txn = st.db.begin().await?;
    let e = employee::Entity::find_by_id(employee_id)
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::not_found(format!("employee {employee_id}")))?;
    if e.status != "active" {
        return Err(AppError::Conflict("employee is terminated".into()));
    }
    let plan = benefit_plan::Entity::find_by_id(req.plan_id)
        .one(&txn)
        .await?
        .filter(|p| p.is_active)
        .ok_or_else(|| AppError::BadRequest("plan does not exist or is inactive".into()))?;
    let m = employee_benefit::ActiveModel {
        id: NotSet,
        employee_id: Set(employee_id),
        plan_id: Set(plan.id),
        effective_date: Set(req.effective_date),
        end_date: Set(None),
        created_at: NotSet,
    }
    .insert(&txn)
    .await?;
    txn.commit().await?;
    Ok((StatusCode::CREATED, Json(m)))
}

pub async fn list_for_employee(
    State(st): State<AppState>,
    Path(employee_id): Path<i64>,
) -> ApiResult<Vec<employee_benefit::Model>> {
    let rows = employee_benefit::Entity::find()
        .filter(employee_benefit::Column::EmployeeId.eq(employee_id))
        .order_by_desc(employee_benefit::Column::EffectiveDate)
        .all(&st.db)
        .await?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct EndReq {
    end_date: NaiveDate,
}

pub async fn end_enrollment(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<EndReq>,
) -> ApiResult<employee_benefit::Model> {
    let txn = st.db.begin().await?;
    let row = employee_benefit::Entity::find_by_id(id)
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::not_found(format!("enrolment {id}")))?;
    if row.end_date.is_some() {
        return Err(AppError::Conflict("enrolment has already ended".into()));
    }
    if req.end_date < row.effective_date {
        return Err(AppError::BadRequest("end_date must not be before effective_date".into()));
    }
    let mut am: employee_benefit::ActiveModel = row.into();
    am.end_date = Set(Some(req.end_date));
    let updated = am.update(&txn).await?;
    txn.commit().await?;
    Ok(Json(updated))
}
