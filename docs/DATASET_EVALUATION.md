# Dataset evaluation

Ten datasets were tested against this system in two rounds (round 1: credit and financial-statement data, sections 1-5; round 2: HR and accounting data, sections 6-10). Each was profiled, checked for whether its labels
carry learnable signal, and then either used to build/validate a feature or rejected with a reason.
Everything below was produced by the code in this repo (`src/bin/dataset_eval.rs`,
`scripts/load_datasets.py`) or by the offline fits described under *Provenance*.

**Short version:** three datasets support real features (counterparty credit scoring, financial-health
scoring, and a pay benchmark whose implementation was validated on the salary data). Five have labels
that are statistically indistinguishable from noise or are derived from other columns, and were *not*
used to train anything. Two have data-quality problems that rule them out. Treat the shipped scores as
screening aids, not decisions.

## What each dataset was used for

| Dataset | Rows | Verdict | Used for |
|---|---|---|---|
| Enterprise Financial Network | 2,934 nodes, 9,000 edges | **Usable** | Counterparty credit score; scale test (2,934 customers) |
| Entrepreneurship Risk Assessment | 2,500 | **Partly usable** | Financial-health score (ledger-computable part) |
| Corporate Financial Risk Monitoring | 1,500 | **Labels are noise** | Scale test (1,500 invoices + 1,500 bills); data-sanity checks |
| Financial Statement Anomaly | 4,000 | **Labels are noise** | Nothing trained; documented why |
| Financial Statement analysis (xlsx) | 161 company-years | **Not usable as benchmark** | Nothing |

## 1. Counterparty credit score (Network dataset)

- Node data is clean: 2,934 unique ids, no missing edge endpoints, no self-loops, 1 duplicate edge,
  8 isolated nodes, attributes consistent between the node and combined files.
- Bankruptcy base rate 14.2%. Signal is modest but real. Single-feature AUC: debt ratio 0.666,
  credit score 0.619, liquidity 0.563, legal cases 0.517.
- **Model:** logistic regression on credit score, debt ratio, liquidity. 5-fold CV AUC **0.719**
  (5 seeds: 0.716-0.720). A gradient-boosted model scored 0.733 with `legal_cases`; not worth the
  opacity.
- **`legal_cases` was dropped.** Its fitted coefficient was negative (more lawsuits => lower risk),
  which is a noise artifact; removing it cost nothing (0.719 vs 0.718).
- **No contagion signal.** Correlation between a node's bankruptcy and the share of its neighbours
  that are bankrupt is -0.018; adding neighbour features moved AUC from 0.733 to 0.742. The data
  does not support network-propagation risk, so none was built. The edges were not imported.
- Bands (run through the shipped Rust code): low (p<0.10) 5.3% observed bankrupt, medium 16.1%,
  high (p>=0.20) 28.1%.

**Limits.** This is a ranking aid. The probability is tied to the dataset's 14% base rate and is not a
default probability for your customers. AUC 0.72 means roughly 1 in 4 pairs are ordered wrongly.
Inputs outside the fitted range (credit 500-849, debt 0.1-1.0, liquidity 0.5-2.0) are flagged
`extrapolated`.

## 2. Financial-health score (Entrepreneurship dataset)

- The dataset's `Risk_Level` is 94.2% "High" and `Decision_Priority` is almost constant, so they
  cannot be used as targets.
- Its sub-scores are formulaic. `Sustainability_Risk_Score` is exactly linear in the raw features
  (CV R^2 1.00); `Financial_Risk_Score` is exactly linear in 11 inputs:
  `90.38 + 0.22*debt% - 3.33*current_ratio - 0.275*margin% - 0.3125*runway + 0.12*funding_gap - 0.0212*credit_score`.
  `Overall_Risk_Score` is not (R^2 0.55), so it was not modelled.
