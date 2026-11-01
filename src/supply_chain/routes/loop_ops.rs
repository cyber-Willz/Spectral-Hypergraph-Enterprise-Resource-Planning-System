//! Closing the loop: approved sources, automatic replenishment orders (with contract validation)
//! and the one-call planning cycle forecast -> replenishment -> procurement -> resilience check.
use std::collections::{BTreeMap, HashMap, HashSet};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, post},
    Json, Router,
};
use chrono::{Duration, NaiveDate, Utc};
use sea_orm::{
    prelude::Decimal, ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend,
    EntityTrait, FromQueryResult, QueryFilter, QueryOrder, Set, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    forecasting::{generate_for, GenerateReq},
    gen_number,
    planning::compute_replenishment,
    resilience::network_snapshot,
    supplier_perf,
};
use crate::supply_chain::{
    entities::{contract, product, purchase_order, purchase_order_line, supplier, warehouse},
    error::{AppError, AppResult},
    state::AppState,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/products/{id}/suppliers", post(upsert_source).get(list_sources))
        .route("/products/{id}/suppliers/{supplier_id}", delete(deactivate_source))
        .route("/replenishment/run", post(run_handler))
        .route("/planning/cycle", post(cycle))
}

// ---- approved sources -----------------------------------------------------------------------------------

#[derive(Debug, FromQueryResult, Serialize)]
pub struct SourceRow {
    id: Uuid,
    product_id: Uuid,
    supplier_id: Uuid,
    supplier_code: String,
    is_preferred: bool,
    unit_cost: Option<Decimal>,
    lead_time_days: Option<i32>,
    allocation_share: Option<f64>,
    min_order_qty: i64,
    order_multiple: i64,
    active: bool,
}

/// Round an order up to the supplier's minimum and order multiple.
pub fn apply_order_rules(qty: i64, moq: i64, multiple: i64) -> i64 {
    let q = qty.max(moq).max(1);
    if multiple > 1 {
        (q + multiple - 1) / multiple * multiple
    } else {
        q
    }
}

const SOURCE_SELECT: &str = "SELECT ps.id, ps.product_id, ps.supplier_id, s.code AS supplier_code, ps.is_preferred, \
    ps.unit_cost, ps.lead_time_days, ps.allocation_share, ps.min_order_qty, ps.order_multiple, ps.active \
    FROM product_suppliers ps JOIN suppliers s ON s.id = ps.supplier_id";

#[derive(Deserialize)]
pub struct SourceIn {
    supplier_id: Uuid,
    is_preferred: Option<bool>,
    unit_cost: Option<Decimal>,
    lead_time_days: Option<i32>,
    /// target share of volume, 0-1
    allocation_share: Option<f64>,
    /// supplier minimum order quantity (default 1) and order multiple (default 1)
    min_order_qty: Option<i64>,
    order_multiple: Option<i64>,
}

