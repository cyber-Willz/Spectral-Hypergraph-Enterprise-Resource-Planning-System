> Reference copy of the standalone erp_system README. Auth is now JWT + roles (see ../README.md); paths below are unchanged.

# erp_system

ERP core on **Axum + SeaORM + PostgreSQL**: Financial Management, Human Resources and Risk.
The schema in `migrations/` is idempotent and applied at startup.

```bash
createdb erp
cp .env.example .env      # set DATABASE_URL and API_KEY
cargo run                 # listens on BIND_ADDR
cargo test                # unit tests (no database needed)
./scripts/smoke_test.sh   # end-to-end API run against a running server (set BASE_URL, API_KEY)
python3 scripts/load_datasets.py <dir>     # finance volume test; scripts/load_hr_dataset.py <dir> loads employees
cargo run --bin dataset_eval -- <data_dir> [<data_dir>...]   # evaluate the models against the reference datasets
```

All `/api/*` routes need the `x-api-key` header when `API_KEY` is set. `GET /health` is open.
Money is `NUMERIC(18,2)` (`rust_decimal`), sent and returned as strings.

## Financial management — `/api/finance`
| Area | Endpoints |
|---|---|
| General ledger | `POST/GET /accounts`, `POST/GET /journal-entries`, `GET /journal-entries/{id}`, `POST /journal-entries/{id}/reverse` |
| Accounts payable | `POST/GET /vendors`, `POST/GET /bills`, `GET /bills/{id}`, `POST /bills/{id}/pay`, `POST /bills/{id}/void` |
| Accounts receivable | `POST/GET /customers`, `POST/GET /invoices`, `GET /invoices/{id}`, `POST /invoices/{id}/receive`, `POST /invoices/{id}/void` |
| Fixed assets | `POST/GET /fixed-assets`, `GET /fixed-assets/{id}`, `POST /fixed-assets/depreciate {period_end}` (straight-line, monthly, idempotent) |
| Budgeting & forecasting | `POST/GET /budgets`, `GET /budgets/{id}`, `GET /budgets/{id}/variance?through_month=`, `GET /cash/forecast?weeks=` |
| Cash management | `POST/GET /bank-accounts`, `POST/GET /cash/transactions`, `POST /cash/transactions/{id}/reconcile` |
| Financial reporting | `GET /reports/trial-balance`, `/income-statement?from=&to=`, `/balance-sheet`, `/ap-aging`, `/ar-aging` |

## Human resources — `/api/hr`
| Area | Endpoints |
|---|---|
| Employee records | `POST/GET /departments`, `POST/GET /employees`, `GET/PATCH /employees/{id}`, `POST /employees/{id}/terminate` |
| Recruitment | `POST/GET /recruitment/postings`, `POST .../postings/{id}/close`, `POST .../postings/{id}/applications`, `GET .../applications`, `POST .../applications/{id}/advance`, `POST .../applications/{id}/hire` |
| Payroll | `POST/GET /payroll/runs`, `GET/DELETE(draft) /payroll/runs/{id}`, `POST .../approve`, `POST .../post {bank_account_id}`, `GET /employees/{id}/payslips` |
| Benefits | `POST/GET /benefit-plans`, `POST/GET /employees/{id}/benefits`, `POST /employee-benefits/{id}/end` |
| Training | `POST/GET /training/courses`, `POST /training/enrollments`, `POST /training/enrollments/{id}/complete`, `GET /employees/{id}/training` |
| Performance | `POST/GET /performance/reviews` |
| Compensation benchmark | `GET /compensation/benchmark?limit=` (expected pay fitted on your own employees; outliers; needs >= 50 employees with `education_level`, `years_experience`, `city_tier`, `remote_work`) |
| Workforce analytics | `GET /analytics/summary` (headcount, departments, salary, tenure, turnover, ratings, training completion, recruiting funnel) |

## Risk — `/api/risk`
Models were fitted on and validated against the supplied datasets; accuracy, limits and the datasets
that were rejected are in [docs/DATASET_EVALUATION.md](docs/DATASET_EVALUATION.md).
| Area | Endpoints |
|---|---|
| Counterparty credit | `PUT/GET /counterparties/{customer\|vendor}/{id}`, `GET /counterparties?kind=&band=` (score from credit score, debt ratio, liquidity; CV AUC 0.72) |
| Receivables exposure | `GET /receivables-exposure` (open AR by risk band, risk-weighted exposure, unassessed AR) |
| Financial health | `GET /financial-health?as_of=&months=` (ledger ratios, benchmark-relative score, warning flags) |
| Integrity | `GET /integrity-checks` (10 checks: trial balance, AR/AP control accounts, cash vs ledger, abnormal balances, stale AR, unreconciled cash, payroll vs ledger, terminated-employee benefits, pay-benchmark data completeness) |

Scores are screening aids relative to the benchmark data, not default probabilities. Invoices to a
customer flagged bankrupt are rejected.

## Design notes
- **Double entry throughout.** Every bill, invoice, payment, asset acquisition, depreciation run and payroll run posts a balanced journal entry in the same DB transaction as the business record.
- **Immutable ledger.** DB triggers reject UPDATE/DELETE on journal tables; fix mistakes by reversing (manual entries) or voiding (bills/invoices).
- **Cash stays reconciled with the GL.** A bank account's GL account can only move via cash transactions, bill/invoice payments and payroll; direct manual postings to it are rejected. The same applies to the AR (1100) and AP (2000) control accounts, so the subledgers always reconcile to the GL (verified by `/api/risk/integrity-checks`).
- **Payroll flow:** draft (computed from salaries, proration and benefit enrolments) → approved → posted. Posting: Dr Salaries, Dr Benefits; Cr Bank (net), Cr Payroll Liabilities (tax + deductions + employer benefits). Remit liabilities later with a cash transaction against account 2100.
- Concurrency: documents are row-locked before payment/void/post; payroll creation takes an advisory lock to prevent overlapping periods.

## Known limits (deliberate scope cuts)
- Payroll tax is a flat `PAYROLL_TAX_RATE`; replace with real jurisdictional tables before production use.
- Auth is a single shared API key: no users, roles or per-record permissions. Salary data is sensitive; add RBAC before multi-user use.
- Pay benchmark: fitted on your own data (not the market); flags are prompts for review, not evidence of unfair pay; age and gender are not inputs.
- Risk scores: see the limits in docs/DATASET_EVALUATION.md (relative scores, synthetic training data, no out-of-sample test on real customers).
- Single currency per ledger, no tax/VAT on bills or invoices, no asset disposal, no bank transfers between accounts, no period close.
- The test suite covers pure logic; the API was exercised end-to-end against PostgreSQL 16 but there are no automated integration tests yet.
