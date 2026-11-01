# Dataset analysis and what it changed

Seven datasets were profiled with pandas (and scikit-learn for the risk model). Every number below was
computed from the supplied files. The importer `scripts/import_aerospace.py` loads the aerospace data into
this system; the new reports then reproduce the pandas figures exactly (checked: on-time 44.2%, in-full 88.7%,
per-supplier on-time range 3.6%-61.2%, forecast WAPE/bias for both forecast types, weekly demand mean/sd).

## Findings

| Dataset | Key findings |
|---|---|
| **Aerospace** (29,666 POs, 300 parts, 6 sites, 280,800 weekly rows, 368 incidents) | Only **44.2% of POs arrive on time** and 88.7% in full (OTIF 39.7%); late POs average +1.2 days. Short deliveries ship a median 75% of the order (range 50-96%) and the balance is never delivered. Supplier risk class drives lateness: High 3.6% on-time (lead time 97 d), Medium 31% (52 d), Low 57% (33 d); part criticality has no effect. Per-supplier on-time ranges 3.6%-61.2%; incident rate correlates -0.58 with on-time. Incidents: 66% minor, 25% major, 9% critical. Weekly demand is intermittent (median CV 0.97, p90 1.76) with +/-20% seasonality. **Baseline forecast WAPE is 64.5%; "adjusted" forecasts add +12% bias and are no more accurate (65.8%)**, and planned-maintenance weeks actually consumed *less* (1.94 vs 2.45 units). Quarantined (blocked) stock appears in 0.84% of rows; shelf life exists for 8.7% of parts. |
| **High-dimensional supply chain** (50 SKUs x 5 warehouses, 365 days) | Zero stock-outs but heavy overstock: static reorder points average **2.7x lead-time demand** (implied z of about 7.3) and inventory covers about 23 days. No order is ever recorded on a day when inventory is at or below the reorder point (0% vs 5.8% above it), so the stored ROP is not what drives replenishment. Monthly demand index swings 0.51-1.49 while ROP is constant. Lead time averages 8 days with sd 3.8 (CV 0.47). Promotions lift units by 28%. Forecast WAPE 11.9%, unbiased. |
| **Defence logistics** (5,000 steps, 9 episodes, Tier 3 only) | Stock-outs in 3.7% of steps, all at zero inventory, each costing about $69k against $2.50 holding per pallet-step. Lead time mean 27.7 h, sd 17.4 h (CV 0.63); demand mean 21.7, sd 25.7 (CV 1.18). Penalty-to-holding ratio implies a service level above 99.9%. |
| **Pharma optimisation** (100,000 rows) | `Optimal_Stock_Level` is **uncorrelated with demand (r = -0.002)** and the restocking strategy (weekly/monthly/quarterly) has no effect on it. This is noise; it was *not* used to train or tune anything. Only the weekly/monthly/quarterly cadence idea was kept (as `review_period_days`). |
| **Pharma value chain, Ghana** (49 activities) | Distribution costs GHS 1.21M, including cold-chain management (0.2M), logistics, warehousing and freight. Revenue appears only in the sales stage. Motivates cold-chain handling. |
| **Procurement** (5,200 POs, 15 suppliers, 5 currencies) | 64.1% on time. **The supplier risk label does not predict lateness** (63.1% / 64.3% / 64.1% on-time for High / Medium / Low). Invoice matching matters: 3-way matched invoices are disputed 11.8% of the time and paid 50.3%; unmatched invoices are disputed 21.5% and paid 11.5%. 16.9% of POs cost more than budget; 5.4% of contract POs (182 of 3,375) were placed outside the contract window; 12.5% of POs are flagged maverick (10.8% of spend); 10.2% are emergency orders. Terms are Net 30/45/60. |
| **Supplier risk** (28,098 rows, 24,112 unique suppliers) | 3,986 duplicate supplier IDs (3,447 fully identical rows) and 230-404 missing values per column. 70% carry `Risk_Level = 1` (treated here as the high-risk flag). A logistic model on five inputs reaches **91.6% CV accuracy, AUC 0.972**. On-time rate and defect rate carry almost no weight in the label (correlations -0.06 and 0.01), so the label measures *structural* exposure, not delivery behaviour. `Supplier_Dependency_Score` is exactly 1/(alternatives+1). |

## What changed in the system (v0.2.0)