/// Qualify (or update) a supplier as an approved source for a product.
async fn upsert_source(
    State(s): State<AppState>,
    Path(product_id): Path<Uuid>,
    Json(i): Json<SourceIn>,
) -> AppResult<(StatusCode, Json<SourceRow>)> {
    get_or_404!(product::Entity, &s.db, product_id, "product");
    let sup = get_or_404!(supplier::Entity, &s.db, i.supplier_id, "supplier");
    if !sup.active {
        return Err(AppError::conflict("supplier is inactive"));
    }
    if matches!(i.allocation_share, Some(x) if !(0.0..=1.0).contains(&x)) {
        return Err(AppError::bad("allocation_share must be between 0 and 1"));
    }
    if matches!(i.min_order_qty, Some(x) if x < 1) || matches!(i.order_multiple, Some(x) if x < 1) {
        return Err(AppError::bad("min_order_qty and order_multiple must be at least 1"));
    }
    if matches!(i.lead_time_days, Some(x) if x < 0) || matches!(i.unit_cost, Some(x) if x.is_sign_negative()) {
        return Err(AppError::bad("lead_time_days and unit_cost must not be negative"));
    }
    s.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO product_suppliers (id, product_id, supplier_id, is_preferred, unit_cost, lead_time_days, allocation_share, min_order_qty, order_multiple, active) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,TRUE) \
         ON CONFLICT (product_id, supplier_id) DO UPDATE SET is_preferred = EXCLUDED.is_preferred, \
           unit_cost = EXCLUDED.unit_cost, lead_time_days = EXCLUDED.lead_time_days, \
           allocation_share = EXCLUDED.allocation_share, min_order_qty = EXCLUDED.min_order_qty, \
           order_multiple = EXCLUDED.order_multiple, active = TRUE",
        [
            Uuid::new_v4().into(),
            product_id.into(),
            i.supplier_id.into(),
            i.is_preferred.unwrap_or(false).into(),
            i.unit_cost.into(),
            i.lead_time_days.into(),
            i.allocation_share.into(),
            i.min_order_qty.unwrap_or(1).into(),
            i.order_multiple.unwrap_or(1).into(),
        ],
    ))
    .await?;
    let row = SourceRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        format!("{SOURCE_SELECT} WHERE ps.product_id = $1 AND ps.supplier_id = $2"),
        [product_id.into(), i.supplier_id.into()],
    ))
    .one(&s.db)
    .await?
    .ok_or_else(|| AppError::not_found("source"))?;
    Ok((StatusCode::CREATED, Json(row)))
}

async fn list_sources(State(s): State<AppState>, Path(product_id): Path<Uuid>) -> AppResult<Json<Vec<SourceRow>>> {
    get_or_404!(product::Entity, &s.db, product_id, "product");
    Ok(Json(
        SourceRow::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!("{SOURCE_SELECT} WHERE ps.product_id = $1 ORDER BY ps.is_preferred DESC, s.code"),
            [product_id.into()],
        ))
        .all(&s.db)
        .await?,
    ))
}

async fn deactivate_source(
    State(s): State<AppState>,
    Path((product_id, supplier_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<Value>> {
    let res = s
        .db
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE product_suppliers SET active = FALSE WHERE product_id = $1 AND supplier_id = $2",
            [product_id.into(), supplier_id.into()],
        ))
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::not_found("source"));
    }
    Ok(Json(json!({ "deactivated": true })))
}

// ---- automatic replenishment orders ---------------------------------------------------------------------------

#[derive(Deserialize, Clone)]
pub struct RunReq {
    pub warehouse_id: Uuid,
    pub as_of: Option<NaiveDate>,
    pub lookback_days: Option<i32>,
    /// forecast (default) | history
    pub demand_source: Option<String>,
    pub history_weeks: Option<i32>,
    /// order date for the generated POs (default today)
    pub order_date: Option<NaiveDate>,
    /// submit the generated orders instead of leaving them as drafts
    pub submit: Option<bool>,
    /// propose only, write nothing
    pub dry_run: Option<bool>,
    /// create orders even when the supplier has no contract in force (flagged as standard POs)
    pub allow_uncontracted: Option<bool>,
}

#[derive(Debug, FromQueryResult)]
struct ObservedSource {
    product_id: Uuid,
    supplier_id: Uuid,
}
#[derive(Debug, FromQueryResult)]
struct DraftProduct {
    product_id: Uuid,
}

struct Plan {
    moq: i64,
    multiple: i64,
    base_qty: i64,
    sku: String,
    product_id: Uuid,
    qty: i64,
    unit_cost: Decimal,
    lead_days: i64,
    why: String,
}

