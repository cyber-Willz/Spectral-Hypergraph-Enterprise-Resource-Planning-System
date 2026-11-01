//! Static catalogue of the hand-written (non-generated) API: list feeds, analytics reports and row actions.
//! The generated CRUD resources come from `schema_generated.rs`.

/// A read-mostly list backed by a hand-written endpoint (finance, HR, risk, supply chain).
pub struct Feed {
    pub section: &'static str,
    pub slug: &'static str,
    pub title: &'static str,
    pub path: &'static str,
    /// Allow creating records by posting a JSON document.
    pub create: bool,
}

const fn feed(section: &'static str, slug: &'static str, title: &'static str, path: &'static str, create: bool) -> Feed {
    Feed { section, slug, title, path, create }
}

pub static FEEDS: &[Feed] = &[
    feed("finance", "accounts", "Chart of accounts", "/api/finance/accounts", true),
    feed("finance", "journal-entries", "Journal entries", "/api/finance/journal-entries", true),
    feed("finance", "invoices", "Customer invoices", "/api/finance/invoices", true),
    feed("finance", "customers", "Customers", "/api/finance/customers", true),
    feed("finance", "bills", "Vendor bills", "/api/finance/bills", true),
    feed("finance", "vendors", "Vendors", "/api/finance/vendors", true),
    feed("finance", "bank-accounts", "Bank accounts", "/api/finance/bank-accounts", true),
    feed("finance", "cash-transactions", "Cash transactions", "/api/finance/cash/transactions", true),
    feed("finance", "fixed-assets", "Fixed assets", "/api/finance/fixed-assets", true),
    feed("finance", "budgets", "Budgets", "/api/finance/budgets", true),
    feed("hr", "employees", "Employees", "/api/hr/employees", true),
    feed("hr", "departments", "Departments", "/api/hr/departments", true),
    feed("hr", "payroll-runs", "Payroll runs", "/api/hr/payroll/runs", true),
    feed("hr", "postings", "Job postings", "/api/hr/recruitment/postings", true),
    feed("hr", "applications", "Applications", "/api/hr/recruitment/applications", false),
    feed("hr", "benefit-plans", "Benefit plans", "/api/hr/benefit-plans", true),
    feed("hr", "courses", "Training courses", "/api/hr/training/courses", true),
    feed("hr", "reviews", "Performance reviews", "/api/hr/performance/reviews", true),
    feed("risk", "counterparties", "Counterparty risk", "/api/risk/counterparties", false),
    feed("scm", "products", "Products", "/api/scm/products", true),
    feed("scm", "suppliers", "Suppliers", "/api/scm/suppliers", true),
    feed("scm", "customers", "Customers", "/api/scm/customers", true),
    feed("scm", "warehouses", "Warehouses", "/api/scm/warehouses", true),
    feed("scm", "purchase-orders", "Purchase orders", "/api/scm/purchase-orders", true),
    feed("scm", "sales-orders", "Sales orders", "/api/scm/sales-orders", true),
    feed("scm", "shipments", "Shipments", "/api/scm/shipments", false),
    feed("scm", "carriers", "Carriers", "/api/scm/carriers", true),
    feed("scm", "contracts", "Contracts", "/api/scm/contracts", true),
    feed("scm", "production-runs", "Production runs", "/api/scm/production-runs", true),
    feed("scm", "quality-incidents", "Quality incidents", "/api/scm/quality-incidents", true),
    feed("scm", "supplier-invoices", "Supplier invoices", "/api/scm/invoices", false),
    feed("scm", "stock", "Stock levels", "/api/scm/inventory/stock", false),
    feed("scm", "movements", "Stock movements", "/api/scm/inventory/movements", false),
    feed("scm", "cognitive-actions", "Planner actions", "/api/scm/cognitive/actions", false),
];

pub fn feeds_in(section: &str) -> impl Iterator<Item = &'static Feed> + '_ {
    FEEDS.iter().filter(move |f| f.section == section)
}
pub fn find_feed(section: &str, slug: &str) -> Option<&'static Feed> {
    FEEDS.iter().find(|f| f.section == section && f.slug == slug)
}

// ------------------------------------------------------------------ reports

pub struct Param {
    pub name: &'static str,
    pub options: &'static [&'static str],
    /// Substituted into `{name}` in the path instead of the query string.
    pub in_path: bool,
}

