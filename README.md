# Enterprise Suite v0.3 — Axum + SeaORM + PostgreSQL

One Rust service covering: **CRM**, **Project Management**, **GRC**, **Asset Management**, **QMS**, **EHS**, **Document Management**, plus the former `erp_system`: **Finance**, **Human Resources** and **Risk** (merged in v0.2), and the former `supply_chain` service: **Supply Chain** — logistics, warehouse, procurement, resilience and a cognitive planning layer (merged in v0.3).
130+ tables · 11 modules · JWT auth with roles · audit trail · business rules enforced in the database.

## Run (Windows PowerShell)

```powershell
# 1. create the database
psql -U postgres -c "CREATE USER suite WITH PASSWORD 'suite'"
psql -U postgres -c "CREATE DATABASE suite OWNER suite"

# 2. configure and start (migrations run automatically on startup)
Copy-Item .env.example .env        # edit JWT_SECRET (>= 32 chars) and ADMIN_PASSWORD
PS C:\Users\cyber-Willz\Desktop\enterprise_suite_v5> netstat -ano | findstr :8080
PS C:\Users\cyber-Willz\Desktop\enterprise_suite_v5> Get-Process -Id 24300

cd frontend
trunk build --release
cd ..


cargo run --release

# optional: set IMPORT_PM_CSV=data\Project_Management_Dataset.csv in .env to load the sample portfolio at startup

# optional: load the bundled CRM datasets (idempotent)
python scripts\import_crm_datasets.py

# 3. in another window: end-to-end smoke test (needs Python 3)
python scripts\smoke_test.py http://127.0.0.1:8080        # suite modules (140 checks)
python scripts\smoke_test_erp.py http://127.0.0.1:8080    # finance/HR/risk, roles, bridges (42 checks)
python scripts\smoke_test_scm_bridges.py http://127.0.0.1:8080   # supply chain roles + bridges to finance/CRM/QMS/GRC (40 checks)
python scripts\scm\smoke_test.py                          # supply chain core flows (also smoke_test_v2..v4; v4/v5 want an empty database)
# v5 (cognitive layer) needs the demo history first: psql $env:DATABASE_URL -f scripts\scm\seed_cognitive_demo.sql; python scripts\scm\smoke_test_v5.py
cargo test --release --lib                                # unit tests (no database needed)
```

A bootstrap admin (`ADMIN_EMAIL` / `ADMIN_PASSWORD`) is created when the users table is empty.
Login: `POST /api/auth/login {"email","password"}` → `Authorization: Bearer <token>`.

## Web UI

`frontend/` is a Yew + Stylist single-page app covering every module (see `frontend/README.md`). Build it with `trunk build --release` and the API serves it from the same origin.

## Architecture

| Path | Purpose |
|---|---|
| `tools/spec.py` | **Single source of truth**: tables, columns, enums, FKs, checks, DB triggers, seed data |
| `tools/gen.py` | Generates `migrations/001_schema.sql`, `src/entities/*.rs` (SeaORM), `src/routes_generated.rs` |
| `src/crud.rs` | `crud_router!` macro: list / get / create / update / delete with typed filters |
| `src/modules/*.rs` | Hand-written domain logic per module (below) |
| `src/auth.rs` | Argon2 passwords, JWT, RBAC middleware, audit logging |

After editing `tools/spec.py` run `python tools/gen.py`. For an already-deployed database add a **new** migration file
(the runner applies each file once); don't edit `001_schema.sql` in place.

### Generic CRUD (all 47 tables, see `CRUD_ENDPOINTS.md`)
`GET /api/<module>/<resource>?page=&per_page=&sort=&order=&<column>=<value>&<column>_gte=&<column>_lte=`,
`POST` (create), `GET/PUT/PATCH/DELETE /{id}`. Money is `numeric(18,2)` (returned as strings, accepted as numbers or strings).
Errors: 400 bad input · 401/403 auth · 404 · 409 duplicate / FK / invalid transition · 422 missing field, check or business-rule violation.

### Roles
`admin` (everything, user management) · `manager` (read/write/delete, verify CAPAs) · `user` (read/write) · `auditor` (read-only).
Every mutating request is written to `sys_audit_log` (`GET /api/sys/audit-log`).

## Domain endpoints