/// For every product at or below its reorder point: pick a source (preferred > best performance >
/// shortest lead time, falling back to the usual supplier), validate the contract, and raise one
/// draft/submitted purchase order per supplier. Products that already sit on a draft order are skipped.
pub async fn run_replenishment(db: &DatabaseConnection, r: &RunReq) -> AppResult<Value> {
    let wh = get_or_404!(warehouse::Entity, db, r.warehouse_id, "warehouse");
    if !wh.active {
        return Err(AppError::conflict("warehouse is inactive"));
    }
    let as_of = r.as_of.unwrap_or_else(|| Utc::now().date_naive());
    let order_date = r.order_date.unwrap_or_else(|| Utc::now().date_naive());
    let use_forecast = match r.demand_source.as_deref().unwrap_or("forecast") {
        "forecast" => true,
        "history" => false,
        _ => return Err(AppError::bad("demand_source must be forecast or history")),
    };
    let items: Vec<_> = compute_replenishment(db, Some(r.warehouse_id), r.lookback_days.unwrap_or(90), as_of, use_forecast, r.history_weeks.unwrap_or(104))
        .await?
        .into_iter()
        .filter(|i| i.reorder_now && i.suggested_order_qty > 0)
        .collect();

    let scorecards: HashMap<Uuid, Option<f64>> =
        supplier_perf::fetch(db, None, None, 3).await?.into_iter().map(|c| (c.supplier_id, c.performance_score)).collect();
    let suppliers: HashMap<Uuid, supplier::Model> = supplier::Entity::find().all(db).await?.into_iter().map(|m| (m.id, m)).collect();
    let approved = SourceRow::find_by_statement(Statement::from_string(
        DbBackend::Postgres,
        format!("{SOURCE_SELECT} WHERE ps.active AND s.active"),
    ))
    .all(db)
    .await?;
    let observed: HashMap<Uuid, Uuid> = ObservedSource::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT DISTINCT ON (pol.product_id) pol.product_id, po.supplier_id \
         FROM purchase_order_lines pol JOIN purchase_orders po ON po.id = pol.purchase_order_id \
         JOIN suppliers s ON s.id = po.supplier_id AND s.active \
         WHERE po.status NOT IN ('draft', 'cancelled') AND po.order_date > $1::date - 365 \
         GROUP BY pol.product_id, po.supplier_id ORDER BY pol.product_id, SUM(pol.quantity_ordered) DESC",
        [as_of.into()],
    ))
    .all(db)
    .await?
    .into_iter()
    .map(|o| (o.product_id, o.supplier_id))
    .collect();
    let drafts: HashSet<Uuid> = DraftProduct::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT DISTINCT pol.product_id FROM purchase_order_lines pol \
         JOIN purchase_orders po ON po.id = pol.purchase_order_id \
         WHERE po.status = 'draft' AND po.warehouse_id = $1",
        [r.warehouse_id.into()],
    ))
    .all(db)
    .await?
    .into_iter()
    .map(|d| d.product_id)
    .collect();

    let mut contract_cache: HashMap<Uuid, Option<contract::Model>> = HashMap::new();
    let mut groups: BTreeMap<String, (Uuid, Vec<Plan>)> = BTreeMap::new();
    let mut skipped: Vec<Value> = Vec::new();
    for it in &items {
        if drafts.contains(&it.product_id) {
            skipped.push(json!({"sku": it.sku, "reason": "draft_po_exists"}));
            continue;
        }
        let mut cands: Vec<&SourceRow> = approved.iter().filter(|a| a.product_id == it.product_id).collect();
        cands.sort_by(|a, b| {
            let pa = scorecards.get(&a.supplier_id).copied().flatten().unwrap_or(-1.0);
            let pb = scorecards.get(&b.supplier_id).copied().flatten().unwrap_or(-1.0);
            b.is_preferred
                .cmp(&a.is_preferred)
                .then(pb.partial_cmp(&pa).unwrap_or(std::cmp::Ordering::Equal))
                .then(a.lead_time_days.unwrap_or(i32::MAX).cmp(&b.lead_time_days.unwrap_or(i32::MAX)))
                .then(a.supplier_code.cmp(&b.supplier_code))
        });
        let (sid, src_cost, src_lead, why, moq, mult) = if let Some(c) = cands.first() {
            (c.supplier_id, c.unit_cost, c.lead_time_days, if c.is_preferred { "preferred source" } else { "best-scoring approved source" }, c.min_order_qty, c.order_multiple)
        } else if let Some(sid) = observed.get(&it.product_id) {
            (*sid, None, None, "usual supplier from purchase history", 1, 1)
        } else {
            skipped.push(json!({"sku": it.sku, "reason": "no_source"}));
            continue;
        };
        if let std::collections::hash_map::Entry::Vacant(slot) = contract_cache.entry(sid) {
            let c = contract::Entity::find()
                .filter(contract::Column::SupplierId.eq(sid))
                .filter(contract::Column::StartDate.lte(order_date))
                .filter(contract::Column::EndDate.gte(order_date))
                .order_by_desc(contract::Column::EndDate)
                .one(db)
                .await?;
            slot.insert(c);
        }
        if contract_cache[&sid].is_none() && !r.allow_uncontracted.unwrap_or(false) {
            skipped.push(json!({"sku": it.sku, "reason": "no_active_contract", "supplier": suppliers.get(&sid).map(|s| s.code.clone())}));
            continue;
        }
        let prod = get_or_404!(product::Entity, db, it.product_id, "product");
        let lead = src_lead.map(|d| d as f64).unwrap_or(it.lead_time_days).ceil() as i64;
        let code = suppliers.get(&sid).map(|s| s.code.clone()).unwrap_or_default();
        groups.entry(code).or_insert_with(|| (sid, Vec::new())).1.push(Plan {
            sku: it.sku.clone(),
            product_id: it.product_id,
            qty: apply_order_rules(it.suggested_order_qty, moq, mult),
            moq,
            multiple: mult,
            base_qty: it.suggested_order_qty,
            unit_cost: src_cost.unwrap_or(prod.unit_cost),
            lead_days: lead,
            why: why.to_string(),
        });
    }

    let dry = r.dry_run.unwrap_or(false);
    let submit = r.submit.unwrap_or(false);
    let mut orders = Vec::new();
    let txn = db.begin().await?;
    for (code, (sid, plans)) in &groups {
        let sup = &suppliers[sid];
        let ctr = contract_cache.get(sid).and_then(|c| c.clone());
        let longest = plans.iter().map(|p| p.lead_days).max().unwrap_or(0);
        let expected = order_date + Duration::days(longest);
        let total: Decimal = plans.iter().map(|p| p.unit_cost * Decimal::from(p.qty)).sum();
        let mut po_id = None;
        let mut po_number = None;
        if !dry {
            let id = Uuid::new_v4();
            let number = gen_number("PO");
            let now = Utc::now();
            purchase_order::ActiveModel {
                id: Set(id),
                po_number: Set(number.clone()),
                supplier_id: Set(*sid),
                warehouse_id: Set(r.warehouse_id),
                status: Set(if submit { "submitted" } else { "draft" }.into()),
                expected_date: Set(Some(expected)),
                notes: Set(Some("auto-generated by replenishment run".into())),
                created_at: Set(now),
                updated_at: Set(now),
                order_date: Set(order_date),
                po_type: Set(if ctr.is_some() { "contract" } else { "standard" }.into()),
                currency: Set(ctr.as_ref().map(|c| c.currency.clone()).unwrap_or_else(|| "USD".into())),
                payment_terms_days: Set(sup.payment_terms_days),
                contract_id: Set(ctr.as_ref().map(|c| c.id)),
                auto_generated: Set(true),
                ..Default::default()
            }
            .insert(&txn)
            .await?;
            for p in plans {
                purchase_order_line::ActiveModel {
                    id: Set(Uuid::new_v4()),
                    purchase_order_id: Set(id),
                    product_id: Set(p.product_id),
                    quantity_ordered: Set(p.qty),
                    quantity_received: Set(0),
                    unit_cost: Set(p.unit_cost),
                    promised_date: Set(Some(order_date + Duration::days(p.lead_days))),
                    budget_unit_cost: Set(None),
                }
                .insert(&txn)
                .await?;
            }
            po_id = Some(id);
            po_number = Some(number);
        }
        orders.push(json!({
            "po_id": po_id, "po_number": po_number, "supplier": code,
            "contract": ctr.map(|c| c.contract_number),
            "status": if dry { "proposed" } else if submit { "submitted" } else { "draft" },
            "expected_date": expected, "total_value": total,
            "lines": plans.iter().map(|p| json!({"sku": p.sku, "quantity": p.qty, "needed_quantity": p.base_qty, "min_order_qty": p.moq, "order_multiple": p.multiple, "moq_overbuy": p.qty - p.base_qty, "unit_cost": p.unit_cost, "lead_days": p.lead_days, "source": p.why})).collect::<Vec<_>>(),
        }));
    }
    txn.commit().await?;
    Ok(json!({
        "warehouse": wh.code, "as_of": as_of, "order_date": order_date, "dry_run": dry,
        "products_below_reorder_point": items.len(),
        "orders": orders, "skipped": skipped,
    }))
}

