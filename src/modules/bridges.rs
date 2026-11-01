//! Bridges between the operational modules (CRM, PM, assets, GRC) and the ERP core (finance, HR, risk).
//! Every flow is idempotent (guarded by a unique link column), runs in one transaction, and posts through
//! the same ledger code as the ERP's own endpoints.
use crate::db::row;
use crate::error::{AppError, AppResult};
use crate::erp::finance::{
    assets::{open_asset, NewAsset},
    ledger::account_by_code,
    receivables::{issue_invoice, IssueInvoice},
};
use crate::state::{AppState, AuthUser};
use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait, Value};
use serde_json::{json, Value as Json_};
use std::collections::HashMap;
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/crm/accounts/{id}/sync-customer", post(sync_customer))
        .route("/api/crm/opportunities/{id}/invoice", post(invoice_opportunity))
        .route("/api/pm/invoices/{id}/post-to-ledger", post(post_pm_invoice))
        .route("/api/pm/resources/sync-employees", post(sync_employees))
        .route("/api/am/assets/{id}/register-fixed-asset", post(register_fixed_asset))
        .route("/api/risk/integrity-checks/raise-grc-risks", post(raise_grc_risks))
        .route("/api/enterprise/overview", get(overview))
}

fn st(sql: &str, v: Vec<Value>) -> Statement { Statement::from_sql_and_values(DbBackend::Postgres, sql, v) }

fn today() -> NaiveDate { chrono::Utc::now().date_naive() }

fn opt_date(b: &Json_, k: &str) -> AppResult<Option<NaiveDate>> {
    match b[k].as_str() {
        None => Ok(None),
        Some(s) => NaiveDate::parse_from_str(s, "%Y-%m-%d").map(Some).map_err(|_| AppError::BadRequest(format!("{k} must be YYYY-MM-DD"))),
    }
}

fn opt_dec(b: &Json_, k: &str) -> AppResult<Option<Decimal>> {
    match &b[k] {
        Json_::Null => Ok(None),
        Json_::String(s) => s.parse().map(Some).map_err(|_| AppError::BadRequest(format!("{k} must be a decimal"))),
        Json_::Number(n) => n.to_string().parse().map(Some).map_err(|_| AppError::BadRequest(format!("{k} must be a decimal"))),
        _ => Err(AppError::BadRequest(format!("{k} must be a number"))),
    }
}

async fn revenue_account(txn: &impl ConnectionTrait, b: &Json_) -> AppResult<i64> {
    match b["revenue_account_id"].as_i64() {
        Some(id) => Ok(id),
        None => Ok(account_by_code(txn, "4000").await?.id),
    }
}

/// Finds or creates the ERP customer for a CRM account and records the link. Returns (customer_id, created).
pub(crate) async fn ensure_customer(txn: &impl ConnectionTrait, account_id: Uuid) -> AppResult<(i64, bool)> {
    let a = txn.query_one(st("SELECT name, email, phone, erp_customer_id FROM crm_accounts WHERE id=$1 FOR UPDATE", vec![account_id.into()])).await?
        .ok_or_else(|| AppError::not_found("CRM account not found"))?;
    if let Some(id) = a.try_get::<Option<i64>>("", "erp_customer_id")? { return Ok((id, false)); }
    let name: String = a.try_get("", "name")?;
    let email: Option<String> = a.try_get("", "email")?;
    let phone: Option<String> = a.try_get("", "phone")?;
    // reuse a same-named customer that no other account already owns; otherwise create (disambiguating the name)
    let existing = txn.query_one(st("SELECT c.id FROM customers c WHERE c.name=$1 AND NOT EXISTS (SELECT 1 FROM crm_accounts x WHERE x.erp_customer_id=c.id)", vec![name.clone().into()])).await?;
    let (cid, created) = match existing {
        Some(r) => (r.try_get::<i64>("", "id")?, false),
        None => {
            let taken = txn.query_one(st("SELECT 1 AS x FROM customers WHERE name=$1", vec![name.clone().into()])).await?.is_some();
            let cname = if taken { format!("{name} ({})", &account_id.to_string()[..8]) } else { name };
            let r = txn.query_one(st("INSERT INTO customers (name,email,phone) VALUES ($1,$2,$3) RETURNING id", vec![cname.into(), email.into(), phone.into()])).await?
                .ok_or_else(AppError::internal)?;
            (r.try_get::<i64>("", "id")?, true)
        }
    };
    txn.execute(st("UPDATE crm_accounts SET erp_customer_id=$1 WHERE id=$2", vec![cid.into(), account_id.into()])).await?;
    Ok((cid, created))
}

