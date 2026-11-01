//! Counterparty credit profiles, scores and receivables exposure.

use std::collections::BTreeMap;

use axum::{
    extract::{Path, Query, State},
    Json,
};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use sea_orm::{
    prelude::*, ActiveValue::{NotSet, Set}, FromQueryResult, QueryOrder, TransactionTrait,
};
use serde::{Deserialize, Serialize};

use super::model::{counterparty_score, CounterpartyInputs};
use crate::{
    erp::entities::{counterparty_risk_profile as profile, customer, vendor},
    error::{ApiResult, AppError},
    erp::util::{clamp_limit, stmt, today},
    state::AppState,
};

#[derive(Serialize, Clone)]
pub struct ScoreView {
    /// Relative risk in [0,1] (1.0 and band "bankrupt" when flagged bankrupt).
    probability: f64,
    band: &'static str,
    /// True when an input lies outside the range the model was fitted on.
    extrapolated: bool,
}

pub fn score_inputs(credit: i32, debt: Decimal, liq: Decimal, bankrupt: bool) -> ScoreView {
    if bankrupt {
        return ScoreView { probability: 1.0, band: "bankrupt", extrapolated: false };
    }
    let s = counterparty_score(CounterpartyInputs {
        credit_score: credit as f64,
        debt_ratio: debt.to_f64().unwrap_or(0.0),
        liquidity: liq.to_f64().unwrap_or(0.0),
    });
    ScoreView { probability: (s.probability * 10_000.0).round() / 10_000.0, band: s.band, extrapolated: s.extrapolated }
}

#[derive(Serialize)]
pub struct ProfileView {
    #[serde(flatten)]
    profile: profile::Model,
    risk: ScoreView,
}

fn view(p: profile::Model) -> ProfileView {
    let risk = score_inputs(p.credit_score, p.debt_ratio, p.liquidity, p.is_bankrupt);
    ProfileView { profile: p, risk }
}

fn check_kind(kind: &str) -> Result<(), AppError> {
    match kind {
        "customer" | "vendor" => Ok(()),
        _ => Err(AppError::BadRequest("kind must be 'customer' or 'vendor'".into())),
    }
}

#[derive(Deserialize)]
pub struct ProfileReq {
    credit_score: i32,
    debt_ratio: Decimal,
    liquidity: Decimal,
    #[serde(default)]
    legal_cases: i32,
    #[serde(default)]
    is_bankrupt: bool,
}

/// Creates or replaces a counterparty's risk profile.
pub async fn put_profile(
    State(st): State<AppState>,
    Path((kind, id)): Path<(String, i64)>,
    Json(req): Json<ProfileReq>,
) -> ApiResult<ProfileView> {
    check_kind(&kind)?;
    if !(300..=850).contains(&req.credit_score) {
        return Err(AppError::BadRequest("credit_score must be between 300 and 850".into()));
    }
    if req.debt_ratio < Decimal::ZERO || req.debt_ratio > Decimal::from(10) {
        return Err(AppError::BadRequest("debt_ratio must be between 0 and 10 (liabilities / assets)".into()));
    }
    if req.liquidity < Decimal::ZERO || req.liquidity > Decimal::from(100) {
        return Err(AppError::BadRequest("liquidity must be between 0 and 100 (current ratio)".into()));
    }
    if req.legal_cases < 0 {
        return Err(AppError::BadRequest("legal_cases must not be negative".into()));
    }
    let txn = st.db.begin().await?;
    let exists = if kind == "customer" {
        customer::Entity::find_by_id(id).count(&txn).await?
    } else {
        vendor::Entity::find_by_id(id).count(&txn).await?
    };
    if exists == 0 {
        return Err(AppError::not_found(format!("{kind} {id}")));
    }
    let now = chrono::Utc::now();
    let existing = profile::Entity::find()
        .filter(profile::Column::PartyKind.eq(kind.as_str()))
        .filter(profile::Column::PartyId.eq(id))
        .one(&txn)
        .await?;
    let saved = match existing {
        Some(row) => {
            let mut am: profile::ActiveModel = row.into();
            am.credit_score = Set(req.credit_score);
            am.debt_ratio = Set(req.debt_ratio.round_dp(3));
            am.liquidity = Set(req.liquidity.round_dp(3));
            am.legal_cases = Set(req.legal_cases);
            am.is_bankrupt = Set(req.is_bankrupt);
            am.assessed_on = Set(today());
            am.updated_at = Set(now);
            am.update(&txn).await?
        }
        None => {
            profile::ActiveModel {
                id: NotSet,
                party_kind: Set(kind),
                party_id: Set(id),
                credit_score: Set(req.credit_score),
                debt_ratio: Set(req.debt_ratio.round_dp(3)),
                liquidity: Set(req.liquidity.round_dp(3)),
                legal_cases: Set(req.legal_cases),
                is_bankrupt: Set(req.is_bankrupt),
                assessed_on: Set(today()),
                updated_at: Set(now),
            }
            .insert(&txn)
            .await?
        }
    };
    txn.commit().await?;
    Ok(Json(view(saved)))
}

pub async fn get_profile(State(st): State<AppState>, Path((kind, id)): Path<(String, i64)>) -> ApiResult<ProfileView> {
    check_kind(&kind)?;
    profile::Entity::find()
        .filter(profile::Column::PartyKind.eq(kind.as_str()))
        .filter(profile::Column::PartyId.eq(id))
        .one(&st.db)
        .await?
        .map(|p| Json(view(p)))
        .ok_or_else(|| AppError::not_found(format!("no risk profile for {kind} {id}")))
}

