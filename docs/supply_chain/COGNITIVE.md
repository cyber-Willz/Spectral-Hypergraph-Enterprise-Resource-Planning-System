# v0.5: the cognitive layer (CIR-GNN)

The platform now runs the architecture in the design diagram on top of the v0.4 transactional core. Nothing in
v0.4 was changed or removed: the new layer reads the same tables, adds its own (`migration v5`) and its own routes
under `/api/v1/cognitive`. The connectome crates are vendored in `crates/` and the workspace builds as one unit.

## Diagram layer -> implementation

| Layer | Where | Endpoint |
|---|---|---|
| Data ingestion (ERP, WMS, procurement, logistics, quality, production) | `cognitive/data.rs`: flows + weekly node tensor from existing tables; external signals (weather, markets, IoT, disruption) in `cognitive_signals` | `POST/GET /cognitive/signals` |
| Supply-chain graph | `cognitive/graph.rs`: suppliers, products, warehouses, customers, carriers; supplier->product/warehouse, product->customer/warehouse/product, warehouse->warehouse/customer/carrier, carrier->customer | `GET /cognitive/graph` |
| Hypergraph engine | PO, shipment, contract and production hyperedges; feeds the existing hypergraph-Laplacian maths | `/cognitive/graph`, `/cognitive/resilience` |
| Connectome-informed graph encoder | `analytics.rs` + `insights.rs`: four flow layers (material, financial, traffic, dependency) -> `hcrnn::Connectome`; PageRank, Brandes betweenness, low-frequency connectome-harmonic energy -> node criticality; non-backtracking radius and expansion ratio (nbsc); bottlenecks; most probable risk-propagation paths | `GET /cognitive/encoder` |
| Recurrent memory network | `cirgnn.rs`: every node = one excitatory + one inhibitory unit, state replayed from history and stored per node; statistical memory findings (lead-time drift, recurring delays, seasonality, quality degradation, congestion) | `GET /cognitive/memory` |
| Temporal graph learning | week-to-week dynamics inside the CIR-GNN (delayed edges = lead times); trends, oscillation, variability, supplier evolution (last 13 vs prior 13 weeks) | `/cognitive/memory` |
| Demand / risk / inventory forecasts | multi-horizon (1, 2, 4 weeks) heads per node and channel (volume, delay, quality, pressure); P(late); stock-out probability in lead time; excess; reorder timing | `GET /cognitive/forecast`, `POST /cognitive/optimize` |
| Decision optimisation | `optimize.rs`: risk-aware supplier allocation with a concentration cap, MOQ/multiple rules, safety stock and policy from forecast uncertainty, warehouse balancing, carrier ranking, production priority | `POST /cognitive/optimize` |
| Autonomous execution | `ops.rs`: actions `create_po`, `transfer_stock`, `adjust_reorder_policy`; each kind has an autonomy level and guardrails | `/cognitive/actions`, `/cognitive/policies` |
| Digital twin | free-run of the trained network with shocks injected: supplier failure, port congestion, demand surge, warehouse saturation, lead-time increase; plus the existing mechanistic disruption model | `POST /cognitive/twin/simulate` |
| Resilience analytics | hypergraph spectrum, Fiedler/connectivity, criticality, HHI, sole-source exposure, recovery time and financial impact of losing the most critical suppliers | `GET /cognitive/resilience` |
| Continuous learning loop | every forecast is stored; `reconcile` scores it against what happened (forecast WAPE vs naive, lead-time error, Brier risk score, defect error, price-vs-standard error); `learning/step` retrains (warm start) on drift, staleness or network change, and proposes policy updates | `POST /cognitive/learning/*`, `POST /cognitive/cycle` |

## How the CIR-GNN uses the connectome code