/// POST /api/crm/accounts/{id}/sync-customer — link (or create) the ERP customer for a CRM account.
async fn sync_customer(Extension(u): Extension<AuthUser>, State(s): State<AppState>, Path(id): Path<Uuid>) -> AppResult<Json<Json_>> {
    u.require(&["admin", "manager", "finance"])?;
    let txn = s.db.begin().await?;
    let (cid, created) = ensure_customer(&txn, id).await?;
    txn.commit().await?;
    Ok(Json(json!({ "account_id": id, "erp_customer_id": cid, "created": created })))
}

/// POST /api/crm/opportunities/{id}/invoice — bill a closed-won opportunity (quote-to-cash).
/// Optional body: revenue_account_id (default GL 4000), invoice_number, invoice_date, due_date, tax, description.
async fn invoice_opportunity(Extension(u): Extension<AuthUser>, State(s): State<AppState>, Path(id): Path<Uuid>, body: Option<Json<Json_>>) -> AppResult<Json<Json_>> {
    u.require(&["admin", "manager", "finance"])?;
    let b = body.map(|j| j.0).unwrap_or(Json_::Null);
    let txn = s.db.begin().await?;
    let o = txn.query_one(st("SELECT name, stage, amount::text AS amount, account_id, erp_invoice_id FROM crm_opportunities WHERE id=$1 FOR UPDATE", vec![id.into()])).await?
        .ok_or(AppError::NotFound(None))?;
    if o.try_get::<Option<i64>>("", "erp_invoice_id")?.is_some() { return Err(AppError::Conflict("opportunity has already been invoiced".into())); }
    if o.try_get::<String>("", "stage")? != "closed_won" { return Err(AppError::Unprocessable("only closed_won opportunities can be invoiced".into())); }
    let account: Uuid = o.try_get::<Option<Uuid>>("", "account_id")?.ok_or_else(|| AppError::Unprocessable("opportunity has no account to bill".into()))?;
    let amount: Decimal = o.try_get::<String>("", "amount")?.parse().map_err(|_| AppError::internal())?;
    let name: String = o.try_get("", "name")?;
    let (cid, _) = ensure_customer(&txn, account).await?;
    let date = opt_date(&b, "invoice_date")?.unwrap_or_else(today);
    let number = b["invoice_number"].as_str().map(String::from).unwrap_or_else(|| format!("OPP-{}", id.simple().to_string()[..8].to_uppercase()));
    let inv = issue_invoice(&txn, IssueInvoice {
        customer_id: cid, invoice_number: number, invoice_date: date, due_date: opt_date(&b, "due_date")?,
        net_amount: amount, tax: opt_dec(&b, "tax")?.unwrap_or_default(), revenue_account_id: revenue_account(&txn, &b).await?,
        description: Some(b["description"].as_str().map(String::from).unwrap_or(format!("Opportunity: {name}"))),
    }).await?;
    txn.execute(st("UPDATE crm_opportunities SET erp_invoice_id=$1 WHERE id=$2", vec![inv.id.into(), id.into()])).await?;
    txn.commit().await?;
    Ok(Json(json!({ "opportunity_id": id, "erp_customer_id": cid, "invoice": inv })))
}