- An ERP holds four of those inputs (debt ratio, current ratio, margin, runway). A fit on just those
  reproduces the dataset score with **R^2 0.89** (shipped Rust code: r = 0.944, MAE 1.96 points,
  96.7% within 5 points, same band 82.4%).
- Inputs are clamped to the benchmark range and the clamped names are returned.
- Bands use the benchmark's 25th/75th percentiles (64 / 74). The score is a position relative to that
  population of small ventures, not an absolute risk level.

**Limits.** Runway is defined here as cash / average monthly net loss over the window, capped at 48
months. The dataset does not document its own definition (its runway does not follow capital/burn),
so the fit transfers the weights, not the definition. The ledger has no current/non-current split, so
current ratio uses cash + receivables over payables + payroll liabilities. Funding gap and credit
score, which the dataset's formula also uses, are not available in an ERP.

## 3. Corporate Financial Risk Monitoring: labels are noise

- `Risk_Class` (0/1/2) and `Risk_Tag` (7 values) are unrelated to the features. Best logistic or
  boosted model: 48.8% / 45.2% accuracy against a 48.9% majority baseline. Best single-feature rule:
  49.3%. Feature correlations with class are all |r| < 0.04. `Risk_Tag` and `Risk_Class` do not even
  agree: "Critical" appears in class 0 as often as in class 2.
- The shipped health score shows no ordering across the classes either (mean 63.3 / 62.8 / 62.9).
- The rows are internally inconsistent: operating cost exceeds revenue in 38.9%, net profit exceeds
  revenue in 15.3%, and net profit exceeds revenue minus operating cost in 54.7%. Real statements
  cannot do that.
- **Not used for training.** Used for a volume test (below).

## 4. Financial Statement Anomaly: labels are noise

- Features are standardised (all within +/-1.07), so they cannot be mapped to ledger amounts.
- Classes: Normal 2,816, Anomaly 801, High Risk 383. Logistic 70.4% and boosted 69.5% against a
  70.4% majority baseline; best one-feature rule 70.4%. Class means differ by at most 0.08 on any
  feature.
- No anomaly detector was built from it. Statement-level anomaly detection would need data in which
  the label is actually related to the features.

## 5. Financial Statement analysis (xlsx): not usable as a benchmark

- 12 large listed companies (AAPL, AIG, AMZN, BCS, GOOG, INTC, MCD, MSFT, NVDA, PCG, PYPL, SHLDQ), 161
  company-years.
- "Price to earning" has median 0.02 and 25 negative values; as a P/E it is impossible, so it is
  mis-scaled or an earnings yield. 14 gross-margin values are exactly 1.0 or 0.1 (placeholders or
  clipping; BCS is 1.0 throughout), and 4 rows have operating margin above gross margin.
- Large listed companies are also the wrong peer group for an SME ledger. Not integrated.

## Round 2: HR and accounting datasets (sections 6-10)

Summary of verdicts:

| Dataset | Rows | Verdict | Used for |
|---|---|---|---|
| Employee Compensation / Salary Prediction | 1,000 | **Usable, but the structure is dataset-specific** | Validating the pay-benchmark implementation |
| Employee Performance Metrics | 10,000 | **Rating is derived from score; encoded columns** | Nothing trained |
| Enterprise HR Performance | 1,200 (100 employees x 12 months) | **Rejected: constant label, drifting attributes, no signal** | Nothing |
| Employee Performance Classification (xlsx) | 130,000 | **Labels are noise** | Nothing |
| Accounting Data for Financial Management | 1,000 | **Rejected: identities fail, future dates, noise labels** | Nothing |

(Section numbers below continue from round 1.)

## 6. Pay benchmark (Salary dataset): usable, dataset-specific

- 1,000 employees; age and experience correlate at 0.993, so age is redundant (and it is a protected
  characteristic; it is not a model input).
