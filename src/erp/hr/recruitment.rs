//! Recruitment: job postings, applications, pipeline stages and hiring.

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

use super::employees::{insert_employee, valid_email, NewEmployee};
use crate::{
    erp::entities::{application, employee, job_posting},
    error::{ApiResult, AppError, Created},
    erp::util::{clamp_limit, non_empty, today},
    state::AppState,
};

#[derive(Deserialize)]
pub struct NewPosting {
    title: String,
    department_id: Option<i64>,
    description: Option<String>,
}

pub async fn create_posting(State(st): State<AppState>, Json(req): Json<NewPosting>) -> Created<job_posting::Model> {
    let m = job_posting::ActiveModel {
        id: NotSet,
        title: Set(non_empty(&req.title, "title")?),
        department_id: Set(req.department_id),
        description: Set(req.description),
        status: Set("open".into()),
        opened_on: Set(today()),
        closed_on: Set(None),
        created_at: NotSet,
    }
    .insert(&st.db)
    .await?;
    Ok((StatusCode::CREATED, Json(m)))
}

#[derive(Deserialize)]
pub struct PostingQuery {
    status: Option<String>,
}

pub async fn list_postings(State(st): State<AppState>, Query(q): Query<PostingQuery>) -> ApiResult<Vec<job_posting::Model>> {
    let rows = job_posting::Entity::find()
        .apply_if(q.status, |s, v| s.filter(job_posting::Column::Status.eq(v)))
        .order_by_desc(job_posting::Column::OpenedOn)
        .all(&st.db)
        .await?;
    Ok(Json(rows))
}

pub async fn close_posting(State(st): State<AppState>, Path(id): Path<i64>) -> ApiResult<job_posting::Model> {
    let txn = st.db.begin().await?;
    let p = job_posting::Entity::find_by_id(id)
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::not_found(format!("job posting {id}")))?;
    if p.status != "open" {
        return Err(AppError::Conflict("posting is already closed".into()));
    }
    let mut am: job_posting::ActiveModel = p.into();
    am.status = Set("closed".into());
    am.closed_on = Set(Some(today()));
    let updated = am.update(&txn).await?;
    txn.commit().await?;
    Ok(Json(updated))
}

#[derive(Deserialize)]
pub struct NewApplication {
    first_name: String,
    last_name: String,
    email: String,
    phone: Option<String>,
}

pub async fn apply(
    State(st): State<AppState>,
    Path(posting_id): Path<i64>,
    Json(req): Json<NewApplication>,
) -> Created<application::Model> {
    let p = job_posting::Entity::find_by_id(posting_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found(format!("job posting {posting_id}")))?;
    if p.status != "open" {
        return Err(AppError::Conflict("posting is closed".into()));
    }
    let m = application::ActiveModel {
        id: NotSet,
        posting_id: Set(posting_id),
        first_name: Set(non_empty(&req.first_name, "first_name")?),
        last_name: Set(non_empty(&req.last_name, "last_name")?),
        email: Set(valid_email(&req.email)?),
        phone: Set(req.phone),
        stage: Set("applied".into()),
        notes: Set(None),
        applied_on: Set(today()),
        employee_id: Set(None),
        created_at: NotSet,
    }
    .insert(&st.db)
    .await?;
    Ok((StatusCode::CREATED, Json(m)))
}

#[derive(Deserialize)]
pub struct ApplicationQuery {
    posting_id: Option<i64>,
    stage: Option<String>,
    limit: Option<u64>,
}

pub async fn list_applications(
    State(st): State<AppState>,
    Query(q): Query<ApplicationQuery>,
) -> ApiResult<Vec<application::Model>> {
    let rows = application::Entity::find()
        .apply_if(q.posting_id, |s, v| s.filter(application::Column::PostingId.eq(v)))
        .apply_if(q.stage, |s, v| s.filter(application::Column::Stage.eq(v)))
        .order_by_desc(application::Column::AppliedOn)
        .order_by_desc(application::Column::Id)
        .limit(clamp_limit(q.limit))
        .all(&st.db)
        .await?;
    Ok(Json(rows))
}

fn rank(stage: &str) -> Option<u8> {
    match stage {
        "applied" => Some(0),
        "screening" => Some(1),
        "interview" => Some(2),
        "offer" => Some(3),
        _ => None,
    }
}

/// Whether moving from `current` to `next` is allowed via the advance endpoint.
pub fn can_advance(current: &str, next: &str) -> bool {
    let Some(cur) = rank(current) else { return false }; // hired / rejected are terminal
    match next {
        "rejected" => true,
        _ => rank(next).map(|n| n > cur).unwrap_or(false), // "hired" only via /hire
    }
}

#[derive(Deserialize)]
pub struct AdvanceReq {
    stage: String,
    notes: Option<String>,
}

pub async fn advance(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<AdvanceReq>,
) -> ApiResult<application::Model> {
    let txn = st.db.begin().await?;
    let a = application::Entity::find_by_id(id)
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::not_found(format!("application {id}")))?;
    if !can_advance(&a.stage, &req.stage) {
        return Err(AppError::BadRequest(format!(
            "cannot move application from '{}' to '{}' (use /hire to hire; hired and rejected are final)",
            a.stage, req.stage
        )));
    }
    let mut am: application::ActiveModel = a.into();
    am.stage = Set(req.stage);
    if let Some(n) = req.notes {
        am.notes = Set(Some(n));
    }
    let updated = am.update(&txn).await?;
    txn.commit().await?;
    Ok(Json(updated))
}

#[derive(Deserialize)]
pub struct HireReq {
    employee_number: String,
    department_id: Option<i64>,
    job_title: String,
    hire_date: NaiveDate,
    base_salary: Decimal,
    #[serde(default)]
    education_level: Option<String>,
    #[serde(default)]
    years_experience: Option<i32>,
    #[serde(default)]
    city_tier: Option<i16>,
    #[serde(default)]
    remote_work: Option<bool>,
}

/// Converts an application at the `offer` stage into an employee record.
pub async fn hire(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<HireReq>,
) -> Created<employee::Model> {
    let txn = st.db.begin().await?;
    let a = application::Entity::find_by_id(id)
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::not_found(format!("application {id}")))?;
    if a.stage != "offer" {
        return Err(AppError::BadRequest("only applications at the 'offer' stage can be hired".into()));
    }
    let emp = insert_employee(
        &txn,
        NewEmployee {
            employee_number: req.employee_number,
            first_name: a.first_name.clone(),
            last_name: a.last_name.clone(),
            email: a.email.clone(),
            department_id: req.department_id,
            job_title: req.job_title,
            hire_date: req.hire_date,
            base_salary: req.base_salary,
            education_level: req.education_level,
            years_experience: req.years_experience,
            city_tier: req.city_tier,
            remote_work: req.remote_work,
        },
    )
    .await?;
    let mut am: application::ActiveModel = a.into();
    am.stage = Set("hired".into());
    am.employee_id = Set(Some(emp.id));
    am.update(&txn).await?;
    txn.commit().await?;
    Ok((StatusCode::CREATED, Json(emp)))
}

#[cfg(test)]
mod tests {
    use super::can_advance;

    #[test]
    fn pipeline_rules() {
        assert!(can_advance("applied", "screening"));
        assert!(can_advance("screening", "offer"));
        assert!(can_advance("interview", "rejected"));
        assert!(!can_advance("offer", "interview"));
        assert!(!can_advance("offer", "hired"));
        assert!(!can_advance("rejected", "screening"));
        assert!(!can_advance("hired", "rejected"));
    }
}
