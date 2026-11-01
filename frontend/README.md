# Enterprise Suite web UI (Yew + Stylist)

A single-page WebAssembly app for the whole suite: CRM, projects, GRC, assets, QMS, EHS, documents, finance, HR, financial risk and supply chain.

## Run

```bash
rustup target add wasm32-unknown-unknown
cargo install trunk                      # needs a wasm-bindgen-cli matching Cargo.lock; trunk fetches it

# terminal 1: API (repo root)
cargo run --release

# terminal 2: UI with live reload, /api proxied to 127.0.0.1:8080
cd frontend && trunk serve --open
```

Production: `cd frontend && trunk build --release`, then start the API; it serves `frontend/dist` on the same origin (override with `FRONTEND_DIST`). Sign in with `ADMIN_EMAIL` / `ADMIN_PASSWORD` from `.env`.

## What is in it

| Area | Notes |
|---|---|
| Dashboard | `/api/enterprise/overview`: KPIs, "needs attention" list linking to the fix, per-module cards |
| 47 generated CRUD resources | one generic list/detail/create/edit/delete page, driven by `src/schema_generated.rs` |
| Finance, HR, risk, supply chain lists | `src/catalog.rs` feeds; search, sort, CSV export, JSON create, workflow buttons |
| Row actions | convert lead, change stage, pay/receive/void, payroll approve/post, PO/SO workflow, NCR transitions, planner approve/reject… |
| Insights | ~70 analytics endpoints rendered generically (tiles, tables, bar/line charts) with parameter pickers and raw-JSON toggle |
| Data import | CSV upload to the CRM and portfolio import endpoints |
| Users | admin-only account/role management |
| Theming | Stylist components, design tokens, light / dark / system |
| Roles | navigation and buttons mirror `auth::authorize`; the server still enforces everything |

## Schema stays in one place

`tools/spec.py` is the single source of truth. `python tools/gen.py` now also writes `frontend/src/schema_generated.rs`, so a new table or column appears in the UI without hand edits. Hand-written endpoints are added to `src/catalog.rs` (feeds, reports, row actions).

## Tests

`cargo test` (in `frontend/`) runs the pure logic natively: formatting, role policy mirror, form coercion, catalogue consistency (every action targets a real list), report URLs, Stylist CSS parsing.