/// POST /api/pm/invoices/{id}/post-to-ledger — post a project invoice as an AR invoice (tax goes to GL 2200).
/// Payment/void in the ledger flows back to the project invoice through a database trigger.
async fn post_pm_invoice(Extension(u): Extension<AuthUser>, State(s): State<AppState>, Path(id): Path<Uuid>, body: Option<Json<Json_>>) -> AppResult<Json<Json_>> {
    u.require(&["admin", "manager", "finance"])?;
    let b = body.map(|j| j.0).unwrap_or(Json_::Null);
    let txn = s.db.begin().await?;
    let i = txn.query_one(st("SELECT i.invoice_number, i.issue_date, i.due_date, i.amount::text AS amount, i.tax::text AS tax, i.status, i.erp_invoice_id, p.account_id, p.name AS project
        FROM pm_invoices i JOIN pm_projects p ON p.id=i.project_id WHERE i.id=$1 FOR UPDATE OF i", vec![id.into()])).await?
        .ok_or(AppError::NotFound(None))?;
    if i.try_get::<Option<i64>>("", "erp_invoice_id")?.is_some() { return Err(AppError::Conflict("invoice has already been posted to the ledger".into())); }
    let status: String = i.try_get("", "status")?;
    if status == "void" || status == "paid" { return Err(AppError::Unprocessable(format!("a {status} project invoice cannot be posted"))); }
    let account: Uuid = i.try_get::<Option<Uuid>>("", "account_id")?.ok_or_else(|| AppError::Unprocessable("the project has no CRM account to bill".into()))?;
    let (cid, _) = ensure_customer(&txn, account).await?;
    let project: String = i.try_get("", "project")?;
    let inv = issue_invoice(&txn, IssueInvoice {
        customer_id: cid, invoice_number: i.try_get("", "invoice_number")?, invoice_date: i.try_get("", "issue_date")?,
        due_date: i.try_get::<Option<NaiveDate>>("", "due_date")?,
        net_amount: i.try_get::<String>("", "amount")?.parse().map_err(|_| AppError::internal())?,
        tax: i.try_get::<String>("", "tax")?.parse().map_err(|_| AppError::internal())?,
        revenue_account_id: revenue_account(&txn, &b).await?,
        description: Some(format!("Project: {project}")),
    }).await?;
    txn.execute(st("UPDATE pm_invoices SET erp_invoice_id=$1, status=CASE WHEN status='draft' THEN 'sent' ELSE status END WHERE id=$2", vec![inv.id.into(), id.into()])).await?;
    txn.commit().await?;
    Ok(Json(json!({ "pm_invoice_id": id, "erp_customer_id": cid, "invoice": inv })))
}

/// POST /api/pm/resources/sync-employees[?apply_cost=true] — mirror active HR employees into PM resources.
/// With apply_cost, hourly_cost = base_salary / 2080 (salary-derived, so only admin/manager/hr may request it).
async fn sync_employees(Extension(u): Extension<AuthUser>, State(s): State<AppState>, Query(q): Query<HashMap<String, String>>) -> AppResult<Json<Json_>> {
    u.require(&["admin", "manager", "hr"])?;
    let apply = q.get("apply_cost").map(|v| v == "true").unwrap_or(false);
    let txn = s.db.begin().await?;
    let linked = txn.execute(st("UPDATE pm_resources r SET employee_id=e.id FROM employees e
        WHERE r.employee_id IS NULL AND r.kind='person' AND lower(r.email)=lower(e.email) AND e.status='active'
          AND NOT EXISTS (SELECT 1 FROM pm_resources x WHERE x.employee_id=e.id)", vec![])).await?.rows_affected();
    let created = txn.execute(st("INSERT INTO pm_resources (name,kind,email,role,hourly_cost,employee_id,is_active)
        SELECT e.first_name||' '||e.last_name,'person',e.email,e.job_title, CASE WHEN $1 THEN round(e.base_salary/2080,2) ELSE 0 END, e.id, true
        FROM employees e WHERE e.status='active' AND NOT EXISTS (SELECT 1 FROM pm_resources x WHERE x.employee_id=e.id)", vec![apply.into()])).await?.rows_affected();
    let updated = txn.execute(st("UPDATE pm_resources r SET name=e.first_name||' '||e.last_name, role=e.job_title, is_active=(e.status='active'),
        hourly_cost=CASE WHEN $1 THEN round(e.base_salary/2080,2) ELSE r.hourly_cost END
        FROM employees e WHERE r.employee_id=e.id", vec![apply.into()])).await?.rows_affected();
    txn.commit().await?;
    Ok(Json(json!({ "linked_by_email": linked, "created": created, "refreshed": updated, "cost_applied": apply })))
}

/// POST /api/am/assets/{id}/register-fixed-asset — capitalise an operational asset in the ledger.
/// Body: paid_from_account_id (required, e.g. owner's equity), optional asset/accumulated/expense account ids
/// (default GL 1500 / 1590 / 5200) and acquisition_date (default the asset's purchase date).
async fn register_fixed_asset(Extension(u): Extension<AuthUser>, State(s): State<AppState>, Path(id): Path<Uuid>, Json(b): Json<Json_>) -> AppResult<Json<Json_>> {
    u.require(&["admin", "manager", "finance"])?;
    let paid_from = b["paid_from_account_id"].as_i64().ok_or_else(|| AppError::BadRequest("paid_from_account_id is required".into()))?;
    let txn = s.db.begin().await?;
    let a = txn.query_one(st("SELECT asset_tag, name, purchase_date, purchase_cost::text AS cost, salvage_value::text AS salvage, useful_life_months, fixed_asset_id FROM am_assets WHERE id=$1 FOR UPDATE", vec![id.into()])).await?
        .ok_or(AppError::NotFound(None))?;
    if a.try_get::<Option<i64>>("", "fixed_asset_id")?.is_some() { return Err(AppError::Conflict("asset is already registered in the ledger".into())); }
    let date = match opt_date(&b, "acquisition_date")? {
        Some(d) => d,
        None => a.try_get::<Option<NaiveDate>>("", "purchase_date")?.ok_or_else(|| AppError::Unprocessable("asset has no purchase_date; pass acquisition_date".into()))?,
    };
    let get = |k: &'static str| b[k].as_i64();
    let asset_acc = match get("asset_account_id") { Some(x) => x, None => account_by_code(&txn, "1500").await?.id };
    let accum_acc = match get("accumulated_depreciation_account_id") { Some(x) => x, None => account_by_code(&txn, "1590").await?.id };
    let exp_acc = match get("depreciation_expense_account_id") { Some(x) => x, None => account_by_code(&txn, "5200").await?.id };
    let fa = open_asset(&txn, NewAsset {
        asset_tag: a.try_get("", "asset_tag")?, name: a.try_get("", "name")?,
        asset_account_id: asset_acc, accumulated_depreciation_account_id: accum_acc, depreciation_expense_account_id: exp_acc,
        paid_from_account_id: paid_from, acquisition_date: date,
        cost: a.try_get::<String>("", "cost")?.parse().map_err(|_| AppError::internal())?,
        salvage_value: Some(a.try_get::<String>("", "salvage")?.parse().map_err(|_| AppError::internal())?),
        useful_life_months: a.try_get("", "useful_life_months")?,
    }).await?;
    txn.execute(st("UPDATE am_assets SET fixed_asset_id=$1 WHERE id=$2", vec![fa.id.into(), id.into()])).await?;
    txn.commit().await?;
    Ok(Json(json!({ "asset_id": id, "fixed_asset": fa })))
}

/// POST /api/risk/integrity-checks/raise-grc-risks — turn failing/warning ledger integrity checks into GRC risks
/// (financial category). Idempotent: a check that already has an unresolved risk is not raised again.
async fn raise_grc_risks(Extension(u): Extension<AuthUser>, State(s): State<AppState>) -> AppResult<Json<Json_>> {
    u.require(&["admin", "manager", "finance"])?;
    let report = crate::erp::risk::integrity::integrity_checks(State(s.clone())).await?.0;
    let (mut raised, mut existing) = (vec![], 0);
    for c in report.checks.iter().filter(|c| c.status != "pass") {
        let title = format!("Ledger integrity: {}", c.name);
        let dup = row(&s.db, "SELECT count(*)::int AS n FROM grc_risks WHERE title=$1 AND status NOT IN ('closed','accepted')", vec![title.clone().into()]).await?;
        if dup["n"].as_i64().unwrap_or(0) > 0 { existing += 1; continue; }
        let (l, i) = if c.status == "fail" { (4, 5) } else { (3, 3) };
        let r = row(&s.db, "INSERT INTO grc_risks (title, description, category, likelihood, impact, status) VALUES ($1,$2,'financial',$3,$4,'open') RETURNING id, risk_code, rating",
            vec![title.into(), c.detail.clone().into(), l.into(), i.into()]).await?;
        raised.push(r);
    }
    Ok(Json(json!({ "overall": report.overall, "raised": raised, "already_open": existing })))
}

/// GET /api/enterprise/overview — one cross-module snapshot. Sections the caller's role may not read are omitted.
async fn overview(Extension(u): Extension<AuthUser>, State(s): State<AppState>) -> AppResult<Json<Json_>> {
    let can = |p: &str| crate::auth::authorize(&u.role, &axum::http::Method::GET, p).is_ok();
    let mut out = serde_json::Map::new();
    out.insert("crm".into(), row(&s.db, "SELECT
        (SELECT COALESCE(sum(amount),0)::float8 FROM crm_opportunities WHERE stage NOT IN ('closed_won','closed_lost')) AS open_pipeline,
        (SELECT count(*) FROM crm_opportunities WHERE stage NOT IN ('closed_won','closed_lost')) AS open_opportunities,
        (SELECT COALESCE(sum(amount),0)::float8 FROM crm_opportunities WHERE stage='closed_won') AS won_value,
        (SELECT count(*) FROM crm_opportunities WHERE stage='closed_won' AND erp_invoice_id IS NULL AND account_id IS NOT NULL) AS won_not_invoiced,
        (SELECT count(*) FROM crm_cases WHERE status NOT IN ('resolved','closed')) AS open_cases", vec![]).await?);
    out.insert("projects".into(), row(&s.db, "SELECT count(*) FILTER (WHERE status='active') AS active,
        COALESCE(sum(budget) FILTER (WHERE status='active'),0)::float8 AS active_budget,
        (SELECT count(*) FROM pm_invoices WHERE status IN ('draft','sent') AND erp_invoice_id IS NULL) AS invoices_not_in_ledger FROM pm_projects", vec![]).await?);
    out.insert("grc".into(), row(&s.db, "SELECT count(*) FILTER (WHERE status NOT IN ('closed')) AS open_risks,
        count(*) FILTER (WHERE status NOT IN ('closed') AND rating IN ('critical','high')) AS high_or_critical FROM grc_risks", vec![]).await?);
    out.insert("assets".into(), row(&s.db, "SELECT (SELECT count(*) FROM am_assets WHERE status NOT IN ('retired','disposed')) AS active_assets,
        (SELECT count(*) FROM am_assets WHERE fixed_asset_id IS NULL AND purchase_cost>0 AND status NOT IN ('retired','disposed')) AS not_capitalised,
        (SELECT count(*) FROM am_work_orders WHERE status NOT IN ('completed','cancelled')) AS open_work_orders", vec![]).await?);
    out.insert("supply_chain".into(), row(&s.db, "SELECT
        (SELECT count(*) FROM purchase_orders WHERE status IN ('submitted','partially_received')) AS open_purchase_orders,
        (SELECT count(*) FROM sales_orders WHERE status IN ('backordered','partially_allocated')) AS backordered_sales_orders,
        (SELECT COALESCE(sum(sl.quantity::numeric * p.unit_cost),0)::float8 FROM stock_levels sl JOIN products p ON p.id=sl.product_id) AS inventory_value_at_cost,
        (SELECT count(*) FROM quality_incidents WHERE status='open') AS open_quality_incidents,
        (SELECT count(*) FROM sales_orders so WHERE so.erp_invoice_id IS NULL AND so.status IN ('partially_shipped','shipped')
            AND EXISTS (SELECT 1 FROM sales_order_lines l WHERE l.sales_order_id=so.id AND l.quantity_shipped>0)) AS shipped_not_invoiced,
        (SELECT count(*) FROM supplier_invoices WHERE erp_bill_id IS NULL AND status<>'disputed') AS supplier_invoices_not_posted", vec![]).await?);
    out.insert("quality".into(), row(&s.db, "SELECT count(*) FILTER (WHERE status<>'closed') AS open_nonconformances FROM qms_nonconformances", vec![]).await?);
    out.insert("ehs".into(), row(&s.db, "SELECT count(*) FILTER (WHERE status<>'closed') AS open_incidents,
        count(*) FILTER (WHERE is_recordable AND occurred_at > now() - interval '365 days') AS recordable_12m FROM ehs_incidents", vec![]).await?);
    if can("/api/finance") {
        out.insert("finance".into(), row(&s.db, "SELECT
            (SELECT COALESCE(sum(l.debit-l.credit),0)::float8 FROM journal_lines l WHERE l.account_id IN (SELECT gl_account_id FROM bank_accounts)) AS cash,
            (SELECT COALESCE(sum(amount-amount_paid),0)::float8 FROM invoices WHERE status='open') AS receivables_open,
            (SELECT COALESCE(sum(amount-amount_paid),0)::float8 FROM invoices WHERE status='open' AND due_date<current_date) AS receivables_overdue,
            (SELECT COALESCE(sum(amount-amount_paid),0)::float8 FROM bills WHERE status='open') AS payables_open", vec![]).await?);
    }
    if can("/api/hr") {
        out.insert("hr".into(), row(&s.db, "SELECT count(*) FILTER (WHERE status='active') AS headcount,
            COALESCE(sum(base_salary) FILTER (WHERE status='active'),0)::float8 AS annual_base_payroll,
            (SELECT count(*) FROM job_postings WHERE status='open') AS open_postings FROM employees", vec![]).await?);
    }
    
    Ok(Json(Json_::Object(out)))
}
