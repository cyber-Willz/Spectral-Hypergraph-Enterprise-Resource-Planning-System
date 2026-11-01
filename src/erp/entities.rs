//! SeaORM entities for every table in `migrations/001_init.sql`.
//! Defined through a small macro to avoid ~35 near-identical files; each expands to a
//! normal `pub mod <name> { Model, Entity, Column, ActiveModel }`.

macro_rules! entity {
    ($m:ident, $table:literal, { $($(#[$a:meta])* $f:ident : $t:ty),* $(,)? }) => {
        pub mod $m {
            use sea_orm::entity::prelude::*;
            use serde::Serialize;

            #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize)]
            #[sea_orm(table_name = $table)]
            pub struct Model {
                $( $(#[$a])* pub $f: $t, )*
            }

            #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
            pub enum Relation {}

            impl ActiveModelBehavior for ActiveModel {}
        }
    };
}

// ── General ledger ──
entity!(account, "accounts", {
    #[sea_orm(primary_key)] id: i64,
    code: String, name: String, account_type: String,
    parent_id: Option<i64>, is_active: bool, created_at: DateTimeUtc,
});
entity!(journal_entry, "journal_entries", {
    #[sea_orm(primary_key)] id: i64,
    entry_date: Date, memo: String, source: String, source_id: Option<i64>,
    reverses_entry_id: Option<i64>, created_at: DateTimeUtc,
});
entity!(journal_line, "journal_lines", {
    #[sea_orm(primary_key)] id: i64,
    entry_id: i64, account_id: i64, debit: Decimal, credit: Decimal, memo: Option<String>,
});

// ── Cash ──
entity!(bank_account, "bank_accounts", {
    #[sea_orm(primary_key)] id: i64,
    name: String, account_mask: Option<String>, gl_account_id: i64, currency: String,
    is_active: bool, created_at: DateTimeUtc,
});
entity!(cash_transaction, "cash_transactions", {
    #[sea_orm(primary_key)] id: i64,
    bank_account_id: i64, txn_date: Date, amount: Decimal, description: String,
    source: String, source_id: Option<i64>, journal_entry_id: Option<i64>,
    reconciled: bool, reconciled_on: Option<Date>, created_at: DateTimeUtc,
});

// ── Payables ──
entity!(vendor, "vendors", {
    #[sea_orm(primary_key)] id: i64,
    name: String, email: Option<String>, phone: Option<String>,
    payment_terms_days: i32, is_active: bool, created_at: DateTimeUtc,
});
entity!(bill, "bills", {
    #[sea_orm(primary_key)] id: i64,
    vendor_id: i64, bill_number: String, bill_date: Date, due_date: Date,
    amount: Decimal, amount_paid: Decimal, expense_account_id: i64,
    description: Option<String>, status: String, journal_entry_id: Option<i64>,
    created_at: DateTimeUtc,
});
entity!(bill_payment, "bill_payments", {
    #[sea_orm(primary_key)] id: i64,
    bill_id: i64, payment_date: Date, amount: Decimal, bank_account_id: i64,
    journal_entry_id: Option<i64>, created_at: DateTimeUtc,
});

// ── Receivables ──
entity!(customer, "customers", {
    #[sea_orm(primary_key)] id: i64,
    name: String, email: Option<String>, phone: Option<String>,
    payment_terms_days: i32, is_active: bool, created_at: DateTimeUtc,
});
entity!(invoice, "invoices", {
    #[sea_orm(primary_key)] id: i64,
    customer_id: i64, invoice_number: String, invoice_date: Date, due_date: Date,
    amount: Decimal, amount_paid: Decimal, revenue_account_id: i64,
    description: Option<String>, status: String, journal_entry_id: Option<i64>,
    created_at: DateTimeUtc,
});
entity!(invoice_payment, "invoice_payments", {
    #[sea_orm(primary_key)] id: i64,
    invoice_id: i64, payment_date: Date, amount: Decimal, bank_account_id: i64,
    journal_entry_id: Option<i64>, created_at: DateTimeUtc,
});

// ── Fixed assets ──
entity!(fixed_asset, "fixed_assets", {
    #[sea_orm(primary_key)] id: i64,
    asset_tag: String, name: String, asset_account_id: i64,
    accumulated_depreciation_account_id: i64, depreciation_expense_account_id: i64,
    acquisition_date: Date, cost: Decimal, salvage_value: Decimal,
    useful_life_months: i32, accumulated_depreciation: Decimal, status: String,
    acquisition_entry_id: Option<i64>, created_at: DateTimeUtc,
});
entity!(depreciation_entry, "depreciation_entries", {
    #[sea_orm(primary_key)] id: i64,
    asset_id: i64, period_end: Date, amount: Decimal, journal_entry_id: i64,
});

// ── Budgets ──
entity!(budget, "budgets", {
    #[sea_orm(primary_key)] id: i64,
    name: String, fiscal_year: i32, created_at: DateTimeUtc,
});
entity!(budget_line, "budget_lines", {
    #[sea_orm(primary_key)] id: i64,
    budget_id: i64, account_id: i64, month: i16, amount: Decimal,
});

// ── HR ──
entity!(department, "departments", {
    #[sea_orm(primary_key)] id: i64,
    name: String, created_at: DateTimeUtc,
});
entity!(employee, "employees", {
    #[sea_orm(primary_key)] id: i64,
    employee_number: String, first_name: String, last_name: String, email: String,
    department_id: Option<i64>, job_title: String, hire_date: Date,
    termination_date: Option<Date>, status: String, base_salary: Decimal,
    created_at: DateTimeUtc,
    education_level: Option<String>, years_experience: Option<i32>,
    city_tier: Option<i16>, remote_work: Option<bool>,
});
entity!(job_posting, "job_postings", {
    #[sea_orm(primary_key)] id: i64,
    title: String, department_id: Option<i64>, description: Option<String>,
    status: String, opened_on: Date, closed_on: Option<Date>, created_at: DateTimeUtc,
});
entity!(application, "applications", {
    #[sea_orm(primary_key)] id: i64,
    posting_id: i64, first_name: String, last_name: String, email: String,
    phone: Option<String>, stage: String, notes: Option<String>, applied_on: Date,
    employee_id: Option<i64>, created_at: DateTimeUtc,
});
entity!(benefit_plan, "benefit_plans", {
    #[sea_orm(primary_key)] id: i64,
    name: String, plan_type: String, employee_cost: Decimal, employer_cost: Decimal,
    is_active: bool, created_at: DateTimeUtc,
});
entity!(employee_benefit, "employee_benefits", {
    #[sea_orm(primary_key)] id: i64,
    employee_id: i64, plan_id: i64, effective_date: Date, end_date: Option<Date>,
    created_at: DateTimeUtc,
});
entity!(payroll_run, "payroll_runs", {
    #[sea_orm(primary_key)] id: i64,
    period_start: Date, period_end: Date, pay_date: Date, status: String,
    journal_entry_id: Option<i64>, created_at: DateTimeUtc,
});
entity!(payslip, "payslips", {
    #[sea_orm(primary_key)] id: i64,
    run_id: i64, employee_id: i64, gross_pay: Decimal, tax_withheld: Decimal,
    employee_benefit_deductions: Decimal, employer_benefit_cost: Decimal, net_pay: Decimal,
});
entity!(training_course, "training_courses", {
    #[sea_orm(primary_key)] id: i64,
    title: String, description: Option<String>, duration_hours: Decimal,
    created_at: DateTimeUtc,
});
entity!(training_enrollment, "training_enrollments", {
    #[sea_orm(primary_key)] id: i64,
    employee_id: i64, course_id: i64, status: String, enrolled_on: Date,
    completed_on: Option<Date>, score: Option<i16>,
});
entity!(performance_review, "performance_reviews", {
    #[sea_orm(primary_key)] id: i64,
    employee_id: i64, reviewer_id: i64, period_label: String, period_start: Date,
    period_end: Date, rating: i16, strengths: Option<String>, improvements: Option<String>,
    comments: Option<String>, created_at: DateTimeUtc,
});

// ── Risk ──
entity!(counterparty_risk_profile, "counterparty_risk_profiles", {
    #[sea_orm(primary_key)] id: i64,
    party_kind: String, party_id: i64, credit_score: i32, debt_ratio: Decimal,
    liquidity: Decimal, legal_cases: i32, is_bankrupt: bool, assessed_on: Date,
    updated_at: DateTimeUtc,
});