| Finding | Feature |
|---|---|
| Late and short deliveries are the dominant problem; suppliers differ 17x | **Goods receipts with dates**, per-line promised dates, back-dated `order_date`/`received_on`, `close` (short-ship write-off, status `closed_short`), and **supplier scorecards** (on-time, in-full, fill rate, days late, lead-time mean/sd, quality, performance score) |
| Risk label is structural and distinct from delivery performance | **Structural risk model** fitted on the risk dataset, reported *next to* observed performance rather than merged. Suppliers with 3+ missing inputs get band `unknown` instead of a misleading score |
| Quality incidents and quarantined stock | **Quality incidents** with quarantine (`blocked` stock excluded from availability), release and scrap, feeding the supplier quality score |
| Lead-time variability is large (CV 0.23-0.63) and demand is intermittent | **Replenishment report**: safety stock `z*sqrt((L+R)*sd_d^2 + mu_d^2*sd_L^2)`, lead-time statistics taken from receipts, weekly demand buckets, trend flag against seasonality, suggested order quantity, and raise/lower advice against the stored reorder point |
| Defence stock-out penalties vs holding cost; aerospace criticality | **Service level** from the critical ratio when a stock-out cost is set, otherwise A 99% / B 97% / C 95% by `criticality_class` |
| Static ROPs cause overstock | The replenishment report compares current vs recommended ROP (`set` / `raise` / `lower` / `ok`) |
| Value concentration and variability | **ABC/XYZ classification** with policy guidance |
| Forecast quality is poor and "adjustments" add bias | **Forecast store and accuracy report** (WAPE, bias) split by forecast type |
| Cold chain, shelf life | `storage_class` on products and bins (enforced on put-away, adjustment and transfer), `shelf_life_days`, lot expiry, **first-expiry-first-out allocation** that skips expired stock |
| Contract and invoice control | **Contracts** (orders outside the window are rejected), PO type/currency/payment terms/budget price, **automatic 3-way invoice match** (2% price tolerance, quantity vs received), approve/pay workflow, and a **procurement-controls report** (no-contract spend, emergency share, budget variance, dispute rate, overdue) |

## Not done, and why

* The pharma optimisation set was not used for modelling: it contains no signal.
* The defence set contains one tier only, so multi-echelon (tier-to-tier) replenishment is not implemented.
* The high-dimensional set's reorder behaviour is inconsistent with its own ROP column, so it was used for
  demand-variability and overstock evidence, not as a policy to imitate.
* Multi-currency amounts are stored per PO with a currency code but never converted.
* The risk model's thresholds (0.30 / 0.70) follow the dataset's 70% base rate; recalibrate on your own outcomes.

---

# v0.3: lessons from the two uploaded systems

## What the systems are

* **`pde_shpinn`** is a Rust workspace (`shpinn`, `spectral_hypergraph`, `nbsc`, `krylov_ds`): physics-informed
  neural networks with spectral encodings, the normalised hypergraph Laplacian of Zhou et al., a heat-equation
  residual `du/dt + alpha * Delta u = 0`, Krylov methods and non-backtracking spectra.
* **`human_connectome_rnn` v0.4** is a connectome-constrained RNN/LSTM trained with exact BPTT. Its README is
  mostly an *ablation record*: every addition is tested over many seeds against null wirings (degree-preserving
  rewiring, density-matched random, dense).

## What was reused, what was not

| From | Used in the supply chain | Not used, and why |
|---|---|---|
| Zhou hypergraph Laplacian (`spectral_hypergraph`) | Suppliers are vertices, **products are hyperedges**; lambda_2, connected components and the Fiedler bisection measure how interlinked the supplier base is | Non-backtracking spectra: for a supplier network the extra descriptor adds nothing the components/lambda_2/HHI do not already say |
| Heat equation (SHPINN) | `exp(-t Delta)` diffuses a shock from failed suppliers to rank which survivors are structurally closest. Computed in closed form from the eigendecomposition, so no training is involved | The PINN itself: there is no PDE to learn here and a trained network would be less exact than the analytic solution |
| Connectome null-wiring ablations | `/resilience/spectral` reports the network's lambda_2 against **degree-preserving random sourcing** (z-score and a plain-language reading); forecasting methods are only ever judged out of sample with parameters frozen before the test window | The LSTM/RNN: its own README finds the LSTM helps only when memory is the bottleneck (delays of 50-90 steps), is no better on the default task, depends on forget-gate initialisation and has 4x the parameters (capacity confound not excluded). Weekly intermittent demand has no long-memory problem, so a neural forecaster was not built |

## New findings from running the loop on the datasets

**Forecasting (aerospace, 1,800 part-site series, 26 out-of-sample weeks, identical windows):**