#[derive(Deserialize)]
pub struct ListQuery {
    kind: Option<String>,
    band: Option<String>,
    limit: Option<u64>,
    offset: Option<u64>,
}

pub async fn list_profiles(State(st): State<AppState>, Query(q): Query<ListQuery>) -> ApiResult<Vec<ProfileView>> {
    use sea_orm::QueryTrait;
    if let Some(k) = &q.kind {
        check_kind(k)?;
    }
    let rows = profile::Entity::find()
        .apply_if(q.kind, |s, v| s.filter(profile::Column::PartyKind.eq(v)))
        .order_by_asc(profile::Column::Id)
        .all(&st.db)
        .await?;
    let out = rows
        .into_iter()
        .map(view)
        .filter(|v| q.band.as_deref().map_or(true, |b| v.risk.band == b))
        .skip(q.offset.unwrap_or(0) as usize)
        .take(clamp_limit(q.limit) as usize)
        .collect();
    Ok(Json(out))
}

// ───────────────────────── Receivables exposure ─────────────────────────

#[derive(FromQueryResult)]
struct ExposureRow {
    customer_id: i64,
    name: String,
    outstanding: Decimal,
    overdue: Decimal,
    credit_score: Option<i32>,
    debt_ratio: Option<Decimal>,
    liquidity: Option<Decimal>,
    is_bankrupt: Option<bool>,
}

#[derive(Serialize)]
pub struct CustomerExposure {
    customer_id: i64,
    name: String,
    outstanding: Decimal,
    overdue: Decimal,
    risk: Option<ScoreView>,
    /// outstanding x relative risk. A prioritisation measure, not an expected loss
    /// (loss given default is unknown and the score is not a calibrated default rate).
    risk_weighted_exposure: Option<f64>,
}

#[derive(Serialize, Default)]
pub struct BandTotal {
    customers: usize,
    outstanding: Decimal,
}

#[derive(Serialize)]
pub struct Exposure {
    total_outstanding: Decimal,
    total_overdue: Decimal,
    assessed_outstanding: Decimal,
    /// Open receivables from customers with no risk profile.
    unassessed_outstanding: Decimal,
    risk_weighted_exposure: f64,
    by_band: BTreeMap<&'static str, BandTotal>,
    /// Largest risk-weighted exposures first.
    customers: Vec<CustomerExposure>,
}

#[derive(Deserialize)]
pub struct ExposureQuery {
    limit: Option<u64>,
}

pub async fn receivables_exposure(State(st): State<AppState>, Query(q): Query<ExposureQuery>) -> ApiResult<Exposure> {
    let sql = r#"
        SELECT c.id AS customer_id, c.name,
               SUM(i.amount - i.amount_paid) AS outstanding,
               COALESCE(SUM(i.amount - i.amount_paid) FILTER (WHERE i.due_date < $1), 0) AS overdue,
               p.credit_score, p.debt_ratio, p.liquidity, p.is_bankrupt
        FROM customers c
        JOIN invoices i ON i.customer_id = c.id AND i.status = 'open'
        LEFT JOIN counterparty_risk_profiles p ON p.party_kind = 'customer' AND p.party_id = c.id
        GROUP BY c.id, c.name, p.credit_score, p.debt_ratio, p.liquidity, p.is_bankrupt"#;
    let raw = ExposureRow::find_by_statement(stmt(sql, vec![today().into()])).all(&st.db).await?;

    let mut rows = vec![];
    let mut by_band: BTreeMap<&'static str, BandTotal> = BTreeMap::new();
    let (mut total, mut overdue, mut assessed, mut weighted) = (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO, 0.0f64);
    for r in raw {
        total += r.outstanding;
        overdue += r.overdue;
        let risk = match (r.credit_score, r.debt_ratio, r.liquidity) {
            (Some(c), Some(d), Some(l)) => Some(score_inputs(c, d, l, r.is_bankrupt.unwrap_or(false))),
            _ => None,
        };
        let band = risk.as_ref().map_or("unassessed", |s| s.band);
        let b = by_band.entry(band).or_default();
        b.customers += 1;
        b.outstanding += r.outstanding;
        let rw = risk.as_ref().map(|s| r.outstanding.to_f64().unwrap_or(0.0) * s.probability);
        if let Some(w) = rw {
            assessed += r.outstanding;
            weighted += w;
        }
        rows.push(CustomerExposure {
            customer_id: r.customer_id,
            name: r.name,
            outstanding: r.outstanding,
            overdue: r.overdue,
            risk,
            risk_weighted_exposure: rw.map(|w| (w * 100.0).round() / 100.0),
        });
    }
    rows.sort_by(|a, b| {
        b.risk_weighted_exposure
            .unwrap_or(-1.0)
            .partial_cmp(&a.risk_weighted_exposure.unwrap_or(-1.0))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.outstanding.cmp(&a.outstanding))
    });
    rows.truncate(clamp_limit(q.limit) as usize);
    Ok(Json(Exposure {
        total_outstanding: total,
        total_overdue: overdue,
        assessed_outstanding: assessed,
        unassessed_outstanding: total - assessed,
        risk_weighted_exposure: (weighted * 100.0).round() / 100.0,
        by_band,
        customers: rows,
    }))
}
