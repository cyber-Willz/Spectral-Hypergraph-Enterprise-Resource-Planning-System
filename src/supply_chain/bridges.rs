//! Bridges between the supply chain module and the rest of the suite.
//!
//! | flow | endpoint (under `/api/scm`) | effect |
//! |------|-----------------------------|--------|
//! | supplier -> AP vendor | `POST /suppliers/{id}/sync-vendor` | link or create the ERP vendor |
//! | customer -> AR customer / CRM account | `POST /customers/{id}/sync-customer` | link or create the ERP customer, optionally tie to a CRM account |
//! | sales order -> ledger | `POST /sales-orders/{id}/invoice` | AR invoice (Dr AR / Cr Revenue) and COGS (Dr COGS / Cr Inventory) |
//! | supplier invoice -> ledger | `POST /supplier-invoices/{id}/post-to-ledger` | AP bill (Dr Inventory / Cr AP) |
//! | quality incident -> QMS | `POST /quality-incidents/{id}/raise-ncr` | supplier-sourced nonconformance |
//! | supplier scorecard -> GRC | `POST /risk/raise-grc-risks` | operational risk per weak / high-risk supplier |
//! | reconciliation | `GET /integration/status` | what is still unlinked, unbilled or unposted |
//!
//! Every flow is idempotent (guarded by a unique link column) and runs in one transaction, posting through
//! the same ledger code as the ERP's own endpoints (so e.g. a bankrupt customer still blocks invoicing).
use crate::erp::entities::bill;
use crate::erp::finance::{
    ledger::{account_by_code, post_entry, NewLine, AP},
    receivables::{issue_invoice, IssueInvoice},
};
use crate::error::{AppError, AppResult};
use crate::state::{AppState, AuthUser};
use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sea_orm::{
    ActiveModelTrait, ActiveValue::{NotSet, Set}, ConnectionTrait, DbBackend, EntityTrait, Statement,
    TransactionTrait, Value,
};
use serde_json::{json, Value as Json_};
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/suppliers/{id}/sync-vendor", post(sync_vendor))
        .route("/customers/{id}/sync-customer", post(sync_customer))
        .route("/sales-orders/{id}/invoice", post(invoice_sales_order))
        .route("/supplier-invoices/{id}/post-to-ledger", post(post_supplier_invoice))
        .route("/quality-incidents/{id}/raise-ncr", post(raise_ncr))
        .route("/risk/raise-grc-risks", post(raise_grc_risks))
        .route("/integration/status", get(integration_status))
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

fn body(b: Option<Json<Json_>>) -> Json_ { b.map(|j| j.0).unwrap_or(Json_::Null) }

// ---- supplier -> vendor ------------------------------------------------------------------------

/// Finds or creates the AP vendor for a supplier and records the link. Returns (vendor_id, created).
async fn ensure_vendor(txn: &impl ConnectionTrait, supplier_id: Uuid) -> AppResult<(i64, bool)> {
    let r = txn.query_one(st("SELECT name, contact_email, phone, payment_terms_days, erp_vendor_id FROM suppliers WHERE id=$1 FOR UPDATE", vec![supplier_id.into()])).await?
        .ok_or_else(|| AppError::not_found("supplier not found"))?;
    if let Some(id) = r.try_get::<Option<i64>>("", "erp_vendor_id")? { return Ok((id, false)); }
    let name: String = r.try_get("", "name")?;
    let email: Option<String> = r.try_get("", "contact_email")?;
    let phone: Option<String> = r.try_get("", "phone")?;
    let terms: i32 = r.try_get::<i32>("", "payment_terms_days")?.clamp(0, 365);
    let existing = txn.query_one(st("SELECT v.id FROM vendors v WHERE v.name=$1 AND NOT EXISTS (SELECT 1 FROM suppliers x WHERE x.erp_vendor_id=v.id)", vec![name.clone().into()])).await?;
    let (vid, created) = match existing {
        Some(r) => (r.try_get::<i64>("", "id")?, false),
        None => {
            let taken = txn.query_one(st("SELECT 1 AS x FROM vendors WHERE name=$1", vec![name.clone().into()])).await?.is_some();
            let vname = if taken { format!("{name} ({})", &supplier_id.to_string()[..8]) } else { name };
            let r = txn.query_one(st("INSERT INTO vendors (name,email,phone,payment_terms_days) VALUES ($1,$2,$3,$4) RETURNING id",
                vec![vname.into(), email.into(), phone.into(), terms.into()])).await?.ok_or_else(AppError::internal)?;
            (r.try_get::<i64>("", "id")?, true)
        }
    };
    txn.execute(st("UPDATE suppliers SET erp_vendor_id=$1 WHERE id=$2", vec![vid.into(), supplier_id.into()])).await?;
    Ok((vid, created))
}