**CRM** — `POST /api/crm/leads/{id}/convert` (account+contact+opportunity, transactional) · `POST /api/crm/opportunities/{id}/stage` ·
`POST /api/crm/marketing/score-leads` (rule-driven lead scoring from `crm_scoring_rules`) ·
`GET /api/crm/analytics/{pipeline,lead-funnel,campaigns,service}` · `GET /api/crm/analytics/customer-360/{account_id}`.
DB rules: case SLA deadline by priority, resolution timestamps, stage-driven win probability.

**Projects** — `POST /api/pm/projects/{id}/schedule` (critical path: FS/SS/FF/SF + lag, slack, cycle detection) ·
`GET /api/pm/projects/{id}/{costs,accounting,analytics}` (cost tracking, T&M / fixed-price revenue & WIP, earned value CPI/SPI/EAC) ·
`GET /api/pm/resources/utilization` · `GET /api/pm/analytics/portfolio`.

**CRM datasets** (bundled in `data/crm/`; `python scripts/import_crm_datasets.py` loads them in dependency order, safe to re-run)

Import: `POST /api/crm/import/{kind}` with the raw CSV as the body (`?strict=true` aborts on any rejected row). Kinds:
`accounts`, `products`, `sales-teams`, `pipeline` (CRM sales opportunities, 8,800 deals) · `funnel` (sales funnel & revenue forecast) ·
`product-info`, `customers`, `sales-orders` (customer behaviour, deliberately dirty) · `donors` (5,000 donors).
- Cleaning: case/whitespace normalisation, typo repair against canonical sets (`femle`→female, `gld`→gold, `delrd`→delivered, `nrth`→North, `bank transfr`→bank_transfer, `five`→5), product-name drift (`GTXPro`→`GTX Pro`, 1,480 rows), dd-mm-yy dates, ZIP codes zero-padded, millions→USD, missing prices imputed from the catalog (flagged).
- Quarantine: invalid rows are never silently dropped; they go to `crm_import_rejects` (queryable at `/api/crm/import-rejects`) with line number, reason and the raw row. Each run is recorded in `crm_import_batches` with every correction counted. Re-imports upsert (idempotent).
- Opportunities may exist without an account (open prospects); deleting an account no longer deletes its sales history.

Sales analytics: `GET /api/crm/sales/performance?by=agent|manager|regional_office|product|series|sector|account|country|stage|month` (win-rate with Wilson 95% CI and a Bonferroni-corrected significance flag) ·
`/api/crm/sales/cycle-time` · `/api/crm/sales/pipeline-health` (stale deals vs the historical 90th-percentile cycle) · `/api/crm/forecast/revenue` (rolling-origin backtest of naive / 3-month mean / Holt, selects the best, 80% interval, plus a pipeline cross-check that excludes stale deals) · `/api/crm/forecast/weighted-pipeline`.

Win-probability model: `POST /api/crm/predict/train` (L2 logistic regression, time-based train/test split) → `GET /api/crm/predict/model` → `POST /api/crm/predict/score-open`.
**The model is validity-gated**: it is only used if the lower 95% bound of holdout AUC is above 0.5 and it beats the base rate on Brier score. Otherwise open deals are scored with the plain historical win rate and the response says so.

Customers, donors, orders: `GET /api/crm/customers/overview?kind=donor|consumer` · `/api/crm/customers/rfm` (quintile RFM segments; consumer recency is dropped automatically when order dates are constant) ·
`/api/crm/donors/lapse` (active / lapsing / lapsed, reactivation candidates) · `/api/crm/donors/insights` (event uplift test, correlations) · `/api/crm/orders/analytics` · `/api/crm/orders/affinity` (co-purchase lift) ·
`/api/crm/data-quality/duplicates` · `/api/crm/data-quality/report` (import history, reject reasons, completeness, integrity checks). `as_of` defaults to the latest date in the data, not today (`as_of=today` to override).

