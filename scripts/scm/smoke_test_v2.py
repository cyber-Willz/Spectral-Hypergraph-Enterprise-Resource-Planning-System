#!/usr/bin/env python3
"""End-to-end test of the dataset-driven features (cold chain, FEFO, quarantine, contracts,
3-way match, scorecards, replenishment, forecasts). Usage as smoke_test.py."""
import datetime as dt, json, os, sys, urllib.request, urllib.error, uuid

BASE = os.environ.get("BASE", "http://localhost:8080/api/scm")


def _login():
    root = BASE.rsplit("/api/", 1)[0]
    body = json.dumps({"email": os.environ.get("ADMIN_EMAIL", "admin@example.com"),
                       "password": os.environ.get("ADMIN_PASSWORD", "ChangeMe123!")}).encode()
    req = urllib.request.Request(root + "/api/auth/login", method="POST", data=body, headers={"content-type": "application/json"})
    return json.loads(urllib.request.urlopen(req).read())["token"]


KEY = _login()  # JWT from the suite's login endpoint

def call(method, path, body=None, expect=None):
    req = urllib.request.Request(BASE + path, method=method,
                                 data=json.dumps(body).encode() if body is not None else None)
    req.add_header("content-type", "application/json")
    if KEY: req.add_header("authorization", "Bearer " + KEY)
    try:
        with urllib.request.urlopen(req, timeout=300) as r: status, data = r.status, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e: status, data = e.code, json.loads(e.read() or b"null")
    if expect is not None and status != expect: sys.exit(f"FAIL {method} {path}: expected {expect}, got {status}: {data}")
    return data

today = dt.date.today(); d = lambda n: (today + dt.timedelta(days=n)).isoformat()
tag = uuid.uuid4().hex[:6].upper()
wh = call("POST", "/warehouses", {"code": f"V2-{tag}", "name": "Cold DC"}, 201)
amb = call("POST", f"/warehouses/{wh['id']}/locations", {"code": "AMB-1"}, 201)
cold = call("POST", f"/warehouses/{wh['id']}/locations", {"code": "COLD-1", "storage_class": "cold"}, 201)
cold2 = call("POST", f"/warehouses/{wh['id']}/locations", {"code": "COLD-2", "storage_class": "cold"}, 201)
call("POST", f"/warehouses/{wh['id']}/locations", {"code": "BAD", "storage_class": "lukewarm"}, 400)
sup = call("POST", "/suppliers", {"code": f"S2-{tag}", "name": "Insulin Co", "country": "DE", "tier": 2,
           "financial_stability_score": 80, "geopolitical_risk_index": 15, "alternatives_available": 6,
           "environmental_compliance": 90, "previous_disruptions": 0}, 201)
weak = call("POST", "/suppliers", {"code": f"W2-{tag}", "name": "Shaky Ltd", "financial_stability_score": 35,
            "geopolitical_risk_index": 70, "previous_disruptions": 3, "alternatives_available": 0,
            "environmental_compliance": 40}, 201)
call("POST", "/suppliers", {"code": f"X-{tag}", "name": "x", "esg_score": 140}, 400)
cus = call("POST", "/customers", {"code": f"C2-{tag}", "name": "Clinic"}, 201)
ins = call("POST", "/products", {"sku": f"INS-{tag}", "name": "Insulin", "unit_cost": "10", "list_price": "25",
           "storage_class": "cold", "shelf_life_days": 180, "criticality_class": "A", "review_period_days": 7,
           "lead_time_days": 10, "reorder_qty": 50}, 201)
call("POST", "/products", {"sku": f"BAD-{tag}", "name": "x", "criticality_class": "Z"}, 400)

# --- contract window enforcement
ct = call("POST", "/contracts", {"supplier_id": sup["id"], "contract_number": f"K-{tag}", "start_date": d(-100), "end_date": d(100)}, 201)
call("POST", "/purchase-orders", {"supplier_id": sup["id"], "warehouse_id": wh["id"], "contract_id": ct["id"],
     "order_date": d(-200), "lines": [{"product_id": ins["id"], "quantity": 10}]}, 409)
