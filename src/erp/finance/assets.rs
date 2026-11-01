//! Fixed assets: acquisition and straight-line monthly depreciation.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::{Datelike, NaiveDate};
use rust_decimal::Decimal;
use sea_orm::{
    prelude::*, ActiveValue::{NotSet, Set}, QueryOrder, QuerySelect, TransactionTrait,
};
use serde::{Deserialize, Serialize};

use super::ledger::{post_entry, NewLine};
use crate::{
    erp::entities::{depreciation_entry, fixed_asset},
    error::{ApiResult, AppError, Created},
    erp::util::{non_empty, require_non_negative, require_positive},
    state::AppState,
};

#[derive(Deserialize)]
pub struct NewAsset {
    pub asset_tag: String,
    pub name: String,
    pub asset_account_id: i64,
    pub accumulated_depreciation_account_id: i64,
    pub depreciation_expense_account_id: i64,
    /// What the asset was funded from (e.g. owner's equity). Bank and receivables/payables
    /// control accounts are rejected so the subledgers keep reconciling.
    pub paid_from_account_id: i64,
    pub acquisition_date: NaiveDate,
    pub cost: Decimal,
    pub salvage_value: Option<Decimal>,
    pub useful_life_months: i32,
}

pub async fn create_asset(State(st): State<AppState>, Json(req): Json<NewAsset>) -> Created<fixed_asset::Model> {
    let txn = st.db.begin().await?;
    let a = open_asset(&txn, req).await?;
    txn.commit().await?;
    Ok((StatusCode::CREATED, Json(a)))
}

/// Validates, posts the acquisition entry (Dr asset, Cr funding account) and records the asset inside the
/// caller's transaction. Shared by the fixed-asset endpoint and the asset-management bridge.
pub async fn open_asset<C: sea_orm::ConnectionTrait>(txn: &C, req: NewAsset) -> Result<fixed_asset::Model, AppError> {
    let cost = require_positive(req.cost, "cost")?;
    let salvage = require_non_negative(req.salvage_value.unwrap_or_default(), "salvage_value")?;
    if salvage > cost {
        return Err(AppError::BadRequest("salvage_value must not exceed cost".into()));
    }
    if req.useful_life_months <= 0 {
        return Err(AppError::BadRequest("useful_life_months must be positive".into()));
    }
    let tag = non_empty(&req.asset_tag, "asset_tag")?;
    let name = non_empty(&req.name, "name")?;
    super::cash::reject_bank_accounts(txn, &[req.paid_from_account_id]).await?;
    super::cash::reject_control_accounts(txn, &[req.paid_from_account_id]).await?;
    let je = post_entry(
        txn,
        req.acquisition_date,
        &format!("Acquisition of asset {tag}: {name}"),
        "fixed_asset",
        None,
        None,
        vec![NewLine::dr(req.asset_account_id, cost), NewLine::cr(req.paid_from_account_id, cost)],
    )
    .await?;
    Ok(fixed_asset::ActiveModel {
        id: NotSet,
        asset_tag: Set(tag),
        name: Set(name),
        asset_account_id: Set(req.asset_account_id),
        accumulated_depreciation_account_id: Set(req.accumulated_depreciation_account_id),
        depreciation_expense_account_id: Set(req.depreciation_expense_account_id),
        acquisition_date: Set(req.acquisition_date),
        cost: Set(cost),
        salvage_value: Set(salvage),
        useful_life_months: Set(req.useful_life_months),
        accumulated_depreciation: Set(Decimal::ZERO),
        status: Set(if cost == salvage { "fully_depreciated" } else { "active" }.into()),
        acquisition_entry_id: Set(Some(je)),
        created_at: NotSet,
    }
    .insert(txn)
    .await?)
}

#[derive(Serialize)]
pub struct AssetView {
    #[serde(flatten)]
    asset: fixed_asset::Model,
    net_book_value: Decimal,
}

fn view(a: fixed_asset::Model) -> AssetView {
    let net_book_value = a.cost - a.accumulated_depreciation;
    AssetView { asset: a, net_book_value }
}

pub async fn list_assets(State(st): State<AppState>) -> ApiResult<Vec<AssetView>> {
    let rows = fixed_asset::Entity::find().order_by_asc(fixed_asset::Column::AssetTag).all(&st.db).await?;
    Ok(Json(rows.into_iter().map(view).collect()))
}

#[derive(Serialize)]
pub struct AssetDetail {
    #[serde(flatten)]
    asset: AssetView,
    schedule: Vec<depreciation_entry::Model>,
}

pub async fn get_asset(State(st): State<AppState>, Path(id): Path<i64>) -> ApiResult<AssetDetail> {
    let a = fixed_asset::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found(format!("asset {id}")))?;
    let schedule = depreciation_entry::Entity::find()
        .filter(depreciation_entry::Column::AssetId.eq(id))
        .order_by_asc(depreciation_entry::Column::PeriodEnd)
        .all(&st.db)
        .await?;
    Ok(Json(AssetDetail { asset: view(a), schedule }))
}