`hcrnn::Connectome` receives the supply network: flow strength is the connection weight and lead time in weeks is the
"fibre length", so conduction delay equals transit lead time. `hcrnn::ConnectomeRnn` builds the wiring, the Dale's-law
excitatory/inhibitory structure, the delays and the Arnoldi (`krylov_ds`) calibration of the delayed recurrence
(initial spectral radius 0.9). The recurrent core in `cognitive/cirgnn.rs` then adds what a supply chain needs and the
brain task did not: node-wise input injection with weights shared per node type, node-wise multi-horizon residual
readouts (each head predicts the change from the node's current value), and exact BPTT (finite-difference check in the
tests). The heavy lifting for topology (`spectral_hypergraph`, `nbsc`) is used by the encoder.

## Autonomy and safety

Defaults are conservative: every policy starts **advisory** (proposals are logged, nothing can execute).
`supervised` means a person approves each action; `autonomous` executes proposals that pass the guardrails.
Guardrails: maximum order value, maximum supplier risk, maximum share per supplier, maximum transfer units, maximum
reorder-point change. Purchase orders are created as drafts unless the policy sets `submit`. An action runs at most once
and every action, rationale and result is in `cognitive_actions`. A new plan supersedes proposals nobody acted on.

## What the evidence says (synthetic data, so read as a check of the machinery, not of business value)

`scripts/seed_cognitive_demo.sql` builds 90 weeks with planted effects. On it, with a 12-week holdout the model never
trains on:

* The statistical layer found the planted effects: S3 lead-time drift, S2 rising defect rate, carrier K2 growing delays.
* **Trusted channels are decided by evidence, not assumed.** A channel is used only if it beats both "same as last week" and
  the node's own mean on the holdout. In the reference run, volume (skill +28% vs persistence, +38% vs mean) and pressure
  were trusted; delay and quality were **not** (no skill over persistence), so the optimiser uses observed recent behaviour
  for them. Results vary by seed and run; `/cognitive/train` reports the numbers every time.
* The planted data has no cross-node propagation, and the twin shows it: a shock mostly stays on the node it hits (impact
  score 88 on the failed supplier vs under 0.4 elsewhere). On real data with real coupling the learned propagation may be
  larger; `learned_propagation` flags channels whose head is not trusted, and the mechanistic block is returned alongside.
* Learning-loop backtest (model trained as of 4 weeks ago, scored on the weeks that then happened, 84 volume predictions):
  WAPE 0.168 vs 0.192 for the naive forecast; Brier score for P(late) 0.085 vs 0.207 for the base rate (75 predictions).
  Small samples on synthetic data.
* The existing `smoke_test_v2.py` and `smoke_test_v4.py` fail in this environment on the **unchanged v0.4 build too**
  (they depend on the calendar date / time of day); v1 and v3 pass on both.

## Limits to know about

* The connectome constructor symmetrises edges, so the network's recurrence is undirected; direction of material flow is
  kept in the flow layers and risk paths but not in the learned wiring.
* The neural graph is capped at 256 nodes (strongest by activity); `/cognitive/graph` reports truncation.
* Demand is modelled per product network-wide; per-warehouse split uses the last 91 days of issues.
* Delay and quality heads need many observed weeks per node; sparse nodes stay untrusted.
* Training runs on CPU in the request (seconds for tens of nodes); for hundreds of nodes run `/cognitive/train` out of band.
* The vendored `human_connectome_rnn` crate is unchanged (31 unit tests + integration test pass).

## Quick start

```bash
cargo run                                              # applies migration v5
psql "$DATABASE_URL" -f scripts/seed_cognitive_demo.sql   # optional demo history
BASE=http://localhost:8080/api/v1 python3 scripts/smoke_test_v5.py
curl -XPOST $BASE/cognitive/train -H 'content-type: application/json' -d '{"weeks":90}'
curl -XPOST $BASE/cognitive/cycle -H 'content-type: application/json' -d '{}'
curl -XPUT  $BASE/cognitive/policies/create_po -H 'content-type: application/json' -d '{"autonomy":"supervised"}'
```