/// POST /api/scm/suppliers/{id}/sync-vendor
async fn sync_vendor(Extension(u): Extension<AuthUser>, State(s): State<AppState>, Path(id): Path<Uuid>) -> AppResult<Json<Json_>> {
    u.require(&["admin", "manager", "finance"])?;
    let txn = s.db.begin().await?;
    let (vid, created) = ensure_vendor(&txn, id).await?;
    txn.commit().await?;
    Ok(Json(json!({ "supplier_id": id, "erp_vendor_id": vid, "created": created })))
}

// ---- customer -> AR customer / CRM account -----------------------------------------------------

/// Finds or creates the AR customer for a supply chain customer. With `crm_account_id` the customer is tied
/// to that CRM account (which itself is synced to the ERP customer through the CRM bridge).
async fn ensure_customer(txn: &impl ConnectionTrait, scm_customer_id: Uuid, crm_account_id: Option<Uuid>) -> AppResult<(i64, bool)> {
    let r = txn.query_one(st("SELECT name, email, erp_customer_id, crm_account_id FROM scm_customers WHERE id=$1 FOR UPDATE", vec![scm_customer_id.into()])).await?
        .ok_or_else(|| AppError::not_found("customer not found"))?;
    let linked: Option<i64> = r.try_get("", "erp_customer_id")?;
    let linked_crm: Option<Uuid> = r.try_get("", "crm_account_id")?;
    if let Some(crm) = crm_account_id {
        if linked_crm.is_some_and(|c| c != crm) { return Err(AppError::Conflict("customer is already linked to a different CRM account".into())); }
        let (cid, created) = crate::modules::bridges::ensure_customer(txn, crm).await?;
        if linked.is_some_and(|l| l != cid) { return Err(AppError::Conflict("customer is already linked to a different ERP customer".into())); }
        txn.execute(st("UPDATE scm_customers SET erp_customer_id=$1, crm_account_id=$2 WHERE id=$3", vec![cid.into(), crm.into(), scm_customer_id.into()])).await?;
        return Ok((cid, created && linked.is_none()));
    }
    if let Some(id) = linked { return Ok((id, false)); }
    let name: String = r.try_get("", "name")?;
    let email: Option<String> = r.try_get("", "email")?;
    let existing = txn.query_one(st("SELECT c.id FROM customers c WHERE c.name=$1 AND NOT EXISTS (SELECT 1 FROM scm_customers x WHERE x.erp_customer_id=c.id) AND NOT EXISTS (SELECT 1 FROM crm_accounts a WHERE a.erp_customer_id=c.id)", vec![name.clone().into()])).await?;
    let (cid, created) = match existing {
        Some(r) => (r.try_get::<i64>("", "id")?, false),
        None => {
            let taken = txn.query_one(st("SELECT 1 AS x FROM customers WHERE name=$1", vec![name.clone().into()])).await?.is_some();
            let cname = if taken { format!("{name} ({})", &scm_customer_id.to_string()[..8]) } else { name };
            let r = txn.query_one(st("INSERT INTO customers (name,email) VALUES ($1,$2) RETURNING id", vec![cname.into(), email.into()])).await?
                .ok_or_else(AppError::internal)?;
            (r.try_get::<i64>("", "id")?, true)
        }
    };
    txn.execute(st("UPDATE scm_customers SET erp_customer_id=$1 WHERE id=$2", vec![cid.into(), scm_customer_id.into()])).await?;
    Ok((cid, created))
}

/// POST /api/scm/customers/{id}/sync-customer  (optional body: {"crm_account_id": "<uuid>"})
async fn sync_customer(Extension(u): Extension<AuthUser>, State(s): State<AppState>, Path(id): Path<Uuid>, b: Option<Json<Json_>>) -> AppResult<Json<Json_>> {
    u.require(&["admin", "manager", "finance"])?;
    let b = body(b);
    let crm = match b["crm_account_id"].as_str() {
        None => None,
        Some(x) => Some(Uuid::parse_str(x).map_err(|_| AppError::BadRequest("crm_account_id must be a UUID".into()))?),
    };
    let txn = s.db.begin().await?;
    let (cid, created) = ensure_customer(&txn, id, crm).await?;
    txn.commit().await?;
    Ok(Json(json!({ "customer_id": id, "erp_customer_id": cid, "crm_account_id": crm, "created": created })))
}