| Method | WAPE | Bias | RMSE |
|---|---|---|---|
| dataset's own stored forecast | **0.683** | +0.043 | **2.045** |
| auto (best of 5 per series by backtest RMSE) | 0.758 | +0.080 | 2.175 |
| SBA | 0.768 | +0.081 | 2.198 |
| SES+TSB / TSB / SES | 0.777-0.779 | +0.12 | 2.21 |
| 13-week average | 0.780 | +0.074 | 2.249 |

The built-in statistical methods **do not beat the forecasts supplied with the dataset** (about 6% worse in
RMSE). Auto selection is the best of the flat methods, 3% better than the 13-week average. A seasonal index
was tested and rejected: it cut RMSE by only about 1% (2.217 to 2.193). The supplied forecasts carry
information that weekly history alone does not reproduce (correlation with actuals 0.64 vs 0.57). Consequences:
the engine is the fallback when no better forecast exists, `forecast-accuracy` now compares
`statistical` / `baseline` / `adjusted` so you can see which source deserves trust, and replenishment uses the
engine's backtest RMSE (not raw demand variance) for safety stock.

**Resilience (two real extremes, cross-checked with pandas):**

| | Aerospace | Procurement |
|---|---|---|
| Sole-sourced products | **300 of 300** | 0 of 51 |
| Supplier islands (components) | 40, lambda_2 = 0, risk **high** | 1, lambda_2 = 1.0, risk **low** |
| Mean product HHI | 1.00 | 0.0778 (pandas 0.0778) |
| Supplier spend HHI / effective suppliers | 0.031 / 32.2 | 0.0703 / 14.23 (pandas 0.0703 / 14.23) |
| Remove the supplier with most parts | SUP012: 14 parts left with **no source**, 4.2% of 90-day demand value ($2.7M of $65M) short, 120-day recovery | Remove the two largest: 19.9% of volume lost per item, remaining suppliers need +24.9%, no shortfall |

Two data-quality findings: the procurement file's `Single Source Flag` marks 11.9% of orders as single-source
although every one of its 51 items was bought from all 15 suppliers, so sourcing must be computed from what was
actually bought, not from that flag; and the aerospace file's spend looks diversified (supplier HHI 0.031) while
every part is single-sourced, so supplier-level spend concentration alone hides the exposure.

## What changed (v0.3.0)

| Diagram stage | Implementation |
|---|---|
| Demand forecasting engine | `ma13`, `ses`, `sba`, `tsb`, `ses_tsb`, `auto`; demand classification (smooth/erratic/intermittent/lumpy); `GET /forecast/demand`, `POST /forecasts/generate` (stored as `statistical`) |
| Forecast accuracy | WAPE, bias and RMSE per method in the backtest, and per forecast type in `/reports/forecast-accuracy` |
| Replenishment optimisation | Reads the forecast engine by default (`demand_source=forecast`), safety stock from forecast error, lead time, review period and stock-out cost |
| Procurement process, contract validation, PO creation | `POST /replenishment/run`: picks a source (preferred > best scorecard > shortest lead time > usual supplier), requires a contract in force, raises one draft or submitted PO per supplier, skips products already on a draft, supports `dry_run` |
| Supplier network | `product_suppliers` (approved sources), scorecards joined with HHI, effective sources and sole-source exposure in `/resilience/concentration` |
| Receive PO, quality inspection | Receipt lines accept `rejected_qty`: failed units are quarantined immediately and a quality incident is opened against the supplier |
| Quarantine outcomes | Release, scrap or **rework** (tracked separately) |
| Pick, pack, stage, ship | `POST /sales-orders/:id/fulfil` enforces picked, packed, staged; `ship` can require staging |
| Hypergraph resilience | `/resilience/spectral` (lambda_2, components, Fiedler clusters, fragmentation risk, null-model comparison) and `/resilience/disruption` |
| Disruption simulation | Remove suppliers; lost share, absorbable vs uncovered volume, required uplift, stock-out timing, units and value short, recovery time, before/after connectivity, nearest suppliers by heat-kernel proximity |
| Closed loop | `POST /planning/cycle`: forecasts, replenishment, purchase orders and a resilience snapshot in one call |

## Limits

* Resilience assumes an alternative supplier can absorb `max_uplift` (default +50%) of its baseline volume and
  a qualified-but-unused one `standby_share` (default 25%) of demand. These are parameters, not measured capacities.
* The aerospace import has no stock on hand, so stock-out timing in the aerospace disruption run is zero days.
* The fragmentation thresholds (lambda_2 below 0.10 high, below 0.30 medium) are heuristics on the Zhou scale
  (eigenvalues in [0, 1]); the null-model comparison is the better guide when partial overlap exists.
