//! Human resources: records, recruitment, payroll, benefits, training, performance, analytics.

pub mod analytics;
pub mod benefits;
pub mod compensation;
pub mod development;
pub mod employees;
pub mod pay_model;
pub mod payroll;
pub mod recruitment;

use axum::{
    routing::{get, post},
    Router,
};

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        // Employee records
        .route("/departments", post(employees::create_department).get(employees::list_departments))
        .route("/employees", post(employees::create_employee).get(employees::list_employees))
        .route("/employees/{id}", get(employees::get_employee).patch(employees::update_employee))
        .route("/employees/{id}/terminate", post(employees::terminate_employee))
        .route("/employees/{id}/benefits", post(benefits::enroll).get(benefits::list_for_employee))
        .route("/employees/{id}/training", get(development::training_for_employee))
        .route("/employees/{id}/payslips", get(payroll::payslips_for_employee))
        // Recruitment
        .route("/recruitment/postings", post(recruitment::create_posting).get(recruitment::list_postings))
        .route("/recruitment/postings/{id}/close", post(recruitment::close_posting))
        .route("/recruitment/postings/{id}/applications", post(recruitment::apply))
        .route("/recruitment/applications", get(recruitment::list_applications))
        .route("/recruitment/applications/{id}/advance", post(recruitment::advance))
        .route("/recruitment/applications/{id}/hire", post(recruitment::hire))
        // Payroll
        .route("/payroll/runs", post(payroll::create_run).get(payroll::list_runs))
        .route("/payroll/runs/{id}", get(payroll::get_run).delete(payroll::delete_draft))
        .route("/payroll/runs/{id}/approve", post(payroll::approve))
        .route("/payroll/runs/{id}/post", post(payroll::post_run))
        // Benefits
        .route("/benefit-plans", post(benefits::create_plan).get(benefits::list_plans))
        .route("/employee-benefits/{id}/end", post(benefits::end_enrollment))
        // Training
        .route("/training/courses", post(development::create_course).get(development::list_courses))
        .route("/training/enrollments", post(development::enroll))
        .route("/training/enrollments/{id}/complete", post(development::complete))
        // Performance
        .route("/performance/reviews", post(development::create_review).get(development::list_reviews))
        // Analytics
        .route("/analytics/summary", get(analytics::summary))
        .route("/compensation/benchmark", get(compensation::benchmark))
}