- Pay is highly learnable here: raw-salary linear CV R^2 0.985; log-linear with performance and skills
  0.968. Experience +2.1%/year, city tier -5.6% per step, PhD +9.8% vs high school, remote +2.4%;
  job role barely matters (within about 1.5%).
- **The structure does not transfer.** Run through the same code, log-salary R^2 from experience,
  education, department and role is 0.127 in Employee Performance Metrics (0.691 only once
  Performance_Score is added, because pay there tracks performance) and -0.001 in the 130k file.
  Fixed pay coefficients shipped in the product would therefore be one synthetic dataset's quirks
  presented as a market rate, so none are shipped.
- **What was built instead:** `GET /api/hr/compensation/benchmark` fits log(base_salary) on the
  company's *own* employees (experience, education, location tier, remote, job title), reports
  expected pay, compa-ratio and statistical outliers, and the correlation between unexplained pay and
  the latest review rating. Needs at least 50 employees with complete attributes.
- **Implementation check against scikit-learn** (identical terms, identical folds): in-sample R^2
  0.9337, adjusted R^2 0.9330, residual SD 0.0687, 5-fold CV R^2 0.9324, MAPE 5.52%, and all eleven
  effect sizes agree to the printed precision. Through the API on the loaded dataset the benchmark
  returned R^2 0.934, typical error 7.1%, the same effects.
- Cost of the design: excluding performance and skills lowers R^2 from 0.969 to 0.934 on this data.
  That is deliberate, so pay-versus-performance can be examined rather than assumed (on 600 reviewed
  employees the correlation between unexplained pay and rating was 0.549).
- Minimum sample (shipped terms, fit on the first n rows, tested on the rest): n=30 -> held-out R^2
  0.898, 50 -> 0.905, 100 -> 0.922, 200 -> 0.933.
- **Robustness bug found by testing and fixed.** Planting two outliers (+50% and -40% pay) made the
  count of flagged employees *fall* (48 to 39): the outliers inflated the residual SD and masked
  others. The model is now refitted without points beyond 3 SD before anyone is judged. With the
  outliers planted, flagged counts are stable (57 vs 58 on the restored data) and the planted
  employees score z = +5.96 and -10.12.

**Limits.** The benchmark reflects your own pay practice, not the market, and it cannot tell
discrimination from legitimate causes. Flags are prompts for review. Years of experience is stored
as of the hire date and advanced by whole years of service.

## 7. Employee Performance Metrics: rating is derived, no prediction built

- `Performance_Rating` is exactly a banding of `Performance_Score` (>=85 Excellent, 70-85 Good, 55-70
  Average, <55 Poor) in 10,000 of 10,000 rows, so 'predicting the rating' (94% accuracy) is just
  predicting a banded score. 68.8% of employees are rated Excellent.
- Every categorical column is a label-encoded integer with no decode map (even `Name`).
- Performance score correlates with job satisfaction (0.87), salary (0.76) and last-year promotion
  (0.76), and a linear model explains 87% of it. Those fields plausibly *follow from* performance
  rather than cause it, so a predictor built on them would be circular. No performance predictor was
  built.

## 8. Enterprise HR Performance: rejected

- `performance_class` is 'Low' for all 1,200 rows even though the score spans 0.0-30.9.
- Every one of the 100 employees changes department, role, education and salary band between months,
  so the file is not a coherent employee history.
- `performance_score` is not predictable from any field: grouped cross-validated R^2 -0.009 (linear)
  and -0.069 (boosted); in-sample linear R^2 is only 0.022.

## 9. Employee Performance Classification (xlsx, 130,000 rows): labels are noise

- Clean (no nulls, no duplicates, sensible ranges) but the three balanced classes are unpredictable:
  33.3% accuracy (logistic) and 33.4% (boosted) against a 33.8% majority baseline on a 20,000-row
  sample. Income is unrelated to tenure (r = 0.006).

## 10. Accounting Data for Financial Management: rejected