#[derive(Deserialize)]
pub struct DepreciateReq {
    /// Must be the last day of a month.
    period_end: NaiveDate,
}

#[derive(Serialize)]
pub struct DepreciationResult {
    asset_id: i64,
    asset_tag: String,
    amount: Decimal,
}

fn is_month_end(d: NaiveDate) -> bool {
    d.succ_opt().map(|n| n.day() == 1).unwrap_or(false)
}

/// Monthly straight-line charge, rounded to cents; the final month absorbs rounding.
pub fn monthly_charge(a: &fixed_asset::Model, periods_done: u64) -> Decimal {
    let depreciable = a.cost - a.salvage_value;
    let remaining = depreciable - a.accumulated_depreciation;
    if periods_done + 1 >= a.useful_life_months as u64 {
        return remaining;
    }
    (depreciable / Decimal::from(a.useful_life_months)).round_dp(2).min(remaining)
}

/// Runs depreciation for every active asset for the given month. Idempotent per (asset, month).
pub async fn depreciate(State(st): State<AppState>, Json(req): Json<DepreciateReq>) -> ApiResult<Vec<DepreciationResult>> {
    if !is_month_end(req.period_end) {
        return Err(AppError::BadRequest("period_end must be the last day of a month".into()));
    }
    let txn = st.db.begin().await?;
    let assets = fixed_asset::Entity::find()
        .filter(fixed_asset::Column::Status.eq("active"))
        .filter(fixed_asset::Column::AcquisitionDate.lte(req.period_end))
        .order_by_asc(fixed_asset::Column::Id)
        .lock_exclusive()
        .all(&txn)
        .await?;
    let mut out = vec![];
    for a in assets {
        let already = depreciation_entry::Entity::find()
            .filter(depreciation_entry::Column::AssetId.eq(a.id))
            .filter(depreciation_entry::Column::PeriodEnd.eq(req.period_end))
            .count(&txn)
            .await?;
        if already > 0 {
            continue;
        }
        let done = depreciation_entry::Entity::find()
            .filter(depreciation_entry::Column::AssetId.eq(a.id))
            .count(&txn)
            .await?;
        let amount = monthly_charge(&a, done);
        if amount <= Decimal::ZERO {
            continue;
        }
        let je = post_entry(
            &txn,
            req.period_end,
            &format!("Depreciation {} for period ending {}", a.asset_tag, req.period_end),
            "depreciation",
            Some(a.id),
            None,
            vec![
                NewLine::dr(a.depreciation_expense_account_id, amount),
                NewLine::cr(a.accumulated_depreciation_account_id, amount),
            ],
        )
        .await?;
        depreciation_entry::ActiveModel {
            id: NotSet,
            asset_id: Set(a.id),
            period_end: Set(req.period_end),
            amount: Set(amount),
            journal_entry_id: Set(je),
        }
        .insert(&txn)
        .await?;
        let accumulated = a.accumulated_depreciation + amount;
        let fully = accumulated == a.cost - a.salvage_value;
        let (id, tag) = (a.id, a.asset_tag.clone());
        let mut am: fixed_asset::ActiveModel = a.into();
        am.accumulated_depreciation = Set(accumulated);
        if fully {
            am.status = Set("fully_depreciated".into());
        }
        am.update(&txn).await?;
        out.push(DepreciationResult { asset_id: id, asset_tag: tag, amount });
    }
    txn.commit().await?;
    Ok(Json(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(cost: &str, salvage: &str, life: i32) -> fixed_asset::Model {
        fixed_asset::Model {
            id: 1,
            asset_tag: "T".into(),
            name: "T".into(),
            asset_account_id: 1,
            accumulated_depreciation_account_id: 2,
            depreciation_expense_account_id: 3,
            acquisition_date: "2026-01-01".parse().unwrap(),
            cost: cost.parse().unwrap(),
            salvage_value: salvage.parse().unwrap(),
            useful_life_months: life,
            accumulated_depreciation: Decimal::ZERO,
            status: "active".into(),
            acquisition_entry_id: None,
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn depreciation_sums_exactly_to_depreciable_amount() {
        let mut a = asset("1000.00", "0", 3);
        let mut total = Decimal::ZERO;
        for n in 0..3u64 {
            let c = monthly_charge(&a, n);
            a.accumulated_depreciation += c;
            total += c;
        }
        assert_eq!(total, "1000.00".parse::<Decimal>().unwrap());
        assert_eq!(monthly_charge(&a, 3), Decimal::ZERO);
    }

    #[test]
    fn month_end_detection() {
        assert!(is_month_end("2026-02-28".parse().unwrap()));
        assert!(!is_month_end("2026-02-27".parse().unwrap()));
        assert!(is_month_end("2028-02-29".parse().unwrap()));
    }
}