call("POST", "/purchase-orders", {"supplier_id": weak["id"], "warehouse_id": wh["id"], "contract_id": ct["id"],
     "lines": [{"product_id": ins["id"], "quantity": 10}]}, 409)  # other supplier's contract

# --- PO with promised date, back-dated order, budget price
po = call("POST", "/purchase-orders", {"supplier_id": sup["id"], "warehouse_id": wh["id"], "contract_id": ct["id"],
          "order_date": d(-20), "expected_date": d(-5), "currency": "eur", "payment_terms_days": 45,
          "lines": [{"product_id": ins["id"], "quantity": 100, "unit_cost": "10", "budget_unit_cost": "9.50"}]}, 201)
oid, line = po["order"]["id"], po["lines"][0]
assert po["order"]["currency"] == "EUR" and line["promised_date"] == d(-5)
call("POST", f"/purchase-orders/{oid}/submit", expect=200)
# cold chain: insulin cannot be put away in an ambient bin
call("POST", f"/purchase-orders/{oid}/receive", {"lines": [{"line_id": line["id"], "quantity": 10, "location_id": amb["id"]}]}, 409)
r = call("POST", f"/purchase-orders/{oid}/receive", {"received_on": d(-3), "lines": [
    {"line_id": line["id"], "quantity": 40, "location_id": cold["id"], "lot_number": "LOT-OLD", "expiry_date": d(10)},
    {"line_id": line["id"], "quantity": 30, "location_id": cold["id"], "lot_number": "LOT-NEW"}]}, 200)  # default expiry = +180d
assert r["order"]["status"] == "partially_received"
stock = call("GET", f"/inventory/stock?product_id={ins['id']}", expect=200)
new = [x for x in stock if x["lot_number"] == "LOT-NEW"][0]
assert new["expiry_date"] == (today - dt.timedelta(days=3) + dt.timedelta(days=180)).isoformat(), new
call("POST", f"/purchase-orders/{oid}/close", expect=200)  # supplier short-ships the last 30 units
# on-time: received d(-3) vs promised d(-5) => 2 days late
sc = call("GET", f"/suppliers/{sup['id']}/scorecard", expect=200)
assert sc["lines_received"] == 1 and sc["on_time_rate"] == 0.0 and sc["avg_days_late"] == 2.0, sc
assert abs(sc["fill_rate"] - 0.7) < 1e-9 and sc["in_full_rate"] == 0.0 and sc["lead_time_mean_days"] == 17.0, sc
assert sc["risk_band"] == "low" and sc["risk_inputs_imputed"] == 0, sc
wk = call("GET", f"/suppliers/{weak['id']}/scorecard", expect=200)
assert wk["risk_band"] == "high" and wk["performance_score"] is None, wk

# --- FEFO allocation + transfer keeps expiry + expired stock skipped
so = call("POST", "/sales-orders", {"customer_id": cus["id"], "warehouse_id": wh["id"], "lines": [{"product_id": ins["id"], "quantity": 20}]}, 201)
call("POST", f"/sales-orders/{so['order']['id']}/confirm", expect=200)
stock = {x["lot_number"]: x for x in call("GET", f"/inventory/stock?product_id={ins['id']}", expect=200)}
assert stock["LOT-OLD"]["reserved"] == 20 and stock["LOT-NEW"]["reserved"] == 0, stock  # earliest expiry first
call("POST", "/inventory/transfer", {"product_id": ins["id"], "from_location_id": cold["id"], "to_location_id": amb["id"], "lot_number": "LOT-NEW", "quantity": 1}, 409)
call("POST", "/inventory/transfer", {"product_id": ins["id"], "from_location_id": cold["id"], "to_location_id": cold2["id"], "lot_number": "LOT-OLD", "quantity": 10}, 200)
moved = [x for x in call("GET", f"/inventory/stock?location_id={cold2['id']}", expect=200)][0]
assert moved["expiry_date"] == d(10), moved

# --- quarantine -> release / scrap
inc = call("POST", "/quality-incidents", {"product_id": ins["id"], "supplier_id": sup["id"], "severity": "major", "defect_type": "Packaging",
           "location_id": cold["id"], "lot_number": "LOT-NEW", "quarantine_qty": 10}, 201)