- Accounting identities fail in every row: net income equals revenue minus expenditure in 0% of 1,000
  rows; net income exceeds revenue in 40.1% and gross profit exceeds revenue in 38.1%.
- Dates run one per day from 2025-01-01 to 2027-09-27, i.e. into the future.
- `Transaction Outcome` is 95.1% one class and no single-field rule beats that; `Account Type` is
  unpredictable (accuracy 22-26% against a 25.7% majority baseline).

## System test: volume and controls

`scripts/load_datasets.py` pushed the network and corporate datasets through the live API on
PostgreSQL 16 (debug build, one machine):

- 2,934 customers + 2,934 risk profiles; 1,500 invoices from `Revenue`; 750 receipts; 1,500 bills
  from `Operating_Cost`. Load time about 28 s with 8 concurrent clients, no server errors.
- All 25 invoices attempted to bankrupt-flagged customers were rejected (HTTP 409).
- Report latency at that volume: trial balance 12 ms, balance sheet 10 ms, AR aging 72 ms, cash
  forecast 51 ms, receivables exposure 21 ms, integrity checks 22 ms.
- Integrity checks at about 136 billion of postings: ledger balanced; AR control account equals open
  invoices; AP control equals open bills; bank GL equals cash transactions; all to the cent.
- The checks do catch problems: a manual 10.00 posting to the AR account made `receivables_control`
  fail with the exact difference. That led to a new guard: manual entries to the AR/AP control
  accounts and to bank GL accounts are now rejected up front.

The data is synthetic and the amounts are huge, so this validates volume, arithmetic and
reconciliation, not business realism.

## What changed in the system as a result

- `GET/PUT /api/risk/counterparties/{customer|vendor}/{id}`, `GET /api/risk/counterparties`
- `GET /api/risk/receivables-exposure` (open AR by risk band, risk-weighted exposure, unassessed AR)
- `GET /api/risk/financial-health?as_of=&months=` (ratios, score, rule-of-thumb flags)
- `GET /api/risk/integrity-checks` (7 checks)
- New invoices to a customer flagged bankrupt are rejected.
- Manual entries and cash offsets can no longer touch the AR/AP control accounts (bank GL accounts
  were already protected).
- `dataset_eval` binary, `scripts/smoke_test.sh`, `scripts/load_datasets.py`.

Round 2 added:

- `GET /api/hr/compensation/benchmark` and four optional employee attributes (`education_level`,
  `years_experience`, `city_tier`, `remote_work`) on create/update/hire (migration 003).
- Three HR integrity controls: `payroll_to_ledger` (posted payroll gross equals ledger salary expense),
  `terminated_benefits`, `pay_benchmark_data` (attribute completeness).
- `dataset_eval` sections 5-7 (pay model validation, performance datasets, accounting data) and
  `scripts/load_hr_dataset.py`.
- Load test: 1,000 employees loaded in 2.2 s, 600 reviews; finance and HR loads coexist on one
  database with all reconciliations passing and no server errors.

## Provenance and how to reproduce

Coefficients live in `src/risk/model.rs` and were fitted offline (scikit-learn, logistic regression
with standardised inputs, converted to raw scale; ordinary least squares for the health score).
To re-check against the data: `cargo run --bin dataset_eval -- <dir with the unzipped datasets>`.
To refit, rerun the same regressions on the same columns and replace the constants.

## Not done, and why

- Network edges (supplier, parent, co-investor, shared director) not imported: no predictive value
  found, and no ERP table needs them yet. They would suit a concentration or related-party exposure
  report, which is a different feature.
- No statement anomaly detector: see section 4.
- Round 2 produced no performance predictor, accounting-anomaly detector or payroll-forecast model:
  no dataset had labels that carry usable signal (sections 7-10).
- No out-of-sample test on real data. Before relying on the credit or health scores, compare them with
  your own outcomes; the pay benchmark refits on your data but still needs human judgement.
