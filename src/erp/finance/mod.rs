//! Financial management: GL, AP, AR, fixed assets, budgeting & forecasting, cash, reporting.

pub mod assets;
pub mod budget;
pub mod cash;
pub mod ledger;
pub mod payables;
pub mod receivables;

use axum::{
    routing::{get, post},
    Router,
};

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        // General ledger
        .route("/accounts", post(ledger::create_account).get(ledger::list_accounts))
        .route("/journal-entries", post(ledger::create_entry).get(ledger::list_entries))
        .route("/journal-entries/{id}", get(ledger::get_entry))
        .route("/journal-entries/{id}/reverse", post(ledger::reverse_manual))
        // Reports
        .route("/reports/trial-balance", get(ledger::trial_balance))
        .route("/reports/income-statement", get(ledger::income_statement))
        .route("/reports/balance-sheet", get(ledger::balance_sheet))
        .route("/reports/ap-aging", get(payables::aging))
        .route("/reports/ar-aging", get(receivables::aging))
        // Accounts payable
        .route("/vendors", post(payables::create_vendor).get(payables::list_vendors))
        .route("/bills", post(payables::create_bill).get(payables::list_bills))
        .route("/bills/{id}", get(payables::get_bill))
        .route("/bills/{id}/pay", post(payables::pay_bill))
        .route("/bills/{id}/void", post(payables::void_bill))
        // Accounts receivable
        .route("/customers", post(receivables::create_customer).get(receivables::list_customers))
        .route("/invoices", post(receivables::create_invoice).get(receivables::list_invoices))
        .route("/invoices/{id}", get(receivables::get_invoice))
        .route("/invoices/{id}/receive", post(receivables::receive_payment))
        .route("/invoices/{id}/void", post(receivables::void_invoice))
        // Fixed assets
        .route("/fixed-assets", post(assets::create_asset).get(assets::list_assets))
        .route("/fixed-assets/depreciate", post(assets::depreciate))
        .route("/fixed-assets/{id}", get(assets::get_asset))
        // Budgeting & forecasting
        .route("/budgets", post(budget::create_budget).get(budget::list_budgets))
        .route("/budgets/{id}", get(budget::get_budget))
        .route("/budgets/{id}/variance", get(budget::variance))
        .route("/cash/forecast", get(cash::forecast))
        // Cash management
        .route("/bank-accounts", post(cash::create_bank).get(cash::list_banks))
        .route("/cash/transactions", post(cash::create_txn).get(cash::list_txns))
        .route("/cash/transactions/{id}/reconcile", post(cash::reconcile))
}