* The null model is skipped when every product is single-sourced or the incidence is already complete, which
  is the case for both real datasets here; it is covered by a unit test on a bottlenecked network instead.
* Forecasts are flat (no trend or seasonality); weekly buckets of complete weeks only.

---

# v0.4: nine more datasets

Datasets: ERP simulation (42k sales, 3.2k PO lines, 12 suppliers, 306 source links), Cacau Prime Foods
(193k production runs, 100k deliveries, 50k purchases), Global Supply Chain Risk (5k shipments), Analyzing Supply
Chain Risk (113k rows), Supply Chain Operation & Delivery Performance (100k shipments x 106 columns), SCMS
Delivery History (10k lines), Supply Chain Logistics (shipments, customers, carriers), a 100-SKU quality/cost file
and a 1,000-company SCM metrics file.

## Findings

| Dataset | What the data show |
|---|---|
| **ERP simulation** | Master data understates reality: actual lead times ran **+2.5 days** over the route means (9 of 12 suppliers more than 20% slower than planned), and the spread was about 3x the stated one (air: planned sd 1.0, actual 4.0). Lead-time CV by mode: sea 0.20, rail 0.34, road 0.48, air 0.75. Orders sit at the supplier **MOQ 86.5%** of the time; cover is median 17 days but runs to 183 days (7 of 120 products at 90+ days, all MOQ-driven). 54 products have two sources and 66 three (every product is multi-sourced), and most orders go to the primary. Events add only about +0.8 days; weather had no extreme days; demand is smooth (weekly CV 0.23) with a November/December peak that a single year cannot validate. |
| **Cacau Prime** | Supplier master lead times of 4-20 days all hide an observed **9.8 days (sd 5.8)**: master data is uncorrelated with reality (supplier ratings likewise). Every product ran **13.8% above standard cost** (range 13.6-13.9%) with a flat 1.10% scrap rate, unrelated to batch size. All five carriers are statistically identical (80.6-81.3% on time, ~$1.14k per delivery); lateness does not depend on distance. 3.6% of production rows (7,102 of 200,000) have zero quantity. Stock snapshots are inconsistent (next-day opening != prior closing in 99.7% of rows; identical rows copied across branches). |
| **Operation & Delivery Performance** (100k) | A held-out logistic model predicts delay with **AUC 0.904** and is well calibrated by quintile (predicted 0.7/3.7/12.7/35.1/75.6% vs observed 0.4/2.9/12.2/35.6/74.6%). Drivers: traffic index (largest), supplier reliability, warehouse utilisation, carrier reliability, weather (storm, snow, heavy rain). Transport mode, order priority, distance, customs flags and documentation errors carry **no signal**. |
| **Global Supply Chain Risk** (5k) | Disruption model **AUC 0.835**: hurricanes disrupted 100% of shipments, storms 80%, fog 48%, rain 42%, clear 37%; geopolitical score moves 48% to 74%; transport mode, distance, fuel price and weight do nothing. Disruption inflates lead time 2.4-3.0x by mode. |
| **Analyzing Supply Chain Risk** (113k) | **No usable signal.** `risk_classification` is a plain threshold of `disruption_likelihood_score`, while `delay_probability` and `delivery_time_deviation` are independent of every feature and of the risk class (class means 0.699/0.709/0.697 and 5.18/5.10/5.18). Nothing was built from it. |
| **SCMS** | Quantity is in **packs** and value = quantity x Pack Price (98.8%); the file's Unit Price is per single unit. Freight is booked once per delivery note (24% of lines only say "See DN-...", 14% are "included in commodity cost"). Freight + insurance is a **median 10.3% of goods value** (air 10.9%, charter 14.8%, road 5.8%, ocean 3.3%; value-weighted 6.9%). In the whole file only 61% of deliveries land on the scheduled day, 11.5% are late and 27% early (21% more than a week early), so "on time = not late" hides early arrivals. |
| **Supply Chain Logistics** | Delay hours rise with warehouse utilisation (r = 0.89) and so do damage claims (r = 0.80): the congested carrier ran at 92% utilisation with 357 claims against 60% and 30. 3.3% of customs clearance times are the words "On-Time" or "Delayed" instead of numbers. |
| **100-SKU file** | Inspection results do not follow measured defect rates (Pass lots range 0.13-4.84% defective with mean 2.04%, Fail lots 0.05-4.94% with mean 2.57%), so a rule against an acceptable quality level is more defensible than a stored label. |
| **Company SCM metrics** | Cross-sectional structure exists (lead time vs fulfilment r = -0.78, variability r = -0.70, lead time vs risk 0.85) but with gross outliers (167 rows with over 5,000 suppliers, 14 with turnover above 20, one negative implementation cost). Only the IQR bands are used, as descriptive references. |