async fn run_handler(State(s): State<AppState>, Json(r): Json<RunReq>) -> AppResult<(StatusCode, Json<Value>)> {
    Ok((StatusCode::CREATED, Json(run_replenishment(&s.db, &r).await?)))
}

// ---- planning cycle -------------------------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CycleReq {
    warehouse_id: Uuid,
    as_of: Option<NaiveDate>,
    /// create purchase orders (default true); false proposes only
    create_pos: Option<bool>,
    submit: Option<bool>,
    allow_uncontracted: Option<bool>,
    method: Option<String>,
    horizon_weeks: Option<i32>,
    history_weeks: Option<i32>,
}

/// One closed-loop pass: store statistical forecasts, plan replenishment from them, raise purchase
/// orders against valid contracts, then check how concentrated and fragile the supply base is.
async fn cycle(State(s): State<AppState>, Json(r): Json<CycleReq>) -> AppResult<Json<Value>> {
    let as_of = r.as_of.unwrap_or_else(|| Utc::now().date_naive());
    let forecast = generate_for(
        &s.db,
        GenerateReq { warehouse_id: r.warehouse_id, method: r.method.clone(), horizon_weeks: r.horizon_weeks, history_weeks: r.history_weeks, as_of: Some(as_of) },
    )
    .await?;
    let replenishment = run_replenishment(
        &s.db,
        &RunReq {
            warehouse_id: r.warehouse_id,
            as_of: Some(as_of),
            lookback_days: None,
            demand_source: Some("forecast".into()),
            history_weeks: r.history_weeks,
            order_date: None,
            submit: r.submit,
            dry_run: Some(!r.create_pos.unwrap_or(true)),
            allow_uncontracted: r.allow_uncontracted,
        },
    )
    .await?;
    let resilience = network_snapshot(&s.db, as_of).await?;
    Ok(Json(json!({ "forecast": forecast, "replenishment": replenishment, "resilience": resilience })))
}

#[cfg(test)]
mod tests {
    use super::apply_order_rules;

    #[test]
    fn order_rules_round_up_to_moq_and_multiple() {
        assert_eq!(apply_order_rules(30, 50, 1), 50); // below MOQ -> MOQ (ERP: 86% of orders sat at the MOQ)
        assert_eq!(apply_order_rules(120, 50, 1), 120);
        assert_eq!(apply_order_rules(120, 50, 25), 125); // multiple of 25
        assert_eq!(apply_order_rules(10, 1, 12), 12);
        assert_eq!(apply_order_rules(0, 1, 1), 1);
    }
}