**Project portfolio** (built around `data/Project_Management_Dataset.csv`: 99 projects, cost, benefit, complexity, phase, region, department, manager)
- `POST /api/pm/import/projects[?strict=true&filename=]` — body is the raw CSV (`Content-Type: text/csv`). Idempotent upsert keyed on a deterministic project code; creates project managers as `pm_resources`; invalid rows are reported with line numbers (or abort everything with `strict=true`). Each import is logged in `pm_import_batches`. Imported projects are non-billable internal projects: `budget` = Project Cost, `expected_benefit` = Project Benefit. Re-importing overwrites those fields, including status/completion.
- `GET /api/pm/portfolio/summary` — cost, benefit, ROI, benefit/cost, realized benefit (completed only), in-flight and on-hold benefit, cancelled sunk cost, overdue active projects, cost↔benefit correlation.
- `GET /api/pm/portfolio/by/{type|region|department|complexity|status|manager|year|phase}` — same measures per group, plus success rate = completed / (completed + cancelled).
- `GET /api/pm/portfolio/rankings` — finish-or-stop ranking of unfinished work by *marginal return* (expected benefit ÷ remaining cost; sunk cost ignored).
- `GET /api/pm/portfolio/risk-flags` — overdue active projects, projects cancelled at ≥70% complete, completed-before-final-phase (data inconsistency), on-hold at ≥85% complete.
- `GET /api/pm/portfolio/timeline?granularity=year|quarter|month`.
- `GET /api/pm/portfolio/benchmarks` and `POST /api/pm/portfolio/estimate` — reference-class cost/benefit/duration percentiles and completion probability for a prospective project (widens the class until ≥8 comparable projects).
- All of the above accept filters `project_type, region, department, complexity, status, manager, phase_no, year`.
- `GET /api/pm/projects/{id}/analytics` now falls back to project-level earned value (`"basis":"project"`) for projects without tasks.
- Portfolio scope = projects with `expected_benefit` set; projects created without a business case are excluded.

**GRC** — `GET /api/grc/risks/{heatmap,summary}` · `GET /api/grc/controls/{coverage,test-schedule}` ·
`POST /api/grc/controls/{id}/test` · `GET /api/grc/compliance/dashboard?area=` · `GET /api/grc/audits/{id}/report`.
Risk score/rating are generated columns (likelihood × impact).

**Assets** — `POST /api/am/maintenance/generate-work-orders` · `POST /api/am/work-orders/{id}/complete` ·
`POST /api/am/meter-readings/bulk` · `POST /api/am/predictive/analyze` (z-score anomaly + trend-to-threshold, optional auto work order) ·
`GET /api/am/assets/{id}/lifecycle` (depreciation, TCO, MTTR/MTBF) · `GET /api/am/analytics/overview`.

**QMS** — failed test results auto-raise a non-conformance (DB trigger) · `POST /api/qms/nonconformances/{id}/transition` (workflow graph; cannot close with open CAPAs) ·
`POST /api/qms/corrective-actions/{id}/verify` · `GET /api/qms/analytics/quality` (pass rates, Pareto) · `GET /api/qms/compliance`.

**EHS** — `GET /api/ehs/analytics/safety` (TRIR, LTIFR, severity rate) · `POST /api/ehs/safety-audits/{id}/complete` (scoring) ·
`GET /api/ehs/environmental/report` (summaries, monthly series, limit exceedances) · `POST /api/ehs/incidents/{id}/corrective-action` · `GET /api/ehs/compliance`.

**Documents** — `POST/GET /api/docs/documents/{id}/versions` (immutable, SHA-256) · `GET .../versions/{n}` · `GET .../diff?from=&to=` ·
`POST .../rollback` · `POST .../workflow` · `GET /api/docs/workflow-tasks/inbox` · `POST /api/docs/workflow-tasks/{id}/decide` (role-based, segregation of duties) ·
`GET /api/docs/records/disposition-due` · `POST /api/docs/records/{id}/dispose`. Records and legal-hold documents cannot be deleted.

## What the sample dataset shows (as of the import date)
- 99 projects, cost 411.5M, expected benefit 874.0M (ROI 112%); 30 completed, 27 cancelled, 25 in progress, 17 on hold.
- All 25 in-progress projects are past their end date; all 27 cancelled projects were cancelled at 72–90% complete (≈95.3M estimated sunk cost).
- 24 of 30 completed projects are not in "Phase 5 - Measure", so phase and status disagree in the source data.
- Benefit barely scales with cost (correlation −0.16), so ROI is driven almost entirely by cost.
- COST REDUCTION has the highest success rate (71%) but not the highest ROI; INCOME GENERATION has the highest ROI (125%).
- On this data every unfinished project scores ≥3 on marginal return, so the `prioritise/continue/review_for_stop` thresholds don't separate them; use the ratio itself for ordering.