// ---- sales order -> invoice + COGS -------------------------------------------------------------

/// POST /api/scm/sales-orders/{id}/invoice
/// Body (all optional): basis "shipped" (default) | "ordered", invoice_date, due_date, tax, revenue_account_id,
/// post_cogs (default true). Revenue = quantity x unit_price; COGS = shipped quantity x product unit_cost.
async fn invoice_sales_order(Extension(u): Extension<AuthUser>, State(s): State<AppState>, Path(id): Path<Uuid>, b: Option<Json<Json_>>) -> AppResult<Json<Json_>> {
    u.require(&["admin", "manager", "finance"])?;
    let b = body(b);
    let basis = b["basis"].as_str().unwrap_or("shipped");
    if basis != "shipped" && basis != "ordered" { return Err(AppError::BadRequest("basis must be 'shipped' or 'ordered'".into())); }
    let post_cogs = b["post_cogs"].as_bool().unwrap_or(true);
    let date = opt_date(&b, "invoice_date")?.unwrap_or_else(today);
    let due = opt_date(&b, "due_date")?;
    let tax = opt_dec(&b, "tax")?.unwrap_or(Decimal::ZERO);

    let txn = s.db.begin().await?;
    let so = txn.query_one(st("SELECT so_number, customer_id, status, erp_invoice_id, cogs_entry_id FROM sales_orders WHERE id=$1 FOR UPDATE", vec![id.into()])).await?
        .ok_or_else(|| AppError::not_found("sales order not found"))?;
    if let Some(inv) = so.try_get::<Option<i64>>("", "erp_invoice_id")? {
        return Ok(Json(json!({ "sales_order_id": id, "erp_invoice_id": inv, "cogs_entry_id": so.try_get::<Option<i64>>("", "cogs_entry_id")?, "created": false })));
    }
    let status: String = so.try_get("", "status")?;
    if status == "draft" || status == "cancelled" {
        return Err(AppError::Conflict(format!("a {status} sales order cannot be invoiced")));
    }
    let number: String = so.try_get("", "so_number")?;
    let customer_id: Uuid = so.try_get("", "customer_id")?;
    let (erp_customer, _) = ensure_customer(&txn, customer_id, None).await?;

    let qty = if basis == "shipped" { "l.quantity_shipped" } else { "l.quantity" };
    let t = txn.query_one(st(&format!(
        "SELECT COALESCE(SUM({qty}::numeric * l.unit_price),0) AS revenue, COALESCE(SUM(l.quantity_shipped::numeric * p.unit_cost),0) AS cost
         FROM sales_order_lines l JOIN products p ON p.id=l.product_id WHERE l.sales_order_id=$1"), vec![id.into()])).await?
        .ok_or_else(AppError::internal)?;
    let revenue = t.try_get::<Decimal>("", "revenue")?.round_dp(2);
    let cost = t.try_get::<Decimal>("", "cost")?.round_dp(2);
    if revenue <= Decimal::ZERO {
        return Err(AppError::Conflict(if basis == "shipped" { "nothing to invoice: no shipped quantity yet (use basis 'ordered' to bill the order)".into() } else { "nothing to invoice: order value is zero".into() }));
    }
    let revenue_account_id = match b["revenue_account_id"].as_i64() { Some(i) => i, None => account_by_code(&txn, "4000").await?.id };
    let inv = issue_invoice(&txn, IssueInvoice {
        customer_id: erp_customer, invoice_number: number.clone(), invoice_date: date, due_date: due,
        net_amount: revenue, tax, revenue_account_id, description: Some(format!("Sales order {number} ({basis})")),
    }).await?;
    let mut cogs_entry: Option<i64> = None;
    if post_cogs && cost > Decimal::ZERO {
        let cogs = account_by_code(&txn, "5300").await?;
        let inventory = account_by_code(&txn, "1300").await?;
        cogs_entry = Some(post_entry(&txn, date, &format!("COGS for sales order {number}"), "scm_cogs", Some(inv.id), None,
            vec![NewLine::dr(cogs.id, cost), NewLine::cr(inventory.id, cost)]).await?);
    }
    txn.execute(st("UPDATE sales_orders SET erp_invoice_id=$1, cogs_entry_id=$2 WHERE id=$3", vec![inv.id.into(), cogs_entry.into(), id.into()])).await?;
    txn.commit().await?;
    Ok(Json(json!({ "sales_order_id": id, "erp_invoice_id": inv.id, "invoice_amount": inv.amount, "cogs_entry_id": cogs_entry, "cogs_amount": cost, "basis": basis, "created": true })))
}

// ---- supplier invoice -> AP bill ---------------------------------------------------------------

/// POST /api/scm/supplier-invoices/{id}/post-to-ledger  (optional body: expense_account_id, default 1300 Inventory)
/// Matched invoices only: a disputed supplier invoice is not booked until the dispute is resolved.
async fn post_supplier_invoice(Extension(u): Extension<AuthUser>, State(s): State<AppState>, Path(id): Path<Uuid>, b: Option<Json<Json_>>) -> AppResult<Json<Json_>> {
    u.require(&["admin", "manager", "finance"])?;
    let b = body(b);
    let txn = s.db.begin().await?;
    let si = txn.query_one(st("SELECT si.invoice_number, si.invoice_date, si.due_date, si.total, si.status, si.erp_bill_id, po.supplier_id, po.po_number
        FROM supplier_invoices si JOIN purchase_orders po ON po.id=si.purchase_order_id WHERE si.id=$1 FOR UPDATE OF si", vec![id.into()])).await?
        .ok_or_else(|| AppError::not_found("supplier invoice not found"))?;
    if let Some(bill_id) = si.try_get::<Option<i64>>("", "erp_bill_id")? {
        return Ok(Json(json!({ "supplier_invoice_id": id, "erp_bill_id": bill_id, "created": false })));
    }
    let status: String = si.try_get("", "status")?;
    if status == "disputed" { return Err(AppError::Conflict("a disputed supplier invoice cannot be posted until it is matched".into())); }
    let number: String = si.try_get("", "invoice_number")?;
    let po_number: String = si.try_get("", "po_number")?;
    let date: NaiveDate = si.try_get("", "invoice_date")?;
    let due: NaiveDate = si.try_get::<NaiveDate>("", "due_date")?.max(date);
    let amount = si.try_get::<Decimal>("", "total")?.round_dp(2);
    if amount <= Decimal::ZERO { return Err(AppError::Unprocessable("supplier invoice total must be greater than zero".into())); }
    let supplier_id: Uuid = si.try_get("", "supplier_id")?;
    let (vendor_id, _) = ensure_vendor(&txn, supplier_id).await?;
    let expense_id = match b["expense_account_id"].as_i64() { Some(i) => i, None => account_by_code(&txn, "1300").await?.id };
    let exp = crate::erp::entities::account::Entity::find_by_id(expense_id).one(&txn).await?
        .ok_or_else(|| AppError::BadRequest("expense_account_id does not exist".into()))?;
    if exp.account_type != "expense" && exp.account_type != "asset" {
        return Err(AppError::BadRequest("a bill must be charged to an expense or asset account".into()));
    }
    let ap = account_by_code(&txn, AP).await?;
    let je = post_entry(&txn, date, &format!("Supplier invoice {number} (PO {po_number})"), "bill", None, None,
        vec![NewLine::dr(exp.id, amount), NewLine::cr(ap.id, amount)]).await?;
    let bill = bill::ActiveModel {
        id: NotSet, vendor_id: Set(vendor_id), bill_number: Set(number), bill_date: Set(date), due_date: Set(due),
        amount: Set(amount), amount_paid: Set(Decimal::ZERO), expense_account_id: Set(exp.id),
        description: Set(Some(format!("Supply chain PO {po_number}"))), status: Set("open".into()),
        journal_entry_id: Set(Some(je)), created_at: NotSet,
    }.insert(&txn).await?;
    txn.execute(st("UPDATE supplier_invoices SET erp_bill_id=$1 WHERE id=$2", vec![bill.id.into(), id.into()])).await?;
    txn.commit().await?;
    let note = (status == "paid").then_some("invoice is already marked paid in the supply chain module; record the payment against the bill in AP");
    Ok(Json(json!({ "supplier_invoice_id": id, "erp_bill_id": bill.id, "erp_vendor_id": vendor_id, "amount": amount, "due_date": due, "created": true, "note": note })))
}

// ---- quality incident -> QMS nonconformance ----------------------------------------------------

/// POST /api/scm/quality-incidents/{id}/raise-ncr
async fn raise_ncr(Extension(u): Extension<AuthUser>, State(s): State<AppState>, Path(id): Path<Uuid>) -> AppResult<Json<Json_>> {
    u.require(&["admin", "manager", "user"])?;
    let txn = s.db.begin().await?;
    let q = txn.query_one(st("SELECT qi.severity, qi.defect_type, qi.note, qi.occurred_on, qi.quarantined_qty, qi.qms_nonconformance_id, p.sku, p.name AS product, s.name AS supplier
        FROM quality_incidents qi JOIN products p ON p.id=qi.product_id LEFT JOIN suppliers s ON s.id=qi.supplier_id WHERE qi.id=$1 FOR UPDATE OF qi", vec![id.into()])).await?
        .ok_or_else(|| AppError::not_found("quality incident not found"))?;
    if let Some(nc) = q.try_get::<Option<Uuid>>("", "qms_nonconformance_id")? {
        let r = txn.query_one(st("SELECT ncr_number FROM qms_nonconformances WHERE id=$1", vec![nc.into()])).await?;
        let ncr = r.map(|r| r.try_get::<String>("", "ncr_number")).transpose()?;
        return Ok(Json(json!({ "quality_incident_id": id, "nonconformance_id": nc, "ncr_number": ncr, "created": false })));
    }
    let sku: String = q.try_get("", "sku")?;
    let product: String = q.try_get("", "product")?;
    let supplier: Option<String> = q.try_get("", "supplier")?;
    let defect: Option<String> = q.try_get("", "defect_type")?;
    let note: Option<String> = q.try_get("", "note")?;
    let sev: String = q.try_get("", "severity")?;
    let qty: i64 = q.try_get("", "quarantined_qty")?;
    let occurred: NaiveDate = q.try_get("", "occurred_on")?;
    let title = format!("Supply chain incident: {sku} {}", defect.clone().unwrap_or_else(|| "quality defect".into()));
    let desc = format!("Raised from supply chain quality incident {id}. Product {product}{}.{}", supplier.map(|n| format!(", supplier {n}")).unwrap_or_default(),
        note.map(|n| format!(" Note: {n}")).unwrap_or_default());
    let r = txn.query_one(st("INSERT INTO qms_nonconformances (title, description, source, severity, detected_on, product_or_process, quantity_affected, detected_by)
        VALUES ($1,$2,'supplier',$3,$4,$5,$6,$7) RETURNING id, ncr_number",
        vec![title.into(), desc.into(), sev.into(), occurred.into(), sku.into(), (qty.min(i32::MAX as i64) as i32).into(), u.id.into()])).await?
        .ok_or_else(AppError::internal)?;
    let nc: Uuid = r.try_get("", "id")?;
    let ncr: String = r.try_get("", "ncr_number")?;
    txn.execute(st("UPDATE quality_incidents SET qms_nonconformance_id=$1 WHERE id=$2", vec![nc.into(), id.into()])).await?;
    txn.commit().await?;
    Ok(Json(json!({ "quality_incident_id": id, "nonconformance_id": nc, "ncr_number": ncr, "created": true })))
}

// ---- supplier scorecard -> GRC risk ------------------------------------------------------------

/// POST /api/scm/risk/raise-grc-risks — one operational GRC risk per high-risk, critically-incident or
/// poorly-performing supplier that has none open. Likelihood and impact come from the scorecard.
async fn raise_grc_risks(Extension(u): Extension<AuthUser>, State(s): State<AppState>) -> AppResult<Json<Json_>> {
    u.require(&["admin", "manager"])?;
    let cards = crate::supply_chain::routes::supplier_perf::fetch(&s.db, None, None, 3).await?;
    let (mut raised, mut already_open) = (vec![], 0usize);
    for c in cards {
        let weak = c.performance_score.is_some_and(|p| p < 50.0);
        if !(c.risk_band == "high" || c.incidents_critical > 0 || weak) { continue; }
        let txn = s.db.begin().await?;
        let cur = txn.query_one(st("SELECT s.grc_risk_id, r.status FROM suppliers s LEFT JOIN grc_risks r ON r.id=s.grc_risk_id WHERE s.id=$1 FOR UPDATE OF s", vec![c.supplier_id.into()])).await?
            .ok_or_else(|| AppError::not_found("supplier not found"))?;
        if let (Some(_), Some(status)) = (cur.try_get::<Option<Uuid>>("", "grc_risk_id")?, cur.try_get::<Option<String>>("", "status")?) {
            if status != "closed" && status != "accepted" { already_open += 1; continue; }
        }
        let likelihood = if c.risk_band == "high" { 4 } else { 3 };
        let impact = if c.incidents_critical > 0 { 4 } else { 3 };
        let pct = |v: Option<f64>| v.map(|x| format!("{:.0}%", x * 100.0)).unwrap_or_else(|| "n/a".into());
        let desc = format!("Raised from the supply chain scorecard of {} ({}). Structural risk {:.0}% ({}); on-time {}, in-full {}; incidents minor/major/critical {}/{}/{}; performance score {}. Recommendation: {}.",
            c.name, c.code, c.structural_risk * 100.0, c.risk_band, pct(c.on_time_rate), pct(c.in_full_rate),
            c.incidents_minor, c.incidents_major, c.incidents_critical,
            c.performance_score.map(|p| format!("{p:.0}")).unwrap_or_else(|| "n/a".into()), c.recommendation);
        let r = txn.query_one(st("INSERT INTO grc_risks (title, description, category, likelihood, impact, status) VALUES ($1,$2,'operational',$3,$4,'open') RETURNING id, risk_code, rating",
            vec![format!("Supplier risk: {} ({})", c.name, c.code).into(), desc.into(), likelihood.into(), impact.into()])).await?
            .ok_or_else(AppError::internal)?;
        let rid: Uuid = r.try_get("", "id")?;
        txn.execute(st("UPDATE suppliers SET grc_risk_id=$1 WHERE id=$2", vec![rid.into(), c.supplier_id.into()])).await?;
        txn.commit().await?;
        raised.push(json!({ "supplier_id": c.supplier_id, "supplier": c.name, "risk_id": rid, "risk_code": r.try_get::<String>("", "risk_code")?, "rating": r.try_get::<String>("", "rating")? }));
    }
    Ok(Json(json!({ "raised": raised, "already_open": already_open })))
}

// ---- reconciliation ----------------------------------------------------------------------------

/// GET /api/scm/integration/status — what the bridges have not yet carried over. Payables exposure is only
/// included for roles that may read finance.
async fn integration_status(Extension(u): Extension<AuthUser>, State(s): State<AppState>) -> AppResult<Json<Json_>> {
    let r = crate::db::row(&s.db, "SELECT
        (SELECT count(*) FROM suppliers WHERE active AND erp_vendor_id IS NULL) AS suppliers_without_vendor,
        (SELECT count(*) FROM scm_customers WHERE erp_customer_id IS NULL) AS customers_without_erp_customer,
        (SELECT count(*) FROM sales_orders so WHERE so.erp_invoice_id IS NULL AND so.status IN ('partially_shipped','shipped')
            AND EXISTS (SELECT 1 FROM sales_order_lines l WHERE l.sales_order_id=so.id AND l.quantity_shipped>0)) AS shipped_orders_not_invoiced,
        (SELECT count(*) FROM supplier_invoices WHERE erp_bill_id IS NULL AND status<>'disputed') AS supplier_invoices_not_posted,
        (SELECT count(*) FROM supplier_invoices WHERE status='disputed') AS supplier_invoices_disputed,
        (SELECT count(*) FROM quality_incidents WHERE status='open' AND qms_nonconformance_id IS NULL) AS open_incidents_without_ncr", vec![]).await?;
    let mut out = r.as_object().cloned().unwrap_or_default();
    if crate::auth::authorize(&u.role, &axum::http::Method::GET, "/api/finance").is_ok() {
        let f = crate::db::row(&s.db, "SELECT COALESCE(sum(b.amount-b.amount_paid),0)::float8 AS open_payables_to_suppliers
            FROM bills b JOIN suppliers s ON s.erp_vendor_id=b.vendor_id WHERE b.status='open'", vec![]).await?;
        if let Some(o) = f.as_object() { out.extend(o.clone()); }
    }
    Ok(Json(Json_::Object(out)))
}