st = {x["lot_number"]: x for x in call("GET", f"/inventory/stock?location_id={cold['id']}", expect=200)}
assert st["LOT-NEW"]["blocked"] == 10 and st["LOT-NEW"]["available"] == 20, st
call("POST", "/quality-incidents", {"product_id": ins["id"], "severity": "minor", "location_id": cold["id"], "lot_number": "LOT-NEW", "quarantine_qty": 999}, 409)
call("POST", f"/quality-incidents/{inc['id']}/resolve", {"release_qty": 11}, 409)
r = call("POST", f"/quality-incidents/{inc['id']}/resolve", {"release_qty": 6, "scrap_qty": 2}, 200)
assert r["status"] == "open"
r = call("POST", f"/quality-incidents/{inc['id']}/resolve", {"scrap_qty": 2}, 200)
assert r["status"] == "closed" and r["scrapped_qty"] == 4
st = {x["lot_number"]: x for x in call("GET", f"/inventory/stock?location_id={cold['id']}", expect=200)}
assert st["LOT-NEW"]["quantity"] == 26 and st["LOT-NEW"]["blocked"] == 0, st
assert any(m["movement_type"] == "scrap" for m in call("GET", f"/inventory/movements?product_id={ins['id']}", expect=200))
sc = call("GET", f"/suppliers/{sup['id']}/scorecard", expect=200)
assert sc["incidents_major"] == 1 and sc["quality_score"] < 100

# --- invoices: 3-way match
inv = lambda n, q, p: call("POST", f"/purchase-orders/{oid}/invoices", {"invoice_number": n, "lines": [{"po_line_id": line["id"], "quantity": q, "unit_price": p}]}, 201)
a = inv("A", 50, "10.10"); assert a["invoice"]["status"] == "matched" and a["invoice"]["total"] in ("505", "505.0000", "505.00")
assert a["invoice"]["due_date"] == (today + dt.timedelta(days=45)).isoformat()
b = inv("B", 30, "10"); assert b["invoice"]["status"] == "disputed" and "only 20" in b["invoice"]["match_notes"], b   # 70 received, 50 billed
c = inv("C", 10, "11"); assert c["invoice"]["status"] == "disputed" and "price" in c["invoice"]["match_notes"], c
call("POST", f"/invoices/{b['invoice']['id']}/pay", expect=409)
call("POST", f"/invoices/{c['invoice']['id']}/approve", {"reason": ""}, 400)
call("POST", f"/invoices/{c['invoice']['id']}/approve", {"reason": "price change agreed by phone"}, 200)
assert call("POST", f"/invoices/{a['invoice']['id']}/pay", expect=200)["invoice"]["status"] == "paid"
call("POST", f"/purchase-orders/{oid}/invoices", {"invoice_number": "A", "lines": [{"po_line_id": line["id"], "quantity": 1, "unit_price": "10"}]}, 409)  # duplicate number
ctl = call("GET", "/reports/procurement-controls", expect=200)
assert ctl["invoices"]["disputed"] >= 1 and ctl["spend"]["over_budget_lines"] >= 1 and ctl["spend"]["price_variance_vs_budget"] > 0

# --- planning endpoints
for path in ("/reports/replenishment", "/reports/abc-xyz", "/reports/low-stock", "/reports/supplier-scorecards"):
    call("GET", path, expect=200)
call("GET", "/reports/replenishment?lookback_days=3", expect=400)
call("POST", "/forecasts", {"items": [{"warehouse_id": wh["id"], "product_id": ins["id"], "period_start": d(-14), "forecast_qty": 12.5, "forecast_type": "adjusted", "uplift_pct": 0.1}]}, 201)
fa = call("GET", f"/reports/forecast-accuracy?product_id={ins['id']}", expect=200)
assert fa[0]["forecast_type"] == "adjusted" and fa[0]["periods"] == 1
call("POST", "/forecasts", {"items": [{"warehouse_id": wh["id"], "product_id": ins["id"], "period_start": d(-14), "forecast_qty": 1, "forecast_type": "bogus"}]}, 400)
print("ALL V2 SMOKE TESTS PASSED")