## What the CRM datasets show
- `CRM Sales Opportunities` and `Predictive Analytics for CRM Sales Performance` are byte-identical (same hashes for every file); one copy is bundled.
- **Win/loss is not predictable from the available fields.** On a time-based holdout the model scores AUC 0.499 (95% CI 0.467-0.531), Brier skill -0.002 (confirmed independently with scikit-learn: 0.49-0.51 for every feature set). Open deals are therefore scored with the 62.7% base rate.
- Agent win rates range from 55% to 70%, but **none of the 30 agents differs significantly** from the overall rate; do not rank agents on win rate.
- 1,432 of 2,089 open deals are older than the historical 90th-percentile cycle (106 days). The naive pipeline expectation is 3.1M, but only 0.9M comes from live deals; the statistical forecast is about 0.93M per month (backtest MAPE 15.4%).
- 1,425 open deals have no account; 1,480 pipeline rows had a product name that did not match the catalog.
- Donors: engagement score is unrelated to amount given (r = -0.005) and event attendance makes no significant difference (|t| = 0.44); 13 donor e-mail addresses are shared by two records, but none with the same name. 1,695 of 5,000 donors (20.9M lifetime value) are lapsed relative to the latest gift date.
- Customer orders: every order has the same date (6 Jul 2025), so recency/trends cannot be analysed; 1,048 of 2,963 orders are priced more than 2x or less than 0.5x the catalog price; product co-purchase lifts are all about 1.0 (no cross-sell signal).

## Notes & limits
- Document content is stored as text (`content_text`) or referenced by `storage_uri`; there is no binary upload endpoint.
- Predictive maintenance is statistical (z-score + linear trend), not a trained ML model.
- The forecast has only 10 months of history, so no seasonality can be estimated.
- Datasets are synthetic: the absence of predictive signal is a property of the data, not of the code. With real CRM data the same gate will let a model through if it earns it.
- Schedule uses calendar days (no working-day calendars).
- Set a strong `JWT_SECRET` and change the bootstrap password before any real deployment; put the service behind TLS.
- Verified here: compiles on Rust 1.91, `scripts/smoke_test.py` passes 140/140 checks against PostgreSQL 16 (upgrade from 001/002 and a fresh install both tested).


## Finance, HR and Risk (merged from erp_system)

Endpoints are unchanged: `/api/finance/*`, `/api/hr/*`, `/api/risk/*` (see the endpoint tables in `docs/ERP_REFERENCE.md`).
Tables are unprefixed (`accounts`, `journal_entries`, `invoices`, `employees`, ...). Code lives in `src/erp/`; the ledger is
double-entry and immutable (DB triggers), payroll posts balanced entries, risk models are described in `docs/DATASET_EVALUATION.md`.
`cargo run --release --bin dataset_eval -- <data_dir>` evaluates the risk models against the reference datasets;
`scripts/load_datasets.py` / `load_hr_dataset.py` load volume-test data.

**Auth change:** the ERP's shared `x-api-key` is gone. Everything uses JWT login + the role policy below, and every ERP write is now in `sys_audit_log`.

| role | CRM, PM, GRC, assets, QMS, EHS, docs | finance + risk | HR (salaries) | audit log |
|---|---|---|---|---|
| admin, manager | read/write | read/write | read/write | read |
| auditor | read | read | read | read |
| user | read/write | none | none | none |
| finance | read (+ the finance bridges below) | read/write | none | none |
| hr | read (+ employee sync) | none | read/write | none |

DELETE needs admin/manager, or the owning finance/hr role inside its own area. Error codes follow the suite's convention
(FK violation 409, check/business-rule violation 422; the standalone ERP used 400).

## Cross-module bridges