## What changed (v0.4.0)

| Finding | Feature |
|---|---|
| Master lead times are wrong, with far too little spread | `GET /reports/lead-time-accuracy` (planned vs observed per supplier and per transport mode). Planning now uses a **hierarchy**: product receipts (5+), else the product's suppliers' pooled receipts, else master data with a CV of **0.5** (was 0.3; observed supplier CVs run 0.23-0.81, median about 0.55). On the ERP data this matters: trusting planned lead times with the old CV would have understated reorder points by **32%**, and by 20%+ for 87% of products |
| MOQ-driven overstock | `min_order_qty` and `order_multiple` on approved sources; the replenishment run rounds up and reports `moq_overbuy`; `GET /reports/excess-cover` values stock above the order-up-to level, its annual holding charge and whether the MOQ explains it |
| Early deliveries are hidden | Scorecards add `early_rate`, `avg_days_early` and `on_window_rate` (configurable `early_tolerance_days`) |
| Predictable delay and disruption | `POST /risk/shipment` runs both fitted models; `ship` accepts `expected_delivery_date`, `freight_cost` and a `risk` context (carrier reliability and warehouse utilisation are filled in from our own data) and stores the predicted risks |
| Carriers need measuring | `GET /reports/carrier-scorecards`: on-time, early, observed late rate, days late, transit days, freight, and the model's predicted late rate beside the observed one |
| Freight booked per shipment | PO `incoterm`, `transport_mode`, `freight_cost`, `insurance_cost`, `freight_included`; `GET /purchase-orders/:id/landed-cost` spreads freight and insurance over lines by value |
| Congestion drives delay and damage | `GET /reports/warehouse-utilization` from bin capacities (also feeds the delay model) |
| Snapshots that do not reconcile | `GET /reports/stock-reconciliation` compares stock levels with the movement ledger and reservations with open allocations |
| Inspection labels are unreliable | `aql_defect_pct` on products and `defect_rate_pct` on receipt lines: a lot above the AQL is quarantined whole and an incident is opened (`AQL exceeded`) |
| Production cost and scrap | `POST /production-runs` (good output enters stock, scrap recorded, ledger `production`) and `GET /reports/production-yield` (scrap rate, units per hour, actual vs standard unit cost) |
| Company benchmarks | `GET /reports/kpis`: inventory turnover, fill rate, supplier lead time mean/sd, on-time and in-full, against reference quartiles |

## Validation (each cross-checked with independent pandas calculations)

| Check | Result |
|---|---|
| Cacau production yield | scrap 1.096%, cost variance +13.78%: identical to pandas |
| Cacau carrier scorecards | on-time, early, days late and freight identical for all five carriers |
| Cacau lead-time accuracy | observed means and sds identical for all eight suppliers |
| ERP lead-time accuracy | receipt-weighted bias +2.544 days and every mode's mean and sd identical to pandas |
| ERP excess cover | 7 products at 90+ days (pandas 7 of 120); replenishment orders all respect the MOQ (17% of ordered units are MOQ over-buy) |
| SCMS timing | on-time 94.45%, early >3 d 4.33%, in-window 90.11% on the 4,592 imported lines: identical |
| SCMS landed cost | median 10.3%, p25 5.0%, p75 22.2%, value-weighted 6.9%, per mode identical |
| Delay model | held-out AUC 0.904; Rust coefficients reproduce the scikit-learn fit to 4 decimals on real rows |
| Ledger reconciliation | `balanced: true` after every earlier workflow (receipts, transfers, scrap, shipping, production) |

## Limits and caveats

* The delay model covers domestic/operational drivers and the disruption model international ones; each was fitted on a
  single synthetic-looking dataset, so check the calibration against your own carriers with the carrier scorecard.
* The ERP, Cacau and Global-risk files look synthetic, so effect sizes may not transfer; the structural lessons
  (master data understates variability, MOQ drives cover, freight is shipment-level) are the transferable part.
* Inventory turnover in `/reports/kpis` divides issue cost by the current inventory value (no snapshots are kept).
* Seasonality was not modelled: the ERP data hold one year only, so a seasonal index cannot be tested out of sample.
* The mode/route cost-versus-speed decision (air 5 days at about $2,300 vs sea 31 days at about $300 in the ERP data)
  was left out: a route table plus a stock-out-cost trade-off would be needed and the data cannot calibrate it.
* Importers are provided for ERP, Cacau and SCMS; the 100k-row delay and risk files were used only to fit the models.
