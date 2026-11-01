# Changelog

## 0.4.0 — Web UI
- New `frontend/` crate: Yew + Stylist SPA for all modules (dashboard, generic CRUD for 47 resources, finance/HR/risk/supply-chain lists, workflow actions, ~70 analytics views, CSV import, user admin, light/dark theme).
- `tools/gen.py` also generates `frontend/src/schema_generated.rs`.
- API serves `frontend/dist` (or `FRONTEND_DIST`) as a fallback; tower-http `fs` feature added. Workspace excludes `frontend/`.
## 0.3.0 — merged with supply_chain v5
- Supply chain integrated as `src/supply_chain/` at `/api/scm` (migrations 008-012 = former sea-orm migrations; 013 = integration links); research crates in `crates/` (workspace).
- axum 0.7 -> 0.8 for the supply chain routes; its API key and `/api/v1` replaced by the suite's JWT, role policy and audit log.
- `customers` renamed `scm_customers` (collided with the ERP table).
- Seven new bridges (vendor, customer/CRM, sales order -> invoice + COGS, supplier invoice -> bill, incident -> QMS NCR, supplier risk -> GRC, reconciliation) and a supply chain block in the enterprise overview. Ledger accounts 1300 Inventory, 5300 COGS, 5400 Freight.
- New scripts: `smoke_test_scm_bridges.py`; supply chain smoke tests moved to `scripts/scm/` and use JWT.
## 0.2.0 — merged with erp_system_improved_v2
- Finance, HR and Risk modules integrated (src/erp/, migrations 004-006); one binary, one database, one login.
- JWT + role policy replaces the ERP API key; new `finance` and `hr` roles; audit log restricted; ERP writes audited.
- Seven cross-module bridges and an enterprise overview (migration 007, src/modules/bridges.rs).
- Sales tax payable account (2200); invoice issuing/asset opening refactored into reusable functions.
- Crate is now lib + bins; `dataset_eval` kept. New scripts: smoke_test_erp.py, reset_erp_sequences.sql.
- Behaviour change: ERP errors use suite codes (FK 409, check 422).