pub struct Report {
    pub module: &'static str,
    pub title: &'static str,
    pub path: &'static str,
    pub params: &'static [Param],
}

const fn rep(module: &'static str, title: &'static str, path: &'static str) -> Report {
    Report { module, title, path, params: &[] }
}
const fn rep_p(module: &'static str, title: &'static str, path: &'static str, params: &'static [Param]) -> Report {
    Report { module, title, path, params }
}

pub static REPORTS: &[Report] = &[
    rep("crm", "Pipeline", "/api/crm/analytics/pipeline"),
    rep("crm", "Lead funnel", "/api/crm/analytics/lead-funnel"),
    rep("crm", "Campaigns", "/api/crm/analytics/campaigns"),
    rep("crm", "Service desk", "/api/crm/analytics/service"),
    rep_p("crm", "Sales performance", "/api/crm/sales/performance", &[Param { name: "by", options: &["agent", "manager", "regional_office", "product", "series", "sector", "account", "country", "stage", "month"], in_path: false }]),
    rep("crm", "Cycle time", "/api/crm/sales/cycle-time"),
    rep("crm", "Pipeline health", "/api/crm/sales/pipeline-health"),
    rep("crm", "Revenue forecast", "/api/crm/forecast/revenue"),
    rep("crm", "Weighted pipeline", "/api/crm/forecast/weighted-pipeline"),
    rep_p("crm", "Customer overview", "/api/crm/customers/overview", &[Param { name: "kind", options: &["donor", "consumer"], in_path: false }]),
    rep_p("crm", "RFM segments", "/api/crm/customers/rfm", &[Param { name: "kind", options: &["donor", "consumer"], in_path: false }]),
    rep("crm", "Donor lapse", "/api/crm/donors/lapse"),
    rep("crm", "Donor insights", "/api/crm/donors/insights"),
    rep("crm", "Order analytics", "/api/crm/orders/analytics"),
    rep("crm", "Product affinity", "/api/crm/orders/affinity"),
    rep("crm", "Win-probability model", "/api/crm/predict/model"),
    rep("crm", "Data quality", "/api/crm/data-quality/report"),
    rep("crm", "Duplicates", "/api/crm/data-quality/duplicates"),
    rep("pm", "Portfolio summary", "/api/pm/portfolio/summary"),
    rep_p("pm", "Portfolio breakdown", "/api/pm/portfolio/by/{dimension}", &[Param { name: "dimension", options: &["type", "region", "department", "complexity", "status", "manager", "year", "phase"], in_path: true }]),
    rep("pm", "Finish-or-stop ranking", "/api/pm/portfolio/rankings"),
    rep("pm", "Risk flags", "/api/pm/portfolio/risk-flags"),
    rep_p("pm", "Timeline", "/api/pm/portfolio/timeline", &[Param { name: "granularity", options: &["year", "quarter", "month"], in_path: false }]),
    rep("pm", "Benchmarks", "/api/pm/portfolio/benchmarks"),
    rep("pm", "Delivery analytics", "/api/pm/analytics/portfolio"),
    rep("pm", "Resource utilization", "/api/pm/resources/utilization"),
    rep("grc", "Risk summary", "/api/grc/risks/summary"),
    rep("grc", "Risk heatmap", "/api/grc/risks/heatmap"),
    rep("grc", "Control coverage", "/api/grc/controls/coverage"),
    rep("grc", "Control test schedule", "/api/grc/controls/test-schedule"),
    rep("grc", "Compliance dashboard", "/api/grc/compliance/dashboard"),
    rep("am", "Asset overview", "/api/am/analytics/overview"),
    rep("ehs", "Safety metrics", "/api/ehs/analytics/safety"),
    rep("ehs", "Environmental report", "/api/ehs/environmental/report"),
    rep("ehs", "Compliance", "/api/ehs/compliance"),
    rep("qms", "Quality analytics", "/api/qms/analytics/quality"),
    rep("qms", "Compliance", "/api/qms/compliance"),
    rep("docs", "Records due for disposition", "/api/docs/records/disposition-due"),
    rep("docs", "My approval inbox", "/api/docs/workflow-tasks/inbox"),
    rep("finance", "Trial balance", "/api/finance/reports/trial-balance"),
    rep("finance", "Income statement", "/api/finance/reports/income-statement"),
    rep("finance", "Balance sheet", "/api/finance/reports/balance-sheet"),
    rep("finance", "Receivables aging", "/api/finance/reports/ar-aging"),
    rep("finance", "Payables aging", "/api/finance/reports/ap-aging"),
    rep("finance", "Cash forecast", "/api/finance/cash/forecast"),
    rep("hr", "Workforce summary", "/api/hr/analytics/summary"),
    rep("hr", "Pay benchmark", "/api/hr/compensation/benchmark"),
    rep("risk", "Financial health", "/api/risk/financial-health"),
    rep("risk", "Receivables exposure", "/api/risk/receivables-exposure"),
    rep("risk", "Integrity checks", "/api/risk/integrity-checks"),
    rep("scm", "KPIs", "/api/scm/reports/kpis"),
    rep("scm", "Low stock", "/api/scm/reports/low-stock"),
    rep("scm", "Inventory valuation", "/api/scm/reports/valuation"),
    rep("scm", "ABC / XYZ", "/api/scm/reports/abc-xyz"),
    rep("scm", "Replenishment", "/api/scm/reports/replenishment"),
    rep("scm", "Excess cover", "/api/scm/reports/excess-cover"),
    rep("scm", "Supplier scorecards", "/api/scm/reports/supplier-scorecards"),
    rep("scm", "Carrier scorecards", "/api/scm/reports/carrier-scorecards"),
    rep("scm", "Forecast accuracy", "/api/scm/reports/forecast-accuracy"),
    rep("scm", "Lead-time accuracy", "/api/scm/reports/lead-time-accuracy"),
    rep("scm", "Procurement controls", "/api/scm/reports/procurement-controls"),
    rep("scm", "Production yield", "/api/scm/reports/production-yield"),
    rep("scm", "Stock reconciliation", "/api/scm/reports/stock-reconciliation"),
    rep("scm", "Warehouse utilization", "/api/scm/reports/warehouse-utilization"),
    rep("scm", "Supplier concentration", "/api/scm/resilience/concentration"),
    rep("scm", "Network resilience (spectral)", "/api/scm/resilience/spectral"),
    rep("scm", "Planner status", "/api/scm/cognitive/status"),
    rep("scm", "Planner policies", "/api/scm/cognitive/policies"),
    rep("scm", "Planner forecast", "/api/scm/cognitive/forecast"),
    rep("scm", "Planner memory", "/api/scm/cognitive/memory"),
    rep("scm", "Planner resilience", "/api/scm/cognitive/resilience"),
    rep("scm", "Bridges to finance / CRM / QMS", "/api/scm/integration/status"),
];