| Endpoint | Roles | Effect |
|---|---|---|
| `POST /api/crm/accounts/{id}/sync-customer` | admin, manager, finance | links (or creates) the ERP customer for a CRM account (`crm_accounts.erp_customer_id`) |
| `POST /api/crm/opportunities/{id}/invoice` | admin, manager, finance | closed-won opportunity -> AR invoice + journal entry; optional `tax`, `revenue_account_id`, `invoice_number`, dates; one invoice per opportunity |
| `POST /api/pm/invoices/{id}/post-to-ledger` | admin, manager, finance | project invoice -> AR invoice (Dr AR gross, Cr revenue, Cr 2200 Sales Tax Payable). Paying/voiding it in the ledger updates the project invoice via a DB trigger |
| `POST /api/pm/resources/sync-employees[?apply_cost=true]` | admin, manager, hr | mirrors active employees into PM resources; `apply_cost` sets hourly cost = salary / 2080 (salary-derived, so it makes pay visible to PM readers: use deliberately) |
| `POST /api/am/assets/{id}/register-fixed-asset` | admin, manager, finance | capitalises an asset (needs `paid_from_account_id`); `GET /api/am/assets/{id}/lifecycle` then includes a `ledger` block |
| `POST /api/risk/integrity-checks/raise-grc-risks` | admin, manager, finance | failing/warning integrity checks become GRC risks (idempotent) |
| `GET /api/enterprise/overview` | any role | cross-module KPIs; finance/HR sections only for roles allowed to read them |

All bridge flows are single transactions and post through the same ledger code as the native endpoints.

## Supply chain (merged from supply_chain v5)

Mounted at `/api/scm` behind the suite's JWT login, role policy and audit log (reads: every role; writes: admin, manager, user; finance only through the bridges below). The standalone service's `x-api-key` and `/api/v1` prefix are gone. Full reference: `docs/supply_chain/` (`README.md`, `ANALYSIS.md`, `COGNITIVE.md`); its example scripts live in `scripts/scm/`.
Contents: master data (warehouses, bins, products, suppliers, carriers, `scm_customers`), inventory with lots/FEFO/quarantine, purchasing, sales and shipments, contracts and supplier invoices with three-way matching, supplier scorecards, demand forecasting and planning, resilience (what-if, risk), production/BOM, and the cognitive layer (supply-chain connectome, hcrnn / spectral hypergraph). Research crates are in `crates/` (a workspace; `cargo build` builds only the service).

### Supply chain bridges

| Endpoint (under `/api/scm`) | Roles | Effect |
|---|---|---|
| `POST /suppliers/{id}/sync-vendor` | admin, manager, finance | links (or creates) the AP vendor for a supplier (`suppliers.erp_vendor_id`) |
| `POST /customers/{id}/sync-customer` | admin, manager, finance | links (or creates) the ERP customer; body `{"crm_account_id"}` also ties it to a CRM account |
| `POST /sales-orders/{id}/invoice` | admin, manager, finance | AR invoice (Dr AR / Cr revenue, via the ERP invoice code, so blocked customers are refused) plus COGS (Dr 5300 / Cr 1300 Inventory at product unit cost of shipped goods). Body: `basis` `shipped` (default) or `ordered`, `tax`, dates, `revenue_account_id`, `post_cogs`. One invoice per order |
| `POST /supplier-invoices/{id}/post-to-ledger` | admin, manager, finance | matched supplier invoice -> AP bill (Dr 1300 Inventory or `expense_account_id` / Cr AP); disputed invoices are refused |
| `POST /quality-incidents/{id}/raise-ncr` | admin, manager, user | supplier-sourced QMS nonconformance |
| `POST /risk/raise-grc-risks` | admin, manager | operational GRC risk for each high-risk, critical-incident or low-performance supplier (idempotent while open) |
| `GET /integration/status` | any role | what is unlinked, unbilled or unposted; payables exposure for roles that may read finance |

`GET /api/enterprise/overview` gains a `supply_chain` section. All flows are idempotent, single transactions.

## Upgrading
- **From supply_chain v5:** start the suite on a new database (migrations 008-013 create the supply chain schema; its `customers` table is now `scm_customers`), then copy data with `pg_dump --data-only` (rename `customers` to `scm_customers` in the dump) and update clients to `/api/scm` with a bearer token. The old SeaORM migration table is not used.
- **From enterprise_suite v4:** just start v0.3; migrations 008-013 apply once.
- **From enterprise_suite v3:** just start v0.2; migrations 004-007 are applied once (new ERP tables, link columns, roles, sales-tax account, trigger).
- **From a standalone erp_system database:** start v0.2 on a new database, then copy the data and realign sequences:
  `pg_dump --data-only --exclude-table=accounts <old_db> | psql <new_db>` then `psql <new_db> -f scripts/reset_erp_sequences.sql`
  (dump the other tables as needed; `accounts` is seeded by the migration, so restore your own chart separately if you changed it). Tested with customers/departments/employees.
- Regenerate the suite's schema code after editing `tools/spec.py` with `python tools/gen.py`.
