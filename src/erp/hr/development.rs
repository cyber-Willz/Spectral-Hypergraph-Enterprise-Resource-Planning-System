//! Training & development, and performance management.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sea_orm::{
    prelude::*, ActiveValue::{NotSet, Set}, QueryOrder, QuerySelect, QueryTrait, TransactionTrait,
};
use serde::Deserialize;

use crate::{
    erp::entities::{employee, performance_review, training_course, training_enrollment},
    error::{ApiResult, AppError, Created},
    erp::util::{clamp_limit, non_empty, require_non_negative, today},
    state::AppState,
};

// ───────────────────────── Training ─────────────────────────

#[derive(Deserialize)]
pub struct NewCourse {
    title: String,
    description: Option<String>,
    duration_hours: Decimal,
}

pub async fn create_course(State(st): State<AppState>, Json(req): Json<NewCourse>) -> Created<training_course::Model> {
    if req.duration_hours.round_dp(1) != req.duration_hours {
        return Err(AppError::BadRequest("duration_hours must have at most 1 decimal place".into()));
    }
    let m = training_course::ActiveModel {
        id: NotSet,
        title: Set(non_empty(&req.title, "title")?),
        description: Set(req.description),
        duration_hours: Set(require_non_negative(req.duration_hours, "duration_hours")?),
        created_at: NotSet,
    }
    .insert(&st.db)
    .await?;
    Ok((StatusCode::CREATED, Json(m)))
}

pub async fn list_courses(State(st): State<AppState>) -> ApiResult<Vec<training_course::Model>> {
    Ok(Json(training_course::Entity::find().order_by_asc(training_course::Column::Title).all(&st.db).await?))
}

#[derive(Deserialize)]
pub struct EnrollReq {
    employee_id: i64,
    course_id: i64,
}

pub async fn enroll(State(st): State<AppState>, Json(req): Json<EnrollReq>) -> Created<training_enrollment::Model> {
    let txn = st.db.begin().await?;
    let e = employee::Entity::find_by_id(req.employee_id)
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::BadRequest("employee does not exist".into()))?;
    if e.status != "active" {
        return Err(AppError::Conflict("employee is terminated".into()));
    }
    let m = training_enrollment::ActiveModel {
        id: NotSet,
        employee_id: Set(req.employee_id),
        course_id: Set(req.course_id),
        status: Set("enrolled".into()),
        enrolled_on: Set(today()),
        completed_on: Set(None),
        score: Set(None),
    }
    .insert(&txn)
    .await?;
    txn.commit().await?;
    Ok((StatusCode::CREATED, Json(m)))
}

#[derive(Deserialize)]
pub struct CompleteReq {
    score: Option<i16>,
    completed_on: Option<NaiveDate>,
}

pub async fn complete(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<CompleteReq>,
) -> ApiResult<training_enrollment::Model> {
    if let Some(s) = req.score {
        if !(0..=100).contains(&s) {
            return Err(AppError::BadRequest("score must be between 0 and 100".into()));
        }
    }
    let txn = st.db.begin().await?;
    let row = training_enrollment::Entity::find_by_id(id)
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::not_found(format!("enrolment {id}")))?;
    if row.status == "completed" {
        return Err(AppError::Conflict("already completed".into()));
    }
    let done = req.completed_on.unwrap_or_else(today);
    if done < row.enrolled_on {
        return Err(AppError::BadRequest("completed_on must not be before enrolled_on".into()));
    }
    let mut am: training_enrollment::ActiveModel = row.into();
    am.status = Set("completed".into());
    am.completed_on = Set(Some(done));
    am.score = Set(req.score);
    let updated = am.update(&txn).await?;
    txn.commit().await?;
    Ok(Json(updated))
}

pub async fn training_for_employee(
    State(st): State<AppState>,
    Path(employee_id): Path<i64>,
) -> ApiResult<Vec<training_enrollment::Model>> {
    let rows = training_enrollment::Entity::find()
        .filter(training_enrollment::Column::EmployeeId.eq(employee_id))
        .order_by_desc(training_enrollment::Column::EnrolledOn)
        .all(&st.db)
        .await?;
    Ok(Json(rows))
}

// ───────────────────────── Performance ─────────────────────────

#[derive(Deserialize)]
pub struct NewReview {
    employee_id: i64,
    reviewer_id: i64,
    period_label: String,
    period_start: NaiveDate,
    period_end: NaiveDate,
    /// 1 (lowest) to 5 (highest).
    rating: i16,
    strengths: Option<String>,
    improvements: Option<String>,
    comments: Option<String>,
}

pub async fn create_review(State(st): State<AppState>, Json(req): Json<NewReview>) -> Created<performance_review::Model> {
    if !(1..=5).contains(&req.rating) {
        return Err(AppError::BadRequest("rating must be between 1 and 5".into()));
    }
    if req.employee_id == req.reviewer_id {
        return Err(AppError::BadRequest("an employee cannot review themselves".into()));
    }
    if req.period_end < req.period_start {
        return Err(AppError::BadRequest("period_end must not be before period_start".into()));
    }
    let m = performance_review::ActiveModel {
        id: NotSet,
        employee_id: Set(req.employee_id),
        reviewer_id: Set(req.reviewer_id),
        period_label: Set(non_empty(&req.period_label, "period_label")?),
        period_start: Set(req.period_start),
        period_end: Set(req.period_end),
        rating: Set(req.rating),
        strengths: Set(req.strengths),
        improvements: Set(req.improvements),
        comments: Set(req.comments),
        created_at: NotSet,
    }
    .insert(&st.db)
    .await?;
    Ok((StatusCode::CREATED, Json(m)))
}

#[derive(Deserialize)]
pub struct ReviewQuery {
    employee_id: Option<i64>,
    limit: Option<u64>,
}

pub async fn list_reviews(
    State(st): State<AppState>,
    Query(q): Query<ReviewQuery>,
) -> ApiResult<Vec<performance_review::Model>> {
    let rows = performance_review::Entity::find()
        .apply_if(q.employee_id, |s, v| s.filter(performance_review::Column::EmployeeId.eq(v)))
        .order_by_desc(performance_review::Column::PeriodEnd)
        .limit(clamp_limit(q.limit))
        .all(&st.db)
        .await?;
    Ok(Json(rows))
}