pub fn reports_in(module: &str) -> impl Iterator<Item = &'static Report> + '_ {
    REPORTS.iter().filter(move |r| r.module == module)
}

/// Modules in sidebar order, with the icon each uses.
pub static MODULES: &[(&str, &str)] = &[
    ("crm", "users"),
    ("pm", "briefcase"),
    ("grc", "shield"),
    ("am", "tool"),
    ("ehs", "activity"),
    ("qms", "check"),
    ("docs", "file"),
    ("finance", "dollar"),
    ("hr", "user"),
    ("risk", "alert"),
    ("scm", "truck"),
];

// ------------------------------------------------------------------ row actions

#[derive(Clone, Copy, PartialEq)]
pub enum FieldKind {
    Text,
    Area,
    Number,
    Int,
    Date,
    Bool,
    Secret,
    Select(&'static [&'static str]),
}

#[derive(Clone, Copy, PartialEq)]
pub struct ActField {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: FieldKind,
    pub req: bool,
}

const fn f(key: &'static str, label: &'static str, kind: FieldKind, req: bool) -> ActField {
    ActField { key, label, kind, req }
}

#[derive(Clone, Copy, PartialEq)]
pub struct RowAction {
    /// List path this action belongs to; the call goes to `{list_path}/{id}{suffix}`.
    pub list: &'static str,
    pub label: &'static str,
    pub suffix: &'static str,
    pub fields: &'static [ActField],
    /// Only offer when `column` has one of these values (empty = always).
    pub when: Option<(&'static str, &'static [&'static str])>,
    pub danger: bool,
    pub confirm: &'static str,
}

const fn act(list: &'static str, label: &'static str, suffix: &'static str, fields: &'static [ActField]) -> RowAction {
    RowAction { list, label, suffix, fields, when: None, danger: false, confirm: "" }
}
const fn when(mut a: RowAction, col: &'static str, vals: &'static [&'static str]) -> RowAction {
    a.when = Some((col, vals));
    a
}
const fn danger(mut a: RowAction, confirm: &'static str) -> RowAction {
    a.danger = true;
    a.confirm = confirm;
    a
}

const OPP_STAGES: &[&str] = &["prospecting", "qualification", "proposal", "negotiation", "closed_won", "closed_lost"];
const NC_STATUS: &[&str] = &["open", "investigating", "contained", "pending_verification", "closed"];
const BANK_FIELDS: &[ActField] = &[f("bank_account_id", "Bank account id", FieldKind::Int, true), f("payment_date", "Payment date", FieldKind::Date, false), f("amount", "Amount (blank = full balance)", FieldKind::Number, false)];
const POST_RUN: &[ActField] = &[f("bank_account_id", "Bank account id", FieldKind::Int, true)];
const STAGE_FIELDS: &[ActField] = &[f("stage", "Stage", FieldKind::Select(OPP_STAGES), true), f("lost_reason", "Lost reason (required when lost)", FieldKind::Text, false)];
const NC_FIELDS: &[ActField] = &[f("status", "Move to", FieldKind::Select(NC_STATUS), true), f("root_cause", "Root cause", FieldKind::Area, false), f("disposition", "Disposition (needed to close)", FieldKind::Text, false)];
const WO_FIELDS: &[ActField] = &[f("labor_hours", "Labour hours", FieldKind::Number, false), f("labor_rate", "Labour rate", FieldKind::Number, false), f("parts_cost", "Parts cost", FieldKind::Number, false), f("downtime_hours", "Downtime hours", FieldKind::Number, false), f("failure_code", "Failure code", FieldKind::Text, false), f("notes", "Notes", FieldKind::Area, false)];
const DECIDE_FIELDS: &[ActField] = &[f("approve", "Approve", FieldKind::Bool, true), f("comments", "Comments", FieldKind::Area, false)];
const VERIFY_FIELDS: &[ActField] = &[f("effective", "Action was effective", FieldKind::Bool, true), f("notes", "Notes", FieldKind::Area, false)];
const INV_APPROVE: &[ActField] = &[f("reason", "Reason for override", FieldKind::Area, true)];

pub static ACTIONS: &[RowAction] = &[
    act("/api/crm/leads", "Convert to account", "/convert", &[]),
    act("/api/crm/opportunities", "Change stage", "/stage", STAGE_FIELDS),
    act("/api/crm/opportunities", "Create invoice", "/invoice", &[]),
    act("/api/crm/accounts", "Sync to finance", "/sync-customer", &[]),
    act("/api/pm/invoices", "Post to ledger", "/post-to-ledger", &[]),
    act("/api/am/work-orders", "Complete", "/complete", WO_FIELDS),
    act("/api/am/assets", "Capitalise as fixed asset", "/register-fixed-asset", &[]),
    act("/api/qms/nonconformances", "Move workflow", "/transition", NC_FIELDS),
    act("/api/qms/corrective-actions", "Verify", "/verify", VERIFY_FIELDS),
    when(act("/api/docs/workflow-tasks", "Decide", "/decide", DECIDE_FIELDS), "status", &["pending"]),
    when(act("/api/finance/bills", "Pay", "/pay", BANK_FIELDS), "status", &["open"]),
    when(danger(act("/api/finance/bills", "Void", "/void", &[]), "Void this bill? A reversing entry is posted."), "status", &["open"]),
    when(act("/api/finance/invoices", "Receive payment", "/receive", BANK_FIELDS), "status", &["open"]),
    when(danger(act("/api/finance/invoices", "Void", "/void", &[]), "Void this invoice? A reversing entry is posted."), "status", &["open"]),
    when(act("/api/hr/payroll/runs", "Approve", "/approve", &[]), "status", &["draft"]),
    when(act("/api/hr/payroll/runs", "Post to ledger", "/post", POST_RUN), "status", &["approved"]),
    when(act("/api/hr/recruitment/postings", "Close posting", "/close", &[]), "status", &["open"]),
    when(act("/api/scm/purchase-orders", "Submit", "/submit", &[]), "status", &["draft"]),
    when(danger(act("/api/scm/purchase-orders", "Cancel", "/cancel", &[]), "Cancel this purchase order?"), "status", &["draft", "submitted"]),
    when(act("/api/scm/purchase-orders", "Close", "/close", &[]), "status", &["partially_received", "received"]),
    when(act("/api/scm/sales-orders", "Confirm", "/confirm", &[]), "status", &["draft"]),
    when(act("/api/scm/sales-orders", "Allocate stock", "/allocate", &[]), "status", &["confirmed", "backordered", "partially_allocated"]),
    when(act("/api/scm/sales-orders", "Create invoice", "/invoice", &[]), "status", &["partially_shipped", "shipped"]),
    when(danger(act("/api/scm/sales-orders", "Cancel", "/cancel", &[]), "Cancel this sales order?"), "status", &["draft", "confirmed", "backordered", "partially_allocated"]),
    act("/api/scm/suppliers", "Sync to finance (vendor)", "/sync-vendor", &[]),
    act("/api/scm/customers", "Sync to finance (customer)", "/sync-customer", &[]),
    act("/api/scm/invoices", "Approve mismatch", "/approve", INV_APPROVE),
    act("/api/scm/invoices", "Post to ledger", "/post-to-ledger", &[]),
    when(act("/api/scm/cognitive/actions", "Approve", "/approve", &[]), "status", &["proposed", "pending"]),
    when(act("/api/scm/cognitive/actions", "Reject", "/reject", &[]), "status", &["proposed", "pending"]),
    when(act("/api/scm/cognitive/actions", "Execute", "/execute", &[]), "status", &["approved"]),
];

pub fn actions_for(list: &str) -> Vec<&'static RowAction> {
    ACTIONS.iter().filter(|a| a.list == list).collect()
}

impl RowAction {
    pub fn applies(&self, row: &serde_json::Value) -> bool {
        match self.when {
            None => true,
            Some((col, vals)) => row.get(col).and_then(|v| v.as_str()).map(|s| vals.contains(&s)).unwrap_or(true),
        }
    }
}

/// CSV import endpoints (`POST` with the raw file as body).
pub static IMPORTS: &[(&str, &str, &str)] = &[
    ("crm-accounts", "CRM accounts", "/api/crm/import/accounts"),
    ("crm-products", "CRM products", "/api/crm/import/products"),
    ("crm-sales-teams", "CRM sales teams", "/api/crm/import/sales-teams"),
    ("crm-pipeline", "CRM sales pipeline", "/api/crm/import/pipeline"),
    ("crm-funnel", "Sales funnel and forecast", "/api/crm/import/funnel"),
    ("crm-product-info", "Customer behaviour: products", "/api/crm/import/product-info"),
    ("crm-customers", "Customer behaviour: customers", "/api/crm/import/customers"),
    ("crm-sales-orders", "Customer behaviour: orders", "/api/crm/import/sales-orders"),
    ("crm-donors", "Donors", "/api/crm/import/donors"),
    ("pm-projects", "Project portfolio", "/api/pm/import/projects"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn catalogue_is_consistent() {
        // every feed slug is unique within its section
        for (i, a) in FEEDS.iter().enumerate() {
            assert!(FEEDS.iter().skip(i + 1).all(|b| !(a.section == b.section && a.slug == b.slug)), "{} duplicated", a.slug);
            assert!(a.path.starts_with("/api/"));
        }
        for r in REPORTS {
            assert!(r.path.starts_with("/api/"));
            for p in r.params {
                assert!(!p.options.is_empty());
                assert_eq!(p.in_path, r.path.contains(&format!("{{{}}}", p.name)));
            }
        }
        // every action targets a list the UI actually shows
        for a in ACTIONS {
            let known = FEEDS.iter().any(|f| f.path == a.list) || crate::schema::RESOURCES.iter().any(|r| r.path() == a.list);
            assert!(known, "action {} targets unknown list {}", a.label, a.list);
        }
    }

    #[test]
    fn action_visibility() {
        let a = actions_for("/api/finance/bills");
        let pay = a.iter().find(|x| x.label == "Pay").unwrap();
        assert!(pay.applies(&json!({"status":"open"})));
        assert!(!pay.applies(&json!({"status":"paid"})));
        assert!(pay.applies(&json!({"id":1}))); // unknown status: let the server decide
    }
}
